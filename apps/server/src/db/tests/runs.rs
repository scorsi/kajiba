use super::*;
use crate::db::events::{EventBus, NewEvent, append_event};
use crate::db::runs::{
    AttemptState, Ci, JobState, NewRun, RunState, attempt_transition, create_run, job_transition,
    run_transition,
};

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

async fn drain(writer: &mut SqliteConnection, bus: &EventBus) {
    let mut ci = Ci {
        now: || 5000,
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
    let branch = push(&mut writer, Some(1), 1, "refs/heads/main", SHA).await;
    let tag = push(&mut writer, Some(1), 1, "refs/tags/v1", SHA).await;
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
    drain(&mut writer, &bus).await;
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
            SHA.into(),
            Some("refs/tags/v1".into()),
            "push".into(),
            Some(tag),
            Some(1),
            "queued".into(),
            5000,
        ),
    ];
    assert_eq!(runs(&mut writer).await, expected);
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
            serde_json::json!({ "run": run, "trigger": "push", "ref": reference, "sha": SHA })
        );
    }

    // Replaying the journal from an older checkpoint creates nothing more.
    sqlx::query("UPDATE handler_checkpoint SET last_event_id = 0 WHERE handler = 'ci'")
        .execute(&mut writer)
        .await
        .unwrap();
    drain(&mut writer, &bus).await;
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
