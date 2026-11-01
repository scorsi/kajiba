//! Interchangeable execution backends (0009): the `Executor` trait and the `nix` backend.

use std::{future::Future, io::Read, os::unix::process::ExitStatusExt, path::Path};
use tokio::{process::Command, sync::mpsc};

/// ADR 0105: a log chunk holds at most 64 KiB.
pub const CHUNK: usize = 64 * 1024;

/// Output stamped at the pipe read, before channel backpressure (ADR 0164).
#[derive(Debug)]
pub struct Log {
    pub data: Vec<u8>,
    pub produced_at_ms: i64,
}

impl Log {
    pub fn new(data: Vec<u8>) -> Self {
        Self {
            data,
            produced_at_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as i64,
        }
    }
}

/// An attempt's sources, checked out at its commit.
pub struct Job<'a> {
    pub source: &'a Path,
    pub sha: &'a str,
    /// The Nix system to build for, e.g. `x86_64-linux`.
    pub system: &'a str,
    /// The checks to build one by one; empty for the whole `nix flake check` (0097).
    pub checks: &'a [String],
}

/// How a step ended.
#[derive(Debug, PartialEq, Eq)]
pub enum Exit {
    Code(i32),
    Signal(i32),
    Cancelled,
}

pub trait Executor: Send + Sync + 'static {
    type Env: Send + Sync;

    /// The job's environment and the names of its steps, in order.
    fn prepare(&self, job: &Job) -> Result<(Self::Env, Vec<String>), String>;

    /// Run step `index`, sending its output in chunks of at most [`CHUNK`] bytes, until it
    /// exits or `cancel` resolves.
    fn run(
        &self,
        env: &Self::Env,
        index: usize,
        log: mpsc::Sender<Log>,
        cancel: impl Future<Output = ()> + Send,
    ) -> impl Future<Output = Result<Exit, String>> + Send;

    /// The check whose build failed, when the backend knows it structurally (0112).
    fn failing_check(&self, _env: &Self::Env) -> Option<String> {
        None
    }

    fn cleanup(&self, env: Self::Env);
}

/// The `nix` backend: one step, `nix flake check` on the checked-out commit, or a
/// `nix build` of each selected check in turn, stopping at the first failure (0009, 0097).
pub struct Nix {
    pub program: std::path::PathBuf,
}

impl Default for Nix {
    fn default() -> Self {
        Self {
            program: "nix".into(),
        }
    }
}

/// The flake reference of the checkout, the selected checks as `(name, installable)`, and
/// the one whose build failed.
pub struct Flake {
    reference: String,
    checks: Vec<(String, String)>,
    failed: std::sync::Mutex<Option<String>>,
}

impl Executor for Nix {
    type Env = Flake;

    fn prepare(&self, job: &Job) -> Result<(Flake, Vec<String>), String> {
        let source = job.source.to_str().ok_or("source: path is not UTF-8")?;
        let reference = format!("git+file://{source}?rev={}", job.sha);
        let checks = job
            .checks
            .iter()
            .map(|name| {
                let installable = format!("{reference}#checks.{}.{name}", job.system);
                (name.clone(), installable)
            })
            .collect::<Vec<_>>();
        let step = if checks.is_empty() {
            "flake-check"
        } else {
            "build-checks"
        };
        let flake = Flake {
            reference,
            checks,
            failed: std::sync::Mutex::default(),
        };
        Ok((flake, vec![step.into()]))
    }

    async fn run(
        &self,
        flake: &Flake,
        _: usize,
        log: mpsc::Sender<Log>,
        cancel: impl Future<Output = ()> + Send,
    ) -> Result<Exit, String> {
        if flake.checks.is_empty() {
            let mut command = Command::new(&self.program);
            command.args(["flake", "check", "--print-build-logs", &flake.reference]);
            return spawn(command, log, cancel).await;
        }
        // Completes only when a step returns `Cancelled`, so it is never polled again.
        let mut cancel = std::pin::pin!(cancel);
        for (name, installable) in &flake.checks {
            let mut command = Command::new(&self.program);
            command.args(["build", "--no-link", "--print-build-logs", installable]);
            let exit = spawn(command, log.clone(), cancel.as_mut()).await?;
            if exit != Exit::Code(0) {
                if exit != Exit::Cancelled {
                    *flake.failed.lock().unwrap() = Some(name.clone());
                }
                return Ok(exit);
            }
        }
        Ok(Exit::Code(0))
    }

    fn failing_check(&self, flake: &Flake) -> Option<String> {
        flake.failed.lock().unwrap().clone()
    }

    fn cleanup(&self, _: Flake) {}
}

/// Run `command` in its own process group with stdout and stderr interleaved into `log`;
/// cancellation kills the whole group.
pub async fn spawn(
    mut command: Command,
    log: mpsc::Sender<Log>,
    cancel: impl Future<Output = ()> + Send,
) -> Result<Exit, String> {
    let program = command
        .as_std()
        .get_program()
        .to_string_lossy()
        .into_owned();
    let failed = |err: std::io::Error| format!("{program}: {err}");
    let (mut reader, writer) = std::io::pipe().map_err(failed)?;
    command
        .stdin(std::process::Stdio::null())
        .stdout(writer.try_clone().map_err(failed)?)
        .stderr(writer)
        .process_group(0)
        .kill_on_drop(true);
    let mut child = command.spawn().map_err(failed)?;
    // The parent's copies of the pipe's write end must close for the log to end.
    drop(command);
    let group = -(child.id().ok_or("child already reaped")? as i32);
    let reading = tokio::task::spawn_blocking(move || {
        let mut buffer = vec![0; CHUNK];
        while let Ok(read @ 1..) = reader.read(&mut buffer) {
            if log
                .blocking_send(Log::new(buffer[..read].to_vec()))
                .is_err()
            {
                break;
            }
        }
    });
    let status = tokio::select! {
        status = child.wait() => status.map_err(failed)?,
        () = cancel => {
            // SAFETY: plain syscall; the leader is not reaped yet, so the group id is ours.
            unsafe { libc::kill(group, libc::SIGKILL) };
            let _ = child.wait().await;
            let _ = reading.await;
            return Ok(Exit::Cancelled);
        }
    };
    // shortcut: leftovers of the group are killed after the leader is reaped, so they cannot
    // hold the log open; the group id could in theory be reused in between.
    // SAFETY: plain syscall.
    unsafe { libc::kill(group, libc::SIGKILL) };
    let _ = reading.await;
    Ok(match (status.code(), status.signal()) {
        (Some(code), _) => Exit::Code(code),
        (None, Some(signal)) => Exit::Signal(signal),
        (None, None) => Exit::Code(-1),
    })
}

#[cfg(test)]
mod tests;
