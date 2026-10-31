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
    let git = Git {
        generation: kajiba_core::new_id().unwrap(),
        repositories: Arc::new(crate::repositories::Repositories::new(root.into())),
        server_socket: root.join("gate.sock"),
    };
    let local = kajiba_core::local::Local::bind(
        &git.server_socket,
        Arc::default(),
        0o600,
        vec![kajiba_core::local::effective_uid()],
    )
    .unwrap();
    let server = ImportGate {
        repositories: git.repositories.clone(),
    };
    tokio::spawn(local.serve(
        tonic::service::Routes::new(gate::gate_service_server::GateServiceServer::new(server)),
        std::future::pending(),
    ));
    git
}

struct ImportGate {
    repositories: Arc<crate::repositories::Repositories>,
}

#[tonic::async_trait]
impl gate::gate_service_server::GateService for ImportGate {
    async fn hello(
        &self,
        _: Request<gate::HelloRequest>,
    ) -> Result<Response<gate::HelloResponse>, Status> {
        Ok(Response::new(gate::HelloResponse {
            version: env!("CARGO_PKG_VERSION").into(),
        }))
    }
    async fn open_session(
        &self,
        request: Request<gate::OpenSessionRequest>,
    ) -> Result<Response<gate::OpenSessionResponse>, Status> {
        let request = request.into_inner();
        let session = self
            .repositories
            .sessions()
            .into_iter()
            .find(|s| s.correlation_id == request.correlation_id)
            .unwrap();
        Ok(Response::new(gate::OpenSessionResponse {
            session: Some(session),
            ..Default::default()
        }))
    }
    async fn session_exited(
        &self,
        _: Request<gate::SessionExitedRequest>,
    ) -> Result<Response<gate::SessionExitedResponse>, Status> {
        Ok(Response::new(gate::SessionExitedResponse::default()))
    }
    async fn get_auth_key(
        &self,
        _: Request<gate::GetAuthKeyRequest>,
    ) -> Result<Response<gate::GetAuthKeyResponse>, Status> {
        Err(Status::unimplemented("test"))
    }
    async fn pre_receive(
        &self,
        _: Request<gate::PreReceiveRequest>,
    ) -> Result<Response<gate::PreReceiveResponse>, Status> {
        Err(Status::unimplemented("test"))
    }
    async fn post_receive(
        &self,
        _: Request<gate::PostReceiveRequest>,
    ) -> Result<Response<gate::PostReceiveResponse>, Status> {
        Err(Status::unimplemented("test"))
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
async fn quarantine_cannot_replace_history_or_reuse_an_identity() {
    let dir = TempDir::new();
    let lock = prepare(&dir.0).unwrap();
    let mut daemon = Git {
        generation: kajiba_core::new_id().unwrap(),
        repositories: Arc::new(crate::repositories::Repositories::new(dir.0.clone())),
        server_socket: dir.0.join("absent.sock"),
    };
    let target = dir.0.join(format!("repos/{ID}.git"));
    // Filesystem fixtures keep this regression independent of writing Git commands.
    for name in ["refs/heads", "objects/info", "objects/pack"] {
        std::fs::create_dir_all(target.join(name)).unwrap();
    }
    std::fs::write(target.join("HEAD"), "ref: refs/heads/main\n").unwrap();
    std::fs::write(target.join("config"), "[core]\n\tbare = true\n").unwrap();
    let original = "1111111111111111111111111111111111111111";
    std::fs::write(target.join("refs/heads/main"), format!("{original}\n")).unwrap();
    let replacement = || {
        vec![RemoteRef {
            ref_name: "refs/heads/main".into(),
            object_id: "2222222222222222222222222222222222222222".into(),
        }]
    };
    assert_eq!(
        daemon
            .quarantine_repository(Request::new(QuarantineRepositoryRequest {
                directory_name: format!("{ID}.git").into_bytes(),
            }))
            .await
            .unwrap_err()
            .code(),
        tonic::Code::FailedPrecondition
    );
    assert_eq!(
        daemon
            .create_repository(Request::new(CreateRepositoryRequest {
                repo_public_id: ID.into(),
                default_branch: "main".into(),
            }))
            .await
            .unwrap_err()
            .code(),
        tonic::Code::AlreadyExists
    );
    let fetch = request(&daemon, "/unused/source.git", replacement());
    // Exercise Fetch's shared guard without a Gate socket or a writing Git child.
    assert_eq!(
        daemon.repositories.fetch(fetch).unwrap_err().code(),
        tonic::Code::FailedPrecondition
    );
    assert_eq!(Gix.refs(&target).unwrap()[0].target, original);

    std::fs::remove_file(target.join("refs/heads/main")).unwrap();
    // Even without refs, stored history is not independently known to be empty.
    let object = target.join("objects/pack/history.pack");
    std::fs::write(&object, "preserve history").unwrap();
    assert_eq!(
        daemon
            .quarantine_repository(Request::new(QuarantineRepositoryRequest {
                directory_name: format!("{ID}.git").into_bytes()
            }))
            .await
            .unwrap_err()
            .code(),
        tonic::Code::FailedPrecondition
    );
    std::fs::remove_file(object).unwrap();
    let session = request(&daemon, "/unused/source.git", replacement())
        .session
        .unwrap();
    daemon
        .repositories
        .register(session.clone(), String::new())
        .unwrap();
    assert_eq!(
        daemon
            .quarantine_repository(Request::new(QuarantineRepositoryRequest {
                directory_name: format!("{ID}.git").into_bytes()
            }))
            .await
            .unwrap_err()
            .code(),
        tonic::Code::FailedPrecondition
    );
    daemon.repositories.finished(&session.session_id, 1);
    let destination = daemon
        .quarantine_repository(Request::new(QuarantineRepositoryRequest {
            directory_name: format!("{ID}.git").into_bytes(),
        }))
        .await
        .unwrap()
        .into_inner()
        .destination;
    assert!(Path::new(&destination).join("HEAD").is_file());
    assert_eq!(
        daemon.repositories.create(ID, "main").unwrap_err().code(),
        tonic::Code::AlreadyExists
    );

    // Restart cleanup must retain the identity reservation, including suffixed moves.
    std::fs::rename(&destination, format!("{destination}.3")).unwrap();
    drop(lock);
    let _lock = prepare(&dir.0).unwrap();
    daemon.repositories = Arc::new(crate::repositories::Repositories::new(dir.0.clone()));
    assert_eq!(
        daemon
            .create_repository(Request::new(CreateRepositoryRequest {
                repo_public_id: ID.into(),
                default_branch: "main".into()
            }))
            .await
            .unwrap_err()
            .code(),
        tonic::Code::AlreadyExists
    );
    assert!(!target.exists());
    let fetch = request(&daemon, "/unused/source.git", replacement());
    assert_eq!(
        daemon.repositories.fetch(fetch).unwrap_err().code(),
        tonic::Code::NotFound
    );
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
        assert!(
            git.repositories
                .metrics()
                .contains("kajiba_git_sessions_total{kind=\"import\",result=\"ok\"} 1\n")
        );
        let target = dir.0.join(format!("repos/{ID}.git"));
        assert_eq!(Gix.refs(&target).unwrap(), expected);
        assert_eq!(
            std::fs::read_to_string(target.join("HEAD")).unwrap(),
            "ref: refs/heads/main\n"
        );
        assert!(git.repositories.sessions().is_empty());
        // A server asking twice cannot overwrite the repository.
        let mut request = request;
        request.session.as_mut().unwrap().session_id = kajiba_core::new_id().unwrap();
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
        r.session.as_mut().unwrap().session_id = kajiba_core::new_id().unwrap();
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
    let mut request = request;
    request.session.as_mut().unwrap().session_id = kajiba_core::new_id().unwrap();
    assert_eq!(
        daemon
            .fetch(Request::new(request))
            .await
            .unwrap_err()
            .code(),
        tonic::Code::FailedPrecondition
    );
}

#[tokio::test]
async fn a_cancelled_fetch_rpc_keeps_the_child_accounted_until_wait_finishes() {
    let dir = TempDir::new();
    let _lock = prepare(&dir.0).unwrap();
    let git = Arc::new(service(&dir.0));
    git.repositories.create(ID, "main").unwrap();
    let remote = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let request = request(
        &git,
        &format!("git://{}/repo", remote.local_addr().unwrap()),
        vec![RemoteRef {
            ref_name: "refs/heads/main".into(),
            object_id: "1".repeat(40),
        }],
    );
    let session = request.session.clone().unwrap();
    let fetch = tokio::spawn({
        let git = git.clone();
        async move { git.fetch(Request::new(request)).await }
    });
    let (mut stream, _) = tokio::time::timeout(Duration::from_secs(5), remote.accept())
        .await
        .unwrap()
        .unwrap();
    use tokio::io::AsyncReadExt;
    let mut command = [0; 1024];
    assert!(stream.read(&mut command).await.unwrap() > 0);
    assert_eq!(
        git.list_sessions(Request::new(ListSessionsRequest {}))
            .await
            .unwrap()
            .into_inner()
            .sessions,
        std::slice::from_ref(&session)
    );
    fetch.abort();
    assert!(fetch.await.unwrap_err().is_cancelled());
    assert_eq!(git.repositories.sessions(), std::slice::from_ref(&session));
    assert!(git.repositories.exits().is_empty());
    let metrics = git.repositories.metrics();
    assert!(metrics.contains("kajiba_git_sessions_active{kind=\"import\"} 1\n"));
    assert!(metrics.contains("kajiba_git_sessions_total{kind=\"import\",result=\"error\"} 0\n"));
    drop(stream);
    tokio::time::timeout(Duration::from_secs(5), async {
        while git.repositories.exits().is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(git.repositories.sessions().is_empty());
    assert_eq!(git.repositories.exits(), [(session, 1)]);
    let metrics = git.repositories.metrics();
    assert!(metrics.contains("kajiba_git_sessions_active{kind=\"import\"} 0\n"));
    assert!(metrics.contains("kajiba_git_sessions_total{kind=\"import\",result=\"error\"} 1\n"));
}

// Run in a delegated cgroup v2 on Linux; ordinary Cargo sandboxes cannot create cgroups.
#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "requires KAJIBA_TEST_CGROUP pointing to a delegated cgroup v2"]
async fn cgroup_kill_during_fetch_empties_the_group_before_a_new_generation() {
    use tokio::io::AsyncReadExt;
    if let Some(root) = std::env::var_os("KAJIBA_TEST_FETCH_DAEMON") {
        let root = PathBuf::from(root);
        while !root.join("start").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let _lock = prepare(&root).unwrap();
        let git = service(&root);
        if !root.join(format!("repos/{ID}.git")).exists() {
            git.repositories.create(ID, "main").unwrap();
        }
        std::fs::write(root.join("generation"), &git.generation).unwrap();
        let request = request(
            &git,
            &std::env::var("KAJIBA_TEST_FETCH_SOURCE").unwrap(),
            vec![RemoteRef {
                ref_name: "refs/heads/main".into(),
                object_id: "1".repeat(40),
            }],
        );
        git.fetch(Request::new(request)).await.unwrap();
        panic!("fetch must remain blocked until its cgroup is killed");
    }
    let delegated =
        PathBuf::from(std::env::var_os("KAJIBA_TEST_CGROUP").expect("delegated cgroup required"));
    let cgroup = delegated.join(format!("kajiba-session-{}", std::process::id()));
    std::fs::create_dir(&cgroup).unwrap();
    let dir = TempDir::new();
    let remote = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut generations = Vec::new();
    for _ in 0..2 {
        let _ = std::fs::remove_file(dir.0.join("start"));
        let mut daemon = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--ignored", "--exact", "service::import_tests::cgroup_kill_during_fetch_empties_the_group_before_a_new_generation"])
            .env("KAJIBA_TEST_FETCH_DAEMON", &dir.0)
            .env("KAJIBA_TEST_FETCH_SOURCE", format!("git://{}/repo", remote.local_addr().unwrap()))
            .stdout(std::process::Stdio::null()).spawn().unwrap();
        std::fs::write(cgroup.join("cgroup.procs"), daemon.id().to_string()).unwrap();
        std::fs::write(dir.0.join("start"), "").unwrap();
        let (mut stream, _) = tokio::time::timeout(Duration::from_secs(10), remote.accept())
            .await
            .unwrap()
            .unwrap();
        let mut command = [0; 1024];
        assert!(
            stream.read(&mut command).await.unwrap() > 0,
            "a real fetch child is running"
        );
        generations.push(std::fs::read_to_string(dir.0.join("generation")).unwrap());
        std::fs::write(cgroup.join("cgroup.kill"), "1").unwrap();
        assert!(!daemon.wait().unwrap().success());
        tokio::time::timeout(Duration::from_secs(5), async {
            while !std::fs::read_to_string(cgroup.join("cgroup.events"))
                .unwrap()
                .contains("populated 0")
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            stream.read(&mut command).await.unwrap(),
            0,
            "fetch died with the daemon"
        );
    }
    assert_ne!(generations[0], generations[1]);
    std::fs::remove_dir(cgroup).unwrap();
}
