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
    let gate = Gate::new(&admin, Default::default());
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
        let gate = Gate::new(&admin, Default::default());
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
    let gate = Gate::new(&admin, Default::default());
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
        .post_receive(PostReceiveRequest {
            session_id: kajiba_core::new_id().unwrap(),
        })
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
            .contains("/-/enroll#")
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
    let gate = Gate::new(&admin, Default::default());
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
async fn session_before_snapshot_waits_then_proceeds() {
    let (_state, admin, gate, alice) = session_fixture().await;
    let generation = kajiba_core::new_id().unwrap();
    gate.hello(Request::new(HelloRequest {
        version: env!("CARGO_PKG_VERSION").into(),
        generation: generation.clone(),
    }))
    .await
    .unwrap();
    let request = OpenSessionRequest {
        generation: generation.clone(),
        user_public_id: alice,
        kind: kajiba_proto::git::v1::SessionKind::ReceivePack.into(),
        repository: Some(open_session_request::Repository::RepoAddress(
            "alice/demo".into(),
        )),
        session_id: kajiba_core::new_id().unwrap(),
        correlation_id: "before-snapshot".into(),
    };
    let open = gate.open_session(Request::new(request.clone()));
    tokio::pin!(open);
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut open)
            .await
            .is_err()
    );
    assert!(gate.admission.session(&request.session_id).is_none());
    // Acquiring the writer here also proves the waiter releases it.
    tokio::time::timeout(Duration::from_secs(1), async {
        gate.admission
            .rebuild(&mut *admin.writer.lock().await, &generation, vec![])
            .await
            .unwrap();
        let opened = open.await.unwrap().into_inner().session.unwrap();
        assert_eq!(opened.session_id, request.session_id);
        assert_eq!(opened.generation, generation);
    })
    .await
    .unwrap();
    assert!(gate.admission.session(&request.session_id).is_some());
}

#[tokio::test]
async fn session_before_snapshot_times_out_cleanly() {
    let (_state, admin, gate, alice) = session_fixture().await;
    let generation = kajiba_core::new_id().unwrap();
    gate.hello(Request::new(HelloRequest {
        version: env!("CARGO_PKG_VERSION").into(),
        generation: generation.clone(),
    }))
    .await
    .unwrap();
    let request = OpenSessionRequest {
        generation: generation.clone(),
        user_public_id: alice,
        kind: kajiba_proto::git::v1::SessionKind::ReceivePack.into(),
        repository: Some(open_session_request::Repository::RepoAddress(
            "alice/demo".into(),
        )),
        session_id: kajiba_core::new_id().unwrap(),
        correlation_id: "missing-snapshot".into(),
    };
    let start = tokio::time::Instant::now();
    let err = tokio::time::timeout(
        Duration::from_secs(4),
        gate.open_session(Request::new(request.clone())),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert!(start.elapsed() >= Duration::from_secs(3));
    assert_eq!(err.code(), Code::Unavailable);
    assert_eq!(
        err.message(),
        "GIT_SIDE_UNAVAILABLE: session snapshot required"
    );
    assert_eq!(
        err.get_error_details().error_info().unwrap().reason,
        "GIT_SIDE_UNAVAILABLE"
    );
    assert!(gate.admission.session(&request.session_id).is_none());
    gate.admission
        .rebuild(&mut *admin.writer.lock().await, &generation, vec![])
        .await
        .unwrap();
    gate.open_session(Request::new(request)).await.unwrap();
}

#[tokio::test]
async fn sessions_resolve_redirects_check_roles_and_refuse_archives() {
    use kajiba_proto::git::v1::SessionKind;
    let (_state, admin, gate, alice) = session_fixture().await;
    let request = OpenSessionRequest {
        generation: gate.admission.generation().unwrap(),
        user_public_id: alice,
        kind: SessionKind::ReceivePack.into(),
        repository: Some(open_session_request::Repository::RepoAddress(
            "/alice/old.git".into(),
        )),
        correlation_id: "session-request".into(),
        session_id: kajiba_core::new_id().unwrap(),
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
            .unwrap()
            .into_inner()
            .session,
        opened.session.clone()
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
    read_only.session_id = kajiba_core::new_id().unwrap();
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
    for mut r in [request, read_only] {
        r.session_id = kajiba_core::new_id().unwrap();
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
        .admit(&mut writer, 1, None, "request", &[], Some(&session))
        .await
        .unwrap();
    drop(writer);
    let request = OpenSessionRequest {
        generation: gate.admission.generation().unwrap(),
        user_public_id: String::new(),
        kind: SessionKind::Import.into(),
        repository: Some(open_session_request::Repository::RepoPublicId(id)),
        correlation_id: "request".into(),
        session_id: session.clone(),
    };
    let mut stale = request.clone();
    stale.session_id = kajiba_core::new_id().unwrap();
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
            .unwrap()
            .into_inner()
            .session,
        Some(opened.clone())
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
                    None,
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
        let gate = Gate::new(&admin, Default::default());
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
        .admit(
            &mut *admin.writer.lock().await,
            1,
            None,
            "not-sent",
            &[],
            Some(&session_id),
        )
        .await
        .unwrap();
    admin.gate = Arc::default();
    let gate = Gate::new(&admin, Default::default());
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
            generation: gate.admission.generation().unwrap(),
            user_public_id: String::new(),
            kind: SessionKind::Import.into(),
            repository: Some(open_session_request::Repository::RepoAddress(
                "alice/demo".into(),
            )),
            correlation_id: "not-sent".into(),
            session_id,
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
        Gate::new(&admin, Default::default()),
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

#[tokio::test]
async fn open_session_signers_include_only_the_active_pushers_current_signing_keys() {
    use kajiba_proto::git::v1::SessionKind;
    let (_state, admin, gate, alice) = session_fixture().await;
    for (users, expected) in [
        (
            format!(
                "[users.alice]\nssh_keys=['{AUTH} comment']\nsigning_keys=['{SIGNING} comment']\n[users.bob]\nsigning_keys=['{AUTH}']\n"
            ),
            format!("{alice} {SIGNING}\n"),
        ),
        // Removing the signing key, or moving it to another user, grants no signing authority.
        (
            format!("[users.alice]\nssh_keys=['{SIGNING}']\n[users.bob]\n"),
            String::new(),
        ),
        (
            format!(
                "[users.alice]\nssh_keys=['{AUTH}']\n[users.bob]\nsigning_keys=['{SIGNING}']\n"
            ),
            String::new(),
        ),
    ] {
        let config = kajiba_core::config::ServerConfig::parse(&format!(
            "[http]\npublic_url='{PUBLIC_URL}'\n{users}"
        ))
        .unwrap();
        crate::db::reconcile(&mut *admin.writer.lock().await, &config.users)
            .await
            .unwrap();
        let opened = gate
            .open_session(Request::new(OpenSessionRequest {
                generation: gate.admission.generation().unwrap(),
                session_id: kajiba_core::new_id().unwrap(),
                user_public_id: alice.clone(),
                kind: SessionKind::ReceivePack.into(),
                repository: Some(open_session_request::Repository::RepoAddress(
                    "alice/demo".into(),
                )),
                ..Default::default()
            }))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(opened.allowed_signers, expected);
        gate.session_exited(Request::new(SessionExitedRequest {
            session_id: opened.session.unwrap().session_id,
            exit_status: 0,
        }))
        .await
        .unwrap();
    }
    let config = kajiba_core::config::ServerConfig::parse(&format!(
        "[http]\npublic_url='{PUBLIC_URL}'\n[users.bob]\n"
    ))
    .unwrap();
    crate::db::reconcile(&mut *admin.writer.lock().await, &config.users)
        .await
        .unwrap();
    assert_eq!(
        gate.open_session(Request::new(OpenSessionRequest {
            generation: gate.admission.generation().unwrap(),
            session_id: kajiba_core::new_id().unwrap(),
            user_public_id: alice,
            kind: SessionKind::ReceivePack.into(),
            repository: Some(open_session_request::Repository::RepoAddress(
                "alice/demo".into()
            )),
            ..Default::default()
        }))
        .await
        .unwrap_err()
        .code(),
        Code::PermissionDenied
    );
}

#[tokio::test]
async fn post_receive_returns_before_exit_and_converts_only_applied_refs_in_order() {
    use crate::db::pushes::ZERO;
    use kajiba_proto::git::v1::SessionKind;
    use std::process::{Command, Stdio};

    const CHILD: &str = "KAJIBA_TEST_PUSH_LOGS";
    if std::env::var_os(CHILD).is_none() {
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "transport::gate::tests::post_receive_returns_before_exit_and_converts_only_applied_refs_in_order", "--nocapture"])
            .env(CHILD, "1").env_remove("RUST_LOG").output().unwrap();
        assert!(output.status.success(), "{output:?}");
        let text = String::from_utf8(output.stderr).unwrap();
        let logs: Vec<serde_json::Value> = text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let pushes: Vec<_> = logs.iter().filter(|line| line["event"] == "push").collect();
        assert_eq!(pushes.len(), 9, "{text}");
        for outcome in ["partial", "unexpected", "archived"] {
            let correlation = format!("push-request-{outcome}");
            let applied: Vec<_> = pushes
                .iter()
                .filter(|line| line["request_id"] == correlation)
                .collect();
            assert_eq!(applied.len(), 3, "{text}");
            for (line, name) in applied.iter().zip(["main", "gone", "new"]) {
                assert_eq!(line["level"], "info");
                assert_eq!(line["repo"], "alice/demo");
                assert_eq!(line["user"], "alice");
                assert_eq!(line["ref"], format!("refs/heads/{name}"));
                for field in ["old", "new"] {
                    assert_eq!(line[field].as_str().unwrap().len(), 40);
                }
            }
        }
        return;
    }
    kajiba_core::log::init("info").unwrap();
    for outcome in ["partial", "failed", "unexpected", "archived"] {
        let (_state, admin, gate, alice) = session_fixture().await;
        let mut open = OpenSessionRequest {
            generation: gate.admission.generation().unwrap(),
            user_public_id: alice.clone(),
            kind: SessionKind::ReceivePack.into(),
            repository: Some(open_session_request::Repository::RepoAddress(
                "alice/demo".into(),
            )),
            correlation_id: format!("push-request-{outcome}"),
            session_id: kajiba_core::new_id().unwrap(),
        };
        let session = gate
            .open_session(Request::new(open.clone()))
            .await
            .unwrap()
            .into_inner()
            .session
            .unwrap();
        let path = admin
            .state_dir
            .join(format!("repos/{}.git", session.repo_public_id));
        let git = |args: &[&str]| {
            let output = Command::new("git")
                .arg("--git-dir")
                .arg(&path)
                .args(args)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_AUTHOR_NAME", "test")
                .env("GIT_AUTHOR_EMAIL", "test@example.org")
                .env("GIT_COMMITTER_NAME", "test")
                .env("GIT_COMMITTER_EMAIL", "test@example.org")
                .stdin(Stdio::null())
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8(output.stdout).unwrap().trim().to_owned()
        };
        let tree = git(&["mktree"]);
        let old = git(&["commit-tree", &tree, "-m", "one"]);
        let new = git(&["commit-tree", &tree, "-p", &old, "-m", "two"]);
        for name in ["main", "gone", "failed-update", "failed-delete", "same"] {
            git(&["update-ref", &format!("refs/heads/{name}"), &old]);
        }
        // Deliberately not ref-name order; post-receive's successful subset is
        // not the durable source of truth for a partially applied push.
        let updates: Vec<_> = [
            ("main", old.as_str(), new.as_str()),
            ("same", old.as_str(), old.as_str()),
            ("gone", old.as_str(), ZERO),
            ("failed-create", ZERO, new.as_str()),
            ("new", ZERO, old.as_str()),
            ("failed-update", old.as_str(), new.as_str()),
            ("failed-delete", old.as_str(), ZERO),
        ]
        .into_iter()
        .map(|(name, old, new)| RefUpdate {
            ref_name: format!("refs/heads/{name}"),
            old_id: old.into(),
            new_id: new.into(),
            fast_forward: Some(true),
            signature: SignatureVerdict::Verified.into(),
            signature_reason: String::new(),
        })
        .collect();
        assert!(
            gate.pre_receive(Request::new(PreReceiveRequest {
                session_id: session.session_id.clone(),
                updates: updates.clone(),
            }))
            .await
            .unwrap()
            .into_inner()
            .allowed
        );

        // A real receive-pack blocked on its command input remains alive while
        // the fixture applies deterministic partial results.
        let mut child = tokio::process::Command::new("git")
            .arg("receive-pack")
            .arg(&path)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        if outcome != "failed" {
            git(&["update-ref", "refs/heads/main", &new]);
            git(&["update-ref", "-d", "refs/heads/gone"]);
            git(&["update-ref", "refs/heads/new", &old]);
        }
        if outcome == "unexpected" {
            git(&["update-ref", "-d", "refs/heads/failed-update"]);
        }
        if outcome == "archived" {
            sqlx::query("UPDATE repo SET archived_at = 1")
                .execute(&mut *admin.writer.lock().await)
                .await
                .unwrap();
        }
        let post = PostReceiveRequest {
            session_id: session.session_id.clone(),
        };
        // Neither the writer lock nor the live child may hold up this hook.
        let mut writer = admin.writer.lock().await;
        for _ in 0..2 {
            tokio::time::timeout(
                Duration::from_secs(2),
                gate.post_receive(Request::new(post.clone())),
            )
            .await
            .unwrap()
            .unwrap();
        }
        assert!(child.try_wait().unwrap().is_none());
        gate.admission
            .recover_once(
                &mut writer,
                &admin.bus,
                &admin.state_dir,
                &kajiba_gitstore::Gix,
            )
            .await;
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM event WHERE kind = 'push'")
            .fetch_one(&mut *writer)
            .await
            .unwrap();
        assert_eq!(count, 0);
        drop(writer);
        open.session_id = kajiba_core::new_id().unwrap();
        assert!(gate.open_session(Request::new(open.clone())).await.is_err());
        child.kill().await.unwrap();
        let exit = SessionExitedRequest {
            session_id: session.session_id.clone(),
            exit_status: 1,
        };
        for _ in 0..2 {
            use tracing::Instrument;
            gate.session_exited(Request::new(exit.clone()))
                .instrument(tracing::info_span!(
                    "request",
                    request_id = "completion-rpc"
                ))
                .await
                .unwrap();
            gate.post_receive(Request::new(post.clone())).await.unwrap();
            gate.admission
                .recover_once(
                    &mut *admin.writer.lock().await,
                    &admin.bus,
                    &admin.state_dir,
                    &kajiba_gitstore::Gix,
                )
                .await;
        }
        if matches!(outcome, "unexpected" | "archived") {
            let mut writer = admin.writer.lock().await;
            let pending: (String, String) =
                sqlx::query_as("SELECT correlation_id, updates FROM pending_push")
                    .fetch_one(&mut *writer)
                    .await
                    .unwrap();
            assert_eq!(pending.0, format!("push-request-{outcome}"));
            assert_eq!(
                serde_json::from_str::<Vec<serde_json::Value>>(&pending.1)
                    .unwrap()
                    .len(),
                updates.len()
            );
            let count: i64 = sqlx::query_scalar("SELECT count(*) FROM event WHERE kind = 'push'")
                .fetch_one(&mut *writer)
                .await
                .unwrap();
            assert_eq!(
                count, 0,
                "{outcome}: keep the entire row without partial events"
            );
            drop(writer);
            assert!(gate.open_session(Request::new(open.clone())).await.is_err());
            if outcome == "unexpected" {
                git(&["update-ref", "refs/heads/failed-update", &old]);
            } else {
                sqlx::query("UPDATE repo SET archived_at = NULL")
                    .execute(&mut *admin.writer.lock().await)
                    .await
                    .unwrap();
            }
            gate.admission
                .recover_once(
                    &mut *admin.writer.lock().await,
                    &admin.bus,
                    &admin.state_dir,
                    &kajiba_gitstore::Gix,
                )
                .await;
        }
        let mut writer = admin.writer.lock().await;
        let events: Vec<(String, i64, i64, String, String)> = sqlx::query_as(
            "SELECT principal_kind, principal_id, repo_id, correlation_id, payload FROM event WHERE kind = 'push' ORDER BY id"
        ).fetch_all(&mut *writer).await.unwrap();
        let principal: i64 = sqlx::query_scalar("SELECT id FROM user WHERE public_id = ?")
            .bind(&alice)
            .fetch_one(&mut *writer)
            .await
            .unwrap();
        let expected: Vec<_> = if outcome == "failed" {
            vec![]
        } else {
            [0, 2, 4].into_iter().map(|i| {
                let update = &updates[i];
                ("user".to_owned(), principal, 1, format!("push-request-{outcome}"),
                    serde_json::json!({"ref": update.ref_name, "old": update.old_id, "new": update.new_id}).to_string())
            }).collect()
        };
        assert_eq!(events, expected, "{outcome}");
        let pending: i64 = sqlx::query_scalar("SELECT count(*) FROM pending_push")
            .fetch_one(&mut *writer)
            .await
            .unwrap();
        assert_eq!(pending, 0);
        drop(writer);
        assert!(gate.open_session(Request::new(open)).await.is_ok());
        let metrics = gate.registry.exposition("test", [0; 4], [0; 4]);
        assert!(metrics.contains("kajiba_pushes_total{result=\"accepted\"} 1\n"));
        let refused = if matches!(outcome, "unexpected" | "archived") {
            2
        } else {
            1
        };
        assert!(metrics.contains(&format!(
            "kajiba_pushes_total{{result=\"refused\"}} {refused}\n"
        )));
    }
}

#[tokio::test]
async fn post_receive_validates_session_and_wakes_recovery_without_a_timer_tick() {
    let (_state, admin, gate, _) = session_fixture().await;
    for session_id in ["", "invalid", "../session"] {
        assert_eq!(
            gate.post_receive(Request::new(PostReceiveRequest {
                session_id: session_id.into()
            }))
            .await
            .unwrap_err()
            .code(),
            Code::InvalidArgument
        );
    }
    // An eligible row proves that PostReceive wakes the production loop, even
    // when the notification arrives before the loop starts waiting.
    gate.admission
        .admit(&mut *admin.writer.lock().await, 1, None, "wake", &[], None)
        .await
        .unwrap();
    gate.admission.exited(1);
    gate.post_receive(Request::new(PostReceiveRequest {
        session_id: kajiba_core::new_id().unwrap(),
    }))
    .await
    .unwrap();
    let recovery = gate.admission.recover_periodically(
        &crate::db::events::SystemClock,
        &admin.writer,
        &admin.bus,
        &admin.state_dir,
        &kajiba_gitstore::Gix,
    );
    tokio::pin!(recovery);
    let converted = async {
        loop {
            let pending: i64 = sqlx::query_scalar("SELECT count(*) FROM pending_push")
                .fetch_one(&mut *admin.writer.lock().await)
                .await
                .unwrap();
            if pending == 0 {
                break;
            }
            tokio::task::yield_now().await;
        }
    };
    tokio::select! {
        () = &mut recovery => unreachable!("recovery runs until shutdown"),
        result = tokio::time::timeout(Duration::from_secs(2), converted) => result.unwrap(),
    }
}

#[tokio::test]
async fn push_refusals_have_correlated_logs_and_count_once() {
    use kajiba_proto::git::v1::SessionKind;
    use tracing::Instrument;
    const CHILD: &str = "KAJIBA_TEST_PUSH_REFUSALS";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "transport::gate::tests::push_refusals_have_correlated_logs_and_count_once",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env_remove("RUST_LOG")
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let text = String::from_utf8(output.stderr).unwrap();
        for secret in [AUTH, SIGNING, "ssh-ed25519", "private database diagnostic"] {
            assert!(!text.contains(secret), "{text}");
        }
        let logs: Vec<serde_json::Value> = text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let refused: Vec<_> = logs
            .iter()
            .filter(|line| line["event"] == "push-refused")
            .collect();
        assert_eq!(refused.len(), 5, "{text}");
        for line in &refused {
            assert_eq!(line["level"], "warn");
            assert_eq!(line["request_id"], "admission-request");
            assert!(!line["repo"].as_str().unwrap().is_empty());
            assert!(line["ref"].is_string());
            assert!(!line["reason"].as_str().unwrap().is_empty());
        }
        assert_eq!(refused[0]["repo"], "alice/demo");
        assert_eq!(refused[0]["ref"], ""); // No ref is known before receive-pack starts.
        assert_eq!(refused[0]["reason"], "repository role required");
        for (line, name) in refused[1..3].iter().zip(["one", "two"]) {
            assert_eq!(line["ref"], format!("refs/heads/{name}"));
            assert_eq!(
                line["reason"],
                "signature verification failed: commit 1111111111111111111111111111111111111111: wrong signer"
            );
        }
        assert_eq!(refused[3]["reason"], "push admission failed");
        assert!(refused[4]["reason"].as_str().unwrap().contains("retry"));
        assert!(!logs.iter().any(|line| line["event"] == "push"));
        return;
    }
    kajiba_core::log::init("info").unwrap();
    let (_state, admin, gate, alice) = session_fixture().await;
    let config = kajiba_core::config::ServerConfig::parse(&format!(
        "[http]\npublic_url='{PUBLIC_URL}'\n[users.alice]\nssh_keys=['{AUTH}']\nsigning_keys=['{SIGNING}']\n[users.bob]\n"
    )).unwrap();
    crate::db::reconcile(&mut *admin.writer.lock().await, &config.users)
        .await
        .unwrap();
    async {
        let mut open = OpenSessionRequest {
            generation: gate.admission.generation().unwrap(),
            session_id: kajiba_core::new_id().unwrap(),
            user_public_id: sqlx::query_scalar("SELECT public_id FROM user WHERE name = 'bob'")
                .fetch_one(&mut *admin.writer.lock().await).await.unwrap(),
            kind: SessionKind::ReceivePack.into(),
            repository: Some(open_session_request::Repository::RepoAddress("alice/demo".into())),
            correlation_id: "admission-request".into(),
        };
        assert_eq!(gate.open_session(Request::new(open.clone())).await.unwrap_err().code(), Code::PermissionDenied);
        // A failed read session must not count as a refused push.
        let mut read = open.clone();
        read.kind = SessionKind::UploadPack.into();
        read.repository = Some(open_session_request::Repository::RepoAddress("alice/missing".into()));
        assert!(gate.open_session(Request::new(read)).await.is_err());
        open.user_public_id = alice;
        let opened = gate.open_session(Request::new(open)).await.unwrap().into_inner();
        assert!(opened.allowed_signers.contains(SIGNING));
        let mut request = PreReceiveRequest {
            session_id: opened.session.unwrap().session_id,
            updates: ["one", "two"].into_iter().map(|name| RefUpdate {
                ref_name: format!("refs/heads/{name}"),
                old_id: crate::db::pushes::ZERO.into(),
                new_id: "1111111111111111111111111111111111111111".into(),
                fast_forward: Some(false),
                signature: SignatureVerdict::Refused.into(),
                signature_reason: "commit 1111111111111111111111111111111111111111: wrong signer".into(),
            }).collect(),
        };
        assert!(!gate.pre_receive(Request::new(request.clone())).await.unwrap().into_inner().allowed);
        for update in &mut request.updates {
            update.signature = SignatureVerdict::Verified.into();
            update.signature_reason.clear();
        }
        sqlx::query("CREATE TEMP TRIGGER fail_admission BEFORE INSERT ON pending_push BEGIN SELECT RAISE(ABORT, 'private database diagnostic'); END")
            .execute(&mut *admin.writer.lock().await).await.unwrap();
        assert_eq!(gate.pre_receive(Request::new(request.clone())).await.unwrap_err().code(), Code::Internal);
        sqlx::query("DROP TRIGGER fail_admission").execute(&mut *admin.writer.lock().await).await.unwrap();
        assert!(gate.pre_receive(Request::new(request.clone())).await.unwrap().into_inner().allowed);
        assert!(!gate.pre_receive(Request::new(request)).await.unwrap().into_inner().allowed);
        let metrics = gate.registry.exposition("test", [0; 4], [0; 4]);
        assert!(metrics.contains("kajiba_pushes_total{result=\"accepted\"} 1\n"));
        assert!(metrics.contains("kajiba_pushes_total{result=\"refused\"} 4\n"));
    }.instrument(tracing::info_span!("request", request_id = "current-rpc")).await;
}

#[tokio::test]
async fn lost_open_response_can_be_retried_or_cancelled_without_leaking_admission() {
    use kajiba_proto::git::v1::SessionKind;
    let (_state, _admin, gate, alice) = session_fixture().await;
    let mut open = OpenSessionRequest {
        generation: gate.admission.generation().unwrap(),
        user_public_id: alice,
        kind: SessionKind::ReceivePack.into(),
        repository: Some(open_session_request::Repository::RepoAddress(
            "alice/demo".into(),
        )),
        session_id: kajiba_core::new_id().unwrap(),
        correlation_id: "lost-response".into(),
    };
    let mut invalid = open.clone();
    invalid.session_id.clear();
    assert_eq!(
        gate.open_session(Request::new(invalid))
            .await
            .unwrap_err()
            .code(),
        Code::InvalidArgument
    );
    let mut stale = open.clone();
    stale.generation = kajiba_core::new_id().unwrap();
    assert_eq!(
        gate.open_session(Request::new(stale))
            .await
            .unwrap_err()
            .code(),
        Code::Unavailable
    );
    // The reply is lost after the server has recorded admission.
    drop(gate.open_session(Request::new(open.clone())).await.unwrap());
    assert!(gate.admission.running(1));
    let retry = gate
        .open_session(Request::new(open.clone()))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(retry.session.unwrap().session_id, open.session_id);
    let mut changed = open.clone();
    changed.kind = SessionKind::UploadPack.into();
    assert_eq!(
        gate.open_session(Request::new(changed))
            .await
            .unwrap_err()
            .code(),
        Code::FailedPrecondition
    );
    let mut competing = open.clone();
    competing.session_id = kajiba_core::new_id().unwrap();
    assert!(
        gate.open_session(Request::new(competing.clone()))
            .await
            .is_err()
    );
    let exit = SessionExitedRequest {
        session_id: open.session_id.clone(),
        exit_status: 1,
    };
    for _ in 0..2 {
        gate.session_exited(Request::new(exit.clone()))
            .await
            .unwrap();
    }
    assert!(!gate.admission.running(1));
    gate.hello(Request::new(HelloRequest {
        version: env!("CARGO_PKG_VERSION").into(),
        generation: open.generation.clone(),
    }))
    .await
    .unwrap();
    assert!(gate.open_session(Request::new(open.clone())).await.is_err());
    // Cancellation can beat a delayed OpenSession to the writer lock.
    open.session_id = kajiba_core::new_id().unwrap();
    gate.session_exited(Request::new(SessionExitedRequest {
        session_id: open.session_id.clone(),
        exit_status: 1,
    }))
    .await
    .unwrap();
    assert!(gate.open_session(Request::new(open)).await.is_err());
    assert!(!gate.admission.running(1));
    gate.open_session(Request::new(competing)).await.unwrap();
    assert!(gate.admission.running(1));
}

#[tokio::test]
async fn session_exit_answers_the_runs_its_push_created() {
    use crate::db::{events::Handler, pushes::ZERO};
    use kajiba_proto::git::v1::SessionKind;
    use std::process::Command;
    let (_state, admin, gate, alice) = session_fixture().await;
    let wake = std::sync::Arc::new(tokio::sync::Notify::new());
    let mut ci = crate::db::runs::Ci {
        now: crate::db::runs::now,
        repos: admin.state_dir.join("repos"),
        wake,
    };
    assert_eq!(<crate::db::runs::Ci as Handler>::NAME, "ci");
    // One push: admitted, applied by a stand-in for receive-pack, then its session exits.
    let push = |ci_mode: &'static str| {
        let (admin, gate, alice) = (&admin, &gate, &alice);
        async move {
            sqlx::query("UPDATE repo SET ci_mode = ?")
                .bind(ci_mode)
                .execute(&mut *admin.writer.lock().await)
                .await
                .unwrap();
            let session = gate
                .open_session(Request::new(OpenSessionRequest {
                    generation: gate.admission.generation().unwrap(),
                    user_public_id: alice.clone(),
                    kind: SessionKind::ReceivePack.into(),
                    repository: Some(open_session_request::Repository::RepoAddress(
                        "alice/demo".into(),
                    )),
                    correlation_id: kajiba_core::new_id().unwrap(),
                    session_id: kajiba_core::new_id().unwrap(),
                }))
                .await
                .unwrap()
                .into_inner()
                .session
                .unwrap();
            let path = admin
                .state_dir
                .join(format!("repos/{}.git", session.repo_public_id));
            let git = |args: &[&str]| {
                let output = Command::new("git")
                    .arg("--git-dir")
                    .arg(&path)
                    .args(args)
                    .env("GIT_CONFIG_GLOBAL", "/dev/null")
                    .env("GIT_CONFIG_NOSYSTEM", "1")
                    .env("GIT_AUTHOR_NAME", "test")
                    .env("GIT_AUTHOR_EMAIL", "test@example.org")
                    .env("GIT_COMMITTER_NAME", "test")
                    .env("GIT_COMMITTER_EMAIL", "test@example.org")
                    .output()
                    .unwrap();
                assert!(output.status.success(), "{output:?}");
                String::from_utf8(output.stdout).unwrap().trim().to_owned()
            };
            let tree = git(&["mktree"]);
            let commit = git(&["commit-tree", &tree, "-m", &session.session_id]);
            let old = git(&["for-each-ref", "--format=%(objectname)", "refs/heads/main"]);
            let old = if old.is_empty() { ZERO.to_owned() } else { old };
            let updates = vec![RefUpdate {
                ref_name: "refs/heads/main".into(),
                old_id: old,
                new_id: commit.clone(),
                fast_forward: Some(true),
                signature: SignatureVerdict::Verified.into(),
                signature_reason: String::new(),
            }];
            let admitted = gate
                .pre_receive(Request::new(PreReceiveRequest {
                    session_id: session.session_id.clone(),
                    updates,
                }))
                .await
                .unwrap()
                .into_inner();
            assert!(admitted.allowed, "{admitted:?}");
            git(&["update-ref", "refs/heads/main", &commit]);
            let started = tokio::time::Instant::now();
            let runs = gate
                .session_exited(Request::new(SessionExitedRequest {
                    session_id: session.session_id,
                    exit_status: 0,
                }))
                .await
                .unwrap()
                .into_inner()
                .runs;
            (runs, started.elapsed())
        }
    };
    // Nothing takes the events: the answer comes empty after the bound.
    let (runs, waited) = push("push").await;
    assert!(runs.is_empty());
    assert!(waited >= PUSH_RUNS_WAIT, "{waited:?}");
    // The `ci` handler catches up, and keeps up from then on.
    let dispatcher = async {
        loop {
            {
                let mut writer = admin.writer.lock().await;
                while admin.bus.dispatch_next(&mut writer, &mut ci).await.unwrap() {}
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    let pushes = async {
        let (runs, _) = push("push").await;
        let created: String =
            sqlx::query_scalar("SELECT public_id FROM run ORDER BY id DESC LIMIT 1")
                .fetch_one(&mut *admin.writer.lock().await)
                .await
                .unwrap();
        assert_eq!(
            runs.iter()
                .map(|run| (run.run_public_id.as_str(), run.ref_name.as_str()))
                .collect::<Vec<_>>(),
            [(created.as_str(), "refs/heads/main")]
        );
        // CI off: no run, and no wait.
        let (runs, waited) = push("off").await;
        assert!(runs.is_empty());
        assert!(waited < PUSH_RUNS_WAIT, "{waited:?}");
    };
    tokio::select! {
        () = dispatcher => unreachable!(),
        () = pushes => {}
    }
}

#[tokio::test]
async fn maintenance_refuses_writing_sessions_and_existing_pushes_but_allows_clone_and_exit() {
    use kajiba_proto::git::v1::SessionKind;
    let (_state, admin, gate, alice) = session_fixture().await;
    let mut request = OpenSessionRequest {
        generation: gate.admission.generation().unwrap(),
        user_public_id: alice,
        kind: SessionKind::ReceivePack.into(),
        repository: Some(open_session_request::Repository::RepoAddress(
            "alice/demo".into(),
        )),
        correlation_id: String::new(),
        session_id: kajiba_core::new_id().unwrap(),
    };
    let session = gate
        .open_session(Request::new(request.clone()))
        .await
        .unwrap()
        .into_inner()
        .session
        .unwrap();
    let closed = admin.maintenance.close().unwrap();
    for kind in [
        SessionKind::ReceivePack,
        SessionKind::Import,
        SessionKind::Maintenance,
    ] {
        request.kind = kind.into();
        request.session_id = kajiba_core::new_id().unwrap();
        let status = gate
            .open_session(Request::new(request.clone()))
            .await
            .unwrap_err();
        assert_eq!(status.code(), Code::Unavailable);
        assert_eq!(
            status.get_error_details().error_info().unwrap().reason,
            "MAINTENANCE"
        );
        assert!(status.message().contains("retry"));
        assert!(gate.admission.session(&request.session_id).is_none());
    }
    let status = gate
        .pre_receive(Request::new(PreReceiveRequest {
            session_id: session.session_id.clone(),
            ..Default::default()
        }))
        .await
        .unwrap_err();
    assert_eq!(status.code(), Code::Unavailable);
    assert_eq!(
        status.get_error_details().error_info().unwrap().reason,
        "MAINTENANCE"
    );
    let pending: i64 = sqlx::query_scalar("SELECT count(*) FROM pending_push")
        .fetch_one(&mut *admin.writer.lock().await)
        .await
        .unwrap();
    assert_eq!(pending, 0);
    request.kind = SessionKind::UploadPack.into();
    let read = gate
        .open_session(Request::new(request.clone()))
        .await
        .unwrap()
        .into_inner()
        .session
        .unwrap();
    gate.post_receive(Request::new(PostReceiveRequest {
        session_id: session.session_id.clone(),
    }))
    .await
    .unwrap();
    for id in [session.session_id, read.session_id] {
        gate.session_exited(Request::new(SessionExitedRequest {
            session_id: id,
            exit_status: 0,
        }))
        .await
        .unwrap();
    }
    assert!(!gate.admission.running(1));
    drop(closed);
    request.kind = SessionKind::ReceivePack.into();
    request.session_id = kajiba_core::new_id().unwrap();
    assert!(gate.open_session(Request::new(request)).await.is_ok());
}
