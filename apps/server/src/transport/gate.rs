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

pub const SOCKET_PATH: &str = "/run/kajiba/git.sock";

mod policy;

pub struct Gate {
    pub writer: Writer,
    pub generation: Mutex<Option<String>>,
    pub admission: Arc<crate::db::pushes::Gate>,
    pub bus: Arc<crate::db::events::EventBus>,
    pub state_dir: std::path::PathBuf,
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
        *self.generation.lock().unwrap() = Some(request.generation);
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
        self.open(request.into_inner()).await.map(Response::new)
    }

    async fn pre_receive(
        &self,
        request: Request<PreReceiveRequest>,
    ) -> Result<Response<PreReceiveResponse>, Status> {
        let request = request.into_inner();
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
        Ok(Response::new(PreReceiveResponse {
            allowed: refusals.is_empty(),
            refusals,
        }))
    }

    async fn post_receive(
        &self,
        _: Request<PostReceiveRequest>,
    ) -> Result<Response<PostReceiveResponse>, Status> {
        // Task 22 wires the reconciliation wake-up.
        Ok(Response::new(PostReceiveResponse {}))
    }

    async fn session_exited(
        &self,
        request: Request<SessionExitedRequest>,
    ) -> Result<Response<SessionExitedResponse>, Status> {
        let request = request.into_inner();
        if !kajiba_core::names::is_public_id(&request.session_id) {
            return Err(rpc_error(
                tonic::Code::InvalidArgument,
                "INVALID_FIELD",
                "invalid session_id",
            ));
        }
        let mut writer = self.writer.lock().await;
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
        Ok(Response::new(SessionExitedResponse {}))
    }
}

impl Gate {
    pub fn new(admin: &super::local::Admin) -> Self {
        Self {
            writer: admin.writer.clone(),
            generation: Mutex::default(),
            admission: admin.gate.clone(),
            bus: admin.bus.clone(),
            state_dir: admin.state_dir.clone(),
        }
    }

    async fn open(&self, request: OpenSessionRequest) -> Result<OpenSessionResponse, Status> {
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
        if (kind == SessionKind::Import
            && !kajiba_core::names::is_public_id(&request.import_session_id))
            || (kind != SessionKind::Import && !request.import_session_id.is_empty())
        {
            return Err(rpc_error(
                tonic::Code::InvalidArgument,
                "INVALID_FIELD",
                "invalid import_session_id",
            ));
        }
        let mut writer = self.writer.lock().await;
        let generation = self
            .generation
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| super::git_side::unavailable("Hello required"))?;
        if self.admission.generation().as_ref() != Some(&generation) {
            return Err(super::git_side::unavailable("session snapshot required"));
        }
        let public_id = match request.repository {
            Some(open_session_request::Repository::RepoPublicId(id))
                if kajiba_core::names::is_public_id(&id) =>
            {
                id
            }
            Some(open_session_request::Repository::RepoAddress(address)) => {
                let (owner, name) =
                    kajiba_core::names::repo_address(&address).ok_or_else(|| {
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
        if archived {
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
        let session_id = if kind == SessionKind::Import {
            // The reservation must still exist: a delayed Fetch after recovery cannot write.
            sqlx::query_scalar::<_, String>(
                "UPDATE pending_push SET git_generation = ?
                WHERE repo_id = ? AND correlation_id = ? AND principal_id IS NULL
                  AND git_session_id = ? AND git_generation IS NULL RETURNING git_session_id",
            )
            .bind(&generation)
            .bind(repo)
            .bind(&request.correlation_id)
            .bind(&request.import_session_id)
            .fetch_optional(&mut *writer)
            .await
            .map_err(|err| Status::internal(err.to_string()))?
            .ok_or_else(|| {
                rpc_error(
                    tonic::Code::FailedPrecondition,
                    "INVALID_STATE",
                    "import reservation is absent or already started",
                )
            })?
        } else {
            if kind != SessionKind::UploadPack {
                let pending: bool = sqlx::query_scalar(
                    "SELECT EXISTS(SELECT 1 FROM pending_push WHERE repo_id = ?)",
                )
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
            kajiba_core::new_id().map_err(|err| Status::internal(err.to_string()))?
        };
        let session = Session {
            session_id,
            kind: kind.into(),
            repo_public_id: public_id.clone(),
            user_public_id: request.user_public_id,
            correlation_id: request.correlation_id,
            generation,
        };
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
