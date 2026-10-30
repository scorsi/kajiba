use super::*;
use crate::client::tests::{Fake, TOKEN, TempDir, refusal};
use std::{ffi::OsString, os::unix::fs::PermissionsExt};
use tonic::Code;

fn no_env(_: &str) -> Option<OsString> {
    None
}

async fn login(token_file: &Path, url: &str, input: &str) -> (Result<(), Error>, String) {
    let mut out = Vec::new();
    let result = run(
        token_file,
        Some(url),
        &no_env,
        &mut input.as_bytes(),
        false,
        &mut out,
    )
    .await;
    (result, String::from_utf8(out).unwrap())
}

fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn overlapping_saves_preserve_whole_files_and_unrelated_temporary_files() {
    let dir = TempDir::new();
    let path = dir.0.join("token");
    let unrelated = dir.0.join("token.tmp");
    fs::write(&unrelated, "unrelated file").unwrap();
    let contents: Vec<_> = (0..8).map(|i| i.to_string().repeat(32 * 1024)).collect();
    let start = std::sync::Barrier::new(contents.len());
    std::thread::scope(|scope| {
        let handles: Vec<_> = contents
            .iter()
            .map(|content| {
                scope.spawn(|| {
                    start.wait();
                    save(&path, content).unwrap();
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
    });
    let saved = fs::read_to_string(&path).unwrap();
    assert!(
        contents
            .iter()
            .any(|content| saved == format!("{content}\n"))
    );
    assert_eq!(mode(&path), 0o600);
    assert_eq!(fs::read_to_string(&unrelated).unwrap(), "unrelated file");
    assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 2);
}

#[test]
fn a_failed_save_cleans_up_only_its_temporary_file() {
    let dir = TempDir::new();
    let path = dir.0.join("token");
    fs::create_dir(&path).unwrap();
    let unrelated = dir.0.join("token.tmp");
    fs::write(&unrelated, "unrelated file").unwrap();
    assert!(save(&path, TOKEN).is_err());
    assert!(path.is_dir());
    assert_eq!(fs::read_to_string(&unrelated).unwrap(), "unrelated file");
    assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 2);
}

#[tokio::test]
async fn saves_a_checked_token_and_the_url_mode_0600() {
    let fake = Fake::default();
    fake.0.lock().unwrap().viewer = "alice".into();
    let url = fake.serve().await;
    let dir = TempDir::new();
    let token_file = dir.0.join("nested/kajiba/token");
    let (result, out) = login(&token_file, &url, &format!("{TOKEN}\n")).await;
    result.unwrap();
    assert_eq!(out, format!("logged in to {url} as alice\n"));
    assert!(!out.contains(TOKEN));
    assert_eq!(
        fs::read_to_string(&token_file).unwrap(),
        format!("{TOKEN}\n")
    );
    assert_eq!(mode(&token_file), 0o600);
    assert_eq!(mode(token_file.parent().unwrap()), 0o700);
    let saved = token_file.with_file_name("url");
    assert_eq!(fs::read_to_string(&saved).unwrap(), format!("{url}\n"));
    // The check call carried the new token.
    let calls = &fake.0.lock().unwrap().calls;
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "GetViewer");
    assert_eq!(
        calls[0].1.get("authorization").unwrap(),
        &format!("Bearer {TOKEN}")
    );
}

#[tokio::test]
async fn overwrites_a_previous_token_with_mode_0600() {
    let fake = Fake::default();
    let url = fake.serve().await;
    let dir = TempDir::new();
    let token_file = dir.0.join("token");
    fs::write(&token_file, "kjb_old\n").unwrap();
    fs::set_permissions(&token_file, fs::Permissions::from_mode(0o644)).unwrap();
    login(&token_file, &url, TOKEN).await.0.unwrap();
    assert_eq!(
        fs::read_to_string(&token_file).unwrap(),
        format!("{TOKEN}\n")
    );
    assert_eq!(mode(&token_file), 0o600);
    assert_eq!(
        fs::read_dir(&dir.0).unwrap().count(),
        2,
        "token and url only"
    );
}

#[tokio::test]
async fn malformed_tokens_are_refused_before_any_call() {
    let fake = Fake::default();
    let url = fake.serve().await;
    let dir = TempDir::new();
    let token_file = dir.0.join("token");
    for input in [
        "",
        "kjb_short",
        "ghp_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        "kjb_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
        "kjb_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
    ] {
        let (result, out) = login(&token_file, &url, input).await;
        assert_eq!(
            result.unwrap_err().to_string(),
            "not a personal access token: expected kjb_ and 43 base64url characters",
            "{input}"
        );
        assert!(out.is_empty());
    }
    assert!(fake.0.lock().unwrap().calls.is_empty());
    assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 0);
}

#[tokio::test]
async fn a_refused_token_leaves_the_files_unchanged() {
    let fake = Fake::default();
    fake.0.lock().unwrap().refusal = Some(refusal(
        Code::Unauthenticated,
        "CREDENTIAL_INVALID",
        "credential invalid",
    ));
    let url = fake.serve().await;
    let dir = TempDir::new();
    let token_file = dir.0.join("token");
    fs::write(&token_file, "kjb_previous\n").unwrap();
    let (result, out) = login(&token_file, &url, TOKEN).await;
    let err = result.unwrap_err().to_string();
    assert!(
        err.starts_with("credential invalid (CREDENTIAL_INVALID, request "),
        "{err}"
    );
    assert!(!err.contains(TOKEN) && out.is_empty());
    assert_eq!(fs::read_to_string(&token_file).unwrap(), "kjb_previous\n");
    assert!(!token_file.with_file_name("url").exists());
}

#[tokio::test]
async fn the_url_comes_from_the_environment_or_the_previous_login() {
    let fake = Fake::default();
    let url = fake.serve().await;
    let dir = TempDir::new();
    let token_file = dir.0.join("token");
    let env = |name: &str| (name == "KAJIBA_URL").then(|| OsString::from(&url));
    run(
        &token_file,
        None,
        &env,
        &mut TOKEN.as_bytes(),
        false,
        &mut Vec::new(),
    )
    .await
    .unwrap();
    // Saved: the next login needs neither --url nor KAJIBA_URL.
    run(
        &token_file,
        None,
        &no_env,
        &mut TOKEN.as_bytes(),
        false,
        &mut Vec::new(),
    )
    .await
    .unwrap();
    assert_eq!(fake.0.lock().unwrap().calls.len(), 2);
}
