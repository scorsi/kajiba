//! Bounded SSH stream relays and process-group supervision (ADR 0063).
use std::{io, os::fd::OwnedFd, process::ExitStatus, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    process::{ChildStdin, ChildStdout, Command},
    sync::watch,
    time::Instant,
};

async fn relay(
    mut input: impl AsyncRead + Unpin,
    mut output: impl AsyncWrite + Unpin,
    activity: &watch::Sender<Instant>,
) -> io::Result<()> {
    let mut buffer = [0; 16 * 1024];
    loop {
        let n = input.read(&mut buffer).await?;
        if n == 0 {
            return output.shutdown().await;
        }
        let mut remaining = &buffer[..n];
        while !remaining.is_empty() {
            let n = output.write(remaining).await?;
            if n == 0 {
                return Err(io::ErrorKind::WriteZero.into());
            }
            activity.send_replace(Instant::now());
            remaining = &remaining[n..];
        }
    }
}

async fn deadline(mut activity: watch::Receiver<Instant>, idle: Duration, lifetime: Duration) {
    let absolute = Instant::now() + lifetime;
    loop {
        let idle_deadline = *activity.borrow_and_update() + idle;
        tokio::select! {
            biased;
            _ = tokio::time::sleep_until(absolute) => return,
            _ = tokio::time::sleep_until(idle_deadline) => {
                if Instant::now() >= *activity.borrow() + idle {
                    return;
                }
            },
            _ = activity.changed() => {},
        }
    }
}

struct ProcessGroup(libc::pid_t);

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        // SAFETY: the supervisor retains the unreaped child, reserving its group id.
        unsafe { libc::kill(-self.0, libc::SIGKILL) };
    }
}

fn wait_without_reaping(pid: libc::pid_t) -> io::Result<()> {
    // SAFETY: waitid writes only the initialized out-parameter, and WNOWAIT reserves pid.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    loop {
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                pid as _,
                &mut info,
                libc::WEXITED | libc::WNOWAIT,
            )
        };
        if result == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

pub(super) async fn run(
    command: &mut Command,
    fds: [OwnedFd; 3],
    session: &kajiba_proto::git::v1::Session,
    idle: Duration,
    lifetime: Duration,
    exited: &mut bool,
) -> io::Result<ExitStatus> {
    let [stdin, stdout, stderr] = fds;
    // Tokio makes the handed-off pipes nonblocking; no blocking file worker can pin them.
    let input = ChildStdout::from_std(stdin.into())?;
    let output = ChildStdin::from_std(stdout.into())?;
    let error = ChildStdin::from_std(stderr.into())?;
    let mut child = command
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .process_group(0)
        .kill_on_drop(true)
        .spawn()?;
    *exited = false;
    let group = ProcessGroup(child.id().expect("spawned child") as _);
    tracing::info!(
        event = "session-started",
        request_id = session.correlation_id,
        session = session.session_id,
        kind = crate::metrics::kind_name(session.kind),
        repo = session.repo_public_id
    );
    let pid = group.0;
    let child_input = child.stdin.take().expect("piped");
    let child_output = child.stdout.take().expect("piped");
    let child_error = child.stderr.take().expect("piped");
    let (activity, updates) = watch::channel(Instant::now());
    let wait = tokio::task::spawn_blocking(move || wait_without_reaping(pid));
    let result = {
        let delivery = async {
            tokio::try_join!(
                relay(child_output, output, &activity),
                relay(child_error, error, &activity),
                async { wait.await.map_err(io::Error::other)? },
            )?;
            Ok::<_, io::Error>(())
        };
        tokio::pin!(delivery);
        let transfer = async {
            tokio::select! {
                result = &mut delivery => result,
                result = relay(input, child_input, &activity) => {
                    // Git can stop reading before it finishes writing its response.
                    if let Err(err) = result {
                        if err.kind() != io::ErrorKind::BrokenPipe {
                            return Err(err);
                        }
                    }
                    delivery.await
                }
            }
        };
        tokio::select! {
            biased;
            _ = deadline(updates, idle, lifetime) => Err(io::Error::new(
                io::ErrorKind::TimedOut, "SSH session idle or absolute deadline exceeded",
            )),
            result = transfer => result,
        }
    };
    // Drop relays, kill all descendants while pid is reserved, then reap before reporting exit.
    drop(group);
    let status = child.wait().await?;
    *exited = true;
    result?;
    Ok(status)
}

#[cfg(test)]
mod tests;
