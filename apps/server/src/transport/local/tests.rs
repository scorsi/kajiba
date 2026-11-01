use super::*;
use std::sync::atomic::AtomicUsize;
use std::{fs, os::unix::fs::MetadataExt, sync::atomic::Ordering};
use tokio::net::UnixListener;
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
        maintenance: Default::default(),
        git_socket: dir.join("absent.sock"),
        writer: crate::db::Writer::new(writer.into()),
        public_url: PUBLIC_URL.into(),
        enrollment_ttl: Duration::from_secs(15 * 60),
        gate: Arc::default(),
        bus: Arc::default(),
        state_dir: dir.into(),
        server_state_dir: dir.into(),
        dump_dir: dir.join("dumps"),
        dump_keep: 7,
        registry: Arc::default(),
        retention: crate::db::retention::Pass {
            logs: dir.join("logs"),
            bundles: dir.join("bundles"),
            log_retention: Duration::from_secs(60),
            worktree_retention: Duration::from_secs(60),
        },
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
    let mut local = bind(&dir.socket(), serving).unwrap();
    let owner = fs::metadata(dir.socket()).unwrap().uid();
    for allowed in [vec![owner], vec![owner.wrapping_add(1)]] {
        let accepted = allowed.contains(&owner);
        drop(local);
        local = Local::bind(&dir.socket(), Arc::default(), 0o600, allowed).unwrap();
        let admin = admin(&dir.0, "").await;
        let server = tokio::spawn(serve(local, admin, Arc::default(), std::future::pending()));
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
        local = bind(&dir.socket(), Arc::default()).unwrap();
    }
}

#[tokio::test]
async fn socket_permissions_stale_recovery_and_path_safety() {
    let dir = SocketDir::new();
    fs::write(dir.socket(), "preserve").unwrap();
    assert!(bind(&dir.socket(), Arc::default()).is_err());
    assert_eq!(fs::read(dir.socket()).unwrap(), b"preserve");
    fs::remove_file(dir.socket()).unwrap();
    std::os::unix::fs::symlink("missing", dir.socket()).unwrap();
    assert!(bind(&dir.socket(), Arc::default()).is_err());
    fs::remove_file(dir.socket()).unwrap();
    let stale = UnixListener::bind(dir.socket()).unwrap();
    drop(stale);
    // Parallel tests can fork a child that holds the listener until exec.
    let local = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match bind(&dir.socket(), Arc::default()) {
                Err(err) if err.kind() == io::ErrorKind::AddrInUse => {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
                result => break result.unwrap(),
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(fs::metadata(dir.socket()).unwrap().mode() & 0o777, 0o600);
    assert!(bind(&dir.socket(), Arc::default()).is_err());
    drop(local);
    assert!(!dir.socket().exists());
}

#[tokio::test]
async fn readiness_tracks_local_listener_and_admin_command_reaches_the_service() {
    let state = super::super::tests::State::new();
    state.start().await;
    state
        .health
        .local_serving
        .on
        .store(false, Ordering::Release);
    let dir = SocketDir::new();
    let local = bind(&dir.socket(), state.health.local_serving.clone()).unwrap();
    let mut app = super::super::router(
        state.health.clone(),
        false,
        Arc::default(),
        "https://example.org",
    )
    .unwrap();
    assert_eq!(
        super::super::tests::get(&mut app, "/readyz").await,
        axum::http::StatusCode::SERVICE_UNAVAILABLE
    );
    let admin = admin(&dir.0, "[users.alice]").await;
    let server = tokio::spawn(serve(local, admin, Arc::default(), std::future::pending()));
    // This is the same command handler used by `kajiba-server admin enroll`.
    assert_eq!(
        crate::admin_enroll(&dir.socket(), "alice".into()).await,
        std::process::ExitCode::SUCCESS
    );
    assert_eq!(
        super::super::tests::get(&mut app, "/readyz").await,
        axum::http::StatusCode::OK
    );
    // The listener still runs, but its path no longer reaches it: unlinked, then replaced.
    fs::rename(dir.socket(), dir.0.join("moved.sock")).unwrap();
    assert_eq!(
        super::super::tests::get(&mut app, "/readyz").await,
        axum::http::StatusCode::SERVICE_UNAVAILABLE
    );
    let impostor = std::os::unix::net::UnixListener::bind(dir.socket()).unwrap();
    assert_eq!(
        super::super::tests::get(&mut app, "/readyz").await,
        axum::http::StatusCode::SERVICE_UNAVAILABLE
    );
    drop(impostor);
    fs::remove_file(dir.socket()).unwrap();
    fs::rename(dir.0.join("moved.sock"), dir.socket()).unwrap();
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
    // An invalid name is refused before the database, and never echoed.
    for user in ["bad/DO-NOT-PRINT", "", &"a".repeat(64)] {
        let status = admin
            .enroll(Request::new(EnrollRequest { user: user.into() }))
            .await
            .unwrap_err();
        assert_eq!(status.code(), Code::InvalidArgument);
        assert!(
            user.is_empty() || !status.message().contains(user),
            "{status:?}"
        );
        let details = status.get_error_details();
        assert_eq!(details.error_info().unwrap().reason, "INVALID_FIELD");
        assert_eq!(
            details.bad_request().unwrap().field_violations[0].field,
            "user"
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
        .strip_prefix("https://forge.example.org/-/enroll#")
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
            .filter_map(|line| line.strip_prefix("https://forge.example.org/-/enroll#"))
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
    let local = bind(&dir.socket(), Arc::default()).unwrap();
    let admin = admin(&dir.0, "[users.alice]").await;
    let server = tokio::spawn(serve(local, admin, Arc::default(), std::future::pending()));
    assert_eq!(
        crate::admin_enroll(&dir.socket(), "alice".into()).await,
        std::process::ExitCode::SUCCESS
    );
    server.abort();
}

#[tokio::test]
async fn list_repos_maps_ids_to_names_by_pages_and_marks_archived() {
    let dir = SocketDir::new();
    let local = bind(&dir.socket(), Arc::default()).unwrap();
    let admin = admin(&dir.0, "[users.alice]\n[users.bob]").await;
    sqlx::query(
        "INSERT INTO repo (public_id, config_key, owner_id, name, visibility, ci_mode, default_branch, description, created_at, archived_at)
         SELECT column1, column2, (SELECT id FROM user WHERE name = column3), column2, 'public', 'off', 'main', '', 0, column4
         FROM (VALUES ('00000000000000000000000001', 'zeta', 'bob', NULL),
                      ('00000000000000000000000002', 'demo', 'alice', 5),
                      ('00000000000000000000000003', 'tools', 'alice', NULL))",
    )
    .execute(&mut *admin.writer.lock().await)
    .await
    .unwrap();
    let server = tokio::spawn(serve(local, admin, Arc::default(), std::future::pending()));
    let all = list_repos(&dir.socket()).await.unwrap();
    let names: Vec<_> = all
        .iter()
        .map(|r| {
            (
                r.public_id.as_str(),
                format!("{}/{}", r.owner, r.name),
                r.archived,
            )
        })
        .collect();
    assert_eq!(
        names,
        [
            ("00000000000000000000000002", "alice/demo".into(), true),
            ("00000000000000000000000003", "alice/tools".into(), false),
            ("00000000000000000000000001", "bob/zeta".into(), false),
        ]
    );
    let mut client = client(&dir.socket(), None).await.unwrap();
    let mut page_token = String::new();
    let mut paged = Vec::new();
    loop {
        let page = client
            .list_repos(ListReposRequest {
                page_size: 1,
                page_token,
            })
            .await
            .unwrap()
            .into_inner();
        assert!(page.repos.len() <= 1);
        paged.extend(page.repos);
        if page.next_page_token.is_empty() {
            break;
        }
        page_token = page.next_page_token;
    }
    assert_eq!(paged, all);
    let status = client
        .list_repos(ListReposRequest {
            page_size: 1,
            page_token: "garbage".into(),
        })
        .await
        .unwrap_err();
    assert_eq!(status.code(), Code::InvalidArgument);
    assert_eq!(
        status.get_error_details().error_info().unwrap().reason,
        "INVALID_FIELD"
    );
    server.abort();
}

#[tokio::test]
async fn import_request_limit_fits_both_maximum_size_files() {
    use super::super::tests::grpc;
    use prost::Message;

    let dir = SocketDir::new();
    let admin = admin(&dir.0, "").await;
    let mut app = router(admin, Arc::default());
    let method = "/kajiba.admin.v1.AdminService/Import";
    let max = kajiba_core::config::MAX_SECRET_BYTES as usize;
    let prefix = "https://example.org/";
    let mut request = ImportRequest {
        repo: format!("{}/{}", "a".repeat(63), "b".repeat(63)),
        source: format!("{prefix}{}", "s".repeat(max - prefix.len())),
        credentials: "c".repeat(max),
    };
    // A handler refusal proves that the combined fields passed the wire limit.
    let response = grpc(&mut app, method, request.clone()).await;
    let status = Status::from_header_map(response.headers()).unwrap();
    assert_eq!(status.code(), Code::NotFound);
    assert_eq!(
        status.get_error_details().error_info().unwrap().reason,
        "REPO_NOT_FOUND"
    );
    let limit = 2 * max + 256;
    request
        .credentials
        .push_str(&"c".repeat(limit + 1 - request.encoded_len()));
    assert_eq!(request.encoded_len(), limit + 1);
    let response = grpc(&mut app, method, request).await;
    let status = Status::from_header_map(response.headers()).unwrap();
    assert_eq!(status.code(), Code::OutOfRange);
}

#[tokio::test]
async fn import_never_returns_or_logs_source_credentials() {
    const CHILD: &str = "KAJIBA_IMPORT_CLI_TEST";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "transport::local::tests::import_never_returns_or_logs_source_credentials",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env_remove("RUST_LOG")
            .output()
            .unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(output.status.success(), "{stderr}");
        assert!(
            !stderr.contains("secret") && !stdout.contains("secret"),
            "{stderr}"
        );
        let fatal: Vec<serde_json::Value> = stderr
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .filter(|line| line["event"] == "fatal")
            .collect();
        assert_eq!(fatal.len(), 6, "{stderr}");
        for (i, needle) in [
            "the source cannot be read",
            "the source cannot be read",
            "the source cannot be read",
            "--credentials-file",
            "the source cannot be read",
            "--source-file",
        ]
        .into_iter()
        .enumerate()
        {
            assert!(fatal[i].to_string().contains(needle), "{}", fatal[i]);
        }
        return;
    }
    kajiba_core::log::init("debug").unwrap();
    let dir = SocketDir::new();
    let _lock = crate::state::prepare(&dir.0).unwrap();
    let local = bind(&dir.socket(), Arc::default()).unwrap();
    let mut admin = admin(&dir.0, "[users.alice]").await;
    let config = kajiba_core::config::ServerConfig::parse(&format!(
        "[http]\npublic_url='{PUBLIC_URL}'\n[users.alice]\n[repos.demo]\nowner='alice'\ndescription=''\n"
    ))
    .unwrap();
    let mut daemon = crate::transport::git_side::fake::Fake::start(&dir.0).await;
    admin.git_socket = dir.0.join("git.sock");
    let git_server = crate::transport::git_side::fake::Fake::serve(&dir.0, &admin.git_socket);
    crate::db::repos::reconcile_repos(
        &mut *admin.writer.lock().await,
        &dir.0,
        &mut daemon.client,
        &config.repos,
    )
    .await
    .unwrap()
    .committed();
    let server = tokio::spawn(serve(local, admin, Arc::default(), std::future::pending()));
    let socket = dir.socket();
    let import = |source: &str, credentials_file: Option<PathBuf>| {
        crate::admin_import(
            &socket,
            "alice/demo".into(),
            Some(source.into()),
            None,
            credentials_file,
        )
    };
    // Credentials in the source are refused before any git child runs.
    for source in [
        "https://user:secret@127.0.0.1:9/demo.git",
        "git://user:secret@127.0.0.1:9/demo.git",
    ] {
        assert_eq!(import(source, None).await, std::process::ExitCode::FAILURE);
    }
    // Credentials from a file reach git's environment only; git's failure never echoes them.
    use std::os::unix::fs::PermissionsExt;
    let file = dir.0.join("credentials");
    std::fs::write(&file, "user:secret\n").unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(
        import("https://127.0.0.1:9/demo.git", Some(file.clone())).await,
        std::process::ExitCode::FAILURE
    );
    // A file readable by others is refused unread.
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(
        import("https://127.0.0.1:9/demo.git", Some(file)).await,
        std::process::ExitCode::FAILURE
    );
    // A secret-bearing source comes from a file under the same rules (ADR 0048); git's
    // failure never echoes it.
    let file = dir.0.join("source");
    std::fs::write(&file, "https://127.0.0.1:9/secret/demo.git\n").unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    let import_file =
        || crate::admin_import(&socket, "alice/demo".into(), None, Some(file.clone()), None);
    assert_eq!(import_file().await, std::process::ExitCode::FAILURE);
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(import_file().await, std::process::ExitCode::FAILURE);
    server.abort();
    git_server.abort();
}

#[tokio::test]
async fn retention_deletes_old_logs_on_demand() {
    let dir = SocketDir::new();
    let local = bind(&dir.socket(), Arc::default()).unwrap();
    let admin = admin(&dir.0, "[users.alice]").await;
    sqlx::query("INSERT INTO repo (id, public_id, owner_id, name, visibility, ci_mode, default_branch, description, created_at, config_key) VALUES
        (1, '00000000000000000000000001', 1, 'demo', 'public', 'push', 'main', '', 0, 'demo');
        INSERT INTO runner (id, public_id, name, capabilities, version, connected) VALUES (1, '00000000000000000000000001', 'build-1', '{}', '', 0);
        INSERT INTO run (id, public_id, repo_id, sha, trigger, state, created_at) VALUES (1, '00000000000000000000000001', 1, 'a', 'manual', 'failed', 0);
        INSERT INTO job (id, public_id, run_id, name, requirements, replayable, max_attempts, state, created_at) VALUES (1, '00000000000000000000000001', 1, 'flake-check', '{}', 1, 3, 'failed', 0);
        INSERT INTO job_attempt (id, public_id, job_id, number, runner_id, state, lease_expires_at, started_at, finished_at) VALUES
        (1, '00000000000000000000000001', 1, 1, 1, 'failed', 0, 0, 1);
        INSERT INTO log_chunk (attempt_id, seq, step_idx, offset, length) VALUES (1, 1, 0, 0, 3);
        INSERT INTO run (id, public_id, repo_id, sha, trigger, state, created_at) VALUES (2, '00000000000000000000000002', 1, 'b', 'worktree', 'queued', 0)")
        .execute(&mut *admin.writer.lock().await)
        .await
        .unwrap();
    fs::create_dir(&admin.retention.logs).unwrap();
    let file = admin.retention.logs.join("00000000000000000000000001.log");
    fs::write(&file, "abc").unwrap();
    fs::create_dir(&admin.retention.bundles).unwrap();
    let bundle = admin
        .retention
        .bundles
        .join("00000000000000000000000002.bundle");
    fs::write(&bundle, "bundle").unwrap();
    let writer = admin.writer.clone();
    let server = tokio::spawn(serve(local, admin, Arc::default(), std::future::pending()));
    let deleted = retention(&dir.socket()).await.unwrap();
    assert_eq!((deleted.logs, deleted.worktree_runs), (1, 1));
    assert!(!file.exists());
    // The worktree run goes whole, bundle included; the manual run stays.
    assert!(!bundle.exists());
    let runs: Vec<String> = sqlx::query_scalar("SELECT trigger FROM run")
        .fetch_all(&mut *writer.lock().await)
        .await
        .unwrap();
    assert_eq!(runs, ["manual"]);
    assert_eq!(
        crate::admin_retention(&dir.socket()).await,
        std::process::ExitCode::SUCCESS
    );
    assert_eq!(
        crate::retention_line(0, 2),
        "logs deleted: 0 attempts\nworktree runs deleted: 2"
    );
    server.abort();
}

#[tokio::test]
async fn maintenance_refuses_local_mutations_without_side_effects_and_keeps_reads() {
    let dir = SocketDir::new();
    let admin = admin(&dir.0, "[users.alice]").await;
    let closed = admin.maintenance.close().unwrap();
    for status in [
        admin
            .enroll(Request::new(EnrollRequest {
                user: "alice".into(),
            }))
            .await
            .unwrap_err(),
        admin
            .import(Request::new(ImportRequest::default()))
            .await
            .unwrap_err(),
        admin
            .retention(Request::new(RetentionRequest {}))
            .await
            .unwrap_err(),
    ] {
        assert_eq!(status.code(), Code::Unavailable);
        assert_eq!(
            status.get_error_details().error_info().unwrap().reason,
            "MAINTENANCE"
        );
        assert!(status.message().contains("retry"));
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM enrollment_token")
        .fetch_one(&mut *admin.writer.lock().await)
        .await
        .unwrap();
    assert_eq!(count, 0);
    assert!(
        admin
            .list_repos(Request::new(ListReposRequest::default()))
            .await
            .is_ok()
    );
    assert!(!dir.0.join("logs").exists());
    drop(closed);
    assert!(
        admin
            .enroll(Request::new(EnrollRequest {
                user: "alice".into()
            }))
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn dump_drains_refuses_concurrency_and_publishes_only_success() {
    use crate::transport::git_side::fake::Fake;
    let dir = SocketDir::new();
    let server_state = dir.0.join("server");
    let git_state = dir.0.join("git");
    fs::create_dir_all(server_state.join("tmp")).unwrap();
    let mut admin = admin(&server_state, "[users.alice]").await;
    admin.state_dir = git_state.clone();
    admin.dump_dir = dir.0.join("dumps");
    admin.dump_keep = 1;
    fs::create_dir(&admin.dump_dir).unwrap();
    let mut git = Fake::start(&git_state).await;
    admin
        .gate
        .rebuild(
            &mut *admin.writer.lock().await,
            "00000000000000000000000001",
            vec![],
        )
        .await
        .unwrap();

    let completed = {
        let permit = admin.maintenance.admit().unwrap();
        let first = admin.create_dump(&mut git.client);
        tokio::pin!(first);
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut first)
                .await
                .is_err()
        );
        let status = admin.dump(Request::new(DumpRequest {})).await.unwrap_err();
        assert_eq!(status.code(), Code::Unavailable);
        assert_eq!(
            status.get_error_details().error_info().unwrap().reason,
            "MAINTENANCE"
        );
        // A read still acquires the writer while the admitted mutation drains.
        assert_eq!(
            admin
                .list_repos(Request::new(ListReposRequest::default()))
                .await
                .unwrap()
                .into_inner()
                .repos
                .len(),
            0
        );
        drop(permit);
        PathBuf::from(first.await.unwrap().into_inner().dir)
    };
    crate::dump::verify(&completed).await.unwrap();
    assert!(admin.maintenance.admit().is_ok());
    let success = admin.registry.exposition("test", [0; 4], [0; 4]);
    let sample = |text: String| {
        text.lines()
            .find(|line| line.starts_with("kajiba_dump_last_success_timestamp_seconds "))
            .unwrap()
            .to_owned()
    };
    assert_ne!(
        sample(success.clone()),
        "kajiba_dump_last_success_timestamp_seconds 0"
    );

    // A special file in durable state fails during copying, after staging is created.
    fs::create_dir_all(git_state.join("tmp/orphans")).unwrap();
    std::os::unix::fs::symlink("missing", git_state.join("tmp/orphans/unsafe")).unwrap();
    assert_eq!(
        admin.create_dump(&mut git.client).await.unwrap_err().code(),
        Code::Internal
    );
    assert!(admin.maintenance.admit().is_ok());
    assert_eq!(
        sample(admin.registry.exposition("test", [0; 4], [0; 4])),
        sample(success)
    );
    assert_eq!(fs::read_dir(&admin.dump_dir).unwrap().count(), 1);
    fs::remove_file(git_state.join("tmp/orphans/unsafe")).unwrap();
    let next = admin
        .create_dump(&mut git.client)
        .await
        .unwrap()
        .into_inner()
        .dir;
    assert!(!completed.exists());
    assert!(PathBuf::from(next).is_dir());
    assert_eq!(fs::read_dir(&admin.dump_dir).unwrap().count(), 1);
}

#[tokio::test]
async fn dump_unavailable_daemon_releases_maintenance_without_success() {
    let dir = SocketDir::new();
    let admin = admin(&dir.0, "").await;
    assert_eq!(
        admin
            .dump(Request::new(DumpRequest {}))
            .await
            .unwrap_err()
            .code(),
        Code::Internal
    );
    assert!(admin.maintenance.admit().is_ok());
    assert!(
        admin
            .registry
            .exposition("test", [0; 4], [0; 4])
            .contains("kajiba_dump_last_success_timestamp_seconds 0\n")
    );
}

#[tokio::test]
async fn dump_socket_refuses_unauthorized_peer_before_maintenance() {
    let dir = SocketDir::new();
    let local = Local::bind(
        &dir.socket(),
        Arc::default(),
        0o600,
        vec![kajiba_core::local::effective_uid().wrapping_add(1)],
    )
    .unwrap();
    let admin = admin(&dir.0, "").await;
    let server = tokio::spawn(serve(
        local,
        admin.clone(),
        Arc::default(),
        std::future::pending(),
    ));
    assert!(dump(&dir.socket()).await.is_err());
    assert!(admin.maintenance.admit().is_ok());
    assert!(!admin.dump_dir.exists());
    server.abort();
    let _ = server.await;
}
