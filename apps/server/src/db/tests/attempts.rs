use super::*;
use crate::db::attempts::{
    BACKEND_UNAVAILABLE, LEASE_MS, NO_CONNECTED_RUNNER, Offer, Outcome, RUNNERS_BUSY,
    TAGS_UNMATCHED, accept, assign,
};
use crate::db::runs::{NewRun, RunState, create_run};

type EventRow = (String, String, Option<i64>, Option<i64>, String, String);

const SHA: &str = "1111111111111111111111111111111111111111";

/// User 1, repository 1 and runners 1 (`build-1`) and 2 (`build-2`).
async fn populate(writer: &mut SqliteConnection) {
    sqlx::query("INSERT INTO user (id, public_id, name, kind, is_admin, source, created_at) VALUES (1, '00000000000000000000000001', 'alice', 'local', 0, 'config', 0);
        INSERT INTO repo (id, public_id, owner_id, name, visibility, ci_enabled, default_branch, description, created_at, config_key) VALUES
        (1, '00000000000000000000000001', 1, 'demo', 'public', 1, 'main', '', 0, 'demo');
        INSERT INTO runner (id, public_id, name, capabilities, version, connected) VALUES
        (1, '00000000000000000000000001', 'build-1', '{}', '', 0),
        (2, '00000000000000000000000002', 'build-2', '{}', '', 0)")
        .execute(writer).await.unwrap();
}

async fn new_run(writer: &mut SqliteConnection, now: i64) -> String {
    let mut tx = writer.begin().await.unwrap();
    let run = create_run(
        &mut tx,
        &NewRun {
            repo_id: 1,
            sha: SHA,
            reference: Some("refs/heads/main"),
            trigger: "push",
            event_id: None,
            triggered_by: Some(1),
            correlation_id: "push",
            now,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    run
}

fn offer(runner_id: i64, backends: &[&str], tags: &[&str], max_jobs: i64) -> Offer {
    Offer {
        runner_id,
        arch: "aarch64".into(),
        os: "linux".into(),
        backends: backends.iter().map(|b| (*b).into()).collect(),
        tags: tags.iter().map(|t| (*t).into()).collect(),
        max_jobs,
    }
}

async fn pass(
    writer: &mut SqliteConnection,
    offers: &[Offer],
    now: i64,
) -> (Vec<crate::db::attempts::Assigned>, i64) {
    let mut tx = writer.begin().await.unwrap();
    let result = assign(&mut tx, offers, now).await.unwrap();
    tx.commit().await.unwrap();
    result
}

async fn jobs(writer: &mut SqliteConnection) -> Vec<(String, Option<String>)> {
    sqlx::query_as("SELECT state, pending_reason FROM job ORDER BY id")
        .fetch_all(writer)
        .await
        .unwrap()
}

#[tokio::test]
async fn matching_names_why_a_job_waits_and_clears_it_on_assignment() {
    let dir = TempDir::new();
    let (mut writer, readers) = open(&dir.database()).await.unwrap();
    populate(&mut writer).await;
    new_run(&mut writer, 10).await;
    // Required tags and system are matched too (requirements are opaque, 0013).
    sqlx::query(
        "UPDATE job SET requirements = '{\"backend\":\"nix\",\"tags\":[\"kvm\"],\"os\":\"linux\"}'",
    )
    .execute(&mut writer)
    .await
    .unwrap();
    let darwin = Offer {
        os: "darwin".into(),
        ..offer(2, &["nix"], &["kvm"], 1)
    };
    for (offers, reason) in [
        (vec![], NO_CONNECTED_RUNNER),
        (
            vec![offer(1, &["process"], &["kvm"], 1)],
            BACKEND_UNAVAILABLE,
        ),
        (vec![offer(1, &["nix"], &[], 1)], TAGS_UNMATCHED),
        (vec![darwin], TAGS_UNMATCHED),
        (vec![offer(1, &["nix"], &["kvm"], 0)], RUNNERS_BUSY),
    ] {
        let (assigned, pending) = pass(&mut writer, &offers, 20).await;
        assert!(assigned.is_empty(), "{reason}");
        assert_eq!(pending, 1);
        assert_eq!(
            jobs(&mut writer).await,
            [("pending".into(), Some(reason.into()))]
        );
    }
    let (assigned, pending) = pass(
        &mut writer,
        &[offer(1, &["process", "nix"], &["kvm", "big"], 1)],
        30,
    )
    .await;
    assert_eq!(pending, 0);
    assert_eq!(assigned.len(), 1);
    assert_eq!(
        (
            assigned[0].runner_id,
            assigned[0].repo.as_str(),
            assigned[0].sha.as_str(),
            assigned[0].system.as_str()
        ),
        (1, "00000000000000000000000001", SHA, "aarch64-linux")
    );
    assert_eq!(jobs(&mut writer).await, [("assigned".into(), None)]);
    let attempt: (String, i64, i64, String, i64, i64) = sqlx::query_as(
        "SELECT public_id, number, runner_id, state, lease_expires_at, started_at FROM job_attempt",
    )
    .fetch_one(&mut writer)
    .await
    .unwrap();
    assert_eq!(
        attempt,
        (
            assigned[0].attempt.clone(),
            1,
            1,
            "running".into(),
            30 + LEASE_MS,
            30
        )
    );
    readers.close().await;
    writer.close().await.unwrap();
}

#[tokio::test]
async fn jobs_go_oldest_first_within_max_jobs_and_once() {
    let dir = TempDir::new();
    let (mut writer, readers) = open(&dir.database()).await.unwrap();
    populate(&mut writer).await;
    for now in [1, 2, 3] {
        new_run(&mut writer, now).await;
    }
    let offers = [offer(1, &["nix"], &[], 1), offer(2, &["nix"], &[], 1)];
    let (assigned, pending) = pass(&mut writer, &offers, 5).await;
    assert_eq!(
        assigned.iter().map(|a| a.runner_id).collect::<Vec<_>>(),
        [1, 2]
    );
    assert_eq!(pending, 1);
    assert_eq!(
        jobs(&mut writer).await,
        [
            ("assigned".into(), None),
            ("assigned".into(), None),
            ("pending".into(), Some(RUNNERS_BUSY.into()))
        ]
    );
    // Another pass offers nothing twice: running attempts hold the capacity.
    let (again, pending) = pass(&mut writer, &offers, 6).await;
    assert!(again.is_empty());
    assert_eq!(pending, 1);
    assert_eq!(
        count(&mut writer, "SELECT count(*) FROM job_attempt").await,
        2
    );
    readers.close().await;
    writer.close().await.unwrap();
}

#[tokio::test]
async fn acceptance_starts_the_job_and_the_run_once() {
    let dir = TempDir::new();
    let (mut writer, readers) = open(&dir.database()).await.unwrap();
    populate(&mut writer).await;
    let run = new_run(&mut writer, 1).await;
    let (assigned, _) = pass(&mut writer, &[offer(1, &["nix"], &[], 1)], 2).await;
    let attempt = &assigned[0].attempt;
    let mut tx = writer.begin().await.unwrap();
    // Another runner's attempt and an unknown one are answered with cancel (0008).
    assert_eq!(
        accept(&mut tx, (2, "build-2"), attempt, "r", 3)
            .await
            .unwrap(),
        Outcome::Cancel
    );
    assert_eq!(
        accept(
            &mut tx,
            (1, "build-1"),
            "7ZZZZZZZZZZZZZZZZZZZZZZZZZ",
            "r",
            3
        )
        .await
        .unwrap(),
        Outcome::Cancel
    );
    assert_eq!(
        accept(&mut tx, (1, "build-1"), attempt, "accept", 3)
            .await
            .unwrap(),
        Outcome::Done(vec![(run.clone(), RunState::Running)])
    );
    assert_eq!(
        accept(&mut tx, (1, "build-1"), attempt, "accept", 4)
            .await
            .unwrap(),
        Outcome::Done(Vec::new())
    );
    tx.commit().await.unwrap();
    let states: (String, String, Option<i64>) = sqlx::query_as(
        "SELECT job.state, run.state, run.started_at FROM job JOIN run ON run.id = job.run_id",
    )
    .fetch_one(&mut writer)
    .await
    .unwrap();
    assert_eq!(states, ("running".into(), "running".into(), Some(3)));
    let events: Vec<EventRow> =
        sqlx::query_as("SELECT kind, principal_kind, principal_id, repo_id, correlation_id, payload FROM event WHERE kind = 'run-started'")
            .fetch_all(&mut writer)
            .await
            .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(
        (
            events[0].1.as_str(),
            events[0].2,
            events[0].3,
            events[0].4.as_str()
        ),
        ("runner", Some(1), Some(1), "accept")
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&events[0].5).unwrap(),
        serde_json::json!({ "run": run, "attempt": attempt, "runner": "build-1" })
    );
    readers.close().await;
    writer.close().await.unwrap();
}

async fn count(writer: &mut SqliteConnection, sql: &str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(writer).await.unwrap()
}
