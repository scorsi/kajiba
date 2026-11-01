use super::*;
use crate::transport::{
    router,
    tests::{State, get},
};
use kajiba_proto::git::v1::*;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tonic::{Request, Response, service::Routes};

pub(in crate::transport) fn connected(link: &Link) {
    *link.client.lock().unwrap() = Some(GitServiceClient::new(
        tonic::transport::Endpoint::from_static("http://localhost").connect_lazy(),
    ));
}

struct Daemon {
    version: Arc<Mutex<String>>,
    calls: Arc<AtomicUsize>,
    generation: String,
}
#[tonic::async_trait]
impl git_service_server::GitService for Daemon {
    async fn hello(
        &self,
        request: Request<HelloRequest>,
    ) -> Result<Response<HelloResponse>, Status> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let version = self.version.lock().unwrap().clone();
        if let Some(err) = version_error(&version, &request.into_inner().version) {
            return Err(err);
        }
        Ok(Response::new(HelloResponse {
            version,
            generation: self.generation.clone(),
            sessions: vec![],
        }))
    }
    async fn list_sessions(
        &self,
        _: Request<ListSessionsRequest>,
    ) -> Result<Response<ListSessionsResponse>, Status> {
        Ok(Response::new(ListSessionsResponse {
            generation: self.generation.clone(),
            sessions: vec![Session {
                session_id: "00000000000000000000000002".into(),
                kind: SessionKind::Import.into(),
                repo_public_id: "00000000000000000000000001".into(),
                generation: self.generation.clone(),
                ..Default::default()
            }],
        }))
    }
    async fn create_repository(
        &self,
        _: Request<CreateRepositoryRequest>,
    ) -> Result<Response<CreateRepositoryResponse>, Status> {
        Err(Status::unimplemented("test"))
    }
    async fn set_head(
        &self,
        _: Request<SetHeadRequest>,
    ) -> Result<Response<SetHeadResponse>, Status> {
        Err(Status::unimplemented("test"))
    }
    async fn quarantine_repository(
        &self,
        _: Request<QuarantineRepositoryRequest>,
    ) -> Result<Response<QuarantineRepositoryResponse>, Status> {
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
    async fn gc(&self, _: Request<GcRequest>) -> Result<Response<GcResponse>, Status> {
        Err(Status::unimplemented("test"))
    }
    async fn spawn(&self, _: Request<SpawnRequest>) -> Result<Response<SpawnResponse>, Status> {
        Err(Status::unimplemented("test"))
    }
}

#[tokio::test]
async fn readiness_tracks_missing_mismatched_connected_and_restarted_daemon() {
    use axum::http::StatusCode;
    let state = State::new();
    state.start().await;
    let link = state.health.git_side.clone();
    link.client.lock().unwrap().take();
    let mut app = router(
        state.health.clone(),
        false,
        Arc::default(),
        "https://example.org",
    )
    .unwrap();
    let path = state.health.state_dir.join("git-local.sock");
    assert_eq!(
        get(&mut app, "/readyz").await,
        StatusCode::SERVICE_UNAVAILABLE
    );
    let err = link.client().unwrap_err();
    assert_eq!(err.code(), tonic::Code::Unavailable);
    assert_eq!(
        err.get_error_details().error_info().unwrap().reason,
        "GIT_SIDE_UNAVAILABLE"
    );
    let admin =
        crate::transport::local::tests::admin(&state.health.state_dir, "[users.alice]").await;
    sqlx::query("INSERT INTO repo (public_id, owner_id, name, visibility, ci_mode, default_branch, description, created_at) VALUES ('00000000000000000000000001', 1, 'demo', 'public', 'off', 'main', '', 0)")
        .execute(&mut *admin.writer.lock().await).await.unwrap();
    let admission = admin.gate.clone();
    let monitor = tokio::spawn({
        let path = path.clone();
        let link = link.clone();
        async move { link.run(&path, Some(&admin)).await }
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    let version = Arc::new(Mutex::new(format!("{}-other", env!("CARGO_PKG_VERSION"))));
    let calls = Arc::new(AtomicUsize::new(0));
    let start = || {
        let local = kajiba_core::local::Local::bind(
            &path,
            Arc::default(),
            0o660,
            vec![kajiba_core::local::effective_uid()],
        )
        .unwrap();
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(local.serve(
            Routes::new(git_service_server::GitServiceServer::new(Daemon {
                version: version.clone(),
                calls: calls.clone(),
                generation: kajiba_core::new_id().unwrap(),
            })),
            async move {
                let _ = stopped.await;
            },
        ));
        (stop, server)
    };
    let (stop, server) = start();
    wait_for(|| calls.load(Ordering::SeqCst) >= 2).await;
    assert!(!link.ready());
    assert_eq!(
        get(&mut app, "/readyz").await,
        StatusCode::SERVICE_UNAVAILABLE
    );
    *version.lock().unwrap() = env!("CARGO_PKG_VERSION").into();
    wait_for(|| link.ready()).await;
    assert!(admission.running(1));
    let first_generation = admission.generation();
    assert_eq!(get(&mut app, "/readyz").await, StatusCode::OK);
    stop.send(()).unwrap();
    server.await.unwrap().unwrap();
    wait_for(|| !link.ready()).await;
    assert!(admission.running(1), "a lost connection is not an exit");
    assert_eq!(
        get(&mut app, "/readyz").await,
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(get(&mut app, "/healthz").await, StatusCode::OK);
    let (stop, server) = start();
    wait_for(|| link.ready()).await;
    assert!(admission.running(1));
    assert_ne!(admission.generation(), first_generation);
    assert_eq!(get(&mut app, "/readyz").await, StatusCode::OK);
    monitor.abort();
    let _ = monitor.await;
    stop.send(()).unwrap();
    server.await.unwrap().unwrap();
    assert!(!link.ready());
}

async fn wait_for(check: impl Fn() -> bool) {
    tokio::time::timeout(Duration::from_secs(8), async {
        while !check() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn readyz_requires_both_gate_listener_and_version_accepted_link() {
    use axum::http::StatusCode;
    let state = State::new();
    state.start().await;
    let mut app = router(
        state.health.clone(),
        false,
        Arc::default(),
        "https://example.org",
    )
    .unwrap();
    assert_eq!(get(&mut app, "/readyz").await, StatusCode::OK);
    state.health.git_side.client.lock().unwrap().take();
    assert_eq!(
        get(&mut app, "/readyz").await,
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(get(&mut app, "/healthz").await, StatusCode::OK);
    connected(&state.health.git_side);
    assert_eq!(get(&mut app, "/readyz").await, StatusCode::OK);
    state.health.gate_serving.on.store(false, Ordering::Release);
    assert_eq!(
        get(&mut app, "/readyz").await,
        StatusCode::SERVICE_UNAVAILABLE
    );
    state.health.gate_serving.on.store(true, Ordering::Release);
    assert_eq!(get(&mut app, "/readyz").await, StatusCode::OK);
}
