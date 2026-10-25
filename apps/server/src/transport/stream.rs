//! A bounded response channel supervises git, raw and browsing bodies even when HTTP
//! stops polling them (ADR 0040, 0043, 0050).
use axum::{
    body::{Body, Bytes},
    extract::Request as HttpRequest,
    middleware::Next,
    response::Response as HttpResponse,
};
use http_body::{Body as HttpBody, Frame};
use std::{
    future::Future,
    io,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Child,
    sync::{OwnedSemaphorePermit, mpsc, oneshot},
    time::{Instant, timeout, timeout_at},
};

/// Without bytes handed to the HTTP transport, a streaming body ends.
pub(super) const STREAM_IDLE: Duration = Duration::from_secs(60);
/// Whatever its progress, a streaming body ends.
pub(super) const STREAM_LIFETIME: Duration = Duration::from_secs(60 * 60);

type Chunk = (Frame<Bytes>, oneshot::Sender<()>);
pub(super) type Sender = mpsc::Sender<Chunk>;

struct Supervised {
    chunks: mpsc::Receiver<Chunk>,
    completion: oneshot::Receiver<io::Result<()>>,
    finished: bool,
}

impl HttpBody for Supervised {
    type Data = Bytes;
    type Error = io::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, io::Error>>> {
        if self.finished {
            return Poll::Ready(None);
        }
        if let Some((frame, consumed)) = std::task::ready!(self.chunks.poll_recv(cx)) {
            let _ = consumed.send(());
            return Poll::Ready(Some(Ok(frame)));
        }
        let result = std::task::ready!(Pin::new(&mut self.completion).poll(cx));
        self.finished = true;
        Poll::Ready(
            match result.unwrap_or_else(|_| Err(io::Error::other("git response task stopped"))) {
                Ok(()) => None,
                Err(err) => Some(Err(err)),
            },
        )
    }
}

pub(super) async fn send(chunks: &Sender, frame: Frame<Bytes>) -> io::Result<()> {
    let (consumed, received) = oneshot::channel();
    chunks
        .send((frame, consumed))
        .await
        .map_err(io::Error::other)?;
    // Progress means HTTP polled a chunk, not merely that git wrote into its pipe.
    received.await.map_err(io::Error::other)
}

/// A body fed by `transfer` from its own task, which holds `permit` until it returns;
/// its result ends the body as a clean EOF or an error.
pub(super) fn supervised<F>(
    permit: OwnedSemaphorePermit,
    transfer: impl FnOnce(Sender) -> F,
) -> Body
where
    F: Future<Output = io::Result<()>> + Send + 'static,
{
    let (chunks, receiver) = mpsc::channel(1);
    let (completion, completed) = oneshot::channel();
    let transfer = transfer(chunks);
    tokio::spawn(async move {
        let _permit = permit;
        let _ = completion.send(transfer.await);
    });
    Body::new(Supervised {
        chunks: receiver,
        completion: completed,
        finished: false,
    })
}

/// The public-read permit of a browsing response, left in the response's extensions
/// by the handler for `guarded` (ADR 0050); shared only because extensions clone.
#[derive(Clone)]
pub(super) struct ReadPermit(Arc<Mutex<Option<OwnedSemaphorePermit>>>);

impl ReadPermit {
    pub(super) fn new(permit: OwnedSemaphorePermit) -> Self {
        Self(Arc::new(Mutex::new(Some(permit))))
    }
}

/// Carries a browsing response's permit through its encoded body: the response is
/// bounded like a `/raw/` body, and the permit ends with the delivery or a bound.
pub(super) async fn guarded(request: HttpRequest, next: Next) -> HttpResponse {
    let mut response = next.run(request).await;
    let permit = response
        .extensions_mut()
        .remove::<ReadPermit>()
        .and_then(|held| held.0.lock().unwrap_or_else(|e| e.into_inner()).take());
    match permit {
        Some(permit) => {
            let (parts, body) = response.into_parts();
            HttpResponse::from_parts(parts, carry(body, permit, STREAM_IDLE, STREAM_LIFETIME))
        }
        None => response,
    }
}

/// `body`'s frames, data and trailers, relayed from a task that owns the body and
/// `permit`, and drops both when the body ends or the first bound fires.
pub(super) fn carry(
    mut body: Body,
    permit: OwnedSemaphorePermit,
    idle: Duration,
    lifetime: Duration,
) -> Body {
    supervised(permit, |chunks| async move {
        let transfer = async {
            while let Some(frame) =
                std::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx)).await
            {
                let frame = frame.map_err(io::Error::other)?;
                timeout(idle, send(&chunks, frame)).await??;
            }
            Ok(())
        };
        timeout(lifetime, transfer)
            .await
            .unwrap_or_else(|err| Err(err.into()))
    })
}

struct ProcessGroup(libc::pid_t);

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        // SAFETY: the supervisor retains the unreaped child, reserving its group id.
        unsafe { libc::kill(-self.0, libc::SIGKILL) };
    }
}

fn wait_without_reaping(pid: libc::pid_t) -> io::Result<()> {
    // SAFETY: waitid writes only the initialized out-parameter; WNOWAIT reserves pid.
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

/// The child must lead its own process group, which remains reserved until cleanup.
pub(super) fn body(
    mut child: Child,
    input: Vec<u8>,
    prefix: &'static [u8],
    permit: OwnedSemaphorePermit,
    idle: Duration,
    lifetime: Duration,
) -> Body {
    supervised(permit, |chunks| async move {
        let group = ProcessGroup(child.id().expect("spawned child") as _);
        let mut stdout = child.stdout.take().expect("piped git stdout");
        let stdin = child.stdin.take();
        let transfer = async {
            let write = async move {
                if let Some(mut stdin) = stdin {
                    stdin.write_all(&input).await?;
                }
                Ok::<_, io::Error>(())
            };
            let read = async {
                let mut deadline = Instant::now() + idle;
                if !prefix.is_empty() {
                    timeout_at(
                        deadline,
                        send(&chunks, Frame::data(Bytes::from_static(prefix))),
                    )
                    .await??;
                    deadline = Instant::now() + idle;
                }
                let mut buffer = [0; 16 << 10];
                loop {
                    let count = timeout_at(deadline, async {
                        let count = stdout.read(&mut buffer).await?;
                        if count != 0 {
                            send(
                                &chunks,
                                Frame::data(Bytes::copy_from_slice(&buffer[..count])),
                            )
                            .await?;
                        }
                        Ok::<_, io::Error>(count)
                    })
                    .await??;
                    if count == 0 {
                        break;
                    }
                    deadline = Instant::now() + idle;
                }
                let pid = group.0;
                timeout_at(
                    deadline,
                    tokio::task::spawn_blocking(move || wait_without_reaping(pid)),
                )
                .await?
                .map_err(io::Error::other)??;
                Ok(())
            };
            tokio::try_join!(write, read).map(|_| ())
        };
        let result = tokio::select! {
            _ = chunks.closed() => Err(io::Error::new(io::ErrorKind::BrokenPipe, "response dropped")),
            result = timeout(lifetime, transfer) => result.unwrap_or_else(|err| Err(err.into())),
        };
        // Kill descendants before reaping the leader or releasing the read permit.
        drop(group);
        let status = child.wait().await?;
        result?;
        if !status.success() {
            return Err(io::Error::other("git upload-pack failed"));
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Stdio;
    use tokio::sync::Semaphore;

    fn child(script: &str) -> Child {
        tokio::process::Command::new("sh")
            .args(["-c", script])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0)
            .kill_on_drop(true)
            .spawn()
            .unwrap()
    }

    async fn reaped(budget: &Arc<Semaphore>, pid: u32) {
        let _permit = timeout(Duration::from_secs(5), budget.acquire())
            .await
            .unwrap()
            .unwrap();
        let exists = std::process::Command::new("sh")
            .args(["-c", "kill -0 \"$1\" 2>/dev/null", "sh", &pid.to_string()])
            .status()
            .unwrap();
        assert!(
            !exists.success(),
            "child {pid} still exists after permit release"
        );
    }

    #[tokio::test]
    async fn completion_reports_child_failure_after_output() {
        for (script, success) in [("printf data", true), ("printf data; exit 7", false)] {
            let budget = Arc::new(Semaphore::new(1));
            let child = child(script);
            let pid = child.id().unwrap();
            let mut response = body(
                child,
                Vec::new(),
                b"",
                budget.clone().try_acquire_owned().unwrap(),
                Duration::from_secs(5),
                Duration::from_secs(10),
            );
            let first = std::future::poll_fn(|cx| Pin::new(&mut response).poll_frame(cx))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(first.into_data().unwrap(), "data");
            let result = axum::body::to_bytes(response, 1024).await;
            assert_eq!(result.is_ok(), success);
            reaped(&budget, pid).await;
        }
    }

    #[tokio::test]
    async fn cancellation_kills_and_reaps_an_idle_child() {
        let budget = Arc::new(Semaphore::new(1));
        let child = child("exec sleep 30");
        let pid = child.id().unwrap();
        let response = body(
            child,
            Vec::new(),
            b"",
            budget.clone().try_acquire_owned().unwrap(),
            Duration::from_secs(20),
            Duration::from_secs(30),
        );
        drop(response);
        reaped(&budget, pid).await;
    }

    #[tokio::test]
    async fn cancellation_and_deadlines_kill_descendants_before_releasing_the_permit() {
        for parent in ["wait", "exit 0"] {
            for end in ["cancel", "idle", "lifetime"] {
                let budget = Arc::new(Semaphore::new(1));
                let child = child(&format!("sleep 30 & printf '%s\\n' $!; {parent}"));
                let pid = child.id().unwrap();
                let mut response = body(
                    child,
                    Vec::new(),
                    b"",
                    budget.clone().try_acquire_owned().unwrap(),
                    Duration::from_millis(if end == "idle" { 200 } else { 10_000 }),
                    Duration::from_millis(if end == "lifetime" { 200 } else { 10_000 }),
                );
                let frame = timeout(
                    Duration::from_secs(5),
                    std::future::poll_fn(|cx| Pin::new(&mut response).poll_frame(cx)),
                )
                .await
                .unwrap()
                .unwrap()
                .unwrap()
                .into_data()
                .unwrap();
                let descendant = std::str::from_utf8(&frame)
                    .unwrap()
                    .trim()
                    .parse::<libc::pid_t>()
                    .unwrap();
                if end == "cancel" {
                    drop(response);
                } else {
                    reaped(&budget, pid).await;
                    assert!(axum::body::to_bytes(response, 1024).await.is_err());
                }
                reaped(&budget, pid).await;
                // Orphans may remain zombies until init reaps them; neither is live work.
                let state = std::process::Command::new("ps")
                    .args(["-o", "stat=", "-p", &descendant.to_string()])
                    .output()
                    .unwrap();
                assert!(state.stderr.is_empty(), "ps failed: {state:?}");
                let state = String::from_utf8(state.stdout).unwrap();
                if !state.trim().is_empty() && !state.trim().starts_with('Z') {
                    // SAFETY: clean up the test's surviving descendant on regression.
                    unsafe { libc::kill(descendant, libc::SIGKILL) };
                    panic!("descendant {descendant} survived {end} ({parent}): {state}");
                }
            }
        }
    }

    #[tokio::test]
    async fn idle_timeout_covers_silent_and_unpolled_responses() {
        for script in [
            "exec sleep 30",
            "exec cat /dev/zero",
            "exec 1>&-; exec sleep 30",
        ] {
            let budget = Arc::new(Semaphore::new(1));
            let child = child(script);
            let pid = child.id().unwrap();
            let response = body(
                child,
                Vec::new(),
                b"",
                budget.clone().try_acquire_owned().unwrap(),
                Duration::from_millis(100),
                Duration::from_secs(20),
            );
            // Do not poll: the supervisor must expire even with HTTP backpressure.
            reaped(&budget, pid).await;
            assert!(axum::body::to_bytes(response, 32 << 10).await.is_err());
        }
    }

    #[tokio::test]
    async fn carried_bodies_deliver_every_frame_then_release_and_expire_unpolled() {
        let budget = Arc::new(Semaphore::new(1));
        let mut trailers = axum::http::HeaderMap::new();
        trailers.insert("grpc-status", "0".parse().unwrap());
        // A body of one data frame and one trailers frame, as tonic encodes a response.
        let inner = |trailers: axum::http::HeaderMap| {
            let permit = Arc::new(Semaphore::new(1)).try_acquire_owned().unwrap();
            supervised(permit, |chunks| async move {
                send(&chunks, Frame::data(Bytes::from_static(b"message"))).await?;
                send(&chunks, Frame::trailers(trailers)).await
            })
        };
        // Read whole: data, then trailers, then the permit.
        let mut response = carry(
            inner(trailers.clone()),
            budget.clone().try_acquire_owned().unwrap(),
            Duration::from_secs(5),
            Duration::from_secs(20),
        );
        let mut frames = Vec::new();
        while let Some(frame) =
            std::future::poll_fn(|cx| Pin::new(&mut response).poll_frame(cx)).await
        {
            frames.push(frame.unwrap());
        }
        assert_eq!(frames.len(), 2, "{frames:?}");
        assert_eq!(frames[0].data_ref().unwrap(), "message");
        assert_eq!(frames[1].trailers_ref().unwrap(), &trailers);
        let permit = timeout(Duration::from_secs(5), budget.clone().acquire_owned())
            .await
            .unwrap()
            .unwrap();
        // Never polled: the idle bound frees the permit and the body ends in error.
        let response = carry(
            inner(trailers),
            permit,
            Duration::from_millis(100),
            Duration::from_secs(20),
        );
        let _released = timeout(Duration::from_secs(5), budget.clone().acquire_owned())
            .await
            .unwrap()
            .unwrap();
        assert!(axum::body::to_bytes(response, 1024).await.is_err());
    }

    #[tokio::test]
    async fn absolute_deadline_expires_despite_continuous_progress() {
        let budget = Arc::new(Semaphore::new(1));
        let child = child("exec cat /dev/zero");
        let pid = child.id().unwrap();
        let mut response = body(
            child,
            Vec::new(),
            b"",
            budget.clone().try_acquire_owned().unwrap(),
            Duration::from_secs(5),
            Duration::from_millis(100),
        );
        let mut bytes = 0;
        timeout(Duration::from_secs(5), async {
            loop {
                match std::future::poll_fn(|cx| Pin::new(&mut response).poll_frame(cx)).await {
                    Some(Ok(frame)) => bytes += frame.into_data().unwrap().len(),
                    Some(Err(_)) => break,
                    None => panic!("deadline reported a clean EOF"),
                }
            }
        })
        .await
        .unwrap();
        assert!(bytes > 0);
        reaped(&budget, pid).await;
    }
}
