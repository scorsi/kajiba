//! Browser sessions (ADR 0014): resolution by the auth interceptor.
use super::credentials::Found;
use sqlx::SqliteConnection;

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
