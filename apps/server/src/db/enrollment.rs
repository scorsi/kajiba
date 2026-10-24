use sqlx::{Connection, SqliteConnection};
use std::time::Duration;

#[derive(Debug)]
pub enum Error {
    UnknownUser,
    Disabled,
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
    // 32 bytes from SQLite's CSPRNG, as for public IDs; no extra dependency.
    let token: String = sqlx::query_scalar("SELECT lower(hex(randomblob(32)))")
        .fetch_one(&mut *tx)
        .await?;
    sqlx::query(
        "INSERT INTO enrollment_token (user_id, token_hash, created_at, expires_at) VALUES (?, ?, ?, ?)",
    )
    .bind(id)
    .bind(kajiba_core::sha256(token.as_bytes()).as_slice())
    .bind(now)
    .bind(now + ttl.as_millis() as i64)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(token)
}
