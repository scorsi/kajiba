//! `AdministrationService` (ADR 0139, 0144): runners, mirrors and server information for
//! forge administrators. The interceptor holds the `admin` scope; the user must also be
//! an administrator, as `GetViewer` answers.
// These feed `?` in tonic handlers, which return `Status` by value anyway.
#![allow(clippy::result_large_err)]

use super::{Health, auth::Principal, local::refused, tokens::timestamp};
use kajiba_proto::v1::{
    GetServerInfoRequest, GetServerInfoResponse, ListMirrorsRequest, ListMirrorsResponse,
    ListRunnersRequest, ListRunnersResponse, MirrorStatus, MirrorStatusState, Runner,
    RunnerAttempt, ServerInfo, ServerUser, administration_service_server::AdministrationService,
};
use sqlx::SqlitePool;
use std::{path::Path, time::UNIX_EPOCH};
use tonic::{Request, Response, Status};

pub struct Administration(pub Health);

fn internal(err: impl std::fmt::Display) -> Status {
    Status::internal(format!("administration: {err}"))
}

impl Administration {
    /// The readers, once the caller is a forge administrator.
    async fn admin<T>(&self, request: &Request<T>) -> Result<&SqlitePool, Status> {
        let principal = request
            .extensions()
            .get::<Principal>()
            .ok_or_else(|| Status::internal("administration: no authenticated caller"))?;
        let readers = self
            .0
            .database
            .get()
            .ok_or_else(|| Status::unavailable("starting"))?;
        let admin: bool = sqlx::query_scalar("SELECT is_admin FROM user WHERE id = ?")
            .bind(principal.user_id)
            .fetch_one(readers)
            .await
            .map_err(internal)?;
        if !admin {
            return Err(refused(
                tonic::Code::PermissionDenied,
                "ADMIN_REQUIRED",
                "forge administrators only".into(),
            ));
        }
        Ok(readers)
    }
}

/// Bytes of the regular files below `dir`; unreadable entries count for nothing.
fn tree_bytes(dir: &Path) -> i64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| match entry.file_type() {
            Ok(kind) if kind.is_dir() => tree_bytes(&entry.path()),
            Ok(kind) if kind.is_file() => entry.metadata().map_or(0, |meta| meta.len() as i64),
            _ => 0,
        })
        .sum()
}

type RunnerRow = (
    i64,
    String,
    String,
    String,
    Option<String>,
    bool,
    bool,
    Option<i64>,
);

type MirrorRow = (
    String,
    String,
    String,
    bool,
    String,
    i64,
    Option<i64>,
    Option<String>,
    Option<i64>,
    Option<i64>,
);

#[tonic::async_trait]
impl AdministrationService for Administration {
    async fn list_runners(
        &self,
        request: Request<ListRunnersRequest>,
    ) -> Result<Response<ListRunnersResponse>, Status> {
        let readers = self.admin(&request).await?;
        let rows: Vec<RunnerRow> = sqlx::query_as(
            "SELECT id, name, capabilities, version, hostname, connected,
                    disabled_at IS NOT NULL, last_contact_at
             FROM runner ORDER BY name",
        )
        .fetch_all(readers)
        .await
        .map_err(internal)?;
        let running: Vec<(i64, String, String, Option<String>, i64)> = sqlx::query_as(
            "SELECT attempt.runner_id, run.public_id, owner.name || '/' || repo.name, run.ref,
                    attempt.started_at
             FROM job_attempt attempt
             JOIN job ON job.id = attempt.job_id
             JOIN run ON run.id = job.run_id
             JOIN repo ON repo.id = run.repo_id
             JOIN user owner ON owner.id = repo.owner_id
             WHERE attempt.state = 'running'
             ORDER BY attempt.started_at, attempt.id",
        )
        .fetch_all(readers)
        .await
        .map_err(internal)?;
        let runners = rows
            .into_iter()
            .map(
                |(id, name, capabilities, version, hostname, connected, disabled, contact)| {
                    // Written by the reconciliation from `config::Capabilities`.
                    let capabilities: serde_json::Value =
                        serde_json::from_str(&capabilities).unwrap_or_default();
                    let text =
                        |key: &str| capabilities[key].as_str().unwrap_or_default().to_owned();
                    let list = |key: &str| {
                        capabilities[key]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|value| value.as_str().map(str::to_owned))
                            .collect::<Vec<_>>()
                    };
                    Runner {
                        name,
                        arch: text("arch"),
                        os: text("os"),
                        backends: list("backends"),
                        tags: list("tags"),
                        max_jobs: capabilities["max_jobs"].as_u64().unwrap_or_default() as u32,
                        connected,
                        disabled,
                        version,
                        hostname: hostname.unwrap_or_default(),
                        last_contact_at: contact.map(timestamp),
                        current: running
                            .iter()
                            .filter(|attempt| attempt.0 == id)
                            .map(|(_, run_id, repo, reference, started)| RunnerAttempt {
                                run_id: run_id.clone(),
                                repo: repo.clone(),
                                r#ref: reference.clone().unwrap_or_default(),
                                started_at: Some(timestamp(*started)),
                            })
                            .collect(),
                    }
                },
            )
            .collect();
        Ok(Response::new(ListRunnersResponse { runners }))
    }

    async fn list_mirrors(
        &self,
        request: Request<ListMirrorsRequest>,
    ) -> Result<Response<ListMirrorsResponse>, Status> {
        let readers = self.admin(&request).await?;
        let rows: Vec<MirrorRow> = sqlx::query_as(
            "SELECT owner.name || '/' || repo.name, mirror.remote_url, mirror.credential,
                    mirror.enabled, mirror.state, mirror.attempts, mirror.last_success_at,
                    mirror.last_error, mirror.last_error_at, mirror.next_attempt_at
             FROM mirror
             JOIN repo ON repo.id = mirror.repo_id
             JOIN user owner ON owner.id = repo.owner_id
             ORDER BY 1, mirror.remote_url",
        )
        .fetch_all(readers)
        .await
        .map_err(internal)?;
        let mirrors = rows
            .into_iter()
            .map(
                |(
                    repo,
                    remote_url,
                    credential,
                    enabled,
                    state,
                    attempts,
                    success,
                    error,
                    error_at,
                    next,
                )| MirrorStatus {
                    repo,
                    remote_url,
                    credential,
                    enabled,
                    state: match state.as_str() {
                        "idle" => MirrorStatusState::Idle,
                        "queued" => MirrorStatusState::Queued,
                        "running" => MirrorStatusState::Running,
                        _ => MirrorStatusState::Unspecified,
                    }
                    .into(),
                    attempts,
                    last_success_at: success.map(timestamp),
                    last_error: error.unwrap_or_default(),
                    last_error_at: error_at.map(timestamp),
                    next_attempt_at: next.map(timestamp),
                },
            )
            .collect();
        Ok(Response::new(ListMirrorsResponse { mirrors }))
    }

    async fn get_server_info(
        &self,
        request: Request<GetServerInfoRequest>,
    ) -> Result<Response<GetServerInfoResponse>, Status> {
        let readers = self.admin(&request).await?;
        let users: Vec<(String, bool, bool)> = sqlx::query_as(
            "SELECT name, is_admin, disabled_at IS NOT NULL FROM user ORDER BY name",
        )
        .fetch_all(readers)
        .await
        .map_err(internal)?;
        let declared_tokens: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM api_token WHERE source = 'config' AND revoked_at IS NULL",
        )
        .fetch_one(readers)
        .await
        .map_err(internal)?;
        let database_bytes: i64 = sqlx::query_scalar(
            "SELECT page_count * page_size FROM pragma_page_count(), pragma_page_size()",
        )
        .fetch_one(readers)
        .await
        .map_err(internal)?;
        let repos = self.0.git_state_dir.join("repos");
        let storage_bytes = tokio::task::spawn_blocking(move || tree_bytes(&repos))
            .await
            .map_err(internal)?;
        let started = self
            .0
            .started_at
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_millis() as i64);
        Ok(Response::new(GetServerInfoResponse {
            server: Some(ServerInfo {
                version: env!("CARGO_PKG_VERSION").into(),
                started_at: Some(timestamp(started)),
                users: users
                    .into_iter()
                    .map(|(name, admin, disabled)| ServerUser {
                        name,
                        admin,
                        disabled,
                    })
                    .collect(),
                declared_tokens,
                database_bytes,
                storage_bytes,
                config_generation: self
                    .0
                    .config_generation
                    .load(std::sync::atomic::Ordering::Relaxed),
            }),
        }))
    }
}

#[cfg(test)]
mod tests;
