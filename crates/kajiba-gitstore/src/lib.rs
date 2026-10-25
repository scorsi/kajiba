//! GitStore boundary (0003): gix for reads, the `git` binary for transport.
//!
//! Object ids cross the boundary as lowercase hex strings so that callers never
//! depend on gix types and pieces can be replaced one by one.

pub mod import;
pub mod push;

use std::fmt;
use std::os::unix::process::CommandExt;
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
    Revision,
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
                    Limit::Revision => "revision",
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

/// Input and candidate-lookup budget for revision resolution (ADR 0056).
pub const MAX_REVISION_BYTES: usize = 4096;
pub const MAX_REVISION_COMPONENTS: usize = 128;

pub fn check_revision(spec: &str) -> Result<()> {
    if spec.len() > MAX_REVISION_BYTES
        || spec.bytes().filter(|&byte| byte == b'/').count() >= MAX_REVISION_COMPONENTS
    {
        return Err(Error::TooLarge(Limit::Revision));
    }
    Ok(())
}

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

impl Commit {
    /// What a history page keeps of this commit, counted toward its byte budget
    /// (ADR 0051): message, author name, author email and parent ids.
    pub fn page_bytes(&self) -> usize {
        self.message.len()
            + self.author_name.len()
            + self.author_email.len()
            + self.parents.iter().map(String::len).sum::<usize>()
    }
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
    /// `page_bytes` take the page past `max_bytes`.
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
    fn transport(
        &self,
        service: Service,
        repo: &Path,
        options: &[&str],
        hooks: Option<&Path>,
    ) -> Command;
    /// The branches and tags (`refs/heads/*`, `refs/tags/*`) of a path, bundle or
    /// URL, sorted by name; blocking. `credentials` is `user:token` or a token alone
    /// for an http(s) source, empty otherwise (ADR 0044).
    fn remote_refs(&self, source: &str, credentials: &str) -> Result<Vec<Ref>>;
    /// Fetches exactly these ids into these refs of `repo`, all or none
    /// (`git fetch --atomic`, no tag following); blocking.
    fn fetch(&self, repo: &Path, source: &str, credentials: &str, refs: &[Ref]) -> Result<()>;
    /// Push-wide new commits and ordered ancestry facts, reading quarantined objects.
    fn push_facts(
        &self,
        repo: &Path,
        updates: &[push::Update<'_>],
        quarantine: &push::Quarantine<'_>,
    ) -> Result<push::Facts>;
    /// Verify each selected commit once against this session's signing keys.
    fn verify_commits(
        &self,
        repo: &Path,
        updates: &[push::Update<'_>],
        facts: &push::Facts,
        quarantine: &push::Quarantine<'_>,
        allowed_signers: &Path,
    ) -> Result<Vec<push::CommitRefusal>>;
    /// Require an annotated tag signed by one of this session's SSH signing keys.
    fn verify_tag(
        &self,
        repo: &Path,
        tag: &str,
        quarantine: &push::Quarantine<'_>,
        allowed_signers: &Path,
    ) -> Result<bool>;
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
    // Group-read, never group-write, whatever the ambient umask (ADR 0055).
    // SAFETY: umask(2) is async-signal-safe and touches no shared state.
    unsafe {
        command.pre_exec(|| {
            libc::umask(0o027);
            Ok(())
        });
    }
    command
}

/// Name under which the import source is configured in the child's environment:
/// the child's command line only ever names the remote. The transport helper git
/// starts from it (`git-remote-https`, `ssh`) gets the full source as an argument
/// (ADR 0049), so a secret in the source's path is not hidden from local users.
const IMPORT_REMOTE: &str = "kajiba-import";

/// A git child reading an import source: the source and the credentials travel in
/// the environment, never on the child's own command line (readable by every local
/// user; the credentials never reach any command line, the source reaches the
/// transport helper's, ADR 0049), and
/// the source cannot steer the protocol or run hooks. `credentials` is `user:token`
/// or a token alone (then the user name is `token`, which every forge accepts with
/// a token).
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
    let url_key = format!("remote.{IMPORT_REMOTE}.url");
    let mut config = vec![
        ("core.hooksPath", "/dev/null".to_owned()),
        // A stalled transfer ends on its own; IMPORT_TIMEOUT is the backstop.
        ("http.lowSpeedLimit", "1".to_owned()),
        ("http.lowSpeedTime", "60".to_owned()),
        (url_key.as_str(), source.to_owned()),
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

/// `git ls-remote` of an import source (ADR 0041).
fn ls_remote_command(source: &str, credentials: &str) -> Command {
    let mut command = import_command(source, credentials);
    command.args(["ls-remote", "--refs", "--", IMPORT_REMOTE]);
    command
}

/// `git fetch` of an import source into `repo`, refspecs on stdin (ADR 0041).
fn fetch_command(repo: &Path, source: &str, credentials: &str) -> Command {
    let mut command = import_command(source, credentials);
    command.arg("--git-dir").arg(repo).args([
        "fetch",
        "--atomic",
        "--no-tags",
        "--no-write-fetch-head",
        "--quiet",
        "--stdin",
        "--",
        IMPORT_REMOTE,
    ]);
    command
}

/// Reads a pipe to its end on a thread of its own, so a chatty or stuck pipe never
/// blocks the deadline.
fn drain(
    mut pipe: impl std::io::Read + Send + 'static,
) -> std::sync::mpsc::Receiver<std::io::Result<Vec<u8>>> {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buffer = Vec::new();
        let result = pipe.read_to_end(&mut buffer).map(|_| buffer);
        let _ = sender.send(result);
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
fn run(command: Command, stdin: &str, deadline: Duration) -> Result<String> {
    let output = run_output(command, stdin, deadline)?;
    if !output.status.success() {
        return Err(Error::Git(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }
    String::from_utf8(output.stdout).map_err(|err| Error::Git(err.to_string()))
}

fn run_output(
    mut command: Command,
    stdin: &str,
    deadline: Duration,
) -> Result<std::process::Output> {
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
    let collect = |pipe: std::sync::mpsc::Receiver<std::io::Result<Vec<u8>>>| {
        pipe.recv_timeout(Duration::from_secs(10))
            .map_err(|err| Error::Git(format!("git output: {err}")))?
            .map_err(|err| Error::Git(format!("git output: {err}")))
    };
    Ok(std::process::Output {
        status,
        stdout: collect(stdout)?,
        stderr: collect(stderr)?,
    })
}

/// The gix + `git` binary implementation.
pub struct Gix<'a> {
    trusted_repos: Option<&'a Path>,
}

/// Ordinary ownership checks for callers reading repositories they own.
#[allow(non_upper_case_globals)]
pub const Gix: Gix<'static> = Gix {
    trusted_repos: None,
};

impl<'a> Gix<'a> {
    /// Trust only canonical bare repository paths directly under the configured repos/.
    pub fn shared(repos: &'a Path) -> Self {
        Self {
            trusted_repos: Some(repos),
        }
    }

    fn trusted(&self, repo: &Path) -> Option<std::path::PathBuf> {
        let root = self.trusted_repos?.canonicalize().ok()?;
        let path = repo.canonicalize().ok()?;
        (path.parent() == Some(root.as_path())
            && path
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| name.strip_suffix(".git"))
                .is_some_and(kajiba_core::names::is_public_id)
            && std::fs::symlink_metadata(repo).is_ok_and(|meta| meta.is_dir())
            && !path.join("commondir").exists())
        .then_some(path)
    }

    fn open(&self, repo: &Path) -> Result<gix::Repository> {
        let mut options = gix::open::Options::isolated().bail_if_untrusted(true);
        if self.trusted(repo).is_some() {
            options = options.with(gix::sec::Trust::Full);
        }
        Ok(gix::open_opts(repo, options)?)
    }
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
    load_commit(repo, id)
}

/// A commit object, its kind and size checked before it is loaded.
fn load_commit(repo: &gix::Repository, id: gix::ObjectId) -> Result<gix::Commit<'_>> {
    let header = header(repo, id)?;
    if header.kind() != gix::object::Kind::Commit {
        return Err(Error::NotFound);
    }
    if header.size() > MAX_COMMIT_OBJECT_BYTES {
        return Err(Error::TooLarge(Limit::Commit));
    }
    Ok(repo.find_object(id)?.into_commit())
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

impl GitStore for Gix<'_> {
    fn verify_commits(
        &self,
        repo: &Path,
        updates: &[push::Update<'_>],
        facts: &push::Facts,
        quarantine: &push::Quarantine<'_>,
        allowed_signers: &Path,
    ) -> Result<Vec<push::CommitRefusal>> {
        push::verify_commits(repo, updates, facts, quarantine, |commit| {
            push::verify_signature(repo, commit, quarantine, allowed_signers, "verify-commit")
        })
    }

    fn verify_tag(
        &self,
        repo: &Path,
        tag: &str,
        quarantine: &push::Quarantine<'_>,
        allowed_signers: &Path,
    ) -> Result<bool> {
        push::verify_signature(repo, tag, quarantine, allowed_signers, "verify-tag")
    }

    fn push_facts(
        &self,
        repo: &Path,
        updates: &[push::Update<'_>],
        quarantine: &push::Quarantine<'_>,
    ) -> Result<push::Facts> {
        push::facts(repo, updates, quarantine)
    }

    fn init_bare(&self, path: &Path, default_branch: &str) -> Result<()> {
        kajiba_core::names::validate_ref(&format!("refs/heads/{default_branch}"))
            .map_err(|err| Error::Git(err.to_string()))?;
        let mut command = git();
        command
            .args(["-c", "core.fsync=all", "init", "--bare", "--template="])
            .arg(format!("--initial-branch={default_branch}"))
            .arg(path);
        run(command, "", IMPORT_TIMEOUT)?;
        let mut command = git();
        command
            .arg("--git-dir")
            .arg(path)
            .args(["config", "core.fsync", "all"]);
        run(command, "", IMPORT_TIMEOUT)?;
        Ok(())
    }

    fn set_head(&self, repo: &Path, branch: &str) -> Result<()> {
        let head = format!("refs/heads/{branch}");
        kajiba_core::names::validate_ref(&head).map_err(|err| Error::Git(err.to_string()))?;
        let path = repo.join("HEAD");
        if std::fs::symlink_metadata(&path).is_ok_and(|meta| meta.is_file())
            && std::fs::read(&path).is_ok_and(|bytes| bytes == format!("ref: {head}\n").as_bytes())
        {
            return Ok(());
        }
        // Re-init repairs an interrupted HEAD replacement without touching refs.
        let metadata = std::fs::symlink_metadata(&path);
        let missing = metadata
            .as_ref()
            .is_err_and(|err| err.kind() == std::io::ErrorKind::NotFound);
        if missing
            || metadata.is_ok_and(|meta| meta.is_symlink())
            || std::fs::read(&path).is_ok_and(|bytes| bytes.is_empty())
        {
            kajiba_core::state::real_dir(&repo.join("objects"))
                .map_err(|err| Error::Git(err.to_string()))?;
            kajiba_core::state::real_dir(&repo.join("refs"))
                .map_err(|err| Error::Git(err.to_string()))?;
            if !missing {
                std::fs::remove_file(&path).map_err(|err| Error::Git(err.to_string()))?;
            }
            self.init_bare(repo, branch)?;
        }
        let mut command = git();
        command
            .arg("--git-dir")
            .arg(repo)
            .args(["symbolic-ref", "HEAD", &head]);
        run(command, "", IMPORT_TIMEOUT)?;
        std::fs::File::open(repo.join("HEAD"))
            .and_then(|f| f.sync_all())
            .and_then(|()| kajiba_core::state::sync_dir(repo))
            .map_err(|err| Error::Git(err.to_string()))
    }

    fn refs(&self, repo: &Path) -> Result<Vec<Ref>> {
        let repo = self.open(repo)?;
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
        let repo = self.open(repo)?;
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
        let repo = self.open(repo)?;
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
        use std::cmp::Reverse;
        use std::collections::{BinaryHeap, HashSet};
        let repo = self.open(repo)?;
        let tip = commit(&repo, commit_id)?;
        // The product's own commit-time walk (ADR 0051): newest first, insertion order
        // among equal times; every ancestor's header is checked by `load_commit` before
        // it is read, the ones passed over by `skip` included, and the parents of the
        // commit that completes a page are not read at all.
        let mut queue = BinaryHeap::from([(tip.time()?.seconds, Reverse(0), tip.id)]);
        let mut seen = HashSet::from([tip.id]);
        let mut commits = Vec::new();
        let mut bytes = 0;
        let mut skipped = 0;
        while let Some((_, _, id)) = queue.pop() {
            let object = load_commit(&repo, id)?;
            let parents: Vec<gix::ObjectId> = object.parent_ids().map(|id| id.detach()).collect();
            if skipped < skip {
                skipped += 1;
            } else {
                let author = object.author()?;
                let commit = Commit {
                    id: id.to_string(),
                    parents: parents.iter().map(|id| id.to_string()).collect(),
                    author_name: author.name.to_string(),
                    author_email: author.email.to_string(),
                    time: object.time()?.seconds,
                    message: object.message_raw_sloppy().to_string(),
                };
                bytes += commit.page_bytes();
                commits.push(commit);
            }
            if commits.len() >= limit || bytes > max_bytes {
                break;
            }
            // shortcut: a parent is decoded here for its time and again when popped; cache it if walks get slow.
            for parent in parents {
                if seen.insert(parent) {
                    let time = load_commit(&repo, parent)?.time()?.seconds;
                    queue.push((time, Reverse(seen.len()), parent));
                }
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
        let repo = self.open(repo)?;
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
        check_revision(spec)?;
        let refs = self.refs(repo_path)?;
        let repo = self.open(repo_path)?;
        let ends: Vec<usize> = spec
            .match_indices('/')
            .map(|(end, _)| end)
            .chain([spec.len()])
            .collect();
        let rest = |end: usize| spec.get(end + 1..).unwrap_or("");
        for namespace in ["refs/heads/", "refs/tags/"] {
            let mut name = format!("{namespace}{spec}");
            for &end in ends.iter().rev() {
                name.truncate(namespace.len() + end);
                // refs() is sorted; each bounded candidate needs only a binary search.
                if let Ok(index) = refs.binary_search_by(|reference| reference.name.cmp(&name)) {
                    let id = gix::ObjectId::from_hex(refs[index].target.as_bytes())
                        .map_err(|err| Error::Git(err.to_string()))?;
                    let commit = peel_to_commit(&repo, id)?;
                    return Ok((commit.id.to_string(), rest(end)));
                }
            }
        }
        let id = commit(&repo, &spec[..ends[0]])?.id.to_string();
        Ok((id, rest(ends[0])))
    }

    fn transport(
        &self,
        service: Service,
        repo: &Path,
        options: &[&str],
        hooks: Option<&Path>,
    ) -> Command {
        let mut command = git();
        if let Some(path) = self.trusted(repo) {
            command
                .arg("-c")
                .arg(format!("safe.directory={}", path.display()));
        }
        if let Some(hooks) = hooks {
            command
                .arg("-c")
                .arg(format!("core.hooksPath={}", hooks.display()));
        }
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
        import::check_source(source).map_err(|err| Error::Git(err.into()))?;
        let mut refs = Vec::new();
        for line in run(ls_remote_command(source, credentials), "", IMPORT_TIMEOUT)?.lines() {
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
        import::check_source(source).map_err(|err| Error::Git(err.into()))?;
        let refspecs: String = refs
            .iter()
            .map(|r| format!("{}:{}\n", r.target, r.name))
            .collect();
        run(
            fetch_command(repo, source, credentials),
            &refspecs,
            IMPORT_TIMEOUT,
        )
        .map(drop)
    }
}

#[cfg(test)]
mod tests;
