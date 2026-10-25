use super::*;
use crate::state::tests::TempDir;
use std::{fs, os::unix::fs::MetadataExt, time::Duration};
use tokio::net::{UnixDatagram, UnixStream};

fn config(state: &Path, server: &Path) -> GitConfig {
    GitConfig {
        state_dir: state.into(),
        server_socket: server.into(),
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
            repo_public_id: id.into(),
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
