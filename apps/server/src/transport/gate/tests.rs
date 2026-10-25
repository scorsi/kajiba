use super::*;
use crate::transport::{local, local::tests::PUBLIC_URL, tests::State};
use kajiba_core::local::{channel, effective_uid};
use kajiba_proto::gate::v1::gate_service_client::GateServiceClient;
use std::{fs, os::unix::fs::MetadataExt, time::Duration};
use tonic::Code;
use tonic_types::StatusExt;

#[tokio::test]
async fn hello_checks_exact_version_before_recording_a_canonical_generation() {
    let state = State::new();
    let admin = local::tests::admin(&state.health.state_dir, "").await;
    let gate = Gate::new(&admin);
    let generation = kajiba_core::new_id().unwrap();
    for version in [
        String::new(),
        format!("{}-other", env!("CARGO_PKG_VERSION")),
        format!("{}+build", env!("CARGO_PKG_VERSION")),
    ] {
        let err = gate
            .hello(Request::new(HelloRequest {
                version: version.clone(),
                generation: generation.clone(),
            }))
            .await
            .unwrap_err();
        assert_eq!(err.code(), Code::FailedPrecondition);
        let details = err.get_error_details();
        let info = details.error_info().unwrap();
        assert_eq!(info.reason, "VERSION_INCOMPATIBLE");
        assert_eq!(info.domain, "kajiba");
        assert_eq!(info.metadata["request_id"].len(), 26);
        assert!(err.message().contains(&version));
        assert!(err.message().contains(env!("CARGO_PKG_VERSION")));
        assert!(gate.generation.lock().unwrap().is_none());
    }
    for invalid in [
        "",
        "../bad",
        "8ZZZZZZZZZZZZZZZZZZZZZZZZZ",
        "01arz3ndektsv4rrffq69g5fav",
    ] {
        let err = gate
            .hello(Request::new(HelloRequest {
                version: env!("CARGO_PKG_VERSION").into(),
                generation: invalid.into(),
            }))
            .await
            .unwrap_err();
        assert_eq!(err.code(), Code::InvalidArgument);
        assert_eq!(
            err.get_error_details()
                .bad_request()
                .unwrap()
                .field_violations[0]
                .field,
            "generation"
        );
        assert!(gate.generation.lock().unwrap().is_none());
    }
    let hello = gate
        .hello(Request::new(HelloRequest {
            version: env!("CARGO_PKG_VERSION").into(),
            generation: generation.clone(),
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(hello.version, env!("CARGO_PKG_VERSION"));
    assert_eq!(
        gate.generation.lock().unwrap().as_deref(),
        Some(generation.as_str())
    );
}

#[tokio::test]
async fn git_socket_has_its_own_peer_policy_mode_and_services() {
    let state = State::new();
    let dir = &state.health.state_dir;
    let path = dir.join("git.sock");
    let gid = fs::metadata(dir).unwrap().gid();
    for uid in [effective_uid(), effective_uid().wrapping_add(1)] {
        let local = bind(&path, Arc::default(), uid, gid).unwrap();
        let metadata = fs::metadata(&path).unwrap();
        assert_eq!(metadata.mode() & 0o777, 0o660);
        assert_eq!(metadata.uid(), effective_uid());
        assert_eq!(metadata.gid(), gid);
        let admin = local::tests::admin(&dir.join("db"), "").await;
        let gate = Gate::new(&admin);
        let server = tokio::spawn(serve(local, gate, Arc::default(), std::future::pending()));
        let result = tokio::time::timeout(Duration::from_secs(2), async {
            let channel = channel(&path, Some(Duration::from_secs(1)))
                .await
                .map_err(|err| err.to_string())?;
            GateServiceClient::new(channel)
                .hello(HelloRequest {
                    version: env!("CARGO_PKG_VERSION").into(),
                    generation: kajiba_core::new_id().unwrap(),
                })
                .await
                .map_err(|err| err.to_string())
        })
        .await
        .unwrap();
        assert_eq!(result.is_ok(), uid == effective_uid(), "{result:?}");
        if uid == effective_uid() {
            let err = local::enroll(&path, "alice".into()).await.unwrap_err();
            assert_eq!(
                err.downcast_ref::<Status>().unwrap().code(),
                Code::Unimplemented
            );
        }
        server.abort();
        assert!(server.await.unwrap_err().is_cancelled());
        assert!(!path.exists());
    }
}

#[tokio::test]
async fn gate_auth_and_closed_hooks_leave_admin_socket_unchanged() {
    const KEY: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAINdamAGCsQq31Uv+08lkBzoO4XLz2qYjJa8CGmj3B1Ea";
    let state = State::new();
    let dir = &state.health.state_dir;
    let admin = local::tests::admin(
        dir,
        &format!("[users.alice]\nssh_keys=['{KEY} ignored comment']"),
    )
    .await;
    let gate = Gate::new(&admin);
    let admin_path = dir.join("local.sock");
    let admin_server = tokio::spawn(local::serve(
        local::bind(&admin_path, Arc::default()).unwrap(),
        admin,
        Arc::default(),
        std::future::pending(),
    ));
    let gate_path = dir.join("git.sock");
    let gate_local = bind(
        &gate_path,
        Arc::default(),
        effective_uid(),
        fs::metadata(dir).unwrap().gid(),
    )
    .unwrap();
    let gate_server = tokio::spawn(serve(
        gate_local,
        gate,
        Arc::default(),
        std::future::pending(),
    ));
    let mut client = GateServiceClient::new(
        channel(&gate_path, Some(Duration::from_secs(2)))
            .await
            .unwrap(),
    );
    let found = client
        .get_auth_key(GetAuthKeyRequest {
            fingerprint: "SHA256:bbXpuKG6zhzdmnxq256TlqzFBzRl2f6OOg722cYNbU8".into(),
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(found.public_key, KEY);
    assert!(kajiba_core::names::is_public_id(&found.user_public_id));
    let err = client
        .get_auth_key(GetAuthKeyRequest {
            fingerprint: "missing".into(),
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::NotFound);
    assert_eq!(
        err.get_error_details().error_info().unwrap().reason,
        "KEY_NOT_FOUND"
    );
    assert!(
        !client
            .pre_receive(PreReceiveRequest::default())
            .await
            .unwrap()
            .into_inner()
            .allowed
    );
    client
        .post_receive(PostReceiveRequest::default())
        .await
        .unwrap();
    assert_eq!(
        client
            .open_session(OpenSessionRequest::default())
            .await
            .unwrap_err()
            .code(),
        Code::InvalidArgument
    );
    assert_eq!(fs::metadata(&admin_path).unwrap().mode() & 0o777, 0o600);
    assert!(
        local::enroll(&admin_path, "alice".into())
            .await
            .unwrap()
            .contains("/auth/enroll#")
    );
    let mut hooks = kajiba_proto::hook::v1::hook_service_client::HookServiceClient::new(
        channel(&admin_path, None).await.unwrap(),
    );
    assert_eq!(
        hooks
            .get_auth_key(kajiba_proto::hook::v1::GetAuthKeyRequest::default())
            .await
            .unwrap_err()
            .code(),
        Code::Unimplemented
    );
    assert_eq!(
        hooks
            .pre_receive(kajiba_proto::hook::v1::PreReceiveRequest::default())
            .await
            .unwrap_err()
            .code(),
        Code::Unimplemented
    );
    assert_eq!(
        hooks
            .post_receive(kajiba_proto::hook::v1::PostReceiveRequest::default())
            .await
            .unwrap_err()
            .code(),
        Code::Unimplemented
    );
    let mut wrong = GateServiceClient::new(
        channel(&admin_path, Some(Duration::from_secs(2)))
            .await
            .unwrap(),
    );
    assert_eq!(
        wrong
            .hello(HelloRequest::default())
            .await
            .unwrap_err()
            .code(),
        Code::Unimplemented
    );
    admin_server.abort();
    gate_server.abort();
    let _ = admin_server.await;
    let _ = gate_server.await;
}

async fn session_fixture() -> (State, local::Admin, Gate, String) {
    let state = State::new();
    let admin = local::tests::admin(&state.health.state_dir,
        "[users.alice]\n[users.bob]\n[repos.demo]\nowner='alice'\ndescription=''\nprevious_names=['alice/old']\n").await;
    let config = kajiba_core::config::ServerConfig::parse("[http]\npublic_url='https://example.org'\n[users.alice]\n[users.bob]\n[repos.demo]\nowner='alice'\ndescription=''\nprevious_names=['alice/old']\n").unwrap();
    let mut daemon = crate::transport::git_side::fake::Fake::start(&admin.state_dir).await;
    crate::db::repos::reconcile_repos(
        &mut *admin.writer.lock().await,
        &admin.state_dir,
        &mut daemon.client,
        &config.repos,
    )
    .await
    .unwrap()
    .committed();
    let gate = Gate::new(&admin);
    let generation = kajiba_core::new_id().unwrap();
    gate.admission
        .rebuild(&mut *admin.writer.lock().await, &generation, vec![])
        .await
        .unwrap();
    gate.hello(Request::new(HelloRequest {
        version: env!("CARGO_PKG_VERSION").into(),
        generation,
    }))
    .await
    .unwrap();
    let alice: String = sqlx::query_scalar("SELECT public_id FROM user WHERE name = 'alice'")
        .fetch_one(&mut *admin.writer.lock().await)
        .await
        .unwrap();
    (state, admin, gate, alice)
}

#[tokio::test]
async fn sessions_resolve_redirects_check_roles_and_refuse_archives() {
    use kajiba_proto::git::v1::SessionKind;
    let (_state, admin, gate, alice) = session_fixture().await;
    let request = OpenSessionRequest {
        user_public_id: alice,
        kind: SessionKind::ReceivePack.into(),
        repository: Some(open_session_request::Repository::RepoAddress(
            "/alice/old.git".into(),
        )),
        correlation_id: "session-request".into(),
        import_session_id: String::new(),
    };
    let opened = gate
        .open_session(Request::new(request.clone()))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(opened.role, Role::Admin as i32);
    assert_eq!(opened.default_branch, "main");
    assert!(gate.admission.running(1));
    assert_eq!(
        gate.open_session(Request::new(request.clone()))
            .await
            .unwrap_err()
            .code(),
        Code::FailedPrecondition
    );
    let exit = SessionExitedRequest {
        session_id: opened.session.unwrap().session_id,
        exit_status: 0,
    };
    for _ in 0..2 {
        gate.session_exited(Request::new(exit.clone()))
            .await
            .unwrap();
    }
    assert!(!gate.admission.running(1));
    let bob: String = sqlx::query_scalar("SELECT public_id FROM user WHERE name = 'bob'")
        .fetch_one(&mut *admin.writer.lock().await)
        .await
        .unwrap();
    let mut read_only = request.clone();
    read_only.user_public_id = bob;
    assert_eq!(
        gate.open_session(Request::new(read_only.clone()))
            .await
            .unwrap_err()
            .code(),
        Code::PermissionDenied
    );
    read_only.kind = SessionKind::UploadPack.into();
    let read = gate
        .open_session(Request::new(read_only.clone()))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(read.role, Role::Read as i32);
    gate.session_exited(Request::new(SessionExitedRequest {
        session_id: read.session.unwrap().session_id,
        exit_status: 1,
    }))
    .await
    .unwrap();
    sqlx::query("UPDATE repo SET archived_at = 1")
        .execute(&mut *admin.writer.lock().await)
        .await
        .unwrap();
    for r in [request, read_only] {
        let err = gate.open_session(Request::new(r)).await.unwrap_err();
        assert_eq!(err.code(), Code::FailedPrecondition);
        assert!(err.message().contains("archived"));
    }
}

#[tokio::test]
async fn import_open_requires_the_exact_durable_reservation() {
    use kajiba_proto::git::v1::SessionKind;
    let (_state, admin, gate, _) = session_fixture().await;
    let id: String = sqlx::query_scalar("SELECT public_id FROM repo")
        .fetch_one(&mut *admin.writer.lock().await)
        .await
        .unwrap();
    let session = kajiba_core::new_id().unwrap();
    let mut writer = admin.writer.lock().await;
    gate.admission
        .admit(&mut writer, 1, None, "request", &[])
        .await
        .unwrap();
    sqlx::query("UPDATE pending_push SET git_session_id = ?")
        .bind(&session)
        .execute(&mut *writer)
        .await
        .unwrap();
    drop(writer);
    let request = OpenSessionRequest {
        user_public_id: String::new(),
        kind: SessionKind::Import.into(),
        repository: Some(open_session_request::Repository::RepoPublicId(id)),
        correlation_id: "request".into(),
        import_session_id: session.clone(),
    };
    let mut stale = request.clone();
    stale.import_session_id = kajiba_core::new_id().unwrap();
    assert_eq!(
        gate.open_session(Request::new(stale))
            .await
            .unwrap_err()
            .code(),
        Code::FailedPrecondition
    );
    let opened = gate
        .open_session(Request::new(request.clone()))
        .await
        .unwrap()
        .into_inner()
        .session
        .unwrap();
    assert_eq!(opened.session_id, session);
    let recorded: String = sqlx::query_scalar("SELECT git_generation FROM pending_push")
        .fetch_one(&mut *admin.writer.lock().await)
        .await
        .unwrap();
    assert_eq!(recorded, opened.generation);
    assert_eq!(
        gate.open_session(Request::new(request.clone()))
            .await
            .unwrap_err()
            .code(),
        Code::FailedPrecondition
    );
    gate.session_exited(Request::new(SessionExitedRequest {
        session_id: session,
        exit_status: 1,
    }))
    .await
    .unwrap();
    assert_eq!(
        gate.open_session(Request::new(request))
            .await
            .unwrap_err()
            .code(),
        Code::FailedPrecondition
    );
}

/// The child processes give the test independent server and fetch lifetimes.
#[tokio::test]
async fn controlled_process_crash_recovery() {
    use crate::db::pushes::{RefUpdate, ZERO};
    use kajiba_gitstore::{GitStore, Gix};
    use kajiba_proto::git::v1::{Session, SessionKind};
    use std::{
        io::{BufRead, Write},
        process::{Command, Stdio},
    };
    if let Ok(role) = std::env::var("KAJIBA_TEST_SESSION_ROLE") {
        let dir = std::path::PathBuf::from(std::env::var_os("KAJIBA_TEST_SESSION_DIR").unwrap());
        if role == "server" {
            let (mut writer, _) = crate::db::open(&dir.join("kajiba.sqlite")).await.unwrap();
            let admission = crate::db::pushes::Gate::default();
            admission
                .admit(
                    &mut writer,
                    1,
                    None,
                    "crash",
                    &[RefUpdate {
                        name: "refs/heads/main".into(),
                        old: ZERO.into(),
                        new: std::env::var("KAJIBA_TEST_TIP").unwrap(),
                    }],
                )
                .await
                .unwrap();
            sqlx::query(
                "UPDATE pending_push SET git_session_id = ?, git_generation = ?, created_at = 0",
            )
            .bind(std::env::var("KAJIBA_TEST_SESSION").unwrap())
            .bind(std::env::var("KAJIBA_TEST_GENERATION").unwrap())
            .execute(&mut writer)
            .await
            .unwrap();
        } else {
            let source = dir.join("source.git");
            let target = dir.join(format!(
                "repos/{}.git",
                std::env::var("KAJIBA_TEST_REPO").unwrap()
            ));
            let refs = Gix.remote_refs(source.to_str().unwrap(), "").unwrap();
            Gix.fetch(&target, source.to_str().unwrap(), "", &refs)
                .unwrap();
        }
        println!("CHILD_READY");
        std::io::stdout().flush().unwrap();
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).unwrap();
        return;
    }
    fn ready(child: &mut std::process::Child) {
        let output = child.stdout.take().unwrap();
        let mut reader = std::io::BufReader::new(output);
        let mut line = String::new();
        loop {
            assert!(reader.read_line(&mut line).unwrap() > 0);
            if line.trim() == "CHILD_READY" {
                break;
            }
            line.clear();
        }
        child.stdout = Some(reader.into_inner());
    }
    fn git(path: &Path, args: &[&str], input: &str) -> String {
        let mut child = Command::new("git")
            .arg("--git-dir")
            .arg(path)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@example.org")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@example.org")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success());
        String::from_utf8(output.stdout).unwrap().trim().into()
    }
    for crash in ["server", "daemon", "both"] {
        let (_state, mut admin, _, _) = session_fixture().await;
        let source = admin.state_dir.join("source.git");
        Gix.init_bare(&source, "main").unwrap();
        let tree = git(&source, &["mktree"], "");
        let tip = git(&source, &["commit-tree", &tree, "-m", "imported"], "");
        git(&source, &["update-ref", "refs/heads/main", &tip], "");
        let repo: String = sqlx::query_scalar("SELECT public_id FROM repo")
            .fetch_one(&mut *admin.writer.lock().await)
            .await
            .unwrap();
        let session = Session {
            session_id: kajiba_core::new_id().unwrap(),
            generation: kajiba_core::new_id().unwrap(),
            kind: SessionKind::Import.into(),
            repo_public_id: repo.clone(),
            user_public_id: String::new(),
            correlation_id: "crash".into(),
        };
        let start = |role: &str| {
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "transport::gate::tests::controlled_process_crash_recovery",
                    "--nocapture",
                ])
                .env("KAJIBA_TEST_SESSION_ROLE", role)
                .env("KAJIBA_TEST_SESSION_DIR", &admin.state_dir)
                .env("KAJIBA_TEST_SESSION", &session.session_id)
                .env("KAJIBA_TEST_GENERATION", &session.generation)
                .env("KAJIBA_TEST_TIP", &tip)
                .env("KAJIBA_TEST_REPO", &repo)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .unwrap()
        };
        let mut server = start("server");
        ready(&mut server);
        let mut daemon = start("daemon");
        ready(&mut daemon);
        assert!(daemon.try_wait().unwrap().is_none());
        if crash != "daemon" {
            server.kill().unwrap();
            server.wait().unwrap();
        }
        if crash != "server" {
            daemon.kill().unwrap();
            daemon.wait().unwrap();
        }
        // Reopen the durable database with a fresh gate: server process memory is gone.
        let (writer, _) = crate::db::open(&admin.state_dir.join("kajiba.sqlite"))
            .await
            .unwrap();
        *admin.writer.lock().await = writer;
        admin.gate = Arc::default();
        let gate = Gate::new(&admin);
        let generation = if crash == "server" {
            session.generation.clone()
        } else {
            kajiba_core::new_id().unwrap()
        };
        let snapshot = if crash == "server" {
            vec![session.clone()]
        } else {
            vec![]
        };
        gate.admission
            .rebuild(&mut *admin.writer.lock().await, &generation, snapshot)
            .await
            .unwrap();
        let count = async || -> i64 {
            sqlx::query_scalar("SELECT count(*) FROM event WHERE kind = 'push'")
                .fetch_one(&mut *admin.writer.lock().await)
                .await
                .unwrap()
        };
        gate.admission
            .recover_once(
                &mut *admin.writer.lock().await,
                &admin.bus,
                &admin.state_dir,
                &Gix,
            )
            .await;
        if crash == "server" {
            assert_eq!(
                count().await,
                0,
                "old row and restarted server are not proof of exit"
            );
            assert!(daemon.try_wait().unwrap().is_none());
            // Even an empty same-generation snapshot cannot replace SessionExited.
            gate.admission
                .rebuild(&mut *admin.writer.lock().await, &generation, vec![])
                .await
                .unwrap();
            gate.admission
                .recover_once(
                    &mut *admin.writer.lock().await,
                    &admin.bus,
                    &admin.state_dir,
                    &Gix,
                )
                .await;
            assert_eq!(count().await, 0);
            daemon.stdin.take().unwrap().write_all(b"finish\n").unwrap();
            assert!(daemon.wait().unwrap().success());
        }
        let exit = SessionExitedRequest {
            session_id: session.session_id.clone(),
            exit_status: 0,
        };
        for _ in 0..2 {
            gate.session_exited(Request::new(exit.clone()))
                .await
                .unwrap();
            gate.admission
                .recover_once(
                    &mut *admin.writer.lock().await,
                    &admin.bus,
                    &admin.state_dir,
                    &Gix,
                )
                .await;
            assert_eq!(count().await, 1, "{crash}: exactly one event");
        }
        let pending: i64 = sqlx::query_scalar("SELECT count(*) FROM pending_push")
            .fetch_one(&mut *admin.writer.lock().await)
            .await
            .unwrap();
        assert_eq!(pending, 0);
        if crash == "daemon" {
            server.kill().unwrap();
            server.wait().unwrap();
        }
    }
}

#[tokio::test]
async fn an_unstarted_reservation_is_recovered_and_a_late_fetch_is_refused() {
    use kajiba_proto::git::v1::SessionKind;
    let (_state, mut admin, _, _) = session_fixture().await;
    let session_id = kajiba_core::new_id().unwrap();
    admin
        .gate
        .admit(&mut *admin.writer.lock().await, 1, None, "not-sent", &[])
        .await
        .unwrap();
    sqlx::query("UPDATE pending_push SET git_session_id = ?")
        .bind(&session_id)
        .execute(&mut *admin.writer.lock().await)
        .await
        .unwrap();
    admin.gate = Arc::default();
    let gate = Gate::new(&admin);
    let generation = kajiba_core::new_id().unwrap();
    gate.admission
        .rebuild(&mut *admin.writer.lock().await, &generation, vec![])
        .await
        .unwrap();
    gate.admission
        .recover_once(
            &mut *admin.writer.lock().await,
            &admin.bus,
            &admin.state_dir,
            &kajiba_gitstore::Gix,
        )
        .await;
    gate.hello(Request::new(HelloRequest {
        version: env!("CARGO_PKG_VERSION").into(),
        generation,
    }))
    .await
    .unwrap();
    let err = gate
        .open_session(Request::new(OpenSessionRequest {
            user_public_id: String::new(),
            kind: SessionKind::Import.into(),
            repository: Some(open_session_request::Repository::RepoAddress(
                "alice/demo".into(),
            )),
            correlation_id: "not-sent".into(),
            import_session_id: session_id,
        }))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::FailedPrecondition);
    let pending: i64 = sqlx::query_scalar("SELECT count(*) FROM pending_push")
        .fetch_one(&mut *admin.writer.lock().await)
        .await
        .unwrap();
    assert_eq!(pending, 0);
}

const AUTH: &str =
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAINdamAGCsQq31Uv+08lkBzoO4XLz2qYjJa8CGmj3B1Ea";
const AUTH_FP: &str = "SHA256:bbXpuKG6zhzdmnxq256TlqzFBzRl2f6OOg722cYNbU8";
const SIGNING: &str =
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAID1AF8PoQ4lakrcKp00bfrycmCzPLsSWjMDNVfEq9GYM";
const SIGNING_FP: &str = "SHA256:F34nin7tcaYH6WR5LSWSfj6weFBPfBpuyUUoPFP9YjA";

#[tokio::test]
async fn auth_key_lookup_resolves_only_active_users_auth_keys() {
    let dir = State::new();
    let socket = dir.health.state_dir.join("auth.sock");
    let listener = Local::bind(&socket, Arc::default(), 0o660, vec![effective_uid()]).unwrap();
    // The comment tries to smuggle options into the authorized_keys line.
    let alice = format!(
        "[users.alice]\nssh_keys=['{AUTH} x\",command=\"sh\" restrict']\nsigning_keys=['{SIGNING}']\n[users.bob]\n"
    );
    let admin = local::tests::admin(&dir.health.state_dir, &alice).await;
    let writer = admin.writer.clone();
    let server = tokio::spawn(serve(
        listener,
        Gate::new(&admin),
        Arc::default(),
        std::future::pending(),
    ));
    let public_id = |name: &'static str| {
        let writer = writer.clone();
        async move {
            sqlx::query_scalar::<_, String>("SELECT public_id FROM user WHERE name = ?")
                .bind(name)
                .fetch_one(&mut *writer.lock().await)
                .await
                .unwrap()
        }
    };
    let reconcile = |users: String| {
        let writer = writer.clone();
        async move {
            let users = kajiba_core::config::ServerConfig::parse(&format!(
                "[http]\npublic_url='{PUBLIC_URL}'\n{users}"
            ))
            .unwrap()
            .users;
            crate::db::reconcile(&mut *writer.lock().await, &users)
                .await
                .unwrap();
        }
    };
    let lookup = |fingerprint: &str| {
        let socket = socket.clone();
        let fingerprint = fingerprint.to_owned();
        async move {
            let mut client = GateServiceClient::new(channel(&socket, None).await.unwrap());
            match client.get_auth_key(GetAuthKeyRequest { fingerprint }).await {
                Ok(response) => {
                    let response = response.into_inner();
                    Ok(Some((response.user_public_id, response.public_key)))
                }
                Err(status) if status.code() == Code::NotFound => {
                    assert_eq!(
                        status.get_error_details().error_info().unwrap().reason,
                        "KEY_NOT_FOUND"
                    );
                    Ok(None)
                }
                Err(status) => Err(status),
            }
        }
    };

    assert_eq!(
        lookup(AUTH_FP).await.unwrap(),
        Some((public_id("alice").await, AUTH.into()))
    );
    for unknown in [SIGNING_FP, "SHA256:unknown", ""] {
        assert_eq!(lookup(unknown).await.unwrap(), None, "{unknown}");
    }
    // Reassigned to bob: resolves to him.
    reconcile(format!("[users.alice]\n[users.bob]\nssh_keys=['{AUTH}']\n")).await;
    assert_eq!(
        lookup(AUTH_FP).await.unwrap(),
        Some((public_id("bob").await, AUTH.into()))
    );
    // Removed key, then a disabled user still holding a key row.
    reconcile("[users.alice]\n[users.bob]\n".into()).await;
    assert_eq!(lookup(AUTH_FP).await.unwrap(), None);
    reconcile(format!("[users.bob]\nssh_keys=['{AUTH}']\n")).await;
    sqlx::query("UPDATE user SET disabled_at = 1 WHERE name = 'bob'")
        .execute(&mut *writer.lock().await)
        .await
        .unwrap();
    assert_eq!(lookup(AUTH_FP).await.unwrap(), None);
    server.abort();
}
