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
        vec!["admin", "repo", "create"],
        vec!["admin", "repo", "list", "extra"],
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
fn placeholders_fail_with_distinct_exit_code_and_clear_error() {
    for args in [
        vec!["admin", "enroll", "alice"],
        vec!["admin", "import", "alice/demo", "demo.bundle"],
        vec!["admin", "repo", "create", "alice/demo"],
        vec!["admin", "repo", "list"],
        vec!["hook", "pre-receive"],
        vec!["hook", "post-receive"],
    ] {
        let output = run(&args);
        assert_eq!(output.status.code(), Some(3), "{args:?}: {output:?}");
        assert!(output.stdout.is_empty(), "{args:?}: {output:?}");
        let name = if args[1] == "repo" {
            args[..3].join(" ")
        } else {
            args[..2].join(" ")
        };
        let line: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(line["event"], "fatal");
        assert_eq!(line["err"], format!("{name}: not implemented yet"));
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
fn serve_emits_start_migrations_and_orphan_warning() {
    use std::io::BufRead;
    use std::process::Stdio;

    let dir = std::env::temp_dir().join(format!("kajiba-log-start-{}", std::process::id()));
    std::fs::create_dir(&dir).unwrap();
    let path = dir.join("server.toml");
    let orphan = dir.join("state/repos/orphan.git");
    std::fs::create_dir_all(&orphan).unwrap();
    std::fs::write(orphan.join("HEAD"), "preserve orphan").unwrap();
    std::fs::write(
        &path,
        format!(
            "state_dir={:?}\n[http]\npublic_url='https://example.org'",
            dir.join("state").to_str().unwrap()
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
        for _ in 0..6 {
            let mut text = String::new();
            reader.read_line(&mut text).unwrap();
            let _ = send.send(text);
        }
    });
    let line = receive.recv_timeout(std::time::Duration::from_secs(5));
    let migration = receive.recv_timeout(std::time::Duration::from_secs(5));
    let later: Vec<_> = (0..4)
        .map(|_| receive.recv_timeout(std::time::Duration::from_secs(5)))
        .collect();
    let _ = child.kill();
    child.wait().unwrap();
    let destination = dir.join("state/tmp/orphans/orphan.git");
    let contents = std::fs::read_to_string(destination.join("HEAD"));
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
    for (line, version) in later[..3].iter().zip([2, 3, 4]) {
        assert_eq!(line["event"], "migration-applied");
        assert_eq!(line["version"], version);
    }
    assert_eq!(later[3]["event"], "repo-orphaned");
    assert_eq!(later[3]["level"], "warn");
    assert_eq!(later[3]["path"], orphan.to_str().unwrap());
    assert_eq!(later[3]["destination"], destination.to_str().unwrap());
    assert_eq!(contents.unwrap(), "preserve orphan");
    assert!(!orphan_exists);
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
