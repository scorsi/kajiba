use super::users::changes;
use super::*;
use crate::db::{reconcile, repos::reconcile_repos};
use kajiba_core::config::ServerConfig;
use kajiba_git::{GitStore, Gix};

fn config(repos: &str) -> ServerConfig {
    ServerConfig::parse(&format!(
        "[http]\npublic_url='https://example.org'\n[users.alice]\n[users.bob]\n\
         [mirror.credentials.github]\ntoken_file='/run/secrets/github'\n\
         [mirror.credentials.other]\ntoken_file='/run/secrets/other'\n{repos}"
    ))
    .unwrap()
}

pub(super) struct State {
    pub(super) dir: TempDir,
    _lock: crate::state::StateLock,
    pub(super) writer: SqliteConnection,
    readers: sqlx::SqlitePool,
}

impl State {
    pub(super) async fn new() -> Self {
        Self::at(TempDir::new()).await
    }

    /// A server start on `dir`: takes the state lock and opens the database.
    pub(super) async fn at(dir: TempDir) -> Self {
        let lock = crate::state::prepare(&dir.0).unwrap();
        let (writer, readers) = open(&dir.0.join("db/kajiba.sqlite")).await.unwrap();
        Self {
            dir,
            _lock: lock,
            writer,
            readers,
        }
    }

    pub(super) async fn apply(&mut self, repos: &str) -> Result<(), String> {
        let config = config(repos);
        reconcile(&mut self.writer, &config.users).await.unwrap();
        reconcile_repos(&mut self.writer, &self.dir.0, &Gix, &config.repos)
            .await
            .map(|applied| applied.committed())
    }

    pub(super) async fn repo(&mut self, key: &str) -> (String, String, Option<i64>) {
        sqlx::query_as(
            "SELECT repo.public_id, user.name || '/' || repo.name, archived_at FROM repo
             JOIN user ON user.id = owner_id WHERE config_key = ?",
        )
        .bind(key)
        .fetch_one(&mut self.writer)
        .await
        .unwrap()
    }

    pub(super) async fn strings(&mut self, sql: &str) -> Vec<String> {
        sqlx::query_scalar(sql)
            .fetch_all(&mut self.writer)
            .await
            .unwrap()
    }

    pub(super) fn path(&self, public_id: &str) -> std::path::PathBuf {
        self.dir.0.join(format!("repos/{public_id}.git"))
    }

    fn repo_dirs(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(self.dir.0.join("repos"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    /// A restart: the connections are closed and the file reopened.
    pub(super) async fn reopen(&mut self) {
        let (writer, readers) = open(&self.dir.0.join("db/kajiba.sqlite")).await.unwrap();
        std::mem::replace(&mut self.writer, writer)
            .close()
            .await
            .unwrap();
        std::mem::replace(&mut self.readers, readers).close().await;
    }

    /// A server stop that keeps the state directory for the next start.
    pub(super) async fn stop(self) -> TempDir {
        self.readers.close().await;
        self.writer.close().await.unwrap();
        self.dir
    }

    pub(super) async fn close(self) {
        self.readers.close().await;
        self.writer.close().await.unwrap();
    }
}

pub(super) fn git(repo: &std::path::Path, args: &[&str], stdin: &str) -> String {
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

const DEMO: &str = "[repos.demo]\nowner='alice'\ndescription='Demo'\n\
     previous_names=['alice/old']\npermissions.bob='write'\n\
     mirrors=[{remote_url='https://example.org/demo.git', credential='github'}]\n";

#[tokio::test]
async fn creates_declared_repositories_and_noop_rerun_writes_nothing() {
    let mut state = State::new().await;
    state
        .apply(&format!(
            "{DEMO}[repos.tools]\nowner='bob'\nname='Tools'\ndescription=''\ndefault_branch='trunk'\nci_enabled=true\n"
        ))
        .await
        .unwrap();
    let (demo, name, archived) = state.repo("demo").await;
    assert_eq!((name.as_str(), archived), ("alice/demo", None));
    let (tools, name, _) = state.repo("tools").await;
    assert_eq!(name, "bob/tools");
    let mut dirs = vec![format!("{demo}.git"), format!("{tools}.git")];
    dirs.sort();
    assert_eq!(state.repo_dirs(), dirs);
    for (id, branch) in [(&demo, "main"), (&tools, "trunk")] {
        assert_eq!(
            fs::read_to_string(state.path(id).join("HEAD")).unwrap(),
            format!("ref: refs/heads/{branch}\n")
        );
        assert_eq!(Gix.refs(&state.path(id)).unwrap(), vec![]);
    }
    assert_eq!(
        fs::read_dir(state.dir.0.join("tmp")).unwrap().count(),
        0,
        "nothing left in tmp/"
    );
    assert_eq!(
        state
            .strings("SELECT owner_name || '/' || name FROM repo_redirect")
            .await,
        ["alice/old"]
    );
    assert_eq!(
        state
            .strings("SELECT user.name || ':' || role FROM repo_permission JOIN user ON user.id = user_id")
            .await,
        ["bob:write"]
    );
    assert_eq!(
        state
            .strings("SELECT remote_url || ' ' || credential || ' ' || enabled || ' ' || state FROM mirror")
            .await,
        ["https://example.org/demo.git github 1 idle"]
    );
    let before = changes(&mut state.writer).await;
    state
        .apply(&format!(
            "{DEMO}[repos.tools]\nowner='bob'\nname='tools'\ndescription=''\ndefault_branch='trunk'\nci_enabled=true\n"
        ))
        .await
        .unwrap();
    assert_eq!(changes(&mut state.writer).await, before);
    state.close().await;
}

#[tokio::test]
async fn renames_keep_identity_and_redirects_follow_previous_names() {
    let mut state = State::new().await;
    state
        .apply(&format!(
            "{DEMO}[repos.other]\nowner='alice'\nname='swap'\ndescription=''\n"
        ))
        .await
        .unwrap();
    let (demo, _, _) = state.repo("demo").await;
    let (other, _, _) = state.repo("other").await;
    // Swap the names of two keys under the same owner (UNIQUE(owner_id, name) collides
    // unless the names are freed first), change fields and redirects.
    state
        .apply(
            "[repos.demo]\nowner='alice'\nname='swap'\ndescription='New'\ndefault_branch='dev'\n\
             previous_names=['alice/legacy']\n\
             [repos.other]\nowner='alice'\nname='demo'\ndescription=''\n",
        )
        .await
        .unwrap();
    assert_eq!(
        state.repo("demo").await,
        (demo.clone(), "alice/swap".into(), None)
    );
    assert_eq!(
        state.repo("other").await,
        (other.clone(), "alice/demo".into(), None)
    );
    let mut dirs = vec![format!("{demo}.git"), format!("{other}.git")];
    dirs.sort();
    assert_eq!(state.repo_dirs(), dirs);
    assert_eq!(
        fs::read_to_string(state.path(&demo).join("HEAD")).unwrap(),
        "ref: refs/heads/dev\n"
    );
    assert_eq!(
        state
            .strings("SELECT owner_name || '/' || repo_redirect.name || '>' || repo.config_key FROM repo_redirect JOIN repo ON repo.id = repo_id")
            .await,
        ["alice/legacy>demo"]
    );
    assert_eq!(
        state
            .strings("SELECT description || default_branch FROM repo WHERE config_key = 'demo'")
            .await,
        ["Newdev"]
    );
    // A change of owner keeps the row and the directory too.
    state
        .apply(
            "[repos.demo]\nowner='bob'\nname='swap'\ndescription='New'\ndefault_branch='dev'\n\
             [repos.other]\nowner='alice'\nname='demo'\ndescription=''\n",
        )
        .await
        .unwrap();
    assert_eq!(
        state.repo("demo").await,
        (demo.clone(), "bob/swap".into(), None)
    );
    assert_eq!(state.repo_dirs(), dirs);
    assert!(
        state
            .strings("SELECT role FROM repo_permission")
            .await
            .is_empty()
    );
    assert!(
        state
            .strings("SELECT remote_url FROM mirror")
            .await
            .is_empty()
    );
    state.close().await;
}

#[tokio::test]
async fn mirror_runtime_state_survives_configuration_changes() {
    let mut state = State::new().await;
    state.apply(DEMO).await.unwrap();
    sqlx::query("UPDATE mirror SET attempts = 3, last_success_at = 5, last_error = 'boom'")
        .execute(&mut state.writer)
        .await
        .unwrap();
    let public_id = state.strings("SELECT public_id FROM mirror").await;
    state
        .apply(
            "[repos.demo]\nowner='alice'\ndescription='Demo'\n\
             mirrors=[{remote_url='https://example.org/demo.git', credential='other', enabled=false},\
             {remote_url='https://example.org/second.git', credential='github'}]\n",
        )
        .await
        .unwrap();
    assert_eq!(
        state
            .strings("SELECT public_id || ' ' || credential || ' ' || enabled || ' ' || attempts || ' ' || last_success_at || ' ' || last_error FROM mirror WHERE remote_url = 'https://example.org/demo.git'")
            .await,
        [format!("{} other 0 3 5 boom", public_id[0])]
    );
    assert_eq!(
        state
            .strings("SELECT remote_url FROM mirror ORDER BY id")
            .await
            .len(),
        2
    );
    state.close().await;
}

const TOOLS: &str = "[repos.tools]\nowner='alice'\ndescription=''\n";

#[tokio::test]
async fn archival_keeps_row_directory_history_and_locks_names() {
    let mut state = State::new().await;
    state.apply(&format!("{DEMO}{TOOLS}")).await.unwrap();
    let (public_id, _, _) = state.repo("demo").await;
    let (tools, _, _) = state.repo("tools").await;
    let mut dirs = vec![format!("{public_id}.git"), format!("{tools}.git")];
    dirs.sort();
    let path = state.path(&public_id);
    let tree = git(&path, &["mktree"], "");
    let commit = git(&path, &["commit-tree", &tree, "-m", "init"], "");
    git(&path, &["update-ref", "refs/heads/main", &commit], "");
    let refs = Gix.refs(&path).unwrap();
    sqlx::raw_sql(
        "UPDATE mirror SET state = 'queued', next_attempt_at = 7, last_success_at = 5;
         INSERT INTO run (public_id, repo_id, sha, trigger, state, created_at)
         VALUES ('00000000000000000000000001', 1, 'a', 'manual', 'queued', 0),
                ('00000000000000000000000002', 1, 'a', 'manual', 'succeeded', 0);
         INSERT INTO runner (public_id, name, capabilities, version, connected)
         VALUES ('00000000000000000000000003', 'r', '{}', '0', 0);
         INSERT INTO job (public_id, run_id, name, requirements, replayable, max_attempts, state, created_at)
         VALUES ('00000000000000000000000004', 1, 'build', '{}', 0, 1, 'running', 0),
                ('00000000000000000000000005', 2, 'build', '{}', 0, 1, 'succeeded', 0);
         INSERT INTO job_attempt (public_id, job_id, number, runner_id, state, lease_expires_at, started_at)
         VALUES ('00000000000000000000000006', 1, 1, 1, 'running', 0, 0);
         INSERT INTO pending_push (repo_id, correlation_id, updates, created_at) VALUES (1, 'c', '[]', 0);",
    )
    .execute(&mut state.writer)
    .await
    .unwrap();

    state.apply(TOOLS).await.unwrap();
    let (same, name, archived) = state.repo("demo").await;
    assert_eq!(
        (same.as_str(), name.as_str()),
        (public_id.as_str(), "alice/demo")
    );
    assert!(archived.is_some());
    assert_eq!(state.repo_dirs(), dirs);
    assert_eq!(Gix.refs(&path).unwrap(), refs);
    assert!(
        state
            .strings("SELECT role FROM repo_permission")
            .await
            .is_empty()
    );
    assert_eq!(
        state
            .strings("SELECT owner_name || '/' || repo_redirect.name || '>' || repo.config_key FROM repo_redirect JOIN repo ON repo.id = repo_id")
            .await,
        ["alice/old>demo"]
    );
    assert_eq!(
        state.strings("SELECT state FROM run ORDER BY id").await,
        ["cancelled", "succeeded"]
    );
    assert_eq!(
        state.strings("SELECT state FROM job ORDER BY id").await,
        ["cancelled", "succeeded"]
    );
    assert_eq!(
        state
            .strings("SELECT state || ' ' || (finished_at IS NOT NULL) FROM job_attempt")
            .await,
        ["cancelled 1"]
    );
    // Mirror queue state is runtime state: kept, the worker skips archived repositories.
    assert_eq!(
        state
            .strings("SELECT state || ' ' || ifnull(next_attempt_at, '-') || ' ' || last_success_at FROM mirror")
            .await,
        ["queued 7 5"]
    );
    assert_eq!(
        state
            .strings("SELECT correlation_id FROM pending_push")
            .await,
        ["c"]
    );
    // The orphan sweep counts archived rows.
    crate::state::cleanup(&state.dir.0, &mut state.writer)
        .await
        .unwrap();
    assert_eq!(state.repo_dirs(), dirs);
    assert!(!state.dir.0.join("tmp/orphans").exists());

    // No other key, new or renamed, may take its current or previous name; nothing is written.
    let before = changes(&mut state.writer).await;
    for (repos, key, name) in [
        (
            format!("{TOOLS}[repos.copy]\nowner='alice'\nname='demo'\ndescription=''\n"),
            "copy",
            "alice/demo",
        ),
        (
            format!("{TOOLS}[repos.copy]\nowner='alice'\nname='old'\ndescription=''\n"),
            "copy",
            "alice/old",
        ),
        (
            format!(
                "{TOOLS}[repos.copy]\nowner='alice'\ndescription=''\nprevious_names=['alice/demo']\n"
            ),
            "copy",
            "alice/demo",
        ),
        (
            "[repos.tools]\nowner='alice'\nname='demo'\ndescription=''\n".to_owned(),
            "tools",
            "alice/demo",
        ),
        (
            "[repos.tools]\nowner='alice'\ndescription=''\nprevious_names=['alice/old']\n"
                .to_owned(),
            "tools",
            "alice/old",
        ),
    ] {
        let error = state.apply(&repos).await.unwrap_err();
        assert!(
            error.contains(&format!(
                "repos.{key}: {name} is held by archived repository repos.demo"
            )),
            "{error}"
        );
        assert_eq!(changes(&mut state.writer).await, before);
        assert_eq!(state.repo_dirs(), dirs);
        assert_eq!(state.repo("tools").await.1, "alice/tools");
    }

    // The same key re-declared unarchives the same row, content intact.
    state.apply(&format!("{DEMO}{TOOLS}")).await.unwrap();
    assert_eq!(
        state.repo("demo").await,
        (public_id.clone(), "alice/demo".into(), None)
    );
    assert_eq!(Gix.refs(&path).unwrap(), refs);
    assert_eq!(
        state.strings("SELECT role FROM repo_permission").await,
        ["write"]
    );
    assert_eq!(
        state.strings("SELECT state FROM run ORDER BY id").await,
        ["cancelled", "succeeded"]
    );
    assert_eq!(state.repo_dirs(), dirs);
    state.close().await;
}

#[tokio::test]
async fn a_failure_after_a_head_change_rolls_back_rows_and_the_next_run_converges() {
    use std::os::unix::fs::PermissionsExt;
    let mut state = State::new().await;
    state.apply(DEMO).await.unwrap();
    let (demo, _, _) = state.repo("demo").await;
    let head = state.path(&demo).join("HEAD");
    // demo's HEAD moves first, then creating `fresh` fails: tmp/ is read-only.
    let changed = "[repos.demo]\nowner='alice'\ndescription='Demo'\ndefault_branch='dev'\n\
                   [repos.fresh]\nowner='bob'\ndescription=''\n";
    let tmp = state.dir.0.join("tmp");
    fs::set_permissions(&tmp, fs::Permissions::from_mode(0o500)).unwrap();
    let error = state.apply(changed).await.unwrap_err();
    fs::set_permissions(&tmp, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(error.contains(&tmp.display().to_string()), "{error}");
    assert_eq!(
        state
            .strings("SELECT config_key || ' ' || default_branch FROM repo")
            .await,
        ["demo main"]
    );
    assert_eq!(state.repo_dirs(), [format!("{demo}.git")]);
    assert_eq!(
        fs::read_to_string(&head).unwrap(),
        "ref: refs/heads/dev\n",
        "the file change is not rolled back; the next run converges"
    );
    // The same declaration converges: row and HEAD agree.
    state.apply(changed).await.unwrap();
    assert_eq!(
        state
            .strings("SELECT default_branch FROM repo WHERE config_key = 'demo'")
            .await,
        ["dev"]
    );
    assert_eq!(fs::read_to_string(&head).unwrap(), "ref: refs/heads/dev\n");
    assert_eq!(state.repo_dirs().len(), 2);
    // So does the previous one: the file is compared, not the row's previous value.
    state.apply(DEMO).await.unwrap();
    assert_eq!(fs::read_to_string(&head).unwrap(), "ref: refs/heads/main\n");
    // A HEAD truncated by a crash is repaired by a rerun that writes no row.
    fs::write(&head, "").unwrap();
    let before = changes(&mut state.writer).await;
    state.apply(DEMO).await.unwrap();
    assert_eq!(changes(&mut state.writer).await, before);
    assert_eq!(fs::read_to_string(&head).unwrap(), "ref: refs/heads/main\n");
    state.close().await;
}

#[tokio::test]
async fn a_failed_head_change_converges_before_archival_and_while_archived() {
    use std::os::unix::fs::PermissionsExt;
    let mut state = State::new().await;
    state.apply(DEMO).await.unwrap();
    let (demo, _, _) = state.repo("demo").await;
    let head = state.path(&demo).join("HEAD");
    let tmp = state.dir.0.join("tmp");
    fs::set_permissions(&tmp, fs::Permissions::from_mode(0o500)).unwrap();
    let error = state
        .apply(&format!(
            "{DEMO}default_branch='dev'\n[repos.fresh]\nowner='bob'\ndescription=''\n"
        ))
        .await
        .unwrap_err();
    fs::set_permissions(&tmp, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(error.contains(&tmp.display().to_string()), "{error}");
    assert_eq!(
        state.strings("SELECT default_branch FROM repo").await,
        ["main"]
    );
    assert_eq!(fs::read_to_string(&head).unwrap(), "ref: refs/heads/dev\n");

    state.reopen().await;
    state.apply("").await.unwrap();
    let archived = state.repo("demo").await;
    assert_eq!(archived.0, demo);
    assert!(archived.2.is_some());
    assert_eq!(fs::read_to_string(&head).unwrap(), "ref: refs/heads/main\n");
    assert_eq!(
        state.strings("SELECT default_branch FROM repo").await,
        ["main"]
    );
    assert_eq!(state.repo_dirs(), [format!("{demo}.git")]);

    // Already archived repositories also converge, without changing their rows.
    fs::write(&head, "ref: refs/heads/dev\n").unwrap();
    state.reopen().await;
    let before = changes(&mut state.writer).await;
    state.apply("").await.unwrap();
    assert_eq!(changes(&mut state.writer).await, before);
    assert_eq!(state.repo("demo").await, archived);
    assert_eq!(fs::read_to_string(&head).unwrap(), "ref: refs/heads/main\n");
    state.close().await;
}

#[tokio::test]
async fn a_repository_refusal_rolls_back_user_and_credential_changes() {
    let mut state = State::new().await;
    state.apply(DEMO).await.unwrap();
    state.apply("").await.unwrap();
    // carol is new, but repos.copy takes the archived repository's name.
    let mut config =
        config("[users.carol]\n[repos.copy]\nowner='alice'\nname='demo'\ndescription=''\n");
    config.state_dir = state.dir.0.clone();
    let error = crate::db::reconcile_declared(&mut state.writer, &config, &Gix)
        .await
        .unwrap_err();
    assert!(
        error.contains("held by archived repository repos.demo"),
        "{error}"
    );
    assert_eq!(
        state.strings("SELECT name FROM user ORDER BY name").await,
        ["alice", "bob"]
    );
    assert_eq!(state.repo_dirs().len(), 1);

    // Accepted, the same pass commits users and repositories together.
    config.repos.clear();
    config.repos.insert(
        "demo".into(),
        self::config(DEMO).repos.remove("demo").unwrap(),
    );
    crate::db::reconcile_declared(&mut state.writer, &config, &Gix)
        .await
        .unwrap();
    assert_eq!(state.repo("demo").await.2, None);
    assert_eq!(
        state.strings("SELECT name FROM user ORDER BY name").await,
        ["alice", "bob", "carol"]
    );
    state.close().await;
}
