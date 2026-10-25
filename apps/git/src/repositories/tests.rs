use super::*;
use crate::state::{prepare, tests::TempDir};
use std::os::unix::fs::{MetadataExt, symlink};
const ID: &str = "00000000000000000000000001";

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
        assert_eq!(
            repos.quarantine(bad).unwrap_err().code(),
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
    assert_eq!(repos.quarantine(ID).unwrap_err().code(), Code::NotFound);
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
        .quarantine_with_sync(ID, &mut |parent| {
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
    repos.create(ID, "main").unwrap();
    assert_eq!(
        repos.quarantine(ID).unwrap(),
        orphans.join(format!("{ID}.git.4"))
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
            .quarantine_with_sync(ID, &mut |path| {
                if path == temp.0.join(parent) {
                    Err(io::Error::other("sync failed"))
                } else {
                    sync_dir(path)
                }
            })
            .unwrap_err();
        assert!(err.message().contains("sync failed"));
        assert!(temp.0.join(format!("tmp/orphans/{ID}.git/HEAD")).is_file());
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
                .quarantine(ID)
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
    assert!(repos.quarantine(ID).is_err());
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
        assert_eq!(git(&path, &["config", "core.sharedRepository"]), "0640");
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
    repos.register(session.clone()).unwrap();
    assert!(repos.register(session.clone()).is_err());
    assert_eq!(repos.sessions(), std::slice::from_ref(&session));
    assert!(repos.exits().is_empty());
    assert!(child.try_wait().unwrap().is_none());
    drop(child.stdin.take());
    let status = child.wait().unwrap().code().unwrap();
    repos.finished(&session.session_id, status);
    assert!(repos.sessions().is_empty());
    assert_eq!(repos.exits(), [(session.clone(), status)]);
    assert!(repos.register(session.clone()).is_err());
    repos.finished(&session.session_id, 1);
    assert_eq!(repos.exits(), [(session.clone(), status)]);
    repos.acknowledged(&session.session_id);
    repos.acknowledged(&session.session_id);
    assert!(repos.exits().is_empty());
}
