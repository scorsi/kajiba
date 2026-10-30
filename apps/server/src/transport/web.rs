//! Browser access to the `kajiba.v1` routes: grpc-web translated by `tonic-web` in
//! front of the auth layer, and CORS answering only the forge's own origin (ADR 0005,
//! 0006, 0020).
use crate::metrics::Kind;
use axum::{
    body::Body,
    extract::{Request, State},
    http::{
        HeaderValue, Method, StatusCode,
        header::{
            ACCESS_CONTROL_ALLOW_CREDENTIALS, ACCESS_CONTROL_ALLOW_HEADERS,
            ACCESS_CONTROL_ALLOW_METHODS, ACCESS_CONTROL_ALLOW_ORIGIN,
            ACCESS_CONTROL_EXPOSE_HEADERS, ACCESS_CONTROL_MAX_AGE, CONTENT_TYPE, ORIGIN, VARY,
        },
    },
    middleware::Next,
    response::{IntoResponse, Response},
};
use tonic::{codegen::Service, service::Routes};
use webauthn_rs::prelude::Url;

/// The grpc-web translation of a copy of the `kajiba.v1` routes, auth layer included.
pub(super) type GrpcWeb = tonic_web::GrpcWebService<Routes>;

pub(super) fn grpc_web(api: axum::Router) -> GrpcWeb {
    tower_layer::Layer::layer(&tonic_web::GrpcWebLayer::new(), Routes::from(api))
}

/// grpc-web calls go through the translation; plain gRPC (HTTP/2 or test HTTP/1) and
/// preflights reach the routes unchanged.
pub(super) async fn translate(
    State(mut web): State<GrpcWeb>,
    request: Request,
    next: Next,
) -> Response {
    let content_type = request
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    match Kind::from_content_type(content_type) {
        Kind::GrpcWeb => {}
        Kind::Grpc => return next.run(request).await,
        _ => return StatusCode::UNSUPPORTED_MEDIA_TYPE.into_response(),
    }
    match web.call(request.map(tonic::body::boxed)).await {
        Ok(response) => response.map(Body::new),
        Err(err) => tonic::Status::internal(format!("grpc-web: {err}"))
            .into_http()
            .map(Body::new),
    }
}

/// `scheme://host[:port]` of `http.public_url`, as a browser sends it in `Origin`.
pub(super) fn origin(public_url: &str) -> Result<HeaderValue, String> {
    let origin = Url::parse(public_url)
        .map_err(|err| format!("http.public_url: {err}"))?
        .origin();
    if !origin.is_tuple() {
        return Err("http.public_url: no origin".into());
    }
    HeaderValue::from_str(&origin.ascii_serialization()).map_err(|err| err.to_string())
}

/// Only the forge's origin gets `Access-Control-Allow-*` headers; another origin's
/// requests are refused before dispatch, including simple requests (ADR 0020).
pub(super) async fn cors(
    State(forge): State<HeaderValue>,
    request: Request,
    next: Next,
) -> Response {
    let allowed = request.headers().get(ORIGIN) == Some(&forge);
    if request.headers().contains_key(ORIGIN) && !allowed {
        return (StatusCode::FORBIDDEN, [(VARY, "Origin")]).into_response();
    }
    let mut response = if request.method() == Method::OPTIONS {
        if !allowed {
            return (StatusCode::FORBIDDEN, [(VARY, "Origin")]).into_response();
        }
        (
            StatusCode::NO_CONTENT,
            [
                (ACCESS_CONTROL_ALLOW_METHODS, "POST"),
                (
                    ACCESS_CONTROL_ALLOW_HEADERS,
                    "content-type, x-grpc-web, x-user-agent, grpc-timeout, kajiba-client-version",
                ),
                (ACCESS_CONTROL_MAX_AGE, "600"),
            ],
        )
            .into_response()
    } else {
        next.run(request).await
    };
    let headers = response.headers_mut();
    headers.append(VARY, HeaderValue::from_static("Origin"));
    if allowed {
        headers.insert(ACCESS_CONTROL_ALLOW_ORIGIN, forge);
        headers.insert(
            ACCESS_CONTROL_ALLOW_CREDENTIALS,
            HeaderValue::from_static("true"),
        );
        headers.insert(
            ACCESS_CONTROL_EXPOSE_HEADERS,
            HeaderValue::from_static("grpc-status, grpc-message, grpc-status-details-bin"),
        );
    }
    response
}

#[cfg(test)]
mod tests;
