//! `RunService` (0008, 0010, 0013, 0020): manual trigger and run reads.
// These feed `?` in tonic handlers, which return `Status` by value anyway.
#![allow(clippy::result_large_err)]

use super::{
    Health, cursor,
    local::refused,
    repos::{caller, find_readable, internal},
};
use kajiba_core::config::Role;
use kajiba_gitstore::{GitStore, Gix};
use kajiba_proto::{
    google::protobuf::Timestamp,
    v1::{
        Attempt, AttemptState, FailureSummary, GetRunRequest, GetRunResponse, Job, JobState,
        ListRunsRequest, ListRunsResponse, Run, RunState, RunTrigger, TriggerRunRequest,
        TriggerRunResponse, run_service_server::RunService,
    },
};
use sqlx::SqliteConnection;
use std::{io::Read, path::Path};
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
);

const SELECT: &str = "SELECT run.id, run.public_id, repo.public_id, run.sha, run.ref, run.trigger,
        user.name, run.state, run.created_at, run.started_at, run.finished_at,
        run.failure_check, run.failure_reason
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
        let (finished, check, reason) = (row.10, row.11, row.12);
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
                let attempt: Option<AttemptRow> = sqlx::query_as(
                    "SELECT job_attempt.id, job_attempt.public_id, number, state, runner.name,
                            runner.hostname, exit_code, started_at, finished_at,
                            (SELECT MAX(offset + length) FROM log_chunk WHERE attempt_id = job_attempt.id)
                     FROM job_attempt JOIN runner ON runner.id = runner_id
                     WHERE job_id = ? ORDER BY number DESC LIMIT 1",
                )
                .bind(job_id)
                .fetch_optional(&mut *connection)
                .await
                .map_err(internal)?;
                let current_attempt = attempt.map(|attempt| {
                    current = Some((attempt.1.clone(), attempt.9.unwrap_or(0)));
                    Attempt {
                        public_id: attempt.1,
                        number: attempt.2 as u32,
                        state: attempt_state(&attempt.3).into(),
                        runner: attempt.4,
                        runner_hostname: attempt.5.unwrap_or_default(),
                        exit_code: attempt.6,
                        started_at: time(Some(attempt.7)),
                        finished_at: time(attempt.8),
                    }
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
                .map(|(attempt, end)| {
                    last_lines(
                        &self.0.state_dir.join(format!("ci/logs/{attempt}.log")),
                        end as u64,
                    )
                })
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
        })
    }
}

type AttemptRow = (
    i64,
    String,
    i64,
    String,
    String,
    Option<String>,
    Option<i32>,
    i64,
    Option<i64>,
    Option<i64>,
);

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

#[tonic::async_trait]
impl RunService for Runs {
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
        // ADR 0114: archived, then the role, then CI, then the commit.
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

    async fn get_run(
        &self,
        request: Request<GetRunRequest>,
    ) -> Result<Response<GetRunResponse>, Status> {
        let user = caller(&request)?;
        let mut connection = self.readers()?.begin().await.map_err(internal)?;
        let row: Row = sqlx::query_as(&format!("{SELECT} WHERE run.public_id = ?"))
            .bind(&request.get_ref().run)
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
            .ok_or_else(run_not_found)?;
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
