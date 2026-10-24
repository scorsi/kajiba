use std::{
    collections::HashSet,
    ffi::OsString,
    fs::{self, File, OpenOptions, TryLockError},
    io,
    path::Path,
};

// Keep the returned file open for the server lifetime; never unlink the lock file.
pub fn prepare(state_dir: &Path) -> Result<File, String> {
    fs::create_dir_all(state_dir).map_err(|err| format!("{}: {err}", state_dir.display()))?;
    let path = state_dir.join("kajiba.lock");
    let lock = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(|err| format!("{}: {err}", path.display()))?;
    lock.try_lock().map_err(|err| match err {
        TryLockError::WouldBlock => format!("another instance holds {}", path.display()),
        TryLockError::Error(err) => format!("{}: {err}", path.display()),
    })?;
    for name in ["repos", "db", "ci/logs", "ci/artifacts", "tmp"] {
        let path = state_dir.join(name);
        fs::create_dir_all(&path).map_err(|err| format!("{}: {err}", path.display()))?;
    }
    Ok(lock)
}

// Run after migrations, while holding the state lock and before admitting work.
pub async fn cleanup(state_dir: &Path, writer: &mut sqlx::SqliteConnection) -> Result<(), String> {
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
            fs::create_dir_all(&orphans)?;
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
        for _ in 0..2 {
            let _lock = prepare(&root).unwrap();
            for name in ["repos", "db", "ci/logs", "ci/artifacts", "tmp"] {
                assert!(root.join(name).is_dir(), "{name}");
            }
            assert!(root.join("kajiba.lock").is_file());
        }
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

        cleanup(&temp.0, &mut writer).await.unwrap();

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

        cleanup(&temp.0, &mut writer).await.unwrap();

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
