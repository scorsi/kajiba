use super::*;
use std::path::PathBuf;

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

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kajiba-git-{name}-{}", std::process::id()));
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
    assert_eq!(Gix.refs(&bare).unwrap(), []);
    assert!(Gix.init_bare(&dir.join("bad.git"), "a..b").is_err());
    assert!(!dir.join("bad.git").exists());
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
    assert!(Gix.set_head(&dir.join("missing.git"), "main").is_err());
}

#[test]
fn import_commands_keep_credentials_out_of_the_command_line() {
    let (command, url) = import_command("https://alice:s%40cret@example.org/demo.git?x=1");
    assert_eq!(url, "https://example.org/demo.git?x=1");
    let env: std::collections::BTreeMap<_, _> = command
        .get_envs()
        .map(|(k, v)| {
            (
                k.to_str().unwrap(),
                v.and_then(|v| v.to_str()).unwrap_or(""),
            )
        })
        .collect();
    assert_eq!(env["KAJIBA_IMPORT_USERNAME"], "alice");
    assert_eq!(env["KAJIBA_IMPORT_PASSWORD"], "s@cret");
    assert_eq!(env["GIT_ALLOW_PROTOCOL"], "http:https:ssh:git");
    assert!(env["GIT_CONFIG_VALUE_3"].starts_with("!f() { printf 'username=%s"));
    assert_eq!(env["GIT_CONFIG_KEY_0"], "core.hooksPath");
    assert!(env.contains_key("PATH"), "{env:?}");
    // No credentials, a local path: nothing to pass, `file` allowed.
    let (command, url) = import_command("/srv/source.git");
    let env: Vec<_> = command
        .get_envs()
        .map(|(k, _)| k.to_str().unwrap())
        .collect();
    assert_eq!(url, "/srv/source.git");
    assert_eq!(env.iter().filter(|k| k.starts_with("KAJIBA_")).count(), 0);
    assert_eq!(
        command
            .get_envs()
            .find(|(k, _)| *k == "GIT_ALLOW_PROTOCOL")
            .and_then(|(_, v)| v)
            .unwrap(),
        "file:http:https:ssh:git"
    );
    // An ssh user is kept: it is not a secret and ssh has no credential helper.
    assert_eq!(
        split_userinfo("ssh://git@example.org/demo.git"),
        ("ssh://git@example.org/demo.git".to_owned(), None)
    );
}

#[test]
fn run_kills_a_child_past_its_deadline() {
    let mut command = Command::new("sh");
    command
        .args(["-c", "exec sleep 30"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let started = std::time::Instant::now();
    let err = run(command, "", Duration::from_millis(200)).unwrap_err();
    assert!(err.to_string().contains("did not finish"), "{err}");
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
    let root = Gix.tree(&f.bare, &f.first, "").unwrap();
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
    let src = Gix.tree(&f.bare, &f.first, "src").unwrap();
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
            Gix.tree(&f.bare, &f.first, path),
            Err(Error::NotFound),
            "{path}"
        );
    }
    assert_eq!(Gix.tree(&f.bare, &f.second, "src"), Err(Error::NotFound));
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
        assert_eq!(Gix.tree(&f.bare, &id, ""), Err(Error::NotFound), "{id:?}");
        assert_eq!(Gix.log(&f.bare, &id, 1), Err(Error::NotFound), "{id:?}");
    }
}

#[test]
fn log_walks_newest_first() {
    let f = fixture("log");
    let log = Gix.log(&f.bare, &f.third, 10).unwrap();
    let ids: Vec<&str> = log.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids, [&f.third, &f.second, &f.first]);
    assert_eq!(log[1].parents, std::slice::from_ref(&f.first));
    assert!(log[2].parents.is_empty());
    assert_eq!(log[1].author_name, "Alice");
    assert_eq!(log[1].author_email, "alice@example.org");
    assert_eq!(log[1].time, 1_700_000_100);
    assert_eq!(log[1].message, "second\n\nbody\n");
    assert_eq!(Gix.log(&f.bare, &f.third, 1).unwrap().len(), 1);
    assert_eq!(Gix.log(&f.bare, &f.first, 10).unwrap().len(), 1);
}

#[test]
fn diff_lists_changed_files() {
    let f = fixture("diff");
    let changes = Gix.diff(&f.bare, Some(&f.first), &f.second).unwrap();
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

    let root = Gix.diff(&f.bare, None, &f.first).unwrap();
    assert!(root.iter().all(|c| c.kind == ChangeKind::Added));
    assert_eq!(root.len(), 4);
    assert_eq!(Gix.diff(&f.bare, Some(&f.first), &f.first).unwrap(), []);
}

#[test]
fn transport_spawns_git_upload_pack() {
    let f = fixture("transport");
    let output = Gix
        .transport(
            Service::UploadPack,
            &f.bare,
            &["--stateless-rpc", "--advertise-refs"],
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
    let command = Gix.transport(Service::ReceivePack, &f.bare, &[]);
    assert_eq!(
        command.get_args().collect::<Vec<_>>(),
        [std::ffi::OsStr::new("receive-pack"), f.bare.as_os_str()]
    );
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
            Err(Error::TooLarge)
        );
    }
}
