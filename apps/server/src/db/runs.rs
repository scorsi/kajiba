//! Runs, jobs and attempts (0008, 0010): state machines, run creation and the `ci` handler.

use super::events::{Event, Handler, NewEvent, append_event};
use super::users::new_public_id;
use kajiba_gitstore::{GitStore, Gix};
use sqlx::SqliteConnection;

/// The server clock in epoch milliseconds.
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// ADR 0105: jobs are replayable with at most 3 attempts.
pub const MAX_ATTEMPTS: i64 = 3;

macro_rules! states {
    ($name:ident { $($variant:ident = $text:literal),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum $name { $($variant),+ }

        impl $name {
            #[cfg(test)]
            pub const ALL: &[Self] = &[$(Self::$variant),+];

            pub fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $text),+ }
            }

            pub fn parse(text: &str) -> Option<Self> {
                match text { $($text => Some(Self::$variant),)+ _ => None }
            }
        }
    };
}

states!(RunState {
    Queued = "queued",
    Running = "running",
    Succeeded = "succeeded",
    Failed = "failed",
    Cancelled = "cancelled",
});

impl RunState {
    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::Queued | Self::Running)
    }
}

states!(JobState {
    Pending = "pending",
    Assigned = "assigned",
    Running = "running",
    Succeeded = "succeeded",
    Failed = "failed",
    Cancelled = "cancelled",
    Lost = "lost",
});

states!(AttemptState {
    Running = "running",
    Succeeded = "succeeded",
    Failed = "failed",
    Cancelled = "cancelled",
    Lost = "lost",
});

/// A transition the domain refuses (0008).
#[derive(Debug, PartialEq, Eq)]
pub struct Refused {
    pub from: &'static str,
    pub to: &'static str,
}

fn check<S: Copy>(
    from: S,
    to: S,
    allowed: bool,
    name: fn(S) -> &'static str,
) -> Result<S, Refused> {
    if allowed {
        Ok(to)
    } else {
        Err(Refused {
            from: name(from),
            to: name(to),
        })
    }
}

/// `queued → running → succeeded | failed | cancelled`; a queued run may also end
/// without starting (cancelled, or failed before any attempt).
pub fn run_transition(from: RunState, to: RunState) -> Result<RunState, Refused> {
    use RunState::*;
    let allowed = matches!(
        (from, to),
        (Queued, Running | Failed | Cancelled) | (Running, Succeeded | Failed | Cancelled)
    );
    check(from, to, allowed, RunState::as_str)
}

/// `pending → assigned → running → succeeded | failed | cancelled | lost`. An unfinished
/// job may be cancelled; when its attempt is lost before a result, the job goes back to
/// `pending` for a replay, or ends `failed` (`lease-expired`) or `lost` (not replayable).
pub fn job_transition(from: JobState, to: JobState) -> Result<JobState, Refused> {
    use JobState::*;
    let allowed = matches!(
        (from, to),
        (Pending, Assigned | Cancelled)
            | (Assigned, Running | Pending | Failed | Cancelled | Lost)
            | (Running, Succeeded | Failed | Cancelled | Lost | Pending)
    );
    check(from, to, allowed, JobState::as_str)
}

/// `running → succeeded | failed | cancelled | lost`: the first terminal state wins.
pub fn attempt_transition(from: AttemptState, to: AttemptState) -> Result<AttemptState, Refused> {
    let allowed = from == AttemptState::Running && to != AttemptState::Running;
    check(from, to, allowed, AttemptState::as_str)
}

pub struct NewRun<'a> {
    pub repo_id: i64,
    pub sha: &'a str,
    pub reference: Option<&'a str>,
    /// `push` or `manual` (0013).
    pub trigger: &'a str,
    pub event_id: Option<i64>,
    pub triggered_by: Option<i64>,
    pub correlation_id: &'a str,
    pub now: i64,
    /// A worktree run's selection and requirements (0097); `None` for the others.
    pub worktree: Option<Worktree<'a>>,
}

/// What a worktree run builds and where (0097).
pub struct Worktree<'a> {
    /// Check names; empty for the whole `nix flake check`.
    pub checks: &'a [String],
    pub arch: &'a str,
    pub os: &'a str,
    /// Required runner tags.
    pub features: &'a [String],
}

/// Insert a queued run, its one replayable `nix` job and `run-created` in the caller's
/// transaction; commit through `EventBus::commit`. Returns the run's public id.
pub async fn create_run(
    tx: &mut SqliteConnection,
    run: &NewRun<'_>,
) -> Result<String, sqlx::Error> {
    let public_id = new_public_id(tx, run.now as u128).await?;
    let run_id = sqlx::query(
        "INSERT INTO run (public_id, repo_id, sha, ref, trigger, event_id, triggered_by, state, created_at, checks)
         VALUES (?, ?, ?, ?, ?, ?, ?, 'queued', ?, ?)",
    )
    .bind(&public_id)
    .bind(run.repo_id)
    .bind(run.sha)
    .bind(run.reference)
    .bind(run.trigger)
    .bind(run.event_id)
    .bind(run.triggered_by)
    .bind(run.now)
    .bind(
        run.worktree
            .as_ref()
            .filter(|worktree| !worktree.checks.is_empty())
            .map(|worktree| serde_json::json!(worktree.checks).to_string()),
    )
    .execute(&mut *tx)
    .await?
    .last_insert_rowid();
    let requirements = match &run.worktree {
        Some(worktree) => serde_json::json!({
            "backend": "nix",
            "arch": worktree.arch,
            "os": worktree.os,
            "tags": worktree.features,
        }),
        None => serde_json::json!({ "backend": "nix" }),
    };
    let job_id = new_public_id(tx, run.now as u128).await?;
    sqlx::query(
        "INSERT INTO job (public_id, run_id, name, requirements, replayable, max_attempts, state, created_at)
         VALUES (?, ?, 'flake-check', ?, 1, ?, 'pending', ?)",
    )
    .bind(job_id)
    .bind(run_id)
    // Opaque to the schema (0013); read back by matching only.
    .bind(requirements.to_string())
    .bind(MAX_ATTEMPTS)
    .bind(run.now)
    .execute(&mut *tx)
    .await?;
    let event_id = new_public_id(tx, run.now as u128).await?;
    append_event(
        tx,
        &NewEvent {
            public_id: event_id,
            kind: "run-created".into(),
            version: 1,
            occurred_at: run.now,
            principal_kind: if run.triggered_by.is_some() {
                "user"
            } else {
                "system"
            }
            .into(),
            principal_id: run.triggered_by,
            repo_id: Some(run.repo_id),
            correlation_id: run.correlation_id.into(),
            payload: serde_json::json!({
                "run": public_id,
                "trigger": run.trigger,
                "ref": run.reference,
                "sha": run.sha,
            })
            .to_string(),
        },
    )
    .await?;
    Ok(public_id)
}

/// The `ci` handler (0010, 0013): a push by a `local` user on a branch or a tag, not a
/// deletion, of a non-archived repository with CI enabled creates one run. The run is
/// keyed on the push event (`run.event_id`), so a replay creates nothing.
pub struct Ci {
    pub now: fn() -> i64,
    pub repos: std::path::PathBuf,
    /// Wakes matching once the new job is committed (0008).
    pub wake: std::sync::Arc<tokio::sync::Notify>,
}

impl Handler for Ci {
    const NAME: &'static str = "ci";

    async fn apply(&mut self, tx: &mut SqliteConnection, event: &Event) -> Result<(), sqlx::Error> {
        let data = &event.data;
        if data.kind != "push" || data.principal_kind != "user" {
            return Ok(());
        }
        let (Some(user_id), Some(repo_id)) = (data.principal_id, data.repo_id) else {
            return Ok(());
        };
        let Ok(payload) = serde_json::from_str::<serde_json::Value>(&data.payload) else {
            return Ok(());
        };
        let (Some(reference), Some(new)) = (payload["ref"].as_str(), payload["new"].as_str())
        else {
            return Ok(());
        };
        if !(reference.starts_with("refs/heads/") || reference.starts_with("refs/tags/"))
            || new.bytes().all(|b| b == b'0')
        {
            return Ok(());
        }
        let eligible: Option<String> = sqlx::query_scalar(
            "SELECT repo.public_id FROM repo JOIN user ON user.id = ?
             WHERE repo.id = ? AND repo.archived_at IS NULL AND repo.ci_enabled = 1
               AND user.kind = 'local'
               AND NOT EXISTS (SELECT 1 FROM run WHERE event_id = ?)",
        )
        .bind(user_id)
        .bind(repo_id)
        .bind(event.id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(repo) = eligible else {
            return Ok(());
        };
        let sha = if reference.starts_with("refs/tags/") {
            let root = self.repos.clone();
            let dir = root.join(format!("{repo}.git"));
            let new = new.to_owned();
            tokio::task::spawn_blocking(move || Gix::shared(&root).peel_commit(&dir, &new))
                .await
                .map_err(|err| sqlx::Error::Io(std::io::Error::other(err)))?
                .map_err(|err| sqlx::Error::Io(std::io::Error::other(err)))?
        } else {
            new.to_owned()
        };
        let run = create_run(
            tx,
            &NewRun {
                repo_id,
                sha: &sha,
                reference: Some(reference),
                trigger: "push",
                event_id: Some(event.id),
                triggered_by: Some(user_id),
                correlation_id: &data.correlation_id,
                now: (self.now)(),
                worktree: None,
            },
        )
        .await?;
        // shortcut: logged before the dispatcher commits, so a failed commit logs it again on retry.
        tracing::info!(event = "run-state", run, state = RunState::Queued.as_str());
        // Matching takes the writer lock, so it runs after this transaction commits.
        self.wake.notify_one();
        Ok(())
    }
}
