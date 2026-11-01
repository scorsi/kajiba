//! Attempts (0008): matching pending jobs to connected runners, acceptance, heartbeats,
//! sequenced logs, steps, results, lease expiry and reconnection.

use super::events::{NewEvent, append_event};
use super::runs::{AttemptState, JobState, RunState, job_transition, run_transition};
use super::users::new_public_id;
use sqlx::SqliteConnection;
use std::{collections::BTreeMap, io::Write, path::Path};

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
    /// A worktree run's selected checks; empty for the whole `nix flake check` (0097).
    pub checks: Vec<String>,
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
    let jobs: Vec<(i64, String, String, String, Option<String>)> = sqlx::query_as(
        "SELECT job.id, job.requirements, run.sha, repo.public_id, run.checks FROM job
         JOIN run ON run.id = job.run_id JOIN repo ON repo.id = run.repo_id
         WHERE job.state = 'pending' ORDER BY job.id",
    )
    .fetch_all(&mut *tx)
    .await?;
    let (mut assigned, mut pending) = (Vec::new(), 0);
    for (job_id, requirements, sha, repo, checks) in jobs {
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
            checks: checks
                .and_then(|checks| serde_json::from_str(&checks).ok())
                .unwrap_or_default(),
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
    pub id: i64,
    pub public_id: String,
    pub state: String,
    pub job_id: i64,
    pub job_state: String,
    pub run_id: i64,
    pub run: String,
    pub run_state: String,
    pub repo_id: i64,
    pub number: i64,
    pub lease_expires_at: i64,
    pub job: String,
    pub replayable: bool,
    pub max_attempts: i64,
    pub runner: String,
}

const ATTEMPT: &str = "SELECT a.id, a.public_id, a.state, j.id AS job_id, j.state AS job_state,
        r.id AS run_id, r.public_id AS run, r.state AS run_state, r.repo_id, a.number,
        a.lease_expires_at, j.public_id AS job, j.replayable, j.max_attempts, n.name AS runner
    FROM job_attempt a JOIN job j ON j.id = a.job_id JOIN run r ON r.id = j.run_id
    JOIN runner n ON n.id = a.runner_id";

pub(crate) async fn current(
    tx: &mut SqliteConnection,
    runner_id: i64,
    attempt: &str,
) -> Result<Option<Current>, sqlx::Error> {
    sqlx::query_as(&format!(
        "{ATTEMPT} WHERE a.public_id = ? AND a.runner_id = ?
           AND a.number = (SELECT max(number) FROM job_attempt WHERE job_id = a.job_id)"
    ))
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
    if current.state != "running" || current.lease_expires_at < now {
        return Ok(Outcome::Cancel);
    }
    Ok(Outcome::Done(
        start(tx, &mut current, runner, correlation_id, now).await?,
    ))
}

/// A heartbeat renews unexpired leases only after the runner accepts the job (0008).
/// Every running attempt of the runner was accepted or continued in its current session:
/// [`abandon`] lost the others at admission.
pub async fn heartbeat(
    writer: &mut SqliteConnection,
    runner_id: i64,
    now: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE job_attempt SET lease_expires_at = ?
         WHERE runner_id = ? AND state = 'running' AND lease_expires_at >= ?
           AND job_id IN (SELECT id FROM job WHERE state = 'running')",
    )
    .bind(now + LEASE_MS)
    .bind(runner_id)
    .bind(now)
    .execute(&mut *writer)
    .await?;
    sqlx::query("UPDATE runner SET last_contact_at = ? WHERE id = ?")
        .bind(now)
        .bind(runner_id)
        .execute(writer)
        .await?;
    Ok(())
}

/// One log chunk of an attempt (0008, 0105).
pub struct Chunk<'a> {
    pub attempt: &'a str,
    pub sequence: u64,
    pub step: u32,
    pub data: &'a [u8],
    pub produced_at_ms: i64,
}

/// ADR 0121: an attempt stores at most 64 MiB of log, then this line.
pub const MAX_LOG: i64 = 64 << 20;
pub const TRUNCATED: &[u8] =
    b"\n[kajiba: log truncated: an attempt stores at most 64 MiB of log]\n";

/// Append the chunk when its sequence is the next one: bytes to
/// `<logs>/<attempt public id>.log`, then its `log_chunk` row (offset, length). A
/// duplicate or a gap changes nothing. Past [`MAX_LOG`] stored bytes, the chunk's
/// remainder is replaced by [`TRUNCATED`], and later chunks are acknowledged without
/// being stored (0121). Returns the highest contiguous sequence to acknowledge and
/// whether this chunk truncated the log, or `None` (cancel) when the attempt is not this
/// runner's current one or a new chunk targets a terminal or expired attempt. Duplicates
/// remain acknowledged. Empty chunks advance the last row's sequence without adding
/// a row, except the first chunk which establishes the acknowledgment position.
pub async fn append_log(
    tx: &mut SqliteConnection,
    logs: &Path,
    runner_id: i64,
    chunk: &Chunk<'_>,
    now: i64,
) -> Result<Option<(u64, bool)>, sqlx::Error> {
    let Some(attempt) = current(tx, runner_id, chunk.attempt).await? else {
        return Ok(None);
    };
    let (highest, end): (i64, i64) = sqlx::query_as(
        "SELECT COALESCE(max(seq), 0), COALESCE(max(offset + length), 0) FROM log_chunk WHERE attempt_id = ?",
    )
    .bind(attempt.id)
    .fetch_one(&mut *tx)
    .await?;
    if chunk.sequence <= highest as u64 {
        return Ok(Some((highest as u64, false)));
    }
    if attempt.state != "running" || attempt.lease_expires_at < now {
        return Ok(None);
    }
    if i64::try_from(chunk.sequence) != Ok(highest + 1) {
        return Ok(Some((highest as u64, false)));
    }
    if end > MAX_LOG || (chunk.data.is_empty() && highest > 0) {
        // Truncated or empty: the last row takes the sequence, so no row is added.
        sqlx::query("UPDATE log_chunk SET seq = ? WHERE attempt_id = ? AND seq = ?")
            .bind(highest + 1)
            .bind(attempt.id)
            .bind(highest)
            .execute(&mut *tx)
            .await?;
        return Ok(Some((chunk.sequence, false)));
    }
    let room = (MAX_LOG - end) as usize;
    let truncated = chunk.data.len() > room;
    let data = if truncated {
        [&chunk.data[..room], TRUNCATED].concat()
    } else {
        chunk.data.to_vec()
    };
    // Bytes past the index's end are a write whose row never committed: overwrite them.
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(logs.join(format!("{}.log", attempt.public_id)))?;
    file.set_len(end as u64)?;
    std::io::Seek::seek(&mut file, std::io::SeekFrom::Start(end as u64))?;
    file.write_all(&data)?;
    sqlx::query(
        "INSERT INTO log_chunk (attempt_id, seq, step_idx, offset, length, produced_at_ms) VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(attempt.id)
    .bind(highest + 1)
    .bind(chunk.step)
    .bind(end)
    .bind(data.len() as i64)
    .bind(chunk.produced_at_ms)
    .execute(&mut *tx)
    .await?;
    Ok(Some((chunk.sequence, truncated)))
}

/// Record a step's state (`running`, `succeeded`, `failed`, `skipped`); false when the
/// attempt is not this runner's current, running, unexpired one.
pub async fn step(
    tx: &mut SqliteConnection,
    runner_id: i64,
    attempt: &str,
    (idx, name, state, exit_code): (u32, &str, &str, Option<i32>),
    now: i64,
) -> Result<bool, sqlx::Error> {
    let Some(attempt) = current(tx, runner_id, attempt).await? else {
        return Ok(false);
    };
    if attempt.state != "running" || attempt.lease_expires_at < now {
        return Ok(false);
    }
    let running = state == "running";
    sqlx::query(
        "INSERT INTO step (attempt_id, idx, name, state, exit_code, started_at, finished_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT (attempt_id, idx) DO UPDATE SET name = excluded.name, state = excluded.state,
           exit_code = excluded.exit_code, started_at = COALESCE(step.started_at, excluded.started_at),
           finished_at = excluded.finished_at",
    )
    .bind(attempt.id)
    .bind(idx)
    .bind(name)
    .bind(state)
    .bind(exit_code)
    .bind(running.then_some(now))
    .bind((!running).then_some(now))
    .execute(&mut *tx)
    .await?;
    Ok(true)
}

/// An attempt's terminal result.
pub struct Finish<'a> {
    /// `succeeded`, `failed` or `cancelled`.
    pub state: AttemptState,
    pub exit_code: Option<i32>,
    /// Empty when unknown (0008: known only structurally).
    pub check: &'a str,
    pub reason: &'a str,
}

/// Record the attempt's first terminal result in its job and run, with `run-finished`.
/// A result for an attempt no longer running (already finished, cancelled, lost) is
/// acknowledged and ignored; a result for an expired running attempt receives cancel.
/// A result before acceptance starts the attempt first.
pub async fn finish(
    tx: &mut SqliteConnection,
    runner: (i64, &str),
    attempt: &str,
    result: &Finish<'_>,
    correlation_id: &str,
    now: i64,
) -> Result<Outcome, sqlx::Error> {
    let Some(mut current) = current(tx, runner.0, attempt).await? else {
        return Ok(Outcome::Cancel);
    };
    let Some(Ok(state)) = AttemptState::parse(&current.state)
        .map(|from| super::runs::attempt_transition(from, result.state))
    else {
        return Ok(Outcome::Done(Vec::new()));
    };
    if current.lease_expires_at < now {
        return Ok(Outcome::Cancel);
    }
    let mut changes = start(tx, &mut current, runner, correlation_id, now).await?;
    sqlx::query("UPDATE job_attempt SET state = ?, exit_code = ?, finished_at = ? WHERE id = ?")
        .bind(state.as_str())
        .bind(result.exit_code)
        .bind(now)
        .bind(current.id)
        .execute(&mut *tx)
        .await?;
    let succeeded = state == AttemptState::Succeeded;
    let failure = |text: &str| (!succeeded && !text.is_empty()).then(|| text.to_owned());
    changes.extend(
        end(
            tx,
            &current,
            (
                state.as_str(),
                failure(result.check),
                failure(result.reason),
            ),
            Some(runner.0),
            correlation_id,
            now,
        )
        .await?,
    );
    Ok(Outcome::Done(changes))
}

/// End the attempt's job and run in `state` (`succeeded`, `failed`, `cancelled`, or
/// `lost` for the job alone), journaling `run-finished`. Unfinished states only.
// shortcut: a run has one job (0105), so the run ends with it; several jobs need a rule.
pub(crate) async fn end(
    tx: &mut SqliteConnection,
    attempt: &Current,
    (state, check, reason): (&str, Option<String>, Option<String>),
    runner: Option<i64>,
    correlation_id: &str,
    now: i64,
) -> Result<Vec<(String, RunState)>, sqlx::Error> {
    let job = JobState::parse(&attempt.job_state).zip(JobState::parse(state));
    if let Some((_, to)) = job.filter(|(from, to)| job_transition(*from, *to).is_ok()) {
        sqlx::query("UPDATE job SET state = ?, finished_at = ? WHERE id = ?")
            .bind(to.as_str())
            .bind(now)
            .bind(attempt.job_id)
            .execute(&mut *tx)
            .await?;
    }
    // A lost job fails its run.
    let run_state = if state == "lost" { "failed" } else { state };
    let (Some(from), Some(to)) = (
        RunState::parse(&attempt.run_state),
        RunState::parse(run_state),
    ) else {
        return Ok(Vec::new());
    };
    if run_transition(from, to).is_err() {
        return Ok(Vec::new());
    }
    sqlx::query(
        "UPDATE run SET state = ?, finished_at = ?, failure_check = ?, failure_reason = ? WHERE id = ?",
    )
    .bind(to.as_str())
    .bind(now)
    .bind(&check)
    .bind(&reason)
    .bind(attempt.run_id)
    .execute(&mut *tx)
    .await?;
    journal(
        tx,
        "run-finished",
        runner,
        attempt.repo_id,
        correlation_id,
        serde_json::json!({ "run": attempt.run, "state": to.as_str(), "check": check, "reason": reason }),
        now,
    )
    .await?;
    Ok(vec![(attempt.run.clone(), to)])
}

/// Turn running attempts whose lease passed (`lease_expires_at < now`) into `lost`
/// (`attempt-lost`). A replayable job below `max_attempts` goes back to `pending` for a
/// new attempt (`attempt-replayed`); otherwise the job and its run fail with reason
/// `lease-expired`. Returns the runs that changed.
pub async fn expire(
    tx: &mut SqliteConnection,
    now: i64,
) -> Result<Vec<(String, RunState)>, sqlx::Error> {
    let lost: Vec<Current> = sqlx::query_as(&format!(
        "{ATTEMPT} WHERE a.state = 'running' AND a.lease_expires_at < ? ORDER BY a.id"
    ))
    .bind(now)
    .fetch_all(&mut *tx)
    .await?;
    lose(tx, lost, now).await
}

/// At a runner's admission, its running attempts that its hello does not list (`held`)
/// died with an earlier process: they are lost and replayed as by [`expire`], at once
/// rather than once heartbeats of the new process stop renewing them (0008).
pub async fn abandon(
    tx: &mut SqliteConnection,
    runner_id: i64,
    held: &[String],
    now: i64,
) -> Result<Vec<(String, RunState)>, sqlx::Error> {
    let running: Vec<Current> = sqlx::query_as(&format!(
        "{ATTEMPT} WHERE a.state = 'running' AND a.runner_id = ? ORDER BY a.id"
    ))
    .bind(runner_id)
    .fetch_all(&mut *tx)
    .await?;
    let lost = running
        .into_iter()
        .filter(|attempt| !held.contains(&attempt.public_id))
        .collect();
    lose(tx, lost, now).await
}

async fn lose(
    tx: &mut SqliteConnection,
    lost: Vec<Current>,
    now: i64,
) -> Result<Vec<(String, RunState)>, sqlx::Error> {
    let mut changes = Vec::new();
    for attempt in lost {
        sqlx::query("UPDATE job_attempt SET state = 'lost', finished_at = ? WHERE id = ?")
            .bind(now)
            .bind(attempt.id)
            .execute(&mut *tx)
            .await?;
        // The lost attempt's id ties its events together.
        let correlation_id = attempt.public_id.as_str();
        journal(
            tx,
            "attempt-lost",
            None,
            attempt.repo_id,
            correlation_id,
            serde_json::json!({ "run": attempt.run, "attempt": attempt.public_id, "runner": attempt.runner }),
            now,
        )
        .await?;
        tracing::warn!(
            event = "attempt-lost",
            run = attempt.run,
            attempt = attempt.public_id,
            runner = attempt.runner
        );
        let unfinished = matches!(
            JobState::parse(&attempt.job_state),
            Some(JobState::Assigned | JobState::Running)
        );
        if !unfinished {
            continue;
        }
        if attempt.replayable && attempt.number < attempt.max_attempts {
            sqlx::query("UPDATE job SET state = 'pending' WHERE id = ?")
                .bind(attempt.job_id)
                .execute(&mut *tx)
                .await?;
            journal(
                tx,
                "attempt-replayed",
                None,
                attempt.repo_id,
                correlation_id,
                serde_json::json!({ "run": attempt.run, "job": attempt.job, "attempt": attempt.public_id }),
                now,
            )
            .await?;
            continue;
        }
        let state = if attempt.replayable { "failed" } else { "lost" };
        let reason = Some("lease-expired".to_owned());
        changes.extend(
            end(
                tx,
                &attempt,
                (state, None, reason),
                None,
                correlation_id,
                now,
            )
            .await?,
        );
    }
    Ok(changes)
}

/// Answer a reconnected runner's in-flight attempts (0008): `Some(highest acknowledged
/// log sequence)` to continue an attempt still current, running and leased (its lease is
/// renewed), `None` to cancel it.
pub async fn resume(
    tx: &mut SqliteConnection,
    runner_id: i64,
    attempts: &[String],
    now: i64,
) -> Result<Vec<Option<u64>>, sqlx::Error> {
    let mut answers = Vec::new();
    for attempt in attempts {
        let live = current(tx, runner_id, attempt)
            .await?
            .filter(|attempt| attempt.state == "running" && attempt.lease_expires_at >= now);
        let Some(live) = live else {
            answers.push(None);
            continue;
        };
        sqlx::query("UPDATE job_attempt SET lease_expires_at = ? WHERE id = ?")
            .bind(now + LEASE_MS)
            .bind(live.id)
            .execute(&mut *tx)
            .await?;
        let highest: i64 =
            sqlx::query_scalar("SELECT COALESCE(max(seq), 0) FROM log_chunk WHERE attempt_id = ?")
                .bind(live.id)
                .fetch_one(&mut *tx)
                .await?;
        answers.push(Some(highest as u64));
    }
    Ok(answers)
}
