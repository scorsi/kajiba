//! Passkeys (`webauthn_credential`, ADR 0014): written by the enrollment ceremony.
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
