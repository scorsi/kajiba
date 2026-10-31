use super::*;
use crate::ci::{self, Command as Ci};
use crate::client::{
    connect,
    tests::{Fake, TOKEN, TempDir, refusal},
};
use kajiba_proto::v1::RunState;
use std::{process::ExitCode, sync::OnceLock};
use tonic::Code;

const FORGE: &str = "https://forge.example.org";
const RUN: &str = "01JBRUN0000000000000000000";

/// A fake `nix` answering `["kvm","big-parallel","kvm"]` and writing its arguments next
/// to the evaluated clone, as `<clone>.nix-args`. Written once: a script written while
/// another test forks can fail to exec (`ETXTBSY`).
fn nix() -> &'static Path {
    static NIX: OnceLock<PathBuf> = OnceLock::new();
    NIX.get_or_init(|| {
        let path = std::env::temp_dir().join(format!("kj-cli-nix-{}", std::process::id()));
        std::fs::write(
            &path,
            "#!/bin/sh\ntop=${3#git+file://}; top=${top%%#*}\nprintf '%s\\n' \"$@\" > \"$top.nix-args\"\necho '[\"kvm\",\"big-parallel\",\"kvm\"]'\n",
        )
        .unwrap();
        std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        path
    })
}

fn run_git(dir: &Path, args: &[&str]) -> String {
    git(dir, args, None).unwrap()
}

/// `origin.git` with commits `first` then `second` on main; `work`, a clone whose
/// `origin` remote addresses `alice/demo` on the forge, at `second`.
struct Clone {
    dir: TempDir,
    work: PathBuf,
    first: String,
    second: String,
}

fn clone() -> Clone {
    let dir = TempDir::new();
    let work = dir.0.join("work");
    std::fs::create_dir(&work).unwrap();
    run_git(&work, &["init", "-q", "-b", "main"]);
    run_git(&work, &["config", "user.name", "Alice"]);
    run_git(&work, &["config", "user.email", "alice@example.org"]);
    std::fs::write(work.join("tracked"), "1\n").unwrap();
    run_git(&work, &["add", "tracked"]);
    run_git(&work, &["commit", "-q", "-m", "first"]);
    let first = run_git(&work, &["rev-parse", "HEAD"]);
    std::fs::write(work.join("tracked"), "2\n").unwrap();
    run_git(&work, &["commit", "-q", "-am", "second"]);
    let second = run_git(&work, &["rev-parse", "HEAD"]);
    let origin = dir.0.join("origin.git");
    run_git(
        &dir.0,
        &[
            "clone",
            "-q",
            "--bare",
            work.to_str().unwrap(),
            origin.to_str().unwrap(),
        ],
    );
    run_git(
        &work,
        &[
            "remote",
            "add",
            "origin",
            &format!("{FORGE}/alice/demo.git"),
        ],
    );
    Clone {
        dir,
        work,
        first,
        second,
    }
}

impl Clone {
    fn local(&self) -> Local {
        Local {
            forge: FORGE.into(),
            nix: nix().into(),
            dir: self.work.clone(),
        }
    }

    /// Fetch `bundle` into a copy of the forge's repository; its head, or the error.
    fn apply(&self, bundle: &[u8]) -> Result<String, Error> {
        let copy = self.dir.0.join("applied.git");
        let _ = std::fs::remove_dir_all(&copy);
        let origin = self.dir.0.join("origin.git");
        run_git(
            &self.dir.0,
            &[
                "clone",
                "-q",
                "--bare",
                origin.to_str().unwrap(),
                copy.to_str().unwrap(),
            ],
        );
        let path = self.dir.0.join("upload.bundle");
        std::fs::write(&path, bundle).unwrap();
        git(
            &copy,
            &[
                "fetch",
                "-q",
                path.to_str().unwrap(),
                "+refs/*:refs/fetched/*",
            ],
            None,
        )?;
        Ok(run_git(
            &copy,
            &["for-each-ref", "--format=%(objectname)", "refs/fetched/"],
        ))
    }

    /// No ref under `refs/kajiba/` and no stash entry is left in the clone.
    fn untouched(&self) {
        assert_eq!(run_git(&self.work, &["for-each-ref", "refs/kajiba/"]), "");
        assert_eq!(run_git(&self.work, &["stash", "list"]), "");
    }
}

/// The fake forge: the ids of origin's refs as base, `run` answered to the upload.
async fn forge(base: &[&String]) -> (Fake, String) {
    let fake = Fake::default();
    {
        let mut state = fake.0.lock().unwrap();
        state.base = base.iter().map(|id| id.to_string()).collect();
        state.run = Some(Run {
            public_id: RUN.into(),
            state: RunState::Queued.into(),
            ..Default::default()
        });
    }
    let url = fake.serve().await;
    (fake, url)
}

fn choice(checks: &[&str]) -> Choice {
    Choice {
        repo: None,
        checks: checks.iter().map(|check| check.to_string()).collect(),
        system: None,
    }
}

async fn create_with(fake_url: &str, clone: &Clone, choice: Choice) -> Result<Run, Error> {
    let mut runs = RunServiceClient::new(connect(fake_url, TOKEN, true).unwrap());
    create(&mut runs, &clone.local(), choice).await
}

/// The bundle's header, up to its pack.
fn header(bundle: &[u8]) -> String {
    let end = bundle.windows(5).position(|w| w == b"\nPACK").unwrap() + 1;
    String::from_utf8(bundle[..end].to_vec()).unwrap()
}

#[tokio::test]
async fn a_dirty_tree_sends_its_tracked_files_over_the_forge_base() {
    let clone = clone();
    std::fs::write(clone.work.join("tracked"), "dirty\n").unwrap();
    std::fs::write(clone.work.join("staged"), "new\n").unwrap();
    run_git(&clone.work, &["add", "staged"]);
    std::fs::write(clone.work.join("untracked"), "never\n").unwrap();
    // `first` is on the forge but `unknown` is not here: only `first` is excluded.
    let unknown = "f".repeat(40);
    let (fake, url) = forge(&[&clone.first, &unknown]).await;
    let run = create_with(&url, &clone, choice(&["a", "vm-test_1"]))
        .await
        .unwrap();
    assert_eq!(run.public_id, RUN);
    clone.untouched();
    let state = fake.0.lock().unwrap();
    let spec = &state.specs[0];
    assert_eq!(
        (
            spec.repo.as_str(),
            spec.system.as_str(),
            &spec.checks,
            &spec.features
        ),
        (
            "alice/demo",
            own_system().as_str(),
            &vec!["a".to_string(), "vm-test_1".into()],
            &vec!["big-parallel".to_string(), "kvm".into()]
        )
    );
    let (sent, sizes, bundle) = &state.uploads[0];
    assert_eq!(sent.as_ref(), Some(spec));
    assert!(sizes.iter().all(|size| *size <= 16_000), "{sizes:?}");
    // The minimal bundle: everything since `first`, which the forge has.
    assert!(header(bundle).starts_with(&format!("# v2 git bundle\n-{} ", clone.first)));
    let captured = clone.apply(bundle).unwrap();
    let copy = clone.dir.0.join("applied.git");
    assert_eq!(
        run_git(&copy, &["show", &format!("{captured}:tracked")]),
        "dirty"
    );
    assert_eq!(
        run_git(&copy, &["show", &format!("{captured}:staged")]),
        "new"
    );
    assert!(
        git(
            &copy,
            &["cat-file", "-e", &format!("{captured}:untracked")],
            None
        )
        .is_err()
    );
    // The evaluation selected the checks of the system, from the clone.
    let args = std::fs::read_to_string(clone.dir.0.join("work.nix-args")).unwrap();
    assert!(
        args.contains(&format!(
            "git+file://{}#checks.{}\n",
            clone.work.canonicalize().unwrap().display(),
            own_system()
        )),
        "{args}"
    );
    assert!(args.contains(r#"[ "a" "vm-test_1" ]"#), "{args}");
}

#[tokio::test]
async fn a_clean_tree_sends_head_alone_when_the_forge_has_it() {
    let clone = clone();
    std::fs::write(clone.work.join("untracked"), "never\n").unwrap();
    let (fake, url) = forge(&[&clone.second]).await;
    create_with(&url, &clone, choice(&[])).await.unwrap();
    clone.untouched();
    let state = fake.0.lock().unwrap();
    let bundle = &state.uploads[0].2;
    assert!(header(bundle).starts_with(&format!("# v2 git bundle\n-{} ", clone.first)));
    assert_eq!(clone.apply(bundle).unwrap(), clone.second);
    let args = std::fs::read_to_string(clone.dir.0.join("work.nix-args")).unwrap();
    assert!(args.contains("builtins.attrNames checks"), "{args}");
}

#[tokio::test]
async fn without_a_shared_id_the_whole_history_goes() {
    let clone = clone();
    let (fake, url) = forge(&[]).await;
    create_with(&url, &clone, choice(&[])).await.unwrap();
    let state = fake.0.lock().unwrap();
    let bundle = &state.uploads[0].2;
    assert!(!header(bundle).contains("\n-"), "{}", header(bundle));
}

#[tokio::test]
async fn an_unborn_head_is_refused_before_any_call() {
    let clone = clone();
    let unborn = clone.dir.0.join("unborn");
    std::fs::create_dir(&unborn).unwrap();
    run_git(&unborn, &["init", "-q"]);
    let (fake, url) = forge(&[]).await;
    let mut runs = RunServiceClient::new(connect(&url, TOKEN, true).unwrap());
    let local = Local {
        dir: unborn,
        ..clone.local()
    };
    let choice = Choice {
        repo: Some("alice/demo".into()),
        ..choice(&[])
    };
    let err = create(&mut runs, &local, choice).await.unwrap_err();
    assert!(err.0.contains("HEAD has no commit"), "{err}");
    assert!(fake.0.lock().unwrap().calls.is_empty());
}

#[tokio::test]
async fn the_repository_comes_from_the_single_forge_remote_or_repo() {
    let clone = clone();
    let (fake, url) = forge(&[&clone.second]).await;
    // Another forge's remote and the SSH form of the same repository change nothing.
    run_git(
        &clone.work,
        &[
            "remote",
            "add",
            "elsewhere",
            "https://other.example.org/bob/x.git",
        ],
    );
    run_git(
        &clone.work,
        &[
            "remote",
            "add",
            "ssh",
            "kajiba-git@forge.example.org:Alice/Demo",
        ],
    );
    create_with(&url, &clone, choice(&[])).await.unwrap();
    // Two repositories of the forge: refused, naming both, unless --repo picks one.
    run_git(
        &clone.work,
        &[
            "remote",
            "add",
            "fork",
            "ssh://kajiba-git@forge.example.org:2222/bob/demo.git",
        ],
    );
    let err = create_with(&url, &clone, choice(&[])).await.unwrap_err();
    assert!(
        err.0.contains("several repositories")
            && err.0.contains("alice/demo")
            && err.0.contains("bob/demo"),
        "{err}"
    );
    let picked = Choice {
        repo: Some("bob/demo".into()),
        ..choice(&[])
    };
    create_with(&url, &clone, picked).await.unwrap();
    for name in ["origin", "ssh", "fork"] {
        run_git(&clone.work, &["remote", "remove", name]);
    }
    let err = create_with(&url, &clone, choice(&[])).await.unwrap_err();
    assert!(err.0.contains("no remote of this clone addresses"), "{err}");
    let specs: Vec<String> = fake
        .0
        .lock()
        .unwrap()
        .specs
        .iter()
        .map(|spec| spec.repo.clone())
        .collect();
    assert_eq!(specs, ["alice/demo", "bob/demo"]);
    clone.untouched();
}

#[test]
fn the_repository_comes_from_an_ssh_only_clone() {
    let dir = TempDir::new();
    run_git(&dir.0, &["init", "-q"]);
    for url in [
        "ssh://kajiba-git@forge.example.org:2222/alice/demo.git",
        "kajiba-git@forge.example.org:alice/demo.git",
    ] {
        run_git(&dir.0, &["config", "remote.origin.url", url]);
        assert_eq!(repository(&dir.0, FORGE).unwrap(), "alice/demo", "{url}");
    }
}

#[test]
fn forge_urls_in_every_form() {
    for (url, expected) in [
        (
            "https://forge.example.org/alice/demo.git",
            Some("alice/demo"),
        ),
        ("https://forge.example.org/alice/demo", Some("alice/demo")),
        ("https://forge.example.org/alice/demo/", Some("alice/demo")),
        (
            "ssh://kajiba-git@forge.example.org/alice/demo.git",
            Some("alice/demo"),
        ),
        (
            "ssh://kajiba-git@forge.example.org:2222/alice/demo",
            Some("alice/demo"),
        ),
        (
            "kajiba-git@forge.example.org:alice/demo.git",
            Some("alice/demo"),
        ),
        ("https://forge.example.org.evil/alice/demo", None),
        ("https://forge.example.org/alice", None),
        ("https://forge.example.org/alice/demo/tree", None),
        ("git@forge.example.org:alice/demo.git", None),
        ("ssh://kajiba-git@forge.example.orgx/alice/demo", None),
    ] {
        assert_eq!(addressed(url, FORGE).as_deref(), expected, "{url}");
    }
    assert_eq!(
        addressed(
            "https://forge.example.org:8443/a/b.git",
            "https://forge.example.org:8443/"
        )
        .as_deref(),
        Some("a/b")
    );
}

#[tokio::test]
async fn a_moved_ref_renegotiates_once() {
    let clone = clone();
    std::fs::write(clone.work.join("tracked"), "dirty\n").unwrap();
    let (fake, url) = forge(&[&clone.second]).await;
    let missing = || {
        refusal(
            Code::FailedPrecondition,
            "BUNDLE_PREREQUISITE_MISSING",
            "a prerequisite of the bundle is on no branch or tag of the repository",
        )
    };
    fake.0.lock().unwrap().upload_refusals = vec![missing()];
    create_with(&url, &clone, choice(&[])).await.unwrap();
    assert_eq!(fake.0.lock().unwrap().specs.len(), 2);
    // A second refusal is the command's error, with its reason.
    fake.0.lock().unwrap().upload_refusals = vec![missing(), missing()];
    let err = create_with(&url, &clone, choice(&[])).await.unwrap_err();
    assert!(
        err.0.contains("(BUNDLE_PREREQUISITE_MISSING, request "),
        "{err}"
    );
    assert_eq!(fake.0.lock().unwrap().uploads.len(), 4);
    clone.untouched();
}

#[tokio::test]
async fn refusals_are_printed_with_their_reason_and_leave_no_ref() {
    let clone = clone();
    let (fake, url) = forge(&[]).await;
    fake.0.lock().unwrap().refusal = Some(refusal(
        Code::PermissionDenied,
        "ROLE_MISSING",
        "the write role is needed",
    ));
    let err = create_with(&url, &clone, choice(&[])).await.unwrap_err();
    assert_eq!(
        err.0,
        "the write role is needed (ROLE_MISSING, request 01JBREQUEST0000000000000000)"
    );
    clone.untouched();
    let err = create_with(&url, &clone, choice(&["a.b"]))
        .await
        .unwrap_err();
    assert!(err.0.contains("invalid check name"), "{err}");
}

#[tokio::test]
async fn ci_run_worktree_detaches_like_ci_run() {
    let clone = clone();
    let (_fake, url) = forge(&[&clone.second]).await;
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = ci::run(
        Ci::Run {
            repo: None,
            sha: None,
            detach: true,
            worktree: true,
            worktree_repo: None,
            checks: Vec::new(),
            system: Some("x86_64-linux".into()),
        },
        connect(&url, TOKEN, true).unwrap(),
        &clone.local(),
        false,
        &mut out,
        &mut err,
    )
    .await
    .unwrap();
    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(String::from_utf8(out).unwrap(), format!("{RUN}\n"));
    clone.untouched();
}
