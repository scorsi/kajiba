//! Object checks run by the writer with the hook's quarantine environment (0053).
use crate::{Error, Result, git, run_output_limited};
use std::{
    cell::Cell,
    fs, io,
    path::Path,
    process::Command,
    time::{Duration, Instant},
};

const ZERO: &str = "0000000000000000000000000000000000000000";
const TIMEOUT: Duration = Duration::from_secs(30);
const MAX_UPDATES: usize = 256;
const MAX_COMMITS: usize = 4096;
const MAX_COMMANDS: usize = 16384;
const MAX_COMMAND_OUTPUT: usize = 256 << 10;
const MAX_OUTPUT: usize = 8 << 20;

/// One budget shared by commit selection, ancestry and commit/tag verification (0064).
pub struct Verification {
    deadline: Instant,
    commands: Cell<usize>,
    output: Cell<usize>,
}

impl Default for Verification {
    fn default() -> Self {
        Self {
            deadline: Instant::now() + TIMEOUT,
            commands: Cell::new(MAX_COMMANDS),
            output: Cell::new(MAX_OUTPUT),
        }
    }
}

impl Verification {
    pub fn check_deadline(&self) -> Result<()> {
        if Instant::now() >= self.deadline {
            return Err(Error::Git("push verification exceeds 30 s deadline".into()));
        }
        Ok(())
    }

    fn run(&self, command: Command, stdin: &str) -> Result<std::process::Output> {
        self.check_deadline()?;
        self.commands.set(
            self.commands
                .get()
                .checked_sub(1)
                .ok_or_else(|| Error::Git("push verification exceeds 16384 Git commands".into()))?,
        );
        let result = run_output_limited(
            command,
            stdin,
            self.deadline.saturating_duration_since(Instant::now()),
            MAX_COMMAND_OUTPUT.min(self.output.get()),
        );
        self.check_deadline()?;
        let output = result.map_err(|err| match err {
            Error::Git(message) if message.contains("output limit exceeded") => Error::Git(
                "push verification output exceeds 256 KiB per command or 8 MiB per push".into(),
            ),
            err => err,
        })?;
        self.output
            .set(self.output.get() - output.stdout.len() - output.stderr.len());
        Ok(output)
    }
}

/// Per-push limits, independent of repository configuration (ADR 0062).
pub(crate) const MAX_INPUT_BYTES: u64 = 64 << 20;
pub(crate) const MAX_QUARANTINE_BYTES: u64 = 128 << 20;

pub(crate) fn limit_file_size() -> io::Result<()> {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: both calls receive a valid rlimit pointer and affect only this process.
    unsafe {
        if libc::getrlimit(libc::RLIMIT_FSIZE, &mut limit) != 0 {
            return Err(io::Error::last_os_error());
        }
        limit.rlim_cur = limit.rlim_cur.min(MAX_QUARANTINE_BYTES as libc::rlim_t);
        limit.rlim_max = limit.rlim_max.min(MAX_QUARANTINE_BYTES as libc::rlim_t);
        if libc::setrlimit(libc::RLIMIT_FSIZE, &limit) != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

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
    verification: &Verification,
) -> Result<Facts> {
    verification.check_deadline()?;
    if updates.len() > MAX_UPDATES {
        return Err(Error::Git(
            "push verification exceeds 256 updated refs".into(),
        ));
    }
    for update in updates {
        for id in [update.old_id, update.new_id] {
            if id.len() != 40 || !id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
                return Err(Error::Git("invalid push object id".into()));
            }
        }
    }
    quarantine.command(repo)?;
    quarantine
        .check_size(verification)
        .map_err(|err| Error::Git(err.to_string()))?;
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
        cmd.args([
            "rev-list",
            "--stdin",
            &format!("--max-count={}", MAX_COMMITS + 1),
        ]);
        let output = verification.run(cmd, &format!("{tips}--not\n--all\n"))?;
        if !output.status.success() {
            return Err(Error::Git(
                String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            ));
        }
        let text = String::from_utf8(output.stdout).map_err(|err| Error::Git(err.to_string()))?;
        let commits: Vec<_> = text.lines().map(String::from).collect();
        if commits.len() > MAX_COMMITS {
            return Err(Error::Git(
                "push verification exceeds 4096 new commits".into(),
            ));
        }
        commits
    };
    let fast_forward = updates
        .iter()
        .map(|u| {
            if u.old_id == ZERO || u.new_id == ZERO {
                return Ok(false);
            }
            ancestor(quarantine.command(repo)?, u.old_id, u.new_id, verification)
        })
        .collect::<Result<_>>()?;
    verification.check_deadline()?;
    Ok(Facts {
        commits,
        fast_forward,
    })
}

fn ancestor(
    mut command: Command,
    old: &str,
    new: &str,
    verification: &Verification,
) -> Result<bool> {
    command.args(["merge-base", "--is-ancestor", old, new]);
    let output = verification.run(command, "")?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(Error::Git(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        )),
    }
}

impl Quarantine<'_> {
    fn check_size(&self, verification: &Verification) -> io::Result<()> {
        let mut remaining = MAX_QUARANTINE_BYTES;
        let mut paths = Vec::new();
        for path in [self.object_directory, self.quarantine_path] {
            if !path.is_empty() && !paths.contains(&Path::new(path).to_path_buf()) {
                paths.push(Path::new(path).to_path_buf());
            }
        }
        while let Some(path) = paths.pop() {
            verification.check_deadline().map_err(io::Error::other)?;
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.is_dir() {
                for entry in fs::read_dir(path)? {
                    verification.check_deadline().map_err(io::Error::other)?;
                    paths.push(entry?.path());
                }
            } else if metadata.is_file() {
                remaining = remaining
                    .checked_sub(metadata.len())
                    .ok_or_else(|| io::Error::other("push quarantine exceeds 128 MiB limit"))?;
            } else {
                return Err(io::Error::other("invalid quarantine entry"));
            }
        }
        Ok(())
    }

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
    verification: &Verification,
    mut verify: impl FnMut(&str) -> Result<bool>,
) -> Result<Vec<CommitRefusal>> {
    let mut refusals = Vec::new();
    let mut refused = vec![false; updates.len()];
    for commit in &facts.commits {
        if verify(commit)? {
            continue;
        }
        for (update_index, update) in updates.iter().enumerate() {
            if !refused[update_index]
                && update.new_id != ZERO
                && ancestor(
                    quarantine.command(repo)?,
                    commit,
                    update.new_id,
                    verification,
                )?
            {
                refused[update_index] = true;
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
    verification: &Verification,
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
    Ok(verification.run(command, "")?.status.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shell(script: &str) -> Command {
        let mut command = Command::new("sh");
        command
            .args(["-c", script])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        command
    }

    #[test]
    fn verification_shares_command_and_output_budgets() {
        let verification = Verification::default();
        verification.commands.set(2);
        verification.output.set(4);
        verification.run(shell("printf ab"), "").unwrap();
        verification.run(shell("printf cd >&2"), "").unwrap();
        assert_eq!(verification.output.get(), 0);
        assert!(
            verification
                .run(shell("exit 0"), "")
                .unwrap_err()
                .to_string()
                .contains("16384 Git commands")
        );

        let verification = Verification::default();
        verification.output.set(4);
        verification.run(shell("printf ab"), "").unwrap();
        assert!(
            verification
                .run(shell("printf cde >&2"), "")
                .unwrap_err()
                .to_string()
                .contains("output exceeds")
        );
    }

    #[test]
    fn output_overflow_kills_the_process_group_without_waiting_for_the_deadline() {
        for redirect in ["", ">&2"] {
            let verification = Verification::default();
            let started = Instant::now();
            let err = verification
                .run(
                    shell(&format!(
                        "sleep 30 & head -c {} /dev/zero {redirect}; wait",
                        MAX_COMMAND_OUTPUT + 1
                    )),
                    "",
                )
                .unwrap_err();
            assert!(err.to_string().contains("output exceeds"), "{err}");
            assert!(started.elapsed() < Duration::from_secs(5));
        }
    }

    #[test]
    fn verification_deadline_is_shared_and_kills_a_lingering_child() {
        let verification = Verification {
            deadline: Instant::now() + Duration::from_millis(300),
            ..Default::default()
        };
        verification.run(shell("printf first"), "").unwrap();
        let started = Instant::now();
        let err = verification
            .run(shell("exec >&- 2>&-; exec sleep 30"), "")
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("verification exceeds 30 s deadline"),
            "{err}"
        );
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(
            verification
                .run(shell("exit 0"), "")
                .unwrap_err()
                .to_string()
                .contains("deadline")
        );
    }
}
