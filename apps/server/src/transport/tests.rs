use super::*;
use axum::{body::Body, http};
use kajiba_proto::v1::health_service_client::HealthServiceClient;
use prost::Message;
use std::{
    fs,
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};
use tonic::codegen::Service;
use tonic_reflection::pb::v1::{
    ServerReflectionRequest, ServerReflectionResponse, server_reflection_request::MessageRequest,
    server_reflection_response::MessageResponse,
};

struct State {
    health: Health,
    _lock: fs::File,
}

impl State {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "kajiba-transport-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let lock = crate::state::prepare(&dir).unwrap();
        Self {
            health: Health::new(dir),
            _lock: lock,
        }
    }

    async fn start(&self) {
        let (mut writer, readers) =
            crate::db::open(&self.health.state_dir.join("db/kajiba.sqlite"))
                .await
                .unwrap();
        crate::state::cleanup(&self.health.state_dir, &mut writer)
            .await
            .unwrap();
        crate::db::users::reconcile_users(&mut writer, &Default::default())
            .await
            .unwrap();
        crate::db::credentials::reconcile_credentials(&mut writer, &Default::default())
            .await
            .unwrap();
        let _bus = crate::db::events::EventBus::default();
        self.health.started(readers);
    }
}

impl Drop for State {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.health.state_dir).unwrap();
    }
}

async fn get(app: &mut Router, path: &str) -> StatusCode {
    app.call(http::Request::get(path).body(Body::empty()).unwrap())
        .await
        .unwrap()
        .status()
}

async fn grpc(app: &mut Router, path: &str, message: impl Message) -> http::Response<Body> {
    let payload = message.encode_to_vec();
    let mut frame = vec![0];
    frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    frame.extend_from_slice(&payload);
    app.call(
        http::Request::post(path)
            .header("content-type", "application/grpc")
            .body(Body::from(frame))
            .unwrap(),
    )
    .await
    .unwrap()
}

async fn decode<M: Message + Default>(response: http::Response<Body>) -> M {
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    assert!(body.len() >= 5);
    assert_eq!(body[0], 0);
    let len = u32::from_be_bytes(body[1..5].try_into().unwrap()) as usize;
    M::decode(&body[5..5 + len]).unwrap()
}

async fn serving_status(app: &mut Router) -> ServingStatus {
    let response = grpc(app, "/kajiba.v1.HealthService/Check", CheckRequest {}).await;
    let response: CheckResponse = decode(response).await;
    response.status()
}

#[tokio::test]
async fn readiness_waits_for_startup_and_checks_database_and_directories() {
    let state = State::new();
    let mut app = router(state.health.clone(), false).unwrap();
    assert!(!state.health.state_dir.join("db/kajiba.sqlite").exists());
    assert_eq!(get(&mut app, "/healthz").await, StatusCode::OK);
    assert_eq!(
        get(&mut app, "/readyz").await,
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(serving_status(&mut app).await, ServingStatus::NotServing);

    state.start().await;
    assert_eq!(get(&mut app, "/readyz").await, StatusCode::OK);
    assert_eq!(serving_status(&mut app).await, ServingStatus::Serving);

    fs::remove_dir(state.health.state_dir.join("ci/logs")).unwrap();
    assert_eq!(
        get(&mut app, "/readyz").await,
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(serving_status(&mut app).await, ServingStatus::NotServing);
    fs::create_dir(state.health.state_dir.join("ci/logs")).unwrap();
    assert_eq!(get(&mut app, "/readyz").await, StatusCode::OK);

    state.health.database.get().unwrap().close().await;
    assert_eq!(
        get(&mut app, "/readyz").await,
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(serving_status(&mut app).await, ServingStatus::NotServing);
    assert_eq!(get(&mut app, "/healthz").await, StatusCode::OK);
}

#[tokio::test]
async fn reflection_is_disabled_by_default() {
    let config =
        kajiba_core::config::ServerConfig::parse("[http]\npublic_url='https://example.org'")
            .unwrap();
    assert!(!config.http.reflection);
    let state = State::new();
    let mut app = router(state.health.clone(), config.http.reflection).unwrap();
    for version in ["v1", "v1alpha"] {
        let response = grpc(
            &mut app,
            &format!("/grpc.reflection.{version}.ServerReflection/ServerReflectionInfo"),
            ServerReflectionRequest {
                message_request: Some(MessageRequest::ListServices(String::new())),
                ..Default::default()
            },
        )
        .await;
        assert_eq!(response.headers()["grpc-status"], "12");
    }
}

#[tokio::test]
async fn reflection_lists_health_and_returns_its_descriptor_when_enabled() {
    let state = State::new();
    let mut app = router(state.health.clone(), true).unwrap();
    // v1 and v1alpha use the same wire format; support both grpcurl generations.
    for version in ["v1", "v1alpha"] {
        let path = format!("/grpc.reflection.{version}.ServerReflection/ServerReflectionInfo");
        let response = grpc(
            &mut app,
            &path,
            ServerReflectionRequest {
                message_request: Some(MessageRequest::ListServices(String::new())),
                ..Default::default()
            },
        )
        .await;
        let response: ServerReflectionResponse = decode(response).await;
        let Some(MessageResponse::ListServicesResponse(list)) = response.message_response else {
            panic!("expected service list");
        };
        assert!(
            list.service
                .iter()
                .any(|s| s.name == "kajiba.v1.HealthService")
        );

        let response = grpc(
            &mut app,
            &path,
            ServerReflectionRequest {
                message_request: Some(MessageRequest::FileContainingSymbol(
                    "kajiba.v1.HealthService".into(),
                )),
                ..Default::default()
            },
        )
        .await;
        let response: ServerReflectionResponse = decode(response).await;
        let Some(MessageResponse::FileDescriptorResponse(descriptor)) = response.message_response
        else {
            panic!("expected health descriptor");
        };
        assert!(!descriptor.file_descriptor_proto.is_empty());
        assert!(descriptor.file_descriptor_proto.iter().any(|file| {
            file.windows(b"HealthService".len())
                .any(|part| part == b"HealthService")
        }));
    }
}

#[tokio::test]
async fn http_and_grpc_share_one_ephemeral_port() {
    let state = State::new();
    state.start().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(state.health.clone(), false).unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let http = tokio::task::spawn_blocking(move || {
        use std::io::{Read, Write};
        let mut stream =
            std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(5)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
            .write_all(b"GET /healthz HTTP/1.1\r\nHost: example.org\r\nConnection: close\r\n\r\n")
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        assert!(response.starts_with("HTTP/1.1 200 OK\r\n"), "{response}");
        assert!(response.ends_with("OK"), "{response}");
    });
    let endpoint = tonic::transport::Endpoint::from_shared(format!("http://{addr}"))
        .unwrap()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(5));
    let mut client = HealthServiceClient::new(endpoint.connect().await.unwrap());
    assert_eq!(
        client
            .check(CheckRequest {})
            .await
            .unwrap()
            .into_inner()
            .status(),
        ServingStatus::Serving
    );
    http.await.unwrap();
    server.abort();
    let _ = server.await;
    state.health.database.get().unwrap().close().await;
}
