use super::*;
use crate::state::{prepare, tests::TempDir};
use git_service_server::GitService;
use kajiba_gitstore::{GitStore, Gix, Ref};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
const ID: &str = "00000000000000000000000001";

fn service(root: &Path) -> Git {
    Git {
        generation: kajiba_core::new_id().unwrap(),
        repositories: Arc::new(crate::repositories::Repositories::new(root.into())),
    }
}
fn request(git: &Git, source: &str, refs: Vec<RemoteRef>) -> FetchRequest {
    FetchRequest {
        session: Some(Session {
            session_id: kajiba_core::new_id().unwrap(),
            kind: SessionKind::Import.into(),
            repo_public_id: ID.into(),
            user_public_id: String::new(),
            correlation_id: "import-1".into(),
            generation: git.generation.clone(),
        }),
        repo_public_id: ID.into(),
        source: source.into(),
        credentials: String::new(),
        refs,
    }
}
fn git(repo: &std::path::Path, args: &[&str], stdin: &str) -> String {
    use std::io::Write;
    let mut child = std::process::Command::new("git")
        .arg("--git-dir")
        .arg(repo)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "a")
        .env("GIT_AUTHOR_EMAIL", "a@example.org")
        .env("GIT_COMMITTER_NAME", "a")
        .env("GIT_COMMITTER_EMAIL", "a@example.org")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "git {args:?}");
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}
/// A bare source with `main`, `feature/x`, a lightweight and an annotated tag, and a
/// note ref that import must skip.
fn source(dir: &Path, branch: &str) -> PathBuf {
    let path = dir.join(format!("source-{branch}.git"));
    Gix.init_bare(&path, branch).unwrap();
    let tree = git(&path, &["mktree"], "");
    let one = git(&path, &["commit-tree", &tree, "-m", "one"], "");
    let two = git(&path, &["commit-tree", &tree, "-p", &one, "-m", "two"], "");
    git(
        &path,
        &["update-ref", &format!("refs/heads/{branch}"), &two],
        "",
    );
    git(&path, &["update-ref", "refs/heads/feature/x", &one], "");
    git(&path, &["update-ref", "refs/tags/v1", &one], "");
    git(&path, &["tag", "-a", "v2", "-m", "release", &two], "");
    git(&path, &["update-ref", "refs/notes/commits", &one], "");
    path
}

fn branches_and_tags(path: &Path) -> Vec<Ref> {
    Gix.refs(path)
        .unwrap()
        .into_iter()
        .filter(|r| r.name.starts_with("refs/heads/") || r.name.starts_with("refs/tags/"))
        .collect()
}

#[tokio::test]
async fn imports_paths_bundles_and_urls_preserving_refs_tags_and_head() {
    for kind in ["path", "bundle", "url"] {
        let dir = TempDir::new();
        let _lock = prepare(&dir.0).unwrap();
        let git = service(&dir.0);
        git.repositories.create(ID, "main").unwrap();
        let from = source(&dir.0, "main");
        let expected = branches_and_tags(&from);
        let bundle = dir.0.join("source.bundle");
        self::git(
            &from,
            &["bundle", "create", bundle.to_str().unwrap(), "--all"],
            "",
        );
        let from = match kind {
            "bundle" => bundle.display().to_string(),
            "url" => format!("file://{}", from.display()),
            _ => from.display().to_string(),
        };
        let refs = git
            .list_remote_refs(Request::new(ListRemoteRefsRequest {
                source: from.clone(),
                credentials: String::new(),
            }))
            .await
            .unwrap()
            .into_inner()
            .refs;
        assert_eq!(refs.len(), 4);
        assert!(
            refs.iter()
                .all(|r| r.ref_name.starts_with("refs/heads/")
                    || r.ref_name.starts_with("refs/tags/"))
        );
        let request = request(&git, &from, refs);
        git.fetch(Request::new(request.clone())).await.unwrap();
        let target = dir.0.join(format!("repos/{ID}.git"));
        assert_eq!(Gix.refs(&target).unwrap(), expected);
        assert_eq!(
            std::fs::read_to_string(target.join("HEAD")).unwrap(),
            "ref: refs/heads/main\n"
        );
        assert!(git.repositories.sessions().is_empty());
        // A server asking twice cannot overwrite the repository.
        let err = git.fetch(Request::new(request)).await.unwrap_err();
        assert_eq!(err.code(), tonic::Code::FailedPrecondition);
        assert_eq!(Gix.refs(&target).unwrap(), expected);
    }
}

#[tokio::test]
async fn fetch_refuses_bad_sessions_paths_refs_and_sources_without_writing() {
    let dir = TempDir::new();
    let _lock = prepare(&dir.0).unwrap();
    let git = service(&dir.0);
    git.repositories.create(ID, "main").unwrap();
    let from = source(&dir.0, "main").display().to_string();
    let refs = git
        .list_remote_refs(Request::new(ListRemoteRefsRequest {
            source: from.clone(),
            credentials: String::new(),
        }))
        .await
        .unwrap()
        .into_inner()
        .refs;
    let valid = request(&git, &from, refs);
    for field in [
        "missing session",
        "kind",
        "generation",
        "user",
        "session repo",
        "session id",
        "repo",
        "unknown repo",
        "prefix",
        "ref",
        "id",
        "zero",
        "duplicate",
        "empty",
        "source",
    ] {
        let mut r = valid.clone();
        match field {
            "missing session" => r.session = None,
            "kind" => r.session.as_mut().unwrap().kind = SessionKind::ReceivePack.into(),
            "generation" => r.session.as_mut().unwrap().generation = kajiba_core::new_id().unwrap(),
            "user" => r.session.as_mut().unwrap().user_public_id = ID.into(),
            "session repo" => r.session.as_mut().unwrap().repo_public_id.clear(),
            "session id" => r.session.as_mut().unwrap().session_id.clear(),
            "repo" | "unknown repo" => {
                r.repo_public_id = if field == "repo" {
                    "../escape".into()
                } else {
                    kajiba_core::new_id().unwrap()
                };
                r.session.as_mut().unwrap().repo_public_id = r.repo_public_id.clone();
            }
            "prefix" => r.refs[0].ref_name = "refs/notes/commits".into(),
            "ref" => r.refs[0].ref_name = "refs/heads/../main".into(),
            "id" => r.refs[0].object_id = "HEAD\nrefs/heads/escape".into(),
            "zero" => r.refs[0].object_id = "0".repeat(40),
            "duplicate" => r.refs.push(r.refs[0].clone()),
            "empty" => r.refs.clear(),
            "source" => r.source = "https://user:secret@example.org/repo".into(),
            _ => unreachable!(),
        }
        let err = git.fetch(Request::new(r)).await.unwrap_err();
        assert_eq!(
            err.code(),
            if field == "unknown repo" {
                tonic::Code::NotFound
            } else {
                tonic::Code::InvalidArgument
            },
            "{field}: {err}"
        );
        assert!(!err.message().contains("secret"));
        assert!(
            Gix.refs(&dir.0.join(format!("repos/{ID}.git")))
                .unwrap()
                .is_empty()
        );
    }
    for from in [
        "relative",
        "https://user:secret@example.org/repo",
        "https://example.org/repo?secret",
    ] {
        let err = git
            .list_remote_refs(Request::new(ListRemoteRefsRequest {
                source: from.into(),
                credentials: "secret".into(),
            }))
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
        assert!(!err.message().contains("secret"));
    }
    let mut missing = valid;
    missing.source = dir.0.join("absent-secret").display().to_string();
    let err = git.fetch(Request::new(missing)).await.unwrap_err();
    assert!(matches!(
        err.message(),
        "source not found" | "not a git repository"
    ));
    assert!(git.repositories.sessions().is_empty());
}

#[test]
fn deadline_errors_report_completed_children_without_exposing_stderr() {
    let err = crate::repositories::source_error(kajiba_gitstore::Error::Git(
        "git did not finish within 3600 s: secret".into(),
    ));
    assert_eq!(err.code(), tonic::Code::FailedPrecondition);
    assert_eq!(err.message(), "timed out");
}

#[tokio::test]
async fn imports_exact_admitted_ids_and_refuses_a_target_with_only_a_note() {
    let dir = TempDir::new();
    let _lock = prepare(&dir.0).unwrap();
    let daemon = service(&dir.0);
    daemon.repositories.create(ID, "main").unwrap();
    let from = source(&dir.0, "main");
    let refs = daemon
        .list_remote_refs(Request::new(ListRemoteRefsRequest {
            source: from.display().to_string(),
            credentials: String::new(),
        }))
        .await
        .unwrap()
        .into_inner()
        .refs;
    let expected = branches_and_tags(&from);
    // The source advances after admission: fetch must use the recorded object ids.
    let tree = git(&from, &["mktree"], "");
    let tip = git(
        &from,
        &[
            "commit-tree",
            &tree,
            "-p",
            &refs[0].object_id,
            "-m",
            "later",
        ],
        "",
    );
    git(&from, &["update-ref", "refs/heads/main", &tip], "");
    let request = request(&daemon, from.to_str().unwrap(), refs);
    daemon.fetch(Request::new(request.clone())).await.unwrap();
    let target = dir.0.join(format!("repos/{ID}.git"));
    assert_eq!(Gix.refs(&target).unwrap(), expected);
    git(
        &target,
        &["update-ref", "refs/notes/commits", &expected[0].target],
        "",
    );
    for r in expected {
        git(&target, &["update-ref", "-d", &r.name], "");
    }
    assert_eq!(Gix.refs(&target).unwrap().len(), 1);
    assert_eq!(
        daemon
            .fetch(Request::new(request))
            .await
            .unwrap_err()
            .code(),
        tonic::Code::FailedPrecondition
    );
}
