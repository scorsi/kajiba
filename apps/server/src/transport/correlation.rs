use crate::metrics::{Kind, Registry};
use axum::{
    body::{Body, Bytes},
    extract::{MatchedPath, Request, State},
    http::{HeaderMap, StatusCode},
    middleware::Next,
    response::Response,
};
use http_body::{Body as HttpBody, Frame, SizeHint};
use prost::Message;
use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Instant,
};
use tracing::{Instrument, Span};

use kajiba_core::new_id as request_id;

/// The request id, for handlers that record it (a correlation id, 0022).
#[derive(Clone)]
pub struct RequestId(pub String);

pub(super) async fn request(
    State(metrics): State<Arc<Registry>>,
    mut request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let content_kind = Kind::from_content_type(
        request
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or(""),
    );
    let method = match request.uri().path() {
        "/healthz" => "healthz",
        "/readyz" => "readyz",
        "/kajiba.admin.v1.AdminService/Enroll" => "/kajiba.admin.v1.AdminService/Enroll",
        "/kajiba.admin.v1.AdminService/ListRepos" => "/kajiba.admin.v1.AdminService/ListRepos",
        "/kajiba.admin.v1.AdminService/Import" => "/kajiba.admin.v1.AdminService/Import",
        "/kajiba.v1.HealthService/Check" => "/kajiba.v1.HealthService/Check",
        "/kajiba.v1.RepoService/GetRepo" => "/kajiba.v1.RepoService/GetRepo",
        "/kajiba.v1.RepoService/ListRepos" => "/kajiba.v1.RepoService/ListRepos",
        "/kajiba.v1.RepoService/GetTree" => "/kajiba.v1.RepoService/GetTree",
        "/kajiba.v1.RepoService/GetBlob" => "/kajiba.v1.RepoService/GetBlob",
        "/kajiba.v1.RepoService/ListCommits" => "/kajiba.v1.RepoService/ListCommits",
        "/kajiba.v1.RepoService/GetCommitDiff" => "/kajiba.v1.RepoService/GetCommitDiff",
        "/grpc.reflection.v1.ServerReflection/ServerReflectionInfo" => {
            "/grpc.reflection.v1.ServerReflection/ServerReflectionInfo"
        }
        // Route classes, never the raw path (0022).
        path if path.starts_with("/raw/") => "raw",
        path if path.ends_with(".git/info/refs") => "git-info-refs",
        path if path.ends_with(".git/git-upload-pack") => "git-upload-pack",
        _ if matches!(content_kind, Kind::Other)
            && request
                .extensions()
                .get::<MatchedPath>()
                .is_some_and(|path| path.as_str() == "/:owner/:name") =>
        {
            "ui"
        }
        _ => "other",
    };
    let started = Instant::now();
    let request_id = request_id().map_err(|err| request_id_failed(method, started, &err))?;
    let span = tracing::info_span!("request", request_id = %request_id);
    request
        .extensions_mut()
        .insert(RequestId(request_id.clone()));
    let kind = match method {
        "git-info-refs" | "git-upload-pack" => Kind::GitHttp,
        "raw" => Kind::Raw,
        "ui" => Kind::Ui,
        _ => content_kind,
    };
    let mut body = RequestBody {
        metrics,
        kind,
        inner: Body::empty(),
        span,
        request_id,
        method,
        status: "CANCELLED".into(),
        http_status: None,
        error: None,
        started,
        finished: false,
        grpc: matches!(kind, Kind::Grpc | Kind::GrpcWeb),
    };
    let mut response = next.run(request).instrument(body.span.clone()).await;
    let details = {
        let _entered = body.span.enter();
        grpc_details(response.headers_mut(), &body.request_id)
    };
    body.grpc |= details.is_some();
    let (status, error) = details.unwrap_or_else(|| (response.status().as_u16().to_string(), None));
    let (parts, inner) = response.into_parts();
    body.http_status = Some(parts.status.as_u16());
    body.inner = inner;
    body.status = status;
    body.error = error;
    Ok(Response::from_parts(parts, Body::new(body)))
}

// The process survives a failed request id: log the request, never `fatal`.
fn request_id_failed(method: &str, started: Instant, err: &std::io::Error) -> StatusCode {
    let status = StatusCode::INTERNAL_SERVER_ERROR;
    let dur_us = started.elapsed().as_micros() as u64;
    tracing::error!(event = "request", method, status = status.as_u16(), dur_us, err = %format!("request id: {err}"));
    status
}

// gRPC can finish in either the initial headers or the final trailers.
fn grpc_details(headers: &mut HeaderMap, request_id: &str) -> Option<(String, Option<String>)> {
    let status = tonic::Status::from_header_map(headers)?;
    if status.code() == tonic::Code::Ok {
        return Some(("OK".into(), None));
    }
    let mut details = tonic_types::pb::Status::decode(status.details()).unwrap_or_default();
    details.code = status.code() as i32;
    details.message = status.message().into();
    let mut info = details
        .details
        .iter()
        .find_map(|detail| {
            (detail.type_url == "type.googleapis.com/google.rpc.ErrorInfo")
                .then(|| tonic_types::pb::ErrorInfo::decode(detail.value.as_slice()).ok())
                .flatten()
        })
        // Only the handler knows the situation behind a code (ADR 0020); never invent a reason.
        .unwrap_or_default();
    if status.code() == tonic::Code::Internal {
        info.reason = "INTERNAL".into();
        info.metadata.clear();
        details.details.clear();
    }
    info.domain = "kajiba".into();
    info.metadata.insert("request_id".into(), request_id.into());
    details
        .details
        .retain(|detail| detail.type_url != "type.googleapis.com/google.rpc.ErrorInfo");
    details.details.push(Default::default());
    let detail = details.details.last_mut().unwrap();
    detail.type_url = "type.googleapis.com/google.rpc.ErrorInfo".into();
    detail.value = info.encode_to_vec();
    if status.code() == tonic::Code::Internal {
        details.message = "internal error".into();
    }
    let enriched = tonic::Status::with_details(
        status.code(),
        &details.message,
        details.encode_to_vec().into(),
    );
    enriched.add_header(headers).expect("encoded gRPC status");
    let error = (status.code() == tonic::Code::Internal).then(|| status.message().to_owned());
    Some((code_name(status.code()).into(), error))
}

fn code_name(code: tonic::Code) -> &'static str {
    use tonic::Code;
    match code {
        Code::Ok => "OK",
        Code::Cancelled => "CANCELLED",
        Code::Unknown => "UNKNOWN",
        Code::InvalidArgument => "INVALID_ARGUMENT",
        Code::DeadlineExceeded => "DEADLINE_EXCEEDED",
        Code::NotFound => "NOT_FOUND",
        Code::AlreadyExists => "ALREADY_EXISTS",
        Code::PermissionDenied => "PERMISSION_DENIED",
        Code::ResourceExhausted => "RESOURCE_EXHAUSTED",
        Code::FailedPrecondition => "FAILED_PRECONDITION",
        Code::Aborted => "ABORTED",
        Code::OutOfRange => "OUT_OF_RANGE",
        Code::Unimplemented => "UNIMPLEMENTED",
        Code::Internal => "INTERNAL",
        Code::Unavailable => "UNAVAILABLE",
        Code::DataLoss => "DATA_LOSS",
        Code::Unauthenticated => "UNAUTHENTICATED",
    }
}

struct RequestBody {
    metrics: Arc<Registry>,
    kind: Kind,
    inner: Body,
    span: Span,
    request_id: String,
    method: &'static str,
    // gRPC code name or HTTP status: the metrics `code` label.
    status: String,
    // None when the request is dropped before any response.
    http_status: Option<u16>,
    error: Option<String>,
    started: Instant,
    finished: bool,
    grpc: bool,
}

impl RequestBody {
    fn finish(&mut self) {
        if !self.finished {
            let _entered = self.span.enter();
            let duration = self.started.elapsed();
            self.metrics.observe(self.kind, &self.status, duration);
            let dur_us = duration.as_micros() as u64;
            // `status` is the HTTP status; gRPC adds its code name in `grpc_code`.
            let grpc_code =
                (self.grpc && self.status.parse::<u16>().is_err()).then_some(self.status.as_str());
            if let Some(err) = &self.error {
                tracing::error!(
                    event = "request",
                    method = self.method,
                    status = self.http_status,
                    grpc_code,
                    dur_us,
                    err
                );
            } else {
                tracing::info!(
                    event = "request",
                    method = self.method,
                    status = self.http_status,
                    grpc_code,
                    dur_us
                );
            }
            self.finished = true;
        }
    }
}

impl HttpBody for RequestBody {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        let span = self.span.clone();
        let _entered = span.enter();
        let frame = Pin::new(&mut self.inner).poll_frame(cx);
        match frame {
            Poll::Ready(Some(Ok(mut frame))) => {
                if let Some((status, error)) = frame
                    .trailers_mut()
                    .and_then(|trailers| grpc_details(trailers, &self.request_id))
                {
                    self.grpc = true;
                    self.status = status;
                    self.error = error;
                }
                if self.inner.is_end_stream() {
                    self.finish();
                }
                Poll::Ready(Some(Ok(frame)))
            }
            Poll::Ready(None) => {
                self.finish();
                Poll::Ready(None)
            }
            Poll::Ready(Some(Err(err))) => {
                self.status = "INTERNAL".into();
                self.error = Some(err.to_string());
                self.finish();
                Poll::Ready(Some(Err(err)))
            }
            Poll::Pending => Poll::Pending,
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

impl Drop for RequestBody {
    fn drop(&mut self) {
        if !self.finished && !self.inner.is_end_stream() {
            self.status = "CANCELLED".into();
        }
        self.finish();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::convert::Infallible;
    use tonic::{Code, Status};

    #[test]
    fn enrichment_preserves_existing_reason_metadata_and_other_details() {
        use tonic_types::{ErrorDetails, StatusExt};
        let mut details = ErrorDetails::new();
        details.set_error_info(
            "DECLARED_IN_CONFIG",
            "kajiba",
            [
                ("request_id".into(), "old".into()),
                ("field".into(), "name".into()),
            ]
            .into_iter()
            .collect::<std::collections::HashMap<_, _>>(),
        );
        details.add_precondition_failure_violation("config", "name", "declared");
        let status = Status::with_error_details(Code::FailedPrecondition, "declared", details);
        let mut headers = HeaderMap::new();
        status.add_header(&mut headers).unwrap();
        headers.insert("custom-metadata", "kept".parse().unwrap());
        assert_eq!(
            grpc_details(&mut headers, "new")
                .map(|(status, _)| status)
                .as_deref(),
            Some("FAILED_PRECONDITION")
        );
        let status = Status::from_header_map(&headers).unwrap();
        assert_eq!(status.code(), Code::FailedPrecondition);
        let details = status.get_error_details();
        let info = details.error_info().unwrap();
        assert_eq!(info.reason, "DECLARED_IN_CONFIG");
        assert_eq!(info.domain, "kajiba");
        assert_eq!(info.metadata["request_id"], "new");
        assert_eq!(info.metadata["field"], "name");
        assert!(details.precondition_failure().is_some());
        assert_eq!(headers["custom-metadata"], "kept");
        assert_eq!(grpc_details(&mut HeaderMap::new(), "new"), None);
        let mut ok = HeaderMap::new();
        Status::ok("").add_header(&mut ok).unwrap();
        assert_eq!(
            grpc_details(&mut ok, "new")
                .map(|(status, _)| status)
                .as_deref(),
            Some("OK")
        );
        assert!(!ok.contains_key("grpc-status-details-bin"));
    }

    struct Trailers(Option<HeaderMap>);
    impl HttpBody for Trailers {
        type Data = Bytes;
        type Error = Infallible;
        fn poll_frame(
            mut self: Pin<&mut Self>,
            _: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
            tracing::info!(event = "body-polled");
            Poll::Ready(self.0.take().map(|headers| Ok(Frame::trailers(headers))))
        }
    }

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

    #[tokio::test]
    async fn streamed_error_trailers_receive_details_without_buffering() {
        let mut trailers = HeaderMap::new();
        Status::unavailable("startup")
            .add_header(&mut trailers)
            .unwrap();
        let inner = Body::new(Trailers(Some(trailers)));
        let mut body = RequestBody {
            metrics: Arc::default(),
            kind: Kind::Grpc,
            inner,
            span: tracing::info_span!("request", request_id = "test-id"),
            request_id: "test-id".into(),
            method: "other",
            status: "200".into(),
            http_status: Some(200),
            error: None,
            started: Instant::now(),
            finished: false,
            grpc: true,
        };
        assert!(!body.finished);
        let frame = std::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx))
            .await
            .unwrap()
            .unwrap();
        let trailers = frame.into_trailers().unwrap();
        let status = Status::from_header_map(&trailers).unwrap();
        assert_eq!(status.code(), Code::Unavailable);
        let details = tonic_types::pb::Status::decode(status.details()).unwrap();
        let info = tonic_types::pb::ErrorInfo::decode(details.details[0].value.as_slice()).unwrap();
        // No handler ErrorInfo: the reason stays empty rather than guessed.
        assert_eq!(info.reason, "");
        assert_eq!(info.domain, "kajiba");
        assert_eq!(info.metadata["request_id"], "test-id");
        assert_eq!(body.status, "UNAVAILABLE");
        assert!(
            std::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx))
                .await
                .is_none()
        );
        assert!(body.finished);
    }

    #[test]
    fn request_id_failure_logs_the_request_instead_of_fatal() {
        const CHILD: &str = "KAJIBA_TEST_REQUEST_ID_FAILURE";
        if std::env::var_os(CHILD).is_some() {
            kajiba_core::log::init("info").unwrap();
            let status = request_id_failed(
                "healthz",
                Instant::now(),
                &std::io::Error::other("no entropy"),
            );
            assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
            return;
        }
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "transport::correlation::tests::request_id_failure_logs_the_request_instead_of_fatal",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env_remove("RUST_LOG")
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let lines: Vec<serde_json::Value> = String::from_utf8(output.stderr)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert_eq!(lines[0]["event"], "request");
        assert_eq!(lines[0]["level"], "error");
        assert_eq!(lines[0]["method"], "healthz");
        assert_eq!(lines[0]["status"], 500);
        assert_eq!(lines[0]["err"], "request id: no entropy");
    }
}
