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
fn serve_emits_start_with_version() {
    use std::io::BufRead;
    use std::process::Stdio;

    let path = std::env::temp_dir().join(format!("kajiba-log-start-{}.toml", std::process::id()));
    std::fs::write(&path, "[http]\npublic_url='https://example.org'").unwrap();
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
        let mut text = String::new();
        std::io::BufReader::new(stderr)
            .read_line(&mut text)
            .unwrap();
        let _ = send.send(text);
    });
    let line = receive.recv_timeout(std::time::Duration::from_secs(5));
    let _ = child.kill();
    child.wait().unwrap();
    std::fs::remove_file(path).unwrap();
    reader.join().unwrap();
    let line: serde_json::Value = serde_json::from_str(&line.unwrap()).unwrap();
    assert_eq!(line["event"], "start");
    assert_eq!(line["level"], "info");
    assert_eq!(line["version"], include_str!("../../../VERSION").trim());
}
