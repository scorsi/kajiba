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
pub(in crate::transport) const PUBLIC_URL: &str = "https://forge.example.org/";

/// An admin service over a fresh database in `dir` with the declared `users`.
pub(in crate::transport) async fn admin(dir: &Path, users: &str) -> Admin {
    let (mut writer, _) = crate::db::open(&dir.join("kajiba.sqlite")).await.unwrap();
    let users = kajiba_core::config::ServerConfig::parse(&format!(
        "[http]\npublic_url='{PUBLIC_URL}'\n{users}"
    ))
    .unwrap()
    .users;
    crate::db::reconcile(&mut writer, &users).await.unwrap();
    Admin {
        writer: crate::db::Writer::new(writer.into()),
        public_url: PUBLIC_URL.into(),
        enrollment_ttl: Duration::from_secs(15 * 60),
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
        let admin = admin(&dir.0, "").await;
        let server = tokio::spawn(local.serve(admin, Arc::default(), std::future::pending()));
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            enroll(&dir.socket(), "alice".into()),
        )
        .await
        .unwrap();
        let err = result.unwrap_err();
        if accepted {
            let status = err.downcast_ref::<Status>().unwrap();
            assert_eq!(status.code(), Code::NotFound);
            let details = status.get_error_details();
            let info = details.error_info().unwrap();
            assert_eq!(info.domain, "kajiba");
            assert_eq!(info.reason, "USER_NOT_FOUND");
            assert_eq!(info.metadata["request_id"].len(), 26);
        } else {
            assert!(
                err.downcast_ref::<Status>()
                    .is_none_or(|status| status.code() != Code::NotFound)
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
async fn readiness_tracks_local_listener_and_admin_command_reaches_the_service() {
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
    let admin = admin(&dir.0, "[users.alice]").await;
    let server = tokio::spawn(local.serve(admin, Arc::default(), std::future::pending()));
    // This is the same command handler used by `kajiba-server admin enroll`.
    assert_eq!(
        crate::admin_enroll(&dir.socket(), "alice".into()).await,
        std::process::ExitCode::SUCCESS
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

#[tokio::test]
async fn enroll_maps_refusals_to_adr_0020_statuses_and_returns_a_public_url() {
    let dir = SocketDir::new();
    let admin = admin(&dir.0, "[users.alice]\n[users.bob]").await;
    // bob leaves the configuration and is disabled.
    let users = kajiba_core::config::ServerConfig::parse(&format!(
        "[http]\npublic_url='{PUBLIC_URL}'\n[users.alice]"
    ))
    .unwrap()
    .users;
    crate::db::reconcile(&mut *admin.writer.lock().await, &users)
        .await
        .unwrap();
    for (user, code, reason) in [
        ("carol", Code::NotFound, "USER_NOT_FOUND"),
        ("bob", Code::FailedPrecondition, "INVALID_STATE"),
    ] {
        let status = admin
            .enroll(Request::new(EnrollRequest { user: user.into() }))
            .await
            .unwrap_err();
        assert_eq!(status.code(), code);
        assert!(status.message().contains(user));
        assert_eq!(
            status.get_error_details().error_info().unwrap().reason,
            reason
        );
    }
    let url = admin
        .enroll(Request::new(EnrollRequest {
            user: "Alice".into(),
        }))
        .await
        .unwrap()
        .into_inner()
        .url;
    let token = url
        .strip_prefix("https://forge.example.org/auth/enroll#")
        .unwrap();
    let hash: Vec<u8> = sqlx::query_scalar("SELECT token_hash FROM enrollment_token")
        .fetch_one(&mut *admin.writer.lock().await)
        .await
        .unwrap();
    assert_eq!(hash, kajiba_core::sha256(token.as_bytes()));
}

#[tokio::test]
async fn cli_prints_the_link_and_logs_enrollment_without_the_token() {
    const CHILD: &str = "KAJIBA_ENROLL_CLI_TEST";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "transport::local::tests::cli_prints_the_link_and_logs_enrollment_without_the_token",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env_remove("RUST_LOG")
            .output()
            .unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(output.status.success(), "{stderr}");
        let stdout = String::from_utf8(output.stdout).unwrap();
        let urls: Vec<_> = stdout
            .lines()
            .filter_map(|line| line.strip_prefix("https://forge.example.org/auth/enroll#"))
            .collect();
        assert_eq!(urls.len(), 1, "{stdout}");
        assert!(!stderr.contains(urls[0]));
        let issued: Vec<serde_json::Value> = stderr
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .filter(|line| line["event"] == "enrollment-issued")
            .collect();
        assert_eq!(issued.len(), 1, "{stderr}");
        assert_eq!(issued[0]["level"], "info");
        assert_eq!(issued[0]["user"], "alice");
        return;
    }
    kajiba_core::log::init("info").unwrap();
    let dir = SocketDir::new();
    let local = Local::bind(&dir.socket(), Arc::default()).unwrap();
    let admin = admin(&dir.0, "[users.alice]").await;
    let server = tokio::spawn(local.serve(admin, Arc::default(), std::future::pending()));
    assert_eq!(
        crate::admin_enroll(&dir.socket(), "alice".into()).await,
        std::process::ExitCode::SUCCESS
    );
    server.abort();
}
