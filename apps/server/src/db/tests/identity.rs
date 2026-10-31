use super::*;
use sqlx::Row;

const ROWS: [(&str, &str); 6] = [
    (
        "user",
        "INSERT INTO user (id, public_id, name, kind, is_admin, source, created_at) VALUES (?1, ?2, 'user' || ?1, 'local', 0, 'config', 0)",
    ),
    (
        "ssh_key",
        "INSERT INTO ssh_key (id, public_id, user_id, usage, key_type, public_key, fingerprint, source, created_at) VALUES (?1, ?2, ?1, 'auth', 'ssh-ed25519', 'opaque public key', 'SHA256:key' || ?1, 'config', 0)",
    ),
    (
        "api_token",
        "INSERT INTO api_token (id, public_id, user_id, name, token_hash, scopes, source, created_at) VALUES (?1, ?2, ?1, 'cli', CAST(printf('%032d', ?1) AS BLOB), 'repo:read', 'runtime', 0)",
    ),
    (
        "webauthn_credential",
        "INSERT INTO webauthn_credential (id, public_id, user_id, credential_id, passkey, name, created_at) VALUES (?1, ?2, ?1, CAST('credential' || ?1 AS BLOB), '{}', 'passkey', 0)",
    ),
    (
        "session",
        "INSERT INTO session (id, user_id, token_hash, created_at, expires_at, last_seen_at) VALUES (?1, ?1, CAST(printf('%032d', ?1) AS BLOB), 0, 1000, 0)",
    ),
    (
        "enrollment_token",
        "INSERT INTO enrollment_token (id, user_id, token_hash, created_at, expires_at) VALUES (?1, ?1, CAST(printf('%032d', ?1) AS BLOB), 0, 1000)",
    ),
];

fn ulid() -> String {
    use std::sync::atomic::AtomicU64;
    use std::time::{SystemTime, UNIX_EPOCH};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis();
    let mut value = (millis << 80) | u128::from(NEXT.fetch_add(1, Ordering::Relaxed));
    let mut bytes = [b'0'; 26];
    for byte in bytes.iter_mut().rev() {
        *byte = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ"[(value & 31) as usize];
        value >>= 5;
    }
    String::from_utf8(bytes.to_vec()).unwrap()
}

async fn populate(writer: &mut SqliteConnection) {
    for (table, insert) in ROWS {
        for id in 1..=2_i64 {
            let query = sqlx::query(insert).bind(id);
            let query = if has_public_id(table) {
                query.bind(ulid())
            } else {
                query
            };
            query.execute(&mut *writer).await.unwrap();
        }
    }
}

fn has_public_id(table: &str) -> bool {
    !matches!(table, "session" | "enrollment_token")
}

async fn refuses(writer: &mut SqliteConnection, sql: &str, code: &str) {
    let error = sqlx::query(sql).execute(writer).await.expect_err(sql);
    assert_eq!(
        error.as_database_error().unwrap().code().as_deref(),
        Some(code),
        "{sql}: {error}"
    );
}

#[tokio::test]
async fn identity_valid_rows_nullability_and_session_expiry_index() {
    let dir = TempDir::new();
    let (mut writer, readers) = open(&dir.database()).await.unwrap();
    populate(&mut writer).await;
    // Exercise the other accepted enum values and the external-user implication.
    sqlx::query("UPDATE user SET kind = 'external', source = 'runtime' WHERE id = 2; UPDATE user SET is_admin = 1 WHERE id = 1; UPDATE ssh_key SET usage = 'signing', source = 'runtime' WHERE id = 2; UPDATE api_token SET source = 'config' WHERE id = 2")
        .execute(&mut writer).await.unwrap();
    for (table, _) in ROWS {
        let rows = sqlx::query(&format!("SELECT * FROM {table}"))
            .fetch_all(&mut writer)
            .await
            .unwrap();
        assert_eq!(rows.len(), 2, "{table}");
        let columns = sqlx::query(&format!("PRAGMA table_info({table})"))
            .fetch_all(&mut writer)
            .await
            .unwrap();
        let expected = match table {
            "user" => "id public_id name kind is_admin source created_at disabled_at",
            "ssh_key" => {
                "id public_id user_id usage key_type public_key fingerprint source created_at last_used_at"
            }
            "api_token" => {
                "id public_id user_id name token_hash scopes source created_at expires_at last_used_at revoked_at"
            }
            "webauthn_credential" => {
                "id public_id user_id credential_id passkey name created_at last_used_at"
            }
            "session" => "id user_id token_hash created_at expires_at last_seen_at step_up_at",
            "enrollment_token" => "id user_id token_hash created_at expires_at consumed_at",
            _ => unreachable!(),
        };
        assert_eq!(
            columns
                .iter()
                .map(|column| column.get::<String, _>("name"))
                .collect::<Vec<_>>(),
            expected.split_whitespace().collect::<Vec<_>>()
        );
        for column in columns {
            let name: String = column.get("name");
            let required = name != "id"
                && !matches!(
                    name.as_str(),
                    "disabled_at" | "last_used_at" | "revoked_at" | "consumed_at" | "step_up_at"
                )
                && !(table == "api_token" && name == "expires_at");
            assert_eq!(
                column.get::<i64, _>("notnull"),
                i64::from(required),
                "{table}.{name}"
            );
            if required {
                refuses(
                    &mut writer,
                    &format!("UPDATE {table} SET {name} = NULL WHERE id = 2"),
                    "1299",
                )
                .await;
            }
        }
    }
    let columns = sqlx::query("PRAGMA index_info(session_expires_at)")
        .fetch_all(&mut writer)
        .await
        .unwrap();
    assert_eq!(columns.len(), 1);
    assert_eq!(columns[0].get::<String, _>("name"), "expires_at");
    readers.close().await;
    writer.close().await.unwrap();
}

#[tokio::test]
async fn identity_unique_keys_refuse_duplicates() {
    let dir = TempDir::new();
    let (mut writer, readers) = open(&dir.database()).await.unwrap();
    populate(&mut writer).await;
    for (table, _) in ROWS {
        refuses(
            &mut writer,
            &format!("UPDATE {table} SET id = 1 WHERE id = 2"),
            "1555",
        )
        .await;
        if has_public_id(table) {
            refuses(&mut writer, &format!("UPDATE {table} SET public_id = (SELECT public_id FROM {table} WHERE id = 1) WHERE id = 2"), "2067").await;
        }
    }
    for sql in [
        "UPDATE user SET name = 'user1' WHERE id = 2",
        "UPDATE ssh_key SET fingerprint = 'SHA256:key1' WHERE id = 2",
        "UPDATE api_token SET token_hash = (SELECT token_hash FROM api_token WHERE id = 1) WHERE id = 2",
        "UPDATE api_token SET user_id = 1 WHERE id = 2",
        "UPDATE webauthn_credential SET credential_id = (SELECT credential_id FROM webauthn_credential WHERE id = 1) WHERE id = 2",
        "UPDATE session SET token_hash = (SELECT token_hash FROM session WHERE id = 1) WHERE id = 2",
        "UPDATE enrollment_token SET token_hash = (SELECT token_hash FROM enrollment_token WHERE id = 1) WHERE id = 2",
        "UPDATE enrollment_token SET user_id = 1 WHERE id = 2",
    ] {
        refuses(&mut writer, sql, "2067").await;
    }
    // A fingerprint may be shared across usages, including across users.
    sqlx::query("UPDATE ssh_key SET fingerprint = 'SHA256:key1', usage = 'signing' WHERE id = 2")
        .execute(&mut writer)
        .await
        .unwrap();
    refuses(
        &mut writer,
        "UPDATE ssh_key SET usage = 'auth' WHERE id = 2",
        "2067",
    )
    .await;
    readers.close().await;
    writer.close().await.unwrap();
}

#[tokio::test]
async fn identity_checks_refuse_invalid_values() {
    let dir = TempDir::new();
    let (mut writer, readers) = open(&dir.database()).await.unwrap();
    populate(&mut writer).await;
    for sql in [
        "UPDATE user SET kind = 'unknown' WHERE id = 2",
        "UPDATE user SET is_admin = -1 WHERE id = 2",
        "UPDATE user SET is_admin = 2 WHERE id = 2",
        "UPDATE user SET kind = 'external', is_admin = 1 WHERE id = 2",
        "UPDATE user SET source = 'unknown' WHERE id = 2",
        "UPDATE ssh_key SET usage = 'unknown' WHERE id = 2",
        "UPDATE ssh_key SET source = 'unknown' WHERE id = 2",
        "UPDATE api_token SET source = 'unknown' WHERE id = 2",
    ] {
        refuses(&mut writer, sql, "275").await;
    }
    for (table, _) in ROWS.into_iter().filter(|(table, _)| has_public_id(table)) {
        for invalid in [
            "".to_owned(),
            "0".repeat(25),
            "0".repeat(27),
            format!("8{}", "0".repeat(25)),
            format!("0{}", "a".repeat(25)),
            format!("0{}", "I".repeat(25)),
            format!("0{}", "L".repeat(25)),
            format!("0{}", "O".repeat(25)),
            format!("0{}", "U".repeat(25)),
            format!("0{}!", "0".repeat(24)),
            format!("{}\0hidden", "0".repeat(26)),
        ] {
            let sql = format!("UPDATE {table} SET public_id = ? WHERE id = 2");
            let error = sqlx::query(&sql)
                .bind(&invalid)
                .execute(&mut writer)
                .await
                .unwrap_err();
            assert_eq!(
                error.as_database_error().unwrap().code().as_deref(),
                Some("275"),
                "{table}: {invalid:?}: {error}"
            );
        }
        sqlx::query(&format!(
            "UPDATE {table} SET public_id = '7ZZZZZZZZZZZZZZZZZZZZZZZZZ' WHERE id = 2"
        ))
        .execute(&mut writer)
        .await
        .unwrap();
    }
    readers.close().await;
    writer.close().await.unwrap();
}

#[tokio::test]
async fn identity_foreign_keys_refuse_orphans_and_cascade_only_owned_rows() {
    let dir = TempDir::new();
    let (mut writer, readers) = open(&dir.database()).await.unwrap();
    populate(&mut writer).await;
    for (table, _) in &ROWS[1..] {
        refuses(
            &mut writer,
            &format!("UPDATE {table} SET user_id = 999 WHERE id = 2"),
            "787",
        )
        .await;
    }
    sqlx::query("DELETE FROM user WHERE id = 1")
        .execute(&mut writer)
        .await
        .unwrap();
    for (table, _) in ROWS {
        let rows = sqlx::query(&format!("SELECT id FROM {table}"))
            .fetch_all(&mut writer)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1, "{table}");
        assert_eq!(rows[0].get::<i64, _>("id"), 2, "{table}");
    }
    readers.close().await;
    writer.close().await.unwrap();
}
