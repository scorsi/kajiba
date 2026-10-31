//! Read-only repository access (ADR 0020, 0021, 0026): `RepoService`, metadata and
//! code browsing.
// These feed `?` in tonic handlers, which return `Status` by value anyway.
#![allow(clippy::result_large_err)]

use super::{Health, cursor, local::refused};
use kajiba_gitstore::{GitStore, Gix};
use kajiba_proto::v1::{
    ChangeKind, CiMode, Commit, EntryKind, FileChange, GetBlobRequest, GetBlobResponse,
    GetCommitDiffRequest, GetCommitDiffResponse, GetRepoRequest, GetRepoResponse, GetTreeRequest,
    GetTreeResponse, ListCommitsRequest, ListCommitsResponse, ListReposRequest, ListReposResponse,
    Mirror, Permission, Repo, Role, TreeEntry, Visibility, get_repo_request,
    repo_service_server::RepoService,
};
use sqlx::{SqliteConnection, SqlitePool};
use std::path::{Path, PathBuf};
use tonic::{Request, Response, Status};

/// Repository routes read through the read pool, available once startup completed.
#[derive(Clone)]
pub struct Repos(pub Health);

impl Repos {
    pub fn read_permit(
        &self,
        permits: u32,
    ) -> Result<tokio::sync::OwnedSemaphorePermit, axum::response::Response> {
        use axum::{
            http::{StatusCode, header},
            response::IntoResponse,
        };
        self.0
            .public_reads
            .clone()
            .try_acquire_many_owned(permits)
            .map_err(|_| {
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    [(header::RETRY_AFTER, "1")],
                    "public reads busy\n",
                )
                    .into_response()
            })
    }

    /// The same budget for browsing RPCs (ADR 0043): UNAVAILABLE when exhausted.
    fn browse_permit(&self) -> Result<tokio::sync::OwnedSemaphorePermit, Status> {
        self.0
            .public_reads
            .clone()
            .try_acquire_owned()
            .map_err(|_| {
                refused(
                    tonic::Code::Unavailable,
                    "READS_BUSY",
                    "public reads busy, retry shortly".into(),
                )
            })
    }

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
                .git_state_dir
                .join(format!("repos/{}.git", found.public_id))),
            Ok(None) => Err(not_found_http()),
            Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR.into_response()),
        }
    }
}

impl Repos {
    /// The git directory of a repository the caller can read, active or archived (0026),
    /// by public id or by `<owner>/<name>`, current or previous.
    async fn browse_dir(&self, user: i64, repo: &str) -> Result<PathBuf, Status> {
        let (_, public_id) = find_readable(self.readers()?, user, repo).await?;
        Ok(self.0.git_state_dir.join(format!("repos/{public_id}.git")))
    }
}

/// The row id and public id of a repository the caller can read, active or archived,
/// by public id or by `<owner>/<name>`, current or previous; REPO_NOT_FOUND otherwise.
pub(super) async fn find_readable(
    readers: &SqlitePool,
    user: i64,
    repo: &str,
) -> Result<(i64, String), Status> {
    let (id, public_id) = match repo.split_once('/') {
        Some((owner, name)) => crate::db::repos::find(readers, owner, name)
            .await
            .map_err(internal)?
            .map(|found| (found.id, found.public_id)),
        None => sqlx::query_as("SELECT id, public_id FROM repo WHERE public_id = ?")
            .bind(repo)
            .fetch_optional(readers)
            .await
            .map_err(internal)?,
    }
    .ok_or_else(not_found)?;
    readable(readers, user, id).await?;
    Ok((id, public_id))
}

/// The authenticated caller the auth layer put in the request (0014).
pub(super) fn caller<T>(request: &Request<T>) -> Result<i64, Status> {
    request
        .extensions()
        .get::<super::auth::Principal>()
        .map(|principal| principal.user_id)
        .ok_or_else(|| Status::internal("repositories: no authenticated caller"))
}

/// ADR 0014's role check: without a read role the repository does not exist (0020).
pub(super) async fn readable(
    executor: impl sqlx::SqliteExecutor<'_>,
    user: i64,
    repo: i64,
) -> Result<(), Status> {
    crate::db::permissions::role(executor, user, repo)
        .await
        .map_err(internal)?
        .map(drop)
        .ok_or_else(not_found)
}

/// GetBlob refuses larger files (ADR 0047): a unary response is held in memory whole
/// for as long as its permit; `/raw/` streams the larger ones.
pub const MAX_BLOB_BYTES: usize = 1 << 20;
/// Response bounds of ADR 0043 and 0047: larger trees and diffs are refused, deeper
/// history is reached from an older revision, a commit page ends early past its bytes.
pub const MAX_TREE_ENTRIES: usize = 10_000;
pub const MAX_DIFF_CHANGES: usize = 10_000;
pub const MAX_DIFF_PATH_BYTES: usize = 4 << 20;
pub const MAX_COMMIT_SKIP: usize = 10_000;
pub const MAX_COMMIT_PAGE_BYTES: usize = 1 << 20;

/// A GitStore read, off the async runtime, holding a public-read permit; a cancelled
/// handler signals cancellation and keeps the permit with the running work until
/// it stops. The response carries the permit
/// in its extensions for `stream::guarded`, which keeps it through the delivery of
/// the encoded body (ADR 0050); a refusal releases it at once.
async fn blocking<T: Send + 'static>(
    permit: tokio::sync::OwnedSemaphorePermit,
    read: impl FnOnce(&std::sync::atomic::AtomicBool) -> Result<Response<T>, Status> + Send + 'static,
) -> Result<Response<T>, Status> {
    use std::sync::{Arc, atomic::AtomicBool, atomic::Ordering};
    struct CancelOnDrop(Arc<AtomicBool>);
    impl Drop for CancelOnDrop {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Relaxed);
        }
    }
    let cancelled = Arc::new(AtomicBool::new(false));
    let _cancel = CancelOnDrop(cancelled.clone());
    tokio::task::spawn_blocking(move || {
        let mut response = read(&cancelled)?;
        response
            .extensions_mut()
            .insert(super::stream::ReadPermit::new(permit));
        Ok(response)
    })
    .await
    .map_err(|err| Status::internal(format!("git: {err}")))?
}

/// `NotFound` from git becomes `<reason>`, `TooLarge` the RESOURCE_EXHAUSTED of what
/// was exceeded (ADR 0043, 0047), anything else INTERNAL.
pub(super) fn git_error(reason: &'static str) -> impl Fn(kajiba_gitstore::Error) -> Status {
    use kajiba_gitstore::{Limit, MAX_COMMIT_OBJECT_BYTES, MAX_TREE_OBJECT_BYTES};
    move |err| match err {
        kajiba_gitstore::Error::NotFound => refused(
            tonic::Code::NotFound,
            reason,
            match reason {
                "REVISION_NOT_FOUND" => "no branch, tag or commit by this name",
                _ => "no such path at this revision",
            }
            .into(),
        ),
        kajiba_gitstore::Error::TooLarge(limit) => {
            let (reason, message) = match limit {
                Limit::Revision => (
                    "REVISION_TOO_LARGE",
                    format!(
                        "revision exceeds {} bytes or {} components",
                        kajiba_gitstore::MAX_REVISION_BYTES,
                        kajiba_gitstore::MAX_REVISION_COMPONENTS
                    ),
                ),
                Limit::Blob => (
                    "BLOB_TOO_LARGE",
                    format!("the file exceeds {MAX_BLOB_BYTES} bytes; download it from /raw/"),
                ),
                Limit::Tree => (
                    "TREE_TOO_LARGE",
                    format!(
                        "a tree exceeds {MAX_TREE_ENTRIES} entries or {MAX_TREE_OBJECT_BYTES} bytes"
                    ),
                ),
                Limit::Commit => (
                    "COMMIT_TOO_LARGE",
                    format!("a commit object exceeds {MAX_COMMIT_OBJECT_BYTES} bytes"),
                ),
                Limit::Diff => (
                    "DIFF_TOO_LARGE",
                    format!(
                        "the diff exceeds {MAX_DIFF_CHANGES} changes or {MAX_DIFF_PATH_BYTES} bytes of paths"
                    ),
                ),
                Limit::Traversal => (
                    "TRAVERSAL_TOO_LARGE",
                    "repository traversal exceeded its work budget".into(),
                ),
            };
            refused(tonic::Code::ResourceExhausted, reason, message)
        }
        kajiba_gitstore::Error::Git(err) => Status::internal(format!("git: {err}")),
        kajiba_gitstore::Error::VerificationLimit(message) => Status::internal(message),
    }
}

/// A branch, a tag or a full commit id, resolved to a commit id.
fn revision(store: &Gix<'_>, dir: &Path, revision: &str) -> Result<String, Status> {
    match store.resolve(dir, revision) {
        Ok((commit, "")) => Ok(commit),
        // A path left over: `revision` is no ref.
        Ok(_) => Err(git_error("REVISION_NOT_FOUND")(
            kajiba_gitstore::Error::NotFound,
        )),
        Err(err) => Err(git_error("REVISION_NOT_FOUND")(err)),
    }
}

fn path(path: &str) -> Result<(), Status> {
    if path.is_empty() {
        return Ok(());
    }
    kajiba_core::names::validate_path(path)
        .map_err(|err| cursor::invalid_field("path", &err.to_string()))
}

fn commit(commit: kajiba_gitstore::Commit) -> Commit {
    Commit {
        id: commit.id,
        parent_ids: commit.parents,
        author_name: commit.author_name,
        author_email: commit.author_email,
        commit_time: Some(kajiba_proto::google::protobuf::Timestamp {
            seconds: commit.time,
            nanos: 0,
        }),
        message: commit.message,
    }
}

pub(super) fn not_found() -> Status {
    refused(
        tonic::Code::NotFound,
        "REPO_NOT_FOUND",
        "no repository by this id or name".into(),
    )
}

pub(super) fn internal(err: sqlx::Error) -> Status {
    Status::internal(format!("repositories: {err}"))
}

type Row = (i64, String, String, String, String, String, String, bool);

const SELECT: &str = "SELECT repo.id, repo.public_id, owner.name, repo.name, repo.description,
        repo.default_branch, repo.ci_mode, repo.archived_at IS NOT NULL
    FROM repo JOIN user owner ON owner.id = repo.owner_id";

/// The row with its redirects, permissions and mirror configuration.
async fn message(connection: &mut SqliteConnection, row: Row) -> Result<Repo, sqlx::Error> {
    let (id, public_id, owner, name, description, default_branch, ci_mode, archived) = row;
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
        ci_mode: match ci_mode.as_str() {
            "off" => CiMode::Off,
            "manual" => CiMode::Manual,
            "push" => CiMode::Push,
            _ => CiMode::Unspecified,
        }
        .into(),
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
        let user = caller(&request)?;
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
        readable(readers, user, id).await?;
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
        let user = caller(&request)?;
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
        // The cursor follows the rows read, not the rows returned: a repository the caller
        // cannot read only shortens its page, and pages never skip or repeat a row.
        // shortcut: four queries per repository (at most 200 per page), batch them if pages get slow.
        for row in rows {
            if crate::db::permissions::role(&mut *connection, user, row.0)
                .await
                .map_err(internal)?
                .is_some()
            {
                repos.push(message(&mut connection, row).await.map_err(internal)?);
            }
        }
        Ok(Response::new(ListReposResponse {
            repos,
            next_page_token,
        }))
    }

    async fn get_tree(
        &self,
        request: Request<GetTreeRequest>,
    ) -> Result<Response<GetTreeResponse>, Status> {
        let user = caller(&request)?;
        let request = request.into_inner();
        kajiba_gitstore::check_revision(&request.revision)
            .map_err(git_error("REVISION_NOT_FOUND"))?;
        path(&request.path)?;
        let dir = self.browse_dir(user, &request.repo).await?;
        let permit = self.browse_permit()?;
        let root = self.0.git_state_dir.join("repos");
        blocking(permit, move |cancelled| {
            let store = Gix::shared(&root).cancellable(cancelled);
            let commit_id = revision(&store, &dir, &request.revision)?;
            let entries = store
                .tree(&dir, &commit_id, &request.path, MAX_TREE_ENTRIES)
                .map_err(git_error("PATH_NOT_FOUND"))?
                .into_iter()
                .map(|entry| TreeEntry {
                    name: entry.name,
                    kind: match entry.kind {
                        kajiba_gitstore::EntryKind::Tree => EntryKind::Tree,
                        kajiba_gitstore::EntryKind::Blob => EntryKind::Blob,
                        kajiba_gitstore::EntryKind::Executable => EntryKind::Executable,
                        kajiba_gitstore::EntryKind::Link => EntryKind::Link,
                        kajiba_gitstore::EntryKind::Submodule => EntryKind::Submodule,
                    }
                    .into(),
                    id: entry.id,
                })
                .collect();
            Ok(Response::new(GetTreeResponse { commit_id, entries }))
        })
        .await
    }

    async fn get_blob(
        &self,
        request: Request<GetBlobRequest>,
    ) -> Result<Response<GetBlobResponse>, Status> {
        let user = caller(&request)?;
        let request = request.into_inner();
        kajiba_gitstore::check_revision(&request.revision)
            .map_err(git_error("REVISION_NOT_FOUND"))?;
        path(&request.path)?;
        let dir = self.browse_dir(user, &request.repo).await?;
        let permit = self.browse_permit()?;
        let root = self.0.git_state_dir.join("repos");
        blocking(permit, move |cancelled| {
            let store = Gix::shared(&root).cancellable(cancelled);
            let commit_id = revision(&store, &dir, &request.revision)?;
            let content = store
                .blob(&dir, &commit_id, &request.path, MAX_BLOB_BYTES as u64)
                .map_err(git_error("PATH_NOT_FOUND"))?;
            Ok(Response::new(GetBlobResponse { commit_id, content }))
        })
        .await
    }

    async fn list_commits(
        &self,
        request: Request<ListCommitsRequest>,
    ) -> Result<Response<ListCommitsResponse>, Status> {
        const METHOD: &str = "v1.ListCommits";
        let user = caller(&request)?;
        let request = request.into_inner();
        kajiba_gitstore::check_revision(&request.revision)
            .map_err(git_error("REVISION_NOT_FOUND"))?;
        let size = cursor::page_size(request.page_size)? as usize;
        // The cursor holds the first page's tip and the commits already returned, so
        // pages stay stable under pushes and follow every parent of a merge.
        let cursor = match cursor::decode(METHOD, &request.page_token)? {
            Some((tip, skip)) => Some((
                tip,
                usize::try_from(skip)
                    .map_err(|_| cursor::invalid_field("page_token", "malformed cursor"))?,
            )),
            None => None,
        };
        // Issued cursors never pass the bound: a deeper one is forged (ADR 0043).
        if cursor
            .as_ref()
            .is_some_and(|(_, skip)| *skip > MAX_COMMIT_SKIP)
        {
            return Err(refused(
                tonic::Code::ResourceExhausted,
                "HISTORY_TOO_DEEP",
                format!(
                    "at most {MAX_COMMIT_SKIP} commits past the revision; list from an older one"
                ),
            ));
        }
        let dir = self.browse_dir(user, &request.repo).await?;
        let permit = self.browse_permit()?;
        let root = self.0.git_state_dir.join("repos");
        blocking(permit, move |cancelled| {
            let store = Gix::shared(&root).cancellable(cancelled);
            let (tip, skip) = match cursor {
                Some(cursor) => cursor,
                None => (revision(&store, &dir, &request.revision)?, 0),
            };
            // shortcut: each page walks the history from the tip again, quadratic over deep paging; keep the walk state if that gets slow.
            let mut page = store
                .log(&dir, &tip, skip, size + 1, MAX_COMMIT_PAGE_BYTES)
                .map_err(git_error("REVISION_NOT_FOUND"))?;
            // A page cut short by its bytes may have more behind it; the next page is
            // then empty at worst, as past the end of history.
            let cut = page.iter().map(|c| c.page_bytes()).sum::<usize>() > MAX_COMMIT_PAGE_BYTES;
            let more = page.len() > size || cut;
            page.truncate(size);
            let next_page_token = if more && skip + page.len() <= MAX_COMMIT_SKIP {
                cursor::encode(METHOD, &tip, (skip + page.len()) as i64)
            } else {
                String::new()
            };
            Ok(Response::new(ListCommitsResponse {
                commits: page.into_iter().map(commit).collect(),
                next_page_token,
            }))
        })
        .await
    }

    async fn get_commit_diff(
        &self,
        request: Request<GetCommitDiffRequest>,
    ) -> Result<Response<GetCommitDiffResponse>, Status> {
        let user = caller(&request)?;
        let request = request.into_inner();
        kajiba_gitstore::check_revision(&request.revision)
            .map_err(git_error("REVISION_NOT_FOUND"))?;
        let dir = self.browse_dir(user, &request.repo).await?;
        let permit = self.browse_permit()?;
        let root = self.0.git_state_dir.join("repos");
        blocking(permit, move |cancelled| {
            let store = Gix::shared(&root).cancellable(cancelled);
            let id = revision(&store, &dir, &request.revision)?;
            let found = store
                .log(&dir, &id, 0, 1, MAX_COMMIT_PAGE_BYTES)
                .map_err(git_error("REVISION_NOT_FOUND"))?
                .pop()
                .ok_or_else(|| git_error("REVISION_NOT_FOUND")(kajiba_gitstore::Error::NotFound))?;
            let changes = store
                .diff(
                    &dir,
                    found.parents.first().map(String::as_str),
                    &id,
                    MAX_DIFF_CHANGES,
                    MAX_DIFF_PATH_BYTES,
                )
                .map_err(git_error("REVISION_NOT_FOUND"))?
                .into_iter()
                .map(|change| FileChange {
                    path: change.path,
                    kind: match change.kind {
                        kajiba_gitstore::ChangeKind::Added => ChangeKind::Added,
                        kajiba_gitstore::ChangeKind::Deleted => ChangeKind::Deleted,
                        kajiba_gitstore::ChangeKind::Modified => ChangeKind::Modified,
                    }
                    .into(),
                    old_id: change.old_id.unwrap_or_default(),
                    new_id: change.new_id.unwrap_or_default(),
                })
                .collect();
            Ok(Response::new(GetCommitDiffResponse {
                commit: Some(commit(found)),
                changes,
            }))
        })
        .await
    }
}

#[cfg(test)]
pub(super) mod tests;
