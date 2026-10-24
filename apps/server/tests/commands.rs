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
        vec!["config", "check", "server.toml"],
        vec!["config", "check", "--files", "server.toml"],
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
