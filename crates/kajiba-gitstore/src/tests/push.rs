use super::*;
use crate::push::{Quarantine, Update, Verification};

const ZERO: &str = "0000000000000000000000000000000000000000";
const DATE: &str = "1700000000 +0000";

fn updates<'a>(pairs: &[(&'a str, &'a str)]) -> Vec<Update<'a>> {
    pairs
        .iter()
        .map(|&(old_id, new_id)| Update { old_id, new_id })
        .collect()
}

#[test]
fn receive_limits_are_applied_only_to_receive_pack() {
    let repo = Path::new("/repo.git");
    let hooks = Path::new("/hooks");
    let command = Gix.transport(Service::ReceivePack, repo, &[], Some(hooks));
    assert_eq!(
        command.get_args().collect::<Vec<_>>(),
        [
            "-c",
            "core.hooksPath=/hooks",
            "-c",
            "receive.fsckObjects=true",
            "-c",
            "receive.maxInputSize=67108864",
            "-c",
            "receive.unpackLimit=0",
            "-c",
            "receive.autogc=false",
            "receive-pack",
            "/repo.git",
        ]
    );
    assert_eq!(crate::push::MAX_INPUT_BYTES, 64 << 20);
    let command = Gix.transport(Service::UploadPack, repo, &[], None);
    assert_eq!(
        command.get_args().collect::<Vec<_>>(),
        ["upload-pack", "/repo.git"]
    );
}

#[test]
fn receive_limits_reject_aggregate_quarantine_before_reading_objects() {
    let root = scratch("receive-limits-quarantine");
    let incoming = root.join("incoming");
    fs::create_dir_all(incoming.join("pack")).unwrap();
    let pack = fs::File::create(incoming.join("pack/data.pack")).unwrap();
    let index = fs::File::create(incoming.join("pack/data.idx")).unwrap();
    pack.set_len(crate::push::MAX_QUARANTINE_BYTES - 1).unwrap();
    index.set_len(1).unwrap();
    let quarantine = Quarantine {
        object_directory: incoming.to_str().unwrap(),
        quarantine_path: incoming.to_str().unwrap(),
        ..Default::default()
    };
    // Sparse files exercise the exact boundary without allocating the disk budget.
    assert!(
        Gix.push_facts(&root, &[], &quarantine, &Verification::default())
            .is_ok()
    );
    index.set_len(2).unwrap();
    let missing = "1111111111111111111111111111111111111111";
    assert_eq!(
        Gix.push_facts(
            &root,
            &updates(&[(ZERO, missing)]),
            &quarantine,
            &Verification::default()
        )
        .unwrap_err(),
        Error::VerificationLimit("push quarantine exceeds 128 MiB limit")
    );
    index.set_len(0).unwrap();
    std::os::unix::fs::symlink(&root, incoming.join("link")).unwrap();
    assert_eq!(
        Gix.push_facts(&root, &[], &quarantine, &Verification::default())
            .unwrap_err(),
        Error::Git("invalid quarantine entry".into())
    );
    fs::remove_file(incoming.join("link")).unwrap();
    fs::remove_dir_all(&incoming).unwrap();
    assert!(
        Gix.push_facts(&root, &[], &quarantine, &Verification::default())
            .is_err()
    );
    assert!(
        Gix.push_facts(&root, &[], &Quarantine::default(), &Verification::default())
            .is_ok()
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn receive_limits_bound_child_file_growth() {
    let root = scratch("receive-limits-file");
    let file = root.join("pack");
    for (offset, success) in [
        (crate::push::MAX_QUARANTINE_BYTES - 1, true),
        (crate::push::MAX_QUARANTINE_BYTES, false),
    ] {
        let mut command = Command::new("sh");
        command
            .args([
                "-c",
                "ulimit -c 0; exec dd if=/dev/zero of=\"$1\" bs=1 count=1 seek=\"$2\"",
                "file-limit-test",
            ])
            .arg(&file)
            .arg(offset.to_string());
        // SAFETY: the callback only changes resource limits in the child.
        unsafe { command.pre_exec(crate::push::limit_file_size) };
        let output = command.output().unwrap();
        assert_eq!(output.status.success(), success, "{output:?}");
        assert!(fs::metadata(&file).unwrap().len() <= crate::push::MAX_QUARANTINE_BYTES);
    }
    fs::remove_dir_all(root).unwrap();
}

fn clear_refs(repo: &Path) {
    for r in Gix.refs(repo).unwrap() {
        git(repo, &["update-ref", "-d", &r.name], DATE);
    }
}

#[test]
fn first_push_selects_whole_history_once_across_refs_and_annotated_tags() {
    let f = fixture("push-first");
    let tag = git(&f.bare, &["rev-parse", "refs/tags/v1"], DATE);
    clear_refs(&f.bare);
    let facts = Gix
        .push_facts(
            &f.bare,
            &updates(&[
                (ZERO, &f.third),
                (ZERO, &f.second),
                (ZERO, &f.third),
                (ZERO, &tag),
            ]),
            &Quarantine::default(),
            &Verification::default(),
        )
        .unwrap();
    assert_eq!(facts.commits, [f.third, f.second, f.first]);
    assert_eq!(facts.fast_forward, [false; 4]);
}

#[test]
fn any_pre_push_ref_excludes_commits_even_when_deleted_in_this_push() {
    let f = fixture("push-existing");
    let tag = git(&f.bare, &["rev-parse", "refs/tags/v1"], DATE);
    clear_refs(&f.bare);
    for (name, id, expected) in [
        ("refs/heads/unrelated", &f.second, vec![f.third.clone()]),
        (
            "refs/tags/old",
            &tag,
            vec![f.third.clone(), f.second.clone()],
        ),
        ("refs/contrib/user/old", &f.second, vec![f.third.clone()]),
    ] {
        git(&f.bare, &["update-ref", name, id], DATE);
        let facts = Gix
            .push_facts(
                &f.bare,
                &updates(&[(id, ZERO), (ZERO, &f.third)]),
                &Quarantine::default(),
                &Verification::default(),
            )
            .unwrap();
        assert_eq!(facts.commits, expected);
        git(&f.bare, &["update-ref", "-d", name], DATE);
    }
}

#[test]
fn deletion_only_and_empty_pushes_select_nothing() {
    let f = fixture("push-deletions");
    for changes in [vec![], updates(&[(&f.third, ZERO), (&f.second, ZERO)])] {
        let facts = Gix
            .push_facts(
                &f.bare,
                &changes,
                &Quarantine::default(),
                &Verification::default(),
            )
            .unwrap();
        assert!(facts.commits.is_empty());
        assert_eq!(facts.fast_forward, vec![false; changes.len()]);
    }
}

#[test]
fn ancestry_distinguishes_forward_rewind_divergence_and_unrelated_history() {
    let f = fixture("push-ancestry");
    let tree = git(
        &f.bare,
        &["rev-parse", &format!("{}^{{tree}}", f.first)],
        DATE,
    );
    let sibling = git(
        &f.bare,
        &["commit-tree", &tree, "-p", &f.first, "-m", "sibling"],
        DATE,
    );
    let unrelated = git(&f.bare, &["commit-tree", &tree, "-m", "unrelated"], DATE);
    let facts = Gix
        .push_facts(
            &f.bare,
            &updates(&[
                (&f.first, &f.third),
                (&f.third, &f.first),
                (&f.second, &sibling),
                (&f.first, &unrelated),
                (&f.second, &f.second),
            ]),
            &Quarantine::default(),
            &Verification::default(),
        )
        .unwrap();
    assert_eq!(facts.fast_forward, [true, false, false, false, true]);
}

#[test]
fn quarantine_and_alternates_are_used_for_both_selection_and_ancestry() {
    let f = fixture("push-quarantine");
    clear_refs(&f.bare);
    git(&f.bare, &["update-ref", "refs/heads/main", &f.first], DATE);
    let quarantine = f.bare.join("objects/incoming-test");
    fs::create_dir(&quarantine).unwrap();
    // Only the incoming commits move; their ancestor stays in the ordinary store.
    for id in [&f.second, &f.third] {
        fs::create_dir_all(quarantine.join(&id[..2])).unwrap();
        fs::rename(
            f.bare.join("objects").join(&id[..2]).join(&id[2..]),
            quarantine.join(&id[..2]).join(&id[2..]),
        )
        .unwrap();
    }
    let changes = updates(&[(&f.first, &f.third), (ZERO, &f.second)]);
    assert!(
        Gix.push_facts(
            &f.bare,
            &changes,
            &Quarantine::default(),
            &Verification::default()
        )
        .is_err()
    );
    let facts = Gix
        .push_facts(
            &f.bare,
            &changes,
            &Quarantine {
                object_directory: quarantine.to_str().unwrap(),
                alternate_object_directories: f.bare.join("objects").to_str().unwrap(),
                quarantine_path: quarantine.to_str().unwrap(),
            },
            &Verification::default(),
        )
        .unwrap();
    assert_eq!(facts.commits, [f.third, f.second]);
    assert_eq!(facts.fast_forward, [true, false]);
    assert_eq!(
        git(&f.bare, &["rev-parse", "refs/heads/main"], DATE),
        f.first
    );
}

#[test]
fn invalid_or_missing_objects_and_bad_context_fail_checks() {
    let f = fixture("push-invalid");
    for id in ["--all", "HEAD", "1111111111111111111111111111111111111111"] {
        for pair in [(id, f.third.as_str()), (f.first.as_str(), id)] {
            assert!(
                Gix.push_facts(
                    &f.bare,
                    &updates(&[pair]),
                    &Quarantine::default(),
                    &Verification::default()
                )
                .is_err()
            );
        }
    }
    for context in [
        Quarantine {
            object_directory: "relative",
            ..Default::default()
        },
        Quarantine {
            quarantine_path: "relative",
            ..Default::default()
        },
    ] {
        assert!(
            Gix.push_facts(
                &f.bare,
                &updates(&[(ZERO, &f.third)]),
                &context,
                &Verification::default()
            )
            .is_err()
        );
    }
}

fn signing_key(dir: &Path, name: &str) -> (String, String) {
    let key = dir.join(name);
    let output = Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-C", "fixture", "-f"])
        .arg(&key)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let public = fs::read_to_string(key.with_extension("pub")).unwrap();
    (key.to_str().unwrap().into(), public)
}

#[test]
fn commit_signatures_require_the_pushers_ssh_signing_key() {
    let dir = scratch("commit-signatures");
    let repo = dir.join("repo.git");
    Gix.init_bare(&repo, "main").unwrap();
    let (alice, public) = signing_key(&dir, "alice");
    let (bob, bob_public) = signing_key(&dir, "bob");
    let signers = dir.join("allowed_signers");
    fs::write(&signers, format!("alice {public}")).unwrap();
    let tree = git(&repo, &["mktree"], DATE);
    let signed = |key: &str, parent: Option<&str>| {
        let config = format!("user.signingKey={key}");
        let mut args = vec![
            "-c",
            "gpg.format=ssh",
            "-c",
            &config,
            "commit-tree",
            &tree,
            "-S",
            "-m",
            "signed",
        ];
        if let Some(parent) = parent {
            args.extend(["-p", parent]);
        }
        git(&repo, &args, DATE)
    };
    let valid = signed(&alice, None);
    let wrong = signed(&bob, None);
    let unsigned = git(&repo, &["commit-tree", &tree, "-m", "unsigned"], DATE);
    let signed_tip = signed(&alice, Some(&unsigned));
    let raw = git(&repo, &["cat-file", "commit", &valid], DATE);
    let tampered_file = dir.join("tampered");
    fs::write(
        &tampered_file,
        format!("{}\n", raw.replace("\n\nsigned", "\n\ntampered")),
    )
    .unwrap();
    let tampered = git(
        &repo,
        &[
            "hash-object",
            "-t",
            "commit",
            "-w",
            tampered_file.to_str().unwrap(),
        ],
        DATE,
    );
    let other_signers = dir.join("other_signers");
    fs::write(&other_signers, format!("bob {bob_public}")).unwrap();
    // Repository configuration must not replace the session's authority or verifier.
    git(
        &repo,
        &[
            "config",
            "gpg.ssh.allowedSignersFile",
            other_signers.to_str().unwrap(),
        ],
        DATE,
    );
    git(&repo, &["config", "gpg.ssh.program", "false"], DATE);
    git(&repo, &["config", "gpg.minTrustLevel", "undefined"], DATE);

    for (tip, bad) in [
        (&valid, None),
        (&wrong, Some(&wrong)),
        (&unsigned, Some(&unsigned)),
        (&tampered, Some(&tampered)),
        (&signed_tip, Some(&unsigned)),
    ] {
        let updates = updates(&[(ZERO, tip)]);
        let facts = Gix
            .push_facts(
                &repo,
                &updates,
                &Quarantine::default(),
                &Verification::default(),
            )
            .unwrap();
        let refusals = Gix
            .verify_commits(
                &repo,
                &updates,
                &facts,
                &Quarantine::default(),
                &Verification::default(),
                &signers,
            )
            .unwrap();
        match bad {
            None => assert!(refusals.is_empty(), "{refusals:?}"),
            Some(commit) => {
                assert_eq!(refusals.len(), 1, "{refusals:?}");
                assert_eq!(refusals[0].commit, *commit);
                assert_eq!(refusals[0].update_index, 0);
                assert!(refusals[0].reason.contains("SSH signature"));
            }
        }
    }
    // Auth-only and removed signing keys produce no entry in OpenSession's text.
    fs::write(&signers, "").unwrap();
    let updates = updates(&[(ZERO, &valid)]);
    let facts = Gix
        .push_facts(
            &repo,
            &updates,
            &Quarantine::default(),
            &Verification::default(),
        )
        .unwrap();
    assert_eq!(
        Gix.verify_commits(
            &repo,
            &updates,
            &facts,
            &Quarantine::default(),
            &Verification::default(),
            &signers
        )
        .unwrap()
        .len(),
        1
    );
}

#[test]
fn shared_commits_are_verified_once_in_quarantine_and_refusals_name_each_ref() {
    let dir = scratch("shared-signatures");
    let repo = dir.join("repo.git");
    Gix.init_bare(&repo, "main").unwrap();
    let (key, public) = signing_key(&dir, "alice");
    let signers = dir.join("allowed_signers");
    fs::write(&signers, format!("alice {public}")).unwrap();
    let tree = git(&repo, &["mktree"], DATE);
    let commit = git(
        &repo,
        &[
            "-c",
            "gpg.format=ssh",
            "-c",
            &format!("user.signingKey={key}"),
            "commit-tree",
            &tree,
            "-S",
            "-m",
            "shared",
        ],
        DATE,
    );
    let incoming = repo.join("objects/incoming");
    fs::create_dir_all(incoming.join(&commit[..2])).unwrap();
    fs::rename(
        repo.join("objects").join(&commit[..2]).join(&commit[2..]),
        incoming.join(&commit[..2]).join(&commit[2..]),
    )
    .unwrap();
    let objects = repo.join("objects");
    let quarantine = Quarantine {
        object_directory: incoming.to_str().unwrap(),
        alternate_object_directories: objects.to_str().unwrap(),
        quarantine_path: incoming.to_str().unwrap(),
    };
    let updates = updates(&[(ZERO, &commit), (ZERO, &commit)]);
    let facts = Gix
        .push_facts(&repo, &updates, &quarantine, &Verification::default())
        .unwrap();
    assert_eq!(facts.commits, std::slice::from_ref(&commit));
    let mut verified = Vec::new();
    let refusals = crate::push::verify_commits(
        &repo,
        &updates,
        &facts,
        &quarantine,
        &Verification::default(),
        |id| {
            verified.push(id.to_owned());
            crate::push::verify_signature(
                &repo,
                id,
                &quarantine,
                &Verification::default(),
                &signers,
                "verify-commit",
            )
        },
    )
    .unwrap();
    assert!(refusals.is_empty());
    assert_eq!(verified, std::slice::from_ref(&commit));
    fs::write(&signers, "").unwrap();
    let refusals = Gix
        .verify_commits(
            &repo,
            &updates,
            &facts,
            &quarantine,
            &Verification::default(),
            &signers,
        )
        .unwrap();
    assert_eq!(
        refusals
            .iter()
            .map(|r| (r.update_index, r.commit.as_str()))
            .collect::<Vec<_>>(),
        [(0, commit.as_str()), (1, commit.as_str())]
    );
    assert!(
        Gix.verify_commits(
            &repo,
            &updates,
            &facts,
            &Quarantine::default(),
            &Verification::default(),
            &signers
        )
        .is_err()
    );
}

#[test]
fn tags_require_an_annotated_ssh_signature_by_the_pusher_in_quarantine() {
    let dir = scratch("tag-signatures");
    let repo = dir.join("repo.git");
    Gix.init_bare(&repo, "main").unwrap();
    let (alice, public) = signing_key(&dir, "alice");
    let (bob, bob_public) = signing_key(&dir, "bob");
    let signers = dir.join("allowed_signers");
    fs::write(&signers, format!("alice {public}")).unwrap();
    let tree = git(&repo, &["mktree"], DATE);
    let commit = git(&repo, &["commit-tree", &tree, "-m", "existing"], DATE);
    for (name, key) in [("valid", &alice), ("wrong", &bob)] {
        git(
            &repo,
            &[
                "-c",
                "gpg.format=ssh",
                "-c",
                &format!("user.signingKey={key}"),
                "tag",
                "-s",
                name,
                &commit,
                "-m",
                "release",
            ],
            DATE,
        );
    }
    git(
        &repo,
        &["tag", "-a", "unsigned", &commit, "-m", "release"],
        DATE,
    );
    let valid = git(&repo, &["rev-parse", "refs/tags/valid"], DATE);
    let wrong = git(&repo, &["rev-parse", "refs/tags/wrong"], DATE);
    let unsigned = git(&repo, &["rev-parse", "refs/tags/unsigned"], DATE);
    let raw = git(&repo, &["cat-file", "tag", &valid], DATE);
    let tampered_file = dir.join("tampered");
    fs::write(
        &tampered_file,
        format!("{}\n", raw.replace("release", "tampered")),
    )
    .unwrap();
    let tampered = git(
        &repo,
        &[
            "hash-object",
            "-t",
            "tag",
            "-w",
            tampered_file.to_str().unwrap(),
        ],
        DATE,
    );
    // A configured verifier or signer list cannot bypass the session's authority.
    let other_signers = dir.join("other_signers");
    fs::write(&other_signers, format!("bob {bob_public}")).unwrap();
    git(
        &repo,
        &[
            "config",
            "gpg.ssh.allowedSignersFile",
            other_signers.to_str().unwrap(),
        ],
        DATE,
    );
    git(&repo, &["config", "gpg.ssh.program", "false"], DATE);
    git(&repo, &["config", "gpg.minTrustLevel", "undefined"], DATE);
    let incoming = repo.join("objects/incoming");
    for id in [&valid, &wrong, &unsigned, &tampered] {
        fs::create_dir_all(incoming.join(&id[..2])).unwrap();
        fs::rename(
            repo.join("objects").join(&id[..2]).join(&id[2..]),
            incoming.join(&id[..2]).join(&id[2..]),
        )
        .unwrap();
    }
    let objects = repo.join("objects");
    let quarantine = Quarantine {
        object_directory: incoming.to_str().unwrap(),
        alternate_object_directories: objects.to_str().unwrap(),
        quarantine_path: incoming.to_str().unwrap(),
    };
    for (id, accepted) in [
        (&valid, true),
        (&commit, false), // A lightweight tag points directly at the commit.
        (&tree, false),
        (&unsigned, false),
        (&tampered, false),
        (&wrong, false),
    ] {
        assert_eq!(
            Gix.verify_tag(&repo, id, &quarantine, &Verification::default(), &signers)
                .unwrap(),
            accepted,
            "{id}"
        );
    }
    assert!(
        !Gix.verify_tag(
            &repo,
            &valid,
            &Quarantine::default(),
            &Verification::default(),
            &signers
        )
        .unwrap()
    );
    fs::write(&signers, "").unwrap();
    assert!(
        !Gix.verify_tag(
            &repo,
            &valid,
            &quarantine,
            &Verification::default(),
            &signers
        )
        .unwrap()
    );
}

#[test]
fn verification_ref_and_commit_limits_reject_instead_of_truncating() {
    let root = scratch("verification-limits");
    Gix.init_bare(&root, "main").unwrap();
    let quarantine = Quarantine::default();
    assert!(
        Gix.push_facts(
            &root,
            &updates(&[(ZERO, ZERO); 256]),
            &quarantine,
            &Verification::default()
        )
        .is_ok()
    );
    assert!(
        Gix.push_facts(
            &root,
            &updates(&[(ZERO, ZERO); 257]),
            &quarantine,
            &Verification::default()
        )
        .unwrap_err()
        .to_string()
        .contains("256 updated refs")
    );

    let mut command = crate::git();
    command
        .arg("--git-dir")
        .arg(&root)
        .args(["fast-import", "--quiet"]);
    let history = "commit refs/heads/main\ncommitter Alice <alice@example.org> 1700000000 +0000\ndata 1\nx\n\n".repeat(4097);
    run(command, &history, Duration::from_secs(30)).unwrap();
    let tip = git(&root, &["rev-parse", "HEAD"], DATE);
    let parent = git(&root, &["rev-parse", "HEAD^"], DATE);
    clear_refs(&root);
    let facts = Gix
        .push_facts(
            &root,
            &updates(&[(ZERO, &parent)]),
            &quarantine,
            &Verification::default(),
        )
        .unwrap();
    assert_eq!(facts.commits.len(), 4096);
    assert!(
        Gix.push_facts(
            &root,
            &updates(&[(ZERO, &tip)]),
            &quarantine,
            &Verification::default()
        )
        .unwrap_err()
        .to_string()
        .contains("4096 new commits")
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn verification_keeps_only_the_first_commit_refusal_per_ref() {
    let f = fixture("verification-refusals");
    clear_refs(&f.bare);
    let changes = updates(&[(ZERO, &f.third), (ZERO, &f.second)]);
    let quarantine = Quarantine::default();
    let verification = Verification::default();
    let facts = Gix
        .push_facts(&f.bare, &changes, &quarantine, &verification)
        .unwrap();
    let mut checked = 0;
    let refusals = crate::push::verify_commits(
        &f.bare,
        &changes,
        &facts,
        &quarantine,
        &verification,
        |_| {
            checked += 1;
            Ok(false)
        },
    )
    .unwrap();
    assert_eq!(checked, facts.commits.len());
    assert_eq!(refusals.len(), 2);
    assert_eq!(
        refusals.iter().map(|r| r.update_index).collect::<Vec<_>>(),
        [0, 1]
    );
}
