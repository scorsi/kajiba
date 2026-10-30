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
