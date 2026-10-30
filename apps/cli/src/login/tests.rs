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
async fn saves_the_url_and_the_checked_token_in_one_file_mode_0600() {
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
        format!("{url}\n{TOKEN}\n")
    );
    assert_eq!(mode(&token_file), 0o600);
    assert_eq!(mode(token_file.parent().unwrap()), 0o700);
    assert_eq!(
        fs::read_dir(token_file.parent().unwrap()).unwrap().count(),
        1
    );
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
        format!("{url}\n{TOKEN}\n")
    );
    assert_eq!(mode(&token_file), 0o600);
    assert_eq!(
        fs::read_dir(&dir.0).unwrap().count(),
        1,
        "the token file only"
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
    fs::write(&token_file, "https://example.org\nkjb_previous\n").unwrap();
    let (result, out) = login(&token_file, &url, TOKEN).await;
    let err = result.unwrap_err().to_string();
    assert!(
        err.starts_with("credential invalid (CREDENTIAL_INVALID, request "),
        "{err}"
    );
    assert!(!err.contains(TOKEN) && out.is_empty());
    assert_eq!(
        fs::read_to_string(&token_file).unwrap(),
        "https://example.org\nkjb_previous\n"
    );
    assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 1);
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

#[tokio::test]
async fn token_files_in_one_directory_keep_their_own_forge() {
    let (a, b) = (Fake::default(), Fake::default());
    let (url_a, url_b) = (a.serve().await, b.serve().await);
    let dir = TempDir::new();
    let (file_a, file_b) = (dir.0.join("a"), dir.0.join("b"));
    login(&file_a, &url_a, TOKEN).await.0.unwrap();
    login(&file_b, &url_b, TOKEN).await.0.unwrap();
    // Re-login without --url reuses each file's own forge.
    for file in [&file_a, &file_b] {
        run(
            file,
            None,
            &no_env,
            &mut TOKEN.as_bytes(),
            false,
            &mut Vec::new(),
        )
        .await
        .unwrap();
    }
    assert_eq!(a.0.lock().unwrap().calls.len(), 2);
    assert_eq!(b.0.lock().unwrap().calls.len(), 2);
    assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 2);
}

#[tokio::test]
async fn without_url_or_previous_login_nothing_is_called() {
    let dir = TempDir::new();
    let token_file = dir.0.join("token");
    let mut out = Vec::new();
    let err = run(
        &token_file,
        None,
        &no_env,
        &mut TOKEN.as_bytes(),
        false,
        &mut out,
    )
    .await
    .unwrap_err();
    assert_eq!(
        err.to_string(),
        "no forge URL: pass --url or set KAJIBA_URL"
    );
    assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 0);
}

#[tokio::test]
async fn login_refuses_plain_http_to_another_machine_before_any_call() {
    let dir = TempDir::new();
    let token_file = dir.0.join("token");
    let (result, out) = login(&token_file, "http://forge.example.org", TOKEN).await;
    assert!(
        result
            .unwrap_err()
            .to_string()
            .starts_with("refusing to send a token to http://forge.example.org: "),
    );
    assert!(out.is_empty());
    assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 0);
}
