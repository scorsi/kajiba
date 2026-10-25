use super::*;
use std::{fs, path::PathBuf};

mod push;

/// Runs the `git` binary with an isolated configuration and fixed identity.
fn git(dir: &Path, args: &[&str], date: &str) -> String {
    let output = Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Alice")
        .env("GIT_AUTHOR_EMAIL", "alice@example.org")
        .env("GIT_COMMITTER_NAME", "Alice")
        .env("GIT_COMMITTER_EMAIL", "alice@example.org")
        .env("GIT_AUTHOR_DATE", date)
        .env("GIT_COMMITTER_DATE", date)
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

pub(super) fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kajiba-gitstore-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The fixture: `first` (main~1, tag `v1` annotated), `second` (main, tag `light`),
/// `third` (branch `feature/x`).
struct Fixture {
    bare: PathBuf,
    first: String,
    second: String,
    third: String,
}

fn fixture(name: &str) -> Fixture {
    let dir = scratch(name);
    let work = dir.join("work");
    std::fs::create_dir(&work).unwrap();
    let t1 = "1700000000 +0000";
    git(&work, &["init", "-q", "-b", "main"], t1);
    std::fs::write(work.join("README.md"), "hello\n").unwrap();
    std::fs::create_dir(work.join("src")).unwrap();
    std::fs::write(work.join("src/lib.rs"), "fn main() {}\n").unwrap();
    std::fs::write(work.join("run.sh"), "#!/bin/sh\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(work.join("run.sh"), std::fs::Permissions::from_mode(0o755)).unwrap();
    std::os::unix::fs::symlink("README.md", work.join("link")).unwrap();
    git(&work, &["add", "-A"], t1);
    git(&work, &["commit", "-q", "-m", "first"], t1);
    let first = git(&work, &["rev-parse", "HEAD"], t1);
    git(&work, &["tag", "-a", "v1", "-m", "version 1"], t1);

    let t2 = "1700000100 +0000";
    std::fs::write(work.join("README.md"), "hello again\n").unwrap();
    std::fs::create_dir(work.join("docs")).unwrap();
    std::fs::write(work.join("docs/a.md"), "a\n").unwrap();
    std::fs::remove_file(work.join("src/lib.rs")).unwrap();
    git(&work, &["add", "-A"], t2);
    git(&work, &["commit", "-q", "-m", "second\n\nbody"], t2);
    let second = git(&work, &["rev-parse", "HEAD"], t2);
    git(&work, &["tag", "light"], t2);

    let t3 = "1700000200 +0000";
    git(&work, &["checkout", "-q", "-b", "feature/x"], t3);
    std::fs::write(work.join("README.md"), "feature\n").unwrap();
    git(&work, &["commit", "-q", "-am", "third"], t3);
    let third = git(&work, &["rev-parse", "HEAD"], t3);

    let bare = dir.join("bare.git");
    Gix.init_bare(&bare, "main").unwrap();
    git(
        &work,
        &[
            "push",
            "-q",
            bare.to_str().unwrap(),
            "refs/heads/*:refs/heads/*",
            "refs/tags/*:refs/tags/*",
        ],
        t3,
    );
    Fixture {
        bare,
        first,
        second,
        third,
    }
}

#[test]
fn init_bare_puts_head_on_the_default_branch() {
    let dir = scratch("init");
    let bare = dir.join("repo.git");
    Gix.init_bare(&bare, "trunk").unwrap();
    assert_eq!(
        git(&bare, &["symbolic-ref", "HEAD"], "0 +0000"),
        "refs/heads/trunk"
    );
    assert_eq!(
        git(&bare, &["rev-parse", "--is-bare-repository"], "0 +0000"),
        "true"
    );
    // Writes are durable before the database records them (ADR 0003).
    assert_eq!(git(&bare, &["config", "core.fsync"], "0 +0000"), "all");
    for key in ["receive.fsckObjects", "transfer.fsckObjects"] {
        assert_eq!(git(&bare, &["config", key], "0 +0000"), "true");
    }
    assert_eq!(Gix.refs(&bare).unwrap(), []);
    assert!(Gix.init_bare(&dir.join("bad.git"), "a..b").is_err());
    assert!(!dir.join("bad.git").exists());
}

#[test]
fn incoming_objects_are_checked_for_push_and_import() {
    let dir = scratch("incoming-fsck");
    let date = "1700000000 +0000";
    for (name, valid) in [("file", true), (".git", false)] {
        let source = dir.join(format!("source-{valid}.git"));
        Gix.init_bare(&source, "main").unwrap();
        fs::write(source.join("content"), "hello\n").unwrap();
        let blob = git(&source, &["hash-object", "-w", "content"], date);
        let tree = mktree(&source, &format!("100644 blob {blob}\t{name}\n"));
        let commit = git(&source, &["commit-tree", &tree, "-m", "fixture"], date);
        git(&source, &["update-ref", "refs/heads/main", &commit], date);
        let refs = Gix.refs(&source).unwrap();

        for import in [false, true] {
            let target = dir.join(format!("target-{valid}-{import}.git"));
            Gix.init_bare(&target, "main").unwrap();
            // Existing repositories may explicitly disable checks in local config.
            for key in [
                "receive.fsckObjects",
                "transfer.fsckObjects",
                "fetch.fsckObjects",
            ] {
                git(&target, &["config", key, "false"], date);
            }
            let (success, error) = if import {
                let result = Gix.fetch(&target, source.to_str().unwrap(), "", &refs);
                (result.is_ok(), format!("{result:?}"))
            } else {
                let receive = Gix.transport(Service::ReceivePack, &target, &[], None);
                let args: Vec<_> = receive
                    .get_args()
                    .map(|arg| arg.to_str().unwrap())
                    .collect();
                let receive = format!("git {}", args[..args.len() - 1].join(" "));
                let output = super::git()
                    .current_dir(&source)
                    .args([
                        "push",
                        "--receive-pack",
                        &receive,
                        target.to_str().unwrap(),
                        "refs/heads/main",
                    ])
                    .output()
                    .unwrap();
                (
                    output.status.success(),
                    String::from_utf8(output.stderr).unwrap(),
                )
            };
            assert_eq!(success, valid, "import={import}: {error}");
            if valid {
                assert_eq!(Gix.refs(&target).unwrap(), refs);
            } else {
                assert!(error.contains("hasDotgit"), "import={import}: {error}");
                assert!(Gix.refs(&target).unwrap().is_empty());
                assert!(
                    Gix.open(&target)
                        .unwrap()
                        .find_object(gix::ObjectId::from_hex(tree.as_bytes()).unwrap())
                        .is_err()
                );
            }
        }
    }
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn set_head_is_idempotent_and_replaces_head_atomically() {
    let dir = scratch("set-head");
    let bare = dir.join("repo.git");
    Gix.init_bare(&bare, "main").unwrap();
    let head = bare.join("HEAD");
    let epoch = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1);
    std::fs::File::options()
        .write(true)
        .open(&head)
        .unwrap()
        .set_modified(epoch)
        .unwrap();
    // Already pointing at the branch: the file is left alone.
    Gix.set_head(&bare, "main").unwrap();
    assert_eq!(std::fs::metadata(&head).unwrap().modified().unwrap(), epoch);
    // A truncated HEAD (crash mid-write) and a planted symlink are both replaced.
    std::fs::write(&head, "").unwrap();
    Gix.set_head(&bare, "main").unwrap();
    assert_eq!(
        std::fs::read_to_string(&head).unwrap(),
        "ref: refs/heads/main\n"
    );
    std::fs::remove_file(&head).unwrap();
    std::fs::write(dir.join("elsewhere"), "ref: refs/heads/main\n").unwrap();
    std::os::unix::fs::symlink(dir.join("elsewhere"), &head).unwrap();
    Gix.set_head(&bare, "trunk").unwrap();
    assert!(std::fs::symlink_metadata(&head).unwrap().is_file());
    assert_eq!(
        std::fs::read_to_string(&head).unwrap(),
        "ref: refs/heads/trunk\n"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("elsewhere")).unwrap(),
        "ref: refs/heads/main\n"
    );
    assert!(!bare.join("HEAD.tmp").exists());
    std::fs::remove_file(&head).unwrap();
    Gix.set_head(&bare, "trunk").unwrap();
    assert_eq!(
        std::fs::read_to_string(&head).unwrap(),
        "ref: refs/heads/trunk\n"
    );
    assert!(Gix.set_head(&dir.join("missing.git"), "main").is_err());
}

#[test]
fn import_commands_keep_credentials_out_of_the_command_line() {
    let envs = |command: &Command| -> std::collections::BTreeMap<String, String> {
        command
            .get_envs()
            .map(|(k, v)| {
                (
                    k.to_str().unwrap().to_owned(),
                    v.and_then(|v| v.to_str()).unwrap_or("").to_owned(),
                )
            })
            .collect()
    };
    let command = import_command("https://example.org/demo.git", "alice:s@cret");
    assert!(command.get_args().next().is_none());
    let env = envs(&command);
    assert_eq!(env["KAJIBA_IMPORT_USERNAME"], "alice");
    assert_eq!(env["KAJIBA_IMPORT_PASSWORD"], "s@cret");
    assert_eq!(env["GIT_ALLOW_PROTOCOL"], "http:https:ssh:git");
    assert!(env["GIT_CONFIG_VALUE_4"].starts_with("!f() { printf 'username=%s"));
    assert_eq!(env["GIT_CONFIG_KEY_0"], "core.hooksPath");
    assert!(env.contains_key("PATH"), "{env:?}");
    // The source is configured as a named remote in the environment and never
    // appears in the child's own argv (ADR 0048; git's transport helper still gets
    // it, ADR 0049).
    let source = "https://example.org/s3cret-token/demo.git";
    for command in [
        ls_remote_command(source, ""),
        fetch_command(Path::new("/srv/repo.git"), source, ""),
    ] {
        let args: Vec<_> = command.get_args().map(|a| a.to_str().unwrap()).collect();
        assert!(!args.iter().any(|a| a.contains("s3cret")), "{args:?}");
        assert_eq!(args.last(), Some(&IMPORT_REMOTE));
        let env = envs(&command);
        assert_eq!(env["GIT_CONFIG_KEY_3"], "remote.kajiba-import.url");
        assert_eq!(env["GIT_CONFIG_VALUE_3"], source);
    }
    // A token alone gets a fixed user name; a token containing ':' keeps its tail.
    let env = envs(&import_command("https://example.org/demo.git", "t0:k:en"));
    assert_eq!(env["KAJIBA_IMPORT_USERNAME"], "t0");
    assert_eq!(env["KAJIBA_IMPORT_PASSWORD"], "k:en");
    let env = envs(&import_command("https://example.org/demo.git", "tok3n"));
    assert_eq!(env["KAJIBA_IMPORT_USERNAME"], "token");
    assert_eq!(env["KAJIBA_IMPORT_PASSWORD"], "tok3n");
    // No credentials, a local path: nothing to pass, `file` allowed.
    let env = envs(&import_command("/srv/source.git", ""));
    assert_eq!(env.keys().filter(|k| k.starts_with("KAJIBA_")).count(), 0);
    assert_eq!(env["GIT_ALLOW_PROTOCOL"], "file:http:https:ssh:git");
    assert_eq!(env["GIT_CONFIG_COUNT"], "4");
}

fn sh(script: &str) -> Command {
    let mut command = Command::new("sh");
    command
        .args(["-c", script])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

#[test]
fn drain_reports_read_errors_instead_of_returning_incomplete_output() {
    let directory = fs::File::open(scratch("drain-error")).unwrap();
    let (stop, _) = std::sync::mpsc::channel();
    assert!(
        drain(
            directory,
            std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(16)),
            stop
        )
        .recv()
        .unwrap()
        .is_err()
    );
}

#[test]
fn run_kills_a_child_past_its_deadline() {
    for script in [
        "exec sleep 30",
        // Closing stdout early does not end the deadline (the pipes are not the clock).
        "exec >&- 2>&-; exec sleep 30",
    ] {
        let started = std::time::Instant::now();
        let err = run(sh(script), "", Duration::from_millis(200)).unwrap_err();
        assert!(
            err.to_string().contains("did not finish"),
            "{script}: {err}"
        );
        assert!(started.elapsed() < Duration::from_secs(10), "{script}");
    }
}

#[test]
fn run_does_not_wait_for_a_descendant_holding_the_pipes() {
    // The shell exits at once; its background child inherited stdout and stderr.
    let started = std::time::Instant::now();
    let output = run(
        sh("echo first; sleep 30 & exit 0"),
        "",
        Duration::from_secs(30),
    )
    .unwrap();
    assert_eq!(output, "first\n");
    assert!(started.elapsed() < Duration::from_secs(10));
    let err = run(
        sh("echo oops >&2; sleep 30 & exit 3"),
        "",
        Duration::from_secs(30),
    )
    .unwrap_err();
    assert_eq!(err.to_string(), "oops");
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[test]
fn refs_match_the_git_binary() {
    let f = fixture("refs");
    let expected = git(
        &f.bare,
        &["for-each-ref", "--format=%(refname) %(objectname)"],
        "0 +0000",
    );
    let actual: Vec<String> = Gix
        .refs(&f.bare)
        .unwrap()
        .into_iter()
        .map(|r| format!("{} {}", r.name, r.target))
        .collect();
    assert_eq!(actual.join("\n"), expected);
    // The annotated tag points to its tag object, not to the commit.
    let names: Vec<&str> = expected
        .lines()
        .map(|l| l.split(' ').next().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "refs/heads/feature/x",
            "refs/heads/main",
            "refs/tags/light",
            "refs/tags/v1"
        ]
    );
    assert!(!expected.contains(&format!("refs/tags/v1 {}", f.first)));
}

#[test]
fn tree_lists_entries_with_their_kind() {
    let f = fixture("tree");
    let root = Gix.tree(&f.bare, &f.first, "", usize::MAX).unwrap();
    let kinds: Vec<(&str, EntryKind)> = root.iter().map(|e| (e.name.as_str(), e.kind)).collect();
    assert_eq!(
        kinds,
        [
            ("README.md", EntryKind::Blob),
            ("link", EntryKind::Link),
            ("run.sh", EntryKind::Executable),
            ("src", EntryKind::Tree),
        ]
    );
    let src = Gix.tree(&f.bare, &f.first, "src", usize::MAX).unwrap();
    assert_eq!(src.len(), 1);
    assert_eq!(src[0].name, "lib.rs");
    assert_eq!(
        src[0].id,
        git(
            &f.bare,
            &["rev-parse", &format!("{}:src/lib.rs", f.first)],
            "0 +0000"
        )
    );
    for path in ["missing", "README.md", "src/lib.rs"] {
        assert_eq!(
            Gix.tree(&f.bare, &f.first, path, usize::MAX),
            Err(Error::NotFound),
            "{path}"
        );
    }
    assert_eq!(
        Gix.tree(&f.bare, &f.second, "src", usize::MAX),
        Err(Error::NotFound)
    );
}

#[test]
fn blob_reads_file_content() {
    let f = fixture("blob");
    assert_eq!(
        Gix.blob(&f.bare, &f.first, "README.md", u64::MAX).unwrap(),
        b"hello\n"
    );
    assert_eq!(
        Gix.blob(&f.bare, &f.second, "README.md", u64::MAX).unwrap(),
        b"hello again\n"
    );
    assert_eq!(
        Gix.blob(&f.bare, &f.third, "README.md", u64::MAX).unwrap(),
        b"feature\n"
    );
    assert_eq!(
        Gix.blob(&f.bare, &f.second, "docs/a.md", u64::MAX).unwrap(),
        b"a\n"
    );
    assert_eq!(
        Gix.blob(&f.bare, &f.first, "link", u64::MAX).unwrap(),
        b"README.md"
    );
    for path in ["src", "missing", "docs/a.md", "src/lib.rs/x", ""] {
        assert_eq!(
            Gix.blob(&f.bare, &f.first, path, u64::MAX),
            Err(Error::NotFound),
            "{path:?}"
        );
    }
}

#[test]
fn unknown_commits_are_not_found() {
    let f = fixture("unknown");
    let tree_id = git(
        &f.bare,
        &["rev-parse", &format!("{}^{{tree}}", f.first)],
        "0 +0000",
    );
    for id in [
        "0".repeat(40),
        "main".into(),
        "".into(),
        f.first[..7].into(),
        tree_id,
    ] {
        assert_eq!(
            Gix.tree(&f.bare, &id, "", usize::MAX),
            Err(Error::NotFound),
            "{id:?}"
        );
        assert_eq!(
            Gix.log(&f.bare, &id, 0, 1, usize::MAX),
            Err(Error::NotFound),
            "{id:?}"
        );
    }
}

#[test]
fn log_walks_newest_first() {
    let f = fixture("log");
    let log = Gix.log(&f.bare, &f.third, 0, 10, usize::MAX).unwrap();
    let ids: Vec<&str> = log.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids, [&f.third, &f.second, &f.first]);
    assert_eq!(log[1].parents, std::slice::from_ref(&f.first));
    assert!(log[2].parents.is_empty());
    assert_eq!(log[1].author_name, "Alice");
    assert_eq!(log[1].author_email, "alice@example.org");
    assert_eq!(log[1].time, 1_700_000_100);
    assert_eq!(log[1].message, "second\n\nbody\n");
    assert_eq!(
        Gix.log(&f.bare, &f.third, 0, 1, usize::MAX).unwrap().len(),
        1
    );
    assert_eq!(
        Gix.log(&f.bare, &f.first, 0, 10, usize::MAX).unwrap().len(),
        1
    );
    let skipped = Gix.log(&f.bare, &f.third, 1, 1, usize::MAX).unwrap();
    assert_eq!(skipped[0].id, f.second);
    assert!(
        Gix.log(&f.bare, &f.third, 3, 10, usize::MAX)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn diff_lists_changed_files() {
    let f = fixture("diff");
    let changes = Gix
        .diff(&f.bare, Some(&f.first), &f.second, usize::MAX, usize::MAX)
        .unwrap();
    let summary: Vec<(&str, ChangeKind)> =
        changes.iter().map(|c| (c.path.as_str(), c.kind)).collect();
    assert_eq!(
        summary,
        [
            ("README.md", ChangeKind::Modified),
            ("docs/a.md", ChangeKind::Added),
            ("src/lib.rs", ChangeKind::Deleted),
        ]
    );
    let readme = &changes[0];
    let blob = |rev: &str| {
        git(
            &f.bare,
            &["rev-parse", &format!("{rev}:README.md")],
            "0 +0000",
        )
    };
    assert_eq!(readme.old_id, Some(blob(&f.first)));
    assert_eq!(readme.new_id, Some(blob(&f.second)));
    assert_eq!(changes[1].old_id, None);
    assert_eq!(changes[2].new_id, None);

    let root = Gix
        .diff(&f.bare, None, &f.first, usize::MAX, usize::MAX)
        .unwrap();
    assert!(root.iter().all(|c| c.kind == ChangeKind::Added));
    assert_eq!(root.len(), 4);
    assert_eq!(
        Gix.diff(&f.bare, Some(&f.first), &f.first, usize::MAX, usize::MAX)
            .unwrap(),
        []
    );
}

#[test]
fn transport_spawns_git_upload_pack() {
    let f = fixture("transport");
    let output = Gix
        .transport(
            Service::UploadPack,
            &f.bare,
            &["--stateless-rpc", "--advertise-refs"],
            None,
        )
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let advertised = String::from_utf8_lossy(&output.stdout);
    assert!(
        advertised.contains(&format!("{} refs/heads/main", f.second)),
        "{advertised}"
    );
    assert!(advertised.contains("refs/heads/feature/x"), "{advertised}");
}

#[test]
fn revision_limits_are_checked_before_opening_a_repository() {
    for spec in [
        "a".repeat(MAX_REVISION_BYTES),
        "é".repeat(MAX_REVISION_BYTES / 2),
        vec!["a"; MAX_REVISION_COMPONENTS].join("/"),
    ] {
        assert_eq!(check_revision(&spec), Ok(()));
    }
    for spec in [
        "a".repeat(MAX_REVISION_BYTES + 1),
        "é".repeat(MAX_REVISION_BYTES / 2 + 1),
        vec!["a"; MAX_REVISION_COMPONENTS + 1].join("/"),
        "/".repeat(1 << 20),
    ] {
        assert_eq!(
            Gix.resolve(Path::new("/missing-revision-test"), &spec),
            Err(Error::TooLarge(Limit::Revision))
        );
    }
}

#[test]
fn revision_lookup_is_exact_and_bounded() {
    let dir = scratch("revision-budget");
    let repo = gix::init_bare(&dir).unwrap();
    let signature =
        gix::actor::SignatureRef::from_bytes(b"Alice <alice@example.org> 1700000000 +0000")
            .unwrap();
    let id = repo
        .commit_as(
            signature,
            signature,
            "refs/heads/main",
            "test",
            repo.empty_tree().id,
            Vec::<gix::ObjectId>::new(),
        )
        .unwrap()
        .to_string();
    // Packed refs can contain overlapping names, unlike loose-ref directories.
    fs::write(dir.join("packed-refs"), format!(
        "# pack-refs with: sorted\n{id} refs/heads/feature\n{id} refs/heads/feature/x\n{id} refs/tags/feature/x/deeper\n{id} refs/tags/main/path\n{id} refs/tags/refs/heads/shadow\n{id} refs/tags/v1\n"
    )).unwrap();
    fs::write(dir.join("refs/heads/alias"), "ref: refs/heads/main\n").unwrap();
    for (spec, path) in [
        ("feature/x/deeper/file", "deeper/file"),
        ("feature/other", "other"),
        ("main/path", "path"),
        ("alias/file", "file"),
        ("v1/file", "file"),
        ("main/", ""),
        (id.as_str(), ""),
    ] {
        assert_eq!(Gix.resolve(&dir, spec), Ok((id.clone(), path)), "{spec}");
    }
    for spec in [
        "shadow",
        "HEAD",
        "heads/main",
        "refs/heads/main",
        "",
        "/main",
    ] {
        assert_eq!(Gix.resolve(&dir, spec), Err(Error::NotFound), "{spec}");
    }
    let path = vec!["a"; MAX_REVISION_COMPONENTS - 1].join("/");
    assert_eq!(
        Gix.resolve(&dir, &format!("main/{path}")),
        Ok((id.clone(), path.as_str()))
    );
    let path = "a".repeat(MAX_REVISION_BYTES - "main/".len());
    assert_eq!(
        Gix.resolve(&dir, &format!("main/{path}")),
        Ok((id, path.as_str()))
    );
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn resolve_prefers_the_longest_branch_then_tag_then_commit_id() {
    let f = fixture("resolve");
    // A tag with a branch's name loses to the branch.
    git(&f.bare, &["tag", "feature/x", &f.first], "0 +0000");
    let resolve = |spec: &'static str| Gix.resolve(&f.bare, spec);
    for (spec, commit, path) in [
        ("feature/x/README.md", &f.third, "README.md"),
        ("feature/x", &f.third, ""),
        ("main/docs/a.md", &f.second, "docs/a.md"),
        ("main", &f.second, ""),
        ("main/", &f.second, ""),
        ("v1/README.md", &f.first, "README.md"),
        ("light/src/lib.rs", &f.second, "src/lib.rs"),
    ] {
        assert_eq!(resolve(spec), Ok((commit.clone(), path)), "{spec}");
    }
    let by_id = format!("{}/src/lib.rs", f.first);
    assert_eq!(
        Gix.resolve(&f.bare, &by_id),
        Ok((f.first.clone(), "src/lib.rs"))
    );
    assert_eq!(Gix.resolve(&f.bare, &f.third), Ok((f.third.clone(), "")));
}

#[test]
fn resolve_refuses_unknown_refs() {
    let f = fixture("resolve-unknown");
    let tree_id = git(
        &f.bare,
        &["rev-parse", &format!("{}^{{tree}}", f.first)],
        "0 +0000",
    );
    let short = format!("{}/README.md", &f.first[..7]);
    let zero = format!("{}/README.md", "0".repeat(40));
    for spec in [
        "",
        "/main",
        "nope/README.md",
        "feature/README.md",
        "x/main/README.md",
        "heads/main",
        "refs/heads/main",
        "HEAD",
        &short,
        &zero,
        &tree_id,
    ] {
        assert_eq!(Gix.resolve(&f.bare, spec), Err(Error::NotFound), "{spec:?}");
    }
}

#[test]
fn blob_checks_header_size_before_loading() {
    let f = fixture("blob-limit");
    let data = Gix.blob(&f.bare, &f.first, "README.md", u64::MAX).unwrap();
    let size = data.len() as u64;
    assert!(size > 0);
    for packed in [false, true] {
        if packed {
            git(&f.bare, &["gc", "--prune=now"], "1700000000 +0000");
        }
        assert_eq!(
            Gix.blob(&f.bare, &f.first, "README.md", size).unwrap(),
            data
        );
        assert_eq!(
            Gix.blob(&f.bare, &f.first, "README.md", size - 1),
            Err(Error::TooLarge(Limit::Blob))
        );
    }
}

/// `git mktree` over `listing`, in `dir`.
fn mktree(dir: &Path, listing: &str) -> String {
    use std::io::Write;
    let mut child = Command::new("git")
        .current_dir(dir)
        .arg("mktree")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(listing.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

/// Cuts a loose object after its header: its kind and size stay readable, reading it
/// whole fails with `Error::Git`, so a read that still happens shows.
fn truncate_loose(bare: &Path, id: &str) {
    use std::os::unix::fs::PermissionsExt;
    let path = bare.join(format!("objects/{}/{}", &id[..2], &id[2..]));
    let bytes = std::fs::read(&path).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    std::fs::write(&path, &bytes[..64]).unwrap();
    let repo = Gix.open(bare).unwrap();
    let id = gix::ObjectId::from_hex(id.as_bytes()).unwrap();
    assert!(repo.find_header(id).is_ok() && repo.find_object(id).is_err());
}

#[test]
fn non_commit_revisions_are_refused_on_their_header_without_a_read() {
    let f = fixture("header");
    let t = "0 +0000";
    std::fs::write(
        f.bare.join("big"),
        "a".repeat(MAX_COMMIT_OBJECT_BYTES as usize + 1),
    )
    .unwrap();
    let blob = git(&f.bare, &["hash-object", "-w", "big"], t);
    git(&f.bare, &["update-ref", "refs/tags/big", &blob], t);
    truncate_loose(&f.bare, &blob);
    assert_eq!(
        Gix.tree(&f.bare, &blob, "", usize::MAX),
        Err(Error::NotFound)
    );
    assert_eq!(
        Gix.log(&f.bare, &blob, 0, 1, usize::MAX),
        Err(Error::NotFound)
    );
    assert_eq!(
        Gix.diff(&f.bare, None, &blob, usize::MAX, usize::MAX),
        Err(Error::NotFound)
    );
    assert_eq!(Gix.resolve(&f.bare, &blob), Err(Error::NotFound));
    // A ref whose target is a blob peels to no commit.
    assert_eq!(Gix.resolve(&f.bare, "big"), Err(Error::NotFound));
}

#[test]
fn oversized_tree_and_commit_objects_are_refused_before_they_are_read() {
    let f = fixture("caps");
    let t = "1700000300 +0000";
    let blob = git(
        &f.bare,
        &["rev-parse", &format!("{}:README.md", f.first)],
        t,
    );
    // Past the tree cap: 21 000 entries with 200-byte names.
    let listing: String = (0..21_000)
        .map(|i| format!("100644 blob {blob}\t{i:0>200}\n"))
        .collect();
    let big_tree = mktree(&f.bare, &listing);
    let root = mktree(
        &f.bare,
        &format!("100644 blob {blob}\tREADME.md\n040000 tree {big_tree}\tbig\n"),
    );
    let at_root = git(&f.bare, &["commit-tree", &big_tree, "-m", "big root"], t);
    let under = git(&f.bare, &["commit-tree", &root, "-m", "big subtree"], t);
    truncate_loose(&f.bare, &big_tree);
    let tree = Error::TooLarge(Limit::Tree);
    assert_eq!(
        Gix.tree(&f.bare, &at_root, "", usize::MAX).unwrap_err(),
        tree
    );
    assert_eq!(
        Gix.tree(&f.bare, &under, "big", usize::MAX).unwrap_err(),
        tree
    );
    assert_eq!(
        Gix.blob(&f.bare, &under, "big/x", u64::MAX).unwrap_err(),
        tree
    );
    assert_eq!(
        Gix.diff(&f.bare, Some(&f.first), &under, usize::MAX, usize::MAX)
            .unwrap_err(),
        tree
    );
    assert_eq!(Gix.tree(&f.bare, &under, "", usize::MAX).unwrap().len(), 2);

    let message = f.bare.join("message");
    std::fs::write(&message, "m".repeat(MAX_COMMIT_OBJECT_BYTES as usize + 1)).unwrap();
    let message = message.to_str().unwrap();
    let huge = git(&f.bare, &["commit-tree", &root, "-F", message], t);
    let child = git(
        &f.bare,
        &["commit-tree", &root, "-p", &huge, "-m", "child"],
        t,
    );
    git(&f.bare, &["update-ref", "refs/tags/huge", &huge], t);
    truncate_loose(&f.bare, &huge);
    let commit = Error::TooLarge(Limit::Commit);
    // An ancestor beyond the page is not read; one the walk reaches, returned or
    // passed over by `skip`, is refused on its header (ADR 0051).
    assert_eq!(Gix.log(&f.bare, &child, 0, 1, usize::MAX).unwrap().len(), 1);
    assert_eq!(
        Gix.log(&f.bare, &child, 0, 2, usize::MAX).unwrap_err(),
        commit
    );
    assert_eq!(
        Gix.log(&f.bare, &child, 1, 1, usize::MAX).unwrap_err(),
        commit
    );
    // Named directly or through a tag: refused on its header.
    assert_eq!(
        Gix.tree(&f.bare, &huge, "", usize::MAX).unwrap_err(),
        commit
    );
    assert_eq!(
        Gix.log(&f.bare, &huge, 0, 1, usize::MAX).unwrap_err(),
        commit
    );
    assert_eq!(Gix.resolve(&f.bare, &huge).unwrap_err(), commit);
    assert_eq!(Gix.resolve(&f.bare, "huge").unwrap_err(), commit);
}

#[test]
fn listing_limits_stop_trees_diffs_and_pages() {
    let f = fixture("limits");
    let tree = Error::TooLarge(Limit::Tree);
    assert_eq!(Gix.tree(&f.bare, &f.first, "", 4).unwrap().len(), 4);
    assert_eq!(Gix.tree(&f.bare, &f.first, "", 3).unwrap_err(), tree);
    // Four files added: README.md, link, run.sh, src/lib.rs, 29 bytes of paths.
    let diff = Error::TooLarge(Limit::Diff);
    assert_eq!(Gix.diff(&f.bare, None, &f.first, 4, 29).unwrap().len(), 4);
    assert_eq!(Gix.diff(&f.bare, None, &f.first, 3, 29).unwrap_err(), diff);
    assert_eq!(Gix.diff(&f.bare, None, &f.first, 4, 28).unwrap_err(), diff);
    // A page ends after the commit that takes its page bytes past the budget.
    let newest = Gix.log(&f.bare, &f.third, 0, 1, usize::MAX).unwrap()[0].page_bytes();
    assert_eq!(Gix.log(&f.bare, &f.third, 0, 10, 0).unwrap().len(), 1);
    assert_eq!(
        Gix.log(&f.bare, &f.third, 0, 10, newest - 1).unwrap().len(),
        1
    );
    assert_eq!(Gix.log(&f.bare, &f.third, 0, 10, newest).unwrap().len(), 2);
}

#[test]
fn commit_object_cap_is_checked_on_the_exact_size_of_every_ancestor() {
    let f = fixture("exact");
    let t = "1700000300 +0000";
    let tree = git(&f.bare, &["rev-parse", &format!("{}^{{tree}}", f.first)], t);
    let with_message = |bytes: usize| {
        let path = f.bare.join("message");
        std::fs::write(&path, "m".repeat(bytes)).unwrap();
        git(
            &f.bare,
            &["commit-tree", &tree, "-F", path.to_str().unwrap()],
            t,
        )
    };
    // The object is the message plus a fixed header: measure it, then hit the cap.
    let probe = with_message(100);
    let overhead: usize = git(&f.bare, &["cat-file", "-s", &probe], t)
        .parse::<usize>()
        .unwrap()
        - 100;
    let cap = MAX_COMMIT_OBJECT_BYTES as usize;
    let at_cap = with_message(cap - overhead);
    let over_cap = with_message(cap + 1 - overhead);
    let size = |id: &str| {
        git(&f.bare, &["cat-file", "-s", id], t)
            .parse::<u64>()
            .unwrap()
    };
    assert_eq!(size(&at_cap), MAX_COMMIT_OBJECT_BYTES);
    assert_eq!(size(&over_cap), MAX_COMMIT_OBJECT_BYTES + 1);
    let child_of = |parent: &str| {
        git(
            &f.bare,
            &["commit-tree", &tree, "-p", parent, "-m", "child"],
            t,
        )
    };
    let fine = child_of(&at_cap);
    let bad = child_of(&over_cap);
    assert_eq!(Gix.log(&f.bare, &fine, 0, 2, usize::MAX).unwrap().len(), 2);
    assert_eq!(
        Gix.log(&f.bare, &bad, 0, 2, usize::MAX).unwrap_err(),
        Error::TooLarge(Limit::Commit)
    );
}

#[test]
fn log_follows_merges_in_commit_time_order_and_pages_without_gaps() {
    let f = fixture("merge");
    let tree = git(
        &f.bare,
        &["rev-parse", &format!("{}^{{tree}}", f.first)],
        "0 +0000",
    );
    let side = git(
        &f.bare,
        &["commit-tree", &tree, "-p", &f.first, "-m", "side"],
        "1700000150 +0000",
    );
    let merge = git(
        &f.bare,
        &[
            "commit-tree",
            &tree,
            "-p",
            &f.third,
            "-p",
            &side,
            "-m",
            "merge",
        ],
        "1700000300 +0000",
    );
    let expected = git(&f.bare, &["rev-list", "--date-order", &merge], "0 +0000");
    let expected: Vec<&str> = expected.lines().collect();
    assert_eq!(expected, [&merge, &f.third, &side, &f.second, &f.first]);
    let log = Gix.log(&f.bare, &merge, 0, 10, usize::MAX).unwrap();
    let ids: Vec<&str> = log.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids, expected);
    assert_eq!(log[0].parents, [f.third.clone(), side.clone()]);
    // Pages of two, by skip, cover the history once and in the same order.
    let paged: Vec<String> = (0..3)
        .flat_map(|page| Gix.log(&f.bare, &merge, page * 2, 2, usize::MAX).unwrap())
        .map(|c| c.id)
        .collect();
    assert_eq!(paged, expected);
}

/// Writes a commit object directly, so its author name has no command-line bound.
fn raw_commit(bare: &Path, tree: &str, parents: &[&str], author: &str, time: u64) -> String {
    let mut content = format!("tree {tree}\n");
    for parent in parents {
        content += &format!("parent {parent}\n");
    }
    content += &format!(
        "author {author} <alice@example.org> {time} +0000\ncommitter Alice <alice@example.org> {time} +0000\n\nm\n"
    );
    let path = bare.join("raw-commit");
    std::fs::write(&path, content).unwrap();
    git(
        bare,
        &["hash-object", "-t", "commit", "-w", path.to_str().unwrap()],
        "0 +0000",
    )
}

#[test]
fn page_budget_counts_author_fields_and_parents() {
    let f = fixture("budget");
    let tree = git(
        &f.bare,
        &["rev-parse", &format!("{}^{{tree}}", f.first)],
        "0 +0000",
    );
    let octopus = raw_commit(
        &f.bare,
        &tree,
        &[&f.third, &f.second, &f.first],
        "Alice",
        1_700_000_300,
    );
    let wide = raw_commit(
        &f.bare,
        &tree,
        &[&f.first],
        &"A".repeat(5000),
        1_700_000_400,
    );
    let one = |tip: &str| Gix.log(&f.bare, tip, 0, 1, usize::MAX).unwrap().remove(0);
    let (octopus_bytes, wide_bytes) = (one(&octopus).page_bytes(), one(&wide).page_bytes());
    // Three parents and the 5000-byte name count, with messages of two bytes.
    assert_eq!(
        octopus_bytes,
        2 + "Alice".len() + "alice@example.org".len() + 3 * 40
    );
    assert_eq!(wide_bytes, 2 + 5000 + "alice@example.org".len() + 40);
    for (tip, bytes) in [(&octopus, octopus_bytes), (&wide, wide_bytes)] {
        // Exact boundary: the page ends after the commit that goes past the budget.
        assert_eq!(Gix.log(&f.bare, tip, 0, 10, bytes - 1).unwrap().len(), 1);
        assert_eq!(Gix.log(&f.bare, tip, 0, 10, bytes).unwrap().len(), 2);
    }
    assert_eq!(Gix.log(&f.bare, &wide, 0, 10, 4000).unwrap().len(), 1);
}

#[test]
fn shared_repository_trust_is_scoped_to_configured_canonical_ulid_paths() {
    let root = scratch("trust");
    let repos = root.join("repos");
    fs::create_dir(&repos).unwrap();
    let id = "00000000000000000000000001.git";
    let own = repos.join(id);
    let outside = root.join(id);
    Gix.init_bare(&own, "main").unwrap();
    Gix.init_bare(&outside, "main").unwrap();
    let shared = Gix::shared(&repos);
    assert!(shared.trusted(&own).is_some());
    assert!(shared.trusted(&outside).is_none());
    assert!(Gix.trusted(&own).is_none());
    let alias = repos.join("00000000000000000000000002.git");
    std::os::unix::fs::symlink(&outside, &alias).unwrap();
    assert!(shared.trusted(&alias).is_none());
    let unrelated = repos.join("not-a-public-id.git");
    Gix.init_bare(&unrelated, "main").unwrap();
    assert!(shared.trusted(&unrelated).is_none());
    for path in [&own, &outside, &alias, &unrelated] {
        let command = shared.transport(
            Service::UploadPack,
            path,
            &["--stateless-rpc", "--advertise-refs"],
            None,
        );
        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        let trusted: Vec<_> = args
            .iter()
            .filter(|arg| arg.starts_with("safe.directory="))
            .collect();
        if path == &own {
            assert_eq!(
                trusted,
                [&format!(
                    "safe.directory={}",
                    own.canonicalize().unwrap().display()
                )]
            );
            assert!(shared.refs(path).unwrap().is_empty());
        } else {
            assert!(trusted.is_empty(), "{args:?}");
        }
    }
    fs::write(own.join("commondir"), outside.to_str().unwrap()).unwrap();
    assert!(shared.trusted(&own).is_none());
}
