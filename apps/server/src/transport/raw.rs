//! `/raw/<owner>/<name>/<ref>/<path>` (ADR 0005, 0021, 0026): a file at a branch,
//! tag or commit, the machine download path; refused for an archived repository.

use super::{
    repos::Repos,
    stream::{self, STREAM_IDLE, STREAM_LIFETIME},
};
use axum::{
    Router,
    body::{Body, Bytes},
    extract::{Path, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use http_body::Frame;
use kajiba_gitstore::{Error, GitStore, Gix};
use std::time::Duration;
use tokio::sync::OwnedSemaphorePermit;

const BLOB_LIMIT: u64 = 16 << 20;
/// Bytes handed to HTTP per chunk; copied, so the blob allocation ends with the permit.
const CHUNK: usize = 64 << 10;

pub fn routes(repos: Repos) -> Router {
    Router::new()
        .route("/raw/:owner/:name/*spec", get(raw))
        .with_state(repos)
}

/// The blob in bounded chunks under the upload-pack bounds (ADR 0040, 0043): the
/// permit is released when the body ends or when the first bound fires.
fn body(data: Vec<u8>, permit: OwnedSemaphorePermit, idle: Duration, lifetime: Duration) -> Body {
    stream::supervised(permit, |chunks| async move {
        let transfer = async {
            for chunk in data.chunks(CHUNK) {
                tokio::time::timeout(
                    idle,
                    stream::send(&chunks, Frame::data(Bytes::copy_from_slice(chunk))),
                )
                .await??;
            }
            Ok(())
        };
        tokio::time::timeout(lifetime, transfer)
            .await
            .unwrap_or_else(|err| Err(err.into()))
    })
}

async fn raw(
    State(repos): State<Repos>,
    Path((owner, name, spec)): Path<(String, String, String)>,
) -> Response {
    if kajiba_gitstore::check_revision(&spec).is_err() {
        return (StatusCode::PAYLOAD_TOO_LARGE, "revision too large\n").into_response();
    }
    let path = match repos.git_dir(&owner, &name).await {
        Ok(path) => path,
        Err(response) => return response,
    };
    // Ref names have neither empty nor `..` segments: the whole spec is a valid path.
    if kajiba_core::names::validate_path(&spec).is_err() {
        return (StatusCode::BAD_REQUEST, "invalid path\n").into_response();
    }
    let permit = match repos.read_permit() {
        Ok(permit) => permit,
        Err(response) => return response,
    };
    // The permit travels with the blocking work and its allocation, including after
    // handler cancellation.
    let root = repos.0.git_state_dir.join("repos");
    let blob = tokio::task::spawn_blocking(move || {
        let (commit, file) = Gix::shared(&root).resolve(&path, &spec)?;
        Gix::shared(&root)
            .blob(&path, &commit, file, BLOB_LIMIT)
            .map(|data| (data, permit))
    })
    .await;
    match blob {
        Ok(Ok((data, permit))) => (
            [
                (header::CONTENT_TYPE, "application/octet-stream"),
                (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            ],
            body(data, permit, STREAM_IDLE, STREAM_LIFETIME),
        )
            .into_response(),
        Ok(Err(Error::NotFound)) => (StatusCode::NOT_FOUND, "file not found\n").into_response(),
        // The blob past 16 MiB, or a commit or tree object past its cap (ADR 0047).
        Ok(Err(Error::TooLarge(_))) => {
            (StatusCode::PAYLOAD_TOO_LARGE, "object too large\n").into_response()
        }
        _ => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

#[cfg(test)]
mod tests;
