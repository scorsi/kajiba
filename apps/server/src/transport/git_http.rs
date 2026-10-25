//! Read-only smart HTTP (ADR 0003, 0021, 0026): `git upload-pack --stateless-rpc`.
//! Pushes go over SSH (0004).

use super::repos::Repos;
use super::stream::{self, STREAM_IDLE, STREAM_LIFETIME};
use axum::{
    Router,
    body::Bytes,
    extract::{Path, Query, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{any, get, post},
};
use kajiba_gitstore::{GitStore, Gix, Service};
use std::{collections::HashMap, process::Stdio};

pub fn routes(repos: Repos) -> Router {
    Router::new()
        .route("/:owner/:repo/info/refs", get(info_refs))
        .route("/:owner/:repo/git-upload-pack", post(upload_pack))
        // Any method: two-segment gRPC paths of unknown services must reach the refusal.
        .route("/:owner/:name", any(ui))
        .with_state(repos)
}

/// The `.git` suffix is part of the smart HTTP URL (0021).
async fn repository(
    repos: &Repos,
    owner: &str,
    repo: &str,
) -> Result<std::path::PathBuf, Response> {
    let name = repo
        .strip_suffix(".git")
        .ok_or_else(super::repos::not_found_http)?;
    repos.git_dir(owner, name).await
}

/// `Git-Protocol` is passed to git as `GIT_PROTOCOL`, as git-http-backend does, when
/// it holds only `key=value` items.
fn protocol(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("git-protocol")
        .and_then(|value| value.to_str().ok())
        .filter(|value| {
            value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"=:._-".contains(&b))
        })
}

fn command(
    root: &std::path::Path,
    path: &std::path::Path,
    options: &[&str],
    headers: &HeaderMap,
) -> tokio::process::Command {
    let mut command = tokio::process::Command::from(Gix::shared(root).transport(
        Service::UploadPack,
        path,
        options,
    ));
    command
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    if let Some(protocol) = protocol(headers) {
        command.env("GIT_PROTOCOL", protocol);
    }
    command
}

fn internal(_: std::io::Error) -> Response {
    StatusCode::INTERNAL_SERVER_ERROR.into_response()
}

async fn info_refs(
    State(repos): State<Repos>,
    Path((owner, repo)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let path = match repository(&repos, &owner, &repo).await {
        Ok(path) => path,
        Err(response) => return response,
    };
    if query.get("service").map(String::as_str) != Some("git-upload-pack") {
        return (
            StatusCode::FORBIDDEN,
            "only git-upload-pack is served over HTTP\n",
        )
            .into_response();
    }
    let permit = match repos.read_permit() {
        Ok(permit) => permit,
        Err(response) => return response,
    };
    let child = match command(
        &repos.0.git_state_dir.join("repos"),
        &path,
        &["--stateless-rpc", "--advertise-refs"],
        &headers,
    )
    .stdin(Stdio::null())
    .spawn()
    {
        Ok(child) => child,
        Err(err) => return internal(err),
    };
    // Protocol v2 starts with its capability advertisement; v0 with a service line.
    let prefix: &'static [u8] =
        if protocol(&headers).is_some_and(|p| p.split(':').any(|item| item == "version=2")) {
            b""
        } else {
            b"001e# service=git-upload-pack\n0000"
        };
    let body = stream::body(
        child,
        Vec::new(),
        prefix,
        permit,
        STREAM_IDLE,
        STREAM_LIFETIME,
    );
    (
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/x-git-upload-pack-advertisement"),
            ),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-cache")),
        ],
        body,
    )
        .into_response()
}

/// Requests hold wants and haves only; git gzips those up to its post buffer (1 MiB).
const REQUEST_LIMIT: usize = 16 << 20;

fn gunzip(body: &[u8], cap: usize) -> Result<Vec<u8>, StatusCode> {
    let mut size = (body.len().max(1024) * 4).min(cap);
    loop {
        debug_assert!(size <= cap);
        let mut output = vec![0; size];
        let config = zlib_rs::InflateConfig { window_bits: 31 };
        match zlib_rs::decompress_slice(&mut output, body, config) {
            (data, zlib_rs::ReturnCode::Ok) => return Ok(data.to_vec()),
            (_, zlib_rs::ReturnCode::BufError) if size < cap => size = (size * 2).min(cap),
            (_, zlib_rs::ReturnCode::BufError) => return Err(StatusCode::PAYLOAD_TOO_LARGE),
            _ => return Err(StatusCode::BAD_REQUEST),
        }
    }
}

async fn upload_pack(
    State(repos): State<Repos>,
    Path((owner, repo)): Path<(String, String)>,
    request: Request,
) -> Response {
    let path = match repository(&repos, &owner, &repo).await {
        Ok(path) => path,
        Err(response) => return response,
    };
    let (parts, body) = request.into_parts();
    if parts
        .headers
        .get(header::CONTENT_TYPE)
        .map(HeaderValue::as_bytes)
        != Some(b"application/x-git-upload-pack-request")
    {
        return StatusCode::UNSUPPORTED_MEDIA_TYPE.into_response();
    }
    let mut encodings = parts.headers.get_all(header::CONTENT_ENCODING).iter();
    let encoding = encodings.next().map(HeaderValue::as_bytes);
    if encodings.next().is_some() || !matches!(encoding, None | Some(b"gzip" | b"x-gzip")) {
        return StatusCode::UNSUPPORTED_MEDIA_TYPE.into_response();
    }
    let permit = match repos.read_permit() {
        Ok(permit) => permit,
        Err(response) => return response,
    };
    let Ok(body): Result<Bytes, _> = axum::body::to_bytes(body, REQUEST_LIMIT).await else {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    };
    let body = match encoding {
        None => body.to_vec(),
        Some(b"gzip" | b"x-gzip") => match gunzip(&body, REQUEST_LIMIT * 4) {
            Ok(body) => body,
            Err(StatusCode::BAD_REQUEST) => {
                return (StatusCode::BAD_REQUEST, "invalid gzip body\n").into_response();
            }
            Err(status) => return status.into_response(),
        },
        Some(_) => return StatusCode::UNSUPPORTED_MEDIA_TYPE.into_response(),
    };
    let child = match command(
        &repos.0.git_state_dir.join("repos"),
        &path,
        &["--stateless-rpc"],
        &parts.headers,
    )
    .stdin(Stdio::piped())
    .spawn()
    {
        Ok(child) => child,
        Err(err) => return internal(err),
    };
    (
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/x-git-upload-pack-result"),
            ),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-cache")),
        ],
        stream::body(child, body, b"", permit, STREAM_IDLE, STREAM_LIFETIME),
    )
        .into_response()
}

/// The UI route of a previous name answers 301 to the current one (0021); the UI
/// itself is not served yet, so anything else falls through to the plain 404.
async fn ui(
    State(repos): State<Repos>,
    Path((owner, name)): Path<(String, String)>,
    request: Request,
) -> Response {
    let refused = super::refusal(&request, StatusCode::NOT_FOUND, tonic::Code::Unimplemented);
    let Ok(readers) = repos.readers() else {
        return refused;
    };
    if request.method() != axum::http::Method::GET {
        return refused;
    }
    drop(request);
    match crate::db::repos::find(readers, &owner, &name).await {
        Ok(Some(found)) if found.redirected => (
            StatusCode::MOVED_PERMANENTLY,
            [(header::LOCATION, format!("/{}/{}", found.owner, found.name))],
        )
            .into_response(),
        Ok(_) => refused,
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

#[cfg(test)]
mod tests;
