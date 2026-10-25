use std::{
    collections::HashSet,
    ffi::OsString,
    fs::{self, File},
    io,
    path::Path,
};

// Keep the returned lock for the server lifetime; never unlink the lock file.
pub fn prepare(state_dir: &Path) -> Result<StateLock, String> {
    prepare_with_sync(state_dir, &mut sync_dir)
}

pub use kajiba_core::state::StateLock;
pub(crate) use kajiba_core::state::sync_dir;
use kajiba_core::state::{create_dir_all, real_dir};

/// Every file and directory under `path`, then `path` itself.
pub(crate) fn sync_tree(path: &Path) -> io::Result<()> {
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            sync_tree(&entry.path())?;
        } else {
            File::open(entry.path())?.sync_all()?;
        }
    }
    sync_dir(path)
}

fn prepare_with_sync(
    state_dir: &Path,
    sync: &mut impl FnMut(&Path) -> io::Result<()>,
) -> Result<StateLock, String> {
    create_dir_all(state_dir, sync).map_err(|err| format!("{}: {err}", state_dir.display()))?;
    let lock = StateLock::acquire(&state_dir.join("kajiba.lock"))?;
    for name in ["repos", "db", "ci/logs", "ci/artifacts", "tmp"] {
        let path = state_dir.join(name);
        create_dir_all(&path, sync).map_err(|err| format!("{}: {err}", path.display()))?;
    }
    for name in ["repos", "db", "ci", "ci/logs", "ci/artifacts", "tmp"] {
        real_dir(&state_dir.join(name)).map_err(|err| err.to_string())?;
    }
    Ok(lock)
}

// Run after migrations, while holding the state lock and before admitting work.
pub async fn cleanup(state_dir: &Path, writer: &mut sqlx::SqliteConnection) -> Result<(), String> {
    cleanup_with_sync(state_dir, writer, &mut sync_dir).await
}

async fn cleanup_with_sync(
    state_dir: &Path,
    writer: &mut sqlx::SqliteConnection,
    sync: &mut impl FnMut(&Path) -> io::Result<()>,
) -> Result<(), String> {
    let ids: Vec<String> = sqlx::query_scalar("SELECT public_id FROM repo")
        .fetch_all(writer)
        .await
        .map_err(|err| format!("read registered repositories: {err}"))?;
    let registered: HashSet<OsString> = ids
        .into_iter()
        .map(|id| format!("{id}.git").into())
        .collect();
    let result = (|| -> io::Result<()> {
        let tmp = state_dir.join("tmp");
        let orphans = tmp.join("orphans");
        real_dir(&tmp)?;
        real_dir(&state_dir.join("repos"))?;
        for entry in fs::read_dir(&tmp)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            // Quarantined repositories must survive subsequent startups.
            if entry.file_name() == "orphans" {
                if !kind.is_dir() {
                    return Err(io::Error::other("tmp/orphans is not a directory"));
                }
            } else if kind.is_dir() {
                fs::remove_dir_all(entry.path())?;
            } else {
                fs::remove_file(entry.path())?;
            }
        }
        for entry in fs::read_dir(state_dir.join("repos"))? {
            let entry = entry?;
            let name = entry.file_name();
            if !entry.file_type()?.is_dir() || registered.contains(&name) {
                continue;
            }
            create_dir_all(&orphans, sync)?;
            let mut destination = orphans.join(&name);
            let mut suffix = 0_u64;
            loop {
                match fs::symlink_metadata(&destination) {
                    Ok(_) => {
                        suffix += 1;
                        let mut unique = name.clone();
                        unique.push(format!(".{suffix}"));
                        destination = orphans.join(unique);
                    }
                    Err(err) if err.kind() == io::ErrorKind::NotFound => break,
                    Err(err) => return Err(err),
                }
            }
            fs::rename(entry.path(), &destination)?;
            sync(&state_dir.join("repos"))?;
            sync(&orphans)?;
            tracing::warn!(
                event = "repo-orphaned",
                path = %entry.path().display(),
                destination = %destination.display(),
                "moved unregistered repository to quarantine"
            );
        }
        Ok(())
    })();
    result.map_err(|err| format!("cleanup {}: {err}", state_dir.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::Connection;
    use std::{
        path::PathBuf,
        sync::atomic::{AtomicUsize, Ordering},
    };

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "kajiba-state-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn creates_layout_and_accepts_existing_directories() {
        let temp = TempDir::new();
        let root = temp.0.join("nested/state");
        let mut synced = Vec::new();
        for attempt in 0..2 {
            synced.clear();
            let _lock = prepare_with_sync(&root, &mut |parent| {
                synced.push(parent.to_path_buf());
                sync_dir(parent)
            })
            .unwrap();
            for name in ["repos", "db", "ci/logs", "ci/artifacts", "tmp"] {
                assert!(root.join(name).is_dir(), "{name}");
            }
            assert!(root.join("kajiba.lock").is_file());
            if attempt == 0 {
                assert_eq!(
                    synced,
                    [
                        temp.0.clone(),
                        temp.0.join("nested"),
                        root.clone(),
                        root.clone(),
                        root.clone(),
                        root.join("ci"),
                        root.join("ci"),
                        root.clone(),
                    ]
                );
                fs::write(root.join("db/preserve"), "existing").unwrap();
            } else {
                assert!(synced.is_empty());
                assert_eq!(
                    fs::read_to_string(root.join("db/preserve")).unwrap(),
                    "existing"
                );
            }
        }
    }

    #[test]
    fn prepare_reports_parent_sync_failure_and_releases_lock() {
        let temp = TempDir::new();
        let error = prepare_with_sync(&temp.0, &mut |parent| {
            assert_eq!(parent, temp.0);
            assert!(parent.join("repos").is_dir());
            Err(io::Error::other("parent sync failed"))
        })
        .unwrap_err();
        assert!(error.contains("parent sync failed"));
        assert!(!temp.0.join("db").exists());
        let _lock = prepare(&temp.0).unwrap();
    }

    #[test]
    fn refuses_second_descriptor_and_releases_on_drop() {
        let temp = TempDir::new();
        let first = prepare(&temp.0).unwrap();
        assert_eq!(
            prepare(&temp.0).unwrap_err(),
            format!(
                "another instance holds {}",
                temp.0.join("kajiba.lock").display()
            )
        );
        drop(first);
        let _second = prepare(&temp.0).unwrap();
    }

    #[test]
    fn reports_filesystem_errors_and_releases_lock_on_failure() {
        let temp = TempDir::new();
        let path = temp.0.join("db");
        fs::write(&path, "preserve").unwrap();
        assert!(
            prepare(&temp.0)
                .unwrap_err()
                .contains(&path.display().to_string())
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "preserve");
        fs::remove_file(path).unwrap();
        let _lock = prepare(&temp.0).unwrap();
    }

    #[test]
    fn reports_unopenable_lock_file() {
        let temp = TempDir::new();
        let path = temp.0.join("kajiba.lock");
        fs::create_dir(&path).unwrap();
        assert!(
            prepare(&temp.0)
                .unwrap_err()
                .contains(&path.display().to_string())
        );
    }

    #[tokio::test]
    async fn cleanup_removes_temporary_data_and_preserves_registered_repository() {
        let temp = TempDir::new();
        let _lock = prepare(&temp.0).unwrap();
        let (mut writer, readers) = crate::db::open(&temp.0.join("db/kajiba.sqlite"))
            .await
            .unwrap();
        let id = "00000000000000000000000001";
        sqlx::query("INSERT INTO user (id, public_id, name, kind, is_admin, source, created_at) VALUES (1, ?1, 'owner', 'local', 0, 'config', 0)")
            .bind(id).execute(&mut writer).await.unwrap();
        sqlx::query("INSERT INTO repo (public_id, owner_id, name, visibility, ci_enabled, default_branch, description, created_at) VALUES (?1, 1, 'demo', 'public', 0, 'main', '', 0)")
            .bind(id).execute(&mut writer).await.unwrap();
        let registered = temp.0.join(format!("repos/{id}.git"));
        fs::create_dir(&registered).unwrap();
        fs::write(registered.join("HEAD"), "registered").unwrap();
        // A name matching the ID without the layout's .git suffix is still an orphan.
        let orphan = temp.0.join(format!("repos/{id}"));
        fs::create_dir(&orphan).unwrap();
        fs::write(orphan.join("HEAD"), "orphan").unwrap();
        fs::write(temp.0.join("tmp/file"), "temporary").unwrap();
        fs::create_dir_all(temp.0.join("tmp/nested/dir")).unwrap();
        fs::write(temp.0.join("tmp/nested/dir/file"), "temporary").unwrap();
        fs::write(temp.0.join("repos/file"), "leave non-directories").unwrap();

        let mut synced = Vec::new();
        cleanup_with_sync(&temp.0, &mut writer, &mut |parent| {
            if parent != temp.0.join("tmp") {
                assert!(!orphan.exists());
                assert!(temp.0.join(format!("tmp/orphans/{id}/HEAD")).is_file());
            }
            synced.push(parent.to_path_buf());
            sync_dir(parent)
        })
        .await
        .unwrap();
        assert_eq!(
            synced,
            [
                temp.0.join("tmp"),
                temp.0.join("repos"),
                temp.0.join("tmp/orphans")
            ]
        );

        assert!(!temp.0.join("tmp/file").exists());
        assert!(!temp.0.join("tmp/nested").exists());
        assert!(!orphan.exists());
        assert_eq!(
            fs::read_to_string(registered.join("HEAD")).unwrap(),
            "registered"
        );
        let quarantined = temp.0.join(format!("tmp/orphans/{id}/HEAD"));
        assert_eq!(fs::read_to_string(&quarantined).unwrap(), "orphan");
        assert_eq!(
            fs::read_to_string(temp.0.join("repos/file")).unwrap(),
            "leave non-directories"
        );
        cleanup(&temp.0, &mut writer).await.unwrap();
        assert_eq!(fs::read_to_string(quarantined).unwrap(), "orphan");
        readers.close().await;
        writer.close().await.unwrap();
    }

    #[tokio::test]
    async fn cleanup_suffixes_collisions_without_overwriting_quarantined_data() {
        let temp = TempDir::new();
        let _lock = prepare(&temp.0).unwrap();
        let (mut writer, readers) = crate::db::open(&temp.0.join("db/kajiba.sqlite"))
            .await
            .unwrap();
        let orphans = temp.0.join("tmp/orphans");
        fs::create_dir_all(orphans.join("orphan.git")).unwrap();
        fs::write(orphans.join("orphan.git.1"), "preserve file").unwrap();
        fs::create_dir(temp.0.join("repos/orphan.git")).unwrap();
        fs::write(temp.0.join("repos/orphan.git/HEAD"), "new orphan").unwrap();

        let mut synced = Vec::new();
        cleanup_with_sync(&temp.0, &mut writer, &mut |parent| {
            assert!(!temp.0.join("repos/orphan.git").exists());
            assert!(orphans.join("orphan.git.2/HEAD").is_file());
            synced.push(parent.to_path_buf());
            sync_dir(parent)
        })
        .await
        .unwrap();
        assert_eq!(synced, [temp.0.join("repos"), orphans.clone()]);

        assert!(orphans.join("orphan.git").is_dir());
        assert!(
            fs::read_dir(orphans.join("orphan.git"))
                .unwrap()
                .next()
                .is_none()
        );
        assert_eq!(
            fs::read_to_string(orphans.join("orphan.git.1")).unwrap(),
            "preserve file"
        );
        assert_eq!(
            fs::read_to_string(orphans.join("orphan.git.2/HEAD")).unwrap(),
            "new orphan"
        );
        assert!(!temp.0.join("repos/orphan.git").exists());
        readers.close().await;
        writer.close().await.unwrap();
    }

    #[tokio::test]
    async fn cleanup_reports_source_and_destination_sync_failures() {
        for failed_parent in ["repos", "tmp/orphans"] {
            let temp = TempDir::new();
            let _lock = prepare(&temp.0).unwrap();
            let (mut writer, readers) = crate::db::open(&temp.0.join("db/kajiba.sqlite"))
                .await
                .unwrap();
            fs::create_dir(temp.0.join("repos/orphan.git")).unwrap();
            let error = cleanup_with_sync(&temp.0, &mut writer, &mut |parent| {
                if parent == temp.0.join(failed_parent) {
                    assert!(!temp.0.join("repos/orphan.git").exists());
                    assert!(temp.0.join("tmp/orphans/orphan.git").is_dir());
                    Err(io::Error::other("parent sync failed"))
                } else {
                    sync_dir(parent)
                }
            })
            .await
            .unwrap_err();
            assert!(error.contains("parent sync failed"));
            readers.close().await;
            writer.close().await.unwrap();
        }
    }

    #[tokio::test]
    async fn cleanup_refuses_unreadable_registry_before_touching_files() {
        let temp = TempDir::new();
        let _lock = prepare(&temp.0).unwrap();
        let mut writer = sqlx::SqliteConnection::connect("sqlite::memory:")
            .await
            .unwrap();
        fs::write(temp.0.join("tmp/file"), "preserve").unwrap();
        fs::create_dir(temp.0.join("repos/orphan.git")).unwrap();

        assert!(
            cleanup(&temp.0, &mut writer)
                .await
                .unwrap_err()
                .contains("read registered repositories")
        );
        assert_eq!(
            fs::read_to_string(temp.0.join("tmp/file")).unwrap(),
            "preserve"
        );
        assert!(temp.0.join("repos/orphan.git").is_dir());
        writer.close().await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlinked_managed_directories_are_refused_before_any_deletion() {
        use std::os::unix::fs::symlink;

        for name in ["tmp", "repos", "db", "ci", "ci/logs"] {
            let temp = TempDir::new();
            let lock = prepare(&temp.0).unwrap();
            let target = temp.0.join("elsewhere");
            fs::rename(temp.0.join(name), &target).unwrap();
            symlink(&target, temp.0.join(name)).unwrap();
            drop(lock);
            let error = prepare(&temp.0).unwrap_err();
            assert!(error.contains("symlinks are refused"), "{name}: {error}");
        }
        // Replaced after prepare (cleanup re-checks): a tmp link to repos deletes nothing.
        let temp = TempDir::new();
        let _lock = prepare(&temp.0).unwrap();
        let (mut writer, readers) = crate::db::open(&temp.0.join("db/kajiba.sqlite"))
            .await
            .unwrap();
        fs::create_dir(temp.0.join("repos/keep.git")).unwrap();
        fs::remove_dir(temp.0.join("tmp")).unwrap();
        symlink(temp.0.join("repos"), temp.0.join("tmp")).unwrap();
        let error = cleanup(&temp.0, &mut writer).await.unwrap_err();
        assert!(error.contains("symlinks are refused"), "{error}");
        assert!(temp.0.join("repos/keep.git").is_dir());
        readers.close().await;
        writer.close().await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cleanup_unlinks_temporary_symlinks_and_refuses_symlink_quarantine() {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new();
        let _lock = prepare(&temp.0).unwrap();
        let (mut writer, readers) = crate::db::open(&temp.0.join("db/kajiba.sqlite"))
            .await
            .unwrap();
        let outside = temp.0.join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("file"), "preserve").unwrap();
        symlink(&outside, temp.0.join("tmp/link")).unwrap();
        cleanup(&temp.0, &mut writer).await.unwrap();
        assert!(fs::symlink_metadata(temp.0.join("tmp/link")).is_err());
        assert_eq!(
            fs::read_to_string(outside.join("file")).unwrap(),
            "preserve"
        );

        symlink(&outside, temp.0.join("tmp/orphans")).unwrap();
        fs::create_dir(temp.0.join("repos/orphan.git")).unwrap();
        assert!(
            cleanup(&temp.0, &mut writer)
                .await
                .unwrap_err()
                .contains("tmp/orphans is not a directory")
        );
        assert!(temp.0.join("repos/orphan.git").is_dir());
        assert!(!outside.join("orphan.git").exists());
        readers.close().await;
        writer.close().await.unwrap();
    }
}
