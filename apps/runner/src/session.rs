//! The runner's side of `kajiba.runner.v1` (0008, 0105): admission, heartbeats,
//! reconnection with backoff, and the in-flight attempts whose logs, step statuses
//! and results outlive a connection.
#![allow(clippy::result_large_err)]

use crate::metrics::Metrics;
use kajiba_proto::runner::v1::{
    Accept, Assignment, Capabilities, ConnectRequest, Heartbeat, Hello, LogChunk,
    Result as RunResult, StepStatus, connect_request::Message, connect_response,
    runner_service_client::RunnerServiceClient,
};
use std::{
    collections::{BTreeMap, VecDeque},
    future::Future,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::{Notify, mpsc};
use tokio_stream::wrappers::UnboundedReceiverStream;
use tonic::{Request, Status, transport::Channel};

/// ADR 0105: a heartbeat every 10 s; reconnection from 1 s doubling to 30 s.
pub const HEARTBEAT: Duration = Duration::from_secs(10);
// Bound retained log data to 1 MiB per attempt, plus its outbound copy.
const LOG_WINDOW: usize = 16;
const FIRST_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// The pause before the next connection attempt.
pub fn next_backoff(previous: Option<Duration>) -> Duration {
    previous.map_or(FIRST_BACKOFF, |delay| (delay * 2).min(MAX_BACKOFF))
}

/// What the runner announces in its hello.
#[derive(Clone)]
pub struct Identity {
    pub name: String,
    pub hostname: String,
    pub capabilities: Capabilities,
}

/// What the connection hands to the job side.
#[derive(Debug, PartialEq)]
pub enum Job {
    Assigned(Assignment),
    /// Stop the attempt: the server cancelled it or it is no longer current.
    Cancelled(String),
}

#[derive(Default)]
struct Attempt {
    next: u64,
    disk: Option<crate::durable::Shared>,
    unacknowledged: VecDeque<LogChunk>,
    steps: BTreeMap<u32, StepStatus>,
    result: Option<RunResult>,
    /// Wait for ContinueAttempt and its LogAck before sending on a new stream.
    reconciling: bool,
    /// Continued after a reconnection: resend from the log acknowledgment that follows.
    resend: bool,
}

#[derive(Default)]
struct Inner {
    attempts: BTreeMap<String, Attempt>,
    outbound: Option<mpsc::UnboundedSender<ConnectRequest>>,
}

impl Inner {
    fn send(&self, message: Message) {
        let attempt = match &message {
            Message::Accept(accept) => Some(&accept.attempt_id),
            Message::LogChunk(chunk) => Some(&chunk.attempt_id),
            Message::StepStatus(step) => Some(&step.attempt_id),
            Message::Result(result) => Some(&result.attempt_id),
            _ => None,
        };
        if attempt
            .and_then(|id| self.attempts.get(id))
            .is_some_and(|held| held.reconciling)
        {
            return;
        }
        if let Some(outbound) = &self.outbound {
            let _ = outbound.send(ConnectRequest {
                message: Some(message),
            });
        }
    }
}

/// The attempts this runner holds; their logs are kept until acknowledged and their
/// latest step statuses and result until recorded, across reconnections (0008).
#[derive(Clone, Default)]
pub struct Attempts(Arc<Mutex<Inner>>, Arc<Notify>);

impl Attempts {
    /// Hold an assigned attempt and accept it.
    pub fn accept(&self, attempt: &str) {
        let mut inner = self.0.lock().unwrap();
        inner.attempts.entry(attempt.into()).or_default();
        inner.send(Message::Accept(Accept {
            attempt_id: attempt.into(),
        }));
    }

    /// Attach persistence only while the server still holds this newly accepted attempt.
    pub fn bind(&self, id: &str, disk: crate::durable::Shared) -> bool {
        let mut inner = self.0.lock().unwrap();
        let Some(held) = inner.attempts.get_mut(id) else {
            return false;
        };
        held.disk = Some(disk);
        true
    }

    /// Restore exactly the indexed chunks, before the first hello.
    pub fn adopt(&self, disk: crate::durable::Shared) -> Result<(), String> {
        let state = disk.lock().unwrap();
        let chunks = state.chunks()?;
        let held = Attempt {
            next: state.sequence(),
            unacknowledged: chunks
                .into_iter()
                .enumerate()
                .map(|(i, data)| LogChunk {
                    attempt_id: state.attempt.clone(),
                    sequence: state.acked_sequence + i as u64 + 1,
                    step_index: 0,
                    data,
                    produced_at_ms: state.pending_times[i],
                })
                .collect(),
            disk: Some(disk.clone()),
            ..Default::default()
        };
        self.0
            .lock()
            .unwrap()
            .attempts
            .insert(state.attempt.clone(), held);
        Ok(())
    }

    /// Append log bytes (at most 64 KiB), waiting for acknowledgments when full.
    pub async fn log(&self, attempt: &str, step_index: u32, data: Vec<u8>, produced_at_ms: i64) {
        assert!(data.len() <= kajiba_exec::CHUNK);
        loop {
            let changed = self.1.notified();
            tokio::pin!(changed);
            // Register before checking the window so an acknowledgment cannot be missed.
            changed.as_mut().enable();
            {
                let mut inner = self.0.lock().unwrap();
                let Some(held) = inner.attempts.get_mut(attempt) else {
                    return;
                };
                if held.unacknowledged.len() < LOG_WINDOW {
                    if let Some(disk) = &held.disk {
                        persist(disk.lock().unwrap().index(data.len(), produced_at_ms));
                    }
                    held.next += 1;
                    let chunk = LogChunk {
                        attempt_id: attempt.into(),
                        sequence: held.next,
                        step_index,
                        data,
                        produced_at_ms,
                    };
                    held.unacknowledged.push_back(chunk.clone());
                    inner.send(Message::LogChunk(chunk));
                    return;
                }
            }
            changed.await;
        }
    }

    /// Send a message, retaining the latest status of each step for reconnection.
    pub fn send(&self, message: Message) {
        let mut inner = self.0.lock().unwrap();
        if let Message::StepStatus(step) = &message {
            let Some(held) = inner.attempts.get_mut(&step.attempt_id) else {
                return;
            };
            held.steps.insert(step.step_index, step.clone());
        }
        inner.send(message);
    }

    /// The attempt's terminal result, resent until the server acknowledges it.
    pub fn finish(&self, result: RunResult, metrics: &Metrics) {
        let mut inner = self.0.lock().unwrap();
        let Some(held) = inner.attempts.get_mut(&result.attempt_id) else {
            return;
        };
        metrics.job_finished(result.state());
        held.result = Some(result.clone());
        inner.send(Message::Result(result));
    }

    #[cfg(test)]
    fn ids(&self) -> Vec<String> {
        self.0.lock().unwrap().attempts.keys().cloned().collect()
    }

    fn reconcile(&self) -> Vec<String> {
        let mut inner = self.0.lock().unwrap();
        for held in inner.attempts.values_mut() {
            held.reconciling = true;
            held.resend = false;
        }
        inner.attempts.keys().cloned().collect()
    }

    pub(crate) fn attach(&self, outbound: Option<mpsc::UnboundedSender<ConnectRequest>>) {
        self.0.lock().unwrap().outbound = outbound;
    }

    /// Apply a server message; a cancellation is handed to the job side.
    fn receive(&self, message: connect_response::Message) -> Option<Job> {
        use connect_response::Message as Server;
        let mut inner = self.0.lock().unwrap();
        match message {
            Server::Assignment(assignment) => return Some(Job::Assigned(assignment)),
            Server::CancelAttempt(cancel) => {
                inner.attempts.remove(&cancel.attempt_id);
                self.1.notify_waiters();
                return Some(Job::Cancelled(cancel.attempt_id));
            }
            Server::ContinueAttempt(resumed) => {
                if let Some(held) = inner.attempts.get_mut(&resumed.attempt_id) {
                    held.resend = true;
                }
            }
            Server::LogAck(ack) => {
                let held = inner.attempts.get_mut(&ack.attempt_id)?;
                if let Some(disk) = &held.disk {
                    persist(disk.lock().unwrap().acknowledge(ack.sequence));
                }
                while held
                    .unacknowledged
                    .front()
                    .is_some_and(|chunk| chunk.sequence <= ack.sequence)
                {
                    held.unacknowledged.pop_front();
                }
                self.1.notify_waiters();
                if std::mem::take(&mut held.resend) {
                    held.reconciling = false;
                    // The Accept may have been lost with the connection: the server's
                    // heartbeat renews only accepted attempts (0008: statuses replay).
                    let mut resent = vec![Message::Accept(Accept {
                        attempt_id: ack.attempt_id.clone(),
                    })];
                    resent.extend(held.unacknowledged.iter().cloned().map(Message::LogChunk));
                    resent.extend(held.steps.values().cloned().map(Message::StepStatus));
                    resent.extend(held.result.clone().map(Message::Result));
                    for message in resent {
                        inner.send(message);
                    }
                }
            }
            Server::ResultAck(ack) => {
                if let Some(held) = inner.attempts.remove(&ack.attempt_id) {
                    if let Some(disk) = held.disk {
                        disk.lock().unwrap().remove();
                    }
                }
                self.1.notify_waiters();
            }
        }
        None
    }
}

// A persistence error must stop the runner before any unindexed bytes reach the server.
fn persist(result: Result<(), String>) {
    if let Err(err) = result {
        tracing::error!(event = "attempt-state-failed", err);
        std::process::exit(1);
    }
}

/// One connection: hello, then heartbeats and server messages until the stream ends.
/// The flag says whether the server admitted the runner.
async fn session<F: Future<Output = Result<Channel, tonic::transport::Error>>>(
    connect: &impl Fn() -> F,
    identity: &Identity,
    attempts: &Attempts,
    jobs: &mpsc::Sender<Job>,
) -> (bool, Status) {
    let channel: Channel = match connect().await {
        Ok(channel) => channel,
        Err(err) => return (false, Status::unavailable(format!("connect: {err}"))),
    };
    let (outbound, receiver) = mpsc::unbounded_channel();
    let _ = outbound.send(ConnectRequest {
        message: Some(Message::Hello(Hello {
            name: identity.name.clone(),
            version: env!("CARGO_PKG_VERSION").into(),
            capabilities: Some(identity.capabilities.clone()),
            hostname: identity.hostname.clone(),
            in_flight_attempt_ids: attempts.reconcile(),
        })),
    });
    let request = Request::new(UnboundedReceiverStream::new(receiver));
    let mut inbound = match RunnerServiceClient::new(channel).connect(request).await {
        Ok(response) => response.into_inner(),
        Err(status) => return (false, status),
    };
    tracing::info!(event = "server-connected", runner = identity.name);
    attempts.attach(Some(outbound.clone()));
    let mut heartbeat =
        tokio::time::interval_at(tokio::time::Instant::now() + HEARTBEAT, HEARTBEAT);
    let ended = loop {
        tokio::select! {
            _ = heartbeat.tick() => {
                let _ = outbound.send(ConnectRequest { message: Some(Message::Heartbeat(Heartbeat {})) });
            }
            message = inbound.message() => match message {
                Ok(Some(response)) => {
                    let job = response.message.and_then(|message| attempts.receive(message));
                    if let Some(job) = job {
                        let _ = jobs.send(job).await;
                    }
                }
                Ok(None) => break Status::unavailable("the server ended the stream"),
                Err(status) => break status,
            },
        }
    };
    attempts.attach(None);
    (true, ended)
}

/// Stay connected: reconnect after `pause` with the backoff of 0105, from the first delay
/// again once a connection was admitted. `connect` opens a new connection, so a new handshake
/// presents the current SVID (0125).
pub async fn run<
    C: Future<Output = Result<Channel, tonic::transport::Error>>,
    F: Future<Output = ()>,
>(
    connect: impl Fn() -> C,
    identity: Identity,
    attempts: Attempts,
    jobs: mpsc::Sender<Job>,
    mut pause: impl FnMut(Duration) -> F,
) {
    let mut backoff = None;
    loop {
        let (admitted, status) = session(&connect, &identity, &attempts, &jobs).await;
        if admitted {
            backoff = None;
        }
        let delay = next_backoff(backoff);
        backoff = Some(delay);
        tracing::warn!(
            event = "server-disconnected",
            err = status.message(),
            code = ?status.code(),
            retry_ms = delay.as_millis() as u64
        );
        pause(delay).await;
    }
}

#[cfg(test)]
mod tests;
