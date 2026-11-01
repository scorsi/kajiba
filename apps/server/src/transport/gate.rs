use super::{correlation, local::Local};
use crate::{db::Writer, metrics::Registry};
use axum::middleware;
use kajiba_core::local::{rpc_error, version_error};
use kajiba_proto::gate::v1::{
    gate_service_server::{GateService, GateServiceServer},
    *,
};
use std::{
    io,
    path::Path,
    sync::{Arc, Mutex},
};
use tonic::{Request, Response, Status, service::Routes};

/// The gate socket, in the server's `run_dir` (ADR 0093).
pub const SOCKET_NAME: &str = "git.sock";

mod policy;

pub struct Gate {
    maintenance: super::maintenance::Admission,
    pub writer: Writer,
    pub generation: Mutex<Option<String>>,
    // shortcut: retain closed IDs per generation; use an acknowledgment watermark if volume requires a bound.
    closed: Mutex<std::collections::HashSet<String>>,
    pub admission: Arc<crate::db::pushes::Gate>,
    pub bus: Arc<crate::db::events::EventBus>,
    pub state_dir: std::path::PathBuf,
    pub registry: Arc<Registry>,
}

#[tonic::async_trait]
impl GateService for Gate {
    async fn hello(
        &self,
        request: Request<HelloRequest>,
    ) -> Result<Response<HelloResponse>, Status> {
        let request = request.into_inner();
        if let Some(err) = version_error(env!("CARGO_PKG_VERSION"), &request.version) {
            return Err(err);
        }
        if !kajiba_core::names::is_public_id(&request.generation) {
            let mut details = tonic_types::ErrorDetails::new();
            details.set_error_info("INVALID_FIELD", "kajiba", std::collections::HashMap::new());
            details.add_bad_request_violation("generation", "must be a canonical ULID");
            return Err(<Status as tonic_types::StatusExt>::with_error_details(
                tonic::Code::InvalidArgument,
                "invalid generation",
                details,
            ));
        }
        let _completion = self.maintenance.completion().await;
        let _writer = self.writer.lock().await;
        let mut generation = self.generation.lock().unwrap();
        if generation.as_ref() != Some(&request.generation) {
            self.closed.lock().unwrap().clear();
        }
        *generation = Some(request.generation);
        Ok(Response::new(HelloResponse {
            version: env!("CARGO_PKG_VERSION").into(),
        }))
    }

    async fn get_auth_key(
        &self,
        request: Request<GetAuthKeyRequest>,
    ) -> Result<Response<GetAuthKeyResponse>, Status> {
        auth_key(&self.writer, request.into_inner().fingerprint)
            .await
            .map(Response::new)
    }

    async fn open_session(
        &self,
        request: Request<OpenSessionRequest>,
    ) -> Result<Response<OpenSessionResponse>, Status> {
        let request = request.into_inner();
        let result = self.open(&request).await;
        if request.kind == kajiba_proto::git::v1::SessionKind::ReceivePack as i32 {
            if let Err(err) = &result {
                self.registry.push(false);
                let repo = match &request.repository {
                    Some(open_session_request::Repository::RepoPublicId(id)) => id.as_str(),
                    Some(open_session_request::Repository::RepoAddress(address)) => {
                        address.as_str()
                    }
                    None => "",
                };
                refused(&request.correlation_id, repo, "", refusal_reason(err));
            }
        }
        result.map(Response::new)
    }

    async fn pre_receive(
        &self,
        request: Request<PreReceiveRequest>,
    ) -> Result<Response<PreReceiveResponse>, Status> {
        let fallback = request
            .extensions()
            .get::<correlation::RequestId>()
            .cloned();
        let request = request.into_inner();
        let session = self.admission.session(&request.session_id);
        let request_id = match &session {
            Some((_, session)) => session.correlation_id.clone(),
            None => match fallback {
                Some(id) => id.0,
                None => kajiba_core::new_id().map_err(|err| Status::internal(err.to_string()))?,
            },
        };
        let repo = session
            .as_ref()
            .map_or("", |(_, session)| session.repo_public_id.as_str());
        let result = self.admit(request).await;
        self.registry
            .push(result.as_ref().is_ok_and(|response| response.allowed));
        match &result {
            Ok(response) => {
                for refusal in &response.refusals {
                    refused(&request_id, repo, &refusal.ref_name, &refusal.reason);
                }
            }
            Err(err) => refused(&request_id, repo, "", refusal_reason(err)),
        }
        result.map(Response::new)
    }

    async fn post_receive(
        &self,
        request: Request<PostReceiveRequest>,
    ) -> Result<Response<PostReceiveResponse>, Status> {
        if !kajiba_core::names::is_public_id(&request.into_inner().session_id) {
            return Err(rpc_error(
                tonic::Code::InvalidArgument,
                "INVALID_FIELD",
                "invalid session_id",
            ));
        }
        // The hook runs inside receive-pack: only SessionExited proves completion.
        self.admission.wake_recovery();
        Ok(Response::new(PostReceiveResponse {}))
    }

    async fn session_exited(
        &self,
        request: Request<SessionExitedRequest>,
    ) -> Result<Response<SessionExitedResponse>, Status> {
        let _completion = self.maintenance.completion().await;
        let request = request.into_inner();
        if !kajiba_core::names::is_public_id(&request.session_id) {
            return Err(rpc_error(
                tonic::Code::InvalidArgument,
                "INVALID_FIELD",
                "invalid session_id",
            ));
        }
        // The session's identity is gone once it exits; its push events carry it.
        let push = self.admission.session(&request.session_id);
        let mut writer = self.writer.lock().await;
        let before: i64 = sqlx::query_scalar("SELECT COALESCE(max(id), 0) FROM event")
            .fetch_one(&mut *writer)
            .await
            .map_err(|err| Status::internal(err.to_string()))?;
        // Also cancel an OpenSession still waiting for the writer lock (0061).
        self.closed
            .lock()
            .unwrap()
            .insert(request.session_id.clone());
        if let Some(repo) = self
            .admission
            .session_exited(&mut writer, &request.session_id)
            .await
            .map_err(|err| Status::internal(err.to_string()))?
        {
            self.admission
                .reconcile_repo(
                    &mut writer,
                    &self.bus,
                    &self.state_dir,
                    &kajiba_gitstore::Gix::shared(&self.state_dir.join("repos")),
                    repo,
                )
                .await
                .map_err(Status::internal)?;
        }
        let events: Vec<i64> = match &push {
            Some((repo, session)) => sqlx::query_scalar(
                "SELECT id FROM event WHERE repo_id = ? AND id > ? AND kind = 'push' AND correlation_id = ?",
            )
            .bind(repo)
            .bind(before)
            .bind(&session.correlation_id)
            .fetch_all(&mut *writer)
            .await
            .map_err(|err| Status::internal(err.to_string()))?,
            None => Vec::new(),
        };
        drop(writer);
        let runs = match (&push, events.first(), events.last()) {
            (Some((repo, _)), Some(&first), Some(&last)) => {
                self.push_runs(*repo, first, last).await
            }
            _ => Vec::new(),
        };
        Ok(Response::new(SessionExitedResponse { runs }))
    }
}

/// How long SessionExited waits for the `ci` handler to take a push's events (ADR 0132).
const PUSH_RUNS_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

impl Gate {
    /// The runs the `ci` handler created for the push events `first..=last`, once it
    /// took them; none after `PUSH_RUNS_WAIT` or on an error: the push never waits longer.
    async fn push_runs(&self, repo: i64, first: i64, last: i64) -> Vec<PushRun> {
        let deadline = tokio::time::Instant::now() + PUSH_RUNS_WAIT;
        loop {
            let found = async {
                let mut writer = self.writer.lock().await;
                let handled: i64 = sqlx::query_scalar(
                    "SELECT COALESCE((SELECT last_event_id FROM handler_checkpoint WHERE handler = 'ci'), 0)",
                )
                .fetch_one(&mut *writer)
                .await?;
                if handled < last {
                    return Ok(None);
                }
                let runs: Vec<(String, String)> = sqlx::query_as(
                    "SELECT public_id, ref FROM run WHERE repo_id = ? AND event_id BETWEEN ? AND ? ORDER BY event_id",
                )
                .bind(repo)
                .bind(first)
                .bind(last)
                .fetch_all(&mut *writer)
                .await?;
                Ok::<_, sqlx::Error>(Some(runs))
            }
            .await;
            match found {
                Ok(Some(runs)) => {
                    return runs
                        .into_iter()
                        .map(|(run_public_id, ref_name)| PushRun {
                            run_public_id,
                            ref_name,
                        })
                        .collect();
                }
                Ok(None) if tokio::time::Instant::now() < deadline => {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
                Ok(None) => return Vec::new(),
                Err(err) => {
                    tracing::warn!(event = "push-runs-unavailable", err = %err);
                    return Vec::new();
                }
            }
        }
    }
}

impl Gate {
    async fn admit(&self, request: PreReceiveRequest) -> Result<PreReceiveResponse, Status> {
        let _admitted = self.maintenance.admit()?;
        // Policy, admission and session exit share the writer lock.
        let mut writer = self.writer.lock().await;
        let mut refusals = self.refusals(&mut writer, &request).await?;
        if refusals.is_empty() {
            let (repo, session) = self
                .admission
                .session(&request.session_id)
                .ok_or_else(|| Status::failed_precondition("live receive-pack session required"))?;
            let principal: i64 = sqlx::query_scalar("SELECT id FROM user WHERE public_id = ?")
                .bind(&session.user_public_id)
                .fetch_one(&mut *writer)
                .await
                .map_err(|err| Status::internal(err.to_string()))?;
            let updates: Vec<_> = request
                .updates
                .into_iter()
                .map(|update| crate::db::pushes::RefUpdate {
                    name: update.ref_name,
                    old: update.old_id,
                    new: update.new_id,
                })
                .collect();
            if let Err(err) = self
                .admission
                .admit_session(&mut writer, repo, principal, &session, &updates)
                .await
            {
                if let crate::db::pushes::Refused::Database(err) = err {
                    return Err(Status::internal(err.to_string()));
                }
                refusals.push(Refusal {
                    ref_name: String::new(),
                    reason: err.to_string(),
                });
            }
        }
        Ok(PreReceiveResponse {
            allowed: refusals.is_empty(),
            refusals,
        })
    }

    pub fn new(admin: &super::local::Admin, registry: Arc<Registry>) -> Self {
        Self {
            maintenance: admin.maintenance.clone(),
            writer: admin.writer.clone(),
            generation: Mutex::default(),
            closed: Mutex::default(),
            admission: admin.gate.clone(),
            bus: admin.bus.clone(),
            state_dir: admin.state_dir.clone(),
            registry,
        }
    }

    async fn open(&self, request: &OpenSessionRequest) -> Result<OpenSessionResponse, Status> {
        use kajiba_core::config::Role as Permission;
        use kajiba_proto::git::v1::{Session, SessionKind};
        let kind = SessionKind::try_from(request.kind)
            .ok()
            .filter(|kind| *kind != SessionKind::Unspecified)
            .ok_or_else(|| {
                rpc_error(
                    tonic::Code::InvalidArgument,
                    "INVALID_FIELD",
                    "invalid session kind",
                )
            })?;
        let _admitted = if kind == SessionKind::UploadPack {
            None
        } else {
            Some(self.maintenance.admit()?)
        };
        if !kajiba_core::names::is_public_id(&request.session_id) {
            return Err(rpc_error(
                tonic::Code::InvalidArgument,
                "INVALID_FIELD",
                "invalid session_id",
            ));
        }
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
        let (mut writer, generation) = loop {
            let writer = tokio::time::timeout_at(deadline, self.writer.lock())
                .await
                .map_err(|_| super::git_side::unavailable("session snapshot required"))?;
            let generation = self
                .generation
                .lock()
                .unwrap()
                .clone()
                .ok_or_else(|| super::git_side::unavailable("Hello required"))?;
            if request.generation != generation {
                return Err(super::git_side::unavailable("session generation changed"));
            }
            if self.admission.generation().as_ref() == Some(&generation) {
                break (writer, generation);
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(super::git_side::unavailable("session snapshot required"));
            }
            // Snapshot rebuilding needs the writer lock too.
            drop(writer);
            tokio::time::sleep_until(
                deadline.min(tokio::time::Instant::now() + std::time::Duration::from_millis(25)),
            )
            .await;
        };
        if self.closed.lock().unwrap().contains(&request.session_id) {
            return Err(rpc_error(
                tonic::Code::FailedPrecondition,
                "INVALID_STATE",
                "session already closed",
            ));
        }
        let public_id = match &request.repository {
            Some(open_session_request::Repository::RepoPublicId(id))
                if kajiba_core::names::is_public_id(id) =>
            {
                id.clone()
            }
            Some(open_session_request::Repository::RepoAddress(address)) => {
                let (owner, name) = kajiba_core::names::repo_address(address).ok_or_else(|| {
                    rpc_error(
                        tonic::Code::InvalidArgument,
                        "INVALID_FIELD",
                        "invalid repository address",
                    )
                })?;
                sqlx::query_scalar::<_, String>(
                    "SELECT repo.public_id FROM repo JOIN user ON user.id = repo.owner_id
                     WHERE user.name = ?1 AND repo.name = ?2
                     UNION ALL SELECT repo.public_id FROM repo_redirect JOIN repo ON repo.id = repo_redirect.repo_id
                     WHERE repo_redirect.owner_name = ?1 AND repo_redirect.name = ?2 LIMIT 1")
                    .bind(owner).bind(name).fetch_optional(&mut *writer).await
                    .map_err(|err| Status::internal(err.to_string()))?
                    .ok_or_else(|| rpc_error(tonic::Code::NotFound, "REPO_NOT_FOUND", "repository not found"))?
            }
            _ => {
                return Err(rpc_error(
                    tonic::Code::InvalidArgument,
                    "INVALID_FIELD",
                    "invalid repository",
                ));
            }
        };
        let (repo, branch, archived): (i64, String, bool) = sqlx::query_as(
            "SELECT id, default_branch, archived_at IS NOT NULL FROM repo WHERE public_id = ?",
        )
        .bind(&public_id)
        .fetch_optional(&mut *writer)
        .await
        .map_err(|err| Status::internal(err.to_string()))?
        .ok_or_else(|| {
            rpc_error(
                tonic::Code::NotFound,
                "REPO_NOT_FOUND",
                "repository not found",
            )
        })?;
        if archived && kind != SessionKind::Maintenance {
            return Err(rpc_error(
                tonic::Code::FailedPrecondition,
                "INVALID_STATE",
                "repository is archived",
            ));
        }
        let mut signers = String::new();
        let (role, is_admin) = if matches!(kind, SessionKind::Import | SessionKind::Maintenance) {
            if !request.user_public_id.is_empty() {
                return Err(rpc_error(
                    tonic::Code::PermissionDenied,
                    "ROLE_MISSING",
                    "system session required",
                ));
            }
            (Role::Admin, false)
        } else {
            let (user, admin): (i64, bool) = sqlx::query_as(
                "SELECT id, is_admin FROM user WHERE public_id = ? AND disabled_at IS NULL",
            )
            .bind(&request.user_public_id)
            .fetch_optional(&mut *writer)
            .await
            .map_err(|err| Status::internal(err.to_string()))?
            .ok_or_else(|| {
                rpc_error(
                    tonic::Code::PermissionDenied,
                    "ROLE_MISSING",
                    "active user required",
                )
            })?;
            let permission = crate::db::permissions::role(&mut *writer, user, repo)
                .await
                .map_err(|err| Status::internal(err.to_string()))?;
            let required = if kind == SessionKind::UploadPack {
                Permission::Read
            } else {
                Permission::Write
            };
            let permission = permission.filter(|role| *role >= required).ok_or_else(|| {
                rpc_error(
                    tonic::Code::PermissionDenied,
                    "ROLE_MISSING",
                    "repository role required",
                )
            })?;
            let keys: Vec<String> = sqlx::query_scalar("SELECT public_key FROM ssh_key WHERE user_id = ? AND usage = 'signing' ORDER BY id")
                .bind(user).fetch_all(&mut *writer).await.map_err(|err| Status::internal(err.to_string()))?;
            for key in keys {
                signers.push_str(&format!("{} {key}\n", request.user_public_id));
            }
            (
                match permission {
                    Permission::Read => Role::Read,
                    Permission::Write => Role::Write,
                    Permission::Admin => Role::Admin,
                },
                admin,
            )
        };
        let session = Session {
            session_id: request.session_id.clone(),
            kind: kind.into(),
            repo_public_id: public_id.clone(),
            user_public_id: request.user_public_id.clone(),
            correlation_id: request.correlation_id.clone(),
            generation: generation.clone(),
        };
        let existing = self.admission.session(&request.session_id);
        if existing.as_ref().is_some_and(|(_, live)| live != &session) {
            return Err(rpc_error(
                tonic::Code::FailedPrecondition,
                "INVALID_STATE",
                "session identity changed",
            ));
        }
        if existing.is_none() && kind == SessionKind::Import {
            // The reservation must still exist: a delayed Fetch after recovery cannot write.
            sqlx::query_scalar::<_, String>(
                "UPDATE pending_push SET git_generation = ?
                WHERE repo_id = ? AND correlation_id = ? AND principal_id IS NULL
                  AND git_session_id = ? AND git_generation IS NULL RETURNING git_session_id",
            )
            .bind(&generation)
            .bind(repo)
            .bind(&request.correlation_id)
            .bind(&request.session_id)
            .fetch_optional(&mut *writer)
            .await
            .map_err(|err| Status::internal(err.to_string()))?
            .ok_or_else(|| {
                rpc_error(
                    tonic::Code::FailedPrecondition,
                    "INVALID_STATE",
                    "import reservation is absent or already started",
                )
            })?;
        } else if existing.is_none() && kind != SessionKind::UploadPack {
            let pending: bool =
                sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pending_push WHERE repo_id = ?)")
                    .bind(repo)
                    .fetch_one(&mut *writer)
                    .await
                    .map_err(|err| Status::internal(err.to_string()))?;
            if pending || self.admission.running(repo) {
                return Err(rpc_error(
                    tonic::Code::FailedPrecondition,
                    "INVALID_STATE",
                    "repository has an unfinished session; retry",
                ));
            }
        }
        self.admission.opened(repo, session.clone());
        Ok(OpenSessionResponse {
            session: Some(session),
            repo_public_id: public_id,
            default_branch: branch,
            role: role.into(),
            is_admin,
            allowed_signers: signers,
        })
    }
}

fn refused(request_id: &str, repo: &str, ref_name: &str, reason: &str) {
    tracing::info_span!("push", request_id).in_scope(|| {
        tracing::warn!(event = "push-refused", repo, "ref" = ref_name, reason);
    });
}

fn refusal_reason(status: &Status) -> &str {
    if status.code() == tonic::Code::Internal {
        "push admission failed"
    } else {
        status.message()
    }
}

pub(super) async fn auth_key(
    writer: &Writer,
    fingerprint: String,
) -> Result<GetAuthKeyResponse, Status> {
    let found: Option<(String, String)> = sqlx::query_as(
        "SELECT user.public_id, ssh_key.public_key FROM ssh_key
         JOIN user ON user.id = ssh_key.user_id
         WHERE ssh_key.fingerprint = ? AND ssh_key.usage = 'auth' AND user.disabled_at IS NULL",
    )
    .bind(fingerprint)
    .fetch_optional(&mut *writer.lock().await)
    .await
    .map_err(|err| Status::internal(format!("auth key lookup: {err}")))?;
    let (user_public_id, public_key) = found
        .ok_or_else(|| rpc_error(tonic::Code::NotFound, "KEY_NOT_FOUND", "no active auth key"))?;
    Ok(GetAuthKeyResponse {
        user_public_id,
        public_key,
    })
}

pub(super) fn bind(
    path: &Path,
    serving: Arc<super::local::Serving>,
    uid: u32,
    gid: u32,
) -> io::Result<Local> {
    // Restrict the socket until its group has been installed; never admit root implicitly.
    let local = Local::bind(path, serving, 0o600, vec![uid])?;
    std::os::unix::fs::chown(path, None, Some(gid))?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o660))?;
    Ok(local)
}

pub(super) async fn serve(
    local: Local,
    gate: Gate,
    registry: Arc<Registry>,
    shutdown: impl std::future::Future<Output = ()>,
) -> io::Result<()> {
    let routes = Routes::new(GateServiceServer::new(gate))
        .into_axum_router()
        .layer(middleware::from_fn_with_state(
            registry,
            correlation::request,
        ));
    local.serve(routes.into(), shutdown).await
}

#[cfg(test)]
mod tests;
