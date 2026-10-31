use super::*;
use sqlx::Row;

const ROWS: [(&str, &str); 4] = [
    (
        "repo",
        "INSERT INTO repo (id, public_id, owner_id, name, visibility, ci_mode, default_branch, description, created_at, config_key) VALUES (?1, ?2, ?1, 'demo', 'public', 'off', 'main', '', 0, 'key' || ?1)",
    ),
    (
        "repo_redirect",
        "INSERT INTO repo_redirect (id, owner_name, name, repo_id, created_at) VALUES (?1, 'owner' || ?1, 'old', ?1, 0)",
    ),
    (
        "repo_permission",
        "INSERT INTO repo_permission (user_id, repo_id, role) VALUES (?1, ?1, 'read')",
    ),
    (
        "mirror",
        "INSERT INTO mirror (id, public_id, repo_id, remote_url, credential, enabled, state, redo, attempts) VALUES (?1, ?2, ?1, 'https://example.org/mirror.git', 'github', 1, 'idle', 0, 0)",
    ),
];

async fn populate(writer: &mut SqliteConnection) {
    for id in 1..=2_i64 {
        sqlx::query("INSERT INTO user (id, public_id, name, kind, is_admin, source, created_at) VALUES (?1, ?2, 'owner' || ?1, 'local', 0, 'config', 0)")
            .bind(id).bind(format!("{id:026}")).execute(&mut *writer).await.unwrap();
    }
    for (table, insert) in ROWS {
        for id in 1..=2_i64 {
            let query = sqlx::query(insert).bind(id);
            let query = if matches!(table, "repo" | "mirror") {
                query.bind(format!("{id:026}"))
            } else {
                query
            };
            query.execute(&mut *writer).await.unwrap();
        }
    }
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
async fn repositories_valid_rows_nullability_and_mirror_queue_index() {
    let dir = TempDir::new();
    let (mut writer, readers) = open(&dir.database()).await.unwrap();
    populate(&mut writer).await;
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
            "repo" => {
                "id public_id owner_id name visibility default_branch description created_at config_key archived_at ci_mode"
            }
            "repo_redirect" => "id owner_name name repo_id created_at",
            "repo_permission" => "user_id repo_id role",
            "mirror" => {
                "id public_id repo_id remote_url credential enabled state redo attempts next_attempt_at last_success_at last_error last_error_at"
            }
            _ => unreachable!(),
        };
        assert_eq!(
            columns
                .iter()
                .map(|column| column.get::<String, _>("name"))
                .collect::<Vec<_>>(),
            expected.split_whitespace().collect::<Vec<_>>()
        );
        let predicate = if table == "repo_permission" {
            "user_id = 2"
        } else {
            "id = 2"
        };
        for column in columns {
            let name: String = column.get("name");
            let required = name != "id"
                && !matches!(
                    name.as_str(),
                    "next_attempt_at"
                        | "last_success_at"
                        | "last_error"
                        | "last_error_at"
                        | "archived_at"
                );
            assert_eq!(
                column.get::<i64, _>("notnull"),
                i64::from(required),
                "{table}.{name}"
            );
            if required {
                refuses(
                    &mut writer,
                    &format!("UPDATE {table} SET {name} = NULL WHERE {predicate}"),
                    "1299",
                )
                .await;
            }
        }
    }
    sqlx::query("UPDATE mirror SET next_attempt_at = 1000, last_success_at = 0, last_error = 'push failed', last_error_at = 500 WHERE id = 2")
        .execute(&mut writer).await.unwrap();
    let columns = sqlx::query("PRAGMA index_info(mirror_state_next_attempt_at)")
        .fetch_all(&mut writer)
        .await
        .unwrap();
    assert_eq!(
        columns
            .iter()
            .map(|column| column.get::<String, _>("name"))
            .collect::<Vec<_>>(),
        ["state", "next_attempt_at"]
    );
    readers.close().await;
    writer.close().await.unwrap();
}

#[tokio::test]
async fn repositories_unique_keys_are_scoped_to_their_pairs() {
    let dir = TempDir::new();
    let (mut writer, readers) = open(&dir.database()).await.unwrap();
    populate(&mut writer).await;
    for table in ["repo", "repo_redirect", "mirror"] {
        refuses(
            &mut writer,
            &format!("UPDATE {table} SET id = 1 WHERE id = 2"),
            "1555",
        )
        .await;
    }
    for table in ["repo", "mirror"] {
        refuses(&mut writer, &format!("UPDATE {table} SET public_id = (SELECT public_id FROM {table} WHERE id = 1) WHERE id = 2"), "2067").await;
    }
    for (sql, code) in [
        ("UPDATE repo SET owner_id = 1 WHERE id = 2", "2067"),
        (
            "UPDATE repo_redirect SET owner_name = 'owner1' WHERE id = 2",
            "2067",
        ),
        (
            "INSERT INTO repo_permission (user_id, repo_id, role) VALUES (1, 1, 'write')",
            "1555",
        ),
        ("UPDATE mirror SET repo_id = 1 WHERE id = 2", "2067"),
    ] {
        refuses(&mut writer, sql, code).await;
    }
    for sql in [
        "UPDATE repo SET name = 'other', owner_id = 1 WHERE id = 2",
        "UPDATE repo_redirect SET name = 'other', owner_name = 'owner1' WHERE id = 2",
        "INSERT INTO repo_permission (user_id, repo_id, role) VALUES (1, 2, 'write'), (2, 1, 'admin')",
        "UPDATE mirror SET remote_url = 'https://example.org/other.git', repo_id = 1 WHERE id = 2",
    ] {
        sqlx::query(sql).execute(&mut writer).await.unwrap();
    }
    readers.close().await;
    writer.close().await.unwrap();
}

#[tokio::test]
async fn repositories_checks_refuse_invalid_values() {
    let dir = TempDir::new();
    let (mut writer, readers) = open(&dir.database()).await.unwrap();
    populate(&mut writer).await;
    for sql in [
        "UPDATE repo SET name = 'Demo' WHERE id = 2",
        "UPDATE repo_redirect SET owner_name = 'Owner2' WHERE id = 2",
        "UPDATE repo_redirect SET name = 'Old' WHERE id = 2",
        "UPDATE repo SET visibility = 'private' WHERE id = 2",
        "UPDATE repo SET ci_mode = 'on' WHERE id = 2",
        "UPDATE repo SET ci_mode = 1 WHERE id = 2",
        "UPDATE repo_permission SET role = 'owner' WHERE user_id = 2",
        "UPDATE mirror SET enabled = -1 WHERE id = 2",
        "UPDATE mirror SET enabled = 2 WHERE id = 2",
        "UPDATE mirror SET redo = -1 WHERE id = 2",
        "UPDATE mirror SET redo = 2 WHERE id = 2",
        "UPDATE mirror SET state = 'failed' WHERE id = 2",
    ] {
        refuses(&mut writer, sql, "275").await;
    }
    for sql in [
        "UPDATE repo SET ci_mode = 'manual' WHERE id = 2",
        "UPDATE repo SET ci_mode = 'push' WHERE id = 2",
        "UPDATE repo_permission SET role = 'write' WHERE user_id = 2",
        "UPDATE repo_permission SET role = 'admin' WHERE user_id = 2",
        "UPDATE mirror SET enabled = 0, redo = 1, state = 'queued', attempts = 1 WHERE id = 2",
        "UPDATE mirror SET state = 'running' WHERE id = 2",
    ] {
        sqlx::query(sql).execute(&mut writer).await.unwrap();
    }
    readers.close().await;
    writer.close().await.unwrap();
}

#[tokio::test]
async fn repositories_public_ids_are_canonical_ulids() {
    let dir = TempDir::new();
    let (mut writer, readers) = open(&dir.database()).await.unwrap();
    populate(&mut writer).await;
    for table in ["repo", "mirror"] {
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
            let error = sqlx::query(&format!("UPDATE {table} SET public_id = ? WHERE id = 2"))
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
async fn repositories_foreign_keys_restrict_owners_and_cascade_owned_rows() {
    let dir = TempDir::new();
    let (mut writer, readers) = open(&dir.database()).await.unwrap();
    populate(&mut writer).await;
    for sql in [
        "UPDATE repo SET owner_id = 999 WHERE id = 2",
        "UPDATE repo_redirect SET repo_id = 999 WHERE id = 2",
        "UPDATE repo_permission SET user_id = 999 WHERE user_id = 2",
        "UPDATE repo_permission SET repo_id = 999 WHERE user_id = 2",
        "UPDATE mirror SET repo_id = 999 WHERE id = 2",
    ] {
        refuses(&mut writer, sql, "787").await;
    }
    refuses(&mut writer, "DELETE FROM user WHERE id = 1", "1811").await;
    sqlx::query("DELETE FROM repo WHERE id = 1")
        .execute(&mut writer)
        .await
        .unwrap();
    for (table, _) in ROWS {
        let key = if table == "repo_permission" {
            "repo_id"
        } else {
            "id"
        };
        let rows = sqlx::query(&format!("SELECT {key} FROM {table}"))
            .fetch_all(&mut writer)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1, "{table}");
        assert_eq!(rows[0].get::<i64, _>(key), 2, "{table}");
    }
    sqlx::query("INSERT INTO repo_permission (user_id, repo_id, role) VALUES (1, 2, 'read'); DELETE FROM user WHERE id = 1")
        .execute(&mut writer).await.unwrap();
    let rows = sqlx::query("SELECT user_id FROM repo_permission")
        .fetch_all(&mut writer)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get::<i64, _>("user_id"), 2);
    readers.close().await;
    writer.close().await.unwrap();
}

#[tokio::test]
async fn declared_keys_are_unique_and_archived_rows_keep_their_names() {
    let dir = TempDir::new();
    let (mut writer, readers) = open(&dir.database()).await.unwrap();
    populate(&mut writer).await;
    let index = sqlx::query("PRAGMA index_list(repo)")
        .fetch_all(&mut writer)
        .await
        .unwrap()
        .into_iter()
        .find(|index| index.get::<String, _>("name") == "repo_config_key")
        .unwrap();
    assert_eq!(index.get::<i64, _>("unique"), 1);
    refuses(
        &mut writer,
        "UPDATE repo SET config_key = 'key1' WHERE id = 2",
        "2067",
    )
    .await;
    // An archived row still holds its owner and name (0026): no other row may take them.
    sqlx::query("UPDATE repo SET archived_at = 1000 WHERE id = 1")
        .execute(&mut writer)
        .await
        .unwrap();
    refuses(
        &mut writer,
        "UPDATE repo SET owner_id = 1 WHERE id = 2",
        "2067",
    )
    .await;
    refuses(
        &mut writer,
        "INSERT INTO repo (id, public_id, owner_id, name, visibility, ci_mode, default_branch, description, created_at, config_key) VALUES (3, '00000000000000000000000003', 1, 'demo', 'public', 'off', 'main', '', 0, 'key3')",
        "2067",
    )
    .await;
    // Unarchival clears the column on the same row.
    sqlx::query("UPDATE repo SET archived_at = NULL WHERE id = 1")
        .execute(&mut writer)
        .await
        .unwrap();
    readers.close().await;
    writer.close().await.unwrap();
}
