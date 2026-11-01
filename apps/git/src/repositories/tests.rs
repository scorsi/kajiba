use super::*;
use crate::state::{prepare, tests::TempDir};
use std::os::unix::fs::{MetadataExt, symlink};
const ID: &str = "00000000000000000000000001";

#[test]
fn stalled_import_only_blocks_mutations_of_its_own_repository() {
    use kajiba_proto::git::v1::RemoteRef;
    use std::{sync::mpsc, time::Duration};
    const OTHER: &str = "00000000000000000000000002";
    let temp = TempDir::new();
    let _lock = prepare(&temp.0).unwrap();
    let repos = Repositories::new(temp.0.clone());
    // Minimal empty bare repositories; no Git command needs to write a fixture.
    for id in [ID, OTHER] {
        let path = temp.0.join(format!("repos/{id}.git"));
        for dir in ["refs/heads", "objects/info", "objects/pack"] {
            fs::create_dir_all(path.join(dir)).unwrap();
        }
        fs::write(path.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        fs::write(
            path.join("config"),
            "[core]\nrepositoryformatversion = 0\nbare = true\n",
        )
        .unwrap();
    }
    let request = |id: &str| FetchRequest {
        repo_public_id: id.into(),
        refs: vec![RemoteRef {
            ref_name: "refs/heads/main".into(),
            object_id: "1".repeat(40),
        }],
        ..Default::default()
    };
    let (entered, started) = mpsc::channel();
    let (release, resume) = mpsc::channel();
    let (done, completed) = mpsc::channel();
    let (same_done, same_completed) = mpsc::channel();
    std::thread::scope(|scope| {
        let repos = &repos;
        scope.spawn(move || {
            repos
                .fetch_with(request(ID), |_, _, _, _| {
                    entered.send(()).unwrap();
                    resume.recv().unwrap();
                    Ok(())
                })
                .unwrap();
        });
        started.recv_timeout(Duration::from_secs(5)).unwrap();
        scope.spawn(move || {
            let result = (|| -> Result<(), Status> {
                assert_eq!(
                    repos.create(OTHER, "main").unwrap_err().code(),
                    Code::AlreadyExists
                );
                repos.set_head(OTHER, "main")?;
                repos.fetch_with(request(OTHER), |_, _, _, _| Ok(()))?;
                repos.quarantine(format!("{OTHER}.git").as_bytes())?;
                Ok(())
            })();
            done.send(result).unwrap();
        });
        scope.spawn(move || {
            same_done.send(repos.set_head(ID, "main")).unwrap();
        });
        let other = completed.recv_timeout(Duration::from_secs(5));
        let same = same_completed.recv_timeout(Duration::from_millis(100));
        // Always release the import before asserting, including on a regression.
        release.send(()).unwrap();
        other.unwrap().unwrap();
        assert!(matches!(same, Err(mpsc::RecvTimeoutError::Timeout)));
        same_completed
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap();
    });
}

#[test]
fn mutation_locks_are_shared_with_waiters_and_retired_after_use() {
    let repos = Repositories::new("/unused".into());
    let first = repos.mutation(ID);
    let waiter = repos.mutation(ID);
    assert!(Arc::ptr_eq(&first, &waiter));
    drop(first);
    assert!(Arc::ptr_eq(&waiter, &repos.mutation(ID)));
    drop(waiter);
    let _other = repos.mutation("00000000000000000000000002");
    assert!(!repos.mutations.lock().unwrap().contains_key(ID));
}

#[test]
fn completed_session_counters_saturate() {
    use kajiba_proto::git::v1::SessionKind;
    let repositories = Repositories::new("/unused".into());
    let index = crate::metrics::kind_index(SessionKind::Import.into()).unwrap();
    repositories.sessions.lock().unwrap().completed[index][0] = u64::MAX;
    repositories
        .register(
            Session {
                session_id: "one".into(),
                kind: SessionKind::Import.into(),
                ..Default::default()
            },
            String::new(),
        )
        .unwrap();
    repositories.finished("one", 0);
    assert_eq!(
        repositories.sessions.lock().unwrap().completed[index][0],
        u64::MAX
    );
}

#[test]
fn creation_head_and_quarantine_validate_the_boundary_and_preserve_data() {
    let temp = TempDir::new();
    let _lock = prepare(&temp.0).unwrap();
    let repos = Repositories::new(temp.0.clone());
    for bad in [
        "",
        "../escape",
        "80000000000000000000000000",
        "0000000000000000000000000i",
    ] {
        assert_eq!(
            repos.create(bad, "main").unwrap_err().code(),
            Code::InvalidArgument
        );
        assert_eq!(
            repos.set_head(bad, "main").unwrap_err().code(),
            Code::InvalidArgument
        );
    }
    assert_eq!(
        repos.create(ID, "a..b").unwrap_err().code(),
        Code::InvalidArgument
    );
    assert_eq!(
        repos.set_head(ID, "main").unwrap_err().code(),
        Code::NotFound
    );
    assert_eq!(
        repos
            .quarantine(format!("{ID}.git").as_bytes())
            .unwrap_err()
            .code(),
        Code::NotFound
    );
    let path = temp.0.join(format!("repos/{ID}.git"));
    assert!(!path.exists());
    repos.create(ID, "trunk").unwrap();
    assert!(
        fs::read_dir(temp.0.join("tmp"))
            .unwrap()
            .all(|e| e.unwrap().file_name() == "orphans")
    );
    assert_eq!(
        fs::read_to_string(path.join("HEAD")).unwrap(),
        "ref: refs/heads/trunk\n"
    );
    assert_eq!(
        repos.create(ID, "main").unwrap_err().code(),
        Code::AlreadyExists
    );
    assert_eq!(
        repos.set_head(ID, "../bad").unwrap_err().code(),
        Code::InvalidArgument
    );
    repos.set_head(ID, "dev").unwrap();
    assert_eq!(
        fs::read_to_string(path.join("HEAD")).unwrap(),
        "ref: refs/heads/dev\n"
    );
    fs::write(path.join("evidence"), "preserve").unwrap();
    let orphans = temp.0.join("tmp/orphans");
    fs::create_dir(orphans.join(format!("{ID}.git"))).unwrap();
    fs::write(orphans.join(format!("{ID}.git.1")), "collision").unwrap();
    symlink("missing", orphans.join(format!("{ID}.git.2"))).unwrap();
    let mut synced = vec![];
    let destination = repos
        .quarantine_with_sync(format!("{ID}.git").as_bytes(), &mut |parent| {
            synced.push(parent.to_owned());
            sync_dir(parent)
        })
        .unwrap();
    assert_eq!(destination, orphans.join(format!("{ID}.git.3")));
    assert_eq!(synced, [temp.0.join("repos"), orphans.clone()]);
    assert_eq!(
        fs::read_to_string(destination.join("evidence")).unwrap(),
        "preserve"
    );
    assert_eq!(
        fs::read_to_string(orphans.join(format!("{ID}.git.1"))).unwrap(),
        "collision"
    );
    assert!(!path.exists());
    assert_eq!(
        repos.create(ID, "main").unwrap_err().code(),
        Code::AlreadyExists
    );
}

#[test]
fn quarantine_reports_sync_failures_and_refuses_symlinked_paths() {
    for parent in ["repos", "tmp/orphans"] {
        let temp = TempDir::new();
        let _lock = prepare(&temp.0).unwrap();
        let repos = Repositories::new(temp.0.clone());
        repos.create(ID, "main").unwrap();
        let err = repos
            .quarantine_with_sync(format!("{ID}.git").as_bytes(), &mut |path| {
                if path == temp.0.join(parent) {
                    Err(io::Error::other("sync failed"))
                } else {
                    sync_dir(path)
                }
            })
            .unwrap_err();
        assert!(err.message().contains("sync failed"));
        assert!(temp.0.join(format!("tmp/orphans/{ID}.git/HEAD")).is_file());
        // Rename completed before fsync failed: restart must retain the reservation.
        drop(_lock);
        let _lock = prepare(&temp.0).unwrap();
        let repos = Repositories::new(temp.0.clone());
        assert_eq!(
            repos.create(ID, "main").unwrap_err().code(),
            Code::AlreadyExists
        );
        assert_eq!(
            repos
                .quarantine(format!("{ID}.git").as_bytes())
                .unwrap_err()
                .code(),
            Code::NotFound
        );
        assert_eq!(fs::read_dir(temp.0.join("tmp/orphans")).unwrap().count(), 1);
    }
    for name in ["repos", "tmp", "tmp/orphans"] {
        let temp = TempDir::new();
        let _lock = prepare(&temp.0).unwrap();
        let repos = Repositories::new(temp.0.clone());
        repos.create(ID, "main").unwrap();
        let target = temp.0.join("elsewhere");
        fs::rename(temp.0.join(name), &target).unwrap();
        symlink(&target, temp.0.join(name)).unwrap();
        assert!(
            repos
                .quarantine(format!("{ID}.git").as_bytes())
                .unwrap_err()
                .message()
                .contains("symlinks are refused")
        );
    }
    let temp = TempDir::new();
    let _lock = prepare(&temp.0).unwrap();
    let repos = Repositories::new(temp.0.clone());
    let path = temp.0.join(format!("repos/{ID}.git"));
    symlink(temp.0.join("tmp"), &path).unwrap();
    assert_eq!(
        repos.create(ID, "main").unwrap_err().code(),
        Code::AlreadyExists
    );
    assert!(repos.set_head(ID, "main").is_err());
    assert!(repos.quarantine(format!("{ID}.git").as_bytes()).is_err());
}

#[test]
fn malformed_and_object_bearing_orphans_stay_until_operator_repair() {
    for (name, contents) in [
        ("HEAD", "not a ref\n"),
        ("HEAD", "ref: refs/heads/bad..name\n"),
        ("HEAD", ""),
        ("objects/info/alternates", "/outside/objects\n"),
        ("objects/pack/history.pack", "stored history"),
        ("objects/ab/object", "unreferenced history"),
    ] {
        let temp = TempDir::new();
        let _lock = prepare(&temp.0).unwrap();
        let repos = Repositories::new(temp.0.clone());
        repos.create(ID, "main").unwrap();
        let path = temp.0.join(format!("repos/{ID}.git"));
        let evidence = path.join(name);
        fs::create_dir_all(evidence.parent().unwrap()).unwrap();
        fs::write(&evidence, contents).unwrap();
        for _ in 0..2 {
            assert!(
                repos.quarantine(format!("{ID}.git").as_bytes()).is_err(),
                "{name}"
            );
            assert_eq!(fs::read_to_string(&evidence).unwrap(), contents);
            assert_eq!(fs::read_dir(temp.0.join("tmp/orphans")).unwrap().count(), 0);
        }
    }
}

#[test]
fn init_fetch_and_ref_updates_keep_group_read_without_group_write() {
    if let Some(root) = std::env::var_os("KAJIBA_TEST_REPO_MODES") {
        let root = PathBuf::from(root);
        let _lock = prepare(&root).unwrap();
        let repos = Repositories::new(root.clone());
        repos.create(ID, "main").unwrap();
        let path = root.join(format!("repos/{ID}.git"));
        let source = root.join("tmp/source.git");
        Gix.init_bare(&source, "main").unwrap();
        let git = |repo: &Path, args: &[&str]| {
            let output = std::process::Command::new("git")
                .arg("--git-dir")
                .arg(repo)
                .args(args)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_AUTHOR_NAME", "a")
                .env("GIT_AUTHOR_EMAIL", "a@example.org")
                .env("GIT_COMMITTER_NAME", "a")
                .env("GIT_COMMITTER_EMAIL", "a@example.org")
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
            String::from_utf8(output.stdout).unwrap().trim().to_owned()
        };
        let tree = git(&source, &["mktree"]);
        let commit = git(&source, &["commit-tree", &tree, "-m", "initial"]);
        git(&source, &["update-ref", "refs/heads/main", &commit]);
        fn modes(path: &Path) {
            let metadata = fs::metadata(path).unwrap();
            let mode = metadata.mode() & 0o777;
            assert!(
                if metadata.is_dir() {
                    mode == 0o750
                } else {
                    mode == 0o640 || mode == 0o440
                },
                "{}: {mode:o}",
                path.display()
            );
            if metadata.is_dir() {
                for entry in fs::read_dir(path).unwrap() {
                    modes(&entry.unwrap().path());
                }
            }
        }
        modes(&path);
        let refs = Gix.remote_refs(source.to_str().unwrap(), "").unwrap();
        Gix.fetch(&path, source.to_str().unwrap(), "", &refs)
            .unwrap();
        modes(&path);
        git(&path, &["update-ref", "refs/heads/dev", &commit]);
        git(&path, &["pack-refs", "--all"]);
        repos.set_head(ID, "dev").unwrap();
        modes(&path);
        return;
    }
    let temp = TempDir::new();
    let output = std::process::Command::new("sh")
        .args(["-c", "umask 027; exec \"$@\"", "sh"])
        .arg(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "repositories::tests::init_fetch_and_ref_updates_keep_group_read_without_group_write",
            "--nocapture",
        ])
        .env("KAJIBA_TEST_REPO_MODES", temp.0.join("state"))
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
}

#[test]
fn session_facts_remain_until_the_child_exits_and_the_server_acknowledges() {
    use kajiba_proto::git::v1::SessionKind;
    let temp = TempDir::new();
    let repos = Repositories::new(temp.0.clone());
    let session = Session {
        session_id: kajiba_core::new_id().unwrap(),
        generation: kajiba_core::new_id().unwrap(),
        kind: SessionKind::Import.into(),
        repo_public_id: ID.into(),
        ..Default::default()
    };
    let mut child = std::process::Command::new("cat")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    repos.register(session.clone(), String::new()).unwrap();
    assert!(repos.register(session.clone(), String::new()).is_err());
    assert_eq!(repos.sessions(), std::slice::from_ref(&session));
    assert!(repos.exits().is_empty());
    assert!(child.try_wait().unwrap().is_none());
    drop(child.stdin.take());
    let status = child.wait().unwrap().code().unwrap();
    repos.finished(&session.session_id, status);
    assert!(repos.sessions().is_empty());
    assert_eq!(repos.exits(), [(session.clone(), status)]);
    assert!(repos.register(session.clone(), String::new()).is_err());
    repos.finished(&session.session_id, 1);
    assert_eq!(repos.exits(), [(session.clone(), status)]);
    repos.acknowledged(&session.session_id);
    repos.acknowledged(&session.session_id);
    assert!(repos.exits().is_empty());
}

#[test]
fn quarantine_accepts_only_immediate_directory_names_and_preserves_bytes() {
    let temp = TempDir::new();
    let _lock = prepare(&temp.0).unwrap();
    let repos = Repositories::new(temp.0.clone());
    for name in [
        b"".as_slice(),
        b".",
        b"..",
        b"../escape",
        b"/absolute",
        b"a/b",
        b"a/",
        b"a\0b",
    ] {
        assert_eq!(
            repos.quarantine(name).unwrap_err().code(),
            Code::InvalidArgument
        );
    }
    assert_eq!(
        repos.quarantine(b"missing").unwrap_err().code(),
        Code::NotFound
    );
    fs::write(temp.0.join("repos/file"), "preserve").unwrap();
    symlink(temp.0.join("tmp"), temp.0.join("repos/link")).unwrap();
    assert!(repos.quarantine(b"file").is_err());
    assert!(repos.quarantine(b"link").is_err());
    assert_eq!(
        fs::read_to_string(temp.0.join("repos/file")).unwrap(),
        "preserve"
    );
    assert!(
        fs::symlink_metadata(temp.0.join("repos/link"))
            .unwrap()
            .is_symlink()
    );
    let names: &[&[u8]] = &[
        b"orphan.git",
        b"no-suffix",
        #[cfg(target_os = "linux")]
        b"non-utf8-\xff.git",
    ];
    for name in names {
        let filename = OsStr::from_bytes(name);
        let source = temp.0.join("repos").join(filename);
        // Eligibility (0067) still applies: an empty repository whose name is not a public id.
        fs::create_dir_all(source.join("objects/info")).unwrap();
        fs::create_dir_all(source.join("objects/pack")).unwrap();
        fs::create_dir_all(source.join("refs/heads")).unwrap();
        fs::write(source.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        fs::write(
            source.join("config"),
            "[core]\nrepositoryformatversion = 0\nbare = true\n",
        )
        .unwrap();
        fs::write(source.join("evidence"), "preserve").unwrap();
        let first = temp.0.join("tmp/orphans").join(filename);
        fs::write(&first, "collision").unwrap();
        let mut suffixed = filename.to_os_string();
        suffixed.push(".1");
        let expected = temp.0.join("tmp/orphans").join(suffixed);
        let mut synced = vec![];
        assert_eq!(
            repos
                .quarantine_with_sync(name, &mut |parent| {
                    synced.push(parent.to_owned());
                    sync_dir(parent)
                })
                .unwrap(),
            expected
        );
        assert_eq!(synced, [temp.0.join("repos"), temp.0.join("tmp/orphans")]);
        assert!(!source.exists());
        assert_eq!(
            fs::read_to_string(expected.join("evidence")).unwrap(),
            "preserve"
        );
        assert_eq!(fs::read_to_string(first).unwrap(), "collision");
    }
}

#[test]
fn gc_inventory_rejects_malformed_paths_and_excludes_quarantine() {
    let temp = TempDir::new();
    let _lock = prepare(&temp.0).unwrap();
    let repos = Repositories::new(temp.0.clone());
    repos.create(ID, "main").unwrap();
    fs::create_dir(temp.0.join("tmp/orphans/reserved.git")).unwrap();
    assert_eq!(repos.gc_inventory().unwrap(), [ID]);
    let invalid = temp.0.join("repos/unregistered");
    fs::create_dir(&invalid).unwrap();
    assert!(repos.gc_inventory().is_err());
    fs::remove_dir(&invalid).unwrap();
    let other = temp.0.join("repos/00000000000000000000000002.git");
    symlink(temp.0.join(format!("repos/{ID}.git")), &other).unwrap();
    assert!(repos.gc_inventory().is_err());
}

#[test]
fn gc_serializes_with_import_and_reports_child_failure() {
    use std::{sync::mpsc, time::Duration};
    let temp = TempDir::new();
    let _lock = prepare(&temp.0).unwrap();
    let repos = Repositories::new(temp.0.clone());
    repos.create(ID, "main").unwrap();
    let path = temp.0.join(format!("repos/{ID}.git"));
    assert_eq!(repos.gc(ID).unwrap(), 0);
    let (entered, started) = mpsc::channel();
    let (release, resume) = mpsc::channel();
    let (done, completed) = mpsc::channel();
    std::thread::scope(|scope| {
        let repos = &repos;
        scope.spawn(move || {
            repos
                .fetch_with(
                    FetchRequest {
                        repo_public_id: ID.into(),
                        refs: vec![kajiba_proto::git::v1::RemoteRef {
                            ref_name: "refs/heads/main".into(),
                            object_id: "1".repeat(40),
                        }],
                        ..Default::default()
                    },
                    |_, _, _, _| {
                        entered.send(()).unwrap();
                        resume.recv().unwrap();
                        Ok(())
                    },
                )
                .unwrap()
        });
        started.recv_timeout(Duration::from_secs(5)).unwrap();
        scope.spawn(move || done.send(repos.gc(ID)).unwrap());
        let waiting = completed.recv_timeout(Duration::from_millis(100));
        release.send(()).unwrap();
        assert!(matches!(waiting, Err(mpsc::RecvTimeoutError::Timeout)));
        assert_eq!(
            completed
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
                .unwrap(),
            0
        );
    });
    fs::write(path.join("config"), "[invalid\n").unwrap();
    assert_ne!(repos.gc(ID).unwrap(), 0);
    assert!(repos.gc("../escape").is_err());
}

#[test]
fn shutdown_refuses_new_gc_sessions() {
    let repos = Repositories::new("/unused".into());
    repos.stop_accepting();
    let err = repos
        .register(
            Session {
                session_id: ID.into(),
                kind: kajiba_proto::git::v1::SessionKind::Maintenance.into(),
                ..Default::default()
            },
            String::new(),
        )
        .unwrap_err();
    assert_eq!(err.code(), Code::Unavailable);
    assert!(repos.sessions().is_empty());
}
