//! `admin import` (ADR 0026): content of a path, bundle or URL into a declared,
//! empty repository, through the admission gate of ADR 0003.

use crate::db::{
    events::EventBus,
    pushes::{Gate, RefUpdate, Refused, ZERO},
};
use kajiba_git::{GitStore, Gix};
use std::path::Path;

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
pub async fn import(
    writer: &crate::db::Writer,
    gate: &Gate,
    bus: &EventBus,
    state_dir: &Path,
    repo: &str,
    source: &str,
    correlation_id: &str,
) -> Result<usize, Error> {
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

    let owned = source.to_owned();
    let refs = tokio::task::spawn_blocking(move || Gix.remote_refs(&owned))
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

    let (target, owned, wanted) = (path.clone(), source.to_owned(), refs.clone());
    let fetched = tokio::task::spawn_blocking(move || Gix.fetch(&target, &owned, &wanted)).await;
    gate.exited(repo_id);
    gate.reconcile(&mut *writer.lock().await, bus, state_dir, &Gix)
        .await
        .map_err(Error::Internal)?;
    fetched
        .map_err(|err| Error::Internal(err.to_string()))?
        .map_err(|err| Error::Source(reason(&err.to_string(), source)))?;
    tracing::info!(event = "repo-imported", repo, refs = refs.len());
    Ok(refs.len())
}

/// Git's stderr may echo a source URL with its credentials: drop the userinfo of
/// every URL and of the source as given, on one line of at most 200 characters.
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
            .find(|c: char| c.is_whitespace() || matches!(c, '/' | '?' | '#' | '\'' | '"'))
            .unwrap_or(tail.len());
        let authority = &tail[..end];
        out.push_str(
            authority
                .rsplit_once('@')
                .map_or(authority, |(_, host)| host),
        );
        rest = &tail[end..];
    }
    out.push_str(rest);
    out.chars().take(200).collect()
}
