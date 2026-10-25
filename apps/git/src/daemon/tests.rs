use super::*;
use crate::state::tests::TempDir;
use std::{fs, os::unix::fs::MetadataExt, time::Duration};
use tokio::net::{UnixDatagram, UnixStream};

#[tokio::test]
async fn readiness_arrives_only_after_listening_and_shutdown_removes_socket() {
    let temp = TempDir::new();
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
        let stream = UnixStream::connect(&socket).await.unwrap();
        drop(stream);
    };
    tokio::time::timeout(
        Duration::from_secs(3),
        run(
            &state,
            &socket,
            vec![kajiba_core::local::effective_uid()],
            Some(notify_path.as_os_str()),
            stopped,
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(!socket.exists());
    let _lock = crate::state::prepare(&state).unwrap();
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
            &state,
            &socket,
            peers.clone(),
            Some(notify_path.as_os_str()),
            std::future::pending()
        )
        .await
        .is_err()
    );
    fs::remove_file(&state).unwrap();
    let lock = crate::state::prepare(&state).unwrap();
    assert!(
        run(
            &state,
            &socket,
            peers.clone(),
            Some(notify_path.as_os_str()),
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
            &state,
            &socket,
            peers,
            Some(notify_path.as_os_str()),
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
            &state,
            &socket,
            peers.clone(),
            Some(missing.as_os_str()),
            std::future::pending()
        )
        .await
        .unwrap_err()
        .contains("notify readiness")
    );
    assert!(!socket.exists());
    run(&state, &socket, peers, None, std::future::ready(()))
        .await
        .unwrap();
    assert!(!socket.exists());
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
            &temp.0.join("state"),
            &temp.0.join("local.sock"),
            vec![kajiba_core::local::effective_uid()],
            None,
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
    let line: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(line["event"], "ready");
    assert_eq!(line["level"], "info");
    assert!(line["ts"].is_string());
}
