//! `admin import` (ADR 0026): content of a path, bundle or URL into a declared,
//! empty repository, through the admission gate of ADR 0003.

use crate::db::{
    Writer,
    events::EventBus,
    pushes::{Gate, RefUpdate, Refused, ZERO, checkpoint},
};
use crate::transport::git_side::Repositories;
pub use kajiba_gitstore::import::{check_source, reason};
use kajiba_gitstore::{GitStore, Gix};
use kajiba_proto::git::v1::{FetchRequest, ListRemoteRefsRequest, Session, SessionKind};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Debug)]
pub enum Error {
    /// Not declared, or archived.
    NotFound,
    /// The target has a ref, or a pending ref update.
    NotEmpty(String),
    /// The source lacks the declared default branch.
    NoDefaultBranch(String),
    /// The source cannot be read or fetched; one of `reason`'s classes, or the rule a
    /// refused source broke (`check_source`).
    Source(String),
    Internal(String),
    Unavailable(String),
}

/// Returns the number of imported refs. Refusals write nothing.
#[expect(clippy::too_many_arguments)]
pub async fn import(
    writer: &Writer,
    gate: &Arc<Gate>,
    bus: &Arc<EventBus>,
    state_dir: &Path,
    daemon: &mut Repositories,
    repo: &str,
    source: &str,
    credentials: &str,
    correlation_id: &str,
) -> Result<usize, Error> {
    check_source(source).map_err(|err| Error::Source(err.into()))?;
    let valid = |name: &str| kajiba_core::names::validate_name(name).map_err(|_| Error::NotFound);
    let (owner, name) = repo.split_once('/').ok_or(Error::NotFound)?;
    let (owner, name) = (valid(owner)?, valid(name)?);
    let (repo_id, public_id, default_branch) = sqlx::query_as::<_, (i64, String, String)>(
        "SELECT repo.id, repo.public_id, repo.default_branch FROM repo
             JOIN user owner ON owner.id = repo.owner_id
             WHERE owner.name = ? AND repo.name = ? AND repo.archived_at IS NULL",
    )
    .bind(&owner)
    .bind(&name)
    .fetch_optional(&mut *writer.lock().await)
    .await
    .map_err(|err| Error::Internal(err.to_string()))?
    .ok_or(Error::NotFound)?;
    let path = state_dir.join(format!("repos/{public_id}.git"));

    if !Gix::shared(&state_dir.join("repos"))
        .refs(&path)
        .map_err(|err| Error::Internal(err.to_string()))?
        .is_empty()
    {
        return Err(Error::NotEmpty(format!("{repo} already has refs")));
    }
    let mut client = daemon
        .client()
        .await
        .map_err(|err| Error::Unavailable(err.to_string()))?
        .clone();
    let refs = client
        .list_remote_refs(ListRemoteRefsRequest {
            source: source.into(),
            credentials: credentials.into(),
        })
        .await
        .map_err(remote_error)?
        .into_inner()
        .refs;
    let head = format!("refs/heads/{default_branch}");
    if !refs.iter().any(|r| r.ref_name == head) {
        return Err(Error::NoDefaultBranch(head));
    }

    let session_id = kajiba_core::new_id().map_err(|err| Error::Internal(err.to_string()))?;
    let generation = daemon.generation.clone();
    let (source, credentials, wanted) = (source.to_owned(), credentials.to_owned(), refs.clone());
    let updates: Vec<RefUpdate> = refs
        .iter()
        .map(|r| RefUpdate {
            name: r.ref_name.clone(),
            old: ZERO.into(),
            new: r.object_id.clone(),
        })
        .collect();
    let (writer, gate, bus, state_dir, repo_name, correlation_id) = (
        writer.clone(),
        gate.clone(),
        bus.clone(),
        state_dir.to_owned(),
        repo.to_owned(),
        correlation_id.to_owned(),
    );
    // Own admission and completion independently of the admin RPC's lifetime.
    tokio::spawn(async move {
        {
            let mut connection = writer.lock().await;
            if !Gix::shared(&state_dir.join("repos"))
                .refs(&path)
                .map_err(|err| Error::Internal(err.to_string()))?
                .is_empty()
            {
                return Err(Error::NotEmpty(format!("{repo_name} already has refs")));
            }
            gate.admit(&mut connection, repo_id, None, &correlation_id, &updates)
                .await
                .map_err(|err| match err {
                    Refused::Database(err) => Error::Internal(err.to_string()),
                    err => Error::NotEmpty(err.to_string()),
                })?;
        }
        checkpoint("admitted");
        let request = FetchRequest {
            session: Some(Session {
                session_id,
                kind: SessionKind::Import.into(),
                repo_public_id: public_id.clone(),
                user_public_id: String::new(),
                correlation_id,
                generation,
            }),
            repo_public_id: public_id,
            source,
            credentials,
            refs: wanted,
        };
        supervise(writer, gate, bus, state_dir, repo_id, async move {
            client.fetch(request).await.map(|_| ())
        })
        .await
        .map_err(|err| Error::Internal(err.to_string()))?
        .map_err(Error::Internal)?
        .map_err(remote_error)
    })
    .await
    .map_err(|err| Error::Internal(err.to_string()))??;
    tracing::info!(event = "repo-imported", repo, refs = refs.len());
    Ok(refs.len())
}

/// Runs the admitted mutation in a task of its own, then releases the gate and
/// reconciles the repository: a dropped RPC future (client gone) never pins the
/// repository until restart (ADR 0003). The outer error is internal, the inner one
/// is the mutation's.
pub(crate) fn supervise(
    writer: Writer,
    gate: Arc<Gate>,
    bus: Arc<EventBus>,
    state_dir: PathBuf,
    repo_id: i64,
    mutate: impl std::future::Future<Output = Result<(), tonic::Status>> + Send + 'static,
) -> tokio::task::JoinHandle<Result<Result<(), tonic::Status>, String>> {
    tokio::spawn(async move {
        let mutated = mutate.await;
        // A transport failure is not evidence of child exit (0053).
        if mutated.as_ref().is_err_and(|err| {
            !matches!(
                err.code(),
                tonic::Code::InvalidArgument
                    | tonic::Code::FailedPrecondition
                    | tonic::Code::NotFound
            )
        }) {
            return Ok(mutated);
        }
        checkpoint("fetched");
        gate.exited(repo_id);
        gate.reconcile_repo(
            &mut *writer.lock().await,
            &bus,
            &state_dir,
            &Gix::shared(&state_dir.join("repos")),
            repo_id,
        )
        .await?;
        Ok(mutated)
    })
}

fn remote_error(err: tonic::Status) -> Error {
    if matches!(
        err.code(),
        tonic::Code::Unavailable
            | tonic::Code::DeadlineExceeded
            | tonic::Code::Cancelled
            | tonic::Code::Unknown
    ) {
        Error::Unavailable("import completion unavailable".into())
    } else {
        Error::Source(reason(err.message()).into())
    }
}
