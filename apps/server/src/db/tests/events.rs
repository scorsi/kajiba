use super::*;
use crate::db::events::{NewEvent, append_event, events_after};
use sqlx::Row;

pub(super) fn event(public_id: &str) -> NewEvent {
    NewEvent {
        public_id: public_id.into(),
        kind: "push".into(),
        version: 1,
        occurred_at: 1234,
        principal_kind: "user".into(),
        principal_id: Some(1),
        repo_id: Some(1),
        correlation_id: "request-1".into(),
        payload: r#"{"ref":"refs/heads/main","old":"0000000000000000000000000000000000000000","new":"1111111111111111111111111111111111111111"}"#.into(),
    }
}

async fn populate(writer: &mut SqliteConnection) {
    sqlx::query("INSERT INTO user (id, public_id, name, kind, is_admin, source, created_at) VALUES (1, '00000000000000000000000001', 'owner', 'local', 0, 'config', 0), (2, '00000000000000000000000002', 'pusher', 'local', 0, 'runtime', 0);
        INSERT INTO repo (id, public_id, owner_id, name, visibility, ci_enabled, default_branch, description, created_at, config_key) VALUES (1, '00000000000000000000000001', 1, 'demo', 'public', 0, 'main', '', 0, 'demo'), (2, '00000000000000000000000002', 1, 'other', 'public', 0, 'main', '', 0, 'other')")
        .execute(writer).await.unwrap();
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
async fn events_append_and_read_strictly_after_id_in_sequence_order() {
    let dir = TempDir::new();
    let (mut writer, readers) = open(&dir.database()).await.unwrap();
    let first = event("00000000000000000000000002");
    let mut second = event("00000000000000000000000001");
    second.occurred_at = 0;
    second.kind = "run-finished".into();
    second.principal_kind = "system".into();
    second.principal_id = None;
    second.repo_id = None;
    let first_id = append_event(&mut writer, &first).await.unwrap();
    let second_id = append_event(&mut writer, &second).await.unwrap();
    assert!(second_id > first_id);
    let rows = events_after(&readers, 0).await.unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].id, first_id);
    assert_eq!(rows[0].data, first);
    assert_eq!(rows[1].id, second_id);
    assert_eq!(rows[1].data, second);
    assert_eq!(events_after(&readers, first_id).await.unwrap(), rows[1..]);
    assert!(events_after(&readers, second_id).await.unwrap().is_empty());
    let mut transaction = writer.begin().await.unwrap();
    append_event(&mut transaction, &event("00000000000000000000000003"))
        .await
        .unwrap();
    transaction.rollback().await.unwrap();
    assert_eq!(events_after(&readers, 0).await.unwrap(), rows);
    // Append-only in the database too (migration 7): 1811 is SQLITE_CONSTRAINT_TRIGGER.
    for sql in [
        "UPDATE event SET payload = '{}'",
        "UPDATE event SET id = id + 10",
        "DELETE FROM event",
    ] {
        refuses(&mut writer, sql, "1811").await;
    }
    assert_eq!(events_after(&readers, 0).await.unwrap(), rows);
    readers.close().await;
    writer.close().await.unwrap();
}

#[tokio::test]
async fn pending_push_admission_is_unique_per_repository_and_preserves_ref_order() {
    let dir = TempDir::new();
    let (mut writer, readers) = open(&dir.database()).await.unwrap();
    populate(&mut writer).await;
    let updates = r#"[{"ref":"refs/heads/z","old":"0000000000000000000000000000000000000000","new":"1111111111111111111111111111111111111111"},{"ref":"refs/heads/a","old":"2222222222222222222222222222222222222222","new":"0000000000000000000000000000000000000000"}]"#;
    let insert = "INSERT INTO pending_push (repo_id, principal_id, correlation_id, updates, created_at) VALUES (?, 2, 'request-1', ?, 0)";
    sqlx::query(insert)
        .bind(1_i64)
        .bind(updates)
        .execute(&mut writer)
        .await
        .unwrap();
    let error = sqlx::query(insert)
        .bind(1_i64)
        .bind(updates)
        .execute(&mut writer)
        .await
        .unwrap_err();
    assert_eq!(
        error.as_database_error().unwrap().code().as_deref(),
        Some("2067")
    );
    sqlx::query(insert)
        .bind(2_i64)
        .bind(updates)
        .execute(&mut writer)
        .await
        .unwrap();
    let stored: Vec<String> = sqlx::query_scalar("SELECT updates FROM pending_push ORDER BY id")
        .fetch_all(&readers)
        .await
        .unwrap();
    assert_eq!(stored, [updates, updates]);
    let refs: serde_json::Value = serde_json::from_str(&stored[0]).unwrap();
    assert_eq!(refs[0]["ref"], "refs/heads/z");
    assert_eq!(refs[1]["ref"], "refs/heads/a");
    readers.close().await;
    writer.close().await.unwrap();
}

#[tokio::test]
async fn events_schema_columns_nullability_keys_and_index() {
    let dir = TempDir::new();
    let (mut writer, readers) = open(&dir.database()).await.unwrap();
    populate(&mut writer).await;
    append_event(&mut writer, &event("00000000000000000000000001"))
        .await
        .unwrap();
    sqlx::query("INSERT INTO handler_checkpoint (handler, last_event_id, updated_at) VALUES ('ci', 0, 0); INSERT INTO pending_push (repo_id, principal_id, correlation_id, updates, created_at) VALUES (1, 2, 'request-1', '[]', 0)").execute(&mut writer).await.unwrap();
    for (table, expected) in [
        (
            "event",
            "id public_id kind version occurred_at principal_kind principal_id repo_id correlation_id payload",
        ),
        ("handler_checkpoint", "handler last_event_id updated_at"),
        (
            "pending_push",
            "id repo_id principal_id correlation_id updates created_at git_session_id git_generation",
        ),
    ] {
        let columns = sqlx::query(&format!("PRAGMA table_info({table})"))
            .fetch_all(&mut writer)
            .await
            .unwrap();
        assert_eq!(
            columns
                .iter()
                .map(|c| c.get::<String, _>("name"))
                .collect::<Vec<_>>(),
            expected.split_whitespace().collect::<Vec<_>>()
        );
        for column in columns {
            let name: String = column.get("name");
            let required = name != "id"
                && !(table == "event" && matches!(name.as_str(), "principal_id" | "repo_id"))
                && !(table == "pending_push"
                    && matches!(
                        name.as_str(),
                        "principal_id" | "git_session_id" | "git_generation"
                    ));
            assert_eq!(
                column.get::<i64, _>("notnull"),
                i64::from(required),
                "{table}.{name}"
            );
            if required {
                refuses(
                    &mut writer,
                    &format!("UPDATE {table} SET {name} = NULL"),
                    // The append-only trigger refuses every event UPDATE first; notnull is
                    // asserted above.
                    if table == "event" { "1811" } else { "1299" },
                )
                .await;
            }
        }
    }
    for (sql, code) in [
        ("INSERT INTO event SELECT * FROM event", "1555"),
        (
            "INSERT INTO event (public_id, kind, version, occurred_at, principal_kind, correlation_id, payload) SELECT public_id, kind, version, occurred_at, principal_kind, correlation_id, payload FROM event",
            "2067",
        ),
        ("INSERT INTO handler_checkpoint VALUES ('ci', 1, 0)", "1555"),
        (
            "INSERT INTO pending_push SELECT * FROM pending_push",
            "1555",
        ),
    ] {
        refuses(&mut writer, sql, code).await;
    }
    sqlx::query("INSERT INTO handler_checkpoint VALUES ('mirror', 999, 0)")
        .execute(&mut writer)
        .await
        .unwrap();
    let columns = sqlx::query("PRAGMA index_info(event_repo_id_id)")
        .fetch_all(&mut writer)
        .await
        .unwrap();
    assert_eq!(
        columns
            .iter()
            .map(|c| c.get::<String, _>("name"))
            .collect::<Vec<_>>(),
        ["repo_id", "id"]
    );
    readers.close().await;
    writer.close().await.unwrap();
}

#[tokio::test]
async fn event_checks_refuse_invalid_principals_and_public_ids() {
    let dir = TempDir::new();
    let (mut writer, readers) = open(&dir.database()).await.unwrap();
    for invalid in ["", "service", "User"] {
        let mut input = event("00000000000000000000000001");
        input.principal_kind = invalid.into();
        let error = append_event(&mut writer, &input).await.unwrap_err();
        assert_eq!(
            error.as_database_error().unwrap().code().as_deref(),
            Some("275")
        );
    }
    for invalid in [
        "".to_owned(),
        "0".repeat(25),
        "0".repeat(27),
        format!("8{}", "0".repeat(25)),
        format!("0{}", "a".repeat(25)),
        format!("{}\0hidden", "0".repeat(26)),
    ]
    .into_iter()
    .chain(['I', 'L', 'O', 'U', '!'].map(|c| format!("0{}{}", "0".repeat(24), c)))
    {
        let error = append_event(&mut writer, &event(&invalid))
            .await
            .unwrap_err();
        assert_eq!(
            error.as_database_error().unwrap().code().as_deref(),
            Some("275"),
            "{invalid:?}"
        );
    }
    for (index, kind) in ["user", "runner", "system"].into_iter().enumerate() {
        let mut input = event(&format!("{index:026}"));
        input.principal_kind = kind.into();
        append_event(&mut writer, &input).await.unwrap();
    }
    append_event(&mut writer, &event("7ZZZZZZZZZZZZZZZZZZZZZZZZZ"))
        .await
        .unwrap();
    readers.close().await;
    writer.close().await.unwrap();
}

#[tokio::test]
async fn pending_foreign_keys_protect_principals_and_cascade_repositories_but_journal_survives() {
    let dir = TempDir::new();
    let (mut writer, readers) = open(&dir.database()).await.unwrap();
    populate(&mut writer).await;
    let mut input = event("00000000000000000000000001");
    input.principal_id = Some(2);
    append_event(&mut writer, &input).await.unwrap();
    sqlx::query("INSERT INTO pending_push (repo_id, principal_id, correlation_id, updates, created_at) VALUES (1, 2, 'request-1', '[]', 0); INSERT INTO handler_checkpoint VALUES ('ci', 1, 0)").execute(&mut writer).await.unwrap();
    for sql in [
        "UPDATE pending_push SET repo_id = 999",
        "UPDATE pending_push SET principal_id = 999",
        "DELETE FROM user WHERE id = 2",
    ] {
        refuses(&mut writer, sql, "787").await;
    }
    sqlx::query("DELETE FROM repo WHERE id = 1; DELETE FROM user WHERE id = 2")
        .execute(&mut writer)
        .await
        .unwrap();
    assert_eq!(events_after(&readers, 0).await.unwrap()[0].data, input);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM pending_push")
        .fetch_one(&readers)
        .await
        .unwrap();
    assert_eq!(count, 0);
    let checkpoint: i64 =
        sqlx::query_scalar("SELECT last_event_id FROM handler_checkpoint WHERE handler = 'ci'")
            .fetch_one(&readers)
            .await
            .unwrap();
    assert_eq!(checkpoint, 1);
    for table in ["event", "handler_checkpoint"] {
        assert!(
            sqlx::query(&format!("PRAGMA foreign_key_list({table})"))
                .fetch_all(&readers)
                .await
                .unwrap()
                .is_empty()
        );
    }
    readers.close().await;
    writer.close().await.unwrap();
}

#[test]
fn production_code_never_updates_or_deletes_events() {
    fn visit(dir: &std::path::Path, sources: &mut Vec<(PathBuf, String)>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                // Tests rewrite journal rows on purpose to probe replay and lag.
                if path.file_name().unwrap() != "tests" {
                    visit(&path, sources);
                }
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                sources.push((path.clone(), fs::read_to_string(&path).unwrap()));
            }
        }
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut sources = vec![(
        root.join("db.rs"),
        fs::read_to_string(root.join("db.rs")).unwrap(),
    )];
    visit(&root.join("db"), &mut sources);
    assert!(sources.len() > 3, "{sources:?}");
    for (path, source) in sources {
        // Compare SQL keywords case-insensitively across line breaks.
        let words: Vec<String> = source
            .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .filter(|word| !word.is_empty())
            .map(str::to_ascii_lowercase)
            .collect();
        for window in words.windows(3) {
            let refused = matches!(
                [window[0].as_str(), window[1].as_str(), window[2].as_str()],
                ["update", "event", _] | ["delete", "from", "event"] | ["replace", "into", "event"]
            );
            assert!(!refused, "{}: {window:?}", path.display());
        }
    }
}

#[tokio::test]
async fn pending_push_accepts_a_system_principal() {
    let dir = TempDir::new();
    let (mut writer, readers) = open(&dir.database()).await.unwrap();
    populate(&mut writer).await;
    // `admin import` records its refs with no user: the principal is `system` (0026).
    sqlx::query("INSERT INTO pending_push (repo_id, principal_id, correlation_id, updates, created_at) VALUES (1, NULL, 'import-1', '[]', 0)")
        .execute(&mut writer)
        .await
        .unwrap();
    let principal: Option<i64> =
        sqlx::query_scalar("SELECT principal_id FROM pending_push WHERE repo_id = 1")
            .fetch_one(&mut writer)
            .await
            .unwrap();
    assert_eq!(principal, None);
    // The rebuilt table keeps its user foreign key and its repository cascade.
    refuses(
        &mut writer,
        "UPDATE pending_push SET principal_id = 999",
        "787",
    )
    .await;
    sqlx::query("DELETE FROM repo WHERE id = 1")
        .execute(&mut writer)
        .await
        .unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM pending_push")
        .fetch_one(&mut writer)
        .await
        .unwrap();
    assert_eq!(count, 0);
    readers.close().await;
    writer.close().await.unwrap();
}
