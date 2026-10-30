use super::*;
use crate::db::attempts::{
    BACKEND_UNAVAILABLE, Chunk, Finish, LEASE_MS, NO_CONNECTED_RUNNER, Offer, Outcome,
    RUNNERS_BUSY, TAGS_UNMATCHED, accept, append_log, assign, finish, heartbeat, step,
};
use crate::db::runs::{AttemptState, NewRun, RunState, create_run};

type StepRow = (i64, String, String, Option<i64>, Option<i64>, Option<i64>);
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

/// One run assigned to `build-1` and accepted; returns (run, attempt).
async fn running(writer: &mut SqliteConnection) -> (String, String) {
    let run = new_run(writer, 1).await;
    let (assigned, _) = pass(writer, &[offer(1, &["nix"], &[], 1)], 2).await;
    let attempt = assigned[0].attempt.clone();
    let mut tx = writer.begin().await.unwrap();
    accept(&mut tx, (1, "build-1"), &attempt, "accept", 3)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    (run, attempt)
}

async fn log(
    writer: &mut SqliteConnection,
    logs: &std::path::Path,
    runner: i64,
    attempt: &str,
    sequence: u64,
    data: &[u8],
) -> Option<u64> {
    let mut tx = writer.begin().await.unwrap();
    let ack = append_log(
        &mut tx,
        logs,
        runner,
        &Chunk {
            attempt,
            sequence,
            step: 0,
            data,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    ack
}

#[tokio::test]
async fn log_chunks_are_appended_in_sequence_only() {
    let dir = TempDir::new();
    let (mut writer, readers) = open(&dir.database()).await.unwrap();
    populate(&mut writer).await;
    let (_, attempt) = running(&mut writer).await;
    let logs = dir.0.clone();
    let mut acks = Vec::new();
    for (sequence, data) in [
        (1, &b"one\n"[..]),
        (2, b"two\n"),
        (2, b"again\n"),
        (1, b"first again\n"),
        (4, b"gap\n"),
        (3, b"three\n"),
        (4, b"four\n"),
    ] {
        acks.push(log(&mut writer, &logs, 1, &attempt, sequence, data).await);
    }
    assert_eq!(
        acks,
        [
            Some(1),
            Some(2),
            Some(2),
            Some(2),
            Some(2),
            Some(3),
            Some(4)
        ]
    );
    // A chunk written but never indexed (crash before commit) is overwritten.
    let path = logs.join(format!("{attempt}.log"));
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    std::io::Write::write_all(&mut file, b"torn").unwrap();
    assert_eq!(
        log(&mut writer, &logs, 1, &attempt, 5, b"five\n").await,
        Some(5)
    );
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(bytes, b"one\ntwo\nthree\nfour\nfive\n");
    let index: Vec<(i64, i64, i64)> =
        sqlx::query_as("SELECT seq, offset, length FROM log_chunk ORDER BY seq")
            .fetch_all(&mut writer)
            .await
            .unwrap();
    assert_eq!(
        index,
        [(1, 0, 4), (2, 4, 4), (3, 8, 6), (4, 14, 5), (5, 19, 5)]
    );
    // Another runner's attempt is not this runner's to log.
    assert_eq!(log(&mut writer, &logs, 2, &attempt, 6, b"x").await, None);
    readers.close().await;
    writer.close().await.unwrap();
}

async fn result(
    writer: &mut SqliteConnection,
    runner: (i64, &str),
    attempt: &str,
    state: AttemptState,
    reason: &str,
) -> Outcome {
    let mut tx = writer.begin().await.unwrap();
    let outcome = finish(
        &mut tx,
        runner,
        attempt,
        &Finish {
            state,
            exit_code: Some(1),
            check: "checks.x86_64-linux.unit",
            reason,
        },
        "result",
        9,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    outcome
}

type Ended = (
    String,
    String,
    String,
    Option<i64>,
    Option<String>,
    Option<String>,
);

async fn ended(writer: &mut SqliteConnection) -> Ended {
    sqlx::query_as(
        "SELECT job_attempt.state, job.state, run.state, job_attempt.exit_code, run.failure_check, run.failure_reason
         FROM job_attempt JOIN job ON job.id = job_attempt.job_id JOIN run ON run.id = job.run_id",
    )
    .fetch_one(writer)
    .await
    .unwrap()
}

#[tokio::test]
async fn the_first_terminal_result_wins() {
    let dir = TempDir::new();
    let (mut writer, readers) = open(&dir.database()).await.unwrap();
    populate(&mut writer).await;
    let (run, attempt) = running(&mut writer).await;
    let runner = (1, "build-1");
    assert_eq!(
        result(
            &mut writer,
            (2, "build-2"),
            &attempt,
            AttemptState::Succeeded,
            ""
        )
        .await,
        Outcome::Cancel
    );
    assert_eq!(
        result(
            &mut writer,
            runner,
            &attempt,
            AttemptState::Failed,
            "exit status 1"
        )
        .await,
        Outcome::Done(vec![(run.clone(), RunState::Failed)])
    );
    assert_eq!(
        result(&mut writer, runner, &attempt, AttemptState::Succeeded, "").await,
        Outcome::Done(Vec::new())
    );
    assert_eq!(
        ended(&mut writer).await,
        (
            "failed".into(),
            "failed".into(),
            "failed".into(),
            Some(1),
            Some("checks.x86_64-linux.unit".into()),
            Some("exit status 1".into())
        )
    );
    let payloads: Vec<(String, String, Option<i64>)> = sqlx::query_as(
        "SELECT payload, principal_kind, principal_id FROM event WHERE kind = 'run-finished'",
    )
    .fetch_all(&mut writer)
    .await
    .unwrap();
    assert_eq!(payloads.len(), 1);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&payloads[0].0).unwrap(),
        serde_json::json!({ "run": run, "state": "failed", "check": "checks.x86_64-linux.unit", "reason": "exit status 1" })
    );
    assert_eq!((payloads[0].1.as_str(), payloads[0].2), ("runner", Some(1)));
    readers.close().await;
    writer.close().await.unwrap();
}

#[tokio::test]
async fn a_recorded_cancellation_wins_and_a_result_starts_an_unaccepted_attempt() {
    let dir = TempDir::new();
    let (mut writer, readers) = open(&dir.database()).await.unwrap();
    populate(&mut writer).await;
    let (_, attempt) = running(&mut writer).await;
    sqlx::query("UPDATE job_attempt SET state = 'cancelled'; UPDATE job SET state = 'cancelled'; UPDATE run SET state = 'cancelled'")
        .execute(&mut writer)
        .await
        .unwrap();
    assert_eq!(
        result(
            &mut writer,
            (1, "build-1"),
            &attempt,
            AttemptState::Succeeded,
            ""
        )
        .await,
        Outcome::Done(Vec::new())
    );
    assert_eq!(ended(&mut writer).await.0, "cancelled");

    // A success clears nothing it never set and needs no acceptance first.
    let run = new_run(&mut writer, 4).await;
    let (assigned, _) = pass(&mut writer, &[offer(1, &["nix"], &[], 1)], 5).await;
    let outcome = result(
        &mut writer,
        (1, "build-1"),
        &assigned[0].attempt,
        AttemptState::Succeeded,
        "ignored on success",
    )
    .await;
    assert_eq!(
        outcome,
        Outcome::Done(vec![
            (run.clone(), RunState::Running),
            (run.clone(), RunState::Succeeded)
        ])
    );
    let row: (String, Option<String>, Option<String>, Option<i64>) = sqlx::query_as(
        "SELECT state, failure_check, failure_reason, started_at FROM run WHERE public_id = ?",
    )
    .bind(&run)
    .fetch_one(&mut writer)
    .await
    .unwrap();
    assert_eq!(row, ("succeeded".into(), None, None, Some(9)));
    readers.close().await;
    writer.close().await.unwrap();
}

#[tokio::test]
async fn heartbeats_renew_live_leases_and_steps_are_recorded() {
    let dir = TempDir::new();
    let (mut writer, readers) = open(&dir.database()).await.unwrap();
    populate(&mut writer).await;
    let (_, attempt) = running(&mut writer).await;
    heartbeat(&mut writer, 1, 1000).await.unwrap();
    assert_eq!(
        count(&mut writer, "SELECT lease_expires_at FROM job_attempt").await,
        1000 + LEASE_MS
    );
    // An expired lease is the sweep's, not renewed.
    heartbeat(&mut writer, 1, 1000 + LEASE_MS + 1)
        .await
        .unwrap();
    assert_eq!(
        count(&mut writer, "SELECT lease_expires_at FROM job_attempt").await,
        1000 + LEASE_MS
    );
    assert_eq!(
        count(
            &mut writer,
            "SELECT last_contact_at FROM runner WHERE id = 1"
        )
        .await,
        1000 + LEASE_MS + 1
    );
    let mut tx = writer.begin().await.unwrap();
    assert!(
        step(
            &mut tx,
            1,
            &attempt,
            (0, "flake-check", "running", None),
            10
        )
        .await
        .unwrap()
    );
    assert!(
        step(
            &mut tx,
            1,
            &attempt,
            (0, "flake-check", "failed", Some(1)),
            20
        )
        .await
        .unwrap()
    );
    assert!(
        !step(
            &mut tx,
            2,
            &attempt,
            (0, "flake-check", "failed", Some(1)),
            20
        )
        .await
        .unwrap()
    );
    tx.commit().await.unwrap();
    let steps: Vec<StepRow> =
        sqlx::query_as("SELECT idx, name, state, exit_code, started_at, finished_at FROM step")
            .fetch_all(&mut writer)
            .await
            .unwrap();
    assert_eq!(
        steps,
        [(
            0,
            "flake-check".into(),
            "failed".into(),
            Some(1),
            Some(10),
            Some(20)
        )]
    );
    readers.close().await;
    writer.close().await.unwrap();
}
