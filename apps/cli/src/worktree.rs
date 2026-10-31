//! `kajiba ci run --worktree` (0097, ADR 0118): capture the working tree's tracked files
//! as a commit, send the forge only the objects it lacks as a git bundle, and start a run.
use crate::client::{self, Api, Error};
use kajiba_proto::v1::{
    CreateWorktreeRunRequest, CreateWorktreeRunResponse, GetWorktreeBaseRequest, Run,
    WorktreeRunSpec, create_worktree_run_request::Part, run_service_client::RunServiceClient,
};
use std::{
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
use tonic_types::StatusExt;

/// What a worktree run is made from: the forge the token is for, the `nix` program that
/// evaluates the checks' features, and the clone.
pub struct Local {
    pub forge: String,
    pub nix: PathBuf,
    pub dir: PathBuf,
}

/// The command line's choices: `--repo`, `--check` (repeatable) and `--system`.
pub struct Choice {
    pub repo: Option<String>,
    pub checks: Vec<String>,
    pub system: Option<String>,
}

/// A request message holds at most 16 KiB (0057).
const CHUNK: usize = 16_000;

/// Create the run; the transient ref is gone when this returns, whatever the outcome.
pub async fn create(
    runs: &mut RunServiceClient<Api>,
    local: &Local,
    choice: Choice,
) -> Result<Run, Error> {
    let repo = match choice.repo {
        Some(repo) => repo,
        None => repository(&local.dir, &local.forge)?,
    };
    // Names go into a Nix expression: refused before, as the forge would (0097).
    if let Some(check) = choice.checks.iter().find(|check| !is_check(check)) {
        return Err(Error(format!(
            "invalid check name {check:?}: letters, digits, '_' and '-' only"
        )));
    }
    let system = choice.system.unwrap_or_else(own_system);
    let captured = capture(&local.dir)?;
    let transient = Transient::new(&local.dir, &captured)?;
    let features = features(local, &system, &choice.checks)?;
    let spec = WorktreeRunSpec {
        repo,
        system,
        checks: choice.checks,
        features,
    };
    let mut renegotiated = false;
    loop {
        let base = runs
            .get_worktree_base(GetWorktreeBaseRequest {
                spec: Some(spec.clone()),
            })
            .await;
        let bundle = transient.bundle(&client::answer(base)?.ids)?;
        match upload(runs, &spec, bundle).await {
            // A ref moved between the two calls: once more (0097).
            Err(status) if !renegotiated && reason(&status) == "BUNDLE_PREREQUISITE_MISSING" => {
                renegotiated = true;
            }
            result => {
                return client::answer(result)?
                    .run
                    .ok_or_else(|| Error("the forge answered no run".into()));
            }
        }
    }
}

fn reason(status: &tonic::Status) -> String {
    status
        .get_error_details()
        .error_info()
        .map(|info| info.reason.clone())
        .unwrap_or_default()
}

async fn upload(
    runs: &mut RunServiceClient<Api>,
    spec: &WorktreeRunSpec,
    bundle: Vec<u8>,
) -> Result<tonic::Response<CreateWorktreeRunResponse>, tonic::Status> {
    let mut parts = vec![CreateWorktreeRunRequest {
        part: Some(Part::Spec(spec.clone())),
    }];
    parts.extend(bundle.chunks(CHUNK).map(|chunk| CreateWorktreeRunRequest {
        part: Some(Part::Chunk(chunk.to_vec())),
    }));
    runs.create_worktree_run(tonic::codegen::tokio_stream::iter(parts))
        .await
}

fn is_check(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// This machine's Nix system, e.g. `aarch64-darwin`.
fn own_system() -> String {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        os => os,
    };
    format!("{}-{os}", std::env::consts::ARCH)
}

/// `git` in the clone, with the user's configuration (its remotes and identity); `input`
/// goes to stdin. Its trimmed output.
fn git(dir: &Path, args: &[&str], input: Option<&str>) -> Result<String, Error> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| Error(format!("git: {err}")))?;
    if let Some(input) = input {
        let mut stdin = child.stdin.take().expect("piped");
        let input = input.to_owned();
        // Written from a thread: git may answer before reading all of it.
        std::thread::spawn(move || {
            let _ = stdin.write_all(input.as_bytes());
        });
    }
    let output = child
        .wait_with_output()
        .map_err(|err| Error(format!("git: {err}")))?;
    if !output.status.success() {
        return Err(Error(format!(
            "git {}: {}",
            args.first().unwrap_or(&""),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    String::from_utf8(output.stdout)
        .map(|out| out.trim().to_owned())
        .map_err(|_| Error(format!("git {}: output is not UTF-8", args[0])))
}

/// The repository the clone's remotes address on `forge` (0021: HTTP or SSH form); none
/// or several different ones is an error naming them.
fn repository(dir: &Path, forge: &str) -> Result<String, Error> {
    // No remote at all: `git config` exits 1.
    let urls =
        git(dir, &["config", "--get-regexp", r"^remote\..*\.url$"], None).unwrap_or_default();
    let mut found: Vec<String> = urls
        .lines()
        .filter_map(|line| line.split_once(' '))
        .filter_map(|(_, url)| addressed(url, forge))
        .collect();
    found.sort_by_key(|repo| repo.to_lowercase());
    found.dedup_by_key(|repo| repo.to_lowercase());
    match found.as_slice() {
        [repo] => Ok(repo.clone()),
        [] => Err(Error(format!(
            "no remote of this clone addresses {forge}; pass --repo <owner>/<name>"
        ))),
        several => Err(Error(format!(
            "remotes address several repositories of {forge} ({}); pass --repo <owner>/<name>",
            several.join(", ")
        ))),
    }
}

/// `<owner>/<name>` when `url` is one of the forge's repository URLs: `<forge>/<owner>/
/// <name>[.git]`, `ssh://kajiba-git@<host>[:<port>]/<owner>/<name>[.git]` or
/// `kajiba-git@<host>:<owner>/<name>[.git]`, `<host>` being the forge URL's.
fn addressed(url: &str, forge: &str) -> Option<String> {
    let forge = forge.trim_end_matches('/');
    let authority = forge.split_once("://")?.1;
    let host = authority
        .rsplit_once(':')
        .map_or(authority, |(host, _)| host);
    let path = url
        .strip_prefix(forge)
        .and_then(|rest| rest.strip_prefix('/'))
        .or_else(|| {
            let rest = url.strip_prefix("ssh://kajiba-git@")?.strip_prefix(host)?;
            let rest = match rest.strip_prefix(':') {
                Some(port) => port.trim_start_matches(|c: char| c.is_ascii_digit()),
                None => rest,
            };
            rest.strip_prefix('/')
        })
        .or_else(|| {
            url.strip_prefix("kajiba-git@")?
                .strip_prefix(host)?
                .strip_prefix(':')
        })?;
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    match path.split('/').collect::<Vec<_>>().as_slice() {
        [owner, name] if !owner.is_empty() && !name.is_empty() => Some(path.to_owned()),
        _ => None,
    }
}

/// The tracked files of the working tree and index as a commit: `git stash create`, or
/// `HEAD` when nothing tracked changed (0097).
fn capture(dir: &Path) -> Result<String, Error> {
    let head = git(dir, &["rev-parse", "--verify", "-q", "HEAD^{commit}"], None)
        .map_err(|_| Error("HEAD has no commit yet: commit once, then run again".into()))?;
    let stash = git(dir, &["stash", "create"], None)?;
    Ok(if stash.is_empty() { head } else { stash })
}

/// The union of the selected checks' `requiredSystemFeatures` (all checks of the system
/// when none is selected), evaluated from the working tree as Nix sees it.
fn features(local: &Local, system: &str, checks: &[String]) -> Result<Vec<String>, Error> {
    let top = git(&local.dir, &["rev-parse", "--show-toplevel"], None)?;
    let names = if checks.is_empty() {
        "builtins.attrNames checks".to_owned()
    } else {
        format!(
            "[ {} ]",
            checks
                .iter()
                .map(|c| format!("\"{c}\""))
                .collect::<Vec<_>>()
                .join(" ")
        )
    };
    let apply = format!(
        "checks: builtins.concatLists (map (name: checks.${{name}}.requiredSystemFeatures or [ ]) ({names}))"
    );
    let output = Command::new(&local.nix)
        .args(["eval", "--json"])
        .arg(format!("git+file://{top}#checks.{system}"))
        .args(["--apply", &apply])
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .map_err(|err| Error(format!("{}: {err}", local.nix.display())))?;
    if !output.status.success() {
        return Err(Error(format!(
            "nix eval of checks.{system} failed ({})",
            output.status
        )));
    }
    let mut features: Vec<String> = serde_json::from_slice(&output.stdout)
        .map_err(|err| Error(format!("nix eval of checks.{system}: {err}")))?;
    features.sort();
    features.dedup();
    Ok(features)
}

/// The local ref `git bundle` needs for the captured commit, deleted when dropped.
struct Transient {
    dir: PathBuf,
    name: String,
    captured: String,
}

impl Transient {
    fn new(dir: &Path, captured: &str) -> Result<Self, Error> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let name = format!("refs/kajiba/worktree/{}-{nanos}", std::process::id());
        git(dir, &["update-ref", &name, captured], None)?;
        Ok(Self {
            dir: dir.into(),
            name,
            captured: captured.into(),
        })
    }

    /// The captured commit minus what the forge's `ids` present here already hold.
    fn bundle(&self, ids: &[String]) -> Result<Vec<u8>, Error> {
        let listed = ids.iter().map(|id| format!("{id}\n")).collect::<String>();
        let present = git(
            &self.dir,
            &["cat-file", "--batch-check=%(objectname) %(objecttype)"],
            Some(&listed),
        )?;
        let kept: Vec<&str> = present
            .lines()
            .filter_map(|line| line.split_once(' '))
            .filter(|(_, kind)| *kind != "missing")
            .map(|(id, _)| id)
            .collect();
        let not = |ids: &[&str]| {
            format!(
                "--not\n{}",
                ids.iter().map(|id| format!("{id}\n")).collect::<String>()
            )
        };
        let missing = git(
            &self.dir,
            &["rev-list", "--max-count=1", &self.captured, "--stdin"],
            Some(&not(&kept)),
        )?;
        // Already on the forge (a clean tree at a pushed commit): the commit alone, over
        // its parents, since git refuses an empty bundle.
        let excluded = if missing.is_empty() {
            git(
                &self.dir,
                &["rev-parse", &format!("{}^@", self.captured)],
                None,
            )?
        } else {
            kept.join("\n")
        };
        let excluded: Vec<&str> = excluded.lines().filter(|id| !id.is_empty()).collect();
        let mut child = Command::new("git")
            .arg("-C")
            .arg(&self.dir)
            .args(["bundle", "create", "-q", "-", &self.name, "--stdin"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| Error(format!("git: {err}")))?;
        let mut stdin = child.stdin.take().expect("piped");
        let input = not(&excluded);
        std::thread::spawn(move || {
            let _ = stdin.write_all(input.as_bytes());
        });
        let output = child
            .wait_with_output()
            .map_err(|err| Error(format!("git: {err}")))?;
        if !output.status.success() {
            return Err(Error(format!(
                "git bundle: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        Ok(output.stdout)
    }
}

impl Drop for Transient {
    fn drop(&mut self) {
        let _ = git(&self.dir, &["update-ref", "-d", &self.name], None);
    }
}

#[cfg(test)]
mod tests;
