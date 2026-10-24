use std::process::{Command, Output};

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_kajiba-server"))
        .args(args)
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
        assert_eq!(
            String::from_utf8(output.stderr).unwrap(),
            format!("kajiba-server: {name}: not implemented yet\n")
        );
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
