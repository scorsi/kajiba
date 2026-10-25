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
    admissions: Arc<Mutex<Vec<gate::PreReceiveRequest>>>,
    verdict: Arc<Mutex<gate::PreReceiveResponse>>,
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
        request: Request<gate::PreReceiveRequest>,
    ) -> Result<Response<gate::PreReceiveResponse>, Status> {
        self.admissions.lock().unwrap().push(request.into_inner());
        Ok(Response::new(self.verdict.lock().unwrap().clone()))
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
                admissions: Arc::default(),
                verdict: Arc::default(),
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
    repositories
        .register(session.clone(), String::new())
        .unwrap();
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
            admissions: Arc::default(),
            verdict: Arc::default(),
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
    use kajiba_gitstore::GitStore;
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
            admissions: Arc::default(),
            verdict: Arc::default(),
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
    let repo = temp.0.join("live.git");
    kajiba_gitstore::Gix.init_bare(&repo, "main").unwrap();
    let mut child = tokio::process::Command::new("git")
        .arg("receive-pack")
        .arg(repo)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    for _ in 0..2 {
        assert_eq!(
            tokio::time::timeout(
                Duration::from_secs(2),
                run(&hook_path, Kind::PostReceive, &input[..], &env),
            )
            .await
            .unwrap(),
            ExitCode::SUCCESS
        );
        assert!(child.try_wait().unwrap().is_none());
    }
    child.kill().await.unwrap();
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
async fn pre_receive_reads_quarantined_objects_and_fails_closed_on_missing_objects() {
    use kajiba_proto::hook::v1::{self as hook, hook_service_server::HookService};
    let temp = TempDir::new();
    let _lock = crate::state::prepare(&temp.0).unwrap();
    let git = Git {
        generation: kajiba_core::new_id().unwrap(),
        server_socket: temp.0.join("absent.sock"),
        repositories: Arc::new(crate::repositories::Repositories::new(temp.0.clone())),
    };
    let id = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    git.repositories.create(id, "main").unwrap();
    let repo = temp.0.join(format!("repos/{id}.git"));
    let incoming = repo.join("objects/incoming-test");
    std::fs::create_dir(&incoming).unwrap();
    let command = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .arg("--git-dir")
            .arg(&repo)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "Alice")
            .env("GIT_AUTHOR_EMAIL", "alice@example.org")
            .env("GIT_COMMITTER_NAME", "Alice")
            .env("GIT_COMMITTER_EMAIL", "alice@example.org")
            .env("GIT_OBJECT_DIRECTORY", &incoming)
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    };
    let tree = command(&["mktree"]);
    let commit = command(&["commit-tree", &tree, "-m", "initial"]);
    let mut request = hook::PreReceiveRequest {
        user_public_id: id.into(),
        repo_public_id: id.into(),
        session_id: id.into(),
        updates: vec![hook::RefUpdate {
            ref_name: "refs/heads/main".into(),
            old_id: "0000000000000000000000000000000000000000".into(),
            new_id: commit.clone(),
        }],
        quarantine: Some(hook::Quarantine {
            object_directory: incoming.to_str().unwrap().into(),
            alternate_object_directories: repo.join("objects").to_str().unwrap().into(),
            quarantine_path: incoming.to_str().unwrap().into(),
        }),
    };
    git.repositories
        .register(
            Session {
                session_id: id.into(),
                user_public_id: id.into(),
                repo_public_id: id.into(),
                kind: SessionKind::ReceivePack.into(),
                generation: git.generation.clone(),
                ..Default::default()
            },
            String::new(),
        )
        .unwrap();
    let (facts, refusals) = git.repositories.check_signatures(&request).unwrap();
    assert_eq!(refusals[0].ref_name, "refs/heads/main");
    assert!(refusals[0].reason.contains(&commit));
    assert_eq!(facts.commits, [commit]);
    assert_eq!(facts.fast_forward, [false]);
    assert_eq!(
        HookService::pre_receive(&git, Request::new(request.clone()))
            .await
            .unwrap_err()
            .code(),
        tonic::Code::Unavailable
    );
    request.quarantine = None;
    assert!(
        HookService::pre_receive(&git, Request::new(request))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn hook_context_is_required_and_unreachable_server_fails_closed() {
    use kajiba_proto::hook::v1::{self as hook, hook_service_server::HookService};
    let temp = TempDir::new();
    let _lock = crate::state::prepare(&temp.0).unwrap();
    let git = Git {
        generation: kajiba_core::new_id().unwrap(),
        server_socket: temp.0.join("absent.sock"),
        repositories: Arc::new(crate::repositories::Repositories::new(temp.0.clone())),
    };
    let id = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    git.repositories.create(id, "main").unwrap();
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
    assert_eq!(
        HookService::pre_receive(&git, Request::new(pre))
            .await
            .unwrap_err()
            .code(),
        tonic::Code::FailedPrecondition
    );
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

#[tokio::test]
async fn signature_checks_use_session_signers_and_remove_the_temporary_file() {
    use kajiba_proto::hook::v1::{self as hook, hook_service_server::HookService};
    let temp = TempDir::new();
    let _lock = crate::state::prepare(&temp.0).unwrap();
    let git = Git {
        generation: kajiba_core::new_id().unwrap(),
        server_socket: temp.0.join("absent.sock"),
        repositories: Arc::new(crate::repositories::Repositories::new(temp.0.clone())),
    };
    let id = kajiba_core::new_id().unwrap();
    git.repositories.create(&id, "main").unwrap();
    let repo = temp.0.join(format!("repos/{id}.git"));
    let key = temp.0.join("signing-key");
    let output = std::process::Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-f"])
        .arg(&key)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let command = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .arg("--git-dir")
            .arg(&repo)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "Alice")
            .env("GIT_AUTHOR_EMAIL", "alice@example.org")
            .env("GIT_COMMITTER_NAME", "Alice")
            .env("GIT_COMMITTER_EMAIL", "alice@example.org")
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    };
    let tree = command(&["mktree"]);
    let commit = command(&[
        "-c",
        "gpg.format=ssh",
        "-c",
        &format!("user.signingKey={}", key.display()),
        "commit-tree",
        &tree,
        "-S",
        "-m",
        "signed",
    ]);
    let mut request = hook::PreReceiveRequest {
        user_public_id: id.clone(),
        repo_public_id: id.clone(),
        session_id: id.clone(),
        updates: vec![hook::RefUpdate {
            ref_name: "refs/heads/main".into(),
            old_id: "0000000000000000000000000000000000000000".into(),
            new_id: commit.clone(),
        }],
        ..Default::default()
    };
    let session = Session {
        session_id: id.clone(),
        user_public_id: id.clone(),
        repo_public_id: id.clone(),
        kind: SessionKind::ReceivePack.into(),
        generation: git.generation.clone(),
        ..Default::default()
    };
    let signers = format!(
        "{id} {}",
        std::fs::read_to_string(key.with_extension("pub")).unwrap()
    );
    git.repositories.register(session.clone(), signers).unwrap();
    let (facts, refusals) = git.repositories.check_signatures(&request).unwrap();
    assert_eq!(facts.commits, std::slice::from_ref(&commit));
    assert!(refusals.is_empty());
    assert_eq!(
        HookService::pre_receive(&git, Request::new(request.clone()))
            .await
            .unwrap_err()
            .code(),
        tonic::Code::Unavailable
    );
    command(&["update-ref", "refs/heads/main", &commit]);
    command(&[
        "-c",
        "gpg.format=ssh",
        "-c",
        &format!("user.signingKey={}", key.display()),
        "tag",
        "-s",
        "v1",
        &commit,
        "-m",
        "release",
    ]);
    command(&["tag", "-a", "unsigned", &commit, "-m", "release"]);
    let tag = command(&["rev-parse", "refs/tags/v1"]);
    let unsigned = command(&["rev-parse", "refs/tags/unsigned"]);
    let raw = command(&["cat-file", "tag", &tag]);
    let tampered_file = temp.0.join("tampered-tag");
    std::fs::write(
        &tampered_file,
        format!("{}\n", raw.replace("release", "tampered")),
    )
    .unwrap();
    let tampered = command(&[
        "hash-object",
        "-t",
        "tag",
        "-w",
        tampered_file.to_str().unwrap(),
    ]);
    let other_key = temp.0.join("other-key");
    let output = std::process::Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-f"])
        .arg(&other_key)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    command(&[
        "-c",
        "gpg.format=ssh",
        "-c",
        &format!("user.signingKey={}", other_key.display()),
        "tag",
        "-s",
        "wrong",
        &commit,
        "-m",
        "release",
    ]);
    let wrong_signer = command(&["rev-parse", "refs/tags/wrong"]);
    let mut tag_request = request.clone();
    tag_request.updates[0].ref_name = "refs/tags/release".into();
    for old in ["0000000000000000000000000000000000000000", tag.as_str()] {
        for (new, accepted) in [
            (&tag, true),
            (&commit, false),
            (&unsigned, false),
            (&tampered, false),
            (&wrong_signer, false),
        ] {
            tag_request.updates[0].old_id = old.into();
            tag_request.updates[0].new_id = new.clone();
            let (facts, refusals) = git.repositories.check_signatures(&tag_request).unwrap();
            assert!(facts.commits.is_empty());
            assert_eq!(refusals.is_empty(), accepted, "{refusals:?}");
            assert_eq!(
                HookService::pre_receive(&git, Request::new(tag_request.clone()))
                    .await
                    .unwrap_err()
                    .code(),
                tonic::Code::Unavailable
            );
            if !accepted {
                assert_eq!(refusals.len(), 1);
                assert_eq!(refusals[0].ref_name, "refs/tags/release");
                assert!(refusals[0].reason.contains(&format!("tag {new}:")));
                assert!(refusals[0].reason.contains("SSH signature"));
            }
        }
    }
    // A deletion never needs an object or a signing file; task 17 owns its permission.
    tag_request.updates[0].old_id = "1111111111111111111111111111111111111111".into();
    tag_request.updates[0].new_id = "0000000000000000000000000000000000000000".into();
    std::fs::rename(temp.0.join("tmp"), temp.0.join("tmp-held")).unwrap();
    let (facts, refusals) = git.repositories.check_signatures(&tag_request).unwrap();
    assert!(facts.commits.is_empty());
    assert!(refusals.is_empty());
    std::fs::rename(temp.0.join("tmp-held"), temp.0.join("tmp")).unwrap();
    command(&["update-ref", "-d", "refs/heads/main"]);
    command(&["update-ref", "-d", "refs/tags/v1"]);
    command(&["update-ref", "-d", "refs/tags/unsigned"]);
    command(&["update-ref", "-d", "refs/tags/wrong"]);
    let mut wrong = request.clone();
    wrong.user_public_id = kajiba_core::new_id().unwrap();
    assert_eq!(
        git.repositories
            .check_signatures(&wrong)
            .unwrap_err()
            .code(),
        tonic::Code::FailedPrecondition
    );
    wrong = request.clone();
    wrong.repo_public_id = kajiba_core::new_id().unwrap();
    assert_eq!(
        git.repositories
            .check_signatures(&wrong)
            .unwrap_err()
            .code(),
        tonic::Code::FailedPrecondition
    );
    git.repositories.finished(&id, 0);
    assert_eq!(
        git.repositories
            .check_signatures(&request)
            .unwrap_err()
            .code(),
        tonic::Code::FailedPrecondition
    );
    request.session_id = kajiba_core::new_id().unwrap();
    git.repositories
        .register(
            Session {
                session_id: request.session_id.clone(),
                ..session
            },
            String::new(),
        )
        .unwrap();
    let (_, refusals) = git.repositories.check_signatures(&request).unwrap();
    assert_eq!(refusals[0].ref_name, "refs/heads/main");
    assert!(refusals[0].reason.contains(&commit));
    assert!(refusals[0].reason.contains("SSH signature"));
    assert_eq!(
        HookService::pre_receive(&git, Request::new(request))
            .await
            .unwrap_err()
            .code(),
        tonic::Code::Unavailable
    );
    assert_eq!(std::fs::read_dir(temp.0.join("tmp")).unwrap().count(), 1); // orphans/ only
}

mod admission;
