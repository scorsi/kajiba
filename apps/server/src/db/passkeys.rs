//! Passkeys (`webauthn_credential`, ADR 0014): written by enrollment, read and updated by login.
use sqlx::SqliteConnection;

/// Stores a passkey of `user`: its credential id and its webauthn-rs serialization.
pub async fn insert(
    writer: &mut SqliteConnection,
    user: i64,
    credential_id: &[u8],
    passkey: &str,
    name: &str,
    now: i64,
) -> Result<(), sqlx::Error> {
    let public_id = super::users::new_public_id(&mut *writer, now as u128).await?;
    sqlx::query(
        "INSERT INTO webauthn_credential (public_id, user_id, credential_id, passkey, name, created_at)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(public_id)
    .bind(user)
    .bind(credential_id)
    .bind(passkey)
    .bind(name)
    .bind(now)
    .execute(writer)
    .await
    .map(drop)
}

/// A passkey of an active user, by credential id: `(passkey public id, user id, user public id,
/// serialized passkey)`. A disabled user's passkey reads as `None`.
pub async fn find(
    writer: &mut SqliteConnection,
    credential_id: &[u8],
) -> Result<Option<(String, i64, String, String)>, sqlx::Error> {
    sqlx::query_as(
        "SELECT webauthn_credential.public_id, user.id, user.public_id, passkey FROM webauthn_credential
         JOIN user ON user.id = webauthn_credential.user_id
         WHERE credential_id = ? AND disabled_at IS NULL",
    )
    .bind(credential_id)
    .fetch_optional(writer)
    .await
}

/// Records a login with passkey `public_id`: its updated state and `last_used_at`. False when
/// the passkey is gone or its user disabled since it was read (`admin enroll`, removal).
pub async fn used(
    writer: &mut SqliteConnection,
    public_id: &str,
    user: i64,
    passkey: &str,
    now: i64,
) -> Result<bool, sqlx::Error> {
    sqlx::query(
        "UPDATE webauthn_credential SET passkey = ?, last_used_at = ?
         WHERE public_id = ? AND user_id = ? AND user_id IN (SELECT id FROM user WHERE disabled_at IS NULL)",
    )
    .bind(passkey)
    .bind(now)
    .bind(public_id)
    .bind(user)
    .execute(writer)
    .await
    .map(|done| done.rows_affected() == 1)
}

/// The serialized passkeys of `user`, for a step-up's allowed list or a registration's
/// excluded list (ADR 0143).
pub async fn of_user(
    connection: impl sqlx::SqliteExecutor<'_>,
    user: i64,
) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar("SELECT passkey FROM webauthn_credential WHERE user_id = ? ORDER BY id")
        .bind(user)
        .fetch_all(connection)
        .await
}

/// The outcome of a passkey removal (ADR 0143).
#[derive(Debug, PartialEq, Eq)]
pub enum Removal {
    Removed,
    /// Not a passkey of this user.
    NotFound,
    /// The user's only passkey: kept.
    Last,
}

/// Deletes passkey `public_id` of `user` unless it is the user's last one.
pub async fn remove(
    writer: &mut SqliteConnection,
    user: i64,
    public_id: &str,
) -> Result<Removal, sqlx::Error> {
    let deleted = sqlx::query(
        "DELETE FROM webauthn_credential WHERE public_id = ?1 AND user_id = ?2
         AND (SELECT count(*) FROM webauthn_credential WHERE user_id = ?2) > 1",
    )
    .bind(public_id)
    .bind(user)
    .execute(&mut *writer)
    .await?
    .rows_affected();
    if deleted == 1 {
        return Ok(Removal::Removed);
    }
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM webauthn_credential WHERE public_id = ? AND user_id = ?)",
    )
    .bind(public_id)
    .bind(user)
    .fetch_one(writer)
    .await?;
    Ok(if exists {
        Removal::Last
    } else {
        Removal::NotFound
    })
}
