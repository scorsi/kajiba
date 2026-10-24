//! `/raw/<owner>/<name>/<ref>/<path>` (ADR 0005, 0021, 0026): a file at a branch,
//! tag or commit, the machine download path; refused for an archived repository.

use super::repos::Repos;
use axum::{
    Router,
    extract::{Path, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use kajiba_git::{Error, GitStore, Gix};

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
    let blob = tokio::task::spawn_blocking(move || {
        let (commit, file) = Gix.resolve(&path, &spec)?;
        Gix.blob(&path, &commit, file)
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
        _ => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

#[cfg(test)]
mod tests;
