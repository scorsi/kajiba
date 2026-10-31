use crate::metrics::{Kind, Registry};
use axum::{
    extract::{MatchedPath, Request, State},
    http::StatusCode,
    middleware::Next,
    response::Response,
};
pub use kajiba_core::correlation::RequestId;
use std::sync::Arc;

pub(super) async fn request(
    State(metrics): State<Arc<Registry>>,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let content_kind = Kind::from_content_type(
        request
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or(""),
    );
    let path = request.uri().path();
    let method = match known(path) {
        Some(method) => method,
        // Route classes, never the raw path (0022).
        None if path.starts_with("/raw/") => "raw",
        None if path.ends_with(".git/info/refs") => "git-info-refs",
        None if path.ends_with(".git/git-upload-pack") => "git-upload-pack",
        None if matches!(content_kind, Kind::Other)
            && request
                .extensions()
                .get::<MatchedPath>()
                .is_some_and(|path| path.as_str() == "/:owner/:name") =>
        {
            "ui"
        }
        None => "other",
    };
    let kind = match method {
        "git-info-refs" | "git-upload-pack" => Kind::GitHttp,
        "raw" => Kind::Raw,
        "ui" => Kind::Ui,
        _ => content_kind,
    };
    kajiba_core::correlation::request(
        request,
        next,
        method,
        matches!(kind, Kind::Grpc | Kind::GrpcWeb),
        move |status, duration| metrics.observe(kind, status, duration),
    )
    .await
}

/// Paths logged as themselves: closed `method` labels (0022).
fn known(path: &str) -> Option<&'static str> {
    Some(match path {
        "/healthz" => "healthz",
        "/readyz" => "readyz",
        "/kajiba.admin.v1.AdminService/Enroll" => "/kajiba.admin.v1.AdminService/Enroll",
        "/kajiba.admin.v1.AdminService/ListRepos" => "/kajiba.admin.v1.AdminService/ListRepos",
        "/kajiba.admin.v1.AdminService/Import" => "/kajiba.admin.v1.AdminService/Import",
        "/kajiba.v1.AuthService/Logout" => "/kajiba.v1.AuthService/Logout",
        "/kajiba.v1.AuthService/GetViewer" => "/kajiba.v1.AuthService/GetViewer",
        "/kajiba.v1.HealthService/Check" => "/kajiba.v1.HealthService/Check",
        "/kajiba.v1.RepoService/GetRepo" => "/kajiba.v1.RepoService/GetRepo",
        "/kajiba.v1.RepoService/ListRepos" => "/kajiba.v1.RepoService/ListRepos",
        "/kajiba.v1.RepoService/GetTree" => "/kajiba.v1.RepoService/GetTree",
        "/kajiba.v1.RepoService/GetBlob" => "/kajiba.v1.RepoService/GetBlob",
        "/kajiba.v1.RepoService/ListCommits" => "/kajiba.v1.RepoService/ListCommits",
        "/kajiba.v1.RepoService/GetCommitDiff" => "/kajiba.v1.RepoService/GetCommitDiff",
        "/kajiba.v1.RunService/TriggerRun" => "/kajiba.v1.RunService/TriggerRun",
        "/kajiba.v1.RunService/GetRun" => "/kajiba.v1.RunService/GetRun",
        "/kajiba.v1.RunService/ListRuns" => "/kajiba.v1.RunService/ListRuns",
        "/kajiba.v1.TokenService/CreateToken" => "/kajiba.v1.TokenService/CreateToken",
        "/kajiba.v1.TokenService/ListTokens" => "/kajiba.v1.TokenService/ListTokens",
        "/kajiba.v1.TokenService/RevokeToken" => "/kajiba.v1.TokenService/RevokeToken",
        "/kajiba.runner.v1.RunnerService/Connect" => "/kajiba.runner.v1.RunnerService/Connect",
        "/kajiba.runner.v1.RunnerService/FetchRunBundle" => {
            "/kajiba.runner.v1.RunnerService/FetchRunBundle"
        }
        "/grpc.reflection.v1.ServerReflection/ServerReflectionInfo" => {
            "/grpc.reflection.v1.ServerReflection/ServerReflectionInfo"
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{Body, Bytes};
    use http_body::{Body as HttpBody, Frame};
    use std::{
        pin::Pin,
        task::{Context, Poll},
    };
    struct FailingOrPending(bool);
    impl HttpBody for FailingOrPending {
        type Data = Bytes;
        type Error = std::io::Error;

        fn poll_frame(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
            if self.0 {
                Poll::Ready(Some(Err(std::io::Error::other("body failed"))))
            } else {
                Poll::Pending
            }
        }
    }

    #[test]
    fn every_public_method_is_logged_by_name() {
        for path in kajiba_proto::method_access().unwrap().keys() {
            assert_eq!(known(path), Some(path.as_str()));
        }
    }

    #[tokio::test]
    async fn cancelled_and_failing_git_and_raw_responses_have_no_grpc_code() {
        const CHILD: &str = "KAJIBA_TEST_HTTP_BODY_LOGS";
        if std::env::var_os(CHILD).is_some() {
            use tonic::codegen::Service;
            kajiba_core::log::init("info").unwrap();
            let mut app = axum::Router::new()
                .fallback(|request: Request| async move {
                    Body::new(FailingOrPending(request.uri().query() == Some("fail")))
                })
                .layer(axum::middleware::from_fn_with_state(
                    Arc::new(Registry::default()),
                    request,
                ));
            for path in [
                "/alice/demo.git/info/refs",
                "/alice/demo.git/git-upload-pack",
                "/raw/alice/demo/main/README.md",
            ] {
                for fail in [false, true] {
                    let uri = format!("{path}{}", if fail { "?fail" } else { "" });
                    let response = app
                        .call(Request::builder().uri(uri).body(Body::empty()).unwrap())
                        .await
                        .unwrap();
                    if fail {
                        assert!(
                            axum::body::to_bytes(response.into_body(), 1024)
                                .await
                                .is_err()
                        );
                    } else {
                        drop(response);
                    }
                }
            }
            return;
        }
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "transport::correlation::tests::cancelled_and_failing_git_and_raw_responses_have_no_grpc_code",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env_remove("RUST_LOG")
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let logs: Vec<serde_json::Value> = String::from_utf8(output.stderr)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(logs.len(), 6, "{logs:?}");
        for method in ["git-info-refs", "git-upload-pack", "raw"] {
            let requests: Vec<_> = logs
                .iter()
                .filter(|line| line["method"] == method)
                .collect();
            assert_eq!(requests.len(), 2, "{logs:?}");
            assert_eq!(requests[0]["level"], "info");
            assert_eq!(requests[1]["level"], "error");
            for request in requests {
                assert_eq!(request["event"], "request");
                assert_eq!(request["status"], 200);
                assert!(request.get("grpc_code").is_none(), "{request}");
            }
        }
    }
}
