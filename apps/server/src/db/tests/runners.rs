use super::users::changes;
use super::*;
use crate::db::runners::reconcile_runners;
use kajiba_core::config::{RunnerDeclaration, ServerConfig};
use std::collections::BTreeMap;

fn declared(runners: &str) -> BTreeMap<String, RunnerDeclaration> {
    ServerConfig::parse(&format!(
        "[http]\npublic_url='https://example.org'\n{runners}"
    ))
    .unwrap()
    .runners
}

fn runner(name: &str, capabilities: &str) -> String {
    format!(
        "[runners.{name}]\ntoken_file='/run/secrets/{name}'\ncapabilities={{ {capabilities} }}\n"
    )
}

type RunnerRow = (String, String, String, String, bool, Option<i64>);

async fn rows(writer: &mut SqliteConnection) -> Vec<RunnerRow> {
    sqlx::query_as(
        "SELECT public_id, name, capabilities, version, connected, disabled_at FROM runner ORDER BY name",
    )
    .fetch_all(writer)
    .await
    .unwrap()
}

#[tokio::test]
async fn runner_rows_follow_the_declaration() {
    let dir = TempDir::new();
    let (mut writer, readers) = open(&dir.database()).await.unwrap();
    let linux = "arch='aarch64', os='linux'";
    let config = declared(&format!(
        "{}{}",
        runner("build-1", linux),
        runner(
            "Build-2",
            "arch='x86_64', os='darwin', tags=['kvm'], max_jobs=2"
        )
    ));
    reconcile_runners(&mut writer, &config, 1000).await.unwrap();
    let added = rows(&mut writer).await;
    assert_eq!(added.len(), 2);
    assert_eq!(added[0].1, "build-1");
    assert_eq!(
        added[0].2,
        r#"{"arch":"aarch64","os":"linux","backends":["nix"],"tags":[],"max_jobs":1}"#
    );
    assert_eq!(added[1].1, "build-2");
    assert_eq!(
        added[1].2,
        r#"{"arch":"x86_64","os":"darwin","backends":["nix"],"tags":["kvm"],"max_jobs":2}"#
    );
    for row in &added {
        assert_eq!(row.0.len(), 26);
        assert_eq!((row.3.as_str(), row.4, row.5), ("", false, None));
    }

    // A rerun without change writes nothing.
    let before = changes(&mut writer).await;
    reconcile_runners(&mut writer, &config, 2000).await.unwrap();
    assert_eq!(changes(&mut writer).await, before);
    assert_eq!(rows(&mut writer).await, added);

    // Changed capabilities keep the identity; runtime columns are untouched.
    sqlx::query("UPDATE runner SET version = '0.1.0', connected = 1, hostname = 'host'")
        .execute(&mut writer)
        .await
        .unwrap();
    let changed = declared(&format!(
        "{}{}",
        runner(
            "build-1",
            "arch='aarch64', os='linux', backends=['nix','process']"
        ),
        runner(
            "build-2",
            "arch='x86_64', os='darwin', tags=['kvm'], max_jobs=2"
        )
    ));
    reconcile_runners(&mut writer, &changed, 3000)
        .await
        .unwrap();
    let after = rows(&mut writer).await;
    assert_eq!(after[0].0, added[0].0);
    assert!(after[0].2.contains(r#""backends":["nix","process"]"#));
    assert_eq!(after[1].2, added[1].2);
    for row in &after {
        // Every row starts disconnected; the announced version stays until the next hello.
        assert_eq!((row.3.as_str(), row.4, row.5), ("0.1.0", false, None));
    }

    // Removal disables and keeps the row; a second pass keeps the first timestamp.
    let removed = declared(&runner(
        "build-2",
        "arch='x86_64', os='darwin', tags=['kvm'], max_jobs=2",
    ));
    reconcile_runners(&mut writer, &removed, 4000)
        .await
        .unwrap();
    reconcile_runners(&mut writer, &removed, 5000)
        .await
        .unwrap();
    let after = rows(&mut writer).await;
    assert_eq!(after.len(), 2);
    assert_eq!((after[0].1.as_str(), after[0].5), ("build-1", Some(4000)));
    assert_eq!(after[1].5, None);

    // Re-declaring re-enables the same row.
    reconcile_runners(&mut writer, &config, 6000).await.unwrap();
    let after = rows(&mut writer).await;
    assert_eq!(after[0].0, added[0].0);
    assert_eq!(
        (after[0].2.as_str(), after[0].5),
        (added[0].2.as_str(), None)
    );
    let hostnames: Vec<Option<String>> =
        sqlx::query_scalar("SELECT hostname FROM runner ORDER BY name")
            .fetch_all(&mut writer)
            .await
            .unwrap();
    assert_eq!(hostnames, [Some("host".into()), Some("host".into())]);
    readers.close().await;
    writer.close().await.unwrap();
}

#[tokio::test]
async fn migration_10_keeps_existing_runner_rows() {
    let dir = TempDir::new();
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(dir.database())
        .create_if_missing(true);
    let mut writer = SqliteConnection::connect_with(&options).await.unwrap();
    let before = Migrator {
        migrations: MIGRATOR
            .iter()
            .filter(|migration| migration.version < 10)
            .cloned()
            .collect::<Vec<_>>()
            .into(),
        ..MIGRATOR
    };
    migrate(&mut writer, &before).await.unwrap();
    sqlx::query("INSERT INTO runner (public_id, name, capabilities, version, connected, last_contact_at) VALUES ('00000000000000000000000001', 'build-1', 'opaque', '0.1.0', 1, 42)")
        .execute(&mut writer)
        .await
        .unwrap();
    migrate(&mut writer, &MIGRATOR).await.unwrap();
    let row: (String, String, String, bool, Option<i64>, Option<i64>, Option<String>) =
        sqlx::query_as("SELECT name, capabilities, version, connected, last_contact_at, disabled_at, hostname FROM runner")
            .fetch_one(&mut writer)
            .await
            .unwrap();
    assert_eq!(
        row,
        (
            "build-1".into(),
            "opaque".into(),
            "0.1.0".into(),
            true,
            Some(42),
            None,
            None
        )
    );
    writer.close().await.unwrap();
}
