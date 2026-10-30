use std::{io::Write, process::Command, process::Stdio};

#[test]
fn a_failure_prints_kajiba_message_and_exits_1() {
    let dir = std::env::temp_dir().join(format!("kj-cli-errors-{}", std::process::id()));
    let mut child = Command::new(env!("CARGO_BIN_EXE_kajiba"))
        .args(["login", "--token-file"])
        .arg(dir.join("token"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run kajiba login");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"kjb_not-a-token\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    assert_eq!(
        std::str::from_utf8(&output.stderr).unwrap(),
        "kajiba: not a personal access token: expected kjb_ and 43 base64url characters\n"
    );
    assert!(!dir.exists());
}

#[test]
fn repo_commands_need_a_token_file() {
    let token = std::env::temp_dir().join(format!("kj-cli-none-{}/token", std::process::id()));
    let output = Command::new(env!("CARGO_BIN_EXE_kajiba"))
        .args([
            "repo",
            "list",
            "--url",
            "http://127.0.0.1:1",
            "--token-file",
        ])
        .arg(&token)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        format!(
            "kajiba: no token file at {}; run kajiba login\n",
            token.display()
        )
    );
}

/// Plain http to loopback only with --development, which warns once (ADRs 0099, 0104).
#[test]
fn loopback_http_needs_the_development_mode() {
    let dir = std::env::temp_dir().join(format!("kj-cli-dev-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let token = dir.join("token");
    std::fs::write(
        &token,
        format!("http://127.0.0.1:1\nkjb_{}\n", "A".repeat(43)),
    )
    .unwrap();
    let kajiba = |development: bool| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_kajiba"));
        command.args(["repo", "list", "--token-file"]).arg(&token);
        if development {
            command.arg("--development");
        }
        let output = command.output().unwrap();
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert!(output.stdout.is_empty(), "{output:?}");
        String::from_utf8(output.stderr).unwrap()
    };
    assert_eq!(
        kajiba(false),
        "kajiba: refusing to send a credential to http://127.0.0.1:1: the URL must be https \
         (http only to localhost, 127.0.0.1 or [::1], in development mode)\n"
    );
    let warned = kajiba(true);
    let warning = "kajiba: warning: development mode: sending the token over plain http to http://127.0.0.1:1\n";
    assert!(warned.starts_with(warning), "{warned}");
    assert_eq!(warned.matches("warning").count(), 1, "{warned}");
    std::fs::remove_dir_all(&dir).unwrap();
}
