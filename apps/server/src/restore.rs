//! Offline, two-tree restore (0170); the input verifier never migrates or writes.
use crate::{dump, state::StateLock};
use kajiba_core::{config::ServerConfig, state::real_dir};
use sqlx::{Connection, SqliteConnection, sqlite::SqliteConnectOptions};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt, chown},
    path::{Path, PathBuf},
    process::Command,
};

type Result<T> = std::result::Result<T, String>;
type Identity = (u32, u32);

pub(crate) async fn validate(root: &Path) -> Result<dump::Manifest> {
    dump::verify(root).await
}

pub(crate) async fn run(root: &Path, config: &ServerConfig) -> Result<()> {
    if !cfg!(target_os = "linux") || kajiba_core::local::effective_uid() != 0 {
        return Err("offline restore requires root on Linux with systemd".into());
    }
    stopped()?;
    let server_owner =
        kajiba_core::local::named_user("kajiba-server").map_err(|e| e.to_string())?;
    let git_owner = kajiba_core::local::named_user(&config.git.user).map_err(|e| e.to_string())?;
    for (path, owner) in [
        (&config.state_dir, server_owner),
        (&config.git.state_dir, git_owner),
    ] {
        let metadata = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
        if !metadata.is_dir()
            || (metadata.uid(), metadata.gid()) != owner
            || metadata.permissions().mode() & 0o7777 != 0o750
        {
            return Err(format!(
                "{}: provision a real directory with service ownership and mode 0750",
                path.display()
            ));
        }
    }
    install(
        root,
        &config.state_dir,
        &config.git.state_dir,
        server_owner,
        git_owner,
        &mut |_| Ok(()),
    )
    .await
}

fn stopped() -> Result<()> {
    for unit in [
        "kajiba-server.service",
        "kajiba-git.service",
        "kajiba-server.socket",
        "kajiba-server-runner.socket",
        "kajiba-git.socket",
        "kajiba-git-ssh.socket",
    ] {
        let output = Command::new("systemctl")
            .args([
                "show",
                "--no-pager",
                "--all",
                "--property=LoadState,ActiveState,Job",
                unit,
            ])
            .output()
            .map_err(|e| format!("check stopped units: {e}"))?;
        let state = String::from_utf8(output.stdout).map_err(|e| e.to_string())?;
        let optional = unit == "kajiba-server-runner.socket";
        if (!output.status.success() && !optional) || !stopped_state(&state, optional) {
            return Err(format!(
                "stop both daemons and all activating sockets before restore: {unit}"
            ));
        }
    }
    Ok(())
}

fn stopped_state(state: &str, optional: bool) -> bool {
    let property = |key| state.lines().find_map(|line| line.strip_prefix(key));
    matches!(property("ActiveState="), Some("inactive" | "failed"))
        && property("Job=") == Some("")
        && (property("LoadState=") == Some("loaded")
            || optional && property("LoadState=") == Some("not-found"))
}

/// Record only paths created by this invocation, including successfully switched trees.
struct Created(Vec<PathBuf>);
impl Created {
    fn rollback(&mut self) -> Result<()> {
        let mut errors = Vec::new();
        for path in self.0.iter().rev() {
            if let Err(e) = fs::remove_dir_all(path) {
                if e.kind() != std::io::ErrorKind::NotFound {
                    errors.push(format!("{}: {e}", path.display()));
                }
            }
        }
        if errors.is_empty() {
            self.0.clear();
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }
}
impl Drop for Created {
    fn drop(&mut self) {
        let _ = self.rollback();
    }
}

fn empty(path: &Path, lock: &str, stage: Option<&Path>) -> Result<()> {
    real_dir(path).map_err(|e| e.to_string())?;
    for entry in fs::read_dir(path).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        if entry.file_name() == lock && entry.file_type().map_err(|e| e.to_string())?.is_file()
            || stage == Some(entry.path().as_path())
        {
            continue;
        }
        return Err(format!(
            "{}: restore destination is not empty",
            path.display()
        ));
    }
    Ok(())
}

fn lock(path: &Path, owner: Identity) -> Result<StateLock> {
    let lock = StateLock::acquire(path)?;
    chown(path, Some(owner.0), Some(owner.1)).map_err(|e| e.to_string())?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|e| e.to_string())?;
    Ok(lock)
}

pub(super) async fn install(
    root: &Path,
    server: &Path,
    git: &Path,
    server_owner: Identity,
    git_owner: Identity,
    checkpoint: &mut impl FnMut(&str) -> Result<()>,
) -> Result<()> {
    for path in [root, server, git] {
        real_dir(path).map_err(|e| e.to_string())?;
    }
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    let server = server.canonicalize().map_err(|e| e.to_string())?;
    let git = git.canonicalize().map_err(|e| e.to_string())?;
    if server.starts_with(&git)
        || git.starts_with(&server)
        || [&server, &git]
            .iter()
            .any(|p| root.starts_with(p) || p.starts_with(&root))
    {
        return Err("restore input and destinations must not overlap".into());
    }
    // Check before opening either lock: a symlink must never redirect a root open/chown.
    empty(&server, "kajiba.lock", None)?;
    empty(&git, "kajiba-git.lock", None)?;
    let _server_lock = lock(&server.join("kajiba.lock"), server_owner)?;
    let _git_lock = lock(&git.join("kajiba-git.lock"), git_owner)?;
    empty(&server, "kajiba.lock", None)?;
    empty(&git, "kajiba-git.lock", None)?;
    let manifest = validate(&root).await?;
    let mut created = Created(Vec::new());
    let result = async {
        let mut stages = Vec::new();
        for (side, destination, owner) in
            [("server", &server, server_owner), ("git", &git, git_owner)]
        {
            let stage = destination.join(format!(
                ".restore-{}",
                kajiba_core::new_id().map_err(|e| e.to_string())?
            ));
            fs::DirBuilder::new()
                .mode(0o700)
                .create(&stage)
                .map_err(|e| e.to_string())?;
            created.0.push(stage.clone());
            checkpoint("copy")?;
            let mut copied = BTreeMap::new();
            dump::copy_tree(&root.join(side), &stage, Path::new(side), &mut copied)
                .map_err(|e| e.to_string())?;
            let mut actual = BTreeMap::new();
            dump::inventory(&stage.join(side), Path::new(side), &mut actual, false)
                .map_err(|e| e.to_string())?;
            let expected: BTreeMap<_, _> = manifest
                .entries
                .iter()
                .filter(|(key, _)| dump::decode_path(key).is_ok_and(|path| path.starts_with(side)))
                .map(|(k, v)| (k.clone(), v))
                .collect();
            if actual != copied
                || actual.len() != expected.len()
                || actual.iter().any(|(k, v)| expected.get(k) != Some(&v))
            {
                return Err("restored staging inventory/content mismatch".into());
            }
            stages.push((side, destination, stage, owner));
        }
        validate(&root).await?;
        clear_logs(&stages[0].2.join("server/db/kajiba.sqlite")).await?;
        for (side, _, stage, owner) in &stages {
            permissions(&stage.join(side), *owner).map_err(|e| e.to_string())?;
            dump::sync_tree(stage).map_err(|e| e.to_string())?;
        }
        for (side, destination, stage, _) in &stages {
            empty(
                destination,
                if *side == "server" {
                    "kajiba.lock"
                } else {
                    "kajiba-git.lock"
                },
                Some(stage),
            )?;
        }
        // shortcut: cross-filesystem switching needs manual crash recovery; add a journal for automatic recovery.
        for (side, destination, stage, _) in &stages {
            for child in fs::read_dir(stage.join(side)).map_err(|e| e.to_string())? {
                let child = child.map_err(|e| e.to_string())?;
                let target = destination.join(child.file_name());
                fs::rename(child.path(), &target).map_err(|e| e.to_string())?;
                created.0.push(target);
            }
            checkpoint("switch")?;
        }
        for (_, destination, stage, _) in &stages {
            fs::remove_dir_all(stage).map_err(|e| e.to_string())?;
            crate::state::sync_dir(destination).map_err(|e| e.to_string())?;
        }
        Ok(())
    }
    .await;
    if let Err(error) = result {
        return match created.rollback() {
            Ok(()) => Err(error),
            Err(rollback) => Err(format!("{error}; restore rollback failed: {rollback}")),
        };
    }
    created.0.clear();
    Ok(())
}

fn permissions(path: &Path, owner: Identity) -> std::io::Result<()> {
    let kind = fs::symlink_metadata(path)?.file_type();
    if !kind.is_file() && !kind.is_dir() {
        return Err(std::io::Error::other("unsafe restore staging entry"));
    }
    chown(path, Some(owner.0), Some(owner.1))?;
    fs::set_permissions(
        path,
        fs::Permissions::from_mode(if kind.is_dir() { 0o750 } else { 0o640 }),
    )?;
    if kind.is_dir() {
        for child in fs::read_dir(path)? {
            permissions(&child?.path(), owner)?;
        }
    }
    Ok(())
}

async fn clear_logs(path: &Path) -> Result<()> {
    let mut conn = SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(path))
        .await
        .map_err(|e| e.to_string())?;
    let result = async {
        let chunks: i64 = sqlx::query_scalar("SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'log_chunk'")
            .fetch_one(&mut conn).await.map_err(|e| e.to_string())?;
        if chunks != 0 {
            let mut tx = conn.begin().await.map_err(|e| e.to_string())?;
            let lengths: i64 = sqlx::query_scalar("SELECT count(*) FROM pragma_table_info('job_attempt') WHERE name = 'purged_log_length'")
                .fetch_one(&mut *tx).await.map_err(|e| e.to_string())?;
            if lengths != 0 {
                sqlx::query("UPDATE job_attempt SET purged_log_length = MAX(purged_log_length, (SELECT COALESCE(MAX(offset + length), 0) FROM log_chunk WHERE attempt_id = job_attempt.id))")
                    .execute(&mut *tx).await.map_err(|e| e.to_string())?;
            }
            sqlx::query("DELETE FROM log_chunk").execute(&mut *tx).await.map_err(|e| e.to_string())?;
            tx.commit().await.map_err(|e| e.to_string())?;
        }
        Ok(())
    }.await;
    let close = conn.close().await.map_err(|e| e.to_string());
    result.and(close)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sockets_must_be_stopped_without_pending_jobs() {
        assert!(stopped_state(
            "LoadState=loaded\nActiveState=inactive\nJob=\n",
            false
        ));
        assert!(stopped_state(
            "LoadState=not-found\nActiveState=inactive\nJob=\n",
            true
        ));
        for state in ["active", "activating", "deactivating", "reloading"] {
            assert!(!stopped_state(
                &format!("LoadState=loaded\nActiveState={state}\nJob=\n"),
                false
            ));
        }
        assert!(!stopped_state(
            "LoadState=loaded\nActiveState=inactive\nJob=7\n",
            false
        ));
        assert!(!stopped_state(
            "LoadState=not-found\nActiveState=inactive\nJob=\n",
            false
        ));
        assert!(!stopped_state("", true));
    }
}

#[cfg(test)]
mod log_tests {
    use super::*;
    #[tokio::test]
    async fn log_cleanup_accepts_older_schemas_without_migrating() {
        let root = std::env::temp_dir().join(format!(
            "kajiba-restore-old-{}",
            kajiba_core::new_id().unwrap()
        ));
        fs::create_dir(&root).unwrap();
        let path = root.join("db.sqlite");
        let mut conn = SqliteConnection::connect_with(
            &SqliteConnectOptions::new()
                .filename(&path)
                .create_if_missing(true),
        )
        .await
        .unwrap();
        conn.close().await.unwrap();
        clear_logs(&path).await.unwrap();
        conn = SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&path))
            .await
            .unwrap();
        sqlx::raw_sql("CREATE TABLE job_attempt (id INTEGER PRIMARY KEY); CREATE TABLE log_chunk (attempt_id INTEGER, offset INTEGER, length INTEGER); INSERT INTO job_attempt VALUES (1); INSERT INTO log_chunk VALUES (1, 0, 10);").execute(&mut conn).await.unwrap();
        conn.close().await.unwrap();
        clear_logs(&path).await.unwrap();
        conn = SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&path))
            .await
            .unwrap();
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM log_chunk")
            .fetch_one(&mut conn)
            .await
            .unwrap();
        assert_eq!(count, 0);
        let columns: Vec<String> =
            sqlx::query_scalar("SELECT name FROM pragma_table_info('job_attempt')")
                .fetch_all(&mut conn)
                .await
                .unwrap();
        assert_eq!(columns, ["id"]);
        conn.close().await.unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn excluded_logs_drop_indexes_preserve_history_and_offsets_without_migration() {
        let root = std::env::temp_dir().join(format!(
            "kajiba-restore-logs-{}",
            kajiba_core::new_id().unwrap()
        ));
        fs::create_dir(&root).unwrap();
        let path = root.join("db.sqlite");
        let mut conn = SqliteConnection::connect_with(
            &SqliteConnectOptions::new()
                .filename(&path)
                .create_if_missing(true),
        )
        .await
        .unwrap();
        sqlx::raw_sql("CREATE TABLE job_attempt (id INTEGER PRIMARY KEY, purged_log_length INTEGER, state TEXT); CREATE TABLE log_chunk (attempt_id INTEGER, offset INTEGER, length INTEGER); INSERT INTO job_attempt VALUES (1, 5, 'succeeded'), (2, 40, 'running'); INSERT INTO log_chunk VALUES (1, 0, 12), (1, 12, 9), (2, 0, 10);").execute(&mut conn).await.unwrap();
        conn.close().await.unwrap();
        clear_logs(&path).await.unwrap();
        clear_logs(&path).await.unwrap();
        let mut conn = SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&path))
            .await
            .unwrap();
        let rows: Vec<(i64, String)> =
            sqlx::query_as("SELECT purged_log_length, state FROM job_attempt ORDER BY id")
                .fetch_all(&mut conn)
                .await
                .unwrap();
        assert_eq!(rows, [(21, "succeeded".into()), (40, "running".into())]);
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM log_chunk")
            .fetch_one(&mut conn)
            .await
            .unwrap();
        assert_eq!(count, 0);
        conn.close().await.unwrap();
        fs::remove_dir_all(root).unwrap();
    }
}
