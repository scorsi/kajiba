use super::users::new_public_id;
use kajiba_core::config::{Scope, UserConfig, parse_ssh_key};
use sqlx::{Connection, SqliteConnection};
use std::{
    collections::BTreeMap,
    time::{SystemTime, UNIX_EPOCH},
};

pub async fn reconcile_credentials(
    writer: &mut SqliteConnection,
    declared: &BTreeMap<String, UserConfig>,
) -> Result<(), String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|err| format!("credential reconciliation: {err}"))?
        .as_millis();
    let mut keys = BTreeMap::new();
    let mut tokens = BTreeMap::new();
    for (name, user) in declared {
        for (usage, lines) in [("auth", &user.ssh_keys), ("signing", &user.signing_keys)] {
            for line in lines {
                let mut key = parse_ssh_key(line).map_err(|err| {
                    format!("credential reconciliation: users.{name}.{usage}: {err}")
                })?;
                // Store the canonical form only: the comment never reaches authorized_keys.
                key.set_comment("");
                let canonical = key.to_openssh().map_err(|err| {
                    format!("credential reconciliation: users.{name}.{usage}: {err}")
                })?;
                keys.insert(
                    (
                        name.clone(),
                        usage.to_owned(),
                        key.fingerprint(Default::default()).to_string(),
                    ),
                    (key.algorithm().to_string(), canonical),
                );
            }
        }
        for (token_name, token) in &user.tokens {
            let hash = (0..token.sha256.len()).step_by(2)
                .map(|i| token.sha256.get(i..i + 2).and_then(|s| u8::from_str_radix(s, 16).ok()))
                .collect::<Option<Vec<_>>>()
                .filter(|hash| hash.len() == 32)
                .ok_or_else(|| format!("credential reconciliation: users.{name}.tokens.{token_name}.sha256: invalid hash"))?;
            let scopes = token
                .scopes
                .iter()
                .map(|scope| match scope {
                    Scope::RepoRead => "repo:read",
                    Scope::RepoWrite => "repo:write",
                    Scope::CiTrigger => "ci:trigger",
                    Scope::Admin => "admin",
                })
                .collect::<Vec<_>>()
                .join(" ");
            let expires = token.expires_at_millis().map_err(|err| {
                format!(
                    "credential reconciliation: users.{name}.tokens.{token_name}.expires: {err}"
                )
            })?;
            tokens.insert((name.clone(), token_name.clone()), (hash, scopes, expires));
        }
    }
    let result = async {
        let mut tx = writer.begin().await?;
        let users: BTreeMap<String, i64> = sqlx::query_as::<_, (String, i64)>("SELECT name, id FROM user")
            .fetch_all(&mut *tx).await?.into_iter().collect();
        let existing_keys = sqlx::query_as::<_, (i64, String, String, String)>(
            "SELECT ssh_key.id, user.name, usage, fingerprint FROM ssh_key
             JOIN user ON user.id = ssh_key.user_id WHERE ssh_key.source = 'config'",
        ).fetch_all(&mut *tx).await?;
        for (id, name, usage, fingerprint) in existing_keys {
            if !keys.contains_key(&(name, usage, fingerprint)) {
                sqlx::query("DELETE FROM ssh_key WHERE id = ? AND source = 'config'")
                    .bind(id).execute(&mut *tx).await?;
            }
        }
        let existing_tokens = sqlx::query_as::<_, (i64, String, String, Vec<u8>)>(
            "SELECT api_token.id, user.name, api_token.name, token_hash FROM api_token
             JOIN user ON user.id = api_token.user_id WHERE api_token.source = 'config'",
        ).fetch_all(&mut *tx).await?;
        for (id, name, token_name, hash) in existing_tokens {
            // A rotated hash is a new credential; discard its old usage and revocation state.
            if tokens.get(&(name, token_name)).is_none_or(|token| token.0 != hash) {
                sqlx::query("DELETE FROM api_token WHERE id = ? AND source = 'config'")
                    .bind(id).execute(&mut *tx).await?;
            }
        }
        for ((name, usage, fingerprint), (key_type, line)) in keys {
            let user_id = users.get(&name).ok_or(sqlx::Error::RowNotFound)?;
            let id: Option<i64> = sqlx::query_scalar(
                "SELECT id FROM ssh_key WHERE user_id = ? AND usage = ? AND fingerprint = ? AND source = 'config'",
            ).bind(user_id).bind(&usage).bind(&fingerprint).fetch_optional(&mut *tx).await?;
            if let Some(id) = id {
                sqlx::query("UPDATE ssh_key SET key_type = ?, public_key = ? WHERE id = ? AND (key_type != ? OR public_key != ?)")
                    .bind(&key_type).bind(&line).bind(id).bind(&key_type).bind(&line)
                    .execute(&mut *tx).await?;
            } else {
                let public_id = new_public_id(&mut tx, now).await?;
                sqlx::query("INSERT INTO ssh_key (public_id, user_id, usage, key_type, public_key, fingerprint, source, created_at) VALUES (?, ?, ?, ?, ?, ?, 'config', ?)")
                    .bind(public_id).bind(user_id).bind(usage).bind(key_type).bind(line)
                    .bind(fingerprint).bind(now as i64).execute(&mut *tx).await?;
            }
        }
        for ((name, token_name), (hash, scopes, expires)) in tokens {
            let user_id = users.get(&name).ok_or(sqlx::Error::RowNotFound)?;
            let id: Option<i64> = sqlx::query_scalar(
                "SELECT id FROM api_token WHERE user_id = ? AND name = ? AND source = 'config'",
            ).bind(user_id).bind(&token_name).fetch_optional(&mut *tx).await?;
            if let Some(id) = id {
                sqlx::query("UPDATE api_token SET scopes = ?, expires_at = ? WHERE id = ? AND (scopes != ? OR expires_at IS NOT ?)")
                    .bind(&scopes).bind(expires).bind(id).bind(&scopes).bind(expires)
                    .execute(&mut *tx).await?;
            } else {
                let public_id = new_public_id(&mut tx, now).await?;
                sqlx::query("INSERT INTO api_token (public_id, user_id, name, token_hash, scopes, source, created_at, expires_at) VALUES (?, ?, ?, ?, ?, 'config', ?, ?)")
                    .bind(public_id).bind(user_id).bind(token_name).bind(hash).bind(scopes)
                    .bind(now as i64).bind(expires).execute(&mut *tx).await?;
            }
        }
        tx.commit().await
    }.await;
    result.map_err(|err: sqlx::Error| format!("credential reconciliation: {err}"))
}
