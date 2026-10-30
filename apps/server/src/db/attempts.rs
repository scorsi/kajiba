//! Attempts (0008): matching pending jobs to connected runners and acceptance.

use super::events::{NewEvent, append_event};
use super::runs::{JobState, RunState, job_transition, run_transition};
use super::users::new_public_id;
use sqlx::SqliteConnection;
use std::collections::BTreeMap;

/// ADR 0105: a lease lasts 30 s from the server's clock.
pub const LEASE_MS: i64 = 30_000;

/// The fixed `job.pending_reason` set (0109).
pub const NO_CONNECTED_RUNNER: &str = "no-connected-runner";
pub const BACKEND_UNAVAILABLE: &str = "backend-unavailable";
pub const TAGS_UNMATCHED: &str = "tags-unmatched";
pub const RUNNERS_BUSY: &str = "runners-busy";

/// What a connected runner announced at admission (within its declaration).
#[derive(Clone, Debug)]
pub struct Offer {
    pub runner_id: i64,
    pub arch: String,
    pub os: String,
    pub backends: Vec<String>,
    pub tags: Vec<String>,
    pub max_jobs: i64,
}

/// `job.requirements` (0013: opaque to the schema, read only here).
#[derive(serde::Deserialize, Default)]
struct Requirements {
    #[serde(default)]
    backend: String,
    #[serde(default)]
    tags: Vec<String>,
    arch: Option<String>,
    os: Option<String>,
}

/// An attempt created by matching, to send to its runner.
#[derive(Debug, PartialEq, Eq)]
pub struct Assigned {
    pub runner_id: i64,
    pub attempt: String,
    pub repo: String,
    pub sha: String,
    /// The Nix system of the matched runner (0010).
    pub system: String,
}

fn place<'a>(
    requirements: &Requirements,
    offers: &'a [Offer],
    used: &BTreeMap<i64, i64>,
) -> Result<&'a Offer, &'static str> {
    if offers.is_empty() {
        return Err(NO_CONNECTED_RUNNER);
    }
    let backend: Vec<&Offer> = offers
        .iter()
        .filter(|offer| offer.backends.contains(&requirements.backend))
        .collect();
    if backend.is_empty() {
        return Err(BACKEND_UNAVAILABLE);
    }
    // Required arch and os count as tags: the reason set is fixed (0109).
    let fit: Vec<&Offer> = backend
        .into_iter()
        .filter(|offer| {
            requirements.tags.iter().all(|tag| offer.tags.contains(tag))
                && requirements
                    .arch
                    .as_ref()
                    .is_none_or(|arch| *arch == offer.arch)
                && requirements.os.as_ref().is_none_or(|os| *os == offer.os)
        })
        .collect();
    if fit.is_empty() {
        return Err(TAGS_UNMATCHED);
    }
    fit.into_iter()
        .find(|offer| used.get(&offer.runner_id).copied().unwrap_or(0) < offer.max_jobs)
        .ok_or(RUNNERS_BUSY)
}

/// Offer pending jobs, oldest first, to the runners in `offers` (in order) within their
/// `max_jobs`. Each placed job gets attempt n+1 (`running`, leased) and goes `assigned`;
/// the others keep `pending` with their reason. Returns the assignments and the number
/// of jobs still pending.
pub async fn assign(
    tx: &mut SqliteConnection,
    offers: &[Offer],
    now: i64,
) -> Result<(Vec<Assigned>, i64), sqlx::Error> {
    let mut used: BTreeMap<i64, i64> = sqlx::query_as(
        "SELECT runner_id, count(*) FROM job_attempt WHERE state = 'running' GROUP BY runner_id",
    )
    .fetch_all(&mut *tx)
    .await?
    .into_iter()
    .collect();
    let jobs: Vec<(i64, String, String, String)> = sqlx::query_as(
        "SELECT job.id, job.requirements, run.sha, repo.public_id FROM job
         JOIN run ON run.id = job.run_id JOIN repo ON repo.id = run.repo_id
         WHERE job.state = 'pending' ORDER BY job.id",
    )
    .fetch_all(&mut *tx)
    .await?;
    let (mut assigned, mut pending) = (Vec::new(), 0);
    for (job_id, requirements, sha, repo) in jobs {
        // Unreadable requirements match no backend.
        let requirements = serde_json::from_str(&requirements).unwrap_or_default();
        let offer = match place(&requirements, offers, &used) {
            Ok(offer) => offer,
            Err(reason) => {
                pending += 1;
                sqlx::query(
                    "UPDATE job SET pending_reason = ? WHERE id = ? AND pending_reason IS NOT ?",
                )
                .bind(reason)
                .bind(job_id)
                .bind(reason)
                .execute(&mut *tx)
                .await?;
                continue;
            }
        };
        *used.entry(offer.runner_id).or_default() += 1;
        let attempt = new_public_id(tx, now as u128).await?;
        sqlx::query(
            "INSERT INTO job_attempt (public_id, job_id, number, runner_id, state, lease_expires_at, started_at)
             VALUES (?, ?, (SELECT COALESCE(max(number), 0) + 1 FROM job_attempt WHERE job_id = ?), ?, 'running', ?, ?)",
        )
        .bind(&attempt)
        .bind(job_id)
        .bind(job_id)
        .bind(offer.runner_id)
        .bind(now + LEASE_MS)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        sqlx::query("UPDATE job SET state = 'assigned', pending_reason = NULL WHERE id = ?")
            .bind(job_id)
            .execute(&mut *tx)
            .await?;
        assigned.push(Assigned {
            runner_id: offer.runner_id,
            attempt,
            repo,
            sha,
            system: format!("{}-{}", offer.arch, offer.os),
        });
    }
    Ok((assigned, pending))
}

/// The answer to a runner message about an attempt.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Not the job's current attempt held by this runner, or no longer running (0008).
    Cancel,
    /// Recorded; the runs whose state changed, to log as `run-state`.
    Done(Vec<(String, RunState)>),
}

/// The attempt as seen by the runner that holds it, when it is its job's current one.
#[derive(sqlx::FromRow)]
pub(crate) struct Current {
    pub public_id: String,
    pub state: String,
    pub job_id: i64,
    pub job_state: String,
    pub run_id: i64,
    pub run: String,
    pub run_state: String,
    pub repo_id: i64,
}

pub(crate) async fn current(
    tx: &mut SqliteConnection,
    runner_id: i64,
    attempt: &str,
) -> Result<Option<Current>, sqlx::Error> {
    sqlx::query_as(
        "SELECT a.public_id, a.state, j.id AS job_id, j.state AS job_state, r.id AS run_id,
                r.public_id AS run, r.state AS run_state, r.repo_id
         FROM job_attempt a JOIN job j ON j.id = a.job_id JOIN run r ON r.id = j.run_id
         WHERE a.public_id = ? AND a.runner_id = ?
           AND a.number = (SELECT max(number) FROM job_attempt WHERE job_id = a.job_id)",
    )
    .bind(attempt)
    .bind(runner_id)
    .fetch_optional(&mut *tx)
    .await
}

/// Append a CI event (0108); `runner` is the principal, `system` when none.
pub(crate) async fn journal(
    tx: &mut SqliteConnection,
    kind: &str,
    runner: Option<i64>,
    repo_id: i64,
    correlation_id: &str,
    payload: serde_json::Value,
    now: i64,
) -> Result<(), sqlx::Error> {
    let public_id = new_public_id(tx, now as u128).await?;
    append_event(
        tx,
        &NewEvent {
            public_id,
            kind: kind.into(),
            version: 1,
            occurred_at: now,
            principal_kind: if runner.is_some() { "runner" } else { "system" }.into(),
            principal_id: runner,
            repo_id: Some(repo_id),
            correlation_id: correlation_id.into(),
            payload: payload.to_string(),
        },
    )
    .await?;
    Ok(())
}

/// Move an assigned job and a queued run to `running`; the run's start is journaled
/// (`run-started`). Nothing changes for an attempt already started.
pub(crate) async fn start(
    tx: &mut SqliteConnection,
    attempt: &mut Current,
    runner: (i64, &str),
    correlation_id: &str,
    now: i64,
) -> Result<Vec<(String, RunState)>, sqlx::Error> {
    if let Some(Ok(to)) = JobState::parse(&attempt.job_state)
        .filter(|from| *from == JobState::Assigned)
        .map(|from| job_transition(from, JobState::Running))
    {
        sqlx::query("UPDATE job SET state = ? WHERE id = ?")
            .bind(to.as_str())
            .bind(attempt.job_id)
            .execute(&mut *tx)
            .await?;
        attempt.job_state = to.as_str().into();
    }
    let Some(Ok(to)) = RunState::parse(&attempt.run_state)
        .filter(|from| *from == RunState::Queued)
        .map(|from| run_transition(from, RunState::Running))
    else {
        return Ok(Vec::new());
    };
    sqlx::query("UPDATE run SET state = ?, started_at = ? WHERE id = ?")
        .bind(to.as_str())
        .bind(now)
        .bind(attempt.run_id)
        .execute(&mut *tx)
        .await?;
    attempt.run_state = to.as_str().into();
    journal(
        tx,
        "run-started",
        Some(runner.0),
        attempt.repo_id,
        correlation_id,
        serde_json::json!({ "run": attempt.run, "attempt": attempt.public_id, "runner": runner.1 }),
        now,
    )
    .await?;
    Ok(vec![(attempt.run.clone(), to)])
}

/// The runner accepted `attempt`: job and run go `running` (0008).
pub async fn accept(
    tx: &mut SqliteConnection,
    runner: (i64, &str),
    attempt: &str,
    correlation_id: &str,
    now: i64,
) -> Result<Outcome, sqlx::Error> {
    let Some(mut current) = current(tx, runner.0, attempt).await? else {
        return Ok(Outcome::Cancel);
    };
    if current.state != "running" {
        return Ok(Outcome::Cancel);
    }
    Ok(Outcome::Done(
        start(tx, &mut current, runner, correlation_id, now).await?,
    ))
}
