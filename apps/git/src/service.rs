// Tonic handlers return Status by value across the blocking filesystem work.
#![allow(clippy::result_large_err)]
use kajiba_core::local::{channel, rpc_error, version_error};
use kajiba_proto::{gate::v1 as gate, git::v1::*};
use std::{path::Path, time::Duration};
use tonic::{Request, Response, Status};

pub struct Git {
    pub generation: String,
    pub repositories: std::sync::Arc<crate::repositories::Repositories>,
}

#[tonic::async_trait]
impl git_service_server::GitService for Git {
    async fn hello(
        &self,
        request: Request<HelloRequest>,
    ) -> Result<Response<HelloResponse>, Status> {
        if let Some(err) = version_error(env!("CARGO_PKG_VERSION"), &request.into_inner().version) {
            return Err(err);
        }
        Ok(Response::new(HelloResponse {
            version: env!("CARGO_PKG_VERSION").into(),
            generation: self.generation.clone(),
            sessions: self.repositories.sessions(),
        }))
    }

    async fn list_sessions(
        &self,
        _: Request<ListSessionsRequest>,
    ) -> Result<Response<ListSessionsResponse>, Status> {
        Ok(Response::new(ListSessionsResponse {
            generation: self.generation.clone(),
            sessions: self.repositories.sessions(),
        }))
    }

    async fn create_repository(
        &self,
        request: Request<CreateRepositoryRequest>,
    ) -> Result<Response<CreateRepositoryResponse>, Status> {
        let request = request.into_inner();
        let repositories = self.repositories.clone();
        let () = tokio::task::spawn_blocking(move || {
            repositories.create(&request.repo_public_id, &request.default_branch)
        })
        .await
        .map_err(|err| Status::internal(err.to_string()))??;
        Ok(Response::new(CreateRepositoryResponse {}))
    }
    async fn set_head(
        &self,
        request: Request<SetHeadRequest>,
    ) -> Result<Response<SetHeadResponse>, Status> {
        let request = request.into_inner();
        let repositories = self.repositories.clone();
        let () = tokio::task::spawn_blocking(move || {
            repositories.set_head(&request.repo_public_id, &request.branch)
        })
        .await
        .map_err(|err| Status::internal(err.to_string()))??;
        Ok(Response::new(SetHeadResponse {}))
    }
    async fn quarantine_repository(
        &self,
        request: Request<QuarantineRepositoryRequest>,
    ) -> Result<Response<QuarantineRepositoryResponse>, Status> {
        let request = request.into_inner();
        let repositories = self.repositories.clone();
        let result =
            tokio::task::spawn_blocking(move || repositories.quarantine(&request.repo_public_id))
                .await
                .map_err(|err| Status::internal(err.to_string()))??;
        Ok(Response::new(QuarantineRepositoryResponse {
            destination: result.to_string_lossy().into_owned(),
        }))
    }
    async fn list_remote_refs(
        &self,
        request: Request<ListRemoteRefsRequest>,
    ) -> Result<Response<ListRemoteRefsResponse>, Status> {
        use kajiba_gitstore::GitStore;
        let request = request.into_inner();
        kajiba_gitstore::import::check_source(&request.source)
            .map_err(|err| rpc_error(tonic::Code::InvalidArgument, "INVALID_FIELD", err))?;
        let refs = tokio::task::spawn_blocking(move || {
            kajiba_gitstore::Gix
                .remote_refs(&request.source, &request.credentials)
                .map_err(crate::repositories::source_error)
        })
        .await
        .map_err(|_| Status::internal("import worker failed"))??;
        Ok(Response::new(ListRemoteRefsResponse {
            refs: refs
                .into_iter()
                .map(|r| RemoteRef {
                    ref_name: r.name,
                    object_id: r.target,
                })
                .collect(),
        }))
    }
    async fn fetch(
        &self,
        request: Request<FetchRequest>,
    ) -> Result<Response<FetchResponse>, Status> {
        let request = request.into_inner();
        let valid = request.session.as_ref().is_some_and(|s| {
            kajiba_core::names::is_public_id(&s.session_id)
                && s.kind == SessionKind::Import as i32
                && s.repo_public_id == request.repo_public_id
                && s.user_public_id.is_empty()
                && s.generation == self.generation
        });
        if !valid {
            return Err(rpc_error(
                tonic::Code::InvalidArgument,
                "INVALID_FIELD",
                "invalid import session",
            ));
        }
        kajiba_gitstore::import::check_source(&request.source)
            .map_err(|err| rpc_error(tonic::Code::InvalidArgument, "INVALID_FIELD", err))?;
        let repositories = self.repositories.clone();
        // Blocking work and its session survive a cancelled RPC.
        tokio::task::spawn_blocking(move || repositories.fetch(request))
            .await
            .map_err(|_| Status::internal("import worker failed"))??;
        Ok(Response::new(FetchResponse {}))
    }
    async fn spawn(&self, _: Request<SpawnRequest>) -> Result<Response<SpawnResponse>, Status> {
        Err(not_implemented())
    }
}

fn not_implemented() -> Status {
    rpc_error(
        tonic::Code::Unimplemented,
        "NOT_IMPLEMENTED",
        "repository operations are not implemented yet",
    )
}

/// The daemon is ready independently of the server. Retry until the other unit is up.
pub async fn connect_server(path: &Path, generation: &str) {
    let mut client = None;
    let mut delay = Duration::from_millis(100);
    let mut unavailable = false;
    loop {
        let result = async {
            if client.is_none() {
                client = Some(gate::gate_service_client::GateServiceClient::new(
                    channel(path, Some(Duration::from_secs(5)))
                        .await
                        .map_err(|err| err.to_string())?,
                ));
            }
            let hello = client
                .as_mut()
                .unwrap()
                .hello(gate::HelloRequest {
                    version: env!("CARGO_PKG_VERSION").into(),
                    generation: generation.into(),
                })
                .await
                .map_err(|err| err.to_string())?
                .into_inner();
            if let Some(err) = version_error(env!("CARGO_PKG_VERSION"), &hello.version) {
                return Err(err.to_string());
            }
            Ok::<_, String>(hello)
        }
        .await;
        match result {
            Ok(hello) => {
                if unavailable || delay == Duration::from_millis(100) {
                    tracing::info!(event = "server-connected", version = hello.version);
                }
                unavailable = false;
                delay = Duration::from_secs(1);
            }
            Err(err) => {
                client = None;
                if !unavailable {
                    tracing::warn!(event = "server-unavailable", err);
                }
                unavailable = true;
                delay = (delay * 2).min(Duration::from_secs(5));
            }
        }
        tokio::time::sleep(delay).await;
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod import_tests;
