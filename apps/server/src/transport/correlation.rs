use axum::{
    body::{Body, Bytes},
    extract::Request,
    http::{HeaderMap, StatusCode},
    middleware::Next,
    response::Response,
};
use http_body::{Body as HttpBody, Frame, SizeHint};
use prost::Message;
use std::{
    io::Read,
    pin::Pin,
    task::{Context, Poll},
    time::{Instant, SystemTime, UNIX_EPOCH},
};
use tracing::{Instrument, Span};

fn request_id() -> std::io::Result<String> {
    let mut bytes = [0; 16];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes[6..])?;
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(std::io::Error::other)?
        .as_millis();
    let value = (millis << 80) | u128::from_be_bytes(bytes);
    Ok((0..26)
        .rev()
        .map(|shift| {
            b"0123456789ABCDEFGHJKMNPQRSTVWXYZ"[((value >> (shift * 5)) & 31) as usize] as char
        })
        .collect())
}

pub(super) async fn request(request: Request, next: Next) -> Result<Response, StatusCode> {
    let request_id = request_id().map_err(|err| {
        kajiba_core::log::error("fatal", &format!("request id: {err}"));
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    let span = tracing::info_span!("request", request_id = %request_id);
    let method = match request.uri().path() {
        "/healthz" => "healthz",
        "/readyz" => "readyz",
        "/kajiba.v1.HealthService/Check" => "/kajiba.v1.HealthService/Check",
        "/grpc.reflection.v1.ServerReflection/ServerReflectionInfo" => {
            "/grpc.reflection.v1.ServerReflection/ServerReflectionInfo"
        }
        "/grpc.reflection.v1alpha.ServerReflection/ServerReflectionInfo" => {
            "/grpc.reflection.v1alpha.ServerReflection/ServerReflectionInfo"
        }
        _ => "other",
    };
    let mut body = RequestBody {
        inner: Body::empty(),
        span,
        request_id,
        method,
        status: "CANCELLED".into(),
        error: None,
        started: Instant::now(),
        finished: false,
    };
    let mut response = next.run(request).instrument(body.span.clone()).await;
    let (status, error) = {
        let _entered = body.span.enter();
        grpc_details(response.headers_mut(), &body.request_id)
    }
    .unwrap_or_else(|| (response.status().as_u16().to_string(), None));
    let (parts, inner) = response.into_parts();
    body.inner = inner;
    body.status = status;
    body.error = error;
    Ok(Response::from_parts(parts, Body::new(body)))
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
        .unwrap_or_else(|| tonic_types::pb::ErrorInfo {
            reason: match status.code() {
                tonic::Code::InvalidArgument => "INVALID_FIELD",
                tonic::Code::Unavailable => "MAINTENANCE",
                _ => "INTERNAL",
            }
            .into(),
            ..Default::default()
        });
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
    inner: Body,
    span: Span,
    request_id: String,
    method: &'static str,
    status: String,
    error: Option<String>,
    started: Instant,
    finished: bool,
}

impl RequestBody {
    fn finish(&mut self) {
        if !self.finished {
            let _entered = self.span.enter();
            let dur_us = self.started.elapsed().as_micros() as u64;
            if let Some(err) = &self.error {
                tracing::error!(event = "request", method = self.method, status = %self.status, dur_us, err);
            } else {
                tracing::info!(event = "request", method = self.method, status = %self.status, dur_us);
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

    #[tokio::test]
    async fn streamed_error_trailers_receive_details_without_buffering() {
        let mut trailers = HeaderMap::new();
        Status::unavailable("startup")
            .add_header(&mut trailers)
            .unwrap();
        let inner = Body::new(Trailers(Some(trailers)));
        let mut body = RequestBody {
            inner,
            span: tracing::info_span!("request", request_id = "test-id"),
            request_id: "test-id".into(),
            method: "other",
            status: "200".into(),
            error: None,
            started: Instant::now(),
            finished: false,
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
        assert_eq!(info.reason, "MAINTENANCE");
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
}
