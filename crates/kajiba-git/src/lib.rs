//! GitStore boundary (0003): gix for reads, the `git` binary for transport.
//!
//! Object ids cross the boundary as lowercase hex strings so that callers never
//! depend on gix types and pieces can be replaced one by one.

use std::fmt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    /// The object, ref or path does not exist (or is not of the expected kind).
    NotFound,
    /// The object or listing exceeds the caller's limit or an object cap.
    TooLarge(Limit),
    /// Any other failure, with gix's message.
    Git(String),
}

/// What a read exceeded (ADR 0043, 0047).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Limit {
    Blob,
    Tree,
    Commit,
    Diff,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound => f.write_str("not found"),
            Self::TooLarge(limit) => write!(
                f,
                "{} too large",
                match limit {
                    Limit::Blob => "blob",
                    Limit::Tree => "tree",
                    Limit::Commit => "commit",
                    Limit::Diff => "diff",
                }
            ),
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
    /// Points `HEAD` at `refs/heads/<branch>` (the declared default branch).
    fn set_head(&self, repo: &Path, branch: &str) -> Result<()>;
    /// Every ref except `HEAD`, sorted by name (as `git for-each-ref`).
    fn refs(&self, repo: &Path) -> Result<Vec<Ref>>;
    /// The entries of a tree, refused while listing past `limit` of them.
    fn tree(&self, repo: &Path, commit: &str, path: &str, limit: usize) -> Result<Vec<TreeEntry>>;
    /// Refuses blobs larger than `limit` after reading their object header.
    fn blob(&self, repo: &Path, commit: &str, path: &str, limit: u64) -> Result<Vec<u8>>;
    /// Up to `limit` commits reachable from `commit`, newest first, after passing over
    /// the first `skip` without building them; ends early after the commit whose
    /// message takes the page's message bytes past `max_bytes`.
    fn log(
        &self,
        repo: &Path,
        commit: &str,
        skip: usize,
        limit: usize,
        max_bytes: usize,
    ) -> Result<Vec<Commit>>;
    /// Changes from `from` (the empty tree when `None`) to `to`, refused during the
    /// walk past `max_changes` of them or `max_bytes` of paths.
    fn diff(
        &self,
        repo: &Path,
        from: Option<&str>,
        to: &str,
        max_changes: usize,
        max_bytes: usize,
    ) -> Result<Vec<Change>>;
    /// Splits `<ref>/<path>` (0021) into a commit id and the remaining path (`""`
    /// when none): the longest leading run of segments naming a branch, then a tag,
    /// otherwise a full commit id as the first segment.
    fn resolve<'a>(&self, repo: &Path, spec: &'a str) -> Result<(String, &'a str)>;
    /// `git <service> <options> <repo>`, to be spawned by the transport (SSH, HTTP
    /// with `--stateless-rpc`).
    fn transport(&self, service: Service, repo: &Path, options: &[&str]) -> Command;
    /// The branches and tags (`refs/heads/*`, `refs/tags/*`) of a path, bundle or
    /// URL, sorted by name; blocking. `credentials` is `user:token` or a token alone
    /// for an http(s) source, empty otherwise (ADR 0044).
    fn remote_refs(&self, source: &str, credentials: &str) -> Result<Vec<Ref>>;
    /// Fetches exactly these ids into these refs of `repo`, all or none
    /// (`git fetch --atomic`, no tag following); blocking.
    fn fetch(&self, repo: &Path, source: &str, credentials: &str, refs: &[Ref]) -> Result<()>;
}

/// Deadline of one git child of an import (`ls-remote`, then `fetch`): a hung
/// source must not pin the repository's admission gate (ADR 0003). Past it the
/// child is killed and the import fails.
pub const IMPORT_TIMEOUT: Duration = Duration::from_secs(60 * 60);

/// The git binary with an explicit environment (only `PATH` is inherited), without
/// system or user configuration, never prompting. J3's `receive-pack` passes the
/// module's `core.hooksPath` explicitly (ADR 0003).
fn git() -> Command {
    let mut command = Command::new("git");
    command.env_clear();
    if let Some(path) = std::env::var_os("PATH") {
        command.env("PATH", path);
    }
    command
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

/// A git child reading an import source: credentials travel in the environment,
/// never on the command line (readable by every local user), and the source cannot
/// steer the protocol or run hooks. `credentials` is `user:token` or a token alone
/// (then the user name is `token`, which every forge accepts with a token).
fn import_command(source: &str, credentials: &str) -> Command {
    let local = !source.contains("://") || source.starts_with("file://");
    let mut command = git();
    command.env(
        "GIT_ALLOW_PROTOCOL",
        if local {
            "file:http:https:ssh:git"
        } else {
            "http:https:ssh:git"
        },
    );
    let mut config = vec![
        ("core.hooksPath", "/dev/null".to_owned()),
        // A stalled transfer ends on its own; IMPORT_TIMEOUT is the backstop.
        ("http.lowSpeedLimit", "1".to_owned()),
        ("http.lowSpeedTime", "60".to_owned()),
    ];
    if !credentials.is_empty() {
        let (username, password) = credentials
            .split_once(':')
            .unwrap_or(("token", credentials));
        command
            .env("KAJIBA_IMPORT_USERNAME", username)
            .env("KAJIBA_IMPORT_PASSWORD", password);
        config.push((
            "credential.helper",
            r#"!f() { printf 'username=%s\npassword=%s\n' "$KAJIBA_IMPORT_USERNAME" "$KAJIBA_IMPORT_PASSWORD"; }; f"#
                .to_owned(),
        ));
    }
    command.env("GIT_CONFIG_COUNT", config.len().to_string());
    for (i, (key, value)) in config.iter().enumerate() {
        command
            .env(format!("GIT_CONFIG_KEY_{i}"), key)
            .env(format!("GIT_CONFIG_VALUE_{i}"), value);
    }
    command
}

/// Reads a pipe to its end on a thread of its own, so a chatty or stuck pipe never
/// blocks the deadline.
fn drain(mut pipe: impl std::io::Read + Send + 'static) -> std::sync::mpsc::Receiver<Vec<u8>> {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buffer = Vec::new();
        let _ = pipe.read_to_end(&mut buffer);
        let _ = sender.send(buffer);
    });
    receiver
}

/// Blocks until the child exits, without reaping it: its pid still names its process
/// group afterwards, so killing the group cannot reach a recycled pid.
fn wait_without_reaping(pid: libc::pid_t) {
    // SAFETY: waitid only writes `info`; a zeroed siginfo_t is a valid out-parameter.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    loop {
        let waited = unsafe {
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOWAIT,
            )
        };
        if waited == 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
            return;
        }
    }
}

/// Runs the child to completion within `deadline`, a hard limit on the process, not
/// on its pipes: a child that closes stdout and lingers is still killed, and a
/// descendant (ssh, curl) holding a pipe dies with the child's process group.
fn run(mut command: Command, stdin: &str, deadline: Duration) -> Result<String> {
    use std::io::Write;
    use std::os::unix::process::CommandExt;
    use std::sync::mpsc::channel;
    let mut child = command
        .stdin(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|err| Error::Git(format!("git: {err}")))?;
    let pid = child.id() as libc::pid_t;
    let mut input = child.stdin.take().expect("piped");
    let stdout = drain(child.stdout.take().expect("piped"));
    let stderr = drain(child.stderr.take().expect("piped"));
    // Writing can fail if git exits early; its status reports why.
    let stdin = stdin.to_owned();
    std::thread::spawn(move || input.write_all(stdin.as_bytes()));
    let (exited, exit) = channel::<()>();
    std::thread::spawn(move || {
        wait_without_reaping(pid);
        let _ = exited.send(());
    });
    let timed_out = exit.recv_timeout(deadline).is_err();
    // Exited or past the deadline: nothing of the import's process group outlives git.
    // SAFETY: the child is not reaped yet, so `pid` is ours and still names the group.
    unsafe { libc::kill(-pid, libc::SIGKILL) };
    let status = child
        .wait()
        .map_err(|err| Error::Git(format!("git: {err}")))?;
    if timed_out {
        return Err(Error::Git(format!(
            "git did not finish within {} s",
            deadline.as_secs()
        )));
    }
    // A descendant that left the group (setsid) may still hold a pipe: bounded wait.
    let collect = |pipe: std::sync::mpsc::Receiver<Vec<u8>>| {
        pipe.recv_timeout(Duration::from_secs(10))
            .unwrap_or_default()
    };
    if !status.success() {
        return Err(Error::Git(
            String::from_utf8_lossy(&collect(stderr)).trim().to_owned(),
        ));
    }
    String::from_utf8(collect(stdout)).map_err(|err| Error::Git(err.to_string()))
}

/// The gix + `git` binary implementation.
pub struct Gix;

fn open(repo: &Path) -> Result<gix::Repository> {
    // Isolated: the server's environment and global configuration never apply.
    Ok(gix::open_opts(repo, gix::open::Options::isolated())?)
}

/// Caps on the objects browsing loads whole (ADR 0047), checked on the object header
/// before the object is read: a tree, and a commit or the tag leading to one.
pub const MAX_TREE_OBJECT_BYTES: u64 = 4 << 20;
pub const MAX_COMMIT_OBJECT_BYTES: u64 = 1 << 20;

fn header(repo: &gix::Repository, id: gix::ObjectId) -> Result<gix::odb::find::Header> {
    repo.try_find_header(id)?.ok_or(Error::NotFound)
}

/// The commit a ref target names, through annotated tags; every object's kind and
/// size are checked before it is loaded, so a blob target is never read.
fn peel_to_commit<'r>(repo: &'r gix::Repository, mut id: gix::ObjectId) -> Result<gix::Commit<'r>> {
    use gix::object::Kind;
    loop {
        let header = header(repo, id)?;
        if !matches!(header.kind(), Kind::Commit | Kind::Tag) {
            return Err(Error::NotFound);
        }
        if header.size() > MAX_COMMIT_OBJECT_BYTES {
            return Err(Error::TooLarge(Limit::Commit));
        }
        let object = repo.find_object(id)?;
        if header.kind() == Kind::Commit {
            return Ok(object.into_commit());
        }
        id = object
            .into_tag()
            .target_id()
            .map_err(|err| Error::Git(err.to_string()))?
            .detach();
    }
}

/// The commit named by a full hex id (a tag id is not a commit, 0021).
fn commit<'r>(repo: &'r gix::Repository, id: &str) -> Result<gix::Commit<'r>> {
    let id = gix::ObjectId::from_hex(id.as_bytes()).map_err(|_| Error::NotFound)?;
    if header(repo, id)?.kind() != gix::object::Kind::Commit {
        return Err(Error::NotFound);
    }
    peel_to_commit(repo, id)
}

/// A tree object, its kind and size checked before it is loaded.
fn load_tree(repo: &gix::Repository, id: gix::ObjectId) -> Result<gix::Tree<'_>> {
    let header = header(repo, id)?;
    if header.kind() != gix::object::Kind::Tree {
        return Err(Error::NotFound);
    }
    if header.size() > MAX_TREE_OBJECT_BYTES {
        return Err(Error::TooLarge(Limit::Tree));
    }
    Ok(repo.find_object(id)?.into_tree())
}

fn tree_at<'r>(repo: &'r gix::Repository, commit_id: &str, path: &str) -> Result<gix::Tree<'r>> {
    let mut id = commit(repo, commit_id)?
        .tree_id()
        .map_err(|err| Error::Git(err.to_string()))?
        .detach();
    // Segment by segment, so every tree on the way is checked before it is loaded.
    for segment in path.split('/').filter(|segment| !segment.is_empty()) {
        let tree = load_tree(repo, id)?;
        let entry = tree.find_entry(segment).ok_or(Error::NotFound)?;
        if !entry.mode().is_tree() {
            return Err(Error::NotFound);
        }
        id = entry.oid().to_owned();
    }
    load_tree(repo, id)
}

impl GitStore for Gix {
    fn init_bare(&self, path: &Path, default_branch: &str) -> Result<()> {
        kajiba_core::names::validate_ref(&format!("refs/heads/{default_branch}"))
            .map_err(|err| Error::Git(err.to_string()))?;
        gix::init_bare(path)?;
        // Objects and refs reach the disk before the database records them (ADR 0003).
        let config = path.join("config");
        let mut file =
            gix::config::File::from_path_no_includes(config.clone(), gix::config::Source::Local)
                .map_err(|err| Error::Git(err.to_string()))?;
        file.set_raw_value_by("core", None, "fsync", "all")
            .map_err(|err| Error::Git(err.to_string()))?;
        std::fs::write(&config, file.to_bstring()).map_err(|err| Error::Git(err.to_string()))?;
        self.set_head(path, default_branch)
    }

    fn set_head(&self, repo: &Path, branch: &str) -> Result<()> {
        use std::io::Write;
        let head = format!("refs/heads/{branch}");
        kajiba_core::names::validate_ref(&head).map_err(|err| Error::Git(err.to_string()))?;
        let io = |err: std::io::Error| Error::Git(format!("{}/HEAD: {err}", repo.display()));
        let path = repo.join("HEAD");
        let content = format!("ref: {head}\n");
        // Idempotent: startup calls this for every declared repository, so a HEAD left
        // behind by a failed or interrupted run converges without touching the others.
        let current = std::fs::symlink_metadata(&path).map_err(io)?;
        if current.is_file() && std::fs::read(&path).map_err(io)? == content.as_bytes() {
            return Ok(());
        }
        // Temp file, fsync, rename over HEAD, fsync the directory: git never sees a
        // truncated HEAD. `create_new` refuses to write through a planted symlink.
        let tmp = repo.join("HEAD.tmp");
        let _ = std::fs::remove_file(&tmp);
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .map_err(io)?;
        file.write_all(content.as_bytes()).map_err(io)?;
        file.sync_all().map_err(io)?;
        std::fs::rename(&tmp, &path).map_err(io)?;
        std::fs::File::open(repo)
            .and_then(|dir| dir.sync_all())
            .map_err(io)
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

    fn tree(&self, repo: &Path, commit: &str, path: &str, limit: usize) -> Result<Vec<TreeEntry>> {
        let repo = open(repo)?;
        let tree = tree_at(&repo, commit, path)?;
        let mut entries = Vec::new();
        for entry in tree.iter() {
            let entry = entry?;
            if entries.len() == limit {
                return Err(Error::TooLarge(Limit::Tree));
            }
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

    fn blob(&self, repo: &Path, commit: &str, path: &str, limit: u64) -> Result<Vec<u8>> {
        let repo = open(repo)?;
        let (dir, name) = path.rsplit_once('/').unwrap_or(("", path));
        let tree = tree_at(&repo, commit, dir)?;
        let entry = tree.find_entry(name).ok_or(Error::NotFound)?;
        if entry.mode().is_tree() || entry.mode().is_commit() {
            return Err(Error::NotFound);
        }
        if repo.find_header(entry.id())?.size() > limit {
            return Err(Error::TooLarge(Limit::Blob));
        }
        let data = entry.object()?.detach().data;
        Ok(data)
    }

    fn log(
        &self,
        repo: &Path,
        commit_id: &str,
        skip: usize,
        limit: usize,
        max_bytes: usize,
    ) -> Result<Vec<Commit>> {
        let repo = open(repo)?;
        let tip = commit(&repo, commit_id)?.id;
        let walk = repo
            .rev_walk([tip])
            .sorting(gix::revision::walk::Sorting::ByCommitTime(
                Default::default(),
            ))
            .all()?;
        let mut commits = Vec::new();
        let mut bytes = 0;
        // Skipped entries are walk infos only (id, parents): no object is decoded.
        for info in walk.skip(skip).take(limit) {
            let info = info?;
            if header(&repo, info.id)?.size() > MAX_COMMIT_OBJECT_BYTES {
                return Err(Error::TooLarge(Limit::Commit));
            }
            let object = info.object()?;
            let author = object.author()?;
            let message = object.message_raw_sloppy().to_string();
            bytes += message.len();
            commits.push(Commit {
                id: info.id.to_string(),
                parents: info.parent_ids().map(|id| id.to_string()).collect(),
                author_name: author.name.to_string(),
                author_email: author.email.to_string(),
                time: object.time()?.seconds,
                message,
            });
            if bytes > max_bytes {
                break;
            }
        }
        Ok(commits)
    }

    fn diff(
        &self,
        repo: &Path,
        from: Option<&str>,
        to: &str,
        max_changes: usize,
        max_bytes: usize,
    ) -> Result<Vec<Change>> {
        let repo = open(repo)?;
        let old = match from {
            Some(id) => tree_at(&repo, id, "")?,
            None => repo.empty_tree(),
        };
        let new = tree_at(&repo, to, "")?;
        let mut changes = Vec::new();
        let mut bytes = 0;
        let mut exceeded = None;
        let mut platform = old.changes()?;
        platform.options(|options| {
            options.track_path().track_rewrites(None);
        });
        let walked = platform.for_each_to_obtain_tree(&new, |change| {
            use gix::object::tree::diff::{Action, Change as C};
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
                    Some(id.detach()),
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
                    Some(id.detach()),
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
                    Some(previous_id.detach()),
                    Some(id.detach()),
                    entry_mode.is_tree(),
                ),
                C::Rewrite { .. } => unreachable!("rewrite tracking is disabled"),
            };
            if tree {
                // gix reports a changed subtree before loading it to descend: an
                // oversized one is refused here, unread.
                let oversized = |id: &gix::ObjectId| {
                    repo.find_header(*id)
                        .is_ok_and(|header| header.size() > MAX_TREE_OBJECT_BYTES)
                };
                if old_id.iter().chain(&new_id).any(oversized) {
                    exceeded = Some(Limit::Tree);
                    return Ok(Action::Break(()));
                }
                return Ok(Action::Continue(()));
            }
            bytes += path.len();
            changes.push(Change {
                path: path.to_string(),
                kind,
                old_id: old_id.map(|id| id.to_string()),
                new_id: new_id.map(|id| id.to_string()),
            });
            if changes.len() > max_changes || bytes > max_bytes {
                exceeded = Some(Limit::Diff);
                return Ok(Action::Break(()));
            }
            Ok(Action::Continue(()))
        });
        if let Some(limit) = exceeded {
            return Err(Error::TooLarge(limit));
        }
        walked?;
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
                    let commit = peel_to_commit(&repo, id)?;
                    return Ok((commit.id.to_string(), rest(end)));
                }
            }
        }
        let id = commit(&repo, &spec[..ends[0]])?.id.to_string();
        Ok((id, rest(ends[0])))
    }

    fn transport(&self, service: Service, repo: &Path, options: &[&str]) -> Command {
        let mut command = git();
        command
            .arg(match service {
                Service::UploadPack => "upload-pack",
                Service::ReceivePack => "receive-pack",
            })
            .args(options)
            .arg(repo);
        command
    }

    fn remote_refs(&self, source: &str, credentials: &str) -> Result<Vec<Ref>> {
        let mut command = import_command(source, credentials);
        command.args(["ls-remote", "--refs", "--", source]);
        let mut refs = Vec::new();
        for line in run(command, "", IMPORT_TIMEOUT)?.lines() {
            let (target, name) = line
                .split_once('\t')
                .ok_or_else(|| Error::Git(format!("unexpected ls-remote line: {line}")))?;
            if name.starts_with("refs/heads/") || name.starts_with("refs/tags/") {
                kajiba_core::names::validate_ref(name)
                    .map_err(|err| Error::Git(format!("{name}: {err}")))?;
                refs.push(Ref {
                    name: name.to_owned(),
                    target: target.to_owned(),
                });
            }
        }
        refs.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(refs)
    }

    fn fetch(&self, repo: &Path, source: &str, credentials: &str, refs: &[Ref]) -> Result<()> {
        let mut command = import_command(source, credentials);
        command.arg("--git-dir").arg(repo).args([
            "fetch",
            "--atomic",
            "--no-tags",
            "--no-write-fetch-head",
            "--quiet",
            "--stdin",
            "--",
            source,
        ]);
        let refspecs: String = refs
            .iter()
            .map(|r| format!("{}:{}\n", r.target, r.name))
            .collect();
        run(command, &refspecs, IMPORT_TIMEOUT).map(drop)
    }
}

#[cfg(test)]
mod tests;
