//! CI retention (0010, ADR 0115): logs of attempts finished more than `ci.log_retention`
//! ago are deleted, file then index; runs, jobs and attempts are kept. Worktree runs go
//! whole `ci.worktree_retention` after their creation, bundle included (0097).

use super::runs::{AttemptState, JobState, RunState, attempt_transition, job_transition};
use sqlx::{Connection, SqliteConnection};
use std::{path::Path, time::Duration};

fn remove(path: &Path) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Err(err) if err.kind() != std::io::ErrorKind::NotFound => {
            Err(format!("{}: {err}", path.display()))
        }
        _ => Ok(()),
    }
}

/// End an unfinished run in `state` (`cancelled` or `failed`) with `reason`: its running
/// attempts and unfinished job follow where the state machines allow it (a job that never
/// ran is cancelled), and `run-finished` is journaled by `system`.
pub(crate) async fn stop(
    tx: &mut SqliteConnection,
    (run_id, run, repo_id): (i64, &str, i64),
    state: RunState,
    reason: Option<&str>,
    correlation_id: &str,
    now: i64,
) -> Result<(), sqlx::Error> {
    let to = |from: &str| -> Option<&'static str> {
        let from = JobState::parse(from)?;
        [JobState::parse(state.as_str())?, JobState::Cancelled]
            .into_iter()
            .find(|to| job_transition(from, *to).is_ok())
            .map(JobState::as_str)
    };
    let jobs: Vec<(i64, String)> = sqlx::query_as("SELECT id, state FROM job WHERE run_id = ?")
        .bind(run_id)
        .fetch_all(&mut *tx)
        .await?;
    let attempt = AttemptState::parse(state.as_str())
        .filter(|to| attempt_transition(AttemptState::Running, *to).is_ok())
        .unwrap_or(AttemptState::Cancelled);
    for (job, from) in jobs {
        sqlx::query(
            "UPDATE job_attempt SET state = ?, finished_at = ? WHERE job_id = ? AND state = 'running'",
        )
        .bind(attempt.as_str())
        .bind(now)
        .bind(job)
        .execute(&mut *tx)
        .await?;
        if let Some(to) = to(&from) {
            sqlx::query("UPDATE job SET state = ?, finished_at = ? WHERE id = ?")
                .bind(to)
                .bind(now)
                .bind(job)
                .execute(&mut *tx)
                .await?;
        }
    }
    sqlx::query("UPDATE run SET state = ?, finished_at = ?, failure_reason = ? WHERE id = ?")
        .bind(state.as_str())
        .bind(now)
        .bind(reason)
        .bind(run_id)
        .execute(&mut *tx)
        .await?;
    super::attempts::journal(
        tx,
        "run-finished",
        None,
        repo_id,
        correlation_id,
        serde_json::json!({ "run": run, "state": state.as_str(), "check": null, "reason": reason }),
        now,
    )
    .await
}

/// Unfinished runs among `runs` (id, public id, state, repo id).
fn unfinished(
    runs: &[(i64, String, String, i64)],
) -> impl Iterator<Item = &(i64, String, String, i64)> {
    runs.iter()
        .filter(|(_, _, state, _)| RunState::parse(state).is_some_and(|state| !state.is_terminal()))
}

/// Delete every worktree run created before `now - retention`, with its jobs, attempts,
/// log files and bundle; an unfinished one is cancelled first (0097). Files go before
/// rows, so an interrupted pass leaves rows that the next one deletes. Returns how many
/// runs were deleted.
// shortcut: a runner still building a cancelled attempt learns it at its next message
// about it (log chunk or result); push a CancelAttempt once runs can be cancelled.
pub async fn purge_worktree_runs(
    writer: &mut SqliteConnection,
    logs: &Path,
    bundles: &Path,
    retention: Duration,
    now: i64,
) -> Result<u64, String> {
    let text = |err: sqlx::Error| err.to_string();
    let cutoff =
        super::expiry(0, retention).map_or(i64::MIN, |retention| now.saturating_sub(retention));
    let mut tx = writer.begin().await.map_err(text)?;
    let runs: Vec<(i64, String, String, i64)> = sqlx::query_as(
        "SELECT id, public_id, state, repo_id FROM run WHERE trigger = 'worktree' AND created_at < ?",
    )
    .bind(cutoff)
    .fetch_all(&mut *tx)
    .await
    .map_err(text)?;
    for (id, run, _, repo_id) in unfinished(&runs) {
        stop(
            &mut tx,
            (*id, run, *repo_id),
            RunState::Cancelled,
            None,
            run,
            now,
        )
        .await
        .map_err(text)?;
        tracing::info!(event = "run-state", run, state = "cancelled");
    }
    for (id, run, ..) in &runs {
        let attempts: Vec<String> = sqlx::query_scalar(
            "SELECT job_attempt.public_id FROM job_attempt JOIN job ON job.id = job_id WHERE job.run_id = ?",
        )
        .bind(id)
        .fetch_all(&mut *tx)
        .await
        .map_err(text)?;
        for attempt in attempts {
            remove(&logs.join(format!("{attempt}.log")))?;
        }
        remove(&bundles.join(format!("{run}.bundle")))?;
        // Jobs, attempts, steps, log index and artifacts follow by cascade.
        sqlx::query("DELETE FROM run WHERE id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(text)?;
    }
    tx.commit().await.map_err(text)?;
    Ok(runs.len() as u64)
}

/// Delete the bundle of every terminal worktree run (0122): runs and logs stay until
/// retention. A bundle whose run row is absent is left alone: it may belong to a run
/// whose creation has not committed yet. Returns how many bundles were deleted.
pub async fn drop_finished_bundles(
    connection: &mut SqliteConnection,
    bundles: &Path,
) -> Result<u64, String> {
    let entries =
        std::fs::read_dir(bundles).map_err(|err| format!("{}: {err}", bundles.display()))?;
    let mut dropped = 0;
    for entry in entries {
        let path = entry.map_err(|err| err.to_string())?.path();
        let Some(run) = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(".bundle"))
        else {
            continue;
        };
        let state: Option<String> = sqlx::query_scalar("SELECT state FROM run WHERE public_id = ?")
            .bind(run)
            .fetch_optional(&mut *connection)
            .await
            .map_err(|err| err.to_string())?;
        if state
            .and_then(|state| RunState::parse(&state))
            .is_some_and(RunState::is_terminal)
        {
            remove(&path)?;
            dropped += 1;
        }
    }
    Ok(dropped)
}

/// At startup: fail with reason `bundle-missing` every unfinished worktree run whose
/// bundle is gone, as after a restore from a dump, which skips `ci/bundles/` (0097).
/// Returns the failed runs' public ids.
pub async fn fail_missing_bundles(
    writer: &mut SqliteConnection,
    bundles: &Path,
    now: i64,
) -> Result<Vec<String>, sqlx::Error> {
    let mut tx = writer.begin().await?;
    let runs: Vec<(i64, String, String, i64)> =
        sqlx::query_as("SELECT id, public_id, state, repo_id FROM run WHERE trigger = 'worktree'")
            .fetch_all(&mut *tx)
            .await?;
    let mut failed = Vec::new();
    for (id, run, _, repo_id) in unfinished(&runs) {
        if bundles.join(format!("{run}.bundle")).exists() {
            continue;
        }
        stop(
            &mut tx,
            (*id, run, *repo_id),
            RunState::Failed,
            Some("bundle-missing"),
            run,
            now,
        )
        .await?;
        failed.push(run.clone());
    }
    tx.commit().await?;
    Ok(failed)
}

/// Delete the log file and `log_chunk` rows of every attempt finished before
/// `now - retention`; returns how many attempts lost their logs. A file is removed
/// before its rows, so an interrupted pass leaves rows that the next one deletes.
pub async fn purge_logs(
    writer: &mut SqliteConnection,
    logs: &Path,
    retention: Duration,
    now: i64,
) -> Result<u64, String> {
    let cutoff =
        super::expiry(0, retention).map_or(i64::MIN, |retention| now.saturating_sub(retention));
    let mut tx = writer.begin().await.map_err(|err| err.to_string())?;
    let attempts: Vec<(i64, String)> = sqlx::query_as(
        "SELECT id, public_id FROM job_attempt
         WHERE finished_at < ? AND state != 'running'
           AND EXISTS (SELECT 1 FROM log_chunk WHERE attempt_id = job_attempt.id)",
    )
    .bind(cutoff)
    .fetch_all(&mut *tx)
    .await
    .map_err(|err| err.to_string())?;
    for (id, public_id) in &attempts {
        match std::fs::remove_file(logs.join(format!("{public_id}.log"))) {
            Err(err) if err.kind() != std::io::ErrorKind::NotFound => {
                return Err(format!("{public_id}.log: {err}"));
            }
            _ => {}
        }
        sqlx::query(
            "UPDATE job_attempt SET purged_log_length =
             (SELECT COALESCE(MAX(offset + length), 0) FROM log_chunk WHERE attempt_id = ?)
             WHERE id = ?",
        )
        .bind(id)
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(|err| err.to_string())?;
        sqlx::query("DELETE FROM log_chunk WHERE attempt_id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(|err| err.to_string())?;
    }
    tx.commit().await.map_err(|err| err.to_string())?;
    Ok(attempts.len() as u64)
}

/// The next epoch second, after `now`, whose local time is `hour:minute`.
pub fn next_run(now: i64, (hour, minute): (u8, u8)) -> i64 {
    // SAFETY: localtime_r and mktime only read and write the `tm` we own.
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&(now as libc::time_t), &mut tm);
        for _ in 0..3 {
            (tm.tm_hour, tm.tm_min, tm.tm_sec, tm.tm_isdst) = (hour.into(), minute.into(), 0, -1);
            let at = libc::mktime(&mut tm) as i64;
            if at > now {
                return at;
            }
            // mktime normalized `tm`: one day later, then retry (DST shifts the hour).
            tm.tm_mday += 1;
        }
    }
    now + 86400
}

/// The daily pass at `run_at`, local time; the operator also runs it on demand
/// (`kajiba-server admin retention`).
pub async fn purge_daily(
    writer: &super::Writer,
    pass: &Pass,
    run_at: (u8, u8),
    maintenance: &crate::transport::maintenance::Admission,
) {
    loop {
        let now = super::runs::now() / 1000;
        let wait = next_run(now, run_at) - now;
        tokio::time::sleep(Duration::from_secs(wait.max(1) as u64)).await;
        match pass.run_scheduled(writer, maintenance).await {
            Ok((logs, worktree_runs)) => {
                tracing::info!(event = "retention-completed", logs, worktree_runs)
            }
            Err(err) => tracing::warn!(event = "retention-failed", err),
        }
    }
}

/// What one retention pass deletes, and where.
#[derive(Clone)]
pub struct Pass {
    /// `ci/logs` and `ci/bundles` of the state directory.
    pub logs: std::path::PathBuf,
    pub bundles: std::path::PathBuf,
    pub log_retention: Duration,
    pub worktree_retention: Duration,
}

impl Pass {
    pub(crate) async fn run_scheduled(
        &self,
        writer: &super::Writer,
        maintenance: &crate::transport::maintenance::Admission,
    ) -> Result<(u64, u64), String> {
        let _permit = maintenance.background().await;
        self.run(&mut *writer.lock().await, super::runs::now())
            .await
    }

    /// Worktree runs, then old logs; returns how many attempts lost their logs and how
    /// many worktree runs were deleted.
    pub async fn run(&self, writer: &mut SqliteConnection, now: i64) -> Result<(u64, u64), String> {
        let runs = purge_worktree_runs(
            writer,
            &self.logs,
            &self.bundles,
            self.worktree_retention,
            now,
        )
        .await?;
        let logs = purge_logs(writer, &self.logs, self.log_retention, now).await?;
        drop_finished_bundles(writer, &self.bundles).await?;
        Ok((logs, runs))
    }
}
