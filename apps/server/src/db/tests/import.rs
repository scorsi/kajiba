use super::repos::{State, git};
use crate::db::{
    Writer,
    events::EventBus,
    pushes::{Gate, Refused},
};
use crate::import::{Error, import, supervise};
use kajiba_git::{GitStore, Gix, Ref};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

const DECLARED: &str = "[repos.demo]\nowner='alice'\ndescription=''\n\
     [repos.other]\nowner='alice'\ndescription=''\n\
     [repos.third]\nowner='alice'\ndescription=''\n\
     [repos.gone]\nowner='bob'\ndescription=''\n";

struct Import {
    state_dir: PathBuf,
    writer: Writer,
    gate: Arc<Gate>,
    bus: Arc<EventBus>,
    _state: State,
}

impl Import {
    async fn new() -> Self {
        let mut state = State::new().await;
        state.apply(DECLARED).await.unwrap();
        // Archive `gone`: it is no longer declared.
        state
            .apply(&DECLARED.replace("[repos.gone]\nowner='bob'\ndescription=''\n", ""))
            .await
            .unwrap();
        let state_dir = state.dir.0.clone();
        let writer = std::mem::replace(
            &mut state.writer,
            sqlx::Connection::connect("sqlite::memory:").await.unwrap(),
        );
        Self {
            state_dir,
            writer: Writer::new(writer.into()),
            gate: Arc::default(),
            bus: Arc::default(),
            _state: state,
        }
    }

    async fn run(&self, repo: &str, source: &str) -> Result<usize, Error> {
        import(
            &self.writer,
            &self.gate,
            &self.bus,
            &self.state_dir,
            repo,
            source,
            "",
            "request-1",
        )
        .await
    }

    async fn strings(&self, sql: &str) -> Vec<String> {
        sqlx::query_scalar(sql)
            .fetch_all(&mut *self.writer.lock().await)
            .await
            .unwrap()
    }

    async fn changes(&self) -> i64 {
        super::users::changes(&mut *self.writer.lock().await).await
    }

    async fn path(&self, key: &str) -> PathBuf {
        let id = self
            .strings(&format!(
                "SELECT public_id FROM repo WHERE config_key = '{key}'"
            ))
            .await;
        self.state_dir.join(format!("repos/{}.git", id[0]))
    }
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
async fn imports_a_path_a_bundle_and_a_url_with_refs_tags_and_head() {
    let import = Import::new().await;
    let source = source(&import.state_dir, "main");
    let bundle = import.state_dir.join("source.bundle");
    git(
        &source,
        &["bundle", "create", bundle.to_str().unwrap(), "--all"],
        "",
    );
    let expected = branches_and_tags(&source);
    assert_eq!(expected.len(), 4);
    for (key, from) in [
        ("demo", source.display().to_string()),
        ("other", bundle.display().to_string()),
        ("third", format!("file://{}", source.display())),
    ] {
        assert_eq!(
            import.run(&format!("Alice/{key}"), &from).await.unwrap(),
            4,
            "{key}"
        );
        let target = import.path(key).await;
        assert_eq!(Gix.refs(&target).unwrap(), expected, "{key}");
        assert_eq!(
            std::fs::read_to_string(target.join("HEAD")).unwrap(),
            "ref: refs/heads/main\n"
        );
    }
    let events = import
        .strings("SELECT principal_kind || ifnull(principal_id, '-') || ' ' || correlation_id || ' ' || payload FROM event WHERE kind = 'push' AND repo_id = 1 ORDER BY id")
        .await;
    let wanted: Vec<String> = expected
        .iter()
        .map(|r| {
            format!(
                r#"system- request-1 {{"new":"{}","old":"{}","ref":"{}"}}"#,
                r.target,
                crate::db::pushes::ZERO,
                r.name
            )
        })
        .collect();
    assert_eq!(events, wanted);
    assert_eq!(
        import.strings("SELECT count(*) || '' FROM event").await,
        ["12"]
    );
    assert!(
        import
            .strings("SELECT correlation_id FROM pending_push")
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn refusals_write_nothing() {
    let import = Import::new().await;
    let source = source(&import.state_dir, "main");
    let trunk = self::source(&import.state_dir, "trunk");
    import
        .run("alice/demo", source.to_str().unwrap())
        .await
        .unwrap();
    let before = import.changes().await;
    let refs = Gix.refs(&import.path("other").await).unwrap();
    for (repo, from, check) in [
        ("alice/missing", &source, "not found"),
        ("bob/gone", &source, "not found"),
        ("alice/demo", &source, "not empty"),
        ("alice/other", &trunk, "no default branch"),
        ("alice/other", &import.state_dir.join("absent"), "source"),
        (
            "alice/other",
            &PathBuf::from("relative/source.git"),
            "source",
        ),
        (
            "alice/other",
            &PathBuf::from("https://user:secret@127.0.0.1:9/demo.git"),
            "source",
        ),
    ] {
        let error = import.run(repo, from.to_str().unwrap()).await.unwrap_err();
        let matched = match (&error, check) {
            (Error::NotFound, "not found") => true,
            (Error::NotEmpty(_), "not empty") => true,
            (Error::NoDefaultBranch(head), "no default branch") => head == "refs/heads/main",
            (Error::Source(_), "source") => true,
            _ => false,
        };
        assert!(matched, "{repo}: {error:?}");
        assert_eq!(import.changes().await, before, "{repo}");
        assert_eq!(Gix.refs(&import.path("other").await).unwrap(), refs);
    }
}

#[tokio::test]
async fn a_crash_during_import_is_reconciled_at_restart() {
    let import = Import::new().await;
    let source = source(&import.state_dir, "main");
    let refs = Gix.remote_refs(source.to_str().unwrap(), "").unwrap();
    let target = import.path("demo").await;
    // The server dies after the fetch, before reconciliation.
    let updates: Vec<_> = refs
        .iter()
        .map(|r| crate::db::pushes::RefUpdate {
            name: r.name.clone(),
            old: crate::db::pushes::ZERO.into(),
            new: r.target.clone(),
        })
        .collect();
    import
        .gate
        .admit(
            &mut *import.writer.lock().await,
            1,
            None,
            "request-1",
            &updates,
        )
        .await
        .unwrap();
    Gix.fetch(&target, source.to_str().unwrap(), "", &refs)
        .unwrap();
    // The connections die with the server; the new instance reopens the file.
    let (reopened, _) = crate::db::open(&import.state_dir.join("db/kajiba.sqlite"))
        .await
        .unwrap();
    *import.writer.lock().await = reopened;
    let restarted = Gate::default();
    restarted
        .reconcile(
            &mut *import.writer.lock().await,
            &import.bus,
            &import.state_dir,
            &Gix,
        )
        .await
        .unwrap();
    assert_eq!(
        import
            .strings("SELECT kind FROM event WHERE principal_kind = 'system'")
            .await
            .len(),
        4
    );
    assert!(
        import
            .strings("SELECT correlation_id FROM pending_push")
            .await
            .is_empty()
    );
    assert!(matches!(
        import.run("alice/demo", source.to_str().unwrap()).await,
        Err(Error::NotEmpty(_))
    ));
}

#[test]
fn sources_carrying_credentials_are_refused() {
    use crate::import::check_source;
    for source in [
        "/srv/source.git",
        "file:///srv/source.bundle",
        "https://example.org/demo.git",
        "git://example.org/demo.git",
        "ssh://git@example.org:2222/demo.git",
    ] {
        assert_eq!(check_source(source), Ok(()), "{source}");
    }
    for (source, refused) in [
        ("relative/source.git", "not an absolute path"),
        (
            "https://alice@example.org/demo.git",
            "credentials in the source",
        ),
        (
            "https://alice:token@example.org/demo.git",
            "credentials in the source",
        ),
        (
            "git://alice@example.org/demo.git",
            "credentials in the source",
        ),
        (
            "ssh://git:token@example.org/demo.git",
            "credentials in the source",
        ),
        (
            "https://example.org/demo.git?token=abc",
            "a query or fragment",
        ),
        ("https://example.org/demo.git#token", "a query or fragment"),
    ] {
        let err = check_source(source).unwrap_err();
        assert!(err.contains(refused), "{source}: {err}");
    }
}

#[test]
fn source_reasons_are_fixed_classes_without_any_stderr_text() {
    use crate::import::reason;
    // A token in the path, a credential echoed on its own, a secret in a local path:
    // only the class comes back.
    for (stderr, class) in [
        (
            "fatal: unable to access 'https://host/secret-token/repo/': \
             The requested URL returned error: 401",
            "authentication failed",
        ),
        (
            "remote: password secret rejected\nfatal: Authentication failed for 'https://host/x/'",
            "authentication failed",
        ),
        (
            "git@host: Permission denied (publickey).\nfatal: Could not read from remote repository.",
            "authentication failed",
        ),
        (
            "remote: Repository not found.\nfatal: repository 'https://host/secret/' not found",
            "source not found",
        ),
        (
            "fatal: '/srv/secret-dir' does not appear to be a git repository",
            "not a git repository",
        ),
        (
            "fatal: unable to access 'https://secret@host/': Could not resolve host: host",
            "network error",
        ),
        ("git did not finish within 3600 s", "timed out"),
        (
            "fatal: unable to access 'https://h/': Operation too slow",
            "timed out",
        ),
        (
            "remote: secret\nfatal: something else entirely",
            "other error",
        ),
        ("", "other error"),
    ] {
        let got = reason(stderr);
        assert_eq!(got, class, "{stderr}");
        assert!(!got.contains("secret"));
    }
}

#[tokio::test]
async fn a_dropped_import_future_still_releases_the_gate_and_reconciles() {
    let import = Import::new().await;
    let source = source(&import.state_dir, "main");
    let refs = Gix.remote_refs(source.to_str().unwrap(), "").unwrap();
    let target = import.path("demo").await;
    let updates: Vec<_> = refs
        .iter()
        .map(|r| crate::db::pushes::RefUpdate {
            name: r.name.clone(),
            old: crate::db::pushes::ZERO.into(),
            new: r.target.clone(),
        })
        .collect();
    import
        .gate
        .admit(
            &mut *import.writer.lock().await,
            1,
            None,
            "request-1",
            &updates,
        )
        .await
        .unwrap();
    // The fetch blocks until released; meanwhile the RPC future is dropped.
    let (release, blocked) = std::sync::mpsc::channel::<()>();
    let fetch = {
        let (target, source, refs) = (target.clone(), source.clone(), refs.clone());
        move || {
            blocked.recv().unwrap();
            Gix.fetch(&target, source.to_str().unwrap(), "", &refs)
        }
    };
    drop(supervise(
        import.writer.clone(),
        import.gate.clone(),
        import.bus.clone(),
        import.state_dir.clone(),
        1,
        fetch,
    ));
    let refused = import
        .gate
        .admit(&mut *import.writer.lock().await, 1, None, "request-2", &[])
        .await;
    assert!(matches!(refused, Err(Refused::InProgress)), "{refused:?}");
    release.send(()).unwrap();
    for _ in 0..200 {
        if import
            .strings("SELECT correlation_id FROM pending_push")
            .await
            .is_empty()
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        import
            .strings("SELECT kind FROM event WHERE principal_kind = 'system'")
            .await
            .len(),
        4
    );
    assert_eq!(Gix.refs(&target).unwrap().len(), 4);
    // The gate is released: a new mutation is admitted.
    import
        .gate
        .admit(&mut *import.writer.lock().await, 1, None, "request-3", &[])
        .await
        .unwrap();
}

#[tokio::test]
async fn another_repository_s_broken_row_does_not_fail_the_import() {
    let import = Import::new().await;
    let source = source(&import.state_dir, "main");
    sqlx::query(
        "INSERT INTO pending_push (repo_id, principal_id, correlation_id, updates, created_at)
         VALUES ((SELECT id FROM repo WHERE config_key = 'other'), NULL, 'stale', 'garbage', 0)",
    )
    .execute(&mut *import.writer.lock().await)
    .await
    .unwrap();
    assert_eq!(
        import
            .run("alice/demo", source.to_str().unwrap())
            .await
            .unwrap(),
        4
    );
    assert_eq!(
        import
            .strings("SELECT correlation_id FROM pending_push")
            .await,
        ["stale"]
    );
    // The full pass still reports it.
    assert!(
        import
            .gate
            .reconcile(
                &mut *import.writer.lock().await,
                &import.bus,
                &import.state_dir,
                &Gix
            )
            .await
            .is_err()
    );
}

/// The child half of `a_killed_import_converts_once_after_restart`: a server
/// process importing `alice/demo`, aborted at `KAJIBA_TEST_CRASH_AT`. A no-op when
/// run as a plain test.
#[tokio::test]
async fn crash_child() {
    let Ok(state_dir) = std::env::var("KAJIBA_TEST_STATE_DIR") else {
        return;
    };
    let state_dir = PathBuf::from(state_dir);
    let _lock = crate::state::prepare(&state_dir).unwrap();
    let (writer, _) = crate::db::open(&state_dir.join("db/kajiba.sqlite"))
        .await
        .unwrap();
    let source = std::env::var("KAJIBA_TEST_SOURCE").unwrap();
    let result = import(
        &Writer::new(writer.into()),
        &Arc::default(),
        &Arc::default(),
        &state_dir,
        "alice/demo",
        &source,
        "request-1",
    )
    .await;
    panic!("checkpoint not reached: {result:?}");
}

#[tokio::test]
async fn a_killed_import_converts_once_after_restart() {
    use std::os::unix::process::ExitStatusExt;
    for checkpoint in ["admitted", "fetched", "converting"] {
        let mut state = State::new().await;
        state.apply(DECLARED).await.unwrap();
        let source = source(&state.dir.0, "main");
        let expected = branches_and_tags(&source);
        let (public_id, ..) = state.repo("demo").await;
        let target = state.path(&public_id);
        let dir = state.stop().await;

        // The child really dies: no destructor, no `exited`, connections dropped by the kernel.
        let child = tokio::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "db::tests::import::crash_child", "--nocapture"])
            .env("KAJIBA_TEST_CRASH_AT", checkpoint)
            .env("KAJIBA_TEST_STATE_DIR", &dir.0)
            .env("KAJIBA_TEST_SOURCE", &source)
            .kill_on_drop(true)
            .output();
        let output = tokio::time::timeout(Duration::from_secs(120), child)
            .await
            .expect("child bounded")
            .unwrap();
        // SIGABRT is 6 on Linux and macOS.
        assert_eq!(output.status.signal(), Some(6), "{checkpoint}: {output:?}");

        // Restart on the same state directory, as `serve` does.
        let mut state = State::at(dir).await;
        crate::state::cleanup(&state.dir.0, &mut state.writer)
            .await
            .unwrap();
        let (gate, bus) = (Arc::new(Gate::default()), Arc::new(EventBus::default()));
        gate.reconcile(&mut state.writer, &bus, &state.dir.0, &Gix)
            .await
            .unwrap();
        assert!(
            state
                .strings("SELECT correlation_id FROM pending_push")
                .await
                .is_empty(),
            "{checkpoint}"
        );
        let writer = Writer::new(
            std::mem::replace(
                &mut state.writer,
                sqlx::Connection::connect("sqlite::memory:").await.unwrap(),
            )
            .into(),
        );
        if checkpoint == "admitted" {
            // Nothing was fetched: no event, and the released gate admits a new import.
            assert!(Gix.refs(&target).unwrap().is_empty());
            assert_eq!(
                import(
                    &writer,
                    &gate,
                    &bus,
                    &state.dir.0,
                    "alice/demo",
                    source.to_str().unwrap(),
                    "request-1"
                )
                .await
                .unwrap(),
                4
            );
        }
        assert_eq!(Gix.refs(&target).unwrap(), expected, "{checkpoint}");
        let events: Vec<String> =
            sqlx::query_scalar("SELECT payload FROM event WHERE kind = 'push' ORDER BY id")
                .fetch_all(&mut *writer.lock().await)
                .await
                .unwrap();
        let wanted: Vec<String> = expected
            .iter()
            .map(|r| {
                format!(
                    r#"{{"new":"{}","old":"{}","ref":"{}"}}"#,
                    r.target,
                    crate::db::pushes::ZERO,
                    r.name
                )
            })
            .collect();
        assert_eq!(events, wanted, "{checkpoint}: exactly once, in order");
        // The gate is released: the next ref update is admitted.
        gate.admit(&mut *writer.lock().await, 1, None, "next", &[])
            .await
            .unwrap();
        state.writer = Arc::try_unwrap(writer).unwrap().into_inner();
        state.close().await;
    }
}
