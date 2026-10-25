use std::process::{Command, Output};

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_kajiba-server"))
        .args(args)
        .env_remove("RUST_LOG")
        .output()
        .expect("run kajiba-server")
}

#[test]
fn usage_errors_exit_two() {
    for args in [
        vec![],
        vec!["unknown"],
        vec!["--config", "server.toml"],
        vec!["serve"],
        vec!["serve", "--config"],
        vec!["serve", "--config", "server.toml", "--unknown"],
        vec!["config"],
        vec!["config", "unknown"],
        vec!["config", "check"],
        vec!["config", "check", "--files"],
        vec!["config", "check", "server.toml", "extra"],
        vec!["admin"],
        vec!["admin", "unknown"],
        vec!["admin", "enroll"],
        vec!["admin", "import"],
        vec!["admin", "import", "alice/demo"],
        vec!["admin", "repo"],
        vec!["admin", "repo", "unknown"],
        // Repositories are declared (ADR 0026): there is no create command.
        vec!["admin", "repo", "create"],
        vec!["admin", "repo", "create", "alice/demo"],
        vec!["admin", "repo", "list", "extra"],
        vec!["ssh-keys", "SHA256:abc"],
        vec!["serve-ssh", "--user", "01ARZ3NDEKTSV4RRFFQ69G5FAV"],
        vec!["hook", "pre-receive"],
        vec!["hook", "post-receive"],
        vec!["hook"],
        vec!["hook", "unknown"],
        vec!["hook", "pre-receive", "extra"],
    ] {
        let output = run(&args);
        assert_eq!(output.status.code(), Some(2), "{args:?}: {output:?}");
        assert!(output.stdout.is_empty(), "{args:?}: {output:?}");
        assert!(!output.stderr.is_empty(), "{args:?}: {output:?}");
    }
}

#[test]
fn version_matches_repository_version() {
    let output = run(&["--version"]);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(
        std::str::from_utf8(&output.stdout).unwrap(),
        format!(
            "kajiba-server {}\n",
            include_str!("../../../VERSION").trim()
        )
    );
    assert!(output.stderr.is_empty(), "{output:?}");
}

#[test]
fn help_succeeds() {
    let output = run(&["--help"]);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(
        std::str::from_utf8(&output.stdout)
            .unwrap()
            .contains("Usage:")
    );
    assert!(output.stderr.is_empty(), "{output:?}");
}

#[test]
fn config_check_validates_without_opening_secrets_and_files_checks_access() {
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/kajiba-core/tests/fixtures/server.toml");
    let output = run(&["config", "check", fixture.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(output.stdout.is_empty() && output.stderr.is_empty());

    let dir = std::env::temp_dir().join(format!("kajiba-config-check-{}", std::process::id()));
    std::fs::create_dir(&dir).unwrap();
    let path = dir.join("server.toml");
    let secret = dir.join("token");
    let input = format!(
        "[http]\npublic_url='https://example.org'\n[runners.build-1]\ntoken_file={:?}\n",
        secret.to_str().unwrap()
    );
    std::fs::write(&path, input).unwrap();
    let args = ["config", "check", "--files", path.to_str().unwrap()];
    let output = run(&args);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("runners.build-1.token_file")
    );
    std::fs::write(&secret, "DO-NOT-PRINT").unwrap();
    use std::os::unix::fs::PermissionsExt;
    // A secret readable by others is refused, then accepted once private.
    std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o644)).unwrap();
    let output = run(&args);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(error.contains("readable or writable by others"), "{error}");
    assert!(!error.contains("DO-NOT-PRINT"));
    std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o600)).unwrap();
    let output = run(&args);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(output.stdout.is_empty() && output.stderr.is_empty());

    std::fs::write(
        &path,
        "token='DO-NOT-PRINT'\n[http]\npublic_url='https://example.org'",
    )
    .unwrap();
    let output = run(&["config", "check", path.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(error.contains("token_file"), "{error}");
    assert!(!error.contains("DO-NOT-PRINT"));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn config_check_missing_config_is_a_runtime_error() {
    let output = run(&["config", "check", "/kajiba-test-missing/server.toml"]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("/kajiba-test-missing/server.toml")
    );
}

#[test]
fn invalid_config_is_structured_for_check_and_startup() {
    let dir = std::env::temp_dir().join(format!("kajiba-log-config-{}", std::process::id()));
    std::fs::create_dir(&dir).unwrap();
    let path = dir.join("server.toml");
    for (input, field) in [
        (
            "[http]\npublic_url='https://example.org'\n[metrics]\nlisten='0.0.0.0:9431'",
            "metrics.listen",
        ),
        (
            "token='DO-NOT-PRINT'\n[http]\npublic_url='https://example.org'",
            "token_file",
        ),
        (
            "[http]\npublic_url='https://example.org'\n[log]\nlevel='kajiba=verbose'",
            "log.level",
        ),
        (
            "[http]\npublic_url='https://example.org'\n[runners.build-1]\ntoken_file='/kajiba-test-missing/token'",
            "runners.build-1.token_file",
        ),
    ] {
        std::fs::write(&path, input).unwrap();
        for args in [
            vec!["config", "check", "--files", path.to_str().unwrap()],
            vec!["serve", "--config", path.to_str().unwrap()],
        ] {
            let output = run(&args);
            assert_eq!(output.status.code(), Some(1), "{output:?}");
            assert!(output.stdout.is_empty());
            let text = std::str::from_utf8(&output.stderr).unwrap();
            assert_eq!(text.lines().count(), 1, "{text}");
            assert!(!text.contains("DO-NOT-PRINT"));
            let line: serde_json::Value = serde_json::from_str(text).unwrap();
            assert_eq!(line["v"], 1);
            assert_eq!(line["event"], "config-invalid");
            assert_eq!(line["level"], "error");
            assert!(line["err"].as_str().unwrap().contains(field), "{text}");
        }
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn serve_emits_start_migrations_and_refuses_an_unreachable_orphan_writer() {
    use std::io::BufRead;
    use std::process::Stdio;

    let dir = std::env::temp_dir().join(format!("kajiba-log-start-{}", std::process::id()));
    std::fs::create_dir(&dir).unwrap();
    let path = dir.join("server.toml");
    let orphan = dir.join("git/repos/00000000000000000000000001.git");
    std::fs::create_dir_all(&orphan).unwrap();
    std::fs::write(orphan.join("HEAD"), "preserve orphan").unwrap();
    std::fs::write(
        &path,
        format!(
            "state_dir={:?}\n[git]\nstate_dir={:?}\nsocket={:?}\n[http]\npublic_url='https://example.org'",
            dir.join("state").to_str().unwrap(), dir.join("git").to_str().unwrap(), dir.join("missing.sock").to_str().unwrap()
        ),
    )
    .unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_kajiba-server"))
        .args(["serve", "--config", path.to_str().unwrap()])
        .env_remove("RUST_LOG")
        .stderr(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let stderr = child.stderr.take().unwrap();
    let (send, receive) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut reader = std::io::BufReader::new(stderr);
        for _ in 0..11 {
            let mut text = String::new();
            reader.read_line(&mut text).unwrap();
            let _ = send.send(text);
        }
    });
    let line = receive.recv_timeout(std::time::Duration::from_secs(5));
    let migration = receive.recv_timeout(std::time::Duration::from_secs(5));
    let later: Vec<_> = (0..9)
        .map(|_| receive.recv_timeout(std::time::Duration::from_secs(5)))
        .collect();
    let _ = child.kill();
    child.wait().unwrap();
    let contents = std::fs::read_to_string(orphan.join("HEAD"));
    let orphan_exists = orphan.exists();
    std::fs::remove_dir_all(dir).unwrap();
    reader.join().unwrap();
    let line: serde_json::Value = serde_json::from_str(&line.unwrap()).unwrap();
    assert_eq!(line["event"], "start");
    assert_eq!(line["level"], "info");
    assert_eq!(line["version"], include_str!("../../../VERSION").trim());
    let migration: serde_json::Value = serde_json::from_str(&migration.unwrap()).unwrap();
    assert_eq!(migration["event"], "migration-applied");
    assert_eq!(migration["level"], "info");
    assert_eq!(migration["version"], 1);
    let later: Vec<serde_json::Value> = later
        .into_iter()
        .map(|line| serde_json::from_str(&line.unwrap()).unwrap())
        .collect();
    for (line, version) in later[..8].iter().zip([2, 3, 4, 5, 6, 7, 8, 9]) {
        assert_eq!(line["event"], "migration-applied");
        assert_eq!(line["version"], version);
    }
    assert_eq!(later[8]["event"], "fatal");
    assert!(
        later[8]["err"]
            .as_str()
            .unwrap()
            .contains("GIT_SIDE_UNAVAILABLE")
    );
    assert_eq!(contents.unwrap(), "preserve orphan");
    assert!(orphan_exists);
}

#[test]
fn serve_refuses_held_state_lock_with_fatal_error() {
    let dir = std::env::temp_dir().join(format!("kajiba-lock-refusal-{}", std::process::id()));
    std::fs::create_dir(&dir).unwrap();
    let path = dir.join("server.toml");
    std::fs::write(
        &path,
        format!(
            "state_dir={:?}\n[http]\npublic_url='https://example.org'",
            dir.to_str().unwrap()
        ),
    )
    .unwrap();
    let lock_path = dir.join("kajiba.lock");
    let lock = std::fs::File::create(&lock_path).unwrap();
    lock.try_lock().unwrap();
    let output = run(&["serve", "--config", path.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let lines: Vec<serde_json::Value> = std::str::from_utf8(&output.stderr)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert_eq!(lines[1]["event"], "fatal");
    assert_eq!(lines[1]["level"], "error");
    assert_eq!(
        lines[1]["err"],
        format!("another instance holds {}", lock_path.display())
    );
    assert!(!dir.join("repos").exists());
    assert!(!dir.join("db/kajiba.sqlite").exists());
    drop(lock);
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn serve_reconciles_declared_users_keys_and_tokens_before_serving() {
    use sqlx::Connection;
    use std::time::{Duration, Instant};

    let dir = std::env::temp_dir().join(format!("kajiba-users-start-{}", std::process::id()));
    std::fs::create_dir(&dir).unwrap();
    std::fs::create_dir(dir.join("repos")).unwrap();
    let path = dir.join("server.toml");
    std::fs::write(
        &path,
        format!(
            "state_dir={:?}\n[git]\nstate_dir={:?}\n[http]\npublic_url='https://example.org'\n[users.alice]\nis_admin=true\nssh_keys=['ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAINdamAGCsQq31Uv+08lkBzoO4XLz2qYjJa8CGmj3B1Ea']\n[users.alice.tokens.ci]\nsha256='abababababababababababababababababababababababababababababababab'\nscopes=['repo:read']",
            dir.to_str().unwrap(), dir.to_str().unwrap()
        ),
    )
    .unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_kajiba-server"))
        .args(["serve", "--config", path.to_str().unwrap()])
        .env_remove("RUST_LOG")
        .stderr(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(dir.join("db/kajiba.sqlite"))
        .read_only(true);
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut reconciled = false;
    while Instant::now() < deadline {
        if let Ok(mut connection) = sqlx::SqliteConnection::connect_with(&options).await {
            let count = sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM user JOIN ssh_key ON ssh_key.user_id = user.id JOIN api_token ON api_token.user_id = user.id WHERE user.name = 'alice' AND kind = 'local' AND is_admin = 1 AND user.source = 'config' AND disabled_at IS NULL AND ssh_key.source = 'config' AND ssh_key.usage = 'auth' AND api_token.source = 'config' AND api_token.name = 'ci'",
            )
            .fetch_one(&mut connection)
            .await;
            connection.close().await.unwrap();
            if matches!(count, Ok(1)) {
                reconciled = true;
                break;
            }
        }
        if child.try_wait().unwrap().is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let _ = child.kill();
    child.wait().unwrap();
    std::fs::remove_dir_all(dir).unwrap();
    assert!(
        reconciled,
        "serve did not reconcile the declared user, key and token"
    );
}
