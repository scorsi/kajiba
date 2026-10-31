use std::{collections::HashSet, ffi::OsString, fs, io, os::unix::ffi::OsStrExt, path::Path};

// Keep the returned lock for the server lifetime; never unlink the lock file.
pub fn prepare(state_dir: &Path) -> Result<StateLock, String> {
    prepare_with_sync(state_dir, &mut sync_dir)
}

pub use kajiba_core::state::StateLock;
pub(crate) use kajiba_core::state::sync_dir;
use kajiba_core::state::{create_dir_all, real_dir};

fn prepare_with_sync(
    state_dir: &Path,
    sync: &mut impl FnMut(&Path) -> io::Result<()>,
) -> Result<StateLock, String> {
    create_dir_all(state_dir, sync).map_err(|err| format!("{}: {err}", state_dir.display()))?;
    let lock = StateLock::acquire(&state_dir.join("kajiba.lock"))?;
    for name in ["db", "ci/logs", "ci/artifacts", "ci/bundles", "tmp"] {
        let path = state_dir.join(name);
        create_dir_all(&path, sync).map_err(|err| format!("{}: {err}", path.display()))?;
    }
    for name in ["db", "ci", "ci/logs", "ci/artifacts", "ci/bundles", "tmp"] {
        real_dir(&state_dir.join(name)).map_err(|err| err.to_string())?;
    }
    Ok(lock)
}

// Run after migrations, while holding the state lock and before admitting work.
pub async fn cleanup(
    state_dir: &Path,
    git_state_dir: &Path,
    git: &mut crate::transport::git_side::Repositories,
    writer: &mut sqlx::SqliteConnection,
) -> Result<(), String> {
    use crate::transport::git_side::mutation_error;
    use kajiba_proto::git::v1::QuarantineRepositoryRequest;
    let ids: Vec<String> = sqlx::query_scalar("SELECT public_id FROM repo")
        .fetch_all(writer)
        .await
        .map_err(|err| format!("read registered repositories: {err}"))?;
    let registered: HashSet<OsString> = ids
        .into_iter()
        .map(|id| format!("{id}.git").into())
        .collect();
    let tmp = state_dir.join("tmp");
    let repos = git_state_dir.join("repos");
    let cleanup = || -> io::Result<()> {
        real_dir(&tmp)?;
        real_dir(&repos)?;
        for entry in fs::read_dir(&tmp)? {
            let entry = entry?;
            let kind = entry.file_type()?;
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
        Ok(())
    };
    cleanup().map_err(|err| format!("cleanup {}: {err}", state_dir.display()))?;
    for entry in fs::read_dir(&repos).map_err(|err| err.to_string())? {
        let entry = entry.map_err(|err| err.to_string())?;
        if !entry.file_type().map_err(|err| err.to_string())?.is_dir()
            || registered.contains(&entry.file_name())
        {
            continue;
        }
        let name = entry.file_name();
        let destination = git
            .client()
            .await
            .map_err(mutation_error)?
            .quarantine_repository(QuarantineRepositoryRequest {
                directory_name: name.as_bytes().to_vec(),
            })
            .await
            .map_err(mutation_error)?
            .into_inner()
            .destination;
        tracing::warn!(event = "repo-orphaned", path = %entry.path().display(), destination = %destination);
    }
    Ok(())
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
            for name in ["db", "ci/logs", "ci/artifacts", "ci/bundles", "tmp"] {
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
                        root.join("ci"),
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
            assert!(parent.join("db").is_dir());
            Err(io::Error::other("parent sync failed"))
        })
        .unwrap_err();
        assert!(error.contains("parent sync failed"));
        assert!(!temp.0.join("ci").exists());
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
        let mut daemon = crate::transport::git_side::fake::Fake::start(&temp.0).await;
        let id = "00000000000000000000000001";
        sqlx::query("INSERT INTO user (id, public_id, name, kind, is_admin, source, created_at) VALUES (1, ?1, 'owner', 'local', 0, 'config', 0)")
            .bind(id).execute(&mut writer).await.unwrap();
        sqlx::query("INSERT INTO repo (public_id, owner_id, name, visibility, ci_enabled, default_branch, description, created_at) VALUES (?1, 1, 'demo', 'public', 0, 'main', '', 0)")
            .bind(id).execute(&mut writer).await.unwrap();
        let registered = temp.0.join(format!("repos/{id}.git"));
        fs::create_dir(&registered).unwrap();
        fs::write(registered.join("HEAD"), "registered").unwrap();
        // A valid public id without a row is quarantined.
        let orphan = temp.0.join("repos/00000000000000000000000002.git");
        fs::create_dir(&orphan).unwrap();
        fs::write(orphan.join("HEAD"), "orphan").unwrap();
        fs::write(temp.0.join("tmp/file"), "temporary").unwrap();
        fs::create_dir_all(temp.0.join("tmp/nested/dir")).unwrap();
        fs::write(temp.0.join("tmp/nested/dir/file"), "temporary").unwrap();
        fs::write(temp.0.join("repos/file"), "leave non-directories").unwrap();

        cleanup(&temp.0, &temp.0, &mut daemon.client, &mut writer)
            .await
            .unwrap();

        assert!(!temp.0.join("tmp/file").exists());
        assert!(!temp.0.join("tmp/nested").exists());
        assert!(!orphan.exists());
        assert_eq!(
            fs::read_to_string(registered.join("HEAD")).unwrap(),
            "registered"
        );
        let quarantined = temp
            .0
            .join("tmp/orphans/00000000000000000000000002.git/HEAD");
        assert_eq!(fs::read_to_string(&quarantined).unwrap(), "orphan");
        assert_eq!(
            fs::read_to_string(temp.0.join("repos/file")).unwrap(),
            "leave non-directories"
        );
        cleanup(
            &temp.0,
            &temp.0,
            &mut crate::transport::git_side::Repositories::new(&temp.0.join("fake.sock")),
            &mut writer,
        )
        .await
        .unwrap();
        assert_eq!(fs::read_to_string(quarantined).unwrap(), "orphan");
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
        fs::create_dir_all(temp.0.join("repos/orphan.git")).unwrap();

        assert!(
            cleanup(
                &temp.0,
                &temp.0,
                &mut crate::transport::git_side::Repositories::new(&temp.0.join("fake.sock")),
                &mut writer
            )
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

        for name in ["tmp", "db", "ci", "ci/logs"] {
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
        fs::create_dir_all(temp.0.join("repos/keep.git")).unwrap();
        fs::remove_dir(temp.0.join("tmp")).unwrap();
        symlink(temp.0.join("repos"), temp.0.join("tmp")).unwrap();
        let error = cleanup(
            &temp.0,
            &temp.0,
            &mut crate::transport::git_side::Repositories::new(&temp.0.join("fake.sock")),
            &mut writer,
        )
        .await
        .unwrap_err();
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
        fs::create_dir(temp.0.join("repos")).unwrap();
        let outside = temp.0.join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("file"), "preserve").unwrap();
        symlink(&outside, temp.0.join("tmp/link")).unwrap();
        cleanup(
            &temp.0,
            &temp.0,
            &mut crate::transport::git_side::Repositories::new(&temp.0.join("fake.sock")),
            &mut writer,
        )
        .await
        .unwrap();
        assert!(fs::symlink_metadata(temp.0.join("tmp/link")).is_err());
        assert_eq!(
            fs::read_to_string(outside.join("file")).unwrap(),
            "preserve"
        );

        symlink(&outside, temp.0.join("tmp/orphans")).unwrap();
        fs::create_dir_all(temp.0.join("repos/orphan.git")).unwrap();
        assert!(
            cleanup(
                &temp.0,
                &temp.0,
                &mut crate::transport::git_side::Repositories::new(&temp.0.join("fake.sock")),
                &mut writer
            )
            .await
            .unwrap_err()
            .contains("tmp/orphans is not a directory")
        );
        assert!(temp.0.join("repos/orphan.git").is_dir());
        assert!(!outside.join("orphan.git").exists());
        readers.close().await;
        writer.close().await.unwrap();
    }
    #[tokio::test]
    async fn cleanup_quarantines_non_public_id_names_and_logs_destinations() {
        if std::env::var_os("KAJIBA_TEST_ORPHAN_LOG").is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "state::tests::cleanup_quarantines_non_public_id_names_and_logs_destinations",
                    "--nocapture",
                ])
                .env("KAJIBA_TEST_ORPHAN_LOG", "1")
                .env_remove("RUST_LOG")
                .output()
                .unwrap();
            let stderr = String::from_utf8(output.stderr).unwrap();
            assert!(output.status.success(), "{stderr}");
            let events: Vec<serde_json::Value> = stderr
                .lines()
                .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
                .filter(|line| line["event"] == "repo-orphaned")
                .collect();
            assert_eq!(
                events.len(),
                if cfg!(target_os = "linux") { 3 } else { 2 },
                "{stderr}"
            );
            for event in events {
                assert_eq!(event["level"], "warn");
                let path = event["path"].as_str().unwrap();
                let destination = event["destination"].as_str().unwrap();
                let mut expected = path.replace("/repos/", "/tmp/orphans/");
                if path.ends_with("/orphan.git") {
                    expected.push_str(".1");
                }
                assert_eq!(destination, expected);
            }
            return;
        }
        kajiba_core::log::init("info").unwrap();
        let temp = TempDir::new();
        let _lock = prepare(&temp.0).unwrap();
        let (mut writer, readers) = crate::db::open(&temp.0.join("db/kajiba.sqlite"))
            .await
            .unwrap();
        let mut daemon = crate::transport::git_side::fake::Fake::start(&temp.0).await;
        let names: &[&[u8]] = &[
            b"orphan.git",
            b"no-suffix",
            // APFS refuses non-UTF-8 filenames; Linux filesystems preserve them.
            #[cfg(target_os = "linux")]
            b"non-utf8-\xff.git",
        ];
        for name in names {
            let name = std::ffi::OsStr::from_bytes(name);
            let path = temp.0.join("repos").join(name);
            fs::create_dir(&path).unwrap();
            fs::write(path.join("evidence"), "preserve").unwrap();
        }
        fs::create_dir_all(temp.0.join("tmp/orphans/orphan.git")).unwrap();
        fs::write(temp.0.join("tmp/orphans/orphan.git/evidence"), "earlier").unwrap();
        cleanup(&temp.0, &temp.0, &mut daemon.client, &mut writer)
            .await
            .unwrap();
        assert_eq!(
            fs::read_to_string(temp.0.join("tmp/orphans/orphan.git/evidence")).unwrap(),
            "earlier"
        );
        for name in names {
            let name = std::ffi::OsStr::from_bytes(name);
            assert!(!temp.0.join("repos").join(name).exists());
            let mut destination = temp.0.join("tmp/orphans").join(name);
            if name == "orphan.git" {
                destination = temp.0.join("tmp/orphans/orphan.git.1");
            }
            assert_eq!(
                fs::read_to_string(destination.join("evidence")).unwrap(),
                "preserve"
            );
        }
        // A second sweep preserves the quarantine and emits no duplicate event.
        cleanup(&temp.0, &temp.0, &mut daemon.client, &mut writer)
            .await
            .unwrap();
        readers.close().await;
        writer.close().await.unwrap();
    }
}
