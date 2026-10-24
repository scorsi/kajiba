use std::{
    fs::{self, File, OpenOptions, TryLockError},
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

#[cfg(test)]
mod tests {
    use super::*;
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
}
