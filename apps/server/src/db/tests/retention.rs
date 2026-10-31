use super::*;
use crate::db::retention::{next_run, purge_logs};
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
