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

pub(super) struct State {
    pub(super) health: Health,
    _lock: fs::File,
}

impl State {
    pub(super) fn new() -> Self {
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

    pub(super) async fn start(&self) {
        let (mut writer, readers) =
            crate::db::open(&self.health.state_dir.join("db/kajiba.sqlite"))
                .await
                .unwrap();
        crate::state::cleanup(&self.health.state_dir, &mut writer)
            .await
            .unwrap();
        crate::db::reconcile(&mut writer, &Default::default())
            .await
            .unwrap();
        let _bus = crate::db::events::EventBus::default();
        self.health.started(readers);
        self.health.local_serving.store(true, Ordering::Release);
    }
}

impl Drop for State {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.health.state_dir).unwrap();
    }
}

pub(super) async fn get(app: &mut Router, path: &str) -> StatusCode {
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
    let mut app = router(state.health.clone(), false, Arc::default()).unwrap();
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
    let mut app = router(state.health.clone(), config.http.reflection, Arc::default()).unwrap();
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
    let mut app = router(state.health.clone(), true, Arc::default()).unwrap();
    // Only v1 is served; the deprecated v1alpha is not.
    let response = grpc(
        &mut app,
        "/grpc.reflection.v1alpha.ServerReflection/ServerReflectionInfo",
        ServerReflectionRequest {
            message_request: Some(MessageRequest::ListServices(String::new())),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(response.headers()["grpc-status"], "12");
    {
        let path = "/grpc.reflection.v1.ServerReflection/ServerReflectionInfo";
        let response = grpc(
            &mut app,
            path,
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

        assert!(
            list.service
                .iter()
                .all(|service| !service.name.starts_with("kajiba.admin."))
        );
        let response = grpc(
            &mut app,
            path,
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
    let app = router(state.health.clone(), false, Arc::default()).unwrap();
    let (_stop, stopped) = watch::channel(false);
    let server = tokio::spawn(serve_tcp(listener, app, PUBLIC, stopped));
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
            .layer(axum::middleware::from_fn_with_state(
                Arc::new(Registry::default()),
                correlation::request,
            ));
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
        assert_eq!(request["status"], 200);
        assert_eq!(request["grpc_code"], "INTERNAL");
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
        let state = State::new();
        state.start().await;
        kajiba_core::log::init("info").unwrap();
        let registry = Arc::new(Registry::default());
        let metrics = if std::env::var_os("KAJIBA_TEST_METRICS").is_some() {
            let listener = metrics::bind(Some("127.0.0.1:0".parse().unwrap()))
                .await
                .unwrap()
                .unwrap();
            Some((
                listener,
                metrics::router(
                    registry.clone(),
                    state.health.database.get().unwrap().clone(),
                    Arc::default(),
                ),
            ))
        } else {
            None
        };
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
            .layer(axum::middleware::from_fn_with_state(
                registry,
                correlation::request,
            ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let local = local::Local::bind(
            &state.health.state_dir.join("local.sock"),
            state.health.local_serving.clone(),
        )
        .unwrap();
        let admin = local::tests::admin(&state.health.state_dir.join("db"), "").await;
        serve_listeners(listener, app, metrics, (local, admin), Arc::default())
            .await
            .unwrap();
        return;
    }
    tokio::task::spawn_blocking(|| {
        use std::io::{BufRead, Read, Write};
        use std::process::{Command, Stdio};
        for (signal, metrics) in [("-TERM", false), ("-INT", true)] {
            let mut command = Command::new(std::env::current_exe().unwrap());
            command.args(["--exact", "transport::tests::signals_drain_in_flight_requests_and_exit_zero", "--nocapture"])
                .env(CHILD, "1").env_remove("RUST_LOG").env_remove("KAJIBA_TEST_METRICS")
                .stdout(Stdio::null()).stderr(Stdio::piped());
            if metrics { command.env("KAJIBA_TEST_METRICS", "1"); }
            let mut child = command.spawn().unwrap();
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
                let metrics_addr = if metrics {
                    let listening = recv.recv_timeout(timeout).unwrap();
                    assert_eq!(listening["event"], "metrics-listening");
                    let addr = listening["addr"].as_str().unwrap().parse::<std::net::SocketAddr>().unwrap();
                    assert!(addr.ip().is_loopback());
                    let mut stream = std::net::TcpStream::connect_timeout(&addr, timeout).unwrap();
                    stream.set_read_timeout(Some(timeout)).unwrap();
                    stream.set_write_timeout(Some(timeout)).unwrap();
                    stream.write_all(b"GET /metrics HTTP/1.1\r\nHost: example.org\r\nConnection: close\r\n\r\n").unwrap();
                    let mut response = String::new();
                    stream.read_to_string(&mut response).unwrap();
                    assert!(response.starts_with("HTTP/1.1 200 OK\r\n"), "{response}");
                    assert!(response.contains(&format!("content-type: {}\r\n", metrics::CONTENT_TYPE)));
                    assert!(response.contains("kajiba_build_info{version=\""));
                    Some(addr)
                } else { None };
                let mut stream = std::net::TcpStream::connect_timeout(&addr, timeout).unwrap();
                stream.set_read_timeout(Some(timeout)).unwrap();
                stream.set_write_timeout(Some(timeout)).unwrap();
                stream.write_all(b"POST /slow HTTP/1.1\r\nHost: example.org\r\nContent-Length: 4\r\nConnection: close\r\n\r\na").unwrap();
                assert_eq!(recv.recv_timeout(timeout).unwrap()["event"], "ready");
                let started = recv.recv_timeout(timeout).unwrap();
                assert_eq!(started["event"], "in-flight");
                // PATH lookup: the Linux Nix build sandbox has no /bin/kill; stdenv coreutils provides kill.
                assert!(Command::new("kill").arg(signal).arg(child.id().to_string()).status().unwrap().success());
                let stopping = recv.recv_timeout(timeout).unwrap();
                assert_eq!(stopping["event"], "stopping");
                assert!(stopping.get("request_id").is_none());
                assert!(child.try_wait().unwrap().is_none());
                let deadline = std::time::Instant::now() + timeout;
                while std::iter::once(addr).chain(metrics_addr).any(|addr| std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(100)).is_ok()) {
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
                assert_eq!(request["status"], 200);
                assert!(request.get("grpc_code").is_none(), "{request}");
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
    let mut app = router(state.health.clone(), false, Arc::default()).unwrap();
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
    // UNIMPLEMENTED has no row in ADR 0020's mapping: no reason is invented.
    assert_eq!(info.reason, "");
    assert_eq!(info.metadata["request_id"].len(), 26);
}

#[tokio::test]
async fn requests_update_metrics_after_body_completion_including_grpc_trailers() {
    let state = State::new();
    let registry = Arc::new(Registry::default());
    let mut app = router(state.health.clone(), false, registry.clone()).unwrap();
    let before = registry.exposition("test", [0; 4], [0; 4]);
    assert!(before.contains("kajiba_requests_total{kind=\"other\",code=\"2xx\"} 0\n"));
    let response = app
        .call(http::Request::get("/healthz").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), 1024)
        .await
        .unwrap();
    assert_eq!(bytes.as_ref(), b"OK");
    let response = grpc(&mut app, "/kajiba.v1.HealthService/Check", CheckRequest {}).await;
    let _: CheckResponse = decode(response).await;
    let after = registry.exposition("test", [0; 4], [0; 4]);
    assert!(after.contains("kajiba_requests_total{kind=\"other\",code=\"2xx\"} 1\n"));
    assert!(after.contains("kajiba_requests_total{kind=\"grpc\",code=\"OK\"} 1\n"));
    assert!(after.contains("kajiba_request_duration_seconds_count{kind=\"grpc\"} 1\n"));
    assert!(after.contains("kajiba_request_duration_seconds_count{kind=\"other\"} 1\n"));
    // Unknown routes: HTTP 404 for plain requests, gRPC UNIMPLEMENTED for gRPC callers.
    let response = app
        .call(http::Request::get("/metrics").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert!(response.headers().get("grpc-status").is_none());
    let response = grpc(&mut app, "/kajiba.v1.Unknown/Call", CheckRequest {}).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["grpc-status"], "12");
    assert_eq!(response.headers()["content-type"], "application/grpc");
}

#[tokio::test]
async fn slow_headers_are_cut_and_the_connection_cap_holds_until_then() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = Router::new().route("/", axum::routing::get(|| async { "OK" }));
    let limits = Limits {
        connections: 1,
        header_read: Duration::from_millis(300),
    };
    let (stop, stopped) = watch::channel(false);
    let server = tokio::spawn(serve_tcp(listener, app, limits, stopped));
    tokio::task::spawn_blocking(move || {
        use std::io::{Read, Write};
        let started = std::time::Instant::now();
        // A slowloris client holds the only permit with an unfinished request head.
        let mut slow = std::net::TcpStream::connect(addr).unwrap();
        slow.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        slow.write_all(b"GET / HTTP/1.1\r\nHost: exa").unwrap();
        // The kernel queues the second connection; the server accepts it only once a permit frees.
        let mut next = std::net::TcpStream::connect(addr).unwrap();
        next.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        next.write_all(b"GET / HTTP/1.1\r\nHost: example.org\r\nConnection: close\r\n\r\n")
            .unwrap();
        let mut rest = Vec::new();
        let _ = slow.read_to_end(&mut rest);
        assert!(started.elapsed() >= Duration::from_millis(300));
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "slow head was not cut"
        );
        let mut response = String::new();
        next.read_to_string(&mut response).unwrap();
        assert!(response.starts_with("HTTP/1.1 200 OK\r\n"), "{response}");
    })
    .await
    .unwrap();
    stop.send_replace(true);
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn non_streaming_routes_time_out_with_http_or_grpc_status() {
    let mut app = Router::new()
        .route(
            "/slow",
            axum::routing::any(|| async {
                tokio::time::sleep(Duration::from_secs(5)).await;
                "late"
            }),
        )
        .route_layer(middleware::from_fn_with_state(
            Duration::from_millis(50),
            request_timeout,
        ));
    assert_eq!(get(&mut app, "/slow").await, StatusCode::REQUEST_TIMEOUT);
    let response = grpc(&mut app, "/slow", CheckRequest {}).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["grpc-status"], "4");
}

#[test]
fn only_persistent_accept_errors_back_off() {
    for kind in [
        std::io::ErrorKind::ConnectionAborted,
        std::io::ErrorKind::ConnectionReset,
    ] {
        assert_eq!(accept_backoff(&kind.into()), None);
    }
    // EMFILE: retrying at once would spin a core.
    assert_eq!(
        accept_backoff(&std::io::Error::from_raw_os_error(24)),
        Some(Duration::from_secs(1))
    );
}
