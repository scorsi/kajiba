use super::*;

#[tokio::test]
async fn verified_dumps_rotate_but_unrelated_incomplete_and_symlinks_survive() {
    let mut f = Fixture::new().await;
    let first = f.dump().await.unwrap();
    let second = f.dump().await.unwrap();
    let third = f.dump().await.unwrap();
    // Never prune good backups if the new payload no longer verifies.
    fs::write(third.join("unexpected"), "invalid").unwrap();
    assert!(retain(&f.dumps, 1, &third).await.is_err());
    assert!(first.exists() && second.exists() && third.exists());
    fs::remove_file(third.join("unexpected")).unwrap();
    let unrelated = f.dumps.join("unrelated");
    let partial = f.dumps.join(".partial-interrupted");
    let invalid = f.dumps.join("dump-1-01ARZ3NDEKTSV4RRFFQ69G5FAV");
    for path in [&unrelated, &partial, &invalid] {
        fs::create_dir(path).unwrap();
        fs::write(path.join("keep"), "untouched").unwrap();
    }
    let link = f.dumps.join("dump-2-01ARZ3NDEKTSV4RRFFQ69G5FAV");
    symlink(&first, &link).unwrap();
    // Filesystem timestamps cannot make the oldest payload the newest backup.
    File::options()
        .write(true)
        .open(first.join("manifest.json"))
        .unwrap()
        .set_modified(std::time::SystemTime::now())
        .unwrap();
    // Ancestor symlinks must not prevent recognizing/protecting the canonical new dump.
    let alias = f.root.join("alias");
    symlink(&f.root, &alias).unwrap();
    retain(&alias.join("dumps"), 2, &third).await.unwrap();
    assert!(!first.exists());
    assert!(second.exists());
    verify(&third).await.unwrap();
    for path in [&unrelated, &partial, &invalid] {
        assert_eq!(fs::read(path.join("keep")).unwrap(), b"untouched");
    }
    assert!(fs::symlink_metadata(link).unwrap().is_symlink());
    retain(&f.dumps, 0, &third).await.unwrap();
    assert!(!second.exists());
    verify(&third).await.unwrap();
    f.close().await;
}

#[test]
fn defaults() {
    let config = kajiba_core::config::DumpConfig::default();
    assert_eq!(config.keep, 7);
    assert_eq!(config.dir, Path::new("/var/lib/kajiba-dumps"));
}

#[test]
fn ordering_boundaries_clock_regression_and_failed_deletion() {
    let candidates = || {
        vec![
            (30, "newest".into()),
            (10, "oldest".into()),
            (20, "b".into()),
            (20, "a".into()),
            (5, "current".into()),
        ]
    };
    for (keep, expected) in [
        (0, vec!["oldest", "a", "b", "newest"]),
        (1, vec!["oldest", "a", "b", "newest"]),
        (3, vec!["oldest", "a"]),
        (5, vec![]),
        (10, vec![]),
    ] {
        let mut removed = Vec::new();
        prune(candidates(), keep, Path::new("current"), |path| {
            removed.push(path.to_str().unwrap().to_owned());
            Ok(())
        })
        .unwrap();
        assert_eq!(removed, expected, "keep={keep}");
    }
    prune(vec![], 0, Path::new("current"), |_| panic!("empty")).unwrap();
    let mut attempted = Vec::new();
    let error = prune(candidates(), 1, Path::new("current"), |path| {
        attempted.push(path.to_str().unwrap().to_owned());
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "injected deletion failure",
        ))
    })
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    assert_eq!(attempted, ["oldest"]);
}
