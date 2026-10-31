//! `RunService` (0008, 0010, 0013, 0020): manual trigger and run reads.
// These feed `?` in tonic handlers, which return `Status` by value anyway.
#![allow(clippy::result_large_err)]

use super::{
    Health, cursor,
    local::refused,
    repos::{caller, find_readable, internal},
};
use kajiba_core::config::{Backend, Role};
use kajiba_gitstore::{GitStore, Gix};
use kajiba_proto::{
    google::protobuf::Timestamp,
    v1::{
        Attempt, AttemptState, CreateWorktreeRunRequest, CreateWorktreeRunResponse, FailureSummary,
        GetRunRequest, GetRunResponse, GetWorktreeBaseRequest, GetWorktreeBaseResponse, Job,
        JobState, ListRunsRequest, ListRunsResponse, LogBytes, LogSection, Run, RunState,
        RunTrigger, TriggerRunRequest, TriggerRunResponse, WatchRunRequest, WatchRunResponse,
        WorktreeRunSpec, create_worktree_run_request::Part, run_service_server::RunService,
        watch_run_response,
    },
};
use sqlx::SqliteConnection;
use std::{
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::sync::{mpsc, watch};
use tonic::{Request, Response, Status};

/// Lines of the failure summary (0112), read from at most this many trailing log bytes.
pub const SUMMARY_LINES: usize = 20;
const SUMMARY_BYTES: u64 = 64 * 1024;

#[derive(Clone)]
pub struct Runs(pub Health);

type Row = (
    i64,
    String,
    String,
    String,
    Option<String>,
    String,
    Option<String>,
    String,
    i64,
    Option<i64>,
    Option<i64>,
    Option<String>,
    Option<String>,
    Option<String>,
);

const SELECT: &str = "SELECT run.id, run.public_id, repo.public_id, run.sha, run.ref, run.trigger,
        user.name, run.state, run.created_at, run.started_at, run.finished_at,
        run.failure_check, run.failure_reason, run.checks
    FROM run JOIN repo ON repo.id = run.repo_id LEFT JOIN user ON user.id = run.triggered_by";

fn time(ms: Option<i64>) -> Option<Timestamp> {
    ms.map(|ms| Timestamp {
        seconds: ms.div_euclid(1000),
        nanos: (ms.rem_euclid(1000) * 1_000_000) as i32,
    })
}

fn run_not_found() -> Status {
    refused(
        tonic::Code::NotFound,
        "RUN_NOT_FOUND",
        "no run by this id".into(),
    )
}

/// The last lines of an attempt's stored log: the bytes its index covers, never past.
fn last_lines(path: &Path, end: u64) -> Vec<String> {
    let start = end.saturating_sub(SUMMARY_BYTES);
    let mut bytes = Vec::new();
    let read = std::fs::File::open(path).and_then(|mut file| {
        std::io::Seek::seek(&mut file, std::io::SeekFrom::Start(start))?;
        file.take(end - start).read_to_end(&mut bytes)
    });
    if read.is_err() {
        // Deleted by retention: the summary keeps its reason.
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&bytes);
    let mut lines: Vec<&str> = text.lines().collect();
    if start > 0 && !lines.is_empty() {
        // Cut mid-line.
        lines.remove(0);
    }
    let from = lines.len().saturating_sub(SUMMARY_LINES);
    lines[from..]
        .iter()
        .map(|line| (*line).to_owned())
        .collect()
}

impl Runs {
    fn log(&self, attempt: &str) -> std::path::PathBuf {
        self.0.state_dir.join(format!("ci/logs/{attempt}.log"))
    }

    fn readers(&self) -> Result<&sqlx::SqlitePool, Status> {
        self.0
            .database
            .get()
            .ok_or_else(|| Status::unavailable("starting"))
    }

    /// The run with its job, current attempt and, when failed, its summary.
    async fn message(&self, connection: &mut SqliteConnection, row: Row) -> Result<Run, Status> {
        let (id, public_id, repo, sha, reference, trigger, triggered_by, state, created, started) = (
            row.0, row.1, row.2, row.3, row.4, row.5, row.6, row.7, row.8, row.9,
        );
        let (finished, check, reason, checks) = (row.10, row.11, row.12, row.13);
        let job: Option<(i64, String, String, String, Option<String>)> = sqlx::query_as(
            "SELECT id, public_id, name, state, pending_reason FROM job WHERE run_id = ? ORDER BY id LIMIT 1",
        )
        .bind(id)
        .fetch_optional(&mut *connection)
        .await
        .map_err(internal)?;
        let mut current = None;
        let job = match job {
            Some((job_id, public_id, name, job_state, pending_reason)) => {
                let current_attempt =
                    attempts(connection, job_id)
                        .await?
                        .pop()
                        .map(|(attempt, end, _)| {
                            current = Some((attempt.public_id.clone(), end));
                            attempt
                        });
                Some(Job {
                    public_id,
                    name,
                    state: job_state_of(&job_state).into(),
                    pending_reason: pending_reason.unwrap_or_default(),
                    current_attempt,
                })
            }
            None => None,
        };
        let failure = (state == "failed").then(|| FailureSummary {
            check: check.unwrap_or_default(),
            reason: reason.unwrap_or_default(),
            last_log_lines: current
                .map(|(attempt, end)| last_lines(&self.log(&attempt), end))
                .unwrap_or_default(),
        });
        Ok(Run {
            public_id,
            repo_public_id: repo,
            sha,
            r#ref: reference.unwrap_or_default(),
            trigger: match trigger.as_str() {
                "push" => RunTrigger::Push,
                "manual" => RunTrigger::Manual,
                "worktree" => RunTrigger::Worktree,
                _ => RunTrigger::Unspecified,
            }
            .into(),
            triggered_by: triggered_by.unwrap_or_default(),
            state: match state.as_str() {
                "queued" => RunState::Queued,
                "running" => RunState::Running,
                "succeeded" => RunState::Succeeded,
                "failed" => RunState::Failed,
                "cancelled" => RunState::Cancelled,
                _ => RunState::Unspecified,
            }
            .into(),
            created_at: time(Some(created)),
            started_at: time(started),
            finished_at: time(finished),
            job,
            failure,
            checks: checks
                .and_then(|checks| serde_json::from_str(&checks).ok())
                .unwrap_or_default(),
        })
    }
}

type AttemptRow = (
    String,
    i64,
    String,
    String,
    Option<String>,
    Option<i32>,
    i64,
    Option<i64>,
    Option<i64>,
    i64,
);

/// A job's attempts in order, with original log lengths and availability.
async fn attempts(
    connection: &mut SqliteConnection,
    job_id: i64,
) -> Result<Vec<(Attempt, u64, bool)>, Status> {
    let rows: Vec<AttemptRow> = sqlx::query_as(
        "SELECT job_attempt.public_id, number, state, runner.name, runner.hostname, exit_code,
                started_at, finished_at,
                (SELECT MAX(offset + length) FROM log_chunk WHERE attempt_id = job_attempt.id),
                purged_log_length
         FROM job_attempt JOIN runner ON runner.id = runner_id
         WHERE job_id = ? ORDER BY number",
    )
    .bind(job_id)
    .fetch_all(&mut *connection)
    .await
    .map_err(internal)?;
    Ok(rows
        .into_iter()
        .map(|row| {
            let attempt = Attempt {
                public_id: row.0,
                number: row.1 as u32,
                state: attempt_state(&row.2).into(),
                runner: row.3,
                runner_hostname: row.4.unwrap_or_default(),
                exit_code: row.5,
                started_at: time(Some(row.6)),
                finished_at: time(row.7),
            };
            (
                attempt,
                row.8.unwrap_or(row.9) as u64,
                row.8.is_some() || row.9 == 0,
            )
        })
        .collect())
}

fn attempt_state(state: &str) -> AttemptState {
    match state {
        "running" => AttemptState::Running,
        "succeeded" => AttemptState::Succeeded,
        "failed" => AttemptState::Failed,
        "cancelled" => AttemptState::Cancelled,
        "lost" => AttemptState::Lost,
        _ => AttemptState::Unspecified,
    }
}

fn job_state_of(state: &str) -> JobState {
    match state {
        "pending" => JobState::Pending,
        "assigned" => JobState::Assigned,
        "running" => JobState::Running,
        "succeeded" => JobState::Succeeded,
        "failed" => JobState::Failed,
        "cancelled" => JobState::Cancelled,
        "lost" => JobState::Lost,
        _ => JobState::Unspecified,
    }
}

/// A run of a repository the caller can read: RUN_NOT_FOUND for an unknown id,
/// REPO_NOT_FOUND when its repository is not readable (0020).
async fn readable_run(
    connection: &mut SqliteConnection,
    user: i64,
    run: &str,
) -> Result<Row, Status> {
    let row: Row = sqlx::query_as(&format!("{SELECT} WHERE run.public_id = ?"))
        .bind(run)
        .fetch_optional(&mut *connection)
        .await
        .map_err(internal)?
        .ok_or_else(run_not_found)?;
    let repo_id: i64 = sqlx::query_scalar("SELECT repo_id FROM run WHERE id = ?")
        .bind(row.0)
        .fetch_one(&mut *connection)
        .await
        .map_err(internal)?;
    crate::db::permissions::role(&mut *connection, user, repo_id)
        .await
        .map_err(internal)?
        .ok_or_else(super::repos::not_found)?;
    Ok(row)
}

/// ADR 0097: at most this many non-terminal worktree runs per user.
pub const WORKTREE_RUNS: i64 = 4;

/// A worktree run that passed the creation checks (ADR 0118).
#[derive(Debug)]
pub struct Admitted {
    pub repo_id: i64,
    pub repo: String,
    pub arch: String,
    pub os: String,
}

/// `[A-Za-z0-9_-]+` (0097).
fn check_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}

/// `<arch>-<os>`, each half `[a-z0-9_]+` (ADR 0118).
fn system(name: &str) -> Option<(&str, &str)> {
    let half = |text: &str| {
        !text.is_empty()
            && text
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    };
    name.split_once('-')
        .filter(|(arch, os)| half(arch) && half(os))
}

impl Runs {
    /// ADR 0114 and 0118: not archived, then the `write` role, then CI enabled.
    async fn may_run(&self, user: i64, repo_id: i64) -> Result<(), Status> {
        let (archived, ci_enabled): (bool, bool) =
            sqlx::query_as("SELECT archived_at IS NOT NULL, ci_enabled FROM repo WHERE id = ?")
                .bind(repo_id)
                .fetch_one(self.readers()?)
                .await
                .map_err(internal)?;
        if archived {
            return Err(refused(
                tonic::Code::FailedPrecondition,
                "INVALID_STATE",
                "the repository is archived".into(),
            ));
        }
        let role = crate::db::permissions::role(self.readers()?, user, repo_id)
            .await
            .map_err(internal)?;
        if role.is_none_or(|role| role < Role::Write) {
            return Err(refused(
                tonic::Code::PermissionDenied,
                "ROLE_MISSING",
                "triggering a run needs the write role".into(),
            ));
        }
        if !ci_enabled {
            return Err(refused(
                tonic::Code::FailedPrecondition,
                "CI_DISABLED",
                "CI is disabled on this repository".into(),
            ));
        }
        Ok(())
    }

    /// The checks of a worktree run's creation, in ADR 0118's order, shared by
    /// `GetWorktreeBase` and `CreateWorktreeRun`; the scope is checked by the auth layer.
    pub async fn admit(&self, user: i64, spec: &WorktreeRunSpec) -> Result<Admitted, Status> {
        let declared = self
            .0
            .runners
            .get()
            .ok_or_else(|| Status::unavailable("starting"))?;
        let (repo_id, repo) = find_readable(self.readers()?, user, &spec.repo).await?;
        self.may_run(user, repo_id).await?;
        if !spec.checks.iter().all(|name| check_name(name)) {
            return Err(cursor::invalid_field(
                "spec.checks",
                "a check name is [A-Za-z0-9_-]+",
            ));
        }
        let (arch, os) = system(&spec.system)
            .ok_or_else(|| cursor::invalid_field("spec.system", "a system is <arch>-<os>"))?;
        for feature in &spec.features {
            kajiba_core::config::validate_tag(feature)
                .map_err(|err| cursor::invalid_field("spec.features", err))?;
        }
        let matches = declared.runners.values().any(|runner| {
            let capabilities = &runner.capabilities;
            // Compared by their configuration names, as admission does.
            serde_json::to_value(capabilities.arch).is_ok_and(|name| name == arch)
                && serde_json::to_value(capabilities.os).is_ok_and(|name| name == os)
                && capabilities.backends.contains(&Backend::Nix)
                && spec
                    .features
                    .iter()
                    .all(|feature| capabilities.tags.contains(feature))
        });
        if !matches {
            return Err(refused(
                tonic::Code::FailedPrecondition,
                "NO_MATCHING_RUNNER",
                format!(
                    "no declared runner can run {} with these features",
                    spec.system
                ),
            ));
        }
        active_worktree_runs(self.readers()?, user).await?;
        Ok(Admitted {
            repo_id,
            repo,
            arch: arch.into(),
            os: os.into(),
        })
    }
}

/// WORKTREE_RUNS_LIMIT when `user` already has [`WORKTREE_RUNS`] non-terminal worktree runs.
pub async fn active_worktree_runs<'e>(
    executor: impl sqlx::SqliteExecutor<'e>,
    user: i64,
) -> Result<(), Status> {
    let active: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM run WHERE trigger = 'worktree' AND triggered_by = ?
           AND state IN ('queued', 'running')",
    )
    .bind(user)
    .fetch_one(executor)
    .await
    .map_err(internal)?;
    if active >= WORKTREE_RUNS {
        return Err(refused(
            tonic::Code::ResourceExhausted,
            "WORKTREE_RUNS_LIMIT",
            format!("at most {WORKTREE_RUNS} worktree runs may be active at once"),
        ));
    }
    Ok(())
}

/// ADR 0097: a worktree run's bundle holds at most 64 MiB.
pub const MAX_BUNDLE: u64 = 64 << 20;

/// An upload under `tmp/`, removed when dropped unless it was moved away.
struct Upload(Option<PathBuf>);

impl Drop for Upload {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn bundle_invalid() -> Status {
    refused(
        tonic::Code::InvalidArgument,
        "BUNDLE_INVALID",
        "not a git bundle with exactly one head".into(),
    )
}

/// The prerequisites and the single head of a v2 bundle, or a v3 one in SHA-1, read
/// from its header, and the header's length; a pack must follow.
pub(super) fn bundle_header(path: &Path) -> Result<(Vec<String>, String, u64), Status> {
    use std::io::BufRead;
    let file =
        std::fs::File::open(path).map_err(|err| Status::internal(format!("bundle: {err}")))?;
    let mut reader = std::io::BufReader::new(file);
    let mut length = 0;
    let mut next = |reader: &mut std::io::BufReader<std::fs::File>| {
        let mut line = Vec::new();
        match reader.read_until(b'\n', &mut line) {
            Ok(read) if line.pop() == Some(b'\n') => {
                length += read as u64;
                String::from_utf8(line).map_err(|_| bundle_invalid())
            }
            _ => Err(bundle_invalid()),
        }
    };
    let v3 = match next(&mut reader)?.as_str() {
        "# v2 git bundle" => false,
        "# v3 git bundle" => true,
        _ => return Err(bundle_invalid()),
    };
    let id = |text: &str| {
        (text.len() == 40 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
            .then(|| text.to_owned())
            .ok_or_else(bundle_invalid)
    };
    let (mut prerequisites, mut heads) = (Vec::new(), Vec::new());
    loop {
        let line = next(&mut reader)?;
        if line.is_empty() {
            break;
        }
        if let Some(capability) = line.strip_prefix('@') {
            if !v3 || capability != "object-format=sha1" {
                return Err(bundle_invalid());
            }
        } else if let Some(rest) = line.strip_prefix('-') {
            // `-<id>[ <comment>]`
            prerequisites.push(id(rest.split(' ').next().unwrap_or_default())?);
        } else {
            let (head, name) = line.split_once(' ').ok_or_else(bundle_invalid)?;
            if name.is_empty() {
                return Err(bundle_invalid());
            }
            heads.push(id(head)?);
        }
    }
    let mut magic = [0; 4];
    std::io::Read::read_exact(&mut reader, &mut magic).map_err(|_| bundle_invalid())?;
    match (heads.pop(), heads.is_empty(), &magic) {
        (Some(head), true, b"PACK") => Ok((prerequisites, head, length)),
        _ => Err(bundle_invalid()),
    }
}

type Watcher = mpsc::Sender<Result<WatchRunResponse, Status>>;

/// Watchers re-read the run this often even without a progress signal.
const WATCH_PERIOD: Duration = Duration::from_secs(1);

impl Runs {
    /// Stream the run's log from `position`, then its terminal state; returns when the
    /// stream ends or the watcher is gone (`Ok`), or on an error to send.
    async fn watch(
        &self,
        run: &str,
        mut position: u64,
        follow: bool,
        mut progress: watch::Receiver<()>,
        watcher: &Watcher,
    ) -> Result<(), Status> {
        let send = async |event| {
            watcher
                .send(Ok(WatchRunResponse { event: Some(event) }))
                .await
                .is_ok()
        };
        let mut marked: Option<String> = None;
        loop {
            // Marked seen before the snapshot: a commit after it wakes the next wait.
            progress.borrow_and_update();
            let mut connection = self.readers()?.begin().await.map_err(internal)?;
            let row: Row = sqlx::query_as(&format!("{SELECT} WHERE run.public_id = ?"))
                .bind(run)
                .fetch_optional(&mut *connection)
                .await
                .map_err(internal)?
                .ok_or_else(run_not_found)?;
            let job: Option<i64> =
                sqlx::query_scalar("SELECT id FROM job WHERE run_id = ? ORDER BY id LIMIT 1")
                    .bind(row.0)
                    .fetch_optional(&mut *connection)
                    .await
                    .map_err(internal)?;
            let sections = match job {
                Some(job) => attempts(&mut connection, job).await?,
                None => Vec::new(),
            };
            // A terminal run's attempts have all ended: its log is complete in this snapshot.
            let finished = match row.7.as_str() {
                "succeeded" | "failed" | "cancelled" => {
                    Some(self.message(&mut connection, row).await?)
                }
                _ => None,
            };
            drop(connection);
            let mut start = 0;
            for (attempt, length, available) in sections {
                let stop = start + length;
                if start < position && stop <= position {
                    start = stop;
                    continue;
                }
                let path = self.log(&attempt.public_id);
                if marked.as_deref() != Some(attempt.public_id.as_str()) {
                    marked = Some(attempt.public_id.clone());
                    let section = LogSection {
                        attempt: Some(attempt),
                        offset: start,
                    };
                    if !send(watch_run_response::Event::Section(section)).await {
                        return Ok(());
                    }
                }
                if !available {
                    position = position.max(stop);
                }
                while position < stop {
                    let data = read_at(&path, position - start, (stop - position).min(LOG_PIECE))
                        .map_err(|err| Status::internal(format!("log: {err}")))?;
                    let log = LogBytes {
                        offset: position,
                        data,
                    };
                    position += log.data.len() as u64;
                    if !send(watch_run_response::Event::Log(log)).await {
                        return Ok(());
                    }
                }
                start = stop;
            }
            if let Some(run) = finished {
                send(watch_run_response::Event::Finished(run)).await;
                return Ok(());
            }
            if !follow {
                return Ok(());
            }
            tokio::select! {
                _ = progress.changed() => {}
                () = tokio::time::sleep(WATCH_PERIOD) => {}
                () = watcher.closed() => return Ok(()),
            }
        }
    }
}

/// Bytes sent per log message, as a runner's chunk (0105).
const LOG_PIECE: u64 = 64 * 1024;

/// `length` bytes at `offset` of a log file, all of them indexed hence present.
fn read_at(path: &Path, offset: u64, length: u64) -> std::io::Result<Vec<u8>> {
    let mut file = std::fs::File::open(path)?;
    std::io::Seek::seek(&mut file, std::io::SeekFrom::Start(offset))?;
    let mut data = vec![0; length as usize];
    file.read_exact(&mut data)?;
    Ok(data)
}

#[tonic::async_trait]
impl RunService for Runs {
    type WatchRunStream = std::pin::Pin<
        Box<dyn tokio_stream::Stream<Item = Result<WatchRunResponse, Status>> + Send>,
    >;

    async fn watch_run(
        &self,
        request: Request<WatchRunRequest>,
    ) -> Result<Response<Self::WatchRunStream>, Status> {
        let user = caller(&request)?;
        let request = request.into_inner();
        let mut connection = self.readers()?.begin().await.map_err(internal)?;
        let run = readable_run(&mut connection, user, &request.run).await?.1;
        drop(connection);
        // Subscribed before the first snapshot: nothing stored after it is missed.
        let progress = self.0.ci_progress.subscribe();
        let (watcher, events) = mpsc::channel(4);
        let runs = self.clone();
        tokio::spawn(async move {
            if let Err(status) = runs
                .watch(&run, request.offset, request.follow, progress, &watcher)
                .await
            {
                let _ = watcher.send(Err(status)).await;
            }
        });
        Ok(Response::new(Box::pin(
            tokio_stream::wrappers::ReceiverStream::new(events),
        )))
    }

    async fn trigger_run(
        &self,
        request: Request<TriggerRunRequest>,
    ) -> Result<Response<TriggerRunResponse>, Status> {
        let user = caller(&request)?;
        let correlation_id = request
            .extensions()
            .get::<super::correlation::RequestId>()
            .map_or_else(String::new, |id| id.0.clone());
        let request = request.into_inner();
        let (writer, declared) = match (self.0.writer.get(), self.0.runners.get()) {
            (Some(writer), Some(declared)) => (writer, declared),
            _ => return Err(Status::unavailable("starting")),
        };
        let (repo_id, repo) = find_readable(self.readers()?, user, &request.repo).await?;
        self.may_run(user, repo_id).await?;
        let root = self.0.git_state_dir.join("repos");
        let (dir, sha) = (root.join(format!("{repo}.git")), request.sha.clone());
        let reachable =
            tokio::task::spawn_blocking(move || Gix::shared(&root).reachable(&dir, &sha))
                .await
                .map_err(|err| Status::internal(format!("git: {err}")))?
                .map_err(super::repos::git_error("REVISION_NOT_FOUND"))?;
        if !reachable {
            return Err(refused(
                tonic::Code::NotFound,
                "REVISION_NOT_FOUND",
                "no commit by this id is reachable from a ref".into(),
            ));
        }
        let mut writer = writer.lock().await;
        let mut tx = sqlx::Connection::begin(&mut *writer)
            .await
            .map_err(internal)?;
        let public_id = crate::db::runs::create_run(
            &mut tx,
            &crate::db::runs::NewRun {
                repo_id,
                sha: &request.sha,
                reference: None,
                trigger: "manual",
                event_id: None,
                triggered_by: Some(user),
                correlation_id: &correlation_id,
                now: crate::db::runs::now(),
                worktree: None,
            },
        )
        .await
        .map_err(internal)?;
        let row: Row = sqlx::query_as(&format!("{SELECT} WHERE run.public_id = ?"))
            .bind(&public_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(internal)?;
        let run = self.message(&mut tx, row).await?;
        declared.bus.commit(tx).await.map_err(internal)?;
        drop(writer);
        tracing::info!(event = "run-state", run = public_id, state = "queued");
        declared.wake.notify_one();
        Ok(Response::new(TriggerRunResponse { run: Some(run) }))
    }

    async fn get_worktree_base(
        &self,
        request: Request<GetWorktreeBaseRequest>,
    ) -> Result<Response<GetWorktreeBaseResponse>, Status> {
        let user = caller(&request)?;
        let spec = request.into_inner().spec.unwrap_or_default();
        let admitted = self.admit(user, &spec).await?;
        let root = self.0.git_state_dir.join("repos");
        let dir = root.join(format!("{}.git", admitted.repo));
        let refs = tokio::task::spawn_blocking(move || Gix::shared(&root).refs(&dir))
            .await
            .map_err(|err| Status::internal(format!("git: {err}")))?
            .map_err(super::repos::git_error("REPO_NOT_FOUND"))?;
        let ids: std::collections::BTreeSet<String> = refs
            .into_iter()
            .filter(|r| r.name.starts_with("refs/heads/") || r.name.starts_with("refs/tags/"))
            .map(|r| r.target)
            .collect();
        Ok(Response::new(GetWorktreeBaseResponse {
            ids: ids.into_iter().collect(),
        }))
    }

    async fn create_worktree_run(
        &self,
        request: Request<tonic::Streaming<CreateWorktreeRunRequest>>,
    ) -> Result<Response<CreateWorktreeRunResponse>, Status> {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let user = caller(&request)?;
        let correlation_id = request
            .extensions()
            .get::<super::correlation::RequestId>()
            .map_or_else(String::new, |id| id.0.clone());
        let mut parts = request.into_inner();
        let out_of_order = || cursor::invalid_field("part", "the spec first, then bundle chunks");
        let spec = match parts.message().await?.and_then(|message| message.part) {
            Some(Part::Spec(spec)) => spec,
            _ => return Err(out_of_order()),
        };
        // Every check before a byte of the bundle is stored (0097).
        let admitted = self.admit(user, &spec).await?;
        let (writer, declared) = match (self.0.writer.get(), self.0.runners.get()) {
            (Some(writer), Some(declared)) => (writer, declared),
            _ => return Err(Status::unavailable("starting")),
        };
        let io = |err: std::io::Error| Status::internal(format!("bundle: {err}"));
        // tmp/ is emptied at startup (0032): an interrupted upload leaves nothing.
        let path = self.0.state_dir.join(format!(
            "tmp/worktree-{}.bundle",
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .await
            .map_err(io)?;
        let mut upload = Upload(Some(path.clone()));
        let mut size = 0;
        while let Some(message) = parts.message().await? {
            let Some(Part::Chunk(chunk)) = message.part else {
                return Err(out_of_order());
            };
            size += chunk.len() as u64;
            if size > MAX_BUNDLE {
                return Err(refused(
                    tonic::Code::ResourceExhausted,
                    "BUNDLE_TOO_LARGE",
                    format!("a worktree bundle holds at most {MAX_BUNDLE} bytes"),
                ));
            }
            tokio::io::AsyncWriteExt::write_all(&mut file, &chunk)
                .await
                .map_err(io)?;
        }
        file.sync_all().await.map_err(io)?;
        drop(file);
        let header = path.clone();
        let (prerequisites, head, _) = tokio::task::spawn_blocking(move || bundle_header(&header))
            .await
            .map_err(|err| Status::internal(format!("bundle: {err}")))??;
        let root = self.0.git_state_dir.join("repos");
        let dir = root.join(format!("{}.git", admitted.repo));
        let reached = tokio::task::spawn_blocking(move || {
            Gix::shared(&root).tips_reach(&dir, &prerequisites)
        })
        .await
        .map_err(|err| Status::internal(format!("git: {err}")))?
        .map_err(super::repos::git_error("REPO_NOT_FOUND"))?;
        if !reached {
            return Err(refused(
                tonic::Code::FailedPrecondition,
                "BUNDLE_PREREQUISITE_MISSING",
                "a prerequisite of the bundle is on no branch or tag of the repository".into(),
            ));
        }
        let mut writer = writer.lock().await;
        let mut tx = sqlx::Connection::begin(&mut *writer)
            .await
            .map_err(internal)?;
        // Again under the write lock: concurrent uploads cannot pass the limit together.
        active_worktree_runs(&mut *tx, user).await?;
        let public_id = crate::db::runs::create_run(
            &mut tx,
            &crate::db::runs::NewRun {
                repo_id: admitted.repo_id,
                sha: &head,
                reference: None,
                trigger: "worktree",
                event_id: None,
                triggered_by: Some(user),
                correlation_id: &correlation_id,
                now: crate::db::runs::now(),
                worktree: Some(crate::db::runs::Worktree {
                    checks: &spec.checks,
                    arch: &admitted.arch,
                    os: &admitted.os,
                    features: &spec.features,
                }),
            },
        )
        .await
        .map_err(internal)?;
        let row: Row = sqlx::query_as(&format!("{SELECT} WHERE run.public_id = ?"))
            .bind(&public_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(internal)?;
        let run = self.message(&mut tx, row).await?;
        // Moved before the commit, so a committed run always has its bundle (0097).
        let bundles = self.0.state_dir.join("ci/bundles");
        let bundle = bundles.join(format!("{public_id}.bundle"));
        std::fs::rename(&path, &bundle).map_err(io)?;
        upload.0 = Some(bundle);
        kajiba_core::state::sync_dir(&bundles).map_err(io)?;
        declared.bus.commit(tx).await.map_err(internal)?;
        upload.0 = None;
        drop(writer);
        tracing::info!(event = "run-state", run = public_id, state = "queued");
        declared.wake.notify_one();
        Ok(Response::new(CreateWorktreeRunResponse { run: Some(run) }))
    }

    async fn get_run(
        &self,
        request: Request<GetRunRequest>,
    ) -> Result<Response<GetRunResponse>, Status> {
        let user = caller(&request)?;
        let mut connection = self.readers()?.begin().await.map_err(internal)?;
        let row = readable_run(&mut connection, user, &request.get_ref().run).await?;
        let run = self.message(&mut connection, row).await?;
        Ok(Response::new(GetRunResponse { run: Some(run) }))
    }

    async fn list_runs(
        &self,
        request: Request<ListRunsRequest>,
    ) -> Result<Response<ListRunsResponse>, Status> {
        const METHOD: &str = "v1.ListRuns";
        let user = caller(&request)?;
        let request = request.into_inner();
        let size = cursor::page_size(request.page_size)?;
        let (repo_id, repo) = find_readable(self.readers()?, user, &request.repo).await?;
        // The repository is part of the cursor: a cursor of another repository is foreign.
        let after = match cursor::decode(METHOD, &request.page_token)? {
            Some((key, _)) if key != repo => {
                return Err(cursor::invalid_field(
                    "page_token",
                    "malformed or foreign cursor",
                ));
            }
            Some((_, id)) => id,
            None => i64::MAX,
        };
        let mut connection = self.readers()?.begin().await.map_err(internal)?;
        let mut rows: Vec<Row> = sqlx::query_as(&format!(
            "{SELECT} WHERE run.repo_id = ? AND run.id < ? ORDER BY run.id DESC LIMIT ?"
        ))
        .bind(repo_id)
        .bind(after)
        .bind(size + 1)
        .fetch_all(&mut *connection)
        .await
        .map_err(internal)?;
        let more = rows.len() as i64 > size;
        rows.truncate(size as usize);
        let next_page_token = match rows.last() {
            Some(row) if more => cursor::encode(METHOD, &repo, row.0),
            _ => String::new(),
        };
        let mut runs = Vec::with_capacity(rows.len());
        // shortcut: three queries per run (at most 200 per page), batch them if pages get slow.
        for row in rows {
            runs.push(self.message(&mut connection, row).await?);
        }
        Ok(Response::new(ListRunsResponse {
            runs,
            next_page_token,
        }))
    }
}

#[cfg(test)]
mod tests;
