//! `admin import` (ADR 0026): content of a path, bundle or URL into a declared,
//! empty repository, through the admission gate of ADR 0003.

use crate::db::{
    Writer,
    events::EventBus,
    pushes::{Gate, RefUpdate, Refused, ZERO, checkpoint},
};
use kajiba_git::{GitStore, Gix};
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
    /// The source cannot be read or fetched; the reason is redacted by `reason`.
    Source(String),
    Internal(String),
}

/// Returns the number of imported refs. Refusals write nothing.
#[expect(clippy::too_many_arguments)]
pub async fn import(
    writer: &Writer,
    gate: &Arc<Gate>,
    bus: &Arc<EventBus>,
    state_dir: &Path,
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

    let (owned, secret) = (source.to_owned(), credentials.to_owned());
    let refs = tokio::task::spawn_blocking(move || Gix.remote_refs(&owned, &secret))
        .await
        .map_err(|err| Error::Internal(err.to_string()))?
        .map_err(|err| Error::Source(reason(&err.to_string(), source)))?;
    let head = format!("refs/heads/{default_branch}");
    if !refs.iter().any(|r| r.name == head) {
        return Err(Error::NoDefaultBranch(head));
    }

    {
        // Emptiness and admission under one writer hold: two imports cannot both pass.
        let mut connection = writer.lock().await;
        let existing = Gix
            .refs(&path)
            .map_err(|err| Error::Internal(err.to_string()))?;
        if !existing.is_empty() {
            return Err(Error::NotEmpty(format!("{repo} already has refs")));
        }
        let updates: Vec<RefUpdate> = refs
            .iter()
            .map(|r| RefUpdate {
                name: r.name.clone(),
                old: ZERO.into(),
                new: r.target.clone(),
            })
            .collect();
        gate.admit(&mut connection, repo_id, None, correlation_id, &updates)
            .await
            .map_err(|err| match err {
                Refused::Database(err) => Error::Internal(err.to_string()),
                err => Error::NotEmpty(err.to_string()),
            })?;
    }
    checkpoint("admitted");

    let (target, owned, secret, wanted) = (
        path.clone(),
        source.to_owned(),
        credentials.to_owned(),
        refs.clone(),
    );
    let fetched = supervise(
        writer.clone(),
        gate.clone(),
        bus.clone(),
        state_dir.to_owned(),
        repo_id,
        move || Gix.fetch(&target, &owned, &secret, &wanted),
    )
    .await
    .map_err(|err| Error::Internal(err.to_string()))?
    .map_err(Error::Internal)?;
    fetched.map_err(|err| Error::Source(reason(&err.to_string(), source)))?;
    tracing::info!(event = "repo-imported", repo, refs = refs.len());
    Ok(refs.len())
}

/// An absolute path, or a URL that cannot carry a credential: no userinfo (an ssh
/// user name excepted), query or fragment (ADR 0041, 0044). The CLI's original
/// arguments stay readable by every local user for the whole import.
pub fn check_source(source: &str) -> Result<(), &'static str> {
    let Some((scheme, rest)) = source.split_once("://") else {
        // The server would resolve a relative path from its own working directory.
        return if Path::new(source).is_absolute() {
            Ok(())
        } else {
            Err("not an absolute path or a URL")
        };
    };
    if rest.contains(['?', '#']) {
        return Err("a query or fragment in the source is refused; use --credentials-file");
    }
    let authority = rest.split('/').next().unwrap_or(rest);
    match authority.rsplit_once('@') {
        Some((user, _)) if scheme != "ssh" || user.contains(':') => {
            Err("credentials in the source are refused; use --credentials-file")
        }
        _ => Ok(()),
    }
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
    mutate: impl FnOnce() -> kajiba_git::Result<()> + Send + 'static,
) -> tokio::task::JoinHandle<Result<kajiba_git::Result<()>, String>> {
    tokio::spawn(async move {
        let mutated = tokio::task::spawn_blocking(mutate)
            .await
            .map_err(|err| err.to_string());
        checkpoint("fetched");
        gate.exited(repo_id);
        gate.reconcile_repo(&mut *writer.lock().await, &bus, &state_dir, &Gix, repo_id)
            .await?;
        mutated
    })
}

/// Git's stderr may echo a source URL with its credentials: keep only scheme, host
/// and path of every URL (no userinfo, query or fragment), drop the source's own
/// userinfo wherever it appears, on one line of at most 200 characters.
pub fn reason(stderr: &str, source: &str) -> String {
    let mut text = stderr.split_whitespace().collect::<Vec<_>>().join(" ");
    if let Some((userinfo, _)) = source
        .split_once("://")
        .and_then(|(_, rest)| rest.split_once('@'))
        .filter(|(userinfo, _)| !userinfo.is_empty())
    {
        text = text.replace(userinfo, "");
    }
    let mut out = String::new();
    let mut rest = text.as_str();
    while let Some(start) = rest.find("://") {
        let (head, tail) = rest.split_at(start + 3);
        out.push_str(head);
        let end = tail
            .find(|c: char| c.is_whitespace() || matches!(c, '\'' | '"'))
            .unwrap_or(tail.len());
        let (url, after) = tail.split_at(end);
        let url = url.split(['?', '#']).next().unwrap_or(url);
        let (authority, path) = url.split_at(url.find('/').unwrap_or(url.len()));
        out.push_str(
            authority
                .rsplit_once('@')
                .map_or(authority, |(_, host)| host),
        );
        out.push_str(path);
        rest = after;
    }
    out.push_str(rest);
    out.chars().take(200).collect()
}
