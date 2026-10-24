//! GitStore boundary (0003): gix for reads, the `git` binary for transport.
//!
//! Object ids cross the boundary as lowercase hex strings so that callers never
//! depend on gix types and pieces can be replaced one by one.

use std::fmt;
use std::path::Path;
use std::process::Command;

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    /// The object, ref or path does not exist (or is not of the expected kind).
    NotFound,
    /// Any other failure, with gix's message.
    Git(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound => f.write_str("not found"),
            Self::Git(err) => f.write_str(err),
        }
    }
}

impl std::error::Error for Error {}

impl From<gix::Error> for Error {
    fn from(err: gix::Error) -> Self {
        Self::Git(err.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// A ref and the object it points to (an annotated tag points to its tag object).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ref {
    pub name: String,
    pub target: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Tree,
    Blob,
    Executable,
    Link,
    Submodule,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeEntry {
    pub name: String,
    pub kind: EntryKind,
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    pub id: String,
    pub parents: Vec<String>,
    pub author_name: String,
    pub author_email: String,
    /// Committer time, seconds since the Unix epoch.
    pub time: i64,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    Added,
    Deleted,
    Modified,
}

/// A changed non-tree entry between two commits; renames are not detected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub path: String,
    pub kind: ChangeKind,
    pub old_id: Option<String>,
    pub new_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Service {
    UploadPack,
    ReceivePack,
}

/// Every interaction with git goes through this trait (0003). Commit arguments are
/// full hex ids; paths are `/`-separated, `""` meaning the root tree.
pub trait GitStore {
    /// `git init --bare` at `path` with `HEAD` on `refs/heads/<default_branch>`.
    fn init_bare(&self, path: &Path, default_branch: &str) -> Result<()>;
    /// Every ref except `HEAD`, sorted by name (as `git for-each-ref`).
    fn refs(&self, repo: &Path) -> Result<Vec<Ref>>;
    fn tree(&self, repo: &Path, commit: &str, path: &str) -> Result<Vec<TreeEntry>>;
    fn blob(&self, repo: &Path, commit: &str, path: &str) -> Result<Vec<u8>>;
    /// Up to `limit` commits reachable from `commit`, newest first.
    fn log(&self, repo: &Path, commit: &str, limit: usize) -> Result<Vec<Commit>>;
    /// Changes from `from` (the empty tree when `None`) to `to`.
    fn diff(&self, repo: &Path, from: Option<&str>, to: &str) -> Result<Vec<Change>>;
    /// Splits `<ref>/<path>` (0021) into a commit id and the remaining path (`""`
    /// when none): the longest leading run of segments naming a branch, then a tag,
    /// otherwise a full commit id as the first segment.
    fn resolve<'a>(&self, repo: &Path, spec: &'a str) -> Result<(String, &'a str)>;
    /// `git <service> <options> <repo>`, to be spawned by the transport (SSH, HTTP
    /// with `--stateless-rpc`).
    fn transport(&self, service: Service, repo: &Path, options: &[&str]) -> Command;
}

/// The gix + `git` binary implementation.
pub struct Gix;

fn open(repo: &Path) -> Result<gix::Repository> {
    // Isolated: the server's environment and global configuration never apply.
    Ok(gix::open_opts(repo, gix::open::Options::isolated())?)
}

fn commit<'r>(repo: &'r gix::Repository, id: &str) -> Result<gix::Commit<'r>> {
    let id = gix::ObjectId::from_hex(id.as_bytes()).map_err(|_| Error::NotFound)?;
    repo.try_find_object(id)?
        .ok_or(Error::NotFound)?
        .try_into_commit()
        .map_err(|_| Error::NotFound)
}

fn tree_at<'r>(repo: &'r gix::Repository, commit_id: &str, path: &str) -> Result<gix::Tree<'r>> {
    let root = commit(repo, commit_id)?.tree()?;
    if path.is_empty() {
        return Ok(root);
    }
    root.lookup_entry_by_path(path)?
        .ok_or(Error::NotFound)?
        .object()?
        .try_into_tree()
        .map_err(|_| Error::NotFound)
}

impl GitStore for Gix {
    fn init_bare(&self, path: &Path, default_branch: &str) -> Result<()> {
        let head = format!("refs/heads/{default_branch}");
        kajiba_core::names::validate_ref(&head).map_err(|err| Error::Git(err.to_string()))?;
        gix::init_bare(path)?;
        std::fs::write(path.join("HEAD"), format!("ref: {head}\n"))
            .map_err(|err| Error::Git(err.to_string()))
    }

    fn refs(&self, repo: &Path) -> Result<Vec<Ref>> {
        let repo = open(repo)?;
        let mut refs = Vec::new();
        for reference in repo.references()?.all()? {
            let mut reference = reference?;
            let name = reference.name().as_bstr().to_string();
            // Symbolic refs list the object they resolve to, as `git for-each-ref`.
            let target = reference.follow_to_object()?.to_string();
            refs.push(Ref { name, target });
        }
        refs.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(refs)
    }

    fn tree(&self, repo: &Path, commit: &str, path: &str) -> Result<Vec<TreeEntry>> {
        let repo = open(repo)?;
        let tree = tree_at(&repo, commit, path)?;
        let mut entries = Vec::new();
        for entry in tree.iter() {
            let entry = entry?;
            use gix::object::tree::EntryKind as K;
            entries.push(TreeEntry {
                name: entry.filename().to_string(),
                kind: match entry.mode().kind() {
                    K::Tree => EntryKind::Tree,
                    K::Blob => EntryKind::Blob,
                    K::BlobExecutable => EntryKind::Executable,
                    K::Link => EntryKind::Link,
                    K::Commit => EntryKind::Submodule,
                },
                id: entry.oid().to_string(),
            });
        }
        Ok(entries)
    }

    fn blob(&self, repo: &Path, commit: &str, path: &str) -> Result<Vec<u8>> {
        let repo = open(repo)?;
        let (dir, name) = path.rsplit_once('/').unwrap_or(("", path));
        let tree = tree_at(&repo, commit, dir)?;
        let entry = tree.find_entry(name).ok_or(Error::NotFound)?;
        if entry.mode().is_tree() || entry.mode().is_commit() {
            return Err(Error::NotFound);
        }
        let data = entry.object()?.detach().data;
        Ok(data)
    }

    fn log(&self, repo: &Path, commit_id: &str, limit: usize) -> Result<Vec<Commit>> {
        let repo = open(repo)?;
        let tip = commit(&repo, commit_id)?.id;
        let walk = repo
            .rev_walk([tip])
            .sorting(gix::revision::walk::Sorting::ByCommitTime(
                Default::default(),
            ))
            .all()?;
        let mut commits = Vec::new();
        for info in walk.take(limit) {
            let info = info?;
            let object = info.object()?;
            let author = object.author()?;
            commits.push(Commit {
                id: info.id.to_string(),
                parents: info.parent_ids().map(|id| id.to_string()).collect(),
                author_name: author.name.to_string(),
                author_email: author.email.to_string(),
                time: object.time()?.seconds,
                message: object.message_raw_sloppy().to_string(),
            });
        }
        Ok(commits)
    }

    fn diff(&self, repo: &Path, from: Option<&str>, to: &str) -> Result<Vec<Change>> {
        let repo = open(repo)?;
        let old = from.map(|id| tree_at(&repo, id, "")).transpose()?;
        let new = tree_at(&repo, to, "")?;
        let mut changes = Vec::new();
        for change in repo.diff_tree_to_tree(old.as_ref(), &new, gix::diff::Options::default())? {
            use gix::object::tree::diff::ChangeDetached as C;
            let (path, kind, old_id, new_id, tree) = match change {
                C::Addition {
                    location,
                    entry_mode,
                    id,
                    ..
                } => (
                    location,
                    ChangeKind::Added,
                    None,
                    Some(id),
                    entry_mode.is_tree(),
                ),
                C::Deletion {
                    location,
                    entry_mode,
                    id,
                    ..
                } => (
                    location,
                    ChangeKind::Deleted,
                    Some(id),
                    None,
                    entry_mode.is_tree(),
                ),
                C::Modification {
                    location,
                    previous_id,
                    entry_mode,
                    id,
                    ..
                } => (
                    location,
                    ChangeKind::Modified,
                    Some(previous_id),
                    Some(id),
                    entry_mode.is_tree(),
                ),
                C::Rewrite { .. } => unreachable!("rewrite tracking is disabled"),
            };
            if !tree {
                changes.push(Change {
                    path: path.to_string(),
                    kind,
                    old_id: old_id.map(|id| id.to_string()),
                    new_id: new_id.map(|id| id.to_string()),
                });
            }
        }
        Ok(changes)
    }

    fn resolve<'a>(&self, repo_path: &Path, spec: &'a str) -> Result<(String, &'a str)> {
        // Exact names from the listing: gix's lookup would also try `refs/tags/<name>`, ….
        let refs = self.refs(repo_path)?;
        let repo = open(repo_path)?;
        let ends: Vec<usize> = spec
            .match_indices('/')
            .map(|(end, _)| end)
            .chain([spec.len()])
            .collect();
        let rest = |end: usize| spec.get(end + 1..).unwrap_or("");
        for namespace in ["refs/heads/", "refs/tags/"] {
            for &end in ends.iter().rev() {
                let name = format!("{namespace}{}", &spec[..end]);
                if let Some(found) = refs.iter().find(|r| r.name == name) {
                    let id = gix::ObjectId::from_hex(found.target.as_bytes())
                        .map_err(|err| Error::Git(err.to_string()))?;
                    let commit = repo
                        .find_object(id)?
                        .peel_to_kind(gix::object::Kind::Commit)
                        .map_err(|_| Error::NotFound)?;
                    return Ok((commit.id.to_string(), rest(end)));
                }
            }
        }
        let id = commit(&repo, &spec[..ends[0]])?.id.to_string();
        Ok((id, rest(ends[0])))
    }

    fn transport(&self, service: Service, repo: &Path, options: &[&str]) -> Command {
        let mut command = Command::new("git");
        command
            .arg(match service {
                Service::UploadPack => "upload-pack",
                Service::ReceivePack => "receive-pack",
            })
            .args(options)
            .arg(repo);
        command
    }
}

#[cfg(test)]
mod tests;
