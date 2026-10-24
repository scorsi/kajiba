use super::*;
use std::sync::atomic::AtomicUsize;
use tonic::Code;
use tonic_types::StatusExt;

struct SocketDir(PathBuf);
impl SocketDir {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "kj-local-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn socket(&self) -> PathBuf {
        self.0.join("local.sock")
    }
}
impl Drop for SocketDir {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[tokio::test]
async fn peer_uid_gate_accepts_owner_and_refuses_foreign_uid() {
    let dir = SocketDir::new();
    let serving = Arc::default();
    let mut local = Local::bind(&dir.socket(), serving).unwrap();
    let owner = fs::metadata(dir.socket()).unwrap().uid();
    assert_eq!(local.allowed_uids, [0, owner]);
    for allowed in [vec![owner], vec![owner.wrapping_add(1)]] {
        let accepted = allowed.contains(&owner);
        local.allowed_uids = allowed;
        let server = tokio::spawn(local.serve(Arc::default(), std::future::pending()));
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            enroll(&dir.socket(), "alice".into()),
        )
        .await
        .unwrap();
        let err = result.unwrap_err();
        if accepted {
            let status = err.downcast_ref::<Status>().unwrap();
            assert_eq!(status.code(), Code::Unimplemented);
            let details = status.get_error_details();
            let info = details.error_info().unwrap();
            assert_eq!(info.domain, "kajiba");
            assert_eq!(info.metadata["request_id"].len(), 26);
        } else {
            assert!(
                err.downcast_ref::<Status>()
                    .is_none_or(|status| status.code() != Code::Unimplemented)
            );
        }
        server.abort();
        assert!(server.await.unwrap_err().is_cancelled());
        assert!(!dir.socket().exists());
        local = Local::bind(&dir.socket(), Arc::default()).unwrap();
    }
}

#[tokio::test]
async fn allowed_uids_include_root_and_socket_owner() {
    let dir = SocketDir::new();
    let mut local = Local::bind(&dir.socket(), Arc::default()).unwrap();
    local.allowed_uids = vec![0, 1234];
    assert!(local.allows_uid(0));
    assert!(local.allows_uid(1234));
    assert!(!local.allows_uid(5678));
}

#[tokio::test]
async fn socket_permissions_stale_recovery_and_path_safety() {
    let dir = SocketDir::new();
    fs::write(dir.socket(), "preserve").unwrap();
    assert!(Local::bind(&dir.socket(), Arc::default()).is_err());
    assert_eq!(fs::read(dir.socket()).unwrap(), b"preserve");
    fs::remove_file(dir.socket()).unwrap();
    std::os::unix::fs::symlink("missing", dir.socket()).unwrap();
    assert!(Local::bind(&dir.socket(), Arc::default()).is_err());
    fs::remove_file(dir.socket()).unwrap();
    let stale = UnixListener::bind(dir.socket()).unwrap();
    drop(stale);
    let local = Local::bind(&dir.socket(), Arc::default()).unwrap();
    assert_eq!(fs::metadata(dir.socket()).unwrap().mode() & 0o777, 0o600);
    assert!(Local::bind(&dir.socket(), Arc::default()).is_err());
    drop(local);
    assert!(!dir.socket().exists());
}

#[tokio::test]
async fn readiness_tracks_local_listener_and_admin_command_reaches_stub() {
    let state = super::super::tests::State::new();
    state.start().await;
    state.health.local_serving.store(false, Ordering::Release);
    let dir = SocketDir::new();
    let local = Local::bind(&dir.socket(), state.health.local_serving.clone()).unwrap();
    let mut app = super::super::router(state.health.clone(), false, Arc::default()).unwrap();
    assert_eq!(
        super::super::tests::get(&mut app, "/readyz").await,
        axum::http::StatusCode::SERVICE_UNAVAILABLE
    );
    let server = tokio::spawn(local.serve(Arc::default(), std::future::pending()));
    // This is the same command handler used by `kajiba-server admin enroll`.
    assert_eq!(
        crate::admin_enroll(&dir.socket(), "alice".into()).await,
        std::process::ExitCode::from(crate::NOT_IMPLEMENTED)
    );
    assert_eq!(
        super::super::tests::get(&mut app, "/readyz").await,
        axum::http::StatusCode::OK
    );
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
    assert_eq!(
        super::super::tests::get(&mut app, "/readyz").await,
        axum::http::StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(
        crate::admin_enroll(&dir.socket(), "alice".into()).await,
        std::process::ExitCode::FAILURE
    );
}
