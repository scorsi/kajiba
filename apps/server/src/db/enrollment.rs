use sqlx::{Connection, SqliteConnection};
use std::time::Duration;

#[derive(Debug)]
pub enum Error {
    UnknownUser,
    Disabled,
    /// `now + ttl` is not representable; configuration bounds the TTL, so this is a bug.
    Expiry,
    Database(sqlx::Error),
}

impl From<sqlx::Error> for Error {
    fn from(err: sqlx::Error) -> Self {
        Self::Database(err)
    }
}

/// Recovery and enrollment (ADR 0014), in one transaction: revoke the user's passkeys,
/// sessions and runtime PATs, then replace any previous link with a new single-use token
/// stored hashed. Returns the token; `now` is UTC epoch milliseconds.
pub async fn issue(
    writer: &mut SqliteConnection,
    user: &str,
    ttl: Duration,
    now: i64,
) -> Result<String, Error> {
    // Checked before any write: a wrapped expiry could store a link already expired or eternal.
    let expires_at = super::expiry(now, ttl).ok_or(Error::Expiry)?;
    let mut tx = writer.begin().await?;
    let (id, disabled_at) =
        sqlx::query_as::<_, (i64, Option<i64>)>("SELECT id, disabled_at FROM user WHERE name = ?")
            .bind(user.to_ascii_lowercase())
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(Error::UnknownUser)?;
    if disabled_at.is_some() {
        return Err(Error::Disabled);
    }
    sqlx::query(
        "UPDATE api_token SET revoked_at = ? WHERE user_id = ? AND source = 'runtime' AND revoked_at IS NULL",
    )
    .bind(now)
    .bind(id)
    .execute(&mut *tx)
    .await?;
    for statement in [
        "DELETE FROM webauthn_credential WHERE user_id = ?",
        "DELETE FROM session WHERE user_id = ?",
        // UNIQUE(user_id): the new link replaces the previous one.
        "DELETE FROM enrollment_token WHERE user_id = ?",
    ] {
        sqlx::query(statement).bind(id).execute(&mut *tx).await?;
    }
    let bytes = super::secret_bytes().map_err(sqlx::Error::Io)?;
    let token: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    sqlx::query(
        "INSERT INTO enrollment_token (user_id, token_hash, created_at, expires_at) VALUES (?, ?, ?, ?)",
    )
    .bind(id)
    .bind(kajiba_core::sha256(token.as_bytes()).as_slice())
    .bind(now)
    .bind(expires_at)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(token)
}

/// The user a link enrolls: `(id, public_id, name)` when the token's SHA-256 names an
/// unexpired, unconsumed link of an active user. Unknown, superseded (replaced by a newer
/// link), consumed and expired links all read as `None`. `now` is UTC epoch milliseconds.
pub async fn find(
    readers: &sqlx::SqlitePool,
    hash: &[u8],
    now: i64,
) -> Result<Option<(i64, String, String)>, sqlx::Error> {
    sqlx::query_as(
        "SELECT user.id, user.public_id, user.name FROM enrollment_token
         JOIN user ON user.id = enrollment_token.user_id
         WHERE token_hash = ? AND consumed_at IS NULL AND expires_at > ? AND disabled_at IS NULL",
    )
    .bind(hash)
    .bind(now)
    .fetch_optional(readers)
    .await
}

/// Consumes `user`'s link atomically (ADR 0014): true only for the one caller that finds
/// it still valid, so concurrent ceremonies on the same link enroll once.
pub async fn consume(
    writer: &mut SqliteConnection,
    user: i64,
    hash: &[u8],
    now: i64,
) -> Result<bool, sqlx::Error> {
    sqlx::query(
        "UPDATE enrollment_token SET consumed_at = ?1
         WHERE user_id = ?2 AND token_hash = ?3 AND consumed_at IS NULL AND expires_at > ?1
           AND EXISTS (SELECT 1 FROM user WHERE id = ?2 AND disabled_at IS NULL)",
    )
    .bind(now)
    .bind(user)
    .bind(hash)
    .execute(writer)
    .await
    .map(|done| done.rows_affected() == 1)
}
