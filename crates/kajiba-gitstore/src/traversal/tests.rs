use super::*;
use crate::{GitStore, Gix};

fn refused<T: std::fmt::Debug>(result: Result<T>) {
    assert_eq!(result.unwrap_err(), Error::TooLarge(Limit::Traversal));
}

#[test]
fn traversal_budgets_accept_exact_bounds_and_stop_before_excess() {
    let mut work = Traversal::new(None);
    for _ in 0..MAX_OBJECTS {
        work.object(0).unwrap();
    }
    refused(work.object(0));
    let mut work = Traversal::new(None);
    work.object(MAX_BYTES).unwrap();
    refused(work.object(1));
    for _ in 0..MAX_ENTRIES {
        work.entry().unwrap();
    }
    refused(work.entry());
    work.path(&vec![b'x'; MAX_PATH_BYTES]).unwrap();
    refused(work.path(&vec![b'x'; MAX_PATH_BYTES + 1]));
    work.path(vec!["x"; MAX_DEPTH].join("/").as_bytes())
        .unwrap();
    refused(work.path(vec!["x"; MAX_DEPTH + 1].join("/").as_bytes()));
}

#[test]
fn traversal_deadline_and_cancellation_are_cooperative() {
    let cancelled = AtomicBool::new(false);
    let mut work = Traversal::new(Some(&cancelled));
    work.entry().unwrap();
    cancelled.store(true, Ordering::Relaxed);
    refused(work.entry());
    refused(work.object(0));
    let mut work = Traversal::new(None);
    work.deadline = Instant::now();
    refused(work.entry());
    refused(work.object(0));
}

// Build objects through gix so these regressions never run a Git write command.
fn repository(name: &str) -> gix::Repository {
    gix::init_bare(crate::tests::scratch(name).join("repo.git")).unwrap()
}

fn tree(repo: &gix::Repository, entries: Vec<(&str, bool, gix::ObjectId)>) -> gix::ObjectId {
    let mut entries: Vec<_> = entries
        .into_iter()
        .map(|(name, directory, oid)| gix::objs::tree::Entry {
            mode: if directory {
                gix::objs::tree::EntryKind::Tree
            } else {
                gix::objs::tree::EntryKind::Blob
            }
            .into(),
            filename: name.into(),
            oid,
        })
        .collect();
    entries.sort();
    repo.write_object(gix::objs::Tree { entries })
        .unwrap()
        .detach()
}

fn commit(
    repo: &gix::Repository,
    tree: gix::ObjectId,
    parents: &[gix::ObjectId],
    time: u64,
) -> String {
    let mut text = format!("tree {tree}\n");
    for parent in parents {
        text += &format!("parent {parent}\n");
    }
    text += &format!(
        "author Alice <alice@example.org> {time} +0000\ncommitter Alice <alice@example.org> {time} +0000\n\nm\n"
    );
    repo.write_object(
        gix::objs::CommitRef::from_bytes(text.as_bytes(), gix::hash::Kind::Sha1).unwrap(),
    )
    .unwrap()
    .to_string()
}

#[test]
fn traversal_history_bounds_frontier_before_loading_the_next_parent() {
    let repo = repository("traversal-frontier");
    let empty = tree(&repo, vec![]);
    let mut parents: Vec<_> = (0..MAX_FRONTIER)
        .map(|i| gix::ObjectId::from_hex(commit(&repo, empty, &[], i as u64).as_bytes()).unwrap())
        .collect();
    let tip = commit(&repo, empty, &parents, 2000);
    let page = Gix.log(repo.path(), &tip, 0, 3, usize::MAX).unwrap();
    assert_eq!(
        page.iter().map(|c| c.time).collect::<Vec<_>>(),
        [2000, 1023, 1022]
    );
    assert_eq!(
        Gix.log(repo.path(), &tip, 1, 2, usize::MAX).unwrap(),
        page[1..]
    );
    // The first parent beyond the frontier does not exist: refusal must precede its read.
    parents.push(gix::ObjectId::null(gix::hash::Kind::Sha1));
    let tip = commit(&repo, empty, &parents, 2001);
    assert_eq!(
        Gix.log(repo.path(), &tip, 0, 1, usize::MAX).unwrap().len(),
        1
    );
    refused(Gix.log(repo.path(), &tip, 0, 2, usize::MAX));
    refused(Gix.log(repo.path(), &tip, 1, 1, usize::MAX));
}

#[test]
fn traversal_tree_paths_and_directory_only_diffs_have_depth_bounds() {
    let repo = repository("traversal-depth");
    let mut root = tree(&repo, vec![]);
    for _ in 0..MAX_DEPTH {
        root = tree(&repo, vec![("d", true, root)]);
    }
    let tip = commit(&repo, root, &[], 0);
    let path = vec!["d"; MAX_DEPTH].join("/");
    assert!(Gix.tree(repo.path(), &tip, &path, 1).unwrap().is_empty());
    assert!(Gix.diff(repo.path(), None, &tip, 0, 0).unwrap().is_empty());
    root = tree(&repo, vec![("d", true, root)]);
    let tip = commit(&repo, root, &[], 1);
    refused(Gix.tree(repo.path(), &tip, &format!("{path}/d"), 1));
    refused(Gix.diff(repo.path(), None, &tip, 0, 0));
}

#[test]
fn traversal_diff_counts_unchanged_entries_and_repeated_subtrees() {
    let repo = repository("traversal-entries");
    let empty = tree(&repo, vec![]);
    let directory = tree(&repo, vec![("empty", true, empty)]);
    let blob = repo.write_blob(b"same").unwrap().detach();
    let names: Vec<_> = (0..1000).map(|i| format!("f{i:04}")).collect();
    let files: Vec<_> = names
        .iter()
        .map(|name| (name.as_str(), false, blob))
        .collect();
    let old_child = tree(&repo, files.clone());
    let mut files = files;
    files.push(("z", true, directory));
    let new_child = tree(&repo, files);
    let root = |child, width| {
        tree(
            &repo,
            names[..width]
                .iter()
                .map(|name| (name.as_str(), true, child))
                .collect(),
        )
    };
    let old = commit(&repo, root(old_child, 1), &[], 0);
    let new = commit(&repo, root(new_child, 1), &[], 1);
    assert!(
        Gix.diff(repo.path(), Some(&old), &new, 0, 0)
            .unwrap()
            .is_empty()
    );
    let old = commit(&repo, root(old_child, 100), &[], 2);
    let new = commit(&repo, root(new_child, 100), &[], 3);
    refused(Gix.diff(repo.path(), Some(&old), &new, 0, 0));

    // Path lookup must also charge the siblings scanned in each directory.
    let mut nested = empty;
    for _ in 0..101 {
        let mut entries: Vec<_> = names
            .iter()
            .map(|name| (name.as_str(), false, blob))
            .collect();
        entries.push(("z", true, nested));
        nested = tree(&repo, entries);
    }
    let tip = commit(&repo, nested, &[], 4);
    refused(Gix.tree(repo.path(), &tip, &vec!["z"; 101].join("/"), 1));
}

#[test]
fn traversal_cancelled_reads_stop_before_loading_a_missing_object() {
    let repo = repository("traversal-cancelled");
    let cancelled = AtomicBool::new(true);
    let store = Gix.cancellable(&cancelled);
    let missing = "0000000000000000000000000000000000000000";
    refused(store.tree(repo.path(), missing, "", 1));
    refused(store.log(repo.path(), missing, 0, 1, 1));
    refused(store.diff(repo.path(), None, missing, 1, 1));
}
