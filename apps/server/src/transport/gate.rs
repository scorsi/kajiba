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

pub struct Gate {
    pub writer: Writer,
    pub generation: Mutex<Option<String>>,
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
        _: Request<OpenSessionRequest>,
    ) -> Result<Response<OpenSessionResponse>, Status> {
        Err(rpc_error(
            tonic::Code::Unimplemented,
            "NOT_IMPLEMENTED",
            "sessions are not implemented yet",
        ))
    }

    async fn pre_receive(
        &self,
        _: Request<PreReceiveRequest>,
    ) -> Result<Response<PreReceiveResponse>, Status> {
        Ok(Response::new(pre_receive()))
    }

    async fn post_receive(
        &self,
        _: Request<PostReceiveRequest>,
    ) -> Result<Response<PostReceiveResponse>, Status> {
        // No push is admitted yet; task 22 wires reconciliation.
        Ok(Response::new(PostReceiveResponse {}))
    }

    async fn session_exited(
        &self,
        _: Request<SessionExitedRequest>,
    ) -> Result<Response<SessionExitedResponse>, Status> {
        Err(rpc_error(
            tonic::Code::Unimplemented,
            "NOT_IMPLEMENTED",
            "sessions are not implemented yet",
        ))
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

pub(super) fn pre_receive() -> PreReceiveResponse {
    // Fail closed until ref policy, signatures and durable admission are wired (task 21).
    PreReceiveResponse {
        allowed: false,
        refusals: vec![Refusal {
            ref_name: String::new(),
            reason: "pushes are not accepted yet".into(),
        }],
    }
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
