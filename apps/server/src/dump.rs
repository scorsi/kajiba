//! Verified durable payload; the online command composes this with maintenance (0167).
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{Connection, SqliteConnection, sqlite::SqliteConnectOptions};
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    fs::{self, File, OpenOptions, TryLockError},
    io::{self, Read, Write},
    os::unix::{
        ffi::{OsStrExt, OsStringExt},
        fs::{DirBuilderExt, OpenOptionsExt},
    },
    path::{Component, Path, PathBuf},
    process::Command,
};

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Manifest {
    pub format: u32,
    pub version: String,
    pub created_at_ms: u64,
    pub schema: i64,
    pub repositories: Vec<String>,
    pub entries: BTreeMap<String, Entry>,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub(crate) enum Entry {
    Directory,
    File { size: u64, sha256: String },
}

type Result<T> = std::result::Result<T, String>;

/// Keep quiescence borrowed until the completed payload has been published.
/// The online caller runs this on a blocking worker owning the guard: copy/hash/Git
/// checks are synchronous, so HTTP reads must keep their runtime workers.
pub(crate) async fn create(
    writer: &mut SqliteConnection,
    server: &Path,
    git: &Path,
    destination: &Path,
    _quiescent: &crate::transport::maintenance::Quiescent,
) -> Result<PathBuf> {
    create_payload(writer, server, git, destination).await
}

struct Staging(PathBuf);
impl Drop for Staging {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

async fn create_payload(
    writer: &mut SqliteConnection,
    server: &Path,
    git: &Path,
    destination: &Path,
) -> Result<PathBuf> {
    real_directory(destination).map_err(|e| e.to_string())?;
    // Never put staging inside a source tree, including via an ancestor symlink.
    let destination = destination.canonicalize().map_err(|e| e.to_string())?;
    for source in [server, git] {
        if destination.starts_with(source.canonicalize().map_err(|e| e.to_string())?) {
            return Err("dump destination is inside live state".into());
        }
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?;
    let suffix = format!(
        "{}-{}",
        now.as_millis(),
        kajiba_core::new_id().map_err(|e| e.to_string())?
    );
    let stage = destination.join(format!(".partial-{suffix}"));
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&stage)
        .map_err(|e| e.to_string())?;
    let staging = Staging(stage);
    let root = &staging.0;
    let result = async {
        fs::create_dir_all(root.join("server/db")).map_err(|e| e.to_string())?;
        snapshot(writer, &root.join("server/db/kajiba.sqlite")).await?;
        let mut conn = readonly(&root.join("server/db/kajiba.sqlite")).await?;
        let (schema, repositories) = database_metadata(&mut conn).await?;
        conn.close().await.map_err(|e| e.to_string())?;
        let mut expected = BTreeMap::new();
        let repo_root = git.join("repos");
        real_directory(&repo_root).map_err(|e| e.to_string())?;
        fs::create_dir_all(root.join("git/repos")).map_err(|e| e.to_string())?;
        for id in &repositories {
            validate_id(id)?;
            let relative = PathBuf::from(format!("git/repos/{id}.git"));
            let source = repo_root.join(format!("{id}.git"));
            copy_tree(&source, root, &relative, &mut expected).map_err(|e| e.to_string())?;
        }
        for (side, source) in [("server", server), ("git", git)] {
            real_directory(source).map_err(|e| e.to_string())?;
            let tmp = source.join("tmp");
            real_directory(&tmp).map_err(|e| e.to_string())?;
            let orphans = tmp.join("orphans");
            match fs::symlink_metadata(&orphans) {
                Ok(_) => copy_tree(
                    &orphans,
                    root,
                    &PathBuf::from(format!("{side}/tmp/orphans")),
                    &mut expected,
                )
                .map_err(|e| e.to_string())?,
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.to_string()),
            }
        }
        // Re-read source hashes/inventory so copy omissions or source changes fail closed.
        let mut sources = BTreeMap::new();
        for id in &repositories {
            inventory(
                &repo_root.join(format!("{id}.git")),
                &PathBuf::from(format!("git/repos/{id}.git")),
                &mut sources,
                true,
            )
            .map_err(|e| e.to_string())?;
        }
        for (side, source) in [("server", server), ("git", git)] {
            let path = source.join("tmp/orphans");
            if path.try_exists().map_err(|e| e.to_string())? {
                inventory(
                    &path,
                    &PathBuf::from(format!("{side}/tmp/orphans")),
                    &mut sources,
                    true,
                )
                .map_err(|e| e.to_string())?;
            }
        }
        if sources != expected {
            return Err("dump source changed during copy".into());
        }
        let mut entries = BTreeMap::new();
        inventory(root, Path::new(""), &mut entries, false).map_err(|e| e.to_string())?;
        let manifest = Manifest {
            format: 1,
            version: env!("CARGO_PKG_VERSION").into(),
            created_at_ms: now
                .as_millis()
                .try_into()
                .map_err(|_| "dump date overflow")?,
            schema,
            repositories,
            entries,
        };
        let bytes = serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?;
        let mut file = File::create(root.join("manifest.json")).map_err(|e| e.to_string())?;
        file.write_all(&bytes)
            .and_then(|()| file.sync_all())
            .map_err(|e| e.to_string())?;
        verify(root).await?;
        sync_tree(root).map_err(|e| e.to_string())?;
        let completed = destination.join(format!("dump-{suffix}"));
        fs::rename(root, &completed).map_err(|e| e.to_string())?;
        if let Err(e) = crate::state::sync_dir(&destination) {
            let _ = fs::remove_dir_all(&completed);
            return Err(e.to_string());
        }
        Ok(completed)
    }
    .await;
    result.map_err(|e| format!("dump: {e}"))
}

/// Rotate only verified published dumps; the caller still owns maintenance (0171).
pub(crate) async fn retain(destination: &Path, keep: u32, current: &Path) -> Result<()> {
    real_directory(destination).map_err(|e| e.to_string())?;
    let destination = destination.canonicalize().map_err(|e| e.to_string())?;
    let lock = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(destination.join(".copy.lock"))
        .map_err(|e| e.to_string())?;
    if !lock.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("dump copy lock is not a regular file".into());
    }
    match lock.try_lock() {
        Ok(()) => {}
        // External copies must not extend maintenance; retry pruning on the next dump.
        Err(TryLockError::WouldBlock) => return Ok(()),
        Err(TryLockError::Error(e)) => return Err(e.to_string()),
    }
    let _lock = CopyLock(lock);
    let mut completed = Vec::new();
    for entry in fs::read_dir(&destination).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        if !entry.file_type().map_err(|e| e.to_string())?.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let Some((date, id)) = name
            .to_str()
            .and_then(|n| n.strip_prefix("dump-"))
            .and_then(|n| n.split_once('-'))
        else {
            continue;
        };
        let Ok(date) = date.parse::<u64>() else {
            continue;
        };
        if validate_id(id).is_err() || name != OsStr::new(&format!("dump-{date}-{id}")) {
            continue;
        }
        if let Ok(manifest) = verify(&entry.path()).await {
            if manifest.created_at_ms == date {
                completed.push((date, entry.path()));
            }
        }
    }
    if !completed.iter().any(|(_, path)| path == current) {
        return Err("newly published dump failed retention verification".into());
    }
    prune(completed, keep, current, |path| fs::remove_dir_all(path))
        .and_then(|()| crate::state::sync_dir(&destination))
        .map_err(|e| e.to_string())
}

// Explicit unlock also releases a descriptor briefly inherited by a spawned child.
struct CopyLock(File);
impl Drop for CopyLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

fn prune(
    mut completed: Vec<(u64, PathBuf)>,
    keep: u32,
    current: &Path,
    mut remove: impl FnMut(&Path) -> io::Result<()>,
) -> io::Result<()> {
    completed.sort();
    // Reserve one slot for the new dump even if the host clock moved backwards.
    completed.retain(|(_, path)| path != current);
    let count = completed.len().saturating_sub(keep.max(1) as usize - 1);
    for (_, path) in completed.into_iter().take(count) {
        remove(&path)?;
    }
    Ok(())
}

async fn snapshot(source: &mut SqliteConnection, path: &Path) -> Result<()> {
    let mut destination = SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true),
    )
    .await
    .map_err(|e| e.to_string())?;
    {
        let mut source = source.lock_handle().await.map_err(|e| e.to_string())?;
        let mut target = destination.lock_handle().await.map_err(|e| e.to_string())?;
        // SAFETY: sqlx locks both handles against worker access for the whole backup.
        // No await/cancellation point occurs between init and finish; both handles outlive it.
        unsafe {
            use libsqlite3_sys::*;
            let backup = sqlite3_backup_init(
                target.as_raw_handle().as_ptr(),
                c"main".as_ptr(),
                source.as_raw_handle().as_ptr(),
                c"main".as_ptr(),
            );
            if backup.is_null() {
                return Err("SQLite backup initialization failed".into());
            }
            let step = sqlite3_backup_step(backup, -1);
            let finish = sqlite3_backup_finish(backup);
            if step != SQLITE_DONE || finish != SQLITE_OK {
                return Err(format!(
                    "SQLite backup failed: step={step}, finish={finish}"
                ));
            }
        }
    }
    destination.close().await.map_err(|e| e.to_string())
}

async fn readonly(path: &Path) -> Result<SqliteConnection> {
    SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .filename(path)
            .read_only(true)
            .immutable(true),
    )
    .await
    .map_err(|e| e.to_string())
}

async fn database_metadata(conn: &mut SqliteConnection) -> Result<(i64, Vec<String>)> {
    let integrity: Vec<String> = sqlx::query_scalar("PRAGMA integrity_check")
        .fetch_all(&mut *conn)
        .await
        .map_err(|e| e.to_string())?;
    if integrity != ["ok"]
        || !sqlx::query("PRAGMA foreign_key_check")
            .fetch_all(&mut *conn)
            .await
            .map_err(|e| e.to_string())?
            .is_empty()
    {
        return Err("invalid dump database integrity or foreign keys".into());
    }
    let migrations: Vec<(i64, bool, Vec<u8>)> =
        sqlx::query_as("SELECT version, success, checksum FROM _sqlx_migrations ORDER BY version")
            .fetch_all(&mut *conn)
            .await
            .map_err(|e| e.to_string())?;
    let known: Vec<_> = crate::db::MIGRATOR.iter().collect();
    if migrations.is_empty()
        || migrations.len() > known.len()
        || migrations
            .iter()
            .zip(&known)
            .any(|((v, success, checksum), m)| {
                !success || *v != m.version || checksum.as_slice() != m.checksum.as_ref()
            })
    {
        return Err("unsupported or incomplete dump schema".into());
    }
    let repositories = sqlx::query_scalar("SELECT public_id FROM repo ORDER BY public_id")
        .fetch_all(conn)
        .await
        .map_err(|e| e.to_string())?;
    Ok((migrations.last().unwrap().0, repositories))
}

/// Read-only validation, shared with the offline restore command.
pub(crate) async fn verify(root: &Path) -> Result<Manifest> {
    real_directory(root).map_err(|e| e.to_string())?;
    let path = root.join("manifest.json");
    if !fs::symlink_metadata(&path)
        .map_err(|e| e.to_string())?
        .is_file()
    {
        return Err("manifest is not a regular file".into());
    }
    let manifest: Manifest = serde_json::from_slice(&fs::read(path).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    if manifest.format != 1 {
        return Err("unsupported dump format".into());
    }
    for (key, entry) in &manifest.entries {
        let path = decode_path(key)?;
        let directory = *entry == Entry::Directory;
        let allowed = path == Path::new("server/db/kajiba.sqlite") && !directory
            || [
                "server",
                "server/db",
                "server/tmp",
                "git",
                "git/repos",
                "git/tmp",
            ]
            .iter()
            .any(|parent| path == Path::new(parent) && directory)
            || ["server/tmp/orphans", "git/tmp/orphans", "git/repos"]
                .iter()
                .any(|root| path.starts_with(root) && (path != Path::new(root) || directory));
        if !allowed {
            return Err("unexpected dump payload path".into());
        }
    }
    let mut actual = BTreeMap::new();
    inventory(root, Path::new(""), &mut actual, false).map_err(|e| e.to_string())?;
    if actual != manifest.entries {
        return Err("dump inventory/content mismatch".into());
    }
    let mut conn = readonly(&root.join("server/db/kajiba.sqlite")).await?;
    let (schema, repositories) = database_metadata(&mut conn).await?;
    conn.close().await.map_err(|e| e.to_string())?;
    if schema != manifest.schema || repositories != manifest.repositories {
        return Err("dump database metadata mismatch".into());
    }
    let mut names = Vec::new();
    for entry in fs::read_dir(root.join("git/repos")).map_err(|e| e.to_string())? {
        names.push(entry.map_err(|e| e.to_string())?.file_name());
    }
    names.sort();
    let expected: Vec<OsString> = repositories
        .iter()
        .map(|id| format!("{id}.git").into())
        .collect();
    if names != expected {
        return Err("dump repository inventory mismatch".into());
    }
    for id in &repositories {
        validate_id(id)?;
        check_repository(&root.join(format!("git/repos/{id}.git")))?;
    }
    Ok(manifest)
}

fn validate_id(id: &str) -> Result<()> {
    if id.len() != 26
        || !matches!(id.as_bytes().first(), Some(b'0'..=b'7'))
        || !id
            .bytes()
            .all(|b| b"0123456789ABCDEFGHJKMNPQRSTVWXYZ".contains(&b))
    {
        return Err("invalid repository public id in dump".into());
    }
    Ok(())
}

fn check_repository(path: &Path) -> Result<()> {
    if path
        .join("objects/info/alternates")
        .try_exists()
        .map_err(|e| e.to_string())?
    {
        return Err("dump repository uses external object alternates".into());
    }
    for args in [
        vec!["rev-parse", "--is-bare-repository"],
        vec!["fsck", "--full", "--strict", "--no-reflogs"],
    ] {
        let mut command = Command::new("git");
        for (key, _) in std::env::vars_os() {
            if key.as_bytes().starts_with(b"GIT_") {
                command.env_remove(key);
            }
        }
        let output = command
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .arg("-c")
            .arg(format!("safe.directory={}", path.display()))
            .arg("--git-dir")
            .arg(path)
            .args(&args)
            .output()
            .map_err(|e| e.to_string())?;
        if !output.status.success() || (args[0] == "rev-parse" && output.stdout != b"true\n") {
            return Err(format!(
                "invalid dump repository {}: {}",
                path.display(),
                String::from_utf8_lossy(&output.stderr)
            ));
        }
    }
    Ok(())
}

fn encoded(path: &Path) -> String {
    URL_SAFE_NO_PAD.encode(path.as_os_str().as_bytes())
}
pub(super) fn decode_path(key: &str) -> Result<PathBuf> {
    let bytes = URL_SAFE_NO_PAD.decode(key).map_err(|e| e.to_string())?;
    let path = PathBuf::from(OsString::from_vec(bytes));
    if path.as_os_str().is_empty()
        || path.as_os_str().as_bytes().contains(&0)
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
        || path.components().collect::<PathBuf>().as_os_str() != path.as_os_str()
        || encoded(&path) != key
    {
        return Err("unsafe dump path".into());
    }
    Ok(path)
}

fn real_directory(path: &Path) -> io::Result<()> {
    if fs::symlink_metadata(path)?.is_dir() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "{} is not a real directory",
            path.display()
        )))
    }
}

fn hash(path: &Path) -> io::Result<Entry> {
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut size = 0;
    let mut buffer = [0; 64 * 1024];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        digest.update(&buffer[..n]);
        size += n as u64;
    }
    Ok(Entry::File {
        size,
        sha256: format!("{:x}", digest.finalize()),
    })
}

fn excluded(path: &Path, kind: &fs::FileType) -> bool {
    use std::os::unix::fs::FileTypeExt;
    let name = path.file_name().unwrap_or(OsStr::new("")).as_bytes();
    let internal: PathBuf = path.components().skip(3).collect();
    path.starts_with("git/repos")
        && (kind.is_socket()
            || name.ends_with(b".lock")
            || (internal.parent() == Some(Path::new("objects")) && name.starts_with(b"incoming-"))
            || (internal.parent() == Some(Path::new("objects/pack")) && name.starts_with(b"tmp_")))
}

pub(super) fn copy_tree(
    source: &Path,
    root: &Path,
    relative: &Path,
    entries: &mut BTreeMap<String, Entry>,
) -> io::Result<()> {
    let kind = fs::symlink_metadata(source)?.file_type();
    if excluded(relative, &kind) {
        return Ok(());
    }
    let target = root.join(relative);
    if kind.is_dir() {
        fs::create_dir_all(&target)?;
        entries.insert(encoded(relative), Entry::Directory);
        for child in fs::read_dir(source)? {
            let child = child?;
            copy_tree(
                &child.path(),
                root,
                &relative.join(child.file_name()),
                entries,
            )?;
        }
    } else if kind.is_file() {
        let before = hash(source)?;
        fs::copy(source, &target)?;
        if hash(&target)? != before {
            return Err(io::Error::other("dump copy content mismatch"));
        }
        entries.insert(encoded(relative), before);
    } else {
        return Err(io::Error::other(format!(
            "unsafe dump source {}",
            source.display()
        )));
    }
    Ok(())
}

pub(super) fn inventory(
    source: &Path,
    relative: &Path,
    entries: &mut BTreeMap<String, Entry>,
    filter: bool,
) -> io::Result<()> {
    let kind = fs::symlink_metadata(source)?.file_type();
    if (relative == Path::new("manifest.json") && !filter) || (filter && excluded(relative, &kind))
    {
        return Ok(());
    }
    if kind.is_dir() {
        if !relative.as_os_str().is_empty() {
            entries.insert(encoded(relative), Entry::Directory);
        }
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            inventory(
                &entry.path(),
                &relative.join(entry.file_name()),
                entries,
                filter,
            )?;
        }
    } else if kind.is_file() {
        entries.insert(encoded(relative), hash(source)?);
    } else {
        return Err(io::Error::other(format!(
            "unsafe dump entry {}",
            source.display()
        )));
    }
    Ok(())
}

pub(super) fn sync_tree(path: &Path) -> io::Result<()> {
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            sync_tree(&entry.path())?;
        } else {
            File::open(entry.path())?.sync_all()?;
        }
    }
    crate::state::sync_dir(path)
}

#[cfg(test)]
mod tests;
