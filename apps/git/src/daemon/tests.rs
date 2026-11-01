use super::*;
use crate::state::tests::TempDir;
use std::{fs, os::unix::fs::MetadataExt, time::Duration};
use tokio::net::{UnixDatagram, UnixStream};

fn config(state: &Path, server: &Path) -> GitConfig {
    GitConfig {
        state_dir: state.into(),
        server_socket: server.into(),
        run_dir: state.into(),
        server_user: "kajiba-server".into(),
        user: "kajiba-git".into(),
        hooks_path: state.join("hooks"),
        log: Default::default(),
        metrics: Default::default(),
    }
}

#[tokio::test]
async fn readiness_arrives_only_after_listening_and_shutdown_removes_socket() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let temp = TempDir::new();
    let metrics = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = metrics.local_addr().unwrap();
    let notify_path = temp.0.join("notify");
    let notify = UnixDatagram::bind(&notify_path).unwrap();
    let socket = temp.0.join("local.sock");
    let state = temp.0.join("state");
    let stopped = async {
        let mut bytes = [0; 64];
        let len = notify.recv(&mut bytes).await.unwrap();
        assert_eq!(&bytes[..len], b"READY=1");
        assert!(state.join("tmp/orphans").is_dir());
        assert!(
            crate::state::prepare(&state)
                .unwrap_err()
                .contains("another instance holds")
        );
        assert_eq!(fs::metadata(&socket).unwrap().mode() & 0o777, 0o660);
        let ssh = socket.with_file_name("ssh.sock");
        assert_eq!(fs::metadata(&ssh).unwrap().mode() & 0o777, 0o600);
        assert_eq!(
            fs::metadata(socket.with_file_name("session.sock"))
                .unwrap()
                .mode()
                & 0o777,
            0o600
        );
        drop(UnixStream::connect(&ssh).await.unwrap());
        let stream = UnixStream::connect(&socket).await.unwrap();
        drop(stream);
        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        stream
            .write_all(b"GET /metrics HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();
        assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(response.contains("kajiba_git_sessions_active{kind=\"import\"} 0\n"));
        assert!(tokio::net::TcpListener::bind(addr).await.is_err());
    };
    tokio::time::timeout(
        Duration::from_secs(3),
        run(
            &config(&state, &temp.0.join("git.sock")),
            &socket,
            kajiba_core::local::effective_uid(),
            vec![kajiba_core::local::effective_uid()],
            Some(notify_path.as_os_str()),
            async { Ok(Some(metrics)) },
            stopped,
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(!socket.exists());
    assert!(!socket.with_file_name("ssh.sock").exists());
    assert!(!socket.with_file_name("session.sock").exists());
    let _lock = crate::state::prepare(&state).unwrap();
}

#[tokio::test]
async fn failed_metrics_bind_never_notifies_ready() {
    let temp = TempDir::new();
    let notify_path = temp.0.join("notify");
    let notify = std::os::unix::net::UnixDatagram::bind(&notify_path).unwrap();
    notify.set_nonblocking(true).unwrap();
    let occupied = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let socket = temp.0.join("local.sock");
    let err = run(
        &config(&temp.0.join("state"), &temp.0.join("git.sock")),
        &socket,
        kajiba_core::local::effective_uid(),
        vec![kajiba_core::local::effective_uid()],
        Some(notify_path.as_os_str()),
        crate::metrics::bind(Some(occupied.local_addr().unwrap())),
        std::future::pending(),
    )
    .await
    .unwrap_err();
    assert!(err.contains("metrics.listen"), "{err}");
    assert!(!socket.exists());
    assert!(!socket.with_file_name("ssh.sock").exists());
    assert_eq!(
        notify.recv(&mut [0; 64]).unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
}

#[tokio::test]
async fn failed_startup_never_notifies_ready() {
    let temp = TempDir::new();
    let notify_path = temp.0.join("notify");
    let notify = std::os::unix::net::UnixDatagram::bind(&notify_path).unwrap();
    notify.set_nonblocking(true).unwrap();
    let state = temp.0.join("state");
    let socket = temp.0.join("local.sock");
    // Failure before the lock, at the lock, then at bind.
    fs::write(&state, "preserve").unwrap();
    let peers = vec![kajiba_core::local::effective_uid()];
    assert!(
        run(
            &config(&state, &temp.0.join("git.sock")),
            &socket,
            kajiba_core::local::effective_uid(),
            peers.clone(),
            Some(notify_path.as_os_str()),
            crate::metrics::bind(None),
            std::future::pending()
        )
        .await
        .is_err()
    );
    fs::remove_file(&state).unwrap();
    let lock = crate::state::prepare(&state).unwrap();
    assert!(
        run(
            &config(&state, &temp.0.join("git.sock")),
            &socket,
            kajiba_core::local::effective_uid(),
            peers.clone(),
            Some(notify_path.as_os_str()),
            crate::metrics::bind(None),
            std::future::pending()
        )
        .await
        .unwrap_err()
        .contains("another instance holds")
    );
    drop(lock);
    fs::write(&socket, "preserve").unwrap();
    assert!(
        run(
            &config(&state, &temp.0.join("git.sock")),
            &socket,
            kajiba_core::local::effective_uid(),
            peers,
            Some(notify_path.as_os_str()),
            crate::metrics::bind(None),
            std::future::pending()
        )
        .await
        .unwrap_err()
        .contains("not a socket")
    );
    let mut bytes = [0; 64];
    assert_eq!(
        notify.recv(&mut bytes).unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    assert_eq!(fs::read_to_string(&socket).unwrap(), "preserve");
}

#[tokio::test]
async fn notification_failure_cleans_up_and_missing_notify_socket_is_optional() {
    let temp = TempDir::new();
    let state = temp.0.join("state");
    let socket = temp.0.join("local.sock");
    let peers = vec![kajiba_core::local::effective_uid()];
    let missing = temp.0.join("missing");
    assert!(
        run(
            &config(&state, &temp.0.join("git.sock")),
            &socket,
            kajiba_core::local::effective_uid(),
            peers.clone(),
            Some(missing.as_os_str()),
            crate::metrics::bind(None),
            std::future::pending()
        )
        .await
        .unwrap_err()
        .contains("notify readiness")
    );
    assert!(!socket.exists());
    assert!(!socket.with_file_name("ssh.sock").exists());
    run(
        &config(&state, &temp.0.join("git.sock")),
        &socket,
        kajiba_core::local::effective_uid(),
        peers,
        None,
        crate::metrics::bind(None),
        std::future::ready(()),
    )
    .await
    .unwrap();
    assert!(!socket.exists());
    assert!(!socket.with_file_name("ssh.sock").exists());
}

#[cfg(target_os = "linux")]
#[test]
fn readiness_supports_systemd_abstract_socket() {
    use std::os::{
        linux::net::SocketAddrExt,
        unix::net::{SocketAddr, UnixDatagram},
    };
    let name = format!("kajiba-notify-{}", std::process::id());
    let socket = UnixDatagram::bind_addr(&SocketAddr::from_abstract_name(&name).unwrap()).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    notify_ready(Some(OsStr::new(&format!("@{name}")))).unwrap();
    let mut bytes = [0; 64];
    let len = socket.recv(&mut bytes).unwrap();
    assert_eq!(&bytes[..len], b"READY=1");
}

#[tokio::test]
async fn daemon_emits_shared_json_logs_on_stderr() {
    if std::env::var_os("KAJIBA_TEST_LOGS").is_some() {
        kajiba_core::log::init("info").unwrap();
        let temp = TempDir::new();
        run(
            &config(&temp.0.join("state"), &temp.0.join("git.sock")),
            &temp.0.join("local.sock"),
            kajiba_core::local::effective_uid(),
            vec![kajiba_core::local::effective_uid()],
            None,
            crate::metrics::bind(None),
            std::future::ready(()),
        )
        .await
        .unwrap();
        return;
    }
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "daemon::tests::daemon_emits_shared_json_logs_on_stderr",
            "--nocapture",
        ])
        .env("KAJIBA_TEST_LOGS", "1")
        .env_remove("RUST_LOG")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let line = String::from_utf8(output.stderr)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .find(|line| line["event"] == "ready")
        .unwrap();
    assert_eq!(line["event"], "ready");
    assert_eq!(line["level"], "info");
    assert!(line["ts"].is_string());
}

#[tokio::test]
async fn repository_rpcs_run_in_the_real_daemon() {
    use kajiba_proto::git::v1::{
        CreateRepositoryRequest, QuarantineRepositoryRequest, SetHeadRequest,
        git_service_client::GitServiceClient,
    };
    let temp = TempDir::new();
    let root = temp.0.join("state");
    let socket = temp.0.join("local.sock");
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let daemon = tokio::spawn({
        let root = root.clone();
        let socket = socket.clone();
        let server = temp.0.join("absent.sock");
        async move {
            run(
                &config(&root, &server),
                &socket,
                kajiba_core::local::effective_uid(),
                vec![kajiba_core::local::effective_uid()],
                None,
                crate::metrics::bind(None),
                async {
                    let _ = stopped.await;
                },
            )
            .await
        }
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !socket.exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let mut client =
        GitServiceClient::new(kajiba_core::local::channel(&socket, None).await.unwrap());
    let error = crate::local::auth_key(&socket, "known".into())
        .await
        .unwrap_err();
    let status = error.downcast_ref::<tonic::Status>().unwrap();
    assert_eq!(status.code(), tonic::Code::Unavailable);
    assert!(status.message().contains("server unavailable"));
    let id = "00000000000000000000000001";
    client
        .create_repository(CreateRepositoryRequest {
            repo_public_id: id.into(),
            default_branch: "main".into(),
        })
        .await
        .unwrap();
    assert_eq!(
        client
            .create_repository(CreateRepositoryRequest {
                repo_public_id: id.into(),
                default_branch: "main".into()
            })
            .await
            .unwrap_err()
            .code(),
        tonic::Code::AlreadyExists
    );
    client
        .set_head(SetHeadRequest {
            repo_public_id: id.into(),
            branch: "dev".into(),
        })
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(root.join(format!("repos/{id}.git/HEAD"))).unwrap(),
        "ref: refs/heads/dev\n"
    );
    let destination = client
        .quarantine_repository(QuarantineRepositoryRequest {
            directory_name: format!("{id}.git").into_bytes(),
        })
        .await
        .unwrap()
        .into_inner()
        .destination;
    assert!(Path::new(&destination).join("HEAD").is_file());
    assert_eq!(
        client
            .set_head(SetHeadRequest {
                repo_public_id: id.into(),
                branch: "main".into()
            })
            .await
            .unwrap_err()
            .code(),
        tonic::Code::NotFound
    );
    stop.send(()).unwrap();
    daemon.await.unwrap().unwrap();
}

#[tokio::test]
async fn verification_limits_survive_production_rpc_middleware() {
    use kajiba_proto::{
        git::v1::{Session, SessionKind},
        hook::v1::{
            PreReceiveRequest, Quarantine, RefUpdate, hook_service_client::HookServiceClient,
        },
    };
    use tonic_types::StatusExt;

    let temp = TempDir::new();
    let _lock = crate::state::prepare(&temp.0).unwrap();
    let id = "00000000000000000000000001";
    let repo = temp.0.join(format!("repos/{id}.git"));
    fs::create_dir(&repo).unwrap();
    let repositories = std::sync::Arc::new(crate::repositories::Repositories::new(temp.0.clone()));
    repositories
        .register(
            Session {
                session_id: id.into(),
                repo_public_id: id.into(),
                user_public_id: id.into(),
                kind: SessionKind::ReceivePack.into(),
                ..Default::default()
            },
            String::new(),
        )
        .unwrap();
    let app = routes(std::sync::Arc::new(crate::service::Git {
        generation: "generation".into(),
        server_socket: temp.0.join("absent.sock"),
        repositories,
    }))
    .into_axum_router();
    let mut client = HookServiceClient::new(app);
    let mut request = PreReceiveRequest {
        session_id: id.into(),
        repo_public_id: id.into(),
        user_public_id: id.into(),
        updates: (0..257)
            .map(|i| RefUpdate {
                ref_name: format!("refs/heads/limit-{i}"),
                old_id: "0".repeat(40),
                new_id: "1".repeat(40),
            })
            .collect(),
        ..Default::default()
    };
    let limit = client.pre_receive(request.clone()).await.unwrap_err();
    request.updates.truncate(1);
    let incoming = temp.0.join("incoming");
    fs::create_dir(&incoming).unwrap();
    let pack = fs::File::create(incoming.join("data.pack")).unwrap();
    pack.set_len((128 << 20) + 1).unwrap();
    request.quarantine = Some(Quarantine {
        object_directory: incoming.to_str().unwrap().into(),
        ..Default::default()
    });
    let quarantine = client.pre_receive(request.clone()).await.unwrap_err();
    // An unrelated verification error must still be sanitized by the same routes.
    request.quarantine = None;
    request.updates[0].new_id = "private-invalid-object-id".into();
    let internal = client.pre_receive(request).await.unwrap_err();
    for (error, code, reason, message) in [
        (
            limit,
            tonic::Code::ResourceExhausted,
            "VERIFICATION_LIMIT",
            "push verification exceeds 256 updated refs",
        ),
        (
            quarantine,
            tonic::Code::ResourceExhausted,
            "VERIFICATION_LIMIT",
            "push quarantine exceeds 128 MiB limit",
        ),
        (
            internal,
            tonic::Code::Internal,
            "INTERNAL",
            "internal error",
        ),
    ] {
        assert_eq!(error.code(), code);
        assert_eq!(error.message(), message);
        let details = error.get_error_details();
        let info = details.error_info().unwrap();
        assert_eq!(info.domain, "kajiba");
        assert_eq!(info.reason, reason);
        assert_eq!(info.metadata.len(), 1);
        assert!(kajiba_core::names::is_public_id(
            &info.metadata["request_id"]
        ));
    }
    assert_eq!(fs::read_dir(repo).unwrap().count(), 0);
}

#[tokio::test]
async fn repository_rpc_routes_correlate_errors_and_logs() {
    use tonic_types::StatusExt;
    const CHILD: &str = "KAJIBA_TEST_DAEMON_RPC_LOGS";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "daemon::tests::repository_rpc_routes_correlate_errors_and_logs",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env_remove("RUST_LOG")
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let stdout = String::from_utf8(output.stdout).unwrap();
        let errors: Vec<(String, String)> = serde_json::from_str(
            stdout
                .lines()
                .find_map(|line| line.strip_prefix("ERROR_IDS="))
                .unwrap(),
        )
        .unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(!stderr.contains("DO-NOT-TRUST"));
        let logs: Vec<serde_json::Value> = stderr
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let requests: Vec<_> = logs
            .iter()
            .filter(|line| line["event"] == "request")
            .collect();
        assert_eq!(requests.len(), 4, "{logs:?}");
        let ids: std::collections::BTreeSet<_> = requests
            .iter()
            .map(|line| line["request_id"].as_str().unwrap())
            .collect();
        assert_eq!(ids.len(), requests.len());
        for line in &requests {
            assert_eq!(line["status"], 200);
            assert!(line["dur_us"].is_number());
            assert!(line["method"].as_str().unwrap().starts_with("/kajiba."));
        }
        for (id, code) in errors {
            let request = requests
                .iter()
                .find(|line| line["request_id"] == id)
                .unwrap();
            assert_eq!(request["grpc_code"], code);
        }
        let created = logs
            .iter()
            .find(|line| line["event"] == "repo-created")
            .unwrap();
        assert!(
            requests.iter().any(
                |line| line["request_id"] == created["request_id"] && line["grpc_code"] == "OK"
            )
        );
        let internal = requests
            .iter()
            .find(|line| line["grpc_code"] == "INTERNAL")
            .unwrap();
        assert_eq!(internal["level"], "error");
        assert!(internal["err"].as_str().unwrap().contains("repos"));
        return;
    }
    kajiba_core::log::init("info").unwrap();
    use kajiba_proto::git::v1::{CreateRepositoryRequest, git_service_client::GitServiceClient};
    use kajiba_proto::hook::v1::{PreReceiveRequest, hook_service_client::HookServiceClient};
    let temp = TempDir::new();
    let _lock = crate::state::prepare(&temp.0).unwrap();
    let app = routes(std::sync::Arc::new(crate::service::Git {
        generation: "generation".into(),
        server_socket: temp.0.join("absent.sock"),
        repositories: std::sync::Arc::new(crate::repositories::Repositories::new(temp.0.clone())),
    }))
    .into_axum_router();
    let mut client = GitServiceClient::new(app.clone());
    let request = CreateRepositoryRequest {
        repo_public_id: "00000000000000000000000001".into(),
        default_branch: "main".into(),
    };
    client.create_repository(request.clone()).await.unwrap();
    let mut duplicate = tonic::Request::new(request.clone());
    duplicate
        .metadata_mut()
        .insert("x-request-id", "DO-NOT-TRUST".parse().unwrap());
    let duplicate = client.create_repository(duplicate).await.unwrap_err();
    // A filesystem error stays in the correlated log, never in the response.
    fs::rename(temp.0.join("repos"), temp.0.join("saved-repos")).unwrap();
    fs::write(temp.0.join("repos"), "not a directory").unwrap();
    let internal = client.create_repository(request).await.unwrap_err();
    assert_eq!(internal.message(), "internal error");
    let hook = HookServiceClient::new(app)
        .pre_receive(PreReceiveRequest::default())
        .await
        .unwrap_err();
    let mut ids = Vec::new();
    for (error, code, reason, grpc_code) in [
        (
            duplicate,
            tonic::Code::AlreadyExists,
            "ALREADY_EXISTS",
            "ALREADY_EXISTS",
        ),
        (internal, tonic::Code::Internal, "INTERNAL", "INTERNAL"),
        (
            hook,
            tonic::Code::InvalidArgument,
            "INVALID_FIELD",
            "INVALID_ARGUMENT",
        ),
    ] {
        assert_eq!(error.code(), code);
        let details = error.get_error_details();
        let info = details.error_info().unwrap();
        assert_eq!(info.domain, "kajiba");
        assert_eq!(info.reason, reason);
        assert_eq!(info.metadata.len(), 1);
        assert!(kajiba_core::names::is_public_id(
            &info.metadata["request_id"]
        ));
        ids.push((info.metadata["request_id"].clone(), grpc_code));
    }
    println!("ERROR_IDS={}", serde_json::to_string(&ids).unwrap());
}

#[test]
fn draining_refuses_new_sessions_and_preserves_running_ones() {
    use kajiba_proto::git::v1::Session;
    let repositories = crate::repositories::Repositories::new("/absent".into());
    let session = Session {
        session_id: "running".into(),
        ..Default::default()
    };
    repositories
        .register(session.clone(), String::new())
        .unwrap();
    repositories.stop_accepting();
    assert_eq!(repositories.sessions(), std::slice::from_ref(&session));
    assert_eq!(
        repositories
            .register(
                Session {
                    session_id: "new".into(),
                    kind: kajiba_proto::git::v1::SessionKind::Import as i32,
                    ..Default::default()
                },
                String::new()
            )
            .unwrap_err()
            .code(),
        tonic::Code::Unavailable
    );
    repositories.finished(&session.session_id, 0);
    assert!(repositories.sessions().is_empty());
    // Handoffs accepted before the stop may finish their admission during drain.
    repositories
        .register(
            Session {
                session_id: "accepted-ssh".into(),
                kind: kajiba_proto::git::v1::SessionKind::ReceivePack as i32,
                ..Default::default()
            },
            String::new(),
        )
        .unwrap();
}
