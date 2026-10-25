//! Object checks run by the writer with the hook's quarantine environment (0053).
use crate::{Error, Result, git, run, run_output};
use std::{path::Path, process::Command, time::Duration};

const ZERO: &str = "0000000000000000000000000000000000000000";
const TIMEOUT: Duration = Duration::from_secs(60 * 60);

pub struct Update<'a> {
    pub old_id: &'a str,
    pub new_id: &'a str,
}

#[derive(Default)]
pub struct Quarantine<'a> {
    pub object_directory: &'a str,
    pub alternate_object_directories: &'a str,
    pub quarantine_path: &'a str,
}

#[derive(Debug)]
pub struct Facts {
    /// Each commit reachable from new tips but no pre-push ref, exactly once.
    pub commits: Vec<String>,
    /// In update order; creations and deletions are false (no ancestry pair).
    pub fast_forward: Vec<bool>,
}

pub(crate) fn facts(
    repo: &Path,
    updates: &[Update<'_>],
    quarantine: &Quarantine<'_>,
) -> Result<Facts> {
    for update in updates {
        for id in [update.old_id, update.new_id] {
            if id.len() != 40 || !id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
                return Err(Error::Git("invalid push object id".into()));
            }
        }
    }
    quarantine.command(repo)?;
    let tips: String = updates
        .iter()
        .filter(|u| u.new_id != ZERO)
        .map(|u| format!("{}\n", u.new_id))
        .collect();
    let commits = if tips.is_empty() {
        Vec::new()
    } else {
        let mut cmd = quarantine.command(repo)?;
        // --all is evaluated before receive-pack changes any ref; stdin avoids ARG_MAX.
        cmd.args(["rev-list", "--stdin"]);
        run(cmd, &format!("{tips}--not\n--all\n"), TIMEOUT)?
            .lines()
            .map(String::from)
            .collect()
    };
    let fast_forward = updates
        .iter()
        .map(|u| {
            if u.old_id == ZERO || u.new_id == ZERO {
                return Ok(false);
            }
            ancestor(quarantine.command(repo)?, u.old_id, u.new_id)
        })
        .collect::<Result<_>>()?;
    Ok(Facts {
        commits,
        fast_forward,
    })
}

fn ancestor(mut command: Command, old: &str, new: &str) -> Result<bool> {
    command.args(["merge-base", "--is-ancestor", old, new]);
    let output = run_output(command, "", TIMEOUT)?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(Error::Git(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        )),
    }
}

impl Quarantine<'_> {
    fn command(&self, repo: &Path) -> Result<Command> {
        for path in [self.object_directory, self.quarantine_path] {
            if !path.is_empty() && (!Path::new(path).is_absolute() || path.contains('\0')) {
                return Err(Error::Git("invalid quarantine path".into()));
            }
        }
        let mut command = git();
        command.arg("--git-dir").arg(repo);
        for (name, value) in [
            ("GIT_OBJECT_DIRECTORY", self.object_directory),
            (
                "GIT_ALTERNATE_OBJECT_DIRECTORIES",
                self.alternate_object_directories,
            ),
            ("GIT_QUARANTINE_PATH", self.quarantine_path),
        ] {
            if !value.is_empty() {
                command.env(name, value);
            }
        }
        Ok(command)
    }
}

#[derive(Debug)]
pub struct CommitRefusal {
    pub update_index: usize,
    pub commit: String,
    pub reason: &'static str,
}

pub(crate) fn verify_commits(
    repo: &Path,
    updates: &[Update<'_>],
    facts: &Facts,
    quarantine: &Quarantine<'_>,
    mut verify: impl FnMut(&str) -> Result<bool>,
) -> Result<Vec<CommitRefusal>> {
    let mut refusals = Vec::new();
    for commit in &facts.commits {
        if verify(commit)? {
            continue;
        }
        for (update_index, update) in updates.iter().enumerate() {
            if update.new_id != ZERO && ancestor(quarantine.command(repo)?, commit, update.new_id)?
            {
                refusals.push(CommitRefusal {
                    update_index,
                    commit: commit.clone(),
                    reason: "missing or invalid SSH signature by a signing key of the pusher",
                });
            }
        }
    }
    Ok(refusals)
}

pub(crate) fn verify_signature(
    repo: &Path,
    object: &str,
    quarantine: &Quarantine<'_>,
    allowed_signers: &Path,
    verifier: &str,
) -> Result<bool> {
    let mut command = quarantine.command(repo)?;
    command.args([
        "-c",
        "gpg.format=ssh",
        "-c",
        "gpg.ssh.program=ssh-keygen",
        "-c",
        "gpg.minTrustLevel=fully",
        // Git detects the signature format independently of gpg.format.
        "-c",
        "gpg.openpgp.program=false",
        "-c",
        "gpg.x509.program=false",
        "-c",
        "gpg.ssh.revocationFile=",
    ]);
    command.arg("-c").arg(format!(
        "gpg.ssh.allowedSignersFile={}",
        allowed_signers.display()
    ));
    command.args([verifier, "--", object]);
    Ok(run_output(command, "", TIMEOUT)?.status.success())
}
