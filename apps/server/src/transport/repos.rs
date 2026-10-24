//! Read-only repository access (ADR 0020, 0021, 0026): `RepoService`.
// These feed `?` in tonic handlers, which return `Status` by value anyway.
#![allow(clippy::result_large_err)]

use super::{Health, cursor, local::refused};
use kajiba_proto::v1::{
    GetRepoRequest, GetRepoResponse, ListReposRequest, ListReposResponse, Mirror, Permission, Repo,
    Role, Visibility, get_repo_request, repo_service_server::RepoService,
};
use sqlx::{SqliteConnection, SqlitePool};
use tonic::{Request, Response, Status};

/// Repository routes read through the read pool, available once startup completed.
#[derive(Clone)]
pub struct Repos(pub Health);

impl Repos {
    pub fn readers(&self) -> Result<&SqlitePool, Status> {
        self.0
            .database
            .get()
            .ok_or_else(|| Status::unavailable("starting"))
    }
}

/// Git transport and `/raw/` (0021, 0026): a clean 404 for an unknown repository.
pub fn not_found_http() -> axum::response::Response {
    use axum::response::IntoResponse;
    (axum::http::StatusCode::NOT_FOUND, "repository not found\n").into_response()
}

impl Repos {
    /// The git directory of an active repository found by name or redirect; an
    /// archived one is refused with 403.
    pub async fn git_dir(
        &self,
        owner: &str,
        name: &str,
    ) -> Result<std::path::PathBuf, axum::response::Response> {
        use axum::{http::StatusCode, response::IntoResponse};
        let readers = self
            .readers()
            .map_err(|_| StatusCode::SERVICE_UNAVAILABLE.into_response())?;
        match crate::db::repos::find(readers, owner, name).await {
            Ok(Some(found)) if found.archived => {
                Err((StatusCode::FORBIDDEN, "repository archived\n").into_response())
            }
            Ok(Some(found)) => Ok(self
                .0
                .state_dir
                .join(format!("repos/{}.git", found.public_id))),
            Ok(None) => Err(not_found_http()),
            Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR.into_response()),
        }
    }
}

fn not_found() -> Status {
    refused(
        tonic::Code::NotFound,
        "REPO_NOT_FOUND",
        "no repository by this id or name".into(),
    )
}

fn internal(err: sqlx::Error) -> Status {
    Status::internal(format!("repositories: {err}"))
}

type Row = (i64, String, String, String, String, String, bool, bool);

const SELECT: &str = "SELECT repo.id, repo.public_id, owner.name, repo.name, repo.description,
        repo.default_branch, repo.ci_enabled, repo.archived_at IS NOT NULL
    FROM repo JOIN user owner ON owner.id = repo.owner_id";

/// The row with its redirects, permissions and mirror configuration.
async fn message(connection: &mut SqliteConnection, row: Row) -> Result<Repo, sqlx::Error> {
    let (id, public_id, owner, name, description, default_branch, ci_enabled, archived) = row;
    let previous_names = sqlx::query_scalar(
        "SELECT owner_name || '/' || name FROM repo_redirect WHERE repo_id = ? ORDER BY 1",
    )
    .bind(id)
    .fetch_all(&mut *connection)
    .await?;
    let permissions = sqlx::query_as::<_, (String, String)>(
        "SELECT user.name, role FROM repo_permission JOIN user ON user.id = user_id
         WHERE repo_id = ? ORDER BY user.name",
    )
    .bind(id)
    .fetch_all(&mut *connection)
    .await?
    .into_iter()
    .map(|(user, role)| Permission {
        user,
        role: match role.as_str() {
            "read" => Role::Read,
            "write" => Role::Write,
            "admin" => Role::Admin,
            _ => Role::Unspecified,
        }
        .into(),
    })
    .collect();
    let mirrors = sqlx::query_as::<_, (String, String, bool)>(
        "SELECT remote_url, credential, enabled FROM mirror WHERE repo_id = ? ORDER BY remote_url",
    )
    .bind(id)
    .fetch_all(&mut *connection)
    .await?
    .into_iter()
    .map(|(remote_url, credential, enabled)| Mirror {
        // Archived rows may predate config validation; never publish an unsafe remote.
        remote_url: if kajiba_core::config::validate_mirror_url(&remote_url, "remote_url").is_ok() {
            remote_url
        } else {
            String::new()
        },
        credential,
        enabled,
    })
    .collect();
    Ok(Repo {
        public_id,
        owner,
        name,
        description,
        // `public` is the only visibility until private repositories exist (0013).
        visibility: Visibility::Public.into(),
        default_branch,
        ci_enabled,
        previous_names,
        permissions,
        mirrors,
        archived,
    })
}

#[tonic::async_trait]
impl RepoService for Repos {
    async fn get_repo(
        &self,
        request: Request<GetRepoRequest>,
    ) -> Result<Response<GetRepoResponse>, Status> {
        let readers = self.readers()?;
        let id = match request.into_inner().repo {
            Some(get_repo_request::Repo::PublicId(public_id)) => {
                sqlx::query_scalar("SELECT id FROM repo WHERE public_id = ?")
                    .bind(public_id)
                    .fetch_optional(readers)
                    .await
                    .map_err(internal)?
            }
            Some(get_repo_request::Repo::Name(name)) => {
                let (owner, name) = name.split_once('/').ok_or_else(not_found)?;
                crate::db::repos::find(readers, owner, name)
                    .await
                    .map_err(internal)?
                    .map(|found| found.id)
            }
            None => {
                return Err(cursor::invalid_field(
                    "repo",
                    "public_id or name is required",
                ));
            }
        }
        .ok_or_else(not_found)?;
        let mut connection = readers.begin().await.map_err(internal)?;
        let row: Row = sqlx::query_as(&format!("{SELECT} WHERE repo.id = ?"))
            .bind(id)
            .fetch_one(&mut *connection)
            .await
            .map_err(internal)?;
        let repo = message(&mut connection, row).await.map_err(internal)?;
        Ok(Response::new(GetRepoResponse { repo: Some(repo) }))
    }

    async fn list_repos(
        &self,
        request: Request<ListReposRequest>,
    ) -> Result<Response<ListReposResponse>, Status> {
        const METHOD: &str = "v1.ListRepos";
        let request = request.into_inner();
        let size = cursor::page_size(request.page_size)?;
        let (key, id) = cursor::decode(METHOD, &request.page_token)?.unwrap_or_default();
        // One read transaction: the page and its details come from one snapshot.
        let mut connection = self.readers()?.begin().await.map_err(internal)?;
        let mut rows: Vec<Row> = sqlx::query_as(&format!(
            "{SELECT} WHERE (owner.name || '/' || repo.name, repo.id) > (?1, ?2)
             ORDER BY owner.name || '/' || repo.name, repo.id LIMIT ?3"
        ))
        .bind(key)
        .bind(id)
        .bind(size + 1)
        .fetch_all(&mut *connection)
        .await
        .map_err(internal)?;
        let more = rows.len() as i64 > size;
        rows.truncate(size as usize);
        let next_page_token = match rows.last() {
            Some((id, _, owner, name, ..)) if more => {
                cursor::encode(METHOD, &format!("{owner}/{name}"), *id)
            }
            _ => String::new(),
        };
        let mut repos = Vec::with_capacity(rows.len());
        // shortcut: three queries per repository (at most 200 per page), batch them if pages get slow.
        for row in rows {
            repos.push(message(&mut connection, row).await.map_err(internal)?);
        }
        Ok(Response::new(ListReposResponse {
            repos,
            next_page_token,
        }))
    }
}

#[cfg(test)]
pub(super) mod tests;
