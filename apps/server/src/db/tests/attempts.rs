use super::*;
use crate::db::attempts::{
    BACKEND_UNAVAILABLE, Chunk, Finish, LEASE_MS, MAX_LOG, NO_CONNECTED_RUNNER, Offer, Outcome,
    RUNNERS_BUSY, TAGS_UNMATCHED, TRUNCATED, abandon, accept, append_log, assign, expire, finish,
    heartbeat, resume, step,
};
use crate::db::runs::{AttemptState, NewRun, RunState, create_run};

type StepRow = (i64, String, String, Option<i64>, Option<i64>, Option<i64>);
type EventRow = (String, String, Option<i64>, Option<i64>, String, String);

const SHA: &str = "1111111111111111111111111111111111111111";

/// User 1, repository 1 and runners 1 (`build-1`) and 2 (`build-2`).
async fn populate(writer: &mut SqliteConnection) {
    sqlx::query("INSERT INTO user (id, public_id, name, kind, is_admin, source, created_at) VALUES (1, '00000000000000000000000001', 'alice', 'local', 0, 'config', 0);
        INSERT INTO repo (id, public_id, owner_id, name, visibility, ci_mode, default_branch, description, created_at, config_key) VALUES
        (1, '00000000000000000000000001', 1, 'demo', 'public', 'push', 'main', '', 0, 'demo');
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
            worktree: None,
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
            produced_at_ms: sequence as i64 * 1000,
            data,
        },
        10,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    ack.map(|(highest, _)| highest)
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
    let mut tx = writer.begin().await.unwrap();
    assert_eq!(
        append_log(
            &mut tx,
            &logs,
            1,
            &Chunk {
                attempt: &attempt,
                sequence: 1,
                step: 0,
                data: b"replayed",
                produced_at_ms: 99999,
            },
            10
        )
        .await
        .unwrap(),
        Some((5, false))
    );
    tx.commit().await.unwrap();
    let times: Vec<i64> = sqlx::query_scalar("SELECT produced_at_ms FROM log_chunk ORDER BY seq")
        .fetch_all(&readers)
        .await
        .unwrap();
    assert_eq!(times, [1000, 2000, 3000, 4000, 5000]);
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

#[tokio::test]
async fn empty_log_chunks_advance_acknowledgments_without_growing_the_index() {
    let dir = TempDir::new();
    let (mut writer, readers) = open(&dir.database()).await.unwrap();
    populate(&mut writer).await;
    let (_, attempt) = running(&mut writer).await;
    for (sequence, data, expected) in [
        (1, &b""[..], 1),
        (2, b"", 2),
        (4, b"gap", 2),
        (3, b"one", 3),
        (4, b"", 4),
        (4, b"duplicate", 4),
        (5, b"", 5),
        (6, b"two", 6),
        (7, b"", 7),
    ] {
        assert_eq!(
            log(&mut writer, &dir.0, 1, &attempt, sequence, data).await,
            Some(expected)
        );
    }
    let index: Vec<(i64, i64, i64)> =
        sqlx::query_as("SELECT seq, offset, length FROM log_chunk ORDER BY seq")
            .fetch_all(&mut writer)
            .await
            .unwrap();
    assert_eq!(index, [(2, 0, 0), (5, 0, 3), (7, 3, 3)]);
    assert_eq!(
        std::fs::read(dir.0.join(format!("{attempt}.log"))).unwrap(),
        b"onetwo"
    );
    readers.close().await;
    writer.close().await.unwrap();
}

#[tokio::test]
async fn a_log_past_its_bound_is_truncated_once_and_later_chunks_are_acknowledged() {
    let dir = TempDir::new();
    let (mut writer, readers) = open(&dir.database()).await.unwrap();
    populate(&mut writer).await;
    let (_, attempt) = running(&mut writer).await;
    let logs = dir.0.clone();
    // Three bytes short of the bound, stored as one sparse chunk.
    assert_eq!(log(&mut writer, &logs, 1, &attempt, 1, b"a").await, Some(1));
    let path = logs.join(format!("{attempt}.log"));
    std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(MAX_LOG as u64 - 3)
        .unwrap();
    sqlx::query("UPDATE log_chunk SET length = ?")
        .bind(MAX_LOG - 3)
        .execute(&mut writer)
        .await
        .unwrap();
    let mut tx = writer.begin().await.unwrap();
    let chunk = |sequence, data| Chunk {
        attempt: &attempt,
        sequence,
        step: 0,
        produced_at_ms: sequence as i64 * 1000,
        data,
    };
    // The chunk crossing the bound keeps what fits, then the marker line.
    assert_eq!(
        append_log(&mut tx, &logs, 1, &chunk(2, b"bcdefg"), 10)
            .await
            .unwrap(),
        Some((2, true))
    );
    // Later chunks are acknowledged in sequence and stored nowhere.
    for sequence in 3..6 {
        assert_eq!(
            append_log(&mut tx, &logs, 1, &chunk(sequence, b"more"), 10)
                .await
                .unwrap(),
            Some((sequence, false))
        );
    }
    assert_eq!(
        append_log(&mut tx, &logs, 1, &chunk(4, b"replay"), 10)
            .await
            .unwrap(),
        Some((5, false))
    );
    tx.commit().await.unwrap();
    let times: Vec<i64> = sqlx::query_scalar("SELECT produced_at_ms FROM log_chunk ORDER BY seq")
        .fetch_all(&readers)
        .await
        .unwrap();
    assert_eq!(times, [1000, 2000]);
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(bytes.len() as i64, MAX_LOG + TRUNCATED.len() as i64);
    assert!(bytes.ends_with(&[b"bcd", TRUNCATED].concat()));
    let index: Vec<(i64, i64, i64)> =
        sqlx::query_as("SELECT seq, offset, length FROM log_chunk ORDER BY seq")
            .fetch_all(&mut writer)
            .await
            .unwrap();
    assert_eq!(
        index,
        [
            (1, 0, MAX_LOG - 3),
            (5, MAX_LOG - 3, 3 + TRUNCATED.len() as i64)
        ]
    );
    readers.close().await;
    writer.close().await.unwrap();
}

#[tokio::test]
async fn only_running_unexpired_attempts_accept_new_logs_and_steps() {
    for (state, deadline, writable) in [
        ("running", 20, true),  // The lease includes its last millisecond.
        ("running", 19, false), // Expired, before the scheduler's sweep.
        ("succeeded", 30_000, false),
        ("failed", 30_000, false),
        ("cancelled", 30_000, false),
        ("lost", 30_000, false), // No replacement yet: still the latest attempt.
    ] {
        let dir = TempDir::new();
        let (mut writer, readers) = open(&dir.database()).await.unwrap();
        populate(&mut writer).await;
        let (_, attempt) = running(&mut writer).await;
        assert_eq!(
            log(&mut writer, &dir.0, 1, &attempt, 1, b"original").await,
            Some(1)
        );
        assert!(
            step(
                &mut writer,
                1,
                &attempt,
                (0, "flake-check", "failed", Some(1)),
                10
            )
            .await
            .unwrap()
        );
        sqlx::query("UPDATE job_attempt SET state = ?, lease_expires_at = ?, finished_at = ?")
            .bind(state)
            .bind(deadline)
            .bind((state != "running").then_some(15_i64))
            .execute(&mut writer)
            .await
            .unwrap();
        let mut tx = writer.begin().await.unwrap();
        // Replayed chunks are acknowledged even after termination or lease expiry.
        for (sequence, data, expected) in [
            (1, &b"duplicate"[..], Some(1)),
            (2, &b"new"[..], writable.then_some(2)),
        ] {
            assert_eq!(
                append_log(
                    &mut tx,
                    &dir.0,
                    1,
                    &Chunk {
                        attempt: &attempt,
                        sequence,
                        step: 0,
                        produced_at_ms: sequence as i64 * 1000,
                        data,
                    },
                    20
                )
                .await
                .unwrap()
                .map(|(highest, _)| highest),
                expected,
                "{state}, lease {deadline}"
            );
        }
        assert_eq!(
            step(&mut tx, 1, &attempt, (0, "changed", "running", None), 20)
                .await
                .unwrap(),
            writable
        );
        // An absent step must not be created either.
        assert_eq!(
            step(&mut tx, 1, &attempt, (1, "new", "running", None), 20)
                .await
                .unwrap(),
            writable
        );
        tx.commit().await.unwrap();
        let path = dir.0.join(format!("{attempt}.log"));
        assert_eq!(
            std::fs::read(&path).unwrap(),
            if writable {
                &b"originalnew"[..]
            } else {
                &b"original"[..]
            }
        );
        assert_eq!(
            count(&mut writer, "SELECT count(*) FROM log_chunk").await,
            if writable { 2 } else { 1 }
        );
        if !writable {
            let steps: Vec<StepRow> = sqlx::query_as(
                "SELECT idx, name, state, exit_code, started_at, finished_at FROM step",
            )
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
                    None,
                    Some(10)
                )]
            );
        }
        if state != "running" {
            assert_eq!(
                crate::db::retention::purge_logs(&mut writer, &dir.0, Duration::from_secs(1), 2000)
                    .await
                    .unwrap(),
                1
            );
            assert!(!path.exists());
            assert_eq!(
                log(&mut writer, &dir.0, 1, &attempt, 1, b"recreated").await,
                None
            );
            assert!(!path.exists());
            assert_eq!(
                count(&mut writer, "SELECT count(*) FROM log_chunk").await,
                0
            );
        }
        readers.close().await;
        writer.close().await.unwrap();
    }
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
async fn expired_acceptance_and_results_cannot_beat_the_sweep() {
    for deadline in [8, 9] {
        let dir = TempDir::new();
        let (mut writer, readers) = open(&dir.database()).await.unwrap();
        populate(&mut writer).await;
        let run = new_run(&mut writer, 1).await;
        let offers = [offer(1, &["nix"], &[], 1)];
        let (assigned, _) = pass(&mut writer, &offers, 2).await;
        let attempt = &assigned[0].attempt;
        sqlx::query("UPDATE job_attempt SET lease_expires_at = ?")
            .bind(deadline)
            .execute(&mut writer)
            .await
            .unwrap();
        let before = ended(&mut writer).await;
        let events = count(&mut writer, "SELECT count(*) FROM event").await;
        let mut tx = writer.begin().await.unwrap();
        let accepted = accept(&mut tx, (1, "build-1"), attempt, "accept", 9)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(
            accepted,
            if deadline < 9 {
                Outcome::Cancel
            } else {
                Outcome::Done(vec![(run.clone(), RunState::Running)])
            }
        );
        assert_eq!(
            result(
                &mut writer,
                (1, "build-1"),
                attempt,
                AttemptState::Succeeded,
                ""
            )
            .await,
            if deadline < 9 {
                Outcome::Cancel
            } else {
                Outcome::Done(vec![(run, RunState::Succeeded)])
            }
        );
        if deadline < 9 {
            assert_eq!(ended(&mut writer).await, before);
            assert_eq!(
                count(&mut writer, "SELECT count(*) FROM event").await,
                events
            );
            sweep(&mut writer, 9).await;
            assert_eq!(ended(&mut writer).await.0, "lost");
            let (replayed, _) = pass(&mut writer, &offers, 9).await;
            assert_eq!(replayed.len(), 1);
            assert_ne!(replayed[0].attempt, *attempt);
        } else {
            // A recorded result is still acknowledged after its lease expires.
            sqlx::query("UPDATE job_attempt SET lease_expires_at = 8")
                .execute(&mut writer)
                .await
                .unwrap();
            assert_eq!(
                result(
                    &mut writer,
                    (1, "build-1"),
                    attempt,
                    AttemptState::Failed,
                    "late"
                )
                .await,
                Outcome::Done(Vec::new())
            );
            assert_eq!(ended(&mut writer).await.0, "succeeded");
        }
        readers.close().await;
        writer.close().await.unwrap();
    }
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

#[tokio::test]
async fn heartbeats_do_not_keep_unaccepted_replays_alive_after_a_runner_resumes() {
    let dir = TempDir::new();
    let (mut writer, readers) = open(&dir.database()).await.unwrap();
    populate(&mut writer).await;
    let (_, first) = running(&mut writer).await;
    let offers = [offer(1, &["nix"], &[], 1)];
    // The runner freezes with its only worker occupied; the server replays its job.
    let expired = lease(&mut writer).await + 1;
    sweep(&mut writer, expired).await;
    let (assigned, _) = pass(&mut writer, &offers, expired).await;
    assert_eq!(assigned.len(), 1);
    assert_ne!(assigned[0].attempt, first);
    let deadline = lease(&mut writer).await;
    // On resuming, the runner refuses the replay at max_jobs = 1 but heartbeats work.
    for now in [expired + 10_000, expired + 20_000, deadline] {
        heartbeat(&mut writer, 1, now).await.unwrap();
        assert_eq!(lease(&mut writer).await, deadline);
    }
    sweep(&mut writer, deadline + 1).await;
    assert_eq!(jobs(&mut writer).await, [("pending".into(), None)]);
    let (assigned, _) = pass(&mut writer, &[offer(2, &["nix"], &[], 1)], deadline + 1).await;
    assert_eq!(assigned.len(), 1);
    assert_eq!(assigned[0].runner_id, 2);
    assert!(matches!(
        accept(
            &mut writer,
            (2, "build-2"),
            &assigned[0].attempt,
            "accept",
            deadline + 2
        )
        .await
        .unwrap(),
        Outcome::Done(_)
    ));
    heartbeat(&mut writer, 2, deadline + 10_000).await.unwrap();
    assert_eq!(lease(&mut writer).await, deadline + 10_000 + LEASE_MS);
    readers.close().await;
    writer.close().await.unwrap();
}

async fn sweep(writer: &mut SqliteConnection, now: i64) -> Vec<(String, RunState)> {
    let mut tx = writer.begin().await.unwrap();
    let changes = expire(&mut tx, now).await.unwrap();
    tx.commit().await.unwrap();
    changes
}

async fn lease(writer: &mut SqliteConnection) -> i64 {
    count(
        writer,
        "SELECT lease_expires_at FROM job_attempt ORDER BY number DESC LIMIT 1",
    )
    .await
}

#[tokio::test]
async fn expired_leases_are_replayed_up_to_max_attempts_then_fail_the_run() {
    let dir = TempDir::new();
    let (mut writer, readers) = open(&dir.database()).await.unwrap();
    populate(&mut writer).await;
    let (run, first) = running(&mut writer).await;
    let offers = [offer(1, &["nix"], &[], 1)];
    let mut attempts = vec![first];
    for number in 1..=3 {
        let expiry = lease(&mut writer).await;
        // The lease holds up to its last millisecond.
        assert!(sweep(&mut writer, expiry).await.is_empty());
        assert_eq!(
            count(
                &mut writer,
                "SELECT count(*) FROM job_attempt WHERE state = 'lost'"
            )
            .await,
            number - 1
        );
        let changes = sweep(&mut writer, expiry + 1).await;
        assert_eq!(
            count(
                &mut writer,
                "SELECT count(*) FROM job_attempt WHERE state = 'lost'"
            )
            .await,
            number
        );
        if number < 3 {
            assert!(changes.is_empty());
            assert_eq!(jobs(&mut writer).await, [("pending".into(), None)]);
            let (assigned, _) = pass(&mut writer, &offers, expiry + 2).await;
            attempts.push(assigned[0].attempt.clone());
        } else {
            assert_eq!(changes, [(run.clone(), RunState::Failed)]);
        }
    }
    assert_eq!(
        count(&mut writer, "SELECT max(number) FROM job_attempt").await,
        3
    );
    let row: (String, String, Option<String>, Option<String>) = sqlx::query_as(
        "SELECT job.state, run.state, run.failure_check, run.failure_reason FROM job JOIN run ON run.id = job.run_id",
    )
    .fetch_one(&mut writer)
    .await
    .unwrap();
    assert_eq!(
        row,
        (
            "failed".into(),
            "failed".into(),
            None,
            Some("lease-expired".into())
        )
    );
    let events: Vec<(String, String, String, serde_json::Value)> = sqlx::query_as::<_, (String, String, String, String)>(
        "SELECT kind, principal_kind, correlation_id, payload FROM event WHERE kind IN ('attempt-lost', 'attempt-replayed', 'run-finished') ORDER BY id",
    )
    .fetch_all(&mut writer)
    .await
    .unwrap()
    .into_iter()
    .map(|(kind, principal, correlation, payload)| {
        (kind, principal, correlation, serde_json::from_str(&payload).unwrap())
    })
    .collect();
    let job: String = sqlx::query_scalar("SELECT public_id FROM job")
        .fetch_one(&mut writer)
        .await
        .unwrap();
    let mut expected = Vec::new();
    for (number, attempt) in attempts.iter().enumerate() {
        expected.push((
            "attempt-lost".to_owned(),
            "system".to_owned(),
            attempt.clone(),
            serde_json::json!({ "run": run, "attempt": attempt, "runner": "build-1" }),
        ));
        expected.push(if number < 2 {
            (
                "attempt-replayed".into(),
                "system".into(),
                attempt.clone(),
                serde_json::json!({ "run": run, "job": job, "attempt": attempt }),
            )
        } else {
            (
                "run-finished".into(),
                "system".into(),
                attempt.clone(),
                serde_json::json!({ "run": run, "state": "failed", "check": null, "reason": "lease-expired" }),
            )
        });
    }
    assert_eq!(events, expected);
    // A late result from a lost attempt is acknowledged and ignored, or cancelled when a
    // replay superseded it.
    assert_eq!(
        result(
            &mut writer,
            (1, "build-1"),
            &attempts[2],
            AttemptState::Succeeded,
            ""
        )
        .await,
        Outcome::Done(Vec::new())
    );
    assert_eq!(
        result(
            &mut writer,
            (1, "build-1"),
            &attempts[0],
            AttemptState::Succeeded,
            ""
        )
        .await,
        Outcome::Cancel
    );
    assert_eq!(ended(&mut writer).await.0, "lost");
    readers.close().await;
    writer.close().await.unwrap();
}

#[tokio::test]
async fn reconnection_continues_only_current_leased_attempts() {
    let dir = TempDir::new();
    let (mut writer, readers) = open(&dir.database()).await.unwrap();
    populate(&mut writer).await;
    let offers = [offer(1, &["nix"], &[], 3)];
    // Attempt `stale` is superseded by a replay; `done` has its result; `expired` passed
    // its lease without a sweep yet; `live` is current and leased, with two log chunks.
    new_run(&mut writer, 1).await;
    let (stale, _) = pass(&mut writer, &offers, 2).await;
    sweep(&mut writer, 2 + LEASE_MS + 1).await;
    let (replayed, _) = pass(&mut writer, &offers, 100_000).await;
    new_run(&mut writer, 3).await;
    let (done, _) = pass(&mut writer, &offers, 100_000).await;
    result(
        &mut writer,
        (1, "build-1"),
        &done[0].attempt,
        AttemptState::Succeeded,
        "",
    )
    .await;
    new_run(&mut writer, 4).await;
    let (expired, _) = pass(&mut writer, &offers, 50_000).await;
    for sequence in [1, 2] {
        log(&mut writer, &dir.0, 1, &replayed[0].attempt, sequence, b"x").await;
    }
    let now = 100_000 + 1;
    let ids: Vec<String> = [
        &replayed[0].attempt,
        &stale[0].attempt,
        &done[0].attempt,
        &expired[0].attempt,
        &"7ZZZZZZZZZZZZZZZZZZZZZZZZZ".to_owned(),
    ]
    .into_iter()
    .cloned()
    .collect();
    let mut tx = writer.begin().await.unwrap();
    assert_eq!(
        resume(&mut tx, 1, &ids, now).await.unwrap(),
        [Some(2), None, None, None, None]
    );
    // Another runner holds none of them.
    assert_eq!(resume(&mut tx, 2, &ids[..1], now).await.unwrap(), [None]);
    tx.commit().await.unwrap();
    let renewed: i64 =
        sqlx::query_scalar("SELECT lease_expires_at FROM job_attempt WHERE public_id = ?")
            .bind(&replayed[0].attempt)
            .fetch_one(&mut writer)
            .await
            .unwrap();
    assert_eq!(renewed, now + LEASE_MS);
    readers.close().await;
    writer.close().await.unwrap();
}

#[tokio::test]
async fn admission_loses_only_the_running_attempts_its_hello_does_not_list() {
    let dir = TempDir::new();
    let (mut writer, readers) = open(&dir.database()).await.unwrap();
    populate(&mut writer).await;
    let (_, unlisted) = running(&mut writer).await;
    new_run(&mut writer, 4).await;
    let (listed, _) = pass(&mut writer, &[offer(1, &["nix"], &[], 2)], 5).await;
    new_run(&mut writer, 6).await;
    let (other, _) = pass(&mut writer, &[offer(2, &["nix"], &[], 1)], 7).await;
    let held = [listed[0].attempt.clone()];
    let mut tx = writer.begin().await.unwrap();
    // A replay changes no run state.
    assert!(abandon(&mut tx, 1, &held, 8).await.unwrap().is_empty());
    tx.commit().await.unwrap();
    let states: Vec<(String, String)> =
        sqlx::query_as("SELECT public_id, state FROM job_attempt ORDER BY id")
            .fetch_all(&mut writer)
            .await
            .unwrap();
    assert_eq!(
        states,
        [
            (unlisted.clone(), "lost".into()),
            (listed[0].attempt.clone(), "running".into()),
            (other[0].attempt.clone(), "running".into()),
        ]
    );
    assert_eq!(
        jobs(&mut writer).await,
        [
            ("pending".into(), None),
            ("assigned".into(), None),
            ("assigned".into(), None)
        ]
    );
    // The new process's heartbeats do not revive it.
    heartbeat(&mut writer, 1, 9).await.unwrap();
    let lost: (String, i64) =
        sqlx::query_as("SELECT state, lease_expires_at FROM job_attempt WHERE public_id = ?")
            .bind(&unlisted)
            .fetch_one(&mut writer)
            .await
            .unwrap();
    assert_eq!(lost, ("lost".into(), 2 + LEASE_MS));
    readers.close().await;
    writer.close().await.unwrap();
}

#[tokio::test]
async fn restore_marker_invalidates_unexpired_attempts_and_replays_or_fails() {
    for (accepted, replayable, max_attempts) in [
        (false, true, 3),
        (true, true, 3),
        (true, true, 1),
        (true, false, 3),
    ] {
        let dir = TempDir::new();
        let (mut writer, readers) = open(&dir.database()).await.unwrap();
        populate(&mut writer).await;
        let run = new_run(&mut writer, 1).await;
        let offers = [offer(1, &["nix"], &[], 1)];
        let (assigned, _) = pass(&mut writer, &offers, 2).await;
        let attempt = &assigned[0].attempt;
        if accepted {
            let mut tx = writer.begin().await.unwrap();
            accept(&mut tx, (1, "build-1"), attempt, "accept", 3)
                .await
                .unwrap();
            tx.commit().await.unwrap();
        }
        assert_eq!(
            log(&mut writer, &dir.0, 1, attempt, 1, b"acked").await,
            Some(1)
        );
        sqlx::query("UPDATE job SET replayable = ?, max_attempts = ?")
            .bind(replayable)
            .bind(max_attempts)
            .execute(&mut writer)
            .await
            .unwrap();
        let pending = new_run(&mut writer, 4).await;
        assert!(lease(&mut writer).await > 5);
        // Ordinary restarts preserve the unexpired attempt.
        crate::restore::reconcile(&mut writer, &dir.0, 5)
            .await
            .unwrap();
        assert_eq!(
            count(
                &mut writer,
                "SELECT count(*) FROM job_attempt WHERE state = 'running'"
            )
            .await,
            1
        );
        // Restore drops the index of output the runner has already discarded.
        sqlx::query("DELETE FROM log_chunk")
            .execute(&mut writer)
            .await
            .unwrap();
        std::fs::create_dir(dir.0.join("db")).unwrap();
        let marker = dir.0.join("db/restore-pending");
        std::fs::write(&marker, b"").unwrap();
        crate::restore::reconcile(&mut writer, &dir.0, 5)
            .await
            .unwrap();
        assert!(!marker.exists());
        assert_eq!(
            count(
                &mut writer,
                "SELECT count(*) FROM job_attempt WHERE state = 'lost' AND finished_at = 5"
            )
            .await,
            1
        );
        assert_eq!(
            resume(&mut writer, 1, std::slice::from_ref(attempt), 5)
                .await
                .unwrap(),
            [None]
        );
        let reason: String = sqlx::query_scalar(
            "SELECT json_extract(payload, '$.reason') FROM event WHERE kind = 'attempt-lost'",
        )
        .fetch_one(&mut writer)
        .await
        .unwrap();
        assert_eq!(reason, "restored");
        let state: (String, Option<String>) =
            sqlx::query_as("SELECT state, failure_reason FROM run WHERE public_id = ?")
                .bind(&run)
                .fetch_one(&mut writer)
                .await
                .unwrap();
        if replayable && max_attempts > 1 {
            assert_eq!(state.1, None);
            let (replay, _) = pass(&mut writer, &offers, 6).await;
            assert_eq!(replay.len(), 1);
            assert_ne!(replay[0].attempt, *attempt);
            assert_eq!(
                count(&mut writer, "SELECT max(number) FROM job_attempt").await,
                2
            );
        } else {
            assert_eq!(state, ("failed".into(), Some("restored".into())));
        }
        // Consumed markers do not invalidate later work on an ordinary restart.
        crate::restore::reconcile(&mut writer, &dir.0, 7)
            .await
            .unwrap();
        assert_eq!(
            count(
                &mut writer,
                "SELECT count(*) FROM event WHERE kind = 'attempt-lost'"
            )
            .await,
            1
        );
        let pending_state: String = sqlx::query_scalar("SELECT state FROM run WHERE public_id = ?")
            .bind(pending)
            .fetch_one(&mut writer)
            .await
            .unwrap();
        assert_eq!(pending_state, "queued");
        readers.close().await;
        writer.close().await.unwrap();
    }
}

#[tokio::test]
async fn restore_reconciliation_retries_failures_and_a_crash_after_commit() {
    let dir = TempDir::new();
    let (mut writer, readers) = open(&dir.database()).await.unwrap();
    populate(&mut writer).await;
    let (_, terminal) = running(&mut writer).await;
    result(
        &mut writer,
        (1, "build-1"),
        &terminal,
        AttemptState::Succeeded,
        "",
    )
    .await;
    let (_, attempt) = running(&mut writer).await;
    std::fs::create_dir(dir.0.join("db")).unwrap();
    let marker = dir.0.join("db/restore-pending");
    std::fs::write(&marker, b"").unwrap();
    sqlx::query("CREATE TRIGGER refuse_loss BEFORE INSERT ON event WHEN NEW.kind = 'attempt-lost' BEGIN SELECT RAISE(ABORT, 'injected failure'); END")
        .execute(&mut writer).await.unwrap();
    assert!(
        crate::restore::reconcile(&mut writer, &dir.0, 5)
            .await
            .is_err()
    );
    assert!(marker.exists());
    assert_eq!(
        count(
            &mut writer,
            "SELECT count(*) FROM job_attempt WHERE state = 'lost'"
        )
        .await,
        0
    );
    sqlx::query("DROP TRIGGER refuse_loss")
        .execute(&mut writer)
        .await
        .unwrap();
    crate::restore::reconcile(&mut writer, &dir.0, 5)
        .await
        .unwrap();
    // Simulate a crash after committing but before unlinking the marker.
    std::fs::write(&marker, b"").unwrap();
    crate::restore::reconcile(&mut writer, &dir.0, 6)
        .await
        .unwrap();
    assert!(!marker.exists());
    assert_eq!(
        count(
            &mut writer,
            "SELECT count(*) FROM event WHERE kind = 'attempt-lost'"
        )
        .await,
        1
    );
    assert_eq!(resume(&mut writer, 1, &[attempt], 6).await.unwrap(), [None]);
    assert_eq!(
        count(
            &mut writer,
            "SELECT count(*) FROM job_attempt WHERE state = 'succeeded'"
        )
        .await,
        1
    );
    readers.close().await;
    writer.close().await.unwrap();
}
