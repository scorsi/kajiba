use super::*;
use crate::push::{Quarantine, Update};

const ZERO: &str = "0000000000000000000000000000000000000000";
const DATE: &str = "1700000000 +0000";

fn updates<'a>(pairs: &[(&'a str, &'a str)]) -> Vec<Update<'a>> {
    pairs
        .iter()
        .map(|&(old_id, new_id)| Update { old_id, new_id })
        .collect()
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
            .push_facts(&f.bare, &changes, &Quarantine::default())
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
        Gix.push_facts(&f.bare, &changes, &Quarantine::default())
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
                Gix.push_facts(&f.bare, &updates(&[pair]), &Quarantine::default())
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
            Gix.push_facts(&f.bare, &updates(&[(ZERO, &f.third)]), &context)
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
            .push_facts(&repo, &updates, &Quarantine::default())
            .unwrap();
        let refusals = Gix
            .verify_commits(&repo, &updates, &facts, &Quarantine::default(), &signers)
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
        .push_facts(&repo, &updates, &Quarantine::default())
        .unwrap();
    assert_eq!(
        Gix.verify_commits(&repo, &updates, &facts, &Quarantine::default(), &signers)
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
    let facts = Gix.push_facts(&repo, &updates, &quarantine).unwrap();
    assert_eq!(facts.commits, std::slice::from_ref(&commit));
    let mut verified = Vec::new();
    let refusals = crate::push::verify_commits(&repo, &updates, &facts, &quarantine, |id| {
        verified.push(id.to_owned());
        crate::push::verify_commit(&repo, id, &quarantine, &signers)
    })
    .unwrap();
    assert!(refusals.is_empty());
    assert_eq!(verified, std::slice::from_ref(&commit));
    fs::write(&signers, "").unwrap();
    let refusals = Gix
        .verify_commits(&repo, &updates, &facts, &quarantine, &signers)
        .unwrap();
    assert_eq!(
        refusals
            .iter()
            .map(|r| (r.update_index, r.commit.as_str()))
            .collect::<Vec<_>>(),
        [(0, commit.as_str()), (1, commit.as_str())]
    );
    assert!(
        Gix.verify_commits(&repo, &updates, &facts, &Quarantine::default(), &signers)
            .is_err()
    );
}
