use super::*;
use std::{
    os::unix::fs::{MetadataExt, symlink},
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};

pub(crate) struct TempDir(pub PathBuf);
impl TempDir {
    pub fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "kj-git-{}-{}",
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
fn creates_layout_syncs_parents_and_preserves_repositories_and_quarantine() {
    let temp = TempDir::new();
    let root = temp.0.join("nested/state");
    let mut synced = Vec::new();
    let lock = prepare_with_sync(&root, &mut |path| {
        synced.push(path.to_path_buf());
        sync_dir(path)
    })
    .unwrap();
    assert_eq!(
        synced,
        [
            temp.0.clone(),
            temp.0.join("nested"),
            root.clone(),
            root.clone(),
            root.join("tmp"),
            root.join("tmp")
        ]
    );
    for name in ["repos", "tmp", "tmp/orphans"] {
        assert!(root.join(name).is_dir());
    }
    assert!(root.join("kajiba-git.lock").is_file());
    assert!(!root.join("db").exists());
    fs::write(root.join("repos/keep"), "repository").unwrap();
    fs::write(root.join("tmp/orphans/keep"), "quarantined").unwrap();
    fs::write(root.join("tmp/file"), "discard").unwrap();
    fs::create_dir_all(root.join("tmp/nested/dir")).unwrap();
    symlink(root.join("repos"), root.join("tmp/link")).unwrap();
    // A second instance must fail before removing another instance's staging data.
    assert!(
        prepare(&root)
            .unwrap_err()
            .contains("another instance holds")
    );
    assert!(root.join("tmp/file").exists());
    drop(lock);
    let _lock = prepare(&root).unwrap();
    assert_eq!(
        fs::read_to_string(root.join("repos/keep")).unwrap(),
        "repository"
    );
    assert_eq!(
        fs::read_to_string(root.join("tmp/orphans/keep")).unwrap(),
        "quarantined"
    );
    assert_eq!(fs::read_dir(root.join("tmp")).unwrap().count(), 1);
}

#[test]
fn sync_failure_stops_startup_and_releases_lock() {
    let temp = TempDir::new();
    let err = prepare_with_sync(&temp.0, &mut |path| {
        assert_eq!(path, temp.0);
        assert!(path.join("repos").is_dir());
        Err(io::Error::other("sync failed"))
    })
    .unwrap_err();
    assert!(err.contains("sync failed"));
    assert!(!temp.0.join("tmp").exists());
    let _lock = prepare(&temp.0).unwrap();
}

#[test]
fn refuses_managed_symlinks_without_deleting_their_targets() {
    for name in ["repos", "tmp", "tmp/orphans"] {
        let temp = TempDir::new();
        let outside = temp.0.join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("keep"), "preserve").unwrap();
        if name == "tmp/orphans" {
            fs::create_dir(temp.0.join("tmp")).unwrap();
        }
        symlink(&outside, temp.0.join(name)).unwrap();
        assert!(
            prepare(&temp.0)
                .unwrap_err()
                .contains("symlinks are refused")
        );
        assert_eq!(
            fs::read_to_string(outside.join("keep")).unwrap(),
            "preserve"
        );
    }
}

#[test]
fn directories_obey_umask_and_lock_is_private() {
    if let Some(root) = std::env::var_os("KAJIBA_TEST_STATE") {
        let root = Path::new(&root);
        let _lock = prepare(root).unwrap();
        for name in ["", "repos", "tmp", "tmp/orphans"] {
            assert_eq!(
                fs::metadata(root.join(name)).unwrap().mode() & 0o777,
                0o750,
                "{name}"
            );
        }
        assert_eq!(
            fs::metadata(root.join("kajiba-git.lock")).unwrap().mode() & 0o777,
            0o600
        );
        return;
    }
    let temp = TempDir::new();
    let output = std::process::Command::new("sh")
        .args(["-c", "umask 027; exec \"$@\"", "sh"])
        .arg(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "state::tests::directories_obey_umask_and_lock_is_private",
        ])
        .env("KAJIBA_TEST_STATE", temp.0.join("state"))
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
}
