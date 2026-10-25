//! ADR 0003 push durability: a `pending_push` row is committed before any ref
//! mutation, and turned into one `push` event per applied ref once the mutating
//! process has exited.

use super::events::{Clock, EventBus, NewEvent, append_event};
use super::users::new_public_id;
use kajiba_gitstore::GitStore;
use sqlx::{Connection, SqliteConnection};
use std::{
    collections::{BTreeMap, HashSet},
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

pub const ZERO: &str = "0000000000000000000000000000000000000000";

/// One ref update; a zero id means the ref is absent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefUpdate {
    pub name: String,
    pub old: String,
    pub new: String,
}

#[derive(Debug)]
pub enum Refused {
    /// The admitted process has not exited or its row is not reconciled yet.
    InProgress,
    /// Reconciliation met an unexpected ref value and kept the row as evidence.
    Retained,
    Database(sqlx::Error),
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InProgress => {
                f.write_str("another ref update to this repository is in progress; retry")
            }
            Self::Retained => f.write_str(
                "a previous ref update left an unexpected ref value; ref updates are blocked until it is recovered",
            ),
            Self::Database(err) => write!(f, "{err}"),
        }
    }
}

/// Live daemon sessions supplement durable pending-row session identities (0053).
#[derive(Default)]
pub struct Gate {
    wake: tokio::sync::Notify,
    running: std::sync::Mutex<HashSet<i64>>,
    sessions: std::sync::Mutex<BTreeMap<String, (i64, kajiba_proto::git::v1::Session)>>,
    generation: std::sync::Mutex<Option<String>>,
}

impl Gate {
    pub fn wake_recovery(&self) {
        self.wake.notify_one();
    }

    pub fn running(&self, repo: i64) -> bool {
        self.running.lock().unwrap().contains(&repo)
    }

    pub fn generation(&self) -> Option<String> {
        self.generation.lock().unwrap().clone()
    }

    pub fn session(&self, id: &str) -> Option<(i64, kajiba_proto::git::v1::Session)> {
        self.sessions.lock().unwrap().get(id).cloned()
    }

    /// Called under the database writer lock, also held by admission and exit.
    pub async fn rebuild(
        &self,
        writer: &mut SqliteConnection,
        generation: &str,
        sessions: Vec<kajiba_proto::git::v1::Session>,
    ) -> Result<(), String> {
        let mut rebuilt = BTreeMap::new();
        for session in sessions {
            if session.generation != generation
                || !kajiba_core::names::is_public_id(&session.session_id)
            {
                return Err("invalid git-side session snapshot".into());
            }
            let repo: Option<i64> = sqlx::query_scalar("SELECT id FROM repo WHERE public_id = ?")
                .bind(&session.repo_public_id)
                .fetch_optional(&mut *writer)
                .await
                .map_err(|err| err.to_string())?;
            if let Some(repo) = repo {
                rebuilt.insert(session.session_id.clone(), (repo, session));
            }
        }
        *self.running.lock().unwrap() = rebuilt.values().map(|(repo, _)| *repo).collect();
        *self.sessions.lock().unwrap() = rebuilt;
        *self.generation.lock().unwrap() = Some(generation.into());
        Ok(())
    }

    pub fn opened(&self, repo: i64, session: kajiba_proto::git::v1::Session) {
        self.running.lock().unwrap().insert(repo);
        self.sessions
            .lock()
            .unwrap()
            .insert(session.session_id.clone(), (repo, session));
    }

    pub async fn session_exited(
        &self,
        writer: &mut SqliteConnection,
        session_id: &str,
    ) -> Result<Option<i64>, sqlx::Error> {
        let pending: Option<i64> = sqlx::query_scalar(
            "UPDATE pending_push SET git_session_id = NULL WHERE git_session_id = ? RETURNING repo_id"
        ).bind(session_id).fetch_optional(&mut *writer).await?;
        let session = self.sessions.lock().unwrap().remove(session_id);
        let repo = pending.or_else(|| session.map(|(repo, _)| repo));
        if let Some(repo) = repo {
            if !self
                .sessions
                .lock()
                .unwrap()
                .values()
                .any(|(id, _)| *id == repo)
            {
                self.exited(repo);
            }
        }
        Ok(repo)
    }

    /// Commits the pending row; only then may the caller mutate refs. Call
    /// `exited` once the mutating process has exited, then `reconcile`.
    pub async fn admit(
        &self,
        writer: &mut SqliteConnection,
        repo_id: i64,
        principal_id: Option<i64>,
        correlation_id: &str,
        updates: &[RefUpdate],
    ) -> Result<(), Refused> {
        if self.running(repo_id) {
            return Err(Refused::InProgress);
        }
        self.insert_pending(writer, repo_id, principal_id, correlation_id, updates, None)
            .await
    }

    /// Admit the already-open receive-pack, without mistaking it for another writer.
    /// The caller holds the writer lock from policy evaluation through this commit.
    pub async fn admit_session(
        &self,
        writer: &mut SqliteConnection,
        repo_id: i64,
        principal_id: i64,
        session: &kajiba_proto::git::v1::Session,
        updates: &[RefUpdate],
    ) -> Result<(), Refused> {
        if self
            .sessions
            .lock()
            .unwrap()
            .values()
            .any(|(repo, live)| *repo == repo_id && live.session_id != session.session_id)
        {
            return Err(Refused::InProgress);
        }
        self.insert_pending(
            writer,
            repo_id,
            Some(principal_id),
            &session.correlation_id,
            updates,
            Some(session),
        )
        .await
    }

    async fn insert_pending(
        &self,
        writer: &mut SqliteConnection,
        repo_id: i64,
        principal_id: Option<i64>,
        correlation_id: &str,
        updates: &[RefUpdate],
        session: Option<&kajiba_proto::git::v1::Session>,
    ) -> Result<(), Refused> {
        let json = serde_json::Value::Array(
            updates
                .iter()
                .map(|u| serde_json::json!({ "ref": u.name, "old": u.old, "new": u.new }))
                .collect(),
        )
        .to_string();
        let mut tx = writer.begin().await.map_err(Refused::Database)?;
        let pending: Option<Option<String>> =
            sqlx::query_scalar("SELECT git_session_id FROM pending_push WHERE repo_id = ?")
                .bind(repo_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(Refused::Database)?;
        if let Some(pending) = pending {
            return Err(
                if pending
                    .as_deref()
                    .is_some_and(|id| self.session(id).is_some())
                {
                    Refused::InProgress
                } else {
                    Refused::Retained
                },
            );
        }
        sqlx::query(
            "INSERT INTO pending_push (repo_id, principal_id, correlation_id, updates, created_at, git_session_id, git_generation)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(repo_id)
        .bind(principal_id)
        .bind(correlation_id)
        .bind(json)
        .bind(now())
        .bind(session.map(|s| &s.session_id))
        .bind(session.map(|s| &s.generation))
        .execute(&mut *tx)
        .await
        .map_err(Refused::Database)?;
        tx.commit().await.map_err(Refused::Database)?;
        self.running.lock().unwrap().insert(repo_id);
        Ok(())
    }

    pub fn exited(&self, repo_id: i64) {
        self.running.lock().unwrap().remove(&repo_id);
    }

    /// Converts every eligible row, in `id` order: at startup and on a timer.
    /// Archived repositories resume when unarchived.
    pub async fn reconcile(
        &self,
        writer: &mut SqliteConnection,
        bus: &EventBus,
        state_dir: &Path,
        store: &impl GitStore,
    ) -> Result<(), String> {
        self.reconcile_rows(writer, bus, state_dir, store, None)
            .await
    }

    /// Converts the rows of one repository, after its admitted process exits:
    /// another repository's broken row must not fail this completion.
    pub async fn reconcile_repo(
        &self,
        writer: &mut SqliteConnection,
        bus: &EventBus,
        state_dir: &Path,
        store: &impl GitStore,
        repo_id: i64,
    ) -> Result<(), String> {
        self.reconcile_rows(writer, bus, state_dir, store, Some(repo_id))
            .await
    }

    async fn reconcile_rows(
        &self,
        writer: &mut SqliteConnection,
        bus: &EventBus,
        state_dir: &Path,
        store: &impl GitStore,
        only: Option<i64>,
    ) -> Result<(), String> {
        type Pending = (
            i64,
            i64,
            String,
            String,
            Option<i64>,
            Option<String>,
            String,
            String,
            Option<String>,
            Option<String>,
        );
        let rows: Vec<Pending> = sqlx::query_as(
            "SELECT pending_push.id, repo.id, repo.public_id, owner.name || '/' || repo.name,
                    principal_id, principal.name, correlation_id, updates, git_session_id, git_generation
             FROM pending_push JOIN repo ON repo.id = repo_id JOIN user owner ON owner.id = repo.owner_id
             LEFT JOIN user principal ON principal.id = principal_id
             WHERE repo.archived_at IS NULL AND (?1 IS NULL OR repo.id = ?1)
             ORDER BY pending_push.id",
        )
        .bind(only)
        .fetch_all(&mut *writer)
        .await
        .map_err(|err| format!("push reconciliation: {err}"))?;
        for (
            id,
            repo_id,
            public_id,
            repo,
            principal_id,
            user,
            correlation_id,
            updates,
            session,
            generation,
        ) in rows
        {
            if self.running.lock().unwrap().contains(&repo_id) {
                continue;
            }
            if session.is_some()
                && generation.as_ref().is_some_and(|old| {
                    self.generation()
                        .as_ref()
                        .is_none_or(|current| current == old)
                })
            {
                continue;
            }
            let path = state_dir.join(format!("repos/{public_id}.git"));
            let current: BTreeMap<String, String> = store
                .refs(&path)
                .map_err(|err| format!("push reconciliation: {repo}: {err}"))?
                .into_iter()
                .map(|r| (r.name, r.target))
                .collect();
            let mut applied = Vec::new();
            let mut unexpected = None;
            for update in
                parse(&updates).map_err(|err| format!("push reconciliation: {repo}: {err}"))?
            {
                let value = current.get(&update.name).map_or(ZERO, String::as_str);
                if update.old == update.new || value == update.old {
                    continue;
                } else if value == update.new {
                    applied.push(update);
                } else {
                    unexpected = Some(update.name);
                    break;
                }
            }
            if let Some(name) = unexpected {
                // ADR 0022 names no event for this; the row stays and blocks the repository.
                tracing::error!(event = "push-retained", repo, "ref" = name, correlation_id);
                continue;
            }
            let user = user.unwrap_or_else(|| "system".into());
            let result = async {
                let mut tx = writer.begin().await?;
                for update in &applied {
                    let now = now();
                    let public_id = new_public_id(&mut tx, now as u128).await?;
                    append_event(
                        &mut tx,
                        &NewEvent {
                            public_id,
                            kind: "push".into(),
                            version: 1,
                            occurred_at: now,
                            principal_kind: if principal_id.is_some() { "user" } else { "system" }.into(),
                            principal_id,
                            repo_id: Some(repo_id),
                            correlation_id: correlation_id.clone(),
                            payload: serde_json::json!({ "ref": update.name, "old": update.old, "new": update.new })
                                .to_string(),
                        },
                    )
                    .await?;
                }
                checkpoint("converting");
                sqlx::query("DELETE FROM pending_push WHERE id = ?")
                    .bind(id)
                    .execute(&mut *tx)
                    .await?;
                bus.commit(tx).await
            }
            .await;
            result.map_err(|err| format!("push reconciliation: {repo}: {err}"))?;
            for update in applied {
                tracing::info!(
                    event = "push",
                    request_id = correlation_id,
                    repo,
                    "ref" = update.name,
                    old = update.old,
                    new = update.new,
                    user
                );
            }
        }
        Ok(())
    }
}

// ADR 0003 asks for a timer without a period.
const RECOVERY_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);

impl Gate {
    pub async fn recover_periodically(
        &self,
        clock: &impl Clock,
        writer: &tokio::sync::Mutex<SqliteConnection>,
        bus: &EventBus,
        state_dir: &Path,
        store: &impl GitStore,
    ) {
        loop {
            tokio::select! {
                () = clock.sleep(RECOVERY_INTERVAL) => {},
                () = self.wake.notified() => {},
            }
            self.recover_once(&mut *writer.lock().await, bus, state_dir, store)
                .await;
        }
    }

    /// One pass of the timer: an error is logged and the next pass retries.
    pub async fn recover_once(
        &self,
        writer: &mut SqliteConnection,
        bus: &EventBus,
        state_dir: &Path,
        store: &impl GitStore,
    ) {
        if let Err(err) = self.reconcile(writer, bus, state_dir, store).await {
            // Not in ADR 0022's list; the next tick retries.
            tracing::error!(event = "push-recovery-failed", err);
        }
    }
}

/// Test-only crash injection: the process aborts at the checkpoint named by
/// `KAJIBA_TEST_CRASH_AT`. Compiled out of every non-test build.
#[cfg(test)]
pub(crate) fn checkpoint(name: &str) {
    if std::env::var("KAJIBA_TEST_CRASH_AT").as_deref() == Ok(name) {
        std::process::abort();
    }
}

#[cfg(not(test))]
pub(crate) fn checkpoint(_: &str) {}

fn parse(updates: &str) -> Result<Vec<RefUpdate>, String> {
    let field = |value: &serde_json::Value, name: &str| {
        value[name]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| format!("invalid pending updates: missing {name}"))
    };
    serde_json::from_str::<Vec<serde_json::Value>>(updates)
        .map_err(|err| format!("invalid pending updates: {err}"))?
        .iter()
        .map(|value| {
            Ok(RefUpdate {
                name: field(value, "ref")?,
                old: field(value, "old")?,
                new: field(value, "new")?,
            })
        })
        .collect()
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}
