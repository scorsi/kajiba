use std::process::Command;

#[test]
fn usage_error_is_json_and_help_stays_on_stdout() {
    let output = Command::new(env!("CARGO_BIN_EXE_kajiba-runner"))
        .arg("--unknown")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let line: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(line["event"], "fatal");
    assert_eq!(line["level"], "error");
    assert_eq!(line["err"], "invalid command line; use --help");
    for arg in ["--help", "--version"] {
        let output = Command::new(env!("CARGO_BIN_EXE_kajiba-runner"))
            .arg(arg)
            .output()
            .unwrap();
        assert!(output.status.success());
        assert!(!output.stdout.is_empty());
        assert!(output.stderr.is_empty());
    }
}
