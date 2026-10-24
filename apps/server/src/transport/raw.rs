//! `/raw/<owner>/<name>/<ref>/<path>` (ADR 0005, 0021, 0026): a file at a branch,
//! tag or commit, the machine download path; refused for an archived repository.

use super::repos::Repos;
use axum::{
    Router,
    body::Bytes,
    extract::{Path, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use kajiba_git::{Error, GitStore, Gix};

const BLOB_LIMIT: u64 = 16 << 20;

// Keep the permit with the allocation, including after handler cancellation.
struct Blob {
    data: Vec<u8>,
    _permit: tokio::sync::OwnedSemaphorePermit,
}

impl AsRef<[u8]> for Blob {
    fn as_ref(&self) -> &[u8] {
        &self.data
    }
}

pub fn routes(repos: Repos) -> Router {
    Router::new()
        .route("/raw/:owner/:name/*spec", get(raw))
        .with_state(repos)
}

async fn raw(
    State(repos): State<Repos>,
    Path((owner, name, spec)): Path<(String, String, String)>,
) -> Response {
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
    let blob = tokio::task::spawn_blocking(move || {
        let (commit, file) = Gix.resolve(&path, &spec)?;
        Gix.blob(&path, &commit, file, BLOB_LIMIT).map(|data| {
            Bytes::from_owner(Blob {
                data,
                _permit: permit,
            })
        })
    })
    .await;
    match blob {
        Ok(Ok(data)) => (
            [
                (header::CONTENT_TYPE, "application/octet-stream"),
                (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            ],
            data,
        )
            .into_response(),
        Ok(Err(Error::NotFound)) => (StatusCode::NOT_FOUND, "file not found\n").into_response(),
        Ok(Err(Error::TooLarge)) => {
            (StatusCode::PAYLOAD_TOO_LARGE, "blob too large\n").into_response()
        }
        _ => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

#[cfg(test)]
mod tests;
