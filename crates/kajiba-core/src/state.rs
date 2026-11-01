//! Durable state directories and the daemon lifetime lock.
use std::{
    fs::{self, File, OpenOptions, TryLockError},
    io,
    os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    },
    path::Path,
};

/// The flock belongs to the open file description, which a concurrently spawned
/// child shares until its exec closes the descriptor: unlock explicitly on drop
/// instead of relying on the last close.
#[derive(Debug)]
pub struct StateLock(File);

impl Drop for StateLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

pub fn sync_dir(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

impl StateLock {
    pub fn acquire(path: &Path) -> Result<Self, String> {
        let lock = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
            .map_err(|err| format!("{}: {err}", path.display()))?;
        let metadata = lock.metadata().map_err(|err| err.to_string())?;
        if !metadata.is_file() || metadata.nlink() != 1 {
            return Err(format!(
                "{}: lock must be a regular file with one link",
                path.display()
            ));
        }
        lock.try_lock().map_err(|err| match err {
            TryLockError::WouldBlock => format!("another instance holds {}", path.display()),
            TryLockError::Error(err) => format!("{}: {err}", path.display()),
        })?;
        Ok(StateLock(lock))
    }

    pub fn set_owner(&self, uid: u32, gid: u32) -> io::Result<()> {
        // Ownership and permissions must apply to the inode holding the lock.
        if unsafe { libc::fchown(self.0.as_raw_fd(), uid, gid) } != 0 {
            return Err(io::Error::last_os_error());
        }
        self.0.set_permissions(fs::Permissions::from_mode(0o600))
    }
}

pub fn create_dir_all(
    path: &Path,
    sync: &mut impl FnMut(&Path) -> io::Result<()>,
) -> io::Result<()> {
    if path.is_dir() {
        return Ok(());
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    create_dir_all(parent, sync)?;
    match fs::create_dir(path) {
        Ok(()) => sync(parent),
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists && path.is_dir() => Ok(()),
        Err(err) => Err(err),
    }
}

/// A managed directory must be a directory itself, never a symlink: cleanup deletes
/// recursively under `tmp/`, and a `tmp` link to `repos` would delete repositories.
pub fn real_dir(path: &Path) -> io::Result<()> {
    if fs::symlink_metadata(path)?.is_dir() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "{}: not a directory (symlinks are refused)",
            path.display()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_refuses_links_and_special_files_and_updates_only_its_descriptor() {
        use std::os::unix::fs::symlink;
        let root = std::env::temp_dir().join(format!("kj-lock-links-{}", crate::new_id().unwrap()));
        fs::create_dir(&root).unwrap();
        let target = root.join("target");
        fs::write(&target, "preserve").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o640)).unwrap();
        let path = root.join("lock");
        symlink(&target, &path).unwrap();
        assert!(StateLock::acquire(&path).is_err());
        fs::remove_file(&path).unwrap();
        fs::hard_link(&target, &path).unwrap();
        assert!(StateLock::acquire(&path).is_err());
        fs::remove_file(&path).unwrap();
        assert!(StateLock::acquire(&root).is_err());
        let lock = StateLock::acquire(&path).unwrap();
        fs::rename(&path, root.join("held")).unwrap();
        symlink(&target, &path).unwrap();
        let owner = fs::metadata(&target).unwrap();
        lock.set_owner(owner.uid(), owner.gid()).unwrap();
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o640
        );
        assert_eq!(fs::read(&target).unwrap(), b"preserve");
        drop(lock);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn lock_releases_explicitly_even_with_an_inherited_descriptor() {
        let path = std::env::temp_dir().join(format!("kj-lock-{}", std::process::id()));
        let first = StateLock::acquire(&path).unwrap();
        assert!(
            StateLock::acquire(&path)
                .unwrap_err()
                .contains("another instance holds")
        );
        let inherited = first.0.try_clone().unwrap();
        drop(first);
        let second = StateLock::acquire(&path).unwrap();
        drop(inherited);
        drop(second);
        fs::remove_file(path).unwrap();
    }
}
