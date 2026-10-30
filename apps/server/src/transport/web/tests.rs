use crate::{
    metrics::Registry,
    transport::{
        auth::tests::{READ, session, tokens},
        repos::tests::{DEMO, Fixture},
        router,
    },
};
use axum::{
    body::Body,
    http::{self, StatusCode},
};
use kajiba_proto::v1::ListReposResponse;
use prost::Message;
use std::sync::Arc;
use tonic::{Code, Status, codegen::Service};

const FORGE: &str = "https://example.org";
const LIST: &str = "/kajiba.v1.RepoService/ListRepos";
const SESSION: &str = "web-session";

/// The fixture must outlive the router: it owns the database directory.
async fn app(registry: Arc<Registry>) -> (axum::Router, Fixture) {
    let fixture = Fixture::new(&format!("{DEMO}{}", tokens())).await;
    let later = crate::transport::auth::now().unwrap() + 3_600_000;
    session(&fixture, "alice", SESSION, later).await;
    let app = router(fixture.repos.0.clone(), false, registry, FORGE).unwrap();
    (app, fixture)
}

/// A grpc-web unary call of `ListRepos` with an empty request.
async fn web_call(app: &mut axum::Router, header: Option<(&str, &str)>) -> http::Response<Body> {
    let mut request = http::Request::post(LIST)
        .header("content-type", "application/grpc-web+proto")
        .header("origin", FORGE);
    if let Some((name, value)) = header {
        request = request.header(name, value);
    }
    let response = app
        .call(request.body(Body::from(vec![0u8; 5])).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["content-type"],
        "application/grpc-web+proto"
    );
    assert_eq!(response.headers()["access-control-allow-origin"], FORGE);
    response
}

/// The message and the trailers of a grpc-web body (one message, then the trailers frame).
async fn split(response: http::Response<Body>) -> (Vec<u8>, http::HeaderMap) {
    let body = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    assert_eq!(body[0], 0);
    let length = u32::from_be_bytes(body[1..5].try_into().unwrap()) as usize;
    let (message, rest) = body[5..].split_at(length);
    assert_eq!(rest[0], 0x80);
    let mut trailers = http::HeaderMap::new();
    for line in std::str::from_utf8(&rest[5..]).unwrap().split("\r\n") {
        if let Some((name, value)) = line.split_once(':') {
            trailers.insert(
                http::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                value.parse().unwrap(),
            );
        }
    }
    (message.to_vec(), trailers)
}

#[tokio::test]
async fn grpc_web_calls_pass_the_same_auth_layer() {
    let registry = Arc::new(Registry::default());
    let (mut app, _fixture) = app(registry.clone()).await;
    let bearer = format!("Bearer {READ}");
    let cookie = format!("kajiba_session={SESSION}");
    for header in [
        ("authorization", bearer.as_str()),
        ("cookie", cookie.as_str()),
    ] {
        let (message, trailers) = split(web_call(&mut app, Some(header)).await).await;
        assert_eq!(trailers["grpc-status"], "0");
        let repos = ListReposResponse::decode(message.as_slice()).unwrap().repos;
        assert_eq!(repos.len(), 1, "{header:?}");
    }
    // A refusal is trailers-only: the status travels in the grpc-web response headers.
    let response = web_call(&mut app, None).await;
    let status = Status::from_header_map(response.headers()).unwrap();
    assert_eq!(status.code(), Code::Unauthenticated);
    assert_eq!(
        crate::transport::auth::tests::reason(&status),
        "CREDENTIAL_INVALID"
    );
    // A request is observed when its body ends or is dropped.
    drop(response);
    let metrics = registry.exposition("0", [0; 4], [0; 4]);
    for line in [
        "kajiba_requests_total{kind=\"grpc_web\",code=\"OK\"} 2",
        "kajiba_requests_total{kind=\"grpc_web\",code=\"UNAUTHENTICATED\"} 1",
    ] {
        assert!(metrics.contains(line), "{line} in {metrics}");
    }
}

#[tokio::test]
async fn only_the_forge_origin_passes_cors() {
    let (mut app, _fixture) = app(Arc::default()).await;
    let preflight = |origin: &str| {
        http::Request::builder()
            .method("OPTIONS")
            .uri(LIST)
            .header("origin", origin)
            .header("access-control-request-method", "POST")
            .header(
                "access-control-request-headers",
                "authorization,content-type,x-grpc-web",
            )
            .body(Body::empty())
            .unwrap()
    };
    let allowed = app.call(preflight(FORGE)).await.unwrap();
    assert_eq!(allowed.status(), StatusCode::NO_CONTENT);
    assert_eq!(allowed.headers()["access-control-allow-origin"], FORGE);
    assert_eq!(
        allowed.headers()["access-control-allow-credentials"],
        "true"
    );
    let allowed_headers = allowed.headers()["access-control-allow-headers"]
        .to_str()
        .unwrap();
    for header in ["authorization", "content-type", "x-grpc-web"] {
        assert!(
            allowed_headers
                .split(',')
                .any(|allowed| allowed.trim().eq_ignore_ascii_case(header)),
            "{header} missing from {allowed_headers}"
        );
    }
    for origin in ["https://evil.example.org", "http://example.org", "null"] {
        let refused = app.call(preflight(origin)).await.unwrap();
        assert_eq!(refused.status(), StatusCode::FORBIDDEN, "{origin}");
        assert!(
            refused
                .headers()
                .keys()
                .all(|name| !name.as_str().starts_with("access-control-")),
            "{origin}"
        );
        // A foreign origin's call gets no CORS header either: the browser hides it.
        let call = app
            .call(
                http::Request::post(LIST)
                    .header("content-type", "application/grpc-web+proto")
                    .header("origin", origin)
                    .body(Body::from(vec![0u8; 5]))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(call.status(), StatusCode::FORBIDDEN, "{origin}");
        assert!(!call.headers().contains_key("access-control-allow-origin"));
    }
}

#[tokio::test]
async fn rejected_requests_cannot_log_out_a_session() {
    let (mut app, fixture) = app(Arc::default()).await;
    let cookie = format!("kajiba_session={SESSION}");
    for (origin, content_type, expected) in [
        (None, None, StatusCode::UNSUPPORTED_MEDIA_TYPE),
        (None, Some("text/plain"), StatusCode::UNSUPPORTED_MEDIA_TYPE),
        (
            Some(FORGE),
            Some("text/plain"),
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
        ),
        (
            Some("https://evil.example.org"),
            Some("text/plain"),
            StatusCode::FORBIDDEN,
        ),
        (
            Some("https://evil.example.org"),
            Some("application/grpc"),
            StatusCode::FORBIDDEN,
        ),
        (
            Some("https://evil.example.org"),
            Some("application/grpc-web+proto"),
            StatusCode::FORBIDDEN,
        ),
        (
            Some("null"),
            Some("application/grpc"),
            StatusCode::FORBIDDEN,
        ),
        (None, Some("application/grpc"), StatusCode::OK),
        (
            Some(FORGE),
            Some("application/grpc-web+proto"),
            StatusCode::OK,
        ),
    ] {
        let mut request =
            http::Request::post("/kajiba.v1.AuthService/Logout").header("cookie", &cookie);
        if let Some(origin) = origin {
            request = request.header("origin", origin);
        }
        if let Some(content_type) = content_type {
            request = request.header("content-type", content_type);
        }
        let response = app
            .call(request.body(Body::from(vec![0u8; 5])).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), expected, "{origin:?} {content_type:?}");
        let remaining: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM session")
            .fetch_one(&mut *fixture.writer.lock().await)
            .await
            .unwrap();
        if expected == StatusCode::OK {
            assert_eq!(remaining, 0);
            let later = crate::transport::auth::now().unwrap() + 3_600_000;
            session(&fixture, "alice", SESSION, later).await;
        } else {
            assert!(!response.headers().contains_key("set-cookie"));
            assert_eq!(remaining, 1);
        }
    }
}

#[test]
fn forge_origin_drops_the_path_and_default_port() {
    for (url, origin) in [
        ("https://example.org/", "https://example.org"),
        ("https://example.org:443/forge", "https://example.org"),
        ("http://localhost:8080", "http://localhost:8080"),
    ] {
        assert_eq!(super::origin(url).unwrap(), origin);
    }
    assert!(super::origin("not a url").is_err());
}

#[test]
fn grpc_web_requests_log_their_grpc_code() {
    const CHILD: &str = "KAJIBA_TEST_GRPC_WEB_LOGS";
    if std::env::var_os(CHILD).is_some() {
        kajiba_core::log::init("info").unwrap();
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let (mut app, _fixture) = app(Arc::default()).await;
            let bearer = format!("Bearer {READ}");
            let response = web_call(&mut app, Some(("authorization", &bearer))).await;
            split(response).await;
            web_call(&mut app, None).await;
        });
        return;
    }
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "transport::web::tests::grpc_web_requests_log_their_grpc_code",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .env_remove("RUST_LOG")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let codes: Vec<String> = String::from_utf8(output.stderr)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .filter(|line| line["event"] == "request" && line["method"] == LIST)
        .map(|line| line["grpc_code"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(codes, ["OK", "UNAUTHENTICATED"]);
}
