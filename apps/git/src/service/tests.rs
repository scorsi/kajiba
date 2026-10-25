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
        request: Request<gate::GetAuthKeyRequest>,
    ) -> Result<Response<gate::GetAuthKeyResponse>, Status> {
        match request.into_inner().fingerprint.as_str() {
            "known" => Ok(Response::new(gate::GetAuthKeyResponse {
                user_public_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".into(),
                public_key: "ssh-ed25519 AAAA".into(),
            })),
            "unavailable" => Err(Status::unavailable("auth key lookup unavailable")),
            _ => Err(rpc_error(
                tonic::Code::NotFound,
                "KEY_NOT_FOUND",
                "unknown auth key",
            )),
        }
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
        request: Request<gate::PostReceiveRequest>,
    ) -> Result<Response<gate::PostReceiveResponse>, Status> {
        assert_eq!(
            request.into_inner().session_id,
            "01ARZ3NDEKTSV4RRFFQ69G5FAV"
        );
        Ok(Response::new(gate::PostReceiveResponse {}))
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

#[tokio::test]
async fn hook_service_forwards_keys_and_session_and_fails_when_server_stops() {
    use crate::{
        hook::{Kind, run},
        local,
    };
    use kajiba_proto::hook::v1::{self as hook, hook_service_server::HookServiceServer};
    use std::process::ExitCode;
    let temp = TempDir::new();
    let gate_path = temp.0.join("gate.sock");
    let hook_path = temp.0.join("local.sock");
    let generation = kajiba_core::new_id().unwrap();
    let version = Arc::new(Mutex::new(env!("CARGO_PKG_VERSION").into()));
    let calls = Arc::new(AtomicUsize::new(0));
    let bind = |path: &Path| {
        kajiba_core::local::Local::bind(
            path,
            Arc::default(),
            0o660,
            vec![kajiba_core::local::effective_uid()],
        )
        .unwrap()
    };
    let server = tokio::spawn(bind(&gate_path).serve(
        Routes::new(gate::gate_service_server::GateServiceServer::new(Server {
            version: version.clone(),
            calls: calls.clone(),
            generation: generation.clone(),
            exit_calls: Arc::default(),
            acknowledge: Arc::default(),
        })),
        std::future::pending(),
    ));
    let daemon = tokio::spawn(bind(&hook_path).serve(
        Routes::new(HookServiceServer::new(Git {
            generation,
            server_socket: gate_path,
            repositories: Arc::new(crate::repositories::Repositories::new(temp.0.clone())),
        })),
        std::future::pending(),
    ));
    let found = local::auth_key(&hook_path, "known".into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        found,
        (
            "01ARZ3NDEKTSV4RRFFQ69G5FAV".into(),
            "ssh-ed25519 AAAA".into()
        )
    );
    assert_eq!(
        crate::authorized_key(&found.0, &found.1),
        "command=\"kajiba-git serve-ssh --user 01ARZ3NDEKTSV4RRFFQ69G5FAV\",restrict ssh-ed25519 AAAA"
    );
    assert_eq!(
        local::auth_key(&hook_path, "unknown".into()).await.unwrap(),
        None
    );
    assert_eq!(
        crate::ssh_keys(&hook_path, "unknown".into()).await,
        ExitCode::SUCCESS
    );
    let error = local::auth_key(&hook_path, "unavailable".into())
        .await
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<Status>().unwrap().code(),
        tonic::Code::Unavailable
    );
    assert_eq!(
        error.downcast_ref::<Status>().unwrap().message(),
        "auth key lookup unavailable"
    );
    assert_eq!(
        crate::ssh_keys(&hook_path, "unavailable".into()).await,
        ExitCode::FAILURE
    );
    let input = b"0000000000000000000000000000000000000000 1111111111111111111111111111111111111111 refs/heads/main\n";
    let env = |var: &str| match var {
        "KAJIBA_USER" | "KAJIBA_REPO" | "KAJIBA_SESSION" => {
            Some("01ARZ3NDEKTSV4RRFFQ69G5FAV".into())
        }
        _ => None,
    };
    assert_eq!(
        run(&hook_path, Kind::PreReceive, &input[..], &env).await,
        ExitCode::FAILURE
    );
    assert_eq!(
        run(&hook_path, Kind::PostReceive, &input[..], &env).await,
        ExitCode::SUCCESS
    );
    let mut hooks =
        hook::hook_service_client::HookServiceClient::new(channel(&hook_path, None).await.unwrap());
    for invalid in ["", "not-a-session"] {
        let request = hook::PostReceiveRequest {
            user_public_id: env("KAJIBA_USER").unwrap(),
            repo_public_id: env("KAJIBA_REPO").unwrap(),
            session_id: invalid.into(),
            ..Default::default()
        };
        assert_eq!(
            hooks
                .post_receive(request.clone())
                .await
                .unwrap_err()
                .code(),
            tonic::Code::InvalidArgument
        );
        assert_eq!(
            hooks
                .pre_receive(hook::PreReceiveRequest {
                    user_public_id: request.user_public_id,
                    repo_public_id: request.repo_public_id,
                    session_id: request.session_id,
                    ..Default::default()
                })
                .await
                .unwrap_err()
                .code(),
            tonic::Code::InvalidArgument
        );
    }
    *version.lock().unwrap() = "other-version".into();
    let error = local::auth_key(&hook_path, "known".into())
        .await
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<Status>().unwrap().code(),
        tonic::Code::FailedPrecondition
    );
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
    let error = local::auth_key(&hook_path, "known".into())
        .await
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<Status>().unwrap().code(),
        tonic::Code::Unavailable
    );
    assert!(
        error
            .downcast_ref::<Status>()
            .unwrap()
            .message()
            .contains("server unavailable")
    );
    assert_eq!(
        crate::ssh_keys(&hook_path, "known".into()).await,
        ExitCode::FAILURE
    );
    for kind in [Kind::PreReceive, Kind::PostReceive] {
        assert_eq!(
            run(&hook_path, kind, &input[..], &env).await,
            ExitCode::FAILURE
        );
        assert_eq!(
            run(&temp.0.join("absent.sock"), kind, &input[..], &env).await,
            ExitCode::FAILURE
        );
    }
    daemon.abort();
    let _ = daemon.await;
}

#[tokio::test]
async fn hook_context_is_required_and_unreachable_server_fails_closed() {
    use kajiba_proto::hook::v1::{self as hook, hook_service_server::HookService};
    let temp = TempDir::new();
    let git = Git {
        generation: kajiba_core::new_id().unwrap(),
        server_socket: temp.0.join("absent.sock"),
        repositories: Arc::new(crate::repositories::Repositories::new(temp.0.clone())),
    };
    let id = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    for (user, repo, session) in [("", id, id), (id, "", id), (id, id, ""), (id, id, "bad")] {
        let pre = hook::PreReceiveRequest {
            user_public_id: user.into(),
            repo_public_id: repo.into(),
            session_id: session.into(),
            ..Default::default()
        };
        let post = hook::PostReceiveRequest {
            user_public_id: user.into(),
            repo_public_id: repo.into(),
            session_id: session.into(),
            ..Default::default()
        };
        assert_eq!(
            HookService::pre_receive(&git, Request::new(pre))
                .await
                .unwrap_err()
                .code(),
            tonic::Code::InvalidArgument
        );
        assert_eq!(
            HookService::post_receive(&git, Request::new(post))
                .await
                .unwrap_err()
                .code(),
            tonic::Code::InvalidArgument
        );
    }
    let pre = hook::PreReceiveRequest {
        user_public_id: id.into(),
        repo_public_id: id.into(),
        session_id: id.into(),
        ..Default::default()
    };
    let response = HookService::pre_receive(&git, Request::new(pre))
        .await
        .unwrap()
        .into_inner();
    assert!(!response.allowed);
    assert_eq!(response.refusals[0].reason, "pushes are not accepted yet");
    let post = hook::PostReceiveRequest {
        user_public_id: id.into(),
        repo_public_id: id.into(),
        session_id: id.into(),
        ..Default::default()
    };
    assert_eq!(
        HookService::post_receive(&git, Request::new(post))
            .await
            .unwrap_err()
            .code(),
        tonic::Code::Unavailable
    );
    let error = HookService::get_auth_key(
        &git,
        Request::new(hook::GetAuthKeyRequest {
            fingerprint: "known".into(),
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code(), tonic::Code::Unavailable);
    assert_eq!(error.message(), "server unavailable");
}
