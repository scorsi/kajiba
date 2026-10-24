//! A bounded response channel supervises git and raw bodies even when HTTP stops
//! polling them (ADR 0040, 0043).
use axum::body::{Body, Bytes};
use http_body::{Body as HttpBody, Frame};
use std::{
    future::Future,
    io,
    pin::Pin,
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

type Chunk = (Bytes, oneshot::Sender<()>);
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
        if let Some((bytes, consumed)) = std::task::ready!(self.chunks.poll_recv(cx)) {
            let _ = consumed.send(());
            return Poll::Ready(Some(Ok(Frame::data(bytes))));
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

pub(super) async fn send(chunks: &Sender, bytes: Bytes) -> io::Result<()> {
    let (consumed, received) = oneshot::channel();
    chunks
        .send((bytes, consumed))
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

pub(super) fn body(
    mut child: Child,
    input: Vec<u8>,
    prefix: &'static [u8],
    permit: OwnedSemaphorePermit,
    idle: Duration,
    lifetime: Duration,
) -> Body {
    supervised(permit, |chunks| async move {
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
                    timeout_at(deadline, send(&chunks, Bytes::from_static(prefix))).await??;
                    deadline = Instant::now() + idle;
                }
                let mut buffer = [0; 16 << 10];
                loop {
                    let count = timeout_at(deadline, async {
                        let count = stdout.read(&mut buffer).await?;
                        if count != 0 {
                            send(&chunks, Bytes::copy_from_slice(&buffer[..count])).await?;
                        }
                        Ok::<_, io::Error>(count)
                    })
                    .await??;
                    if count == 0 {
                        break;
                    }
                    deadline = Instant::now() + idle;
                }
                let status = timeout_at(deadline, child.wait()).await??;
                if !status.success() {
                    return Err(io::Error::other("git upload-pack failed"));
                }
                Ok(())
            };
            tokio::try_join!(write, read).map(|_| ())
        };
        let result = tokio::select! {
            _ = chunks.closed() => Err(io::Error::new(io::ErrorKind::BrokenPipe, "response dropped")),
            result = timeout(lifetime, transfer) => result.unwrap_or_else(|err| Err(err.into())),
        };
        if result.is_err() {
            // kill() also reaps; kill_on_drop remains the fallback during runtime shutdown.
            let _ = child.kill().await;
        }
        result
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{process::Stdio, sync::Arc};
    use tokio::sync::Semaphore;

    fn child(script: &str) -> Child {
        tokio::process::Command::new("sh")
            .args(["-c", script])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
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
