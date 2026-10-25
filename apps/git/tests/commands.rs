use std::process::{Command, Output};

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_kajiba-git"))
        .args(args)
        .env_remove("RUST_LOG")
        .output()
        .expect("run kajiba-git")
}

#[test]
fn version_matches_repository_version() {
    let output = run(&["--version"]);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(
        std::str::from_utf8(&output.stdout).unwrap(),
        format!("kajiba-git {}\n", include_str!("../../../VERSION").trim())
    );
    assert!(output.stderr.is_empty(), "{output:?}");
}

#[test]
fn help_succeeds_and_usage_errors_exit_two() {
    for args in [
        vec!["--help"],
        vec!["serve", "--help"],
        vec!["config", "check", "--help"],
    ] {
        let output = run(&args);
        assert_eq!(output.status.code(), Some(0), "{args:?}: {output:?}");
        assert!(!output.stdout.is_empty() && output.stderr.is_empty());
    }
    for args in [
        vec![],
        vec!["unknown"],
        vec!["--config", "git.toml"],
        vec!["serve"],
        vec!["serve", "--config"],
        vec!["serve", "--config", "git.toml", "extra"],
        vec!["config"],
        vec!["config", "unknown"],
        vec!["config", "check"],
        vec!["config", "check", "git.toml", "--unknown"],
    ] {
        let output = run(&args);
        assert_eq!(output.status.code(), Some(2), "{args:?}: {output:?}");
        assert!(output.stdout.is_empty());
        let line: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(line["event"], "fatal");
        assert_eq!(line["level"], "error");
        assert_eq!(line["err"], "invalid command line; use --help");
    }
}

#[test]
fn config_check_loads_example() {
    let fixture = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../crates/kajiba-core/tests/fixtures/git.toml"
    );
    let output = run(&["config", "check", fixture]);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(output.stdout.is_empty() && output.stderr.is_empty());
}

#[test]
fn both_commands_reject_invalid_configuration_naming_the_field() {
    let dir = std::env::temp_dir().join(format!("kajiba-git-config-{}", std::process::id()));
    std::fs::create_dir(&dir).unwrap();
    let path = dir.join("git.toml");
    for (input, field) in [
        (
            "hooks_path = '/nix/store/example-hooks'\ncolour = 'blue'",
            "colour",
        ),
        (
            "hooks_path = '/nix/store/example-hooks'\nstate_dir = 'relative'",
            "state_dir",
        ),
        ("state_dir = '/var/lib/kajiba-git'", "hooks_path"),
    ] {
        std::fs::write(&path, input).unwrap();
        for prefix in [["config", "check"], ["serve", "--config"]] {
            let output = run(&[prefix[0], prefix[1], path.to_str().unwrap()]);
            assert_eq!(output.status.code(), Some(1), "{output:?}");
            assert!(output.stdout.is_empty());
            let line: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
            assert_eq!(line["event"], "config-invalid");
            assert!(line["err"].as_str().unwrap().contains(field), "{line}");
        }
    }
    std::fs::remove_file(&path).unwrap();
    for prefix in [["config", "check"], ["serve", "--config"]] {
        let output = run(&[prefix[0], prefix[1], path.to_str().unwrap()]);
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        let line: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(line["event"], "config-invalid");
    }
    std::fs::remove_dir(dir).unwrap();
}
