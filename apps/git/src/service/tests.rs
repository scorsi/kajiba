use super::*;
use crate::state::tests::TempDir;
use git_service_server::GitService;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use tonic::service::Routes;

#[tokio::test]
async fn hello_requires_exact_version_and_returns_this_process_generation() {
    let git = Git {
        server_socket: "/absent.sock".into(),
        generation: kajiba_core::new_id().unwrap(),
        repositories: Arc::new(crate::repositories::Repositories::new(
            TempDir::new().0.clone(),
        )),
    };
    assert!(kajiba_core::names::is_public_id(&git.generation));
    assert_ne!(git.generation, kajiba_core::new_id().unwrap());
    for version in [
        String::new(),
        format!("{}-other", env!("CARGO_PKG_VERSION")),
        format!("{}+build", env!("CARGO_PKG_VERSION")),
    ] {
        let err = git
            .hello(Request::new(HelloRequest {
                version: version.clone(),
            }))
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::FailedPrecondition);
        assert!(err.message().contains(env!("CARGO_PKG_VERSION")));
        assert!(err.message().contains(&version));
    }
    let hello = git
        .hello(Request::new(HelloRequest {
            version: env!("CARGO_PKG_VERSION").into(),
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(hello.generation, git.generation);
    assert_eq!(hello.version, env!("CARGO_PKG_VERSION"));
    assert!(hello.sessions.is_empty());
    let sessions = git
        .list_sessions(Request::new(ListSessionsRequest {}))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(sessions.generation, hello.generation);
    assert!(sessions.sessions.is_empty());
    assert_eq!(
        git.fetch(Request::new(FetchRequest::default()))
            .await
            .unwrap_err()
            .code(),
        tonic::Code::InvalidArgument
    );
}

struct Server {
    version: Arc<Mutex<String>>,
    calls: Arc<AtomicUsize>,
    generation: String,
    exit_calls: Arc<AtomicUsize>,
    acknowledge: Arc<std::sync::atomic::AtomicBool>,
}

#[tonic::async_trait]
impl gate::gate_service_server::GateService for Server {
    async fn hello(
        &self,
        request: Request<gate::HelloRequest>,
    ) -> Result<Response<gate::HelloResponse>, Status> {
        let request = request.into_inner();
        assert_eq!(request.generation, self.generation);
        self.calls.fetch_add(1, Ordering::SeqCst);
        let version = self.version.lock().unwrap().clone();
        if let Some(err) = version_error(&version, &request.version) {
            return Err(err);
        }
        Ok(Response::new(gate::HelloResponse { version }))
    }
    async fn get_auth_key(
        &self,
        _: Request<gate::GetAuthKeyRequest>,
    ) -> Result<Response<gate::GetAuthKeyResponse>, Status> {
        Err(Status::unimplemented("test"))
    }
    async fn open_session(
        &self,
        _: Request<gate::OpenSessionRequest>,
    ) -> Result<Response<gate::OpenSessionResponse>, Status> {
        Err(Status::unimplemented("test"))
    }
    async fn pre_receive(
        &self,
        _: Request<gate::PreReceiveRequest>,
    ) -> Result<Response<gate::PreReceiveResponse>, Status> {
        Err(Status::unimplemented("test"))
    }
    async fn post_receive(
        &self,
        _: Request<gate::PostReceiveRequest>,
    ) -> Result<Response<gate::PostReceiveResponse>, Status> {
        Err(Status::unimplemented("test"))
    }
    async fn session_exited(
        &self,
        _: Request<gate::SessionExitedRequest>,
    ) -> Result<Response<gate::SessionExitedResponse>, Status> {
        self.exit_calls.fetch_add(1, Ordering::SeqCst);
        if !self.acknowledge.load(Ordering::SeqCst) {
            return Err(Status::unavailable("server restarting"));
        }
        Ok(Response::new(gate::SessionExitedResponse {}))
    }
}

#[tokio::test]
async fn server_handshake_retries_version_refusal_and_server_restart() {
    let temp = TempDir::new();
    let path = temp.0.join("git.sock");
    let generation = kajiba_core::new_id().unwrap();
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
            Routes::new(gate::gate_service_server::GateServiceServer::new(Server {
                version: version.clone(),
                calls: calls.clone(),
                generation: generation.clone(),
                exit_calls: Arc::default(),
                acknowledge: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            })),
            async move {
                let _ = stopped.await;
            },
        ));
        (stop, server)
    };
    let (stop, server) = start();
    let monitor = tokio::spawn({
        let path = path.clone();
        let generation = generation.clone();
        async move {
            connect_server(
                &path,
                &generation,
                &crate::repositories::Repositories::new(path.clone()),
            )
            .await
        }
    });
    wait_for(|| calls.load(Ordering::SeqCst) >= 2).await;
    *version.lock().unwrap() = env!("CARGO_PKG_VERSION").into();
    let before = calls.load(Ordering::SeqCst);
    wait_for(|| calls.load(Ordering::SeqCst) > before).await;
    stop.send(()).unwrap();
    server.await.unwrap().unwrap();
    // Let the connected client observe a missing socket before restarting the server.
    tokio::time::sleep(Duration::from_millis(1200)).await;
    let before = calls.load(Ordering::SeqCst);
    let (stop, server) = start();
    wait_for(|| calls.load(Ordering::SeqCst) > before).await;
    monitor.abort();
    let _ = monitor.await;
    stop.send(()).unwrap();
    server.await.unwrap().unwrap();
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
async fn exit_facts_survive_disconnection_and_are_retried_until_acknowledged() {
    let temp = TempDir::new();
    let path = temp.0.join("gate.sock");
    let generation = kajiba_core::new_id().unwrap();
    let repositories = Arc::new(crate::repositories::Repositories::new(temp.0.clone()));
    let session = Session {
        session_id: kajiba_core::new_id().unwrap(),
        generation: generation.clone(),
        kind: SessionKind::Import.into(),
        repo_public_id: kajiba_core::new_id().unwrap(),
        ..Default::default()
    };
    repositories.register(session.clone()).unwrap();
    let mut child = std::process::Command::new("cat")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    assert_eq!(repositories.sessions(), std::slice::from_ref(&session));
    drop(child.stdin.take());
    assert!(child.wait().unwrap().success());
    repositories.finished(&session.session_id, 0);
    let monitor = tokio::spawn({
        let repositories = repositories.clone();
        let path = path.clone();
        let generation = generation.clone();
        async move { connect_server(&path, &generation, &repositories).await }
    });
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(repositories.exits(), [(session.clone(), 0)]);
    let exit_calls = Arc::new(AtomicUsize::new(0));
    let acknowledge = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let local = kajiba_core::local::Local::bind(
        &path,
        Arc::default(),
        0o600,
        vec![kajiba_core::local::effective_uid()],
    )
    .unwrap();
    let server = tokio::spawn(local.serve(
        Routes::new(gate::gate_service_server::GateServiceServer::new(Server {
            version: Arc::new(Mutex::new(env!("CARGO_PKG_VERSION").into())),
            calls: Arc::default(),
            generation,
            exit_calls: exit_calls.clone(),
            acknowledge: acknowledge.clone(),
        })),
        std::future::pending(),
    ));
    wait_for(|| exit_calls.load(Ordering::SeqCst) >= 2).await;
    assert_eq!(repositories.exits(), [(session, 0)]);
    acknowledge.store(true, Ordering::SeqCst);
    wait_for(|| repositories.exits().is_empty()).await;
    monitor.abort();
    server.abort();
    let _ = monitor.await;
    let _ = server.await;
}
