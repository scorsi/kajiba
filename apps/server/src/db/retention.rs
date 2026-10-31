//! CI retention (0010, ADR 0115): logs of attempts finished more than `ci.log_retention`
//! ago are deleted, file then index; runs, jobs and attempts are kept.

use sqlx::{Connection, SqliteConnection};
use std::{path::Path, time::Duration};

/// Delete the log file and `log_chunk` rows of every attempt finished before
/// `now - retention`; returns how many attempts lost their logs. A file is removed
/// before its rows, so an interrupted pass leaves rows that the next one deletes.
pub async fn purge_logs(
    writer: &mut SqliteConnection,
    logs: &Path,
    retention: Duration,
    now: i64,
) -> Result<u64, String> {
    let cutoff =
        super::expiry(0, retention).map_or(i64::MIN, |retention| now.saturating_sub(retention));
    let mut tx = writer.begin().await.map_err(|err| err.to_string())?;
    let attempts: Vec<(i64, String)> = sqlx::query_as(
        "SELECT id, public_id FROM job_attempt
         WHERE finished_at < ? AND state != 'running'
           AND EXISTS (SELECT 1 FROM log_chunk WHERE attempt_id = job_attempt.id)",
    )
    .bind(cutoff)
    .fetch_all(&mut *tx)
    .await
    .map_err(|err| err.to_string())?;
    for (id, public_id) in &attempts {
        match std::fs::remove_file(logs.join(format!("{public_id}.log"))) {
            Err(err) if err.kind() != std::io::ErrorKind::NotFound => {
                return Err(format!("{public_id}.log: {err}"));
            }
            _ => {}
        }
        sqlx::query("DELETE FROM log_chunk WHERE attempt_id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(|err| err.to_string())?;
    }
    tx.commit().await.map_err(|err| err.to_string())?;
    Ok(attempts.len() as u64)
}

/// The next epoch second, after `now`, whose local time is `hour:minute`.
pub fn next_run(now: i64, (hour, minute): (u8, u8)) -> i64 {
    // SAFETY: localtime_r and mktime only read and write the `tm` we own.
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&(now as libc::time_t), &mut tm);
        for _ in 0..3 {
            (tm.tm_hour, tm.tm_min, tm.tm_sec, tm.tm_isdst) = (hour.into(), minute.into(), 0, -1);
            let at = libc::mktime(&mut tm) as i64;
            if at > now {
                return at;
            }
            // mktime normalized `tm`: one day later, then retry (DST shifts the hour).
            tm.tm_mday += 1;
        }
    }
    now + 86400
}

/// The daily pass at `run_at`, local time; the operator also runs it on demand
/// (`kajiba-server admin retention`).
pub async fn purge_daily(
    writer: &super::Writer,
    logs: &Path,
    retention: Duration,
    run_at: (u8, u8),
) {
    loop {
        let now = super::runs::now() / 1000;
        let wait = next_run(now, run_at) - now;
        tokio::time::sleep(Duration::from_secs(wait.max(1) as u64)).await;
        let now = super::runs::now();
        match purge_logs(&mut *writer.lock().await, logs, retention, now).await {
            Ok(logs) => tracing::info!(event = "retention-completed", logs),
            Err(err) => tracing::warn!(event = "retention-failed", err),
        }
    }
}
