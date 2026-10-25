use super::super::repos::tests::{DEMO, Fixture, git};
use axum::http;
use tonic::codegen::Service;

struct Unread;
impl http_body::Body for Unread {
    type Data = axum::body::Bytes;
    type Error = std::convert::Infallible;
    fn poll_frame(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        panic!("body must not be polled");
    }
}

#[tokio::test]
async fn upload_pack_leads_a_dedicated_process_group() {
    let mut child = super::command(
        std::path::Path::new("/nonexistent"),
        std::path::Path::new("/nonexistent/repo.git"),
        &["--stateless-rpc", "--advertise-refs"],
        &http::HeaderMap::new(),
    )
    .spawn()
    .unwrap();
    let pid = child.id().unwrap() as libc::pid_t;
    // SAFETY: the unreaped child reserves pid; getpgid only reads its group id.
    let group = unsafe { libc::getpgid(pid) };
    child.wait().await.unwrap();
    assert_eq!(group, pid);
}

fn gzip(body: &[u8]) -> Vec<u8> {
    // Allow for the gzip wrapper in addition to the zlib compression bound.
    let mut output = vec![0; zlib_rs::compress_bound(body.len()) + 16];
    let config = zlib_rs::DeflateConfig {
        window_bits: 31,
        ..Default::default()
    };
    let (data, code) = zlib_rs::compress_slice(&mut output, body, config);
    assert_eq!(code, zlib_rs::ReturnCode::Ok);
    data.to_vec()
}

#[test]
fn gunzip_accepts_bodies_up_to_the_cap() {
    let cap = 768 << 10;
    for size in [cap - 1, cap] {
        let body = vec![b'a'; size];
        assert_eq!(super::gunzip(&gzip(&body), cap).unwrap(), body);
    }
}

#[test]
fn gunzip_refuses_bodies_above_the_cap() {
    // Exercise both the initial allocation clamp and growth past a non-power-of-two cap.
    // The assertion before each allocation in gunzip checks that the buffer stays bounded.
    for cap in [2048, 768 << 10] {
        let body = gzip(&vec![b'a'; cap + 1]);
        assert_eq!(
            super::gunzip(&body, cap),
            Err(http::StatusCode::PAYLOAD_TOO_LARGE)
        );
    }
}

#[test]
fn gunzip_refuses_malformed_bodies() {
    let mut body = gzip(b"0000");
    body.truncate(body.len() - 1);
    for body in [b"not gzip".as_slice(), body.as_slice()] {
        assert_eq!(
            super::gunzip(body, 1024),
            Err(http::StatusCode::BAD_REQUEST)
        );
    }
}

#[tokio::test]
async fn gzip_budget_is_global_and_released_on_cancellation_and_error() {
    // No Git repository is needed: admitted requests stall before inflation or spawning git.
    let database = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
    sqlx::migrate!("./migrations").run(&database).await.unwrap();
    sqlx::raw_sql(
        "INSERT INTO user (id, public_id, name, kind, is_admin, source, created_at)
         VALUES (1, '00000000000000000000000001', 'alice', 'local', 0, 'config', 0);
         INSERT INTO repo (public_id, owner_id, name, visibility, ci_enabled,
                           default_branch, description, created_at)
         VALUES ('00000000000000000000000002', 1, 'demo', 'public', 0, 'main', '', 0);",
    )
    .execute(&database)
    .await
    .unwrap();
    let health = super::super::Health::new(Default::default(), Default::default(), None);
    health.started(database);
    let repos = super::Repos(health.clone());
    let mut app = super::super::router(health.clone(), false, Default::default()).unwrap();

    struct Pending(Option<tokio::sync::oneshot::Sender<()>>);
    impl http_body::Body for Pending {
        type Data = axum::body::Bytes;
        type Error = std::convert::Infallible;

        fn poll_frame(
            mut self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
            if let Some(polled) = self.0.take() {
                polled.send(()).unwrap();
            }
            std::task::Poll::Pending
        }
    }
    let request = |encoding: &str, body| {
        http::Request::post("/alice/demo.git/git-upload-pack")
            .header("content-type", "application/x-git-upload-pack-request")
            .header("content-encoding", encoding)
            .body(body)
            .unwrap()
    };

    let mut tasks = Vec::new();
    for (index, encoding) in ["gzip", "x-gzip", "gzip", "x-gzip"].iter().enumerate() {
        let (polled, ready) = tokio::sync::oneshot::channel();
        let request = request(encoding, axum::body::Body::new(Pending(Some(polled))));
        let mut app = app.clone();
        tasks.push(tokio::spawn(
            async move { app.call(request).await.unwrap() },
        ));
        tokio::time::timeout(std::time::Duration::from_secs(5), ready)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            health.public_reads.available_permits(),
            32 - (index + 1) * 8
        );
    }
    assert!(repos.read_permit(1).is_err());
    for encoding in ["gzip", "x-gzip"] {
        let response = app
            .call(request(encoding, axum::body::Body::new(Unread)))
            .await
            .unwrap();
        assert_eq!(response.status(), http::StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(response.headers()["retry-after"], "1");
    }

    let task = tasks.pop().unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert_eq!(health.public_reads.available_permits(), 8);
    let plain = repos.read_permit(1).unwrap();
    let response = app
        .call(request("gzip", axum::body::Body::new(Unread)))
        .await
        .unwrap();
    assert_eq!(response.status(), http::StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(health.public_reads.available_permits(), 7);
    drop(plain);
    let response = app
        .call(request("gzip", axum::body::Body::empty()))
        .await
        .unwrap();
    assert_eq!(response.status(), http::StatusCode::BAD_REQUEST);
    assert_eq!(health.public_reads.available_permits(), 8);
    for task in tasks {
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
    }
    assert_eq!(health.public_reads.available_permits(), 32);
}

/// Serves the whole public router on an ephemeral loopback port.
async fn serve(fixture: &Fixture) -> String {
    let app = super::super::router(fixture.repos.0.clone(), false, Default::default()).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

async fn call(fixture: &Fixture, path: &str) -> http::Response<axum::body::Body> {
    super::super::router(fixture.repos.0.clone(), false, Default::default())
        .unwrap()
        .call(
            http::Request::get(path)
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn text(response: http::Response<axum::body::Body>) -> String {
    let body = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    String::from_utf8(body.to_vec()).unwrap()
}

/// Clones `url` and returns its refs (`git ls-remote` of the clone) and its HEAD.
async fn clone(fixture: &Fixture, url: String, protocol: &str) -> Result<(String, String), String> {
    let dir = fixture.dir.join(format!("tmp/clone-{protocol}"));
    let _ = std::fs::remove_dir_all(&dir);
    let tmp = fixture.dir.join("tmp");
    let protocol = format!("protocol.version={protocol}");
    let output = tokio::process::Command::new("git")
        .current_dir(&tmp)
        .args([
            "-c",
            &protocol,
            "clone",
            "-q",
            "--mirror",
            &url,
            dir.to_str().unwrap(),
        ])
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .await
        .unwrap();
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned());
    }
    let refs = git(&dir, &["for-each-ref", "--format=%(refname) %(objectname)"]);
    let head = git(&dir, &["symbolic-ref", "HEAD"]);
    std::fs::remove_dir_all(dir).unwrap();
    Ok((refs, head))
}

#[tokio::test]
async fn clones_through_the_router_by_name_and_redirect_v0_and_v2() {
    let fixture = Fixture::new(&format!("{DEMO}default_branch='trunk'\n")).await;
    fixture
        .fill(
            "alice/demo",
            &[("README.md", "demo\n"), ("src/lib.rs", "")],
            &["refs/heads/trunk", "refs/heads/feature/x", "refs/tags/v1"],
        )
        .await;
    let bare = fixture.dir.join(format!(
        "repos/{}.git",
        crate::db::repos::find(fixture.repos.readers().unwrap(), "alice", "demo")
            .await
            .unwrap()
            .unwrap()
            .public_id
    ));
    let expected = (
        git(
            &bare,
            &["for-each-ref", "--format=%(refname) %(objectname)"],
        ),
        "refs/heads/trunk\n".to_owned(),
    );
    let base = serve(&fixture).await;
    for protocol in ["0", "2"] {
        for name in ["alice/demo", "alice/old", "Alice/Demo"] {
            assert_eq!(
                clone(&fixture, format!("{base}/{name}.git"), protocol)
                    .await
                    .unwrap(),
                expected,
                "{name} v{protocol}"
            );
        }
    }
    let error = clone(&fixture, format!("{base}/alice/missing.git"), "2")
        .await
        .unwrap_err();
    assert!(
        error.contains("404") || error.contains("not found"),
        "{error}"
    );
}

#[tokio::test]
async fn matched_ui_routes_observe_request_counters_and_durations() {
    let fixture = Fixture::new(DEMO).await;
    let registry = std::sync::Arc::new(crate::metrics::Registry::default());
    let mut app = super::super::router(fixture.repos.0.clone(), false, registry.clone()).unwrap();
    for (path, status) in [
        ("/alice/old", http::StatusCode::MOVED_PERMANENTLY),
        ("/alice/demo", http::StatusCode::NOT_FOUND),
        ("/unknown", http::StatusCode::NOT_FOUND),
    ] {
        let response = app
            .call(
                http::Request::get(path)
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), status);
        text(response).await;
    }
    let metrics = registry.exposition("test", [0; 4], [0; 4]);
    for line in [
        "kajiba_requests_total{kind=\"ui\",code=\"3xx\"} 1\n",
        "kajiba_requests_total{kind=\"ui\",code=\"4xx\"} 1\n",
        "kajiba_request_duration_seconds_count{kind=\"ui\"} 2\n",
        "kajiba_requests_total{kind=\"other\",code=\"4xx\"} 1\n",
        "kajiba_request_duration_seconds_count{kind=\"other\"} 1\n",
    ] {
        assert!(metrics.contains(line), "missing {line} in {metrics}");
    }
}

#[tokio::test]
async fn unknown_archived_and_ui_routes_answer_plain_statuses() {
    let mut fixture = Fixture::new(DEMO).await;
    let advertise = "/alice/demo.git/info/refs?service=git-upload-pack";
    let response = call(&fixture, advertise).await;
    assert_eq!(response.status(), http::StatusCode::OK);
    assert_eq!(
        response.headers()["content-type"],
        "application/x-git-upload-pack-advertisement"
    );
    assert!(
        text(response)
            .await
            .starts_with("001e# service=git-upload-pack\n0000")
    );
    for (path, status) in [
        ("/alice/missing.git/info/refs?service=git-upload-pack", 404),
        ("/alice/demo/info/refs?service=git-upload-pack", 404),
        ("/alice/demo.git/info/refs?service=git-receive-pack", 403),
        ("/alice/demo.git/info/refs", 403),
        ("/alice/demo", 404),
        ("/alice/missing", 404),
    ] {
        assert_eq!(
            call(&fixture, path).await.status().as_u16(),
            status,
            "{path}"
        );
    }
    let moved = call(&fixture, "/alice/old").await;
    assert_eq!(moved.status(), http::StatusCode::MOVED_PERMANENTLY);
    assert_eq!(moved.headers()["location"], "/alice/demo");

    // Archived: git refused by name and by redirect, the UI redirect stays.
    fixture.apply("").await;
    for path in [
        advertise,
        "/alice/old.git/info/refs?service=git-upload-pack",
    ] {
        let response = call(&fixture, path).await;
        assert_eq!(response.status(), http::StatusCode::FORBIDDEN, "{path}");
        assert!(text(response).await.contains("archived"));
    }
    let response = super::super::router(fixture.repos.0.clone(), false, Default::default())
        .unwrap()
        .call(
            http::Request::post("/alice/demo.git/git-upload-pack")
                .header("content-type", "application/x-git-upload-pack-request")
                .body(axum::body::Body::from("0000"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), http::StatusCode::FORBIDDEN);
    assert_eq!(
        call(&fixture, "/alice/old").await.status(),
        http::StatusCode::MOVED_PERMANENTLY
    );
}

#[tokio::test]
async fn public_read_budget_is_shared_and_refuses_before_reading_requests() {
    let fixture = Fixture::new(DEMO).await;
    let permits: Vec<_> = (0..super::super::PUBLIC_READS)
        .map(|_| fixture.repos.read_permit(1).unwrap())
        .collect();
    for path in [
        "/alice/demo.git/info/refs?service=git-upload-pack",
        "/raw/alice/demo/main/README.md",
    ] {
        let response = call(&fixture, path).await;
        assert_eq!(response.status(), http::StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(response.headers()["retry-after"], "1");
    }

    let response = super::super::router(fixture.repos.0.clone(), false, Default::default())
        .unwrap()
        .call(
            http::Request::post("/alice/demo.git/git-upload-pack")
                .header("content-type", "application/x-git-upload-pack-request")
                .body(axum::body::Body::new(Unread))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), http::StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(response.headers()["retry-after"], "1");
    drop(permits);
    let response = call(
        &fixture,
        "/alice/demo.git/info/refs?service=git-upload-pack",
    )
    .await;
    assert_eq!(response.status(), http::StatusCode::OK);
    assert_eq!(
        fixture.repos.0.public_reads.available_permits(),
        super::super::PUBLIC_READS - 1
    );
    drop(response);
    let _all = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        fixture
            .repos
            .0
            .public_reads
            .acquire_many(super::super::PUBLIC_READS as u32),
    )
    .await
    .unwrap()
    .unwrap();
}

async fn post(
    fixture: &Fixture,
    encodings: &[&str],
    body: axum::body::Body,
) -> http::Response<axum::body::Body> {
    let mut request = http::Request::post("/alice/demo.git/git-upload-pack")
        .header("content-type", "application/x-git-upload-pack-request");
    for encoding in encodings {
        request = request.header("content-encoding", *encoding);
    }
    super::super::router(fixture.repos.0.clone(), false, Default::default())
        .unwrap()
        .call(request.body(body).unwrap())
        .await
        .unwrap()
}

#[tokio::test]
async fn upload_pack_accepts_plain_gzip_and_x_gzip_through_the_router() {
    let fixture = Fixture::new(DEMO).await;
    fixture
        .fill("alice/demo", &[("README.md", "demo\n")], &[])
        .await;
    let bare = fixture.repos.git_dir("alice", "demo").await.unwrap();
    let head = git(&bare, &["rev-parse", "HEAD"]);
    let request = format!("0032want {}\n00000009done\n", head.trim());
    for encoding in [None, Some("gzip"), Some("x-gzip")] {
        let encodings: Vec<_> = encoding.into_iter().collect();
        let bytes = if encoding.is_some() {
            gzip(request.as_bytes())
        } else {
            request.as_bytes().to_vec()
        };
        let response = post(&fixture, &encodings, axum::body::Body::from(bytes)).await;
        assert_eq!(response.status(), http::StatusCode::OK);
        assert_eq!(
            response.headers()["content-type"],
            "application/x-git-upload-pack-result"
        );
        let body = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap();
        assert!(body.starts_with(b"0008NAK\nPACK"), "{encoding:?}: {body:?}");
    }
}

#[tokio::test]
async fn upload_pack_rejects_multiple_or_unsupported_encodings_before_reading() {
    let fixture = Fixture::new(DEMO).await;
    for encodings in [
        vec!["br"],
        vec!["identity"],
        vec![""],
        vec!["gzip, x-gzip"],
        vec!["gzip, gzip"],
        vec!["gzip", "gzip"],
        vec!["gzip", "br"],
        vec!["br", "gzip"],
        vec!["gzip", "x-gzip"],
    ] {
        let response = post(&fixture, &encodings, axum::body::Body::new(Unread)).await;
        assert_eq!(
            response.status(),
            http::StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "{encodings:?}"
        );
    }
}

#[tokio::test]
async fn upload_pack_enforces_gzip_validity_and_wire_and_inflated_limits() {
    let fixture = Fixture::new(DEMO).await;
    for encoding in ["gzip", "x-gzip"] {
        let mut truncated = gzip(b"0000");
        truncated.pop();
        for bytes in [b"not gzip".to_vec(), truncated] {
            let response = post(&fixture, &[encoding], axum::body::Body::from(bytes)).await;
            assert_eq!(response.status(), http::StatusCode::BAD_REQUEST);
            assert_eq!(text(response).await, "invalid gzip body\n");
        }
        let response = post(
            &fixture,
            &[encoding],
            axum::body::Body::from(vec![0; super::REQUEST_LIMIT + 1]),
        )
        .await;
        assert_eq!(response.status(), http::StatusCode::PAYLOAD_TOO_LARGE);
        let compressed = gzip(&vec![b'a'; super::REQUEST_LIMIT * 4 + 1]);
        assert!(compressed.len() < super::REQUEST_LIMIT);
        let response = post(&fixture, &[encoding], axum::body::Body::from(compressed)).await;
        assert_eq!(response.status(), http::StatusCode::PAYLOAD_TOO_LARGE);
    }
    let response = post(
        &fixture,
        &[],
        axum::body::Body::from(vec![0; super::REQUEST_LIMIT + 1]),
    )
    .await;
    assert_eq!(response.status(), http::StatusCode::PAYLOAD_TOO_LARGE);
}
