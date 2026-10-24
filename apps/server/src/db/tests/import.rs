use super::repos::{State, git};
use crate::db::{Writer, events::EventBus, pushes::Gate};
use crate::import::{Error, import};
use kajiba_git::{GitStore, Gix, Ref};
use std::path::{Path, PathBuf};

const DECLARED: &str = "[repos.demo]\nowner='alice'\ndescription=''\n\
     [repos.other]\nowner='alice'\ndescription=''\n\
     [repos.third]\nowner='alice'\ndescription=''\n\
     [repos.gone]\nowner='bob'\ndescription=''\n";

struct Import {
    state_dir: PathBuf,
    writer: Writer,
    gate: Gate,
    bus: EventBus,
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
            gate: Gate::default(),
            bus: EventBus::default(),
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
    let refs = Gix.remote_refs(source.to_str().unwrap()).unwrap();
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
    Gix.fetch(&target, source.to_str().unwrap(), &refs).unwrap();
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
