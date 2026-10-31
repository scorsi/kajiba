use super::users::new_public_id;
use kajiba_core::config::RunnerDeclaration;
use sqlx::{Connection, SqliteConnection};
use std::collections::BTreeMap;

/// No connection survives a restart, so at startup every row starts disconnected; a
/// reload keeps live connections (0141).
pub async fn disconnect_all(writer: &mut SqliteConnection) -> Result<(), String> {
    sqlx::query("UPDATE runner SET connected = 0 WHERE connected = 1")
        .execute(writer)
        .await
        .map(drop)
        .map_err(|err| format!("runner reconciliation: {err}"))
}

/// ADR 0026: make `runner` rows equal the declared runners, matching by name. A removed
/// runner is disabled (its row stays for its attempts) and a re-declared one re-enabled.
pub async fn reconcile_runners(
    writer: &mut SqliteConnection,
    declared: &BTreeMap<String, RunnerDeclaration>,
    now: i64,
) -> Result<(), String> {
    let sql = |err: sqlx::Error| format!("runner reconciliation: {err}");
    let mut tx = writer.begin().await.map_err(sql)?;
    let rows: BTreeMap<String, (i64, String, bool)> =
        sqlx::query_as::<_, (String, i64, String, bool)>(
            "SELECT name, id, capabilities, disabled_at IS NOT NULL FROM runner",
        )
        .fetch_all(&mut *tx)
        .await
        .map_err(sql)?
        .into_iter()
        .map(|(name, id, capabilities, disabled)| (name, (id, capabilities, disabled)))
        .collect();
    for (name, runner) in declared {
        // Opaque to the schema (0013); only this code reads it back.
        let capabilities = serde_json::to_string(&runner.capabilities)
            .map_err(|err| format!("runner reconciliation: {err}"))?;
        match rows.get(name) {
            None => {
                let public_id = new_public_id(&mut tx, now as u128).await.map_err(sql)?;
                // `version` is the announced one, unknown until the first connection.
                sqlx::query(
                    "INSERT INTO runner (public_id, name, capabilities, version, connected)
                     VALUES (?, ?, ?, '', 0)",
                )
                .bind(public_id)
                .bind(name)
                .bind(&capabilities)
                .execute(&mut *tx)
                .await
                .map_err(sql)?;
            }
            Some((id, current, disabled)) if *current != capabilities || *disabled => {
                sqlx::query("UPDATE runner SET capabilities = ?, disabled_at = NULL WHERE id = ?")
                    .bind(&capabilities)
                    .bind(id)
                    .execute(&mut *tx)
                    .await
                    .map_err(sql)?;
            }
            Some(_) => {}
        }
    }
    for (name, (id, _, disabled)) in &rows {
        if !disabled && !declared.contains_key(name) {
            sqlx::query("UPDATE runner SET disabled_at = ? WHERE id = ?")
                .bind(now)
                .bind(id)
                .execute(&mut *tx)
                .await
                .map_err(sql)?;
        }
    }
    tx.commit().await.map_err(sql)
}
