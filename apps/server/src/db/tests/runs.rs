use super::*;
use crate::db::events::{EventBus, NewEvent, append_event};
use crate::db::runs::{
    AttemptState, Ci, JobState, NewRun, RunState, attempt_transition, create_run, job_transition,
    run_transition,
};
use kajiba_gitstore::{GitStore, Gix};

const ZERO: &str = "0000000000000000000000000000000000000000";
const SHA: &str = "1111111111111111111111111111111111111111";

/// Users 1 (local) and 2 (external); repositories 1 (CI on), 2 (CI off), 3 (CI on, archived).
async fn populate(writer: &mut SqliteConnection) {
    sqlx::query("INSERT INTO user (id, public_id, name, kind, is_admin, source, created_at) VALUES (1, '00000000000000000000000001', 'alice', 'local', 0, 'config', 0), (2, '00000000000000000000000002', 'guest', 'external', 0, 'runtime', 0);
        INSERT INTO repo (id, public_id, owner_id, name, visibility, ci_enabled, default_branch, description, created_at, config_key, archived_at) VALUES
        (1, '00000000000000000000000001', 1, 'demo', 'public', 1, 'main', '', 0, 'demo', NULL),
        (2, '00000000000000000000000002', 1, 'quiet', 'public', 0, 'main', '', 0, 'quiet', NULL),
        (3, '00000000000000000000000003', 1, 'old', 'public', 1, 'main', '', 0, 'old', 5)")
        .execute(writer).await.unwrap();
}

async fn push(
    writer: &mut SqliteConnection,
    user: Option<i64>,
    repo: i64,
    reference: &str,
    new: &str,
) -> i64 {
    let id: i64 = sqlx::query_scalar("SELECT count(*) + 1 FROM event")
        .fetch_one(&mut *writer)
        .await
        .unwrap();
    append_event(
        writer,
        &NewEvent {
            public_id: format!("{id:026}"),
            kind: "push".into(),
            version: 1,
            occurred_at: 1234,
            principal_kind: if user.is_some() { "user" } else { "system" }.into(),
            principal_id: user,
            repo_id: Some(repo),
            correlation_id: format!("request-{id}"),
            payload: serde_json::json!({ "ref": reference, "old": ZERO, "new": new }).to_string(),
        },
    )
    .await
    .unwrap()
}

async fn drain(writer: &mut SqliteConnection, bus: &EventBus, repos: &std::path::Path) {
    let mut ci = Ci {
        now: || 5000,
        repos: repos.to_owned(),
        wake: std::sync::Arc::default(),
    };
    while bus.dispatch_next(writer, &mut ci).await.unwrap() {}
}

type RunRow = (
    i64,
    String,
    Option<String>,
    String,
    Option<i64>,
    Option<i64>,
    String,
    i64,
);

type EventRow = (String, Option<i64>, Option<i64>, String, String);

async fn runs(writer: &mut SqliteConnection) -> Vec<RunRow> {
    sqlx::query_as("SELECT repo_id, sha, ref, trigger, event_id, triggered_by, state, created_at FROM run ORDER BY id")
        .fetch_all(writer)
        .await
        .unwrap()
}

async fn count(writer: &mut SqliteConnection, sql: &str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(writer).await.unwrap()
}

#[tokio::test]
async fn ci_handler_runs_only_trusted_pushes_once() {
    let dir = TempDir::new();
    let (mut writer, readers) = open(&dir.database()).await.unwrap();
    populate(&mut writer).await;
    let root = dir.0.join("repos");
    std::fs::create_dir(&root).unwrap();
    let repo = root.join("00000000000000000000000001.git");
    let source = dir.0.join("source.git");
    Gix.init_bare(&repo, "main").unwrap();
    Gix.init_bare(&source, "main").unwrap();
    let key = dir.0.join("signing-key");
    assert!(
        std::process::Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-f"])
            .arg(&key)
            .status()
            .unwrap()
            .success()
    );
    let git = |args: &[&str], input: &str| super::repos::git(&source, args, input);
    let tree = git(&["mktree"], "");
    let commit = git(&["commit-tree", &tree], "tagged commit\n");
    git(
        &[
            "-c",
            "gpg.format=ssh",
            "-c",
            &format!("user.signingkey={}", key.display()),
            "tag",
            "-s",
            "v1",
            &commit,
            "-m",
            "signed tag",
        ],
        "",
    );
    let tag_id = git(&["rev-parse", "refs/tags/v1"], "");
    assert_ne!(tag_id, commit);
    git(&["push", repo.to_str().unwrap(), "refs/tags/v1"], "");
    let branch = push(&mut writer, Some(1), 1, "refs/heads/main", SHA).await;
    let tag = push(&mut writer, Some(1), 1, "refs/tags/v1", &tag_id).await;
    // The handler must use the event's object, even if the ref moves before dispatch.
    let other = super::repos::git(&repo, &["commit-tree", &tree], "another commit\n");
    super::repos::git(&repo, &["update-ref", "refs/tags/v1", &other], "");
    for (user, repo, reference, new) in [
        (Some(1), 1, "refs/contrib/alice/x", SHA),
        (Some(1), 1, "refs/heads/gone", ZERO),
        (None, 1, "refs/heads/main", SHA),
        (Some(2), 1, "refs/heads/main", SHA),
        (Some(1), 2, "refs/heads/main", SHA),
        (Some(1), 3, "refs/heads/main", SHA),
    ] {
        push(&mut writer, user, repo, reference, new).await;
    }
    let bus = EventBus::default();
    drain(&mut writer, &bus, &root).await;
    let expected = vec![
        (
            1,
            SHA.into(),
            Some("refs/heads/main".into()),
            "push".into(),
            Some(branch),
            Some(1),
            "queued".into(),
            5000,
        ),
        (
            1,
            commit.clone(),
            Some("refs/tags/v1".into()),
            "push".into(),
            Some(tag),
            Some(1),
            "queued".into(),
            5000,
        ),
    ];
    assert_eq!(runs(&mut writer).await, expected);
    let mut bundle = Vec::new();
    Gix.bundle(&repo, &[expected[1].1.clone()], &[], &mut bundle)
        .unwrap();
    assert!(bundle.starts_with(b"# v2 git bundle\n"));
    let jobs: Vec<(String, String, bool, i64, String, Option<String>)> = sqlx::query_as(
        "SELECT name, requirements, replayable, max_attempts, state, pending_reason FROM job ORDER BY id",
    )
    .fetch_all(&mut writer)
    .await
    .unwrap();
    assert_eq!(
        jobs,
        vec![
            (
                "flake-check".into(),
                r#"{"backend":"nix"}"#.into(),
                true,
                3,
                "pending".into(),
                None
            );
            2
        ]
    );
    let created: Vec<EventRow> = sqlx::query_as(
        "SELECT principal_kind, principal_id, repo_id, correlation_id, payload FROM event WHERE kind = 'run-created' ORDER BY id",
    )
    .fetch_all(&mut writer)
    .await
    .unwrap();
    let run_ids: Vec<String> = sqlx::query_scalar("SELECT public_id FROM run ORDER BY id")
        .fetch_all(&mut writer)
        .await
        .unwrap();
    assert_eq!(created.len(), 2);
    for ((row, run), (event, reference)) in created
        .iter()
        .zip(&run_ids)
        .zip([(branch, "refs/heads/main"), (tag, "refs/tags/v1")])
    {
        assert_eq!((row.0.as_str(), row.1, row.2), ("user", Some(1), Some(1)));
        assert_eq!(row.3, format!("request-{event}"));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&row.4).unwrap(),
            serde_json::json!({ "run": run, "trigger": "push", "ref": reference,
                "sha": if event == tag { commit.as_str() } else { SHA } })
        );
    }

    // Replaying the journal from an older checkpoint creates nothing more.
    sqlx::query("UPDATE handler_checkpoint SET last_event_id = 0 WHERE handler = 'ci'")
        .execute(&mut writer)
        .await
        .unwrap();
    drain(&mut writer, &bus, &root).await;
    assert_eq!(runs(&mut writer).await, expected);
    assert_eq!(count(&mut writer, "SELECT count(*) FROM job").await, 2);
    assert_eq!(
        count(
            &mut writer,
            "SELECT count(*) FROM event WHERE kind = 'run-created'"
        )
        .await,
        2
    );
    readers.close().await;
    writer.close().await.unwrap();
}

#[tokio::test]
async fn run_created_commits_with_its_run() {
    let dir = TempDir::new();
    let (mut writer, readers) = open(&dir.database()).await.unwrap();
    populate(&mut writer).await;
    let run = NewRun {
        repo_id: 1,
        sha: SHA,
        reference: None,
        trigger: "manual",
        event_id: None,
        triggered_by: Some(1),
        correlation_id: "request-9",
        now: 7000,
        worktree: None,
    };
    let mut tx = writer.begin().await.unwrap();
    create_run(&mut tx, &run).await.unwrap();
    tx.rollback().await.unwrap();
    for table in ["run", "job", "event"] {
        assert_eq!(
            count(&mut writer, &format!("SELECT count(*) FROM {table}")).await,
            0
        );
    }
    let bus = EventBus::default();
    let mut tx = writer.begin().await.unwrap();
    let id = create_run(&mut tx, &run).await.unwrap();
    bus.commit(tx).await.unwrap();
    let payload: String =
        sqlx::query_scalar("SELECT payload FROM event WHERE kind = 'run-created'")
            .fetch_one(&mut writer)
            .await
            .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&payload).unwrap(),
        serde_json::json!({ "run": id, "trigger": "manual", "ref": null, "sha": SHA })
    );
    assert_eq!(count(&mut writer, "SELECT count(*) FROM job").await, 1);
    readers.close().await;
    writer.close().await.unwrap();
}

#[test]
fn state_machines_allow_exactly_the_0008_transitions() {
    fn table<S: Copy + PartialEq + std::fmt::Debug>(
        all: &[S],
        name: fn(S) -> &'static str,
        transition: fn(S, S) -> Result<S, crate::db::runs::Refused>,
        allowed: &[(&str, &str)],
    ) {
        for &from in all {
            for &to in all {
                let expected = allowed.contains(&(name(from), name(to)));
                match transition(from, to) {
                    Ok(state) => assert!(expected && state == to, "{from:?} -> {to:?}"),
                    Err(refused) => {
                        assert!(!expected, "{from:?} -> {to:?} refused");
                        assert_eq!((refused.from, refused.to), (name(from), name(to)));
                    }
                }
            }
        }
    }
    table(
        RunState::ALL,
        RunState::as_str,
        run_transition,
        &[
            ("queued", "running"),
            ("queued", "failed"),
            ("queued", "cancelled"),
            ("running", "succeeded"),
            ("running", "failed"),
            ("running", "cancelled"),
        ],
    );
    table(
        JobState::ALL,
        JobState::as_str,
        job_transition,
        &[
            ("pending", "assigned"),
            ("pending", "cancelled"),
            ("assigned", "running"),
            ("assigned", "pending"),
            ("assigned", "failed"),
            ("assigned", "cancelled"),
            ("assigned", "lost"),
            ("running", "succeeded"),
            ("running", "failed"),
            ("running", "cancelled"),
            ("running", "lost"),
            ("running", "pending"),
        ],
    );
    table(
        AttemptState::ALL,
        AttemptState::as_str,
        attempt_transition,
        &[
            ("running", "succeeded"),
            ("running", "failed"),
            ("running", "cancelled"),
            ("running", "lost"),
        ],
    );
}

#[tokio::test]
async fn migration_12_keeps_ci_rows_and_admits_worktree_runs() {
    let dir = TempDir::new();
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(dir.database())
        .create_if_missing(true);
    let mut writer = SqliteConnection::connect_with(&options).await.unwrap();
    let before = Migrator {
        migrations: MIGRATOR
            .iter()
            .filter(|migration| migration.version < 12)
            .cloned()
            .collect::<Vec<_>>()
            .into(),
        ..MIGRATOR
    };
    migrate(&mut writer, &before).await.unwrap();
    populate(&mut writer).await;
    let event = push(&mut writer, Some(1), 1, "refs/heads/main", SHA).await;
    sqlx::query(&format!("INSERT INTO runner (id, public_id, name, capabilities, version, connected) VALUES (1, '00000000000000000000000001', 'build-1', '{{}}', '0.1.0', 1);
        INSERT INTO run (id, public_id, repo_id, sha, ref, trigger, event_id, triggered_by, state, created_at, failure_reason) VALUES (1, '00000000000000000000000001', 1, '{SHA}', 'refs/heads/main', 'push', {event}, 1, 'failed', 7, 'exit status 1');
        INSERT INTO job (id, public_id, run_id, name, requirements, replayable, max_attempts, state, created_at) VALUES (1, '00000000000000000000000001', 1, 'flake-check', '{{}}', 1, 3, 'failed', 7);
        INSERT INTO job_attempt (id, public_id, job_id, number, runner_id, state, lease_expires_at, started_at) VALUES (1, '00000000000000000000000001', 1, 1, 1, 'failed', 9, 8);
        INSERT INTO step (attempt_id, idx, name, state) VALUES (1, 0, 'flake-check', 'failed');
        INSERT INTO log_chunk (attempt_id, seq, step_idx, offset, length) VALUES (1, 0, 0, 0, 5);
        INSERT INTO artifact (public_id, attempt_id, name, size, sha256, created_at, expires_at) VALUES ('00000000000000000000000001', 1, 'out', 1, 'x', 8, 9)"))
        .execute(&mut writer)
        .await
        .unwrap();
    migrate(&mut writer, &MIGRATOR).await.unwrap();

    for (table, rows) in [
        ("run", 1),
        ("job", 1),
        ("job_attempt", 1),
        ("step", 1),
        ("log_chunk", 1),
        ("artifact", 1),
    ] {
        assert_eq!(
            count(&mut writer, &format!("SELECT count(*) FROM {table}")).await,
            rows,
            "{table}"
        );
    }
    let kept: (String, Option<String>, Option<String>) =
        sqlx::query_as("SELECT trigger, failure_reason, checks FROM run")
            .fetch_one(&mut writer)
            .await
            .unwrap();
    assert_eq!(kept, ("push".into(), Some("exit status 1".into()), None));
    let leftovers = count(
        &mut writer,
        "SELECT count(*) FROM sqlite_schema WHERE name LIKE 'copy_%' OR name LIKE 'new_%'",
    )
    .await;
    assert_eq!(leftovers, 0);
    let indexes: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_schema WHERE type = 'index' AND name IN ('run_repo_id_id', 'job_state', 'job_attempt_state_lease_expires_at') ORDER BY name",
    )
    .fetch_all(&mut writer)
    .await
    .unwrap();
    assert_eq!(indexes.len(), 3);

    // A worktree run has no ref and no event, and may carry selected checks.
    sqlx::query(&format!("INSERT INTO run (public_id, repo_id, sha, trigger, triggered_by, state, created_at, checks) VALUES ('00000000000000000000000002', 1, '{SHA}', 'worktree', 1, 'queued', 10, '[\"a\"]')"))
        .execute(&mut writer)
        .await
        .unwrap();
    let refused = sqlx::query(&format!("INSERT INTO run (public_id, repo_id, sha, trigger, state, created_at) VALUES ('00000000000000000000000003', 1, '{SHA}', 'other', 'queued', 10)"))
        .execute(&mut writer)
        .await
        .unwrap_err();
    assert!(refused.to_string().contains("CHECK"), "{refused}");
    // Foreign keys still point at the rebuilt tables: deleting a run cascades.
    sqlx::query("DELETE FROM run WHERE id = 1")
        .execute(&mut writer)
        .await
        .unwrap();
    assert_eq!(
        count(&mut writer, "SELECT count(*) FROM log_chunk").await,
        0
    );
    assert_eq!(count(&mut writer, "SELECT count(*) FROM artifact").await, 0);
    let broken = sqlx::query("PRAGMA foreign_key_check")
        .fetch_all(&mut writer)
        .await
        .unwrap();
    assert!(broken.is_empty());
    writer.close().await.unwrap();
}
