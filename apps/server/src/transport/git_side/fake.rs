//! Filesystem-backed GitService fake: server tests exercise the RPC boundary.
use super::*;
use kajiba_gitstore::{GitStore, Gix};
use kajiba_proto::git::v1::*;
use std::{fs, path::PathBuf};
use tonic::{Request, Response, service::Routes};

pub(crate) struct Fake {
    pub client: Repositories,
}
impl Fake {
    pub async fn start(root: &Path) -> Self {
        fs::create_dir_all(root.join("repos")).unwrap();
        fs::create_dir_all(root.join("tmp")).unwrap();
        let channel = tonic::transport::Endpoint::from_static("http://localhost")
            .connect_with_connector_lazy(Connector(root.into()));
        Self {
            client: Repositories {
                socket: root.join("absent.sock"),
                client: Some(GitServiceClient::new(channel)),
            },
        }
    }
}
#[derive(Clone)]
struct Connector(PathBuf);
impl tonic::codegen::Service<axum::http::Uri> for Connector {
    type Response = hyper_util::rt::TokioIo<tokio::io::DuplexStream>;
    type Error = std::io::Error;
    type Future = std::future::Ready<Result<Self::Response, Self::Error>>;
    fn poll_ready(
        &mut self,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }
    fn call(&mut self, _: axum::http::Uri) -> Self::Future {
        use hyper_util::{
            rt::{TokioExecutor, TokioIo},
            server::conn::auto::Builder,
            service::TowerToHyperService,
        };
        let (client, server) = tokio::io::duplex(64 * 1024);
        let app = Routes::new(git_service_server::GitServiceServer::new(Daemon(
            self.0.clone(),
        )))
        .into_axum_router();
        tokio::spawn(async move {
            let _ = Builder::new(TokioExecutor::new())
                .serve_connection(TokioIo::new(server), TowerToHyperService::new(app))
                .await;
        });
        std::future::ready(Ok(TokioIo::new(client)))
    }
}
struct Daemon(PathBuf);
#[tonic::async_trait]
impl git_service_server::GitService for Daemon {
    async fn hello(&self, _: Request<HelloRequest>) -> Result<Response<HelloResponse>, Status> {
        Ok(Response::new(HelloResponse {
            version: env!("CARGO_PKG_VERSION").into(),
            generation: "00000000000000000000000001".into(),
            sessions: vec![],
        }))
    }
    async fn create_repository(
        &self,
        request: Request<CreateRepositoryRequest>,
    ) -> Result<Response<CreateRepositoryResponse>, Status> {
        let request = request.into_inner();
        let name = format!("{}.git", request.repo_public_id);
        let tmp = self.0.join("tmp").join(&name);
        Gix.init_bare(&tmp, &request.default_branch)
            .map_err(|err| Status::internal(format!("{}: {err}", tmp.display())))?;
        fs::rename(tmp, self.0.join("repos").join(name))
            .map_err(|err| Status::internal(err.to_string()))?;
        Ok(Response::new(CreateRepositoryResponse {}))
    }
    async fn set_head(
        &self,
        request: Request<SetHeadRequest>,
    ) -> Result<Response<SetHeadResponse>, Status> {
        let request = request.into_inner();
        Gix.set_head(
            &self.0.join(format!("repos/{}.git", request.repo_public_id)),
            &request.branch,
        )
        .map_err(|err| Status::internal(err.to_string()))?;
        Ok(Response::new(SetHeadResponse {}))
    }
    async fn quarantine_repository(
        &self,
        request: Request<QuarantineRepositoryRequest>,
    ) -> Result<Response<QuarantineRepositoryResponse>, Status> {
        let name = format!("{}.git", request.into_inner().repo_public_id);
        let orphans = self.0.join("tmp/orphans");
        fs::create_dir_all(&orphans).map_err(|err| Status::internal(err.to_string()))?;
        let mut destination = orphans.join(&name);
        let mut suffix = 0;
        while fs::symlink_metadata(&destination).is_ok() {
            suffix += 1;
            destination = orphans.join(format!("{name}.{suffix}"));
        }
        fs::rename(self.0.join("repos").join(name), &destination)
            .map_err(|err| Status::internal(err.to_string()))?;
        Ok(Response::new(QuarantineRepositoryResponse {
            destination: destination.display().to_string(),
        }))
    }
    async fn list_sessions(
        &self,
        _: Request<ListSessionsRequest>,
    ) -> Result<Response<ListSessionsResponse>, Status> {
        Err(Status::unimplemented("test"))
    }
    async fn list_remote_refs(
        &self,
        _: Request<ListRemoteRefsRequest>,
    ) -> Result<Response<ListRemoteRefsResponse>, Status> {
        Err(Status::unimplemented("test"))
    }
    async fn fetch(&self, _: Request<FetchRequest>) -> Result<Response<FetchResponse>, Status> {
        Err(Status::unimplemented("test"))
    }
    async fn spawn(&self, _: Request<SpawnRequest>) -> Result<Response<SpawnResponse>, Status> {
        Err(Status::unimplemented("test"))
    }
}
