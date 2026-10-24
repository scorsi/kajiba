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

struct LoggedBody(Body);

impl http_body::Body for LoggedBody {
    type Data = axum::body::Bytes;
    type Error = axum::Error;

    fn poll_frame(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        tracing::info!(event = "body-polled");
        http_body::Body::poll_frame(std::pin::Pin::new(&mut self.0), cx)
    }
}

struct FailingHealth;

#[tonic::async_trait]
impl HealthService for FailingHealth {
    async fn check(&self, _: Request<CheckRequest>) -> Result<Response<CheckResponse>, Status> {
        tracing::info!(event = "handling");
        tokio::task::yield_now().await;
        kajiba_core::log::error("request-failed", "test failure");
        tracing::info!(event = "handled");
        Err(Status::internal("private failure"))
    }
}

#[tokio::test]
async fn concurrent_errors_match_request_logs_and_inherit_spans() {
    const CHILD: &str = "KAJIBA_TEST_CORRELATION";
    if std::env::var_os(CHILD).is_some() {
        kajiba_core::log::init("info").unwrap();
        let app = Routes::new(HealthServiceServer::new(FailingHealth))
            .into_axum_router()
            .layer(axum::middleware::from_fn(
                |request: http::Request<Body>, next: axum::middleware::Next| async {
                    next.run(request)
                        .await
                        .map(|body| Body::new(LoggedBody(body)))
                },
            ))
            .layer(axum::middleware::from_fn(correlation::request));
        let call = || {
            let mut app = app.clone();
            async move {
                let response = app
                    .call(
                        http::Request::post("/kajiba.v1.HealthService/Check?token=DO-NOT-PRINT")
                            .header("content-type", "application/grpc")
                            .header("x-request-id", "DO-NOT-TRUST")
                            .body(Body::from(vec![0, 0, 0, 0, 0]))
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                let status = Status::from_header_map(response.headers()).unwrap();
                let details = tonic_types::pb::Status::decode(status.details()).unwrap();
                let info = tonic_types::pb::ErrorInfo::decode(details.details[0].value.as_slice())
                    .unwrap();
                assert_eq!(info.domain, "kajiba");
                assert_eq!(info.reason, "INTERNAL");
                assert_eq!(status.message(), "internal error");
                assert_eq!(details.message, "internal error");
                axum::body::to_bytes(response.into_body(), 1024)
                    .await
                    .unwrap();
                info.metadata["request_id"].clone()
            }
        };
        let (a, b) = tokio::join!(call(), call());
        assert_ne!(a, b);
        println!("CORRELATION_IDS={}", serde_json::json!([a, b]));
        return;
    }
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "transport::tests::concurrent_errors_match_request_logs_and_inherit_spans",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .env_remove("RUST_LOG")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    let ids: Vec<String> = serde_json::from_str(
        stdout
            .lines()
            .find_map(|line| line.strip_prefix("CORRELATION_IDS="))
            .unwrap(),
    )
    .unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(!stderr.contains("DO-NOT-PRINT"));
    assert!(!stderr.contains("DO-NOT-TRUST"));
    let logs: Vec<serde_json::Value> = stderr
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    for id in ids {
        assert_eq!(id.len(), 26);
        assert!(id.as_bytes()[0] <= b'7');
        assert!(
            id.bytes()
                .all(|b| b"0123456789ABCDEFGHJKMNPQRSTVWXYZ".contains(&b))
        );
        let related: Vec<_> = logs
            .iter()
            .filter(|line| line["request_id"] == id)
            .collect();
        for event in ["handling", "handled", "body-polled", "request"] {
            assert_eq!(
                related.iter().filter(|line| line["event"] == event).count(),
                1,
                "{logs:?}"
            );
        }
        assert_eq!(
            related
                .iter()
                .filter(|line| line["event"] == "request-failed")
                .count(),
            1
        );
        let request = related
            .iter()
            .find(|line| line["event"] == "request")
            .unwrap();
        assert_eq!(request["method"], "/kajiba.v1.HealthService/Check");
        assert_eq!(request["status"], "INTERNAL");
        assert_eq!(request["level"], "error");
        assert_eq!(request["err"], "private failure");
        assert!(request["dur_us"].is_u64());
    }
    assert!(
        logs.iter().all(|line| line["request_id"].is_string()),
        "{logs:?}"
    );
}

#[tokio::test]
async fn signals_drain_in_flight_requests_and_exit_zero() {
    const CHILD: &str = "KAJIBA_TEST_SHUTDOWN";
    if std::env::var_os(CHILD).is_some() {
        kajiba_core::log::init("info").unwrap();
        let app = Router::new()
            .route(
                "/slow",
                axum::routing::post(|request: http::Request<Body>| async {
                    tracing::info!(event = "in-flight");
                    let bytes = axum::body::to_bytes(request.into_body(), 4).await.unwrap();
                    assert_eq!(bytes.as_ref(), b"abcd");
                    "completed"
                }),
            )
            .layer(axum::middleware::from_fn(correlation::request));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        serve_listener(listener, app).await.unwrap();
        return;
    }
    tokio::task::spawn_blocking(|| {
        use std::io::{BufRead, Read, Write};
        use std::process::{Command, Stdio};
        for signal in ["-TERM", "-INT"] {
            let mut child = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "transport::tests::signals_drain_in_flight_requests_and_exit_zero", "--nocapture"])
                .env(CHILD, "1").env_remove("RUST_LOG")
                .stdout(Stdio::null()).stderr(Stdio::piped()).spawn().unwrap();
            let (send, recv) = std::sync::mpsc::channel();
            let stderr = child.stderr.take().unwrap();
            let reader = std::thread::spawn(move || {
                for line in std::io::BufReader::new(stderr).lines() {
                    let value: serde_json::Value = serde_json::from_str(&line.unwrap()).unwrap();
                    let _ = send.send(value);
                }
            });
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let timeout = Duration::from_secs(10);
                let listening = recv.recv_timeout(timeout).unwrap();
                assert_eq!(listening["event"], "listening");
                let addr = listening["addr"].as_str().unwrap().parse().unwrap();
                let mut stream = std::net::TcpStream::connect_timeout(&addr, timeout).unwrap();
                stream.set_read_timeout(Some(timeout)).unwrap();
                stream.set_write_timeout(Some(timeout)).unwrap();
                stream.write_all(b"POST /slow HTTP/1.1\r\nHost: example.org\r\nContent-Length: 4\r\nConnection: close\r\n\r\na").unwrap();
                assert_eq!(recv.recv_timeout(timeout).unwrap()["event"], "ready");
                let started = recv.recv_timeout(timeout).unwrap();
                assert_eq!(started["event"], "in-flight");
                assert!(Command::new("/bin/kill").arg(signal).arg(child.id().to_string()).status().unwrap().success());
                let stopping = recv.recv_timeout(timeout).unwrap();
                assert_eq!(stopping["event"], "stopping");
                assert!(stopping.get("request_id").is_none());
                assert!(child.try_wait().unwrap().is_none());
                let deadline = std::time::Instant::now() + timeout;
                while std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(100)).is_ok() {
                    assert!(std::time::Instant::now() < deadline, "listener still accepts requests");
                    std::thread::sleep(Duration::from_millis(10));
                }
                // The accepted request is waiting for the rest of its body.
                stream.write_all(b"bcd").unwrap();
                let mut response = String::new();
                stream.read_to_string(&mut response).unwrap();
                assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");
                assert!(response.ends_with("completed"), "{response}");
                let request = recv.recv_timeout(timeout).unwrap();
                assert_eq!(request["event"], "request");
                assert_eq!(request["request_id"], started["request_id"]);
                let deadline = std::time::Instant::now() + timeout;
                loop {
                    if let Some(status) = child.try_wait().unwrap() {
                        assert_eq!(status.code(), Some(0));
                        break;
                    }
                    assert!(std::time::Instant::now() < deadline, "server did not drain");
                    std::thread::sleep(Duration::from_millis(10));
                }
            }));
            let _ = child.kill();
            child.wait().unwrap();
            reader.join().unwrap();
            if let Err(err) = result { std::panic::resume_unwind(err); }
        }
    }).await.unwrap();
}

#[tokio::test]
async fn tonic_transport_errors_receive_error_info() {
    let state = State::new();
    let mut app = router(state.health.clone(), false).unwrap();
    let response = grpc(
        &mut app,
        "/grpc.reflection.v1.ServerReflection/ServerReflectionInfo",
        ServerReflectionRequest::default(),
    )
    .await;
    let status = Status::from_header_map(response.headers()).unwrap();
    assert_eq!(status.code(), tonic::Code::Unimplemented);
    let details = tonic_types::pb::Status::decode(status.details()).unwrap();
    assert_eq!(details.code, tonic::Code::Unimplemented as i32);
    let info = tonic_types::pb::ErrorInfo::decode(details.details[0].value.as_slice()).unwrap();
    assert_eq!(info.domain, "kajiba");
    assert_eq!(info.reason, "INTERNAL");
    assert_eq!(info.metadata["request_id"].len(), 26);
}
