use sqlx::{
    Connection, SqliteConnection, SqlitePool,
    migrate::{Migrate, MigrateError, Migrator},
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
};
use std::{path::Path, time::Duration};

pub mod credentials;
pub mod enrollment;
pub mod passkeys;
pub mod permissions;
// Production handlers follow in later milestones.
#[cfg_attr(not(test), allow(dead_code))]
pub mod events;
pub mod pushes;
pub mod repos;
pub mod runners;
pub mod sessions;
pub mod users;

/// A bearer secret: 32 bytes from the kernel CSPRNG (SQLite's randomblob is only pseudo-random).
pub fn secret_bytes() -> std::io::Result<[u8; 32]> {
    use std::io::Read;
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes)
}

/// `now + ttl` in epoch milliseconds with ADR 0029's checked conversion and addition;
/// `None` when not representable, never a wrapped value.
pub fn expiry(now: i64, ttl: Duration) -> Option<i64> {
    i64::try_from(ttl.as_millis())
        .ok()
        .and_then(|ttl| now.checked_add(ttl))
}

static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

/// The single writer, shared by the event dispatcher and request handlers. Lock it
/// only for one transaction: append events in the caller's transaction, then
/// commit through `EventBus::commit`.
pub type Writer = std::sync::Arc<tokio::sync::Mutex<SqliteConnection>>;

/// Users and their declared credentials change together or not at all. The caller
/// logs the returned revocations once its own transaction, if any, has committed.
pub async fn reconcile(
    writer: &mut SqliteConnection,
    declared: &std::collections::BTreeMap<String, kajiba_core::config::UserConfig>,
) -> Result<credentials::Revoked, String> {
    let mut transaction = writer
        .begin()
        .await
        .map_err(|err| format!("reconciliation: {err}"))?;
    // Each step's own transaction nests as a savepoint inside this one.
    users::reconcile_users(&mut transaction, declared).await?;
    let revoked = credentials::reconcile_credentials(&mut transaction, declared).await?;
    transaction
        .commit()
        .await
        .map_err(|err| format!("reconciliation: {err}"))?;
    Ok(revoked)
}

/// ADR 0026: users, credentials, repositories and runners are reconciled in one transaction;
/// a refusal anywhere leaves nothing committed.
pub async fn reconcile_declared(
    writer: &mut SqliteConnection,
    config: &kajiba_core::config::ServerConfig,
    git: &mut crate::transport::git_side::Repositories,
) -> Result<(), String> {
    let mut transaction = writer
        .begin()
        .await
        .map_err(|err| format!("reconciliation: {err}"))?;
    let revoked = reconcile(&mut transaction, &config.users).await?;
    let applied =
        repos::reconcile_repos(&mut transaction, &config.git.state_dir, git, &config.repos).await?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|err| format!("runner reconciliation: {err}"))?
        .as_millis() as i64;
    runners::reconcile_runners(&mut transaction, &config.runners, now).await?;
    transaction
        .commit()
        .await
        .map_err(|err| format!("reconciliation: {err}"))?;
    revoked.committed();
    applied.committed();
    Ok(())
}

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
    if let Some(version) = connection.dirty_version().await? {
        return Err(MigrateError::Dirty(version));
    }
    let applied = connection.list_applied_migrations().await?;
    for row in &applied {
        match migrator
            .iter()
            .find(|migration| migration.version == row.version)
        {
            None => return Err(MigrateError::VersionMissing(row.version)),
            Some(migration) if migration.checksum != row.checksum => {
                return Err(MigrateError::VersionMismatch(row.version));
            }
            Some(_) => {}
        }
    }
    // Log each migration once it commits, so a later failure leaves earlier ones logged.
    for migration in migrator.iter() {
        if !applied.iter().any(|row| row.version == migration.version) {
            connection.apply(migration).await?;
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
    mod enrollment;
    mod events;
    mod identity;
    mod import;
    mod permissions;
    mod pushes;
    mod repos;
    mod repositories;
    mod runners;
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
        assert_eq!(history.len(), MIGRATOR.iter().count());
        assert_eq!(history.len(), 10);
        for ((row, migration), version) in history.iter().zip(MIGRATOR.iter()).zip(1..) {
            assert_eq!(row.0, version);
            assert!(row.2);
            assert_eq!(row.3, migration.checksum.as_ref());
        }
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
        assert_eq!(versions, [1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
        readers.close().await;
        writer.close().await.unwrap();
    }

    #[tokio::test]
    async fn logs_each_migration_as_it_commits_before_a_later_failure() {
        static BROKEN: Migrator = sqlx::migrate!("./tests/fixtures/broken-migrations");
        const CHILD: &str = "KAJIBA_MIGRATION_LOG_TEST";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "db::tests::logs_each_migration_as_it_commits_before_a_later_failure",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .env_remove("RUST_LOG")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let versions: Vec<i64> = String::from_utf8(output.stderr)
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
                .inspect(|line| assert_eq!(line["event"], "migration-applied"))
                .map(|line| line["version"].as_i64().unwrap())
                .collect();
            assert_eq!(versions, [1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
            return;
        }
        kajiba_core::log::init("info").unwrap();
        let dir = TempDir::new();
        let options = SqliteConnectOptions::new()
            .filename(dir.database())
            .create_if_missing(true);
        let mut writer = SqliteConnection::connect_with(&options).await.unwrap();
        sqlx::query("CREATE TABLE preserved (value TEXT NOT NULL)")
            .execute(&mut writer)
            .await
            .unwrap();
        let migrator = Migrator {
            migrations: MIGRATOR
                .iter()
                .chain(BROKEN.iter())
                .cloned()
                .collect::<Vec<_>>()
                .into(),
            ..BROKEN
        };
        assert!(migrate(&mut writer, &migrator).await.is_err());
        let versions: Vec<i64> =
            sqlx::query_scalar("SELECT version FROM _sqlx_migrations ORDER BY version")
                .fetch_all(&mut writer)
                .await
                .unwrap();
        assert_eq!(versions, [1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
        writer.close().await.unwrap();
    }

    #[tokio::test]
    async fn user_names_must_be_stored_lowercase() {
        let dir = TempDir::new();
        let (mut writer, readers) = open(&dir.database()).await.unwrap();
        let insert = |name: &'static str| {
            sqlx::query("INSERT INTO user (public_id, name, kind, is_admin, source, created_at) VALUES ('01ARZ3NDEKTSV4RRFFQ69G5FAV', ?, 'local', 0, 'config', 0)")
                .bind(name)
        };
        let err = insert("Alice").execute(&mut writer).await.unwrap_err();
        assert!(err.to_string().contains("CHECK constraint failed"), "{err}");
        insert("alice").execute(&mut writer).await.unwrap();
        let err = sqlx::query("UPDATE user SET name = 'ALICE'")
            .execute(&mut writer)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("CHECK constraint failed"), "{err}");
        readers.close().await;
        writer.close().await.unwrap();
    }
}
