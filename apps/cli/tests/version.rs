use std::process::Command;

#[test]
fn version_matches_repository_version() {
    let output = Command::new(env!("CARGO_BIN_EXE_kajiba"))
        .arg("--version")
        .output()
        .expect("run kajiba --version");

    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        std::str::from_utf8(&output.stdout).expect("UTF-8 version output"),
        format!("kajiba {}\n", include_str!("../../../VERSION").trim())
    );
    assert!(output.stderr.is_empty(), "{output:?}");
}
