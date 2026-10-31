use super::*;
use crate::db::retention::{
    drop_finished_bundles, fail_missing_bundles, next_run, purge_logs, purge_worktree_runs,
};
use std::path::Path;

const DAY: i64 = 86_400_000;

/// Attempts 1 (finished at `DAY`), 2 (finished at `DAY + 1`) and 3 (running), each with
/// two indexed chunks and its log file.
async fn populate(writer: &mut SqliteConnection, logs: &Path) {
    sqlx::query("INSERT INTO user (id, public_id, name, kind, is_admin, source, created_at) VALUES (1, '00000000000000000000000001', 'alice', 'local', 0, 'config', 0);
        INSERT INTO repo (id, public_id, owner_id, name, visibility, ci_enabled, default_branch, description, created_at, config_key) VALUES
        (1, '00000000000000000000000001', 1, 'demo', 'public', 1, 'main', '', 0, 'demo');
        INSERT INTO runner (id, public_id, name, capabilities, version, connected) VALUES (1, '00000000000000000000000001', 'build-1', '{}', '', 0);
        INSERT INTO run (id, public_id, repo_id, sha, trigger, state, created_at) VALUES (1, '00000000000000000000000001', 1, 'a', 'manual', 'running', 0);
        INSERT INTO job (id, public_id, run_id, name, requirements, replayable, max_attempts, state, created_at) VALUES (1, '00000000000000000000000001', 1, 'flake-check', '{}', 1, 3, 'running', 0);
        INSERT INTO job_attempt (id, public_id, job_id, number, runner_id, state, lease_expires_at, started_at, finished_at) VALUES
        (1, '00000000000000000000000001', 1, 1, 1, 'lost', 0, 0, 86400000),
        (2, '00000000000000000000000002', 1, 2, 1, 'failed', 0, 0, 86400001),
        (3, '00000000000000000000000003', 1, 3, 1, 'running', 0, 0, NULL);
        INSERT INTO log_chunk (attempt_id, seq, step_idx, offset, length)
        SELECT id, seq, 0, (seq - 1) * 3, 3 FROM job_attempt, (SELECT 1 AS seq UNION SELECT 2)")
        .execute(writer).await.unwrap();
    fs::create_dir(logs).unwrap();
    for id in 1..=3 {
        fs::write(
            logs.join(format!("0000000000000000000000000{id}.log")),
            "abcdef",
        )
        .unwrap();
    }
}

async fn indexed(writer: &mut SqliteConnection) -> Vec<i64> {
    sqlx::query_scalar("SELECT DISTINCT attempt_id FROM log_chunk ORDER BY attempt_id")
        .fetch_all(writer)
        .await
        .unwrap()
}

#[tokio::test]
async fn logs_go_after_their_retention_and_runs_stay() {
    let dir = TempDir::new();
    let logs = dir.0.join("logs");
    let (mut writer, _) = open(&dir.database()).await.unwrap();
    populate(&mut writer, &logs).await;
    let minute = Duration::from_secs(60);
    let files = || {
        let mut names: Vec<_> = fs::read_dir(&logs)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    };
    // Finished exactly one retention ago: kept.
    assert_eq!(
        purge_logs(&mut writer, &logs, minute, DAY + 60_000)
            .await
            .unwrap(),
        0
    );
    assert_eq!(indexed(&mut writer).await, [1, 2, 3]);
    // One millisecond later: attempt 1 goes, file and index together.
    assert_eq!(
        purge_logs(&mut writer, &logs, minute, DAY + 60_001)
            .await
            .unwrap(),
        1
    );
    assert_eq!(indexed(&mut writer).await, [2, 3]);
    assert_eq!(
        files(),
        [
            "00000000000000000000000002.log",
            "00000000000000000000000003.log"
        ]
    );
    // Much later: the running attempt is untouched; a missing file is not an error.
    fs::remove_file(logs.join("00000000000000000000000002.log")).unwrap();
    assert_eq!(
        purge_logs(&mut writer, &logs, minute, 10 * DAY)
            .await
            .unwrap(),
        1
    );
    assert_eq!(indexed(&mut writer).await, [3]);
    assert_eq!(files(), ["00000000000000000000000003.log"]);
    let kept: (i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM run), (SELECT count(*) FROM job), (SELECT count(*) FROM job_attempt)",
    )
    .fetch_one(&mut writer)
    .await
    .unwrap();
    assert_eq!(kept, (1, 1, 3));
}

#[test]
fn the_next_run_is_the_next_local_occurrence() {
    for now in [0, 1_700_000_000, 1_711_846_800, 1_729_990_800] {
        for run_at in [(0, 0), (3, 0), (23, 59)] {
            let at = next_run(now, run_at);
            assert!(at > now && at <= now + 25 * 3600, "{now} {run_at:?} {at}");
            // SAFETY: localtime_r writes only the `tm` we own.
            let tm = unsafe {
                let mut tm: libc::tm = std::mem::zeroed();
                libc::localtime_r(&(at as libc::time_t), &mut tm);
                tm
            };
            assert_eq!((tm.tm_hour as u8, tm.tm_min as u8), run_at);
        }
    }
}

/// Worktree runs 2 (created at `DAY`, running, attempt 4 with a log), 3 (created at
/// `DAY + 1`, queued) and 4 (created at 0, succeeded), each with its bundle, beside
/// `populate`'s manual run 1.
async fn worktree_runs(writer: &mut SqliteConnection, logs: &Path, bundles: &Path) {
    populate(writer, logs).await;
    sqlx::query("INSERT INTO run (id, public_id, repo_id, sha, trigger, state, created_at) VALUES
        (2, '00000000000000000000000002', 1, 'b', 'worktree', 'running', 86400000),
        (3, '00000000000000000000000003', 1, 'c', 'worktree', 'queued', 86400001),
        (4, '00000000000000000000000004', 1, 'd', 'worktree', 'succeeded', 0);
        INSERT INTO job (id, public_id, run_id, name, requirements, replayable, max_attempts, state, created_at) VALUES
        (2, '00000000000000000000000002', 2, 'flake-check', '{}', 1, 3, 'running', 0),
        (3, '00000000000000000000000003', 3, 'flake-check', '{}', 1, 3, 'pending', 0),
        (4, '00000000000000000000000004', 4, 'flake-check', '{}', 1, 3, 'succeeded', 0);
        INSERT INTO job_attempt (id, public_id, job_id, number, runner_id, state, lease_expires_at, started_at, finished_at) VALUES
        (4, '00000000000000000000000004', 2, 1, 1, 'running', 0, 0, NULL);
        INSERT INTO log_chunk (attempt_id, seq, step_idx, offset, length) VALUES (4, 1, 0, 0, 3)")
        .execute(&mut *writer).await.unwrap();
    fs::write(logs.join("00000000000000000000000004.log"), "abc").unwrap();
    fs::create_dir(bundles).unwrap();
    for id in 2..=4 {
        fs::write(
            bundles.join(format!("0000000000000000000000000{id}.bundle")),
            "b",
        )
        .unwrap();
    }
}

async fn run_ids(writer: &mut SqliteConnection) -> Vec<i64> {
    sqlx::query_scalar("SELECT id FROM run ORDER BY id")
        .fetch_all(writer)
        .await
        .unwrap()
}

#[tokio::test]
async fn worktree_runs_go_whole_after_their_retention_unfinished_ones_cancelled_first() {
    let dir = TempDir::new();
    let (logs, bundles) = (dir.0.join("logs"), dir.0.join("bundles"));
    let (mut writer, _) = open(&dir.database()).await.unwrap();
    worktree_runs(&mut writer, &logs, &bundles).await;
    let minute = Duration::from_secs(60);
    // Created exactly one retention ago: kept (run 4, older, goes).
    assert_eq!(
        purge_worktree_runs(&mut writer, &logs, &bundles, minute, DAY + 60_000)
            .await
            .unwrap(),
        1
    );
    assert_eq!(run_ids(&mut writer).await, [1, 2, 3]);
    assert!(!bundles.join("00000000000000000000000004.bundle").exists());
    // One millisecond later: the running run 2 is cancelled, then deleted whole.
    assert_eq!(
        purge_worktree_runs(&mut writer, &logs, &bundles, minute, DAY + 60_001)
            .await
            .unwrap(),
        1
    );
    assert_eq!(run_ids(&mut writer).await, [1, 3]);
    assert!(!bundles.join("00000000000000000000000002.bundle").exists());
    assert!(!logs.join("00000000000000000000000004.log").exists());
    let gone: (i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM job WHERE id = 2), (SELECT count(*) FROM job_attempt WHERE id = 4),
                (SELECT count(*) FROM log_chunk WHERE attempt_id = 4)",
    )
    .fetch_one(&mut writer)
    .await
    .unwrap();
    assert_eq!(gone, (0, 0, 0));
    let finished: Vec<String> =
        sqlx::query_scalar("SELECT payload FROM event WHERE kind = 'run-finished'")
            .fetch_all(&mut writer)
            .await
            .unwrap();
    assert_eq!(finished.len(), 1);
    let payload: serde_json::Value = serde_json::from_str(&finished[0]).unwrap();
    assert_eq!(
        payload,
        serde_json::json!({ "run": "00000000000000000000000002", "state": "cancelled", "check": null, "reason": null })
    );
    // Normal runs, their attempts and logs are untouched.
    assert_eq!(indexed(&mut writer).await, [1, 2, 3]);
    let manual: String = sqlx::query_scalar("SELECT state FROM run WHERE id = 1")
        .fetch_one(&mut writer)
        .await
        .unwrap();
    assert_eq!(manual, "running");
}

#[tokio::test]
async fn an_unfinished_worktree_run_without_its_bundle_fails_at_startup() {
    let dir = TempDir::new();
    let (logs, bundles) = (dir.0.join("logs"), dir.0.join("bundles"));
    let (mut writer, _) = open(&dir.database()).await.unwrap();
    worktree_runs(&mut writer, &logs, &bundles).await;
    // As after a restore: bundles of the running run 2 and the finished run 4 are gone.
    fs::remove_file(bundles.join("00000000000000000000000002.bundle")).unwrap();
    fs::remove_file(bundles.join("00000000000000000000000004.bundle")).unwrap();
    assert_eq!(
        fail_missing_bundles(&mut writer, &bundles, 5)
            .await
            .unwrap(),
        ["00000000000000000000000002"]
    );
    let states: Vec<(String, Option<String>, String, Option<String>)> = sqlx::query_as(
        "SELECT run.state, run.failure_reason, job.state,
                (SELECT state FROM job_attempt WHERE job_id = job.id)
         FROM run JOIN job ON job.run_id = run.id WHERE run.id > 1 ORDER BY run.id",
    )
    .fetch_all(&mut writer)
    .await
    .unwrap();
    assert_eq!(
        states,
        [
            (
                "failed".into(),
                Some("bundle-missing".into()),
                "failed".into(),
                Some("failed".into())
            ),
            ("queued".into(), None, "pending".into(), None),
            ("succeeded".into(), None, "succeeded".into(), None),
        ]
    );
    // A queued run whose job never ran: the job is cancelled, the run fails.
    fs::remove_file(bundles.join("00000000000000000000000003.bundle")).unwrap();
    fail_missing_bundles(&mut writer, &bundles, 6)
        .await
        .unwrap();
    let run3: (String, String) = sqlx::query_as(
        "SELECT run.state, job.state FROM run JOIN job ON job.run_id = run.id WHERE run.id = 3",
    )
    .fetch_one(&mut writer)
    .await
    .unwrap();
    assert_eq!(run3, ("failed".into(), "cancelled".into()));
}

#[tokio::test]
async fn a_finished_worktree_runs_bundle_goes_at_once_its_run_and_logs_stay() {
    let dir = TempDir::new();
    let (logs, bundles) = (dir.0.join("logs"), dir.0.join("bundles"));
    let (mut writer, _) = open(&dir.database()).await.unwrap();
    worktree_runs(&mut writer, &logs, &bundles).await;
    // A bundle renamed before its run's row commits is not touched.
    fs::write(bundles.join("00000000000000000000000009.bundle"), "b").unwrap();
    assert_eq!(
        drop_finished_bundles(&mut writer, &bundles).await.unwrap(),
        1
    );
    let mut left: Vec<_> = fs::read_dir(&bundles)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    left.sort();
    assert_eq!(
        left,
        [
            "00000000000000000000000002.bundle",
            "00000000000000000000000003.bundle",
            "00000000000000000000000009.bundle"
        ]
    );
    assert_eq!(run_ids(&mut writer).await, [1, 2, 3, 4]);
    // The running run fails: its bundle goes on the next pass, its log stays.
    sqlx::query("UPDATE run SET state = 'failed' WHERE id = 2")
        .execute(&mut writer)
        .await
        .unwrap();
    assert_eq!(
        drop_finished_bundles(&mut writer, &bundles).await.unwrap(),
        1
    );
    assert!(!bundles.join("00000000000000000000000002.bundle").exists());
    assert!(logs.join("00000000000000000000000004.log").exists());
}
