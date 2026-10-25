use super::*;
use kajiba_proto::git::v1::Session;

#[test]
fn golden_exposition() {
    assert_eq!(
        exposition("test", [1, 2, 3, 4], [[1, 2], [3, 4], [5, 6], [7, 8]]),
        include_str!("../../tests/fixtures/metrics.prom")
    );
    assert_eq!(CONTENT_TYPE, "text/plain; version=0.0.4; charset=utf-8");
}

#[test]
fn labels_are_closed_and_version_is_escaped() {
    for kind in [i32::MIN, -1, 0, 5, i32::MAX] {
        assert_eq!(kind_index(kind), None);
    }
    for (index, (kind, _)) in KINDS.iter().enumerate() {
        assert_eq!(kind_index(*kind as i32), Some(index));
    }
    let output = exposition("a\\b\"c\nd", [0; 4], [[0; 2]; 4]);
    assert!(output.contains("version=\"a\\\\b\\\"c\\nd\""));
    assert_eq!(
        output.lines().filter(|line| !line.starts_with('#')).count(),
        13
    );
}

#[test]
fn sessions_count_once_and_totals_survive_acknowledgement() {
    let repositories = Repositories::new("/unused".into());
    let mut completed = [[0; 2]; 4];
    for (index, (kind, _)) in KINDS.iter().enumerate() {
        for status in [0, 1, 128, -1] {
            let session = Session {
                session_id: format!("{index}-{status}"),
                kind: *kind as i32,
                repo_public_id: "private-repo".into(),
                user_public_id: "private-user".into(),
                correlation_id: "private-correlation".into(),
                generation: "private-generation".into(),
            };
            repositories.register(session.clone()).unwrap();
            assert!(repositories.register(session.clone()).is_err());
            let mut active = [0; 4];
            active[index] = 1;
            assert_eq!(
                repositories.metrics(),
                exposition(env!("CARGO_PKG_VERSION"), active, completed)
            );
            repositories.finished(&session.session_id, status);
            completed[index][usize::from(status != 0)] += 1;
            let expected = exposition(env!("CARGO_PKG_VERSION"), [0; 4], completed);
            assert_eq!(repositories.metrics(), expected);
            repositories.finished(&session.session_id, 0);
            repositories.finished("unknown", 1);
            assert_eq!(repositories.metrics(), expected);
            repositories.acknowledged(&session.session_id);
            repositories.acknowledged(&session.session_id);
            assert_eq!(repositories.metrics(), expected);
        }
    }
    assert_eq!(completed, [[1, 3]; 4]);
}

#[tokio::test]
async fn absent_configuration_opens_no_listener() {
    let config =
        kajiba_core::config::GitConfig::parse("hooks_path='/nix/store/example-hooks'").unwrap();
    assert!(bind(config.metrics.listen).await.unwrap().is_none());
}

#[tokio::test]
async fn endpoint_serves_only_metrics_with_prometheus_content_type() {
    use axum::{body::Body, http::Request};
    use tonic::codegen::Service;
    let repositories = Arc::new(Repositories::new("/unused".into()));
    let expected = repositories.metrics();
    let mut app = router(repositories);
    for (method, path, status) in [
        ("GET", "/metrics", 200),
        ("POST", "/metrics", 405),
        ("GET", "/readyz", 404),
        ("GET", "/", 404),
    ] {
        let response = app
            .call(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), status);
        if status == 200 {
            assert_eq!(response.headers()["content-type"], CONTENT_TYPE);
            let body = axum::body::to_bytes(response.into_body(), 4096)
                .await
                .unwrap();
            assert_eq!(body.as_ref(), expected.as_bytes());
        }
    }
}
