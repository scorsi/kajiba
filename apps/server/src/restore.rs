//! Offline, two-tree restore (0170); the input verifier never migrates or writes.
use crate::{dump, state::StateLock};
use kajiba_core::{config::ServerConfig, state::real_dir};
use sqlx::{Connection, SqliteConnection, sqlite::SqliteConnectOptions};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    os::{
        fd::AsRawFd,
        unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    },
    path::{Path, PathBuf},
    process::Command,
};

type Result<T> = std::result::Result<T, String>;
type Identity = (u32, u32);

const MARKER: &str = "db/restore-pending";

/// Commit invalidation before removing the marker; a crash retries before admission.
pub(crate) async fn reconcile(
    writer: &mut SqliteConnection,
    state_dir: &Path,
    now: i64,
) -> Result<()> {
    let marker = state_dir.join(MARKER);
    match fs::symlink_metadata(&marker) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err.to_string()),
        Ok(_) => {}
    }
    let mut tx = writer.begin().await.map_err(|e| e.to_string())?;
    crate::db::attempts::restored(&mut tx, now)
        .await
        .map_err(|e| e.to_string())?;
    tx.commit().await.map_err(|e| e.to_string())?;
    fs::remove_file(&marker).map_err(|e| e.to_string())?;
    crate::state::sync_dir(marker.parent().unwrap()).map_err(|e| e.to_string())
}

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
        &std::env::temp_dir(),
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
    lock.set_owner(owner.0, owner.1)
        .map_err(|e| e.to_string())?;
    Ok(lock)
}

// Keep all privileged operations anchored even if a service renames its entries.
struct Directory {
    file: File,
    path: PathBuf,
}
impl Directory {
    fn open(path: &Path) -> std::io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(path)?;
        #[cfg(target_os = "linux")]
        let path = PathBuf::from(format!("/proc/self/fd/{}", file.as_raw_fd()));
        #[cfg(not(target_os = "linux"))]
        let path = path.to_path_buf(); // Only unprivileged tests run restore outside Linux.
        Ok(Self { file, path })
    }
}

pub(super) async fn install(
    root: &Path,
    server: &Path,
    git: &Path,
    server_owner: Identity,
    git_owner: Identity,
    temporary: &Path,
    checkpoint: &mut impl FnMut(&str) -> Result<()>,
) -> Result<()> {
    for path in [root, server, git] {
        real_dir(path).map_err(|e| e.to_string())?;
    }
    let server_dir = Directory::open(server).map_err(|e| e.to_string())?;
    let git_dir = Directory::open(git).map_err(|e| e.to_string())?;
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
    let server = server_dir.path.join(".");
    let git = git_dir.path.join(".");
    // The final lock component is opened without following symlinks.
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
            let pinned = Directory::open(&stage).map_err(|e| e.to_string())?;
            let metadata = pinned.file.metadata().map_err(|e| e.to_string())?;
            if metadata.uid() != kajiba_core::local::effective_uid()
                || metadata.permissions().mode() & 0o7777 != 0o700
            {
                return Err("unsafe restore staging directory".into());
            }
            let stage_path = pinned.path.join(".");
            checkpoint("copy")?;
            let mut copied = BTreeMap::new();
            dump::copy_tree(&root.join(side), &stage_path, Path::new(side), &mut copied)
                .map_err(|e| e.to_string())?;
            let mut actual = BTreeMap::new();
            dump::inventory(&stage_path.join(side), Path::new(side), &mut actual, false)
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
            stages.push((side, destination, stage, owner, pinned));
        }
        validate(&root).await?;
        clear_staged_logs(&stages[0].4.path.join("server/db/kajiba.sqlite"), temporary).await?;
        fs::write(stages[0].4.path.join("server").join(MARKER), b"").map_err(|e| e.to_string())?;
        for (side, _, _, owner, pinned) in &stages {
            let stage = pinned.path.join(".");
            permissions(&stage.join(side), *owner).map_err(|e| e.to_string())?;
            dump::sync_tree(&stage).map_err(|e| e.to_string())?;
        }
        for (side, destination, stage, _, _) in &stages {
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
        for (side, destination, _, _, pinned) in &stages {
            let stage = pinned.path.join(".");
            for child in fs::read_dir(stage.join(side)).map_err(|e| e.to_string())? {
                let child = child.map_err(|e| e.to_string())?;
                let target = destination.join(child.file_name());
                fs::rename(child.path(), &target).map_err(|e| e.to_string())?;
                created.0.push(target);
            }
            checkpoint("switch")?;
        }
        for (_, destination, stage, _, _) in &stages {
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
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    let kind = metadata.file_type();
    if !kind.is_file() && !kind.is_dir() {
        return Err(std::io::Error::other("unsafe restore staging entry"));
    }
    if kind.is_file() && metadata.nlink() != 1 {
        return Err(std::io::Error::other("hard-linked restore staging entry"));
    }
    if unsafe { libc::fchown(file.as_raw_fd(), owner.0, owner.1) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    file.set_permissions(fs::Permissions::from_mode(if kind.is_dir() {
        0o750
    } else {
        0o640
    }))?;
    if kind.is_dir() {
        for child in fs::read_dir(path)? {
            permissions(&child?.path(), owner)?;
        }
    }
    Ok(())
}

fn protected_directory(uid: u32, mode: u32, effective_uid: u32) -> bool {
    [0, effective_uid].contains(&uid) && (mode & 0o022 == 0 || uid == 0 && mode & 0o1000 != 0)
}

fn protected_temporary(temporary: &Path) -> Result<PathBuf> {
    let metadata = fs::symlink_metadata(temporary).map_err(|e| e.to_string())?;
    if !metadata.is_dir() {
        return Err("restore requires a protected temporary directory".into());
    }
    let temporary = temporary.canonicalize().map_err(|e| e.to_string())?;
    for parent in temporary.ancestors() {
        let metadata = fs::metadata(parent).map_err(|e| e.to_string())?;
        if !protected_directory(
            metadata.uid(),
            metadata.permissions().mode(),
            kajiba_core::local::effective_uid(),
        ) {
            return Err("restore requires a protected temporary directory".into());
        }
    }
    Ok(temporary)
}

async fn clear_staged_logs(path: &Path, temporary: &Path) -> Result<()> {
    // SQLite resolves descriptor paths: keep its scratch pathname outside service-owned parents.
    let temporary = protected_temporary(temporary)?;
    let scratch = temporary.join(format!(
        "kajiba-restore-db-{}",
        kajiba_core::new_id().map_err(|e| e.to_string())?
    ));
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&scratch)
        .map_err(|e| e.to_string())?;
    let mut created = Created(vec![scratch.clone()]);
    let database = scratch.join("db.sqlite");
    fs::copy(path, &database).map_err(|e| e.to_string())?;
    clear_logs(&database).await?;
    fs::copy(&database, path).map_err(|e| e.to_string())?;
    created.rollback()
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
    fn temporary_directory_ownership_and_modes() {
        // Synthetic IDs cover foreign ownership without requiring chown privileges.
        for (uid, mode, accepted) in [
            (1000, 0o700, true),
            (1000, 0o750, true),
            (0, 0o755, true),
            (0, 0o1777, true),
            (1000, 0o770, false),
            (1000, 0o707, false),
            (1000, 0o1777, false),
            (0, 0o777, false),
            (2000, 0o700, false),
            (2000, 0o1777, false),
        ] {
            assert_eq!(
                protected_directory(uid, mode, 1000),
                accepted,
                "uid={uid}, mode={mode:o}"
            );
        }
        assert!(protected_directory(0, 0o700, 0));
    }

    #[test]
    fn temporary_directory_paths_reject_symlinks_files_and_unsafe_ancestors() {
        let root = Path::new("/tmp").canonicalize().unwrap().join(format!(
            "kajiba-restore-temp-{}",
            kajiba_core::new_id().unwrap()
        ));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let mut created = Created(vec![root.clone()]);
        assert_eq!(protected_temporary(&root).unwrap(), root);
        let link = root.join("link");
        std::os::unix::fs::symlink(&root, &link).unwrap();
        assert!(protected_temporary(&link).is_err());
        let file = root.join("file");
        fs::write(&file, b"").unwrap();
        assert!(protected_temporary(&file).is_err());
        let child = root.join("child");
        fs::DirBuilder::new().mode(0o700).create(&child).unwrap();
        for mode in [0o770, 0o707, 0o777] {
            fs::set_permissions(&root, fs::Permissions::from_mode(mode)).unwrap();
            assert!(protected_temporary(&root).is_err(), "mode={mode:o}");
            assert!(
                protected_temporary(&child).is_err(),
                "ancestor mode={mode:o}"
            );
        }
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(protected_temporary(&child).is_ok());
        created.rollback().unwrap();
    }

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
