use kajiba_core::config::UserConfig;
use sqlx::{Connection, SqliteConnection};
use std::{
    collections::BTreeMap,
    time::{SystemTime, UNIX_EPOCH},
};

pub async fn reconcile_users(
    writer: &mut SqliteConnection,
    declared: &BTreeMap<String, UserConfig>,
) -> Result<(), String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|err| format!("user reconciliation: {err}"))?
        .as_millis();
    let result = async {
        let mut tx = writer.begin().await?;
        let existing = sqlx::query_as::<_, (i64, String, String)>(
            "SELECT id, name, source FROM user",
        )
        .fetch_all(&mut *tx)
        .await?;
        for (name, user) in declared {
            if let Some((id, _, _)) = existing.iter().find(|(_, stored, _)| stored == name) {
                sqlx::query(
                    "UPDATE user SET kind = 'local', is_admin = ?, source = 'config', disabled_at = NULL
                     WHERE id = ? AND (kind != 'local' OR is_admin != ? OR source != 'config' OR disabled_at IS NOT NULL)",
                )
                .bind(user.is_admin)
                .bind(id)
                .bind(user.is_admin)
                .execute(&mut *tx)
                .await?;
            } else {
                let random: Vec<u8> = sqlx::query_scalar("SELECT randomblob(10)")
                    .fetch_one(&mut *tx)
                    .await?;
                let mut bytes = [0_u8; 16];
                bytes[6..].copy_from_slice(&random);
                let value = (now << 80) | u128::from_be_bytes(bytes);
                let public_id: String = (0..26)
                    .rev()
                    .map(|shift| {
                        b"0123456789ABCDEFGHJKMNPQRSTVWXYZ"[((value >> (shift * 5)) & 31) as usize] as char
                    })
                    .collect();
                sqlx::query(
                    "INSERT INTO user (public_id, name, kind, is_admin, source, created_at)
                     VALUES (?, ?, 'local', ?, 'config', ?)",
                )
                .bind(public_id)
                .bind(name)
                .bind(user.is_admin)
                .bind(now as i64)
                .execute(&mut *tx)
                .await?;
            }
        }
        for (id, name, source) in existing {
            if source != "config" || declared.contains_key(&name) {
                continue;
            }
            sqlx::query("UPDATE user SET disabled_at = ? WHERE id = ? AND disabled_at IS NULL")
                .bind(now as i64)
                .bind(id)
                .execute(&mut *tx)
                .await?;
            sqlx::query("DELETE FROM session WHERE user_id = ?")
                .bind(id)
                .execute(&mut *tx)
                .await?;
            sqlx::query(
                "UPDATE api_token SET revoked_at = ? WHERE user_id = ? AND source = 'runtime' AND revoked_at IS NULL",
            )
            .bind(now as i64)
            .bind(id)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await
    }
    .await;
    result.map_err(|err: sqlx::Error| format!("user reconciliation: {err}"))
}
