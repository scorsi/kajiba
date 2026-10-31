//! GitStore boundary (0003): gix for reads, the `git` binary for transport.
//!
//! Object ids cross the boundary as lowercase hex strings so that callers never
//! depend on gix types and pieces can be replaced one by one.

pub mod import;
pub mod push;
mod traversal;

use traversal::Traversal;

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
    /// A known push verification limit, with a fixed, client-safe diagnostic.
    VerificationLimit(&'static str),
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
    Traversal,
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
                    Limit::Traversal => "traversal",
                }
            ),
            Self::VerificationLimit(message) => f.write_str(message),
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
    /// The commit named by an object id, peeling annotated tags without resolving refs.
    fn peel_commit(&self, repo: &Path, id: &str) -> Result<String>;
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
        verification: &push::Verification,
    ) -> Result<push::Facts>;
    /// Verify each selected commit once against this session's signing keys.
    fn verify_commits(
        &self,
        repo: &Path,
        updates: &[push::Update<'_>],
        facts: &push::Facts,
        quarantine: &push::Quarantine<'_>,
        verification: &push::Verification,
        allowed_signers: &Path,
    ) -> Result<Vec<push::CommitRefusal>>;
    /// Require an annotated tag signed by one of this session's SSH signing keys.
    fn verify_tag(
        &self,
        repo: &Path,
        tag: &str,
        quarantine: &push::Quarantine<'_>,
        verification: &push::Verification,
        allowed_signers: &Path,
    ) -> Result<bool>;
    /// Writes to `out` a git bundle of `commits`, each a head named [`BUNDLE_REF`], minus
    /// the history of the `have` ids that are commits of the repository (others are
    /// ignored); read only (ADR 0106). The bundle is valid even when `have` already holds
    /// `commits`. Blocking; `NotFound` when one of `commits` is not a commit of the
    /// repository. [`Abort::abort`] from another thread kills its git child.
    fn bundle(
        &self,
        repo: &Path,
        commits: &[String],
        have: &[String],
        out: &mut dyn std::io::Write,
        abort: &Abort,
    ) -> Result<()>;
    /// Whether the history of the `have` ids that are commits of the repository already
    /// holds every one of `commits`; read only, blocking.
    fn covered(&self, repo: &Path, commits: &[String], have: &[String]) -> Result<bool>;
    /// Whether `commit` (a full id) is a commit reachable from any ref of the repository,
    /// whatever its namespace; read only, blocking.
    fn reachable(&self, repo: &Path, commit: &str) -> Result<bool>;
    /// Whether every one of `commits` (full ids) is a commit reachable from
    /// `refs/heads/*` or `refs/tags/*`, as a worktree bundle's prerequisites must be
    /// (ADR 0097); true for none. Read only, blocking.
    fn tips_reach(&self, repo: &Path, commits: &[String]) -> Result<bool>;
}

/// The single ref of a run bundle (ADR 0106).
pub const BUNDLE_REF: &str = "refs/kajiba/run";

/// Kills the git child of a running [`GitStore::bundle`] from another thread, and any
/// child started after the abort.
#[derive(Default)]
pub struct Abort(std::sync::Mutex<(bool, Option<u32>)>);

impl Abort {
    pub fn abort(&self) {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        state.0 = true;
        if let Some(pid) = state.1 {
            // SAFETY: the child is not reaped before `reaping` clears its pid.
            unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
        }
    }

    fn started(&self, child: &mut std::process::Child) {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if state.0 {
            let _ = child.kill();
        } else {
            state.1 = Some(child.id());
        }
    }

    /// Before waiting on the child: its pid may be reused once reaped.
    fn reaping(&self) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).1 = None;
    }
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
        "-c",
        "transfer.fsckObjects=true",
        "-c",
        "fetch.fsckObjects=true",
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
    remaining: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    stop: std::sync::mpsc::Sender<()>,
) -> std::sync::mpsc::Receiver<std::io::Result<Vec<u8>>> {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buffer = Vec::new();
        let result = (|| {
            let mut chunk = [0; 8192];
            loop {
                let count = match pipe.read(&mut chunk) {
                    Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                    result => result?,
                };
                if count == 0 {
                    return Ok(buffer);
                }
                if remaining
                    .fetch_update(
                        std::sync::atomic::Ordering::Relaxed,
                        std::sync::atomic::Ordering::Relaxed,
                        |left| left.checked_sub(count),
                    )
                    .is_err()
                {
                    let _ = stop.send(());
                    return Err(std::io::Error::other("git output limit exceeded"));
                }
                buffer.extend_from_slice(&chunk[..count]);
            }
        })();
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

struct ProcessGroup {
    child: std::process::Child,
    reaped: bool,
}

impl ProcessGroup {
    fn finish(&mut self) -> std::io::Result<std::process::ExitStatus> {
        if !self.reaped {
            // SAFETY: the unreaped child reserves its pid and process group id.
            unsafe { libc::kill(-(self.child.id() as libc::pid_t), libc::SIGKILL) };
        }
        loop {
            match self.child.wait() {
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                result => {
                    self.reaped = result.is_ok();
                    return result;
                }
            }
        }
    }
}

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        if !self.reaped {
            let _ = self.finish();
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

fn run_output(command: Command, stdin: &str, deadline: Duration) -> Result<std::process::Output> {
    run_output_limited(command, stdin, deadline, usize::MAX)
}

fn run_output_limited(
    mut command: Command,
    stdin: &str,
    deadline: Duration,
    output_limit: usize,
) -> Result<std::process::Output> {
    use std::io::Write;
    use std::os::unix::process::CommandExt;
    use std::sync::mpsc::channel;
    let end = std::time::Instant::now() + deadline;
    let mut group = ProcessGroup {
        child: command
            .stdin(Stdio::piped())
            .process_group(0)
            .spawn()
            .map_err(|err| Error::Git(format!("git: {err}")))?,
        reaped: false,
    };
    let pid = group.child.id() as libc::pid_t;
    let mut input = group.child.stdin.take().expect("piped");
    let (exited, exit) = channel::<()>();
    let remaining = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(output_limit));
    let stdout = drain(
        group.child.stdout.take().expect("piped"),
        remaining.clone(),
        exited.clone(),
    );
    let stderr = drain(
        group.child.stderr.take().expect("piped"),
        remaining,
        exited.clone(),
    );
    // Writing can fail if git exits early; its status reports why.
    let stdin = stdin.to_owned();
    std::thread::spawn(move || input.write_all(stdin.as_bytes()));
    std::thread::spawn(move || {
        wait_without_reaping(pid);
        let _ = exited.send(());
    });
    let timed_out = exit
        .recv_timeout(end.saturating_duration_since(std::time::Instant::now()))
        .is_err();
    // Exit, output overflow or deadline: no descendant in the group outlives git.
    // A failed wait must unwind so the supervisor cannot report an unproven exit.
    let status = group.finish().expect("git child could not be reaped");
    if timed_out {
        return Err(Error::Git(format!(
            "git did not finish within {} s",
            deadline.as_secs()
        )));
    }
    // A descendant that left the group (setsid) may still hold a pipe: bounded wait.
    let collect = |pipe: std::sync::mpsc::Receiver<std::io::Result<Vec<u8>>>| {
        pipe.recv_timeout(
            Duration::from_secs(10).min(end.saturating_duration_since(std::time::Instant::now())),
        )
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
    cancelled: Option<&'a std::sync::atomic::AtomicBool>,
}

/// Ordinary ownership checks for callers reading repositories they own.
#[allow(non_upper_case_globals)]
pub const Gix: Gix<'static> = Gix {
    trusted_repos: None,
    cancelled: None,
};

impl<'a> Gix<'a> {
    /// Trust only canonical bare repository paths directly under the configured repos/.
    pub fn shared(repos: &'a Path) -> Self {
        Self {
            trusted_repos: Some(repos),
            cancelled: None,
        }
    }

    /// Stop browsing work cooperatively when its requesting handler is dropped.
    pub fn cancellable(mut self, cancelled: &'a std::sync::atomic::AtomicBool) -> Self {
        self.cancelled = Some(cancelled);
        self
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

    /// `commits` and `have` as full ids of commits of the repository: `NotFound` when one
    /// of `commits` is not one, `have` ids that are not are dropped (sorted, deduplicated).
    fn commits(
        &self,
        repo: &Path,
        commits: &[String],
        have: &[String],
    ) -> Result<(Vec<String>, Vec<String>)> {
        let opened = self.open(repo)?;
        let is_commit = |id: &String| {
            gix::ObjectId::from_hex(id.as_bytes())
                .ok()
                .filter(|id| {
                    opened
                        .try_find_header(*id)
                        .ok()
                        .flatten()
                        .is_some_and(|header| header.kind() == gix::object::Kind::Commit)
                })
                .map(|id| id.to_string())
        };
        let commits = commits
            .iter()
            .map(|id| is_commit(id).ok_or(Error::NotFound))
            .collect::<Result<_>>()?;
        let mut have: Vec<String> = have.iter().filter_map(is_commit).collect();
        have.sort();
        have.dedup();
        Ok((commits, have))
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
fn commit<'r>(
    repo: &'r gix::Repository,
    id: &str,
    work: &mut Traversal<'_>,
) -> Result<gix::Commit<'r>> {
    let id = gix::ObjectId::from_hex(id.as_bytes()).map_err(|_| Error::NotFound)?;
    load_commit(repo, id, work)
}

/// A commit object, its kind and size checked before it is loaded.
fn load_commit<'r>(
    repo: &'r gix::Repository,
    id: gix::ObjectId,
    work: &mut Traversal<'_>,
) -> Result<gix::Commit<'r>> {
    work.check()?;
    let header = header(repo, id)?;
    if header.kind() != gix::object::Kind::Commit {
        return Err(Error::NotFound);
    }
    if header.size() > MAX_COMMIT_OBJECT_BYTES {
        return Err(Error::TooLarge(Limit::Commit));
    }
    work.object(header.size())?;
    Ok(repo.find_object(id)?.into_commit())
}

/// A tree object, its kind and size checked before it is loaded.
fn load_tree<'r>(
    repo: &'r gix::Repository,
    id: gix::ObjectId,
    work: &mut Traversal<'_>,
) -> Result<gix::Tree<'r>> {
    work.check()?;
    let header = header(repo, id)?;
    if header.kind() != gix::object::Kind::Tree {
        return Err(Error::NotFound);
    }
    if header.size() > MAX_TREE_OBJECT_BYTES {
        return Err(Error::TooLarge(Limit::Tree));
    }
    work.object(header.size())?;
    let tree = repo.find_object(id)?.into_tree();
    // Charge unchanged entries too, before gix can scan or enqueue this subtree.
    for entry in tree.iter() {
        work.entry()?;
        entry?;
    }
    work.check()?;
    Ok(tree)
}

fn tree_at<'r>(
    repo: &'r gix::Repository,
    commit_id: &str,
    path: &str,
    work: &mut Traversal<'_>,
) -> Result<gix::Tree<'r>> {
    work.path(path.as_bytes())?;
    let mut id = commit(repo, commit_id, work)?
        .tree_id()
        .map_err(|err| Error::Git(err.to_string()))?
        .detach();
    // Segment by segment, so every tree on the way is checked before it is loaded.
    for segment in path.split('/').filter(|segment| !segment.is_empty()) {
        let tree = load_tree(repo, id, work)?;
        let entry = tree.find_entry(segment).ok_or(Error::NotFound)?;
        if !entry.mode().is_tree() {
            return Err(Error::NotFound);
        }
        id = entry.oid().to_owned();
    }
    load_tree(repo, id, work)
}

impl GitStore for Gix<'_> {
    fn verify_commits(
        &self,
        repo: &Path,
        updates: &[push::Update<'_>],
        facts: &push::Facts,
        quarantine: &push::Quarantine<'_>,
        verification: &push::Verification,
        allowed_signers: &Path,
    ) -> Result<Vec<push::CommitRefusal>> {
        push::verify_commits(repo, updates, facts, quarantine, verification, |commit| {
            push::verify_signature(
                repo,
                commit,
                quarantine,
                verification,
                allowed_signers,
                "verify-commit",
            )
        })
    }

    fn verify_tag(
        &self,
        repo: &Path,
        tag: &str,
        quarantine: &push::Quarantine<'_>,
        verification: &push::Verification,
        allowed_signers: &Path,
    ) -> Result<bool> {
        push::verify_signature(
            repo,
            tag,
            quarantine,
            verification,
            allowed_signers,
            "verify-tag",
        )
    }

    fn bundle(
        &self,
        repo: &Path,
        commits: &[String],
        have: &[String],
        out: &mut dyn std::io::Write,
        abort: &Abort,
    ) -> Result<()> {
        use std::io::Write;
        let (commits, have) = self.commits(repo, commits, have)?;
        // `git bundle create` refuses a bare commit id and an empty bundle, and naming a
        // ref would write the repository: the bundle (v2 format) is assembled here, its
        // prerequisites being the `have` commits, which the receiver holds.
        let mut header = String::from("# v2 git bundle\n");
        for id in &have {
            header.push_str(&format!("-{id}\n"));
        }
        for commit in &commits {
            header.push_str(&format!("{commit} {BUNDLE_REF}\n"));
        }
        header.push('\n');
        let io = |err: std::io::Error| Error::Git(format!("bundle: {err}"));
        out.write_all(header.as_bytes()).map_err(io)?;
        let mut command = git();
        if let Some(path) = self.trusted(repo) {
            command
                .arg("-c")
                .arg(format!("safe.directory={}", path.display()));
        }
        let mut child = command
            .arg("-C")
            .arg(repo)
            .args([
                "pack-objects",
                "--stdout",
                "--thin",
                "--delta-base-offset",
                "--revs",
                "-q",
            ])
            .stdin(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|err| Error::Git(format!("git: {err}")))?;
        abort.started(&mut child);
        let lines = |ids: &[String]| ids.iter().map(|id| format!("{id}\n")).collect::<String>();
        let revs = format!("{}--not\n{}", lines(&commits), lines(&have));
        // Revisions are read whole before the pack is written: no deadlock on the pipes.
        let written = child
            .stdin
            .take()
            .expect("piped")
            .write_all(revs.as_bytes());
        let copied = written.and_then(|()| {
            std::io::copy(&mut child.stdout.take().expect("piped"), out).map(|_| ())
        });
        if copied.is_err() {
            let _ = child.kill();
        }
        abort.reaping();
        let status = child.wait().map_err(io)?;
        copied.map_err(io)?;
        if !status.success() {
            return Err(Error::Git(format!("git pack-objects: {status}")));
        }
        Ok(())
    }

    fn covered(&self, repo: &Path, commits: &[String], have: &[String]) -> Result<bool> {
        let (commits, have) = self.commits(repo, commits, have)?;
        if commits.is_empty() {
            return Ok(true);
        }
        let mut command = git();
        if let Some(path) = self.trusted(repo) {
            command
                .arg("-c")
                .arg(format!("safe.directory={}", path.display()));
        }
        let mut arguments = vec!["rev-list".to_owned(), "--max-count=1".to_owned()];
        arguments.extend(commits);
        arguments.push("--not".into());
        arguments.extend(have);
        let output = command
            .arg("-C")
            .arg(repo)
            .args(&arguments)
            .stdin(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .map_err(|err| Error::Git(format!("git: {err}")))?;
        if !output.status.success() {
            return Err(Error::Git(format!(
                "git rev-list: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        Ok(output.stdout.is_empty())
    }

    fn tips_reach(&self, repo: &Path, commits: &[String]) -> Result<bool> {
        let opened = self.open(repo)?;
        let is_commit = |commit: &String| {
            commit
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
                && gix::ObjectId::from_hex(commit.as_bytes())
                    .ok()
                    .and_then(|id| opened.try_find_header(id).ok().flatten())
                    .is_some_and(|header| header.kind() == gix::object::Kind::Commit)
        };
        if !commits.iter().all(is_commit) {
            return Ok(false);
        }
        if commits.is_empty() {
            return Ok(true);
        }
        let mut command = git();
        if let Some(path) = self.trusted(repo) {
            command
                .arg("-c")
                .arg(format!("safe.directory={}", path.display()));
        }
        // The commits come on stdin, unaffected by `--not`: any output is one the
        // branches and tags do not reach.
        let mut child = command
            .arg("-C")
            .arg(repo)
            .args([
                "rev-list",
                "--max-count=1",
                "--not",
                "--branches",
                "--tags",
                "--stdin",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| Error::Git(format!("git: {err}")))?;
        let input = commits
            .iter()
            .map(|id| format!("{id}\n"))
            .collect::<String>();
        let mut stdin = child.stdin.take().expect("piped");
        // Written from a thread: rev-list may answer before reading all of it.
        let writer = std::thread::spawn(move || {
            let _ = std::io::Write::write_all(&mut stdin, input.as_bytes());
        });
        let output = child
            .wait_with_output()
            .map_err(|err| Error::Git(format!("git: {err}")))?;
        let _ = writer.join();
        if !output.status.success() {
            return Err(Error::Git(format!(
                "git rev-list: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        Ok(output.stdout.is_empty())
    }

    fn reachable(&self, repo: &Path, commit: &str) -> Result<bool> {
        let opened = self.open(repo)?;
        let lowercase = commit
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
        let is_commit = gix::ObjectId::from_hex(commit.as_bytes())
            .ok()
            .filter(|_| lowercase)
            .and_then(|id| opened.try_find_header(id).ok().flatten())
            .is_some_and(|header| header.kind() == gix::object::Kind::Commit);
        if !is_commit {
            return Ok(false);
        }
        let mut command = git();
        if let Some(path) = self.trusted(repo) {
            command
                .arg("-c")
                .arg(format!("safe.directory={}", path.display()));
        }
        let output = command
            .arg("-C")
            .arg(repo)
            .args([
                "for-each-ref",
                "--count=1",
                "--format=%(refname)",
                "--contains",
                commit,
            ])
            .stdin(Stdio::null())
            .output()
            .map_err(|err| Error::Git(format!("git: {err}")))?;
        if !output.status.success() {
            return Err(Error::Git(format!(
                "git for-each-ref: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        Ok(!output.stdout.is_empty())
    }

    fn push_facts(
        &self,
        repo: &Path,
        updates: &[push::Update<'_>],
        quarantine: &push::Quarantine<'_>,
        verification: &push::Verification,
    ) -> Result<push::Facts> {
        push::facts(repo, updates, quarantine, verification)
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
        for (key, value) in [
            ("core.fsync", "all"),
            ("receive.fsckObjects", "true"),
            ("transfer.fsckObjects", "true"),
        ] {
            let mut command = git();
            command
                .arg("--git-dir")
                .arg(path)
                .args(["config", key, value]);
            run(command, "", IMPORT_TIMEOUT)?;
        }
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
        let mut work = Traversal::new(self.cancelled);
        let tree = tree_at(&repo, commit, path, &mut work)?;
        let mut entries = Vec::new();
        for entry in tree.iter() {
            work.check()?;
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
        work.check()?;
        Ok(entries)
    }

    fn blob(&self, repo: &Path, commit: &str, path: &str, limit: u64) -> Result<Vec<u8>> {
        let repo = self.open(repo)?;
        let mut work = Traversal::new(self.cancelled);
        work.path(path.as_bytes())?;
        let (dir, name) = path.rsplit_once('/').unwrap_or(("", path));
        let tree = tree_at(&repo, commit, dir, &mut work)?;
        let entry = tree.find_entry(name).ok_or(Error::NotFound)?;
        if entry.mode().is_tree() || entry.mode().is_commit() {
            return Err(Error::NotFound);
        }
        if repo.find_header(entry.id())?.size() > limit {
            return Err(Error::TooLarge(Limit::Blob));
        }
        let data = entry.object()?.detach().data;
        work.check()?;
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
        let mut work = Traversal::new(self.cancelled);
        let tip = commit(&repo, commit_id, &mut work)?;
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
            let object = load_commit(&repo, id, &mut work)?;
            let mut parents = Vec::new();
            for parent in object.parent_ids() {
                work.entry()?;
                parents.push(parent.detach());
            }
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
                work.check()?;
                if !seen.contains(&parent) {
                    if queue.len() == traversal::MAX_FRONTIER {
                        return Err(Error::TooLarge(Limit::Traversal));
                    }
                    let time = load_commit(&repo, parent, &mut work)?.time()?.seconds;
                    seen.insert(parent);
                    queue.push((time, Reverse(seen.len()), parent));
                }
            }
        }
        work.check()?;
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
        let mut work = Traversal::new(self.cancelled);
        let old = match from {
            Some(id) => tree_at(&repo, id, "", &mut work)?,
            None => repo.empty_tree(),
        };
        let new = tree_at(&repo, to, "", &mut work)?;
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
            if let Err(err) = work.path(path) {
                exceeded = Some(err);
                return Ok(Action::Break(()));
            }
            if tree {
                // gix reports subtrees before descent; account for each occurrence,
                // even when the same object appears at many different paths.
                for id in old_id.iter().chain(&new_id) {
                    if let Err(err) = load_tree(&repo, *id, &mut work) {
                        exceeded = Some(err);
                        return Ok(Action::Break(()));
                    }
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
                exceeded = Some(Error::TooLarge(Limit::Diff));
                return Ok(Action::Break(()));
            }
            Ok(Action::Continue(()))
        });
        if let Some(err) = exceeded {
            return Err(err);
        }
        walked?;
        work.check()?;
        Ok(changes)
    }

    fn peel_commit(&self, repo: &Path, id: &str) -> Result<String> {
        let repo = self.open(repo)?;
        let id = gix::ObjectId::from_hex(id.as_bytes()).map_err(|_| Error::NotFound)?;
        let commit = peel_to_commit(&repo, id)?;
        Ok(commit.id.to_string())
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
        let id = commit(&repo, &spec[..ends[0]], &mut Traversal::new(self.cancelled))?
            .id
            .to_string();
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
        if service == Service::ReceivePack {
            command
                .args(["-c", "receive.fsckObjects=true"])
                .arg("-c")
                .arg(format!("receive.maxInputSize={}", push::MAX_INPUT_BYTES))
                // Keep deltas packed; loose objects can amplify a small input.
                .args(["-c", "receive.unpackLimit=0", "-c", "receive.autogc=false"]);
            // SAFETY: the callback only calls getrlimit/setrlimit in the child.
            unsafe { command.pre_exec(push::limit_file_size) };
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
