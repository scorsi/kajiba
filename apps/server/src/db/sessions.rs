//! Browser sessions (ADR 0014, 0079): creation by the WebAuthn ceremonies, resolution
//! by the auth interceptor, deletion by logout, purge of expired rows.
use super::credentials::Found;
use sqlx::SqliteConnection;
use std::time::Duration;

/// Opens a session for `user` expiring `ttl` after `now` (absolute, ADR 0014) and returns
/// the cookie value; only its SHA-256 is stored. `now` is UTC epoch milliseconds.
pub async fn create(
    writer: &mut SqliteConnection,
    user: i64,
    ttl: Duration,
    now: i64,
) -> Result<String, sqlx::Error> {
    // Checked before the write (ADR 0029): configuration bounds the TTL, so this is a bug.
    let expires_at = super::expiry(now, ttl)
        .ok_or_else(|| sqlx::Error::Io(std::io::Error::other("session expiry overflows")))?;
    let value: String = super::secret_bytes()
        .map_err(sqlx::Error::Io)?
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    sqlx::query(
        "INSERT INTO session (user_id, token_hash, created_at, expires_at, last_seen_at)
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(user)
    .bind(kajiba_core::sha256(value.as_bytes()).as_slice())
    .bind(now)
    .bind(expires_at)
    .bind(now)
    .execute(writer)
    .await?;
    Ok(value)
}

/// Logout: deletes the session whose SHA-256 is `hash`, if any.
pub async fn delete(writer: &mut SqliteConnection, hash: &[u8]) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM session WHERE token_hash = ?")
        .bind(hash)
        .execute(writer)
        .await
        .map(drop)
}

/// Deletes the sessions expired at `now` (ADR 0079); returns how many.
pub async fn purge(writer: &mut SqliteConnection, now: i64) -> Result<u64, sqlx::Error> {
    sqlx::query("DELETE FROM session WHERE expires_at <= ?")
        .bind(now)
        .execute(writer)
        .await
        .map(|done| done.rows_affected())
}

const PURGE_INTERVAL: Duration = Duration::from_secs(3600);

/// Purges at startup, then every hour; a failure is logged and the next pass retries.
pub async fn purge_periodically(writer: &super::Writer) {
    let mut interval = tokio::time::interval(PURGE_INTERVAL);
    loop {
        // The first tick completes at once: the startup purge.
        interval.tick().await;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_millis() as i64);
        match purge(&mut *writer.lock().await, now).await {
            Ok(0) => {}
            Ok(count) => tracing::info!(event = "sessions-purged", count),
            Err(err) => tracing::warn!(event = "sessions-purge-failed", err = %err),
        }
    }
}

/// The session whose SHA-256 is `hash`; a session holds every scope of its user.
pub async fn find(
    readers: impl sqlx::SqliteExecutor<'_>,
    hash: &[u8],
) -> Result<Option<Found>, sqlx::Error> {
    sqlx::query_as(
        "SELECT session.id, user_id, NULL AS scopes, expires_at, 0 AS revoked,
                user.disabled_at IS NOT NULL AS disabled, last_seen_at AS last_used_at
         FROM session JOIN user ON user.id = session.user_id WHERE token_hash = ?",
    )
    .bind(hash)
    .fetch_optional(readers)
    .await
}

/// Records activity, at most once per minute per session (ADR 0078); expiry stays absolute.
pub async fn seen(writer: &mut SqliteConnection, id: i64, now: i64) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE session SET last_seen_at = ?1 WHERE id = ?2 AND last_seen_at <= ?1 - 60000")
        .bind(now)
        .bind(id)
        .execute(writer)
        .await
        .map(drop)
}

/// Records a step-up re-authentication of session `id` at `now` (ADR 0143); false when the
/// session is gone or expired.
pub async fn stepped_up(
    writer: &mut SqliteConnection,
    id: i64,
    now: i64,
) -> Result<bool, sqlx::Error> {
    sqlx::query("UPDATE session SET step_up_at = ?1 WHERE id = ?2 AND expires_at > ?1")
        .bind(now)
        .bind(id)
        .execute(writer)
        .await
        .map(|done| done.rows_affected() == 1)
}

/// Whether session `id` stepped up less than `window` milliseconds before `now`.
pub async fn step_up_fresh(
    connection: impl sqlx::SqliteExecutor<'_>,
    id: i64,
    window: i64,
    now: i64,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM session
         WHERE id = ?1 AND step_up_at > ?2 - ?3 AND step_up_at <= ?2 AND expires_at > ?2)",
    )
    .bind(id)
    .bind(now)
    .bind(window)
    .fetch_one(connection)
    .await
}
