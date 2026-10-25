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
            // No children can start until session accounting is implemented (J3 task 13).
            sessions: vec![],
        }))
    }

    async fn list_sessions(
        &self,
        _: Request<ListSessionsRequest>,
    ) -> Result<Response<ListSessionsResponse>, Status> {
        Ok(Response::new(ListSessionsResponse {
            generation: self.generation.clone(),
            sessions: vec![],
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
        _: Request<ListRemoteRefsRequest>,
    ) -> Result<Response<ListRemoteRefsResponse>, Status> {
        Err(not_implemented())
    }
    async fn fetch(&self, _: Request<FetchRequest>) -> Result<Response<FetchResponse>, Status> {
        Err(not_implemented())
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
