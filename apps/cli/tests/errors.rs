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
