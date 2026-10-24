use super::*;
use sqlx::Row;

const ROWS: [(&str, &str); 7] = [
    (
        "runner",
        "INSERT INTO runner (id, public_id, name, capabilities, version, connected) VALUES (?1, ?2, 'runner' || ?1, 'opaque capabilities', '0.1.0', 1)",
    ),
    (
        "run",
        "INSERT INTO run (id, public_id, repo_id, sha, trigger, event_id, triggered_by, state, created_at) VALUES (?1, ?2, ?1, '1111111111111111111111111111111111111111', 'push', ?1, 2, 'queued', 0)",
    ),
    (
        "job",
        "INSERT INTO job (id, public_id, run_id, name, requirements, replayable, max_attempts, state, created_at) VALUES (?1, ?2, ?1, 'check', 'opaque requirements', 1, 3, 'pending', 0)",
    ),
    (
        "job_attempt",
        "INSERT INTO job_attempt (id, public_id, job_id, number, runner_id, state, lease_expires_at, started_at) VALUES (?1, ?2, ?1, 1, ?1, 'running', 30000, 0)",
    ),
    (
        "step",
        "INSERT INTO step (id, attempt_id, idx, name, state) VALUES (?1, ?1, 0, 'flake check', 'running')",
    ),
    (
        "log_chunk",
        "INSERT INTO log_chunk (attempt_id, seq, step_idx, offset, length) VALUES (?1, 0, 0, 0, 10)",
    ),
    (
        "artifact",
        "INSERT INTO artifact (id, public_id, attempt_id, name, size, sha256, created_at, expires_at) VALUES (?1, ?2, ?1, 'report', 10, 'digest', 0, 1000)",
    ),
];

async fn populate(writer: &mut SqliteConnection) {
    sqlx::query("INSERT INTO user (id, public_id, name, kind, is_admin, source, created_at) VALUES (1, '00000000000000000000000001', 'owner', 'local', 0, 'config', 0), (2, '00000000000000000000000002', 'pusher', 'local', 0, 'runtime', 0);
        INSERT INTO repo (id, public_id, owner_id, name, visibility, ci_enabled, default_branch, description, created_at, config_key) VALUES (1, '00000000000000000000000001', 1, 'demo', 'public', 1, 'main', '', 0, 'demo'), (2, '00000000000000000000000002', 1, 'other', 'public', 1, 'main', '', 0, 'other')")
        .execute(&mut *writer).await.unwrap();
    for id in 1..=2_i64 {
        sqlx::query("INSERT INTO event (id, public_id, kind, version, occurred_at, principal_kind, correlation_id, payload) VALUES (?1, ?2, 'push', 1, 0, 'system', 'request-1', '{}')")
            .bind(id).bind(format!("{id:026}")).execute(&mut *writer).await.unwrap();
    }
    for (table, insert) in ROWS {
        for id in 1..=2_i64 {
            let query = sqlx::query(insert).bind(id);
            let query = if matches!(table, "step" | "log_chunk") {
                query
            } else {
                query.bind(format!("{id:026}"))
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
async fn ci_valid_rows_columns_nullability_and_indexes() {
    let dir = TempDir::new();
    let (mut writer, readers) = open(&dir.database()).await.unwrap();
    populate(&mut writer).await;
    for (table, expected) in [
        (
            "runner",
            "id public_id name capabilities version connected last_contact_at",
        ),
        (
            "run",
            "id public_id repo_id sha ref trigger event_id triggered_by state created_at started_at finished_at",
        ),
        (
            "job",
            "id public_id run_id name requirements replayable max_attempts state pending_reason created_at finished_at",
        ),
        (
            "job_attempt",
            "id public_id job_id number runner_id state lease_expires_at exit_code started_at finished_at",
        ),
        (
            "step",
            "id attempt_id idx name state exit_code started_at finished_at",
        ),
        ("log_chunk", "attempt_id seq step_idx offset length"),
        (
            "artifact",
            "id public_id attempt_id name size sha256 created_at expires_at",
        ),
    ] {
        let count: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
            .fetch_one(&readers)
            .await
            .unwrap();
        assert_eq!(count, 2, "{table}");
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
                && !matches!(
                    name.as_str(),
                    "last_contact_at"
                        | "ref"
                        | "event_id"
                        | "triggered_by"
                        | "pending_reason"
                        | "exit_code"
                        | "finished_at"
                )
                && (name != "started_at" || table == "job_attempt");
            assert_eq!(
                column.get::<i64, _>("notnull"),
                i64::from(required),
                "{table}.{name}"
            );
            let key = if table == "log_chunk" {
                "attempt_id"
            } else {
                "id"
            };
            if required {
                refuses(
                    &mut writer,
                    &format!("UPDATE {table} SET {name} = NULL WHERE {key} = 2"),
                    "1299",
                )
                .await;
            } else if name != "id" {
                sqlx::query(&format!("UPDATE {table} SET {name} = NULL WHERE {key} = 2"))
                    .execute(&mut writer)
                    .await
                    .unwrap();
            }
        }
    }
    for (index, expected) in [
        ("run_repo_id_id", "repo_id id"),
        ("job_state", "state"),
        (
            "job_attempt_state_lease_expires_at",
            "state lease_expires_at",
        ),
    ] {
        let columns = sqlx::query(&format!("PRAGMA index_info({index})"))
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
    }
    readers.close().await;
    writer.close().await.unwrap();
}

#[tokio::test]
async fn ci_checks_accept_all_states_and_refuse_invalid_values() {
    let dir = TempDir::new();
    let (mut writer, readers) = open(&dir.database()).await.unwrap();
    populate(&mut writer).await;
    for (table, states, invalid) in [
        (
            "run",
            "queued running succeeded failed cancelled",
            "pending assigned lost skipped",
        ),
        (
            "job",
            "pending assigned running succeeded failed cancelled lost",
            "queued skipped",
        ),
        (
            "job_attempt",
            "running succeeded failed cancelled lost",
            "queued pending assigned skipped",
        ),
        (
            "step",
            "running succeeded failed skipped",
            "queued pending assigned cancelled lost",
        ),
    ] {
        for state in states.split_whitespace() {
            sqlx::query(&format!("UPDATE {table} SET state = ? WHERE id = 2"))
                .bind(state)
                .execute(&mut writer)
                .await
                .unwrap();
        }
        for state in invalid.split_whitespace().chain(["", "unknown", "Running"]) {
            let error = sqlx::query(&format!("UPDATE {table} SET state = ? WHERE id = 2"))
                .bind(state)
                .execute(&mut writer)
                .await
                .unwrap_err();
            assert_eq!(
                error.as_database_error().unwrap().code().as_deref(),
                Some("275"),
                "{table}: {state}: {error}"
            );
        }
    }
    for sql in [
        "UPDATE runner SET name = 'Runner' WHERE id = 2",
        "UPDATE runner SET connected = -1 WHERE id = 2",
        "UPDATE runner SET connected = 2 WHERE id = 2",
        "UPDATE job SET replayable = -1 WHERE id = 2",
        "UPDATE job SET replayable = 2 WHERE id = 2",
        "UPDATE run SET trigger = 'scheduled' WHERE id = 2",
    ] {
        refuses(&mut writer, sql, "275").await;
    }
    sqlx::query("UPDATE run SET trigger = 'manual' WHERE id = 2; UPDATE runner SET connected = 0 WHERE id = 2; UPDATE job SET replayable = 0 WHERE id = 2").execute(&mut writer).await.unwrap();
    readers.close().await;
    writer.close().await.unwrap();
}

#[tokio::test]
async fn ci_unique_keys_include_event_and_per_attempt_log_sequence() {
    let dir = TempDir::new();
    let (mut writer, readers) = open(&dir.database()).await.unwrap();
    populate(&mut writer).await;
    for (table, _) in ROWS {
        if table == "log_chunk" {
            refuses(
                &mut writer,
                "INSERT INTO log_chunk VALUES (1, 0, 1, 10, 10)",
                "1555",
            )
            .await;
        } else {
            refuses(
                &mut writer,
                &format!("UPDATE {table} SET id = 1 WHERE id = 2"),
                "1555",
            )
            .await;
            if table != "step" {
                refuses(&mut writer, &format!("UPDATE {table} SET public_id = (SELECT public_id FROM {table} WHERE id = 1) WHERE id = 2"), "2067").await;
            }
        }
    }
    for sql in [
        "UPDATE runner SET name = 'runner1' WHERE id = 2",
        "UPDATE run SET event_id = 1 WHERE id = 2",
        "UPDATE job SET run_id = 1 WHERE id = 2",
        "UPDATE job_attempt SET job_id = 1 WHERE id = 2",
        "UPDATE step SET attempt_id = 1 WHERE id = 2",
        "UPDATE artifact SET attempt_id = 1 WHERE id = 2",
    ] {
        refuses(&mut writer, sql, "2067").await;
    }
    sqlx::query(
        "UPDATE run SET event_id = NULL, trigger = 'manual';
        UPDATE job SET run_id = 1, name = 'other' WHERE id = 2;
        UPDATE job_attempt SET job_id = 1, number = 2 WHERE id = 2;
        UPDATE step SET attempt_id = 1, idx = 1 WHERE id = 2;
        UPDATE artifact SET attempt_id = 1, name = 'other' WHERE id = 2;
        INSERT INTO log_chunk VALUES (1, 1, 0, 10, 10)",
    )
    .execute(&mut writer)
    .await
    .unwrap();
    readers.close().await;
    writer.close().await.unwrap();
}

#[tokio::test]
async fn ci_foreign_keys_refuse_missing_parents_and_protect_referenced_rows() {
    let dir = TempDir::new();
    let (mut writer, readers) = open(&dir.database()).await.unwrap();
    populate(&mut writer).await;
    for (table, column) in [
        ("run", "repo_id"),
        ("run", "event_id"),
        ("run", "triggered_by"),
        ("job", "run_id"),
        ("job_attempt", "job_id"),
        ("job_attempt", "runner_id"),
        ("step", "attempt_id"),
        ("log_chunk", "attempt_id"),
        ("artifact", "attempt_id"),
    ] {
        let key = if table == "log_chunk" {
            "attempt_id"
        } else {
            "id"
        };
        refuses(
            &mut writer,
            &format!("UPDATE {table} SET {column} = 999 WHERE {key} = 2"),
            "787",
        )
        .await;
    }
    refuses(&mut writer, "DELETE FROM runner WHERE id = 1", "1811").await;
    // The append-only trigger (migration 7) refuses before the run.event_id foreign key.
    refuses(&mut writer, "DELETE FROM event WHERE id = 1", "1811").await;
    refuses(&mut writer, "DELETE FROM user WHERE id = 2", "787").await;
    readers.close().await;
    writer.close().await.unwrap();
}

#[tokio::test]
async fn ci_owned_rows_cascade_from_each_parent_and_preserve_other_runs() {
    for parent in ["repo", "run", "job", "job_attempt"] {
        let dir = TempDir::new();
        let (mut writer, readers) = open(&dir.database()).await.unwrap();
        populate(&mut writer).await;
        sqlx::query(&format!("DELETE FROM {parent} WHERE id = 1"))
            .execute(&mut writer)
            .await
            .unwrap();
        let deleted_tables: &[&str] = match parent {
            "repo" | "run" => &["run", "job", "job_attempt", "step", "log_chunk", "artifact"],
            "job" => &["job", "job_attempt", "step", "log_chunk", "artifact"],
            "job_attempt" => &["job_attempt", "step", "log_chunk", "artifact"],
            _ => unreachable!(),
        };
        for table in deleted_tables {
            let key = if *table == "log_chunk" {
                "attempt_id"
            } else {
                "id"
            };
            let ids: Vec<i64> =
                sqlx::query_scalar(&format!("SELECT {key} FROM {table} ORDER BY {key}"))
                    .fetch_all(&readers)
                    .await
                    .unwrap();
            assert_eq!(ids, [2], "{parent} -> {table}");
        }
        for table in ["runner", "event", "user"] {
            let count: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
                .fetch_one(&readers)
                .await
                .unwrap();
            assert_eq!(count, 2, "{parent} -> {table}");
        }
        sqlx::query("DELETE FROM runner WHERE id = 1")
            .execute(&mut writer)
            .await
            .unwrap();
        readers.close().await;
        writer.close().await.unwrap();
    }
}

#[tokio::test]
async fn ci_public_ids_are_canonical_ulids() {
    let dir = TempDir::new();
    let (mut writer, readers) = open(&dir.database()).await.unwrap();
    populate(&mut writer).await;
    for table in ["runner", "run", "job", "job_attempt", "artifact"] {
        for invalid in [
            "".to_owned(),
            "0".repeat(25),
            "0".repeat(27),
            format!("8{}", "0".repeat(25)),
            format!("0{}", "a".repeat(25)),
            format!("{}\0hidden", "0".repeat(26)),
        ]
        .into_iter()
        .chain(['I', 'L', 'O', 'U', '!'].map(|c| format!("0{}{c}", "0".repeat(24))))
        {
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
