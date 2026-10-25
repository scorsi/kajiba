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
