//! Filesystem-backed GitService fake: server tests exercise the RPC boundary.
use super::*;
use kajiba_gitstore::{GitStore, Gix};
use kajiba_proto::git::v1::*;
use std::{fs, os::unix::ffi::OsStringExt, path::PathBuf};
use tonic::{Request, Response, service::Routes};

pub(crate) struct Fake {
    pub client: Repositories,
}
impl Fake {
    pub fn serve(root: &Path, socket: &Path) -> tokio::task::JoinHandle<()> {
        let local = kajiba_core::local::Local::bind(
            socket,
            Default::default(),
            0o600,
            vec![kajiba_core::local::effective_uid()],
        )
        .unwrap();
        let routes = Routes::new(git_service_server::GitServiceServer::new(Daemon(
            root.into(),
        )));
        tokio::spawn(async move {
            local.serve(routes, std::future::pending()).await.unwrap();
        })
    }

    pub async fn start(root: &Path) -> Self {
        fs::create_dir_all(root.join("repos")).unwrap();
        fs::create_dir_all(root.join("tmp")).unwrap();
        let channel = tonic::transport::Endpoint::from_static("http://localhost")
            .connect_with_connector_lazy(Connector(root.into()));
        Self {
            client: Repositories {
                socket: root.join("absent.sock"),
                generation: String::new(),
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
        let name = std::ffi::OsString::from_vec(request.into_inner().directory_name);
        let orphans = self.0.join("tmp/orphans");
        fs::create_dir_all(&orphans).map_err(|err| Status::internal(err.to_string()))?;
        let mut destination = orphans.join(&name);
        let mut suffix = 0;
        while fs::symlink_metadata(&destination).is_ok() {
            suffix += 1;
            let mut suffixed = name.clone();
            suffixed.push(format!(".{suffix}"));
            destination = orphans.join(suffixed);
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
        Ok(Response::new(ListSessionsResponse {
            generation: "00000000000000000000000001".into(),
            sessions: vec![],
        }))
    }
    async fn list_remote_refs(
        &self,
        request: Request<ListRemoteRefsRequest>,
    ) -> Result<Response<ListRemoteRefsResponse>, Status> {
        let r = request.into_inner();
        let refs = tokio::task::spawn_blocking(move || Gix.remote_refs(&r.source, &r.credentials))
            .await
            .unwrap()
            .map_err(|err| {
                Status::failed_precondition(kajiba_gitstore::import::reason(&err.to_string()))
            })?;
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
        let r = request.into_inner();
        let session = r.session.unwrap();
        assert_eq!(session.kind, SessionKind::Import as i32);
        assert_eq!(session.repo_public_id, r.repo_public_id);
        assert_eq!(session.generation, "00000000000000000000000001");
        use sqlx::Connection;
        let mut reader = sqlx::SqliteConnection::connect_with(
            &sqlx::sqlite::SqliteConnectOptions::new()
                .filename(self.0.join("db/kajiba.sqlite"))
                .read_only(true),
        )
        .await
        .unwrap();
        let (principal, correlation, updates): (Option<i64>, String, String) = sqlx::query_as(
            "SELECT principal_id, correlation_id, updates FROM pending_push WHERE repo_id = (SELECT id FROM repo WHERE public_id = ?)"
        ).bind(&r.repo_public_id).fetch_one(&mut reader).await.unwrap();
        assert_eq!(principal, None);
        assert_eq!(correlation, session.correlation_id);
        let updates: serde_json::Value = serde_json::from_str(&updates).unwrap();
        assert_eq!(updates.as_array().unwrap().len(), r.refs.len());
        for (u, r) in updates.as_array().unwrap().iter().zip(&r.refs) {
            assert_eq!(u["ref"], r.ref_name);
            assert_eq!(u["new"], r.object_id);
            assert_eq!(u["old"], crate::db::pushes::ZERO);
        }
        let path = self.0.join(format!("repos/{}.git", r.repo_public_id));
        assert!(Gix.refs(&path).unwrap().is_empty());
        let refs: Vec<_> = r
            .refs
            .into_iter()
            .map(|r| kajiba_gitstore::Ref {
                name: r.ref_name,
                target: r.object_id,
            })
            .collect();
        tokio::task::spawn_blocking(move || Gix.fetch(&path, &r.source, &r.credentials, &refs))
            .await
            .unwrap()
            .map_err(|err| {
                Status::failed_precondition(kajiba_gitstore::import::reason(&err.to_string()))
            })?;
        Ok(Response::new(FetchResponse {}))
    }
    async fn spawn(&self, _: Request<SpawnRequest>) -> Result<Response<SpawnResponse>, Status> {
        Err(Status::unimplemented("test"))
    }
}
