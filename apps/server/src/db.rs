use sqlx::{
    Connection, SqliteConnection, SqlitePool,
    migrate::{Migrate, MigrateError, Migrator},
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
};
use std::{path::Path, time::Duration};

pub mod credentials;
// Production handlers and the dispatch lifecycle follow in later tasks.
#[cfg_attr(not(test), allow(dead_code))]
pub mod events;
pub mod users;

static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

// The caller must hold the state lock for the lifetime of both handles.
pub async fn open(path: &Path) -> Result<(SqliteConnection, SqlitePool), String> {
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .foreign_keys(true)
        .synchronous(SqliteSynchronous::Full)
        .busy_timeout(Duration::from_millis(5000));
    let result = async {
        let mut writer = SqliteConnection::connect_with(&options).await?;
        migrate(&mut writer, &MIGRATOR).await?;
        let readers = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(options.create_if_missing(false).read_only(true))
            .await?;
        Ok::<_, MigrateError>((writer, readers))
    }
    .await;
    result.map_err(|err| match err {
        MigrateError::VersionMissing(version) => format!(
            "database {}: applied migration {version} is unknown to this binary; refusing a newer schema (no downgrade)",
            path.display()
        ),
        err => format!("database {}: {err}", path.display()),
    })
}

async fn migrate(
    connection: &mut SqliteConnection,
    migrator: &Migrator,
) -> Result<(), MigrateError> {
    connection.ensure_migrations_table().await?;
    let applied = connection.list_applied_migrations().await?;
    migrator.run_direct(&mut *connection).await?;
    for migration in migrator.iter() {
        if !applied.iter().any(|row| row.version == migration.version) {
            tracing::info!(event = "migration-applied", version = migration.version);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        path::PathBuf,
        sync::atomic::{AtomicUsize, Ordering},
    };

    mod ci;
    mod credentials;
    mod dispatch;
    mod events;
    mod identity;
    mod repositories;
    mod users;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "kajiba-db-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn database(&self) -> PathBuf {
            self.0.join("kajiba.sqlite")
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[tokio::test]
    async fn creates_database_and_reopen_preserves_migration_history() {
        let dir = TempDir::new();
        let (mut writer, readers) = open(&dir.database()).await.unwrap();
        assert!(dir.database().is_file());
        let history = sqlx::query_as::<_, (i64, String, bool, Vec<u8>, i64)>(
            "SELECT version, CAST(installed_on AS TEXT), success, checksum, execution_time FROM _sqlx_migrations ORDER BY version",
        )
        .fetch_all(&mut writer)
        .await
        .unwrap();
        assert_eq!(history.len(), 5);
        assert_eq!(history[4].0, 5);
        assert!(history[4].2);
        assert_eq!(
            history[4].3,
            MIGRATOR.iter().nth(4).unwrap().checksum.as_ref()
        );
        assert_eq!(history[3].0, 4);
        assert!(history[3].2);
        assert_eq!(
            history[3].3,
            MIGRATOR.iter().nth(3).unwrap().checksum.as_ref()
        );
        assert_eq!(history[2].0, 3);
        assert!(history[2].2);
        assert_eq!(
            history[2].3,
            MIGRATOR.iter().nth(2).unwrap().checksum.as_ref()
        );
        assert_eq!(history[1].0, 2);
        assert!(history[1].2);
        assert_eq!(
            history[1].3,
            MIGRATOR.iter().nth(1).unwrap().checksum.as_ref()
        );
        assert_eq!(history[0].0, 1);
        assert!(history[0].2);
        assert_eq!(
            history[0].3,
            MIGRATOR.iter().next().unwrap().checksum.as_ref()
        );
        readers.close().await;
        writer.close().await.unwrap();

        let (mut writer, readers) = open(&dir.database()).await.unwrap();
        let reopened = sqlx::query_as::<_, (i64, String, bool, Vec<u8>, i64)>(
            "SELECT version, CAST(installed_on AS TEXT), success, checksum, execution_time FROM _sqlx_migrations ORDER BY version",
        )
        .fetch_all(&mut writer)
        .await
        .unwrap();
        assert_eq!(reopened, history);
        let tables: Vec<String> =
            sqlx::query_scalar("SELECT name FROM sqlite_schema WHERE type = 'table' ORDER BY name")
                .fetch_all(&mut writer)
                .await
                .unwrap();
        assert_eq!(
            tables,
            [
                "_sqlx_migrations",
                "api_token",
                "artifact",
                "enrollment_token",
                "event",
                "handler_checkpoint",
                "job",
                "job_attempt",
                "log_chunk",
                "mirror",
                "pending_push",
                "repo",
                "repo_permission",
                "repo_redirect",
                "run",
                "runner",
                "session",
                "ssh_key",
                "step",
                "user",
                "webauthn_credential"
            ]
        );
        readers.close().await;
        writer.close().await.unwrap();
    }

    async fn assert_policy(connection: &mut SqliteConnection) {
        let journal: String = sqlx::query_scalar("PRAGMA journal_mode")
            .fetch_one(&mut *connection)
            .await
            .unwrap();
        assert_eq!(journal, "wal");
        for (pragma, expected) in [
            ("PRAGMA foreign_keys", 1_i64),
            ("PRAGMA synchronous", 2),
            ("PRAGMA busy_timeout", 5000),
        ] {
            let value: i64 = sqlx::query_scalar(pragma)
                .fetch_one(&mut *connection)
                .await
                .unwrap();
            assert_eq!(value, expected, "{pragma}");
        }
    }

    #[tokio::test]
    async fn applies_policy_to_writer_and_every_read_connection() {
        let dir = TempDir::new();
        let (mut writer, readers) = open(&dir.database()).await.unwrap();
        assert_policy(&mut writer).await;
        let mut connections = Vec::new();
        for _ in 0..4 {
            let mut connection = readers.acquire().await.unwrap();
            assert_policy(&mut connection).await;
            assert!(
                sqlx::query("CREATE TABLE forbidden (id INTEGER)")
                    .execute(&mut *connection)
                    .await
                    .is_err()
            );
            connections.push(connection);
        }
        drop(connections);
        readers.close().await;
        writer.close().await.unwrap();
    }

    #[tokio::test]
    async fn refuses_unknown_newer_migration() {
        let dir = TempDir::new();
        let (mut writer, readers) = open(&dir.database()).await.unwrap();
        sqlx::query("INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) VALUES (999, 'newer binary', TRUE, X'00', 0)")
            .execute(&mut writer).await.unwrap();
        readers.close().await;
        writer.close().await.unwrap();
        let err = open(&dir.database()).await.unwrap_err();
        assert!(err.contains("999"), "{err}");
        assert!(err.contains("unknown to this binary"), "{err}");
        assert!(err.contains(&dir.database().display().to_string()), "{err}");
    }

    #[tokio::test]
    async fn failed_migration_rolls_back_schema_data_and_history() {
        static BROKEN: Migrator = sqlx::migrate!("./tests/fixtures/broken-migrations");
        let dir = TempDir::new();
        let (mut writer, readers) = open(&dir.database()).await.unwrap();
        sqlx::query("CREATE TABLE preserved (value TEXT NOT NULL); INSERT INTO preserved VALUES ('original')")
            .execute(&mut writer).await.unwrap();
        // Include the already applied migrations so failure reaches the broken SQL.
        let migrator = Migrator {
            migrations: MIGRATOR
                .iter()
                .chain(BROKEN.iter())
                .cloned()
                .collect::<Vec<_>>()
                .into(),
            ..BROKEN
        };
        let err = migrate(&mut writer, &migrator).await.unwrap_err();
        assert!(err.to_string().contains("THIS"), "{err}");
        readers.close().await;
        writer.close().await.unwrap();
        let (mut writer, readers) = open(&dir.database()).await.unwrap();
        let value: String = sqlx::query_scalar("SELECT value FROM preserved")
            .fetch_one(&mut writer)
            .await
            .unwrap();
        assert_eq!(value, "original");
        let probe: i64 =
            sqlx::query_scalar("SELECT count(*) FROM sqlite_schema WHERE name = 'migration_probe'")
                .fetch_one(&mut writer)
                .await
                .unwrap();
        assert_eq!(probe, 0);
        let versions: Vec<i64> =
            sqlx::query_scalar("SELECT version FROM _sqlx_migrations ORDER BY version")
                .fetch_all(&mut writer)
                .await
                .unwrap();
        assert_eq!(versions, [1, 2, 3, 4, 5]);
        readers.close().await;
        writer.close().await.unwrap();
    }
}
