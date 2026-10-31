//! `kajiba.runner.v1` admission (0008, 0026, 0105, 0108, 0124): a runner is the declared one
//! whose `spiffe_id` is its verified certificate's [`Peer`] ID, a request extension;
//! outside the `kajiba.v1` auth layer and the request timeout.

use super::{
    Health,
    auth::compatible,
    cursor::invalid_field,
    local::refused,
    slots::{Slot, Slots},
};
use crate::{
    db::{
        attempts::{self, Offer, Outcome},
        events::{EventBus, NewEvent, append_event},
        runs::{AttemptState, RunState},
    },
    metrics::Registry,
};
use kajiba_core::config::{Capabilities, RunnerDeclaration};
use kajiba_core::svid::Peer;
use kajiba_gitstore::{Abort, GitStore, Gix};
use kajiba_proto::runner::v1::{
    Assignment, CancelAttempt, ConnectRequest, ConnectResponse, ContinueAttempt,
    FetchRunBundleRequest, FetchRunBundleResponse, Hello, LogAck, ResultAck, ResultState,
    StepState, connect_request::Message, connect_response, runner_service_server::RunnerService,
};
use sqlx::{Connection, SqliteConnection};
use std::{
    collections::BTreeMap,
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::{Notify, Semaphore, mpsc, oneshot};
use tokio_stream::{Stream, wrappers::ReceiverStream};
use tonic::{Request, Response, Status, Streaming};

/// Declared runners and the bus their events wake, set once startup has reconciled them.
pub struct Declared {
    pub runners: BTreeMap<String, RunnerDeclaration>,
    pub bus: Arc<EventBus>,
    /// Wakes matching: job creation, runner admission and attempt end (0008).
    pub wake: Arc<Notify>,
}

/// Matching and lease expiry also run this often without a wake-up.
const PASS_PERIOD: Duration = Duration::from_secs(1);

/// A runner silent this long after opening `Connect` is dropped: one heartbeat period (0105).
const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
use kajiba_core::config::MAX_HOSTNAME;
/// ADR 0105: a log chunk holds at most 64 KiB.
const MAX_CHUNK: usize = 64 * 1024;
/// `have` ids considered per bundle request (0106).
const MAX_HAVE: usize = 64;
/// ADR 0119: runner requests not yet admitted, server-wide.
pub const MAX_PENDING: usize = 16;
/// ADR 0119: a decoded runner message holds at most 128 KiB.
pub const MAX_MESSAGE: usize = 128 * 1024;
/// ADR 0119: bundle transfers at once, server-wide; per runner, its `max_jobs`.
const MAX_TRANSFERS: usize = 8;
/// ADR 0119: a transfer delivering nothing for this long is cut.
const TRANSFER_IDLE: Duration = Duration::from_secs(60);

type Outbound = mpsc::Sender<Result<ConnectResponse, Status>>;

struct Live {
    generation: u64,
    replaced: oneshot::Sender<()>,
    outbound: Outbound,
    offer: Offer,
}

fn response(message: connect_response::Message) -> ConnectResponse {
    ConnectResponse {
        message: Some(message),
    }
}

fn cancel(attempt_id: &str) -> ConnectResponse {
    response(connect_response::Message::CancelAttempt(CancelAttempt {
        attempt_id: attempt_id.into(),
    }))
}

#[derive(Clone)]
pub struct Runners {
    health: Health,
    registry: Arc<Registry>,
    // Only read or changed under the writer lock, so it always agrees with `runner.connected`.
    live: Arc<Mutex<(u64, BTreeMap<String, Live>)>>,
    /// Bundle transfers per runner name (0119).
    transfers: Arc<Slots<String>>,
}

impl Runners {
    pub fn new(health: Health, registry: Arc<Registry>) -> Self {
        Self {
            health,
            registry,
            live: Arc::default(),
            transfers: Slots::new(MAX_TRANSFERS),
        }
    }
}

/// ADR 0119: at most [`MAX_PENDING`] runner requests wait for admission (the hello of
/// `Connect`, the request of `FetchRunBundle`); each gets [`HELLO_TIMEOUT`] to be
/// admitted, i.e. for its response head. Other routes pass through.
pub async fn admission(
    axum::extract::State(pending): axum::extract::State<Arc<Semaphore>>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use kajiba_proto::runner::v1::runner_service_server::SERVICE_NAME;
    let path = request.uri().path();
    if !path
        .strip_prefix('/')
        .and_then(|path| path.strip_prefix(SERVICE_NAME))
        .is_some_and(|rest| rest.starts_with('/'))
    {
        return next.run(request).await;
    }
    let status = match pending.try_acquire_owned() {
        Ok(_permit) => match tokio::time::timeout(HELLO_TIMEOUT, next.run(request)).await {
            Ok(response) => return response,
            Err(_) => Status::deadline_exceeded("not admitted in time"),
        },
        Err(_) => refused(
            tonic::Code::ResourceExhausted,
            "RUNNER_ADMISSIONS_LIMIT",
            format!("at most {MAX_PENDING} runner requests may wait for admission"),
        ),
    };
    status.into_http().map(axum::body::Body::new)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Reason {
    Identity,
    Disabled,
    Version,
    Capabilities,
}

impl Reason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Identity => "identity",
            Self::Disabled => "disabled",
            Self::Version => "version",
            Self::Capabilities => "capabilities",
        }
    }
}

/// The declared runner whose `spiffe_id` is the verified peer's ID (0124).
fn declared_by<'a>(
    runners: &'a BTreeMap<String, RunnerDeclaration>,
    peer: Option<&Peer>,
) -> Option<(&'a str, &'a RunnerDeclaration)> {
    let peer = peer?;
    runners
        .iter()
        .find(|(_, declaration)| declaration.spiffe_id == peer.id)
        .map(|(name, declaration)| (name.as_str(), declaration))
}

fn unauthenticated() -> Status {
    refused(
        tonic::Code::Unauthenticated,
        "CREDENTIAL_INVALID",
        "the certificate is not a declared runner's".into(),
    )
}

/// Configuration enums compare by their TOML name.
fn named<T: serde::Serialize>(value: &T, name: &str) -> bool {
    serde_json::to_value(value).is_ok_and(|value| value.as_str() == Some(name))
}

/// Announced capabilities must stay within the declared ones (0026).
fn within(announced: &kajiba_proto::runner::v1::Capabilities, declared: &Capabilities) -> bool {
    named(&declared.arch, &announced.arch)
        && named(&declared.os, &announced.os)
        && announced
            .backends
            .iter()
            .all(|backend| declared.backends.iter().any(|d| named(d, backend)))
        && announced.tags.iter().all(|tag| declared.tags.contains(tag))
        && (1..=declared.max_jobs).contains(&announced.max_jobs)
}

fn runner_event(
    runner_id: i64,
    correlation_id: &str,
    kind: &str,
    payload: serde_json::Value,
) -> impl FnOnce(String) -> NewEvent {
    let (kind, correlation_id) = (kind.to_owned(), correlation_id.to_owned());
    move |public_id| NewEvent {
        public_id,
        kind,
        version: 1,
        occurred_at: crate::db::runs::now(),
        principal_kind: "runner".into(),
        principal_id: Some(runner_id),
        repo_id: None,
        correlation_id,
        payload: payload.to_string(),
    }
}

/// Apply `update` (bound to `values`, then the runner id) and append the event, together.
async fn journal(
    writer: &mut SqliteConnection,
    bus: &EventBus,
    update: Option<(&str, &[&str], i64, i64)>,
    event: impl FnOnce(String) -> NewEvent,
) -> Result<(), sqlx::Error> {
    let now = crate::db::runs::now();
    let mut tx = writer.begin().await?;
    if let Some((sql, values, contact, id)) = update {
        let mut query = sqlx::query(sql);
        for value in values {
            query = query.bind(*value);
        }
        query.bind(contact).bind(id).execute(&mut *tx).await?;
    }
    let public_id = crate::db::users::new_public_id(&mut tx, now as u128).await?;
    append_event(&mut tx, &event(public_id)).await?;
    bus.commit(tx).await
}

fn internal(err: impl std::fmt::Display) -> Status {
    tracing::error!(event = "runner-admission-failed", err = %err);
    refused(tonic::Code::Internal, "INTERNAL", String::new())
}

impl Runners {
    fn refuse(&self, runner: &str, reason: Reason) {
        self.registry.runner_refused(reason.as_str());
        tracing::warn!(event = "runner-refused", runner, reason = reason.as_str());
    }

    /// An ID of no declared runner, or of another runner than the hello's (0124): logged
    /// with the ID, not journaled.
    fn refuse_identity(&self, runner: &str, peer: Option<&Peer>) {
        let reason = Reason::Identity.as_str();
        self.registry.runner_refused(reason);
        let spiffe_id = peer.map_or("", |peer| peer.id.as_str());
        tracing::warn!(event = "runner-refused", runner, reason, spiffe_id);
    }

    /// Check the hello against the declaration; on success the row is connected and the
    /// stream registered, replacing an older one under the same name.
    async fn admit(
        &self,
        peer: Option<&Peer>,
        hello: &Hello,
        request_id: &str,
        outbound: Outbound,
    ) -> Result<(i64, u64, oneshot::Receiver<()>), Status> {
        let declared =
            self.health.runners.get().ok_or_else(|| {
                refused(tonic::Code::Unavailable, "MAINTENANCE", "starting".into())
            })?;
        let writer = self
            .health
            .writer
            .get()
            .ok_or_else(|| internal("no writer"))?;
        let Some((name, declaration)) = declared_by(&declared.runners, peer) else {
            let mut writer = writer.lock().await;
            let disabled: Option<i64> = sqlx::query_scalar(
                "SELECT 1 FROM runner WHERE name = ? AND disabled_at IS NOT NULL",
            )
            .bind(&hello.name)
            .fetch_optional(&mut *writer)
            .await
            .map_err(internal)?;
            drop(writer);
            // An unverified hello name is logged only once known (0022).
            match disabled {
                Some(_) => self.refuse(&hello.name, Reason::Disabled),
                None => self.refuse_identity("", peer),
            }
            return Err(unauthenticated());
        };
        if name != hello.name {
            self.refuse_identity(name, peer);
            return Err(unauthenticated());
        }
        if hello.hostname.len() > MAX_HOSTNAME || hello.hostname.chars().any(char::is_control) {
            return Err(invalid_field(
                "hello.hostname",
                "must be at most 253 characters without control characters",
            ));
        }
        let mut writer = writer.lock().await;
        let id: i64 = sqlx::query_scalar("SELECT id FROM runner WHERE name = ?")
            .bind(name)
            .fetch_one(&mut *writer)
            .await
            .map_err(internal)?;
        let server = env!("CARGO_PKG_VERSION");
        let refusal = if !compatible(server, &hello.version) {
            Some((
                Reason::Version,
                refused(
                    tonic::Code::FailedPrecondition,
                    "VERSION_INCOMPATIBLE",
                    format!(
                        "runner version {} is incompatible with server version {server}",
                        hello.version
                    ),
                ),
            ))
        } else if !hello
            .capabilities
            .as_ref()
            .is_some_and(|announced| within(announced, &declaration.capabilities))
        {
            Some((
                Reason::Capabilities,
                invalid_field(
                    "hello.capabilities",
                    "must be within the runner's declared capabilities",
                ),
            ))
        } else {
            None
        };
        if let Some((reason, status)) = refusal {
            self.refuse(name, reason);
            // The identity was proven: the refusal is journaled (0108).
            journal(
                &mut writer,
                &declared.bus,
                None,
                runner_event(
                    id,
                    request_id,
                    "runner-refused",
                    serde_json::json!({ "runner": name, "reason": reason.as_str() }),
                ),
            )
            .await
            .map_err(internal)?;
            return Err(status);
        }
        let announced = hello.capabilities.clone().unwrap_or_default();
        let offer = Offer {
            runner_id: id,
            arch: announced.arch,
            os: announced.os,
            backends: announced.backends,
            tags: announced.tags,
            max_jobs: announced.max_jobs.into(),
        };
        let (replaced, receiver) = oneshot::channel();
        let generation = {
            let mut live = self.live.lock().unwrap();
            live.0 += 1;
            let generation = live.0;
            if let Some(older) = live.1.insert(
                name.to_owned(),
                Live {
                    generation,
                    replaced,
                    outbound,
                    offer,
                },
            ) {
                let _ = older.replaced.send(());
            }
            self.registry.set_runners_connected(live.1.len() as u64);
            generation
        };
        let result = journal(
            &mut writer,
            &declared.bus,
            Some((
                "UPDATE runner SET version = ?, hostname = ?, connected = 1, last_contact_at = ? WHERE id = ?",
                &[&hello.version, &hello.hostname],
                crate::db::runs::now(),
                id,
            )),
            runner_event(
                id,
                request_id,
                "runner-connected",
                serde_json::json!({ "runner": name, "version": hello.version, "hostname": hello.hostname }),
            ),
        )
        .await;
        if let Err(err) = result {
            self.forget(name, generation);
            return Err(internal(err));
        }
        tracing::info!(
            event = "runner-connected",
            runner = name,
            version = hello.version,
            hostname = hello.hostname
        );
        declared.wake.notify_one();
        Ok((id, generation, receiver))
    }

    /// Unregister a stream if it is still the current one; true when it was.
    fn forget(&self, name: &str, generation: u64) -> bool {
        let mut live = self.live.lock().unwrap();
        if live
            .1
            .get(name)
            .is_none_or(|live| live.generation != generation)
        {
            return false;
        }
        live.1.remove(name);
        self.registry.set_runners_connected(live.1.len() as u64);
        true
    }

    /// A stream ended by the runner or by an error; a replaced one leaves the row connected.
    async fn disconnected(&self, id: i64, name: &str, generation: u64, request_id: &str) {
        let (Some(writer), Some(declared)) = (self.health.writer.get(), self.health.runners.get())
        else {
            return;
        };
        let mut writer = writer.lock().await;
        if !self.forget(name, generation) {
            return;
        }
        let result = journal(
            &mut writer,
            &declared.bus,
            Some((
                "UPDATE runner SET connected = 0, last_contact_at = ? WHERE id = ?",
                &[],
                crate::db::runs::now(),
                id,
            )),
            runner_event(
                id,
                request_id,
                "runner-disconnected",
                serde_json::json!({ "runner": name }),
            ),
        )
        .await;
        match result {
            Ok(()) => tracing::info!(event = "runner-disconnected", runner = name),
            Err(err) => tracing::error!(event = "runner-disconnected", runner = name, err = %err),
        }
    }
}

impl Runners {
    /// Expire leases and match pending jobs, on every wake-up and at least every second.
    pub async fn schedule(self) {
        let Some(declared) = self.health.runners.get() else {
            return;
        };
        loop {
            self.pass(crate::db::runs::now()).await;
            tokio::select! {
                () = declared.wake.notified() => {}
                () = tokio::time::sleep(PASS_PERIOD) => {}
            }
        }
    }

    async fn pass(&self, now: i64) {
        let (Some(writer), Some(declared)) = (self.health.writer.get(), self.health.runners.get())
        else {
            return;
        };
        let bus = &declared.bus;
        let mut writer = writer.lock().await;
        let result = async {
            let mut tx = writer.begin().await?;
            let fresh: Vec<i64> = sqlx::query_scalar(
                "SELECT id FROM runner WHERE connected = 1 AND last_contact_at >= ?",
            )
            .bind(now - attempts::LEASE_MS)
            .fetch_all(&mut *tx)
            .await?;
            // Read under the writer lock; an open socket alone cannot establish liveness.
            let offers: Vec<Offer> = self
                .live
                .lock()
                .unwrap()
                .1
                .values()
                .filter(|live| fresh.contains(&live.offer.runner_id))
                .map(|live| live.offer.clone())
                .collect();
            // Expiry first: a replayed job is matched in the same pass.
            let changes = attempts::expire(&mut tx, now).await?;
            let matched = attempts::assign(&mut tx, &offers, now).await?;
            bus.commit(tx).await?;
            Ok::<_, sqlx::Error>((changes, matched))
        }
        .await;
        drop(writer);
        // A finished worktree run's bundle goes at once, its run at retention (0122).
        if let Some(database) = self.health.database.get() {
            let bundles = self.health.state_dir.join("ci/bundles");
            let dropped = match database.acquire().await {
                Ok(mut connection) => {
                    crate::db::retention::drop_finished_bundles(&mut connection, &bundles).await
                }
                Err(err) => Err(err.to_string()),
            };
            if let Err(err) = dropped {
                tracing::warn!(event = "bundle-cleanup-failed", err);
            }
        }
        let assigned = match result {
            Ok((changes, (assigned, pending))) => {
                if !changes.is_empty() || !assigned.is_empty() {
                    self.health.ci_progress.send_replace(());
                }
                self.runs_changed(changes);
                self.registry.set_jobs_pending(pending as u64);
                assigned
            }
            Err(err) => {
                tracing::error!(event = "runner-matching-failed", err = %err);
                return;
            }
        };
        for assigned in assigned {
            let outbound = self
                .live
                .lock()
                .unwrap()
                .1
                .iter()
                .find(|(_, live)| live.offer.runner_id == assigned.runner_id)
                .map(|(name, live)| (name.clone(), live.generation, live.outbound.clone()));
            // A runner gone since leaves its attempt to lease expiry (0008).
            if let Some((name, generation, outbound)) = outbound {
                let message = connect_response::Message::Assignment(Assignment {
                    attempt_id: assigned.attempt,
                    repo_public_id: assigned.repo,
                    sha: assigned.sha,
                    system: assigned.system,
                    checks: assigned.checks,
                });
                // A stalled runner must not stop matching or lease expiry (0109).
                if outbound.try_send(Ok(response(message))).is_err() {
                    self.disconnected(assigned.runner_id, &name, generation, "")
                        .await;
                }
            }
        }
    }

    /// Answer each in-flight attempt of a reconnected runner with continue, followed by
    /// its log acknowledgment so the runner resends from there, or cancel (0008).
    async fn resume(&self, id: i64, attempts: &[String]) -> Result<Vec<ConnectResponse>, Status> {
        if attempts.is_empty() {
            return Ok(Vec::new());
        }
        let writer = self
            .health
            .writer
            .get()
            .ok_or_else(|| internal("no writer"))?;
        let mut writer = writer.lock().await;
        let mut tx = writer.begin().await.map_err(internal)?;
        let answers = attempts::resume(&mut tx, id, attempts, crate::db::runs::now())
            .await
            .map_err(internal)?;
        tx.commit().await.map_err(internal)?;
        let mut responses = Vec::new();
        for (attempt, answer) in attempts.iter().zip(answers) {
            let Some(sequence) = answer else {
                responses.push(cancel(attempt));
                continue;
            };
            responses.push(response(connect_response::Message::ContinueAttempt(
                ContinueAttempt {
                    attempt_id: attempt.clone(),
                },
            )));
            responses.push(response(connect_response::Message::LogAck(LogAck {
                attempt_id: attempt.clone(),
                sequence,
            })));
        }
        Ok(responses)
    }

    /// Log `run-state` for each run that changed and count the finished ones.
    fn runs_changed(&self, changes: Vec<(String, RunState)>) {
        for (run, state) in changes {
            tracing::info!(event = "run-state", run, state = state.as_str());
            self.registry.run_finished(state.as_str());
        }
    }

    /// Apply one execution message; the answers go back on the runner's stream.
    async fn handle(
        &self,
        runner: (i64, &str),
        request_id: &str,
        message: Message,
    ) -> Result<Vec<ConnectResponse>, Status> {
        let (writer, declared) = match (self.health.writer.get(), self.health.runners.get()) {
            (Some(writer), Some(declared)) => (writer, declared),
            _ => return Err(internal("not started")),
        };
        let mut writer = writer.lock().await;
        let now = crate::db::runs::now();
        let mut tx = writer.begin().await.map_err(internal)?;
        let mut truncated = false;
        let (attempt, outcome, acknowledgment) = match message {
            Message::Heartbeat(_) => {
                attempts::heartbeat(&mut tx, runner.0, now)
                    .await
                    .map_err(internal)?;
                tx.commit().await.map_err(internal)?;
                return Ok(Vec::new());
            }
            Message::Accept(accept) => {
                let outcome =
                    attempts::accept(&mut tx, runner, &accept.attempt_id, request_id, now)
                        .await
                        .map_err(internal)?;
                (accept.attempt_id, outcome, None)
            }
            Message::LogChunk(chunk) => {
                if chunk.data.len() > MAX_CHUNK {
                    return Err(invalid_field(
                        "log_chunk.data",
                        "must be at most 65536 bytes",
                    ));
                }
                let appended = attempts::append_log(
                    &mut tx,
                    &self.health.state_dir.join("ci/logs"),
                    runner.0,
                    &attempts::Chunk {
                        attempt: &chunk.attempt_id,
                        sequence: chunk.sequence,
                        step: chunk.step_index,
                        data: &chunk.data,
                    },
                    now,
                )
                .await
                .map_err(internal)?;
                truncated = appended.is_some_and(|(_, truncated)| truncated);
                let highest = appended.map(|(highest, _)| highest);
                let acknowledgment = highest.map(|sequence| {
                    connect_response::Message::LogAck(LogAck {
                        attempt_id: chunk.attempt_id.clone(),
                        sequence,
                    })
                });
                let outcome = match highest {
                    Some(_) => Outcome::Done(Vec::new()),
                    None => Outcome::Cancel,
                };
                (chunk.attempt_id, outcome, acknowledgment)
            }
            Message::StepStatus(status) => {
                let Some(state) = step_state(status.state()) else {
                    return Ok(Vec::new());
                };
                let current = attempts::step(
                    &mut tx,
                    runner.0,
                    &status.attempt_id,
                    (status.step_index, &status.name, state, status.exit_code),
                    now,
                )
                .await
                .map_err(internal)?;
                let outcome = match current {
                    true => Outcome::Done(Vec::new()),
                    false => Outcome::Cancel,
                };
                (status.attempt_id, outcome, None)
            }
            Message::Result(result) => {
                let state = match result.state() {
                    ResultState::Succeeded => AttemptState::Succeeded,
                    ResultState::Failed => AttemptState::Failed,
                    ResultState::Cancelled => AttemptState::Cancelled,
                    ResultState::Unspecified => {
                        return Err(invalid_field("result.state", "must be a terminal state"));
                    }
                };
                let outcome = attempts::finish(
                    &mut tx,
                    runner,
                    &result.attempt_id,
                    &attempts::Finish {
                        state,
                        exit_code: result.exit_code,
                        check: &result.failing_check,
                        reason: &result.reason,
                    },
                    request_id,
                    now,
                )
                .await
                .map_err(internal)?;
                let acknowledgment = connect_response::Message::ResultAck(ResultAck {
                    attempt_id: result.attempt_id.clone(),
                });
                (result.attempt_id, outcome, Some(acknowledgment))
            }
            Message::Hello(_) => return Err(invalid_field("hello", "must be sent once")),
        };
        declared.bus.commit(tx).await.map_err(internal)?;
        drop(writer);
        if truncated {
            self.registry.log_truncated();
        }
        self.health.ci_progress.send_replace(());
        Ok(match outcome {
            Outcome::Cancel => vec![cancel(&attempt)],
            Outcome::Done(changes) => {
                if changes.iter().any(|(_, state)| state.is_terminal()) {
                    // The runner's capacity is free again (0109: matching on attempt end).
                    declared.wake.notify_one();
                }
                self.runs_changed(changes);
                acknowledgment.into_iter().map(response).collect()
            }
        })
    }
}

fn step_state(state: StepState) -> Option<&'static str> {
    Some(match state {
        StepState::Unspecified => return None,
        StepState::Running => "running",
        StepState::Succeeded => "succeeded",
        StepState::Failed => "failed",
        StepState::Skipped => "skipped",
    })
}

/// Send the answers, or the status that ends the stream; false once ended.
async fn answer(outbound: &Outbound, answers: Result<Vec<ConnectResponse>, Status>) -> bool {
    match answers {
        Ok(answers) => {
            for answer in answers {
                let _ = outbound.send(Ok(answer)).await;
            }
            true
        }
        Err(status) => {
            let _ = outbound.send(Err(status)).await;
            false
        }
    }
}

type Responses = Pin<Box<dyn Stream<Item = Result<ConnectResponse, Status>> + Send>>;

#[tonic::async_trait]
impl RunnerService for Runners {
    type ConnectStream = Responses;
    type FetchRunBundleStream =
        Pin<Box<dyn Stream<Item = Result<FetchRunBundleResponse, Status>> + Send>>;

    async fn connect(
        &self,
        request: Request<Streaming<ConnectRequest>>,
    ) -> Result<Response<Self::ConnectStream>, Status> {
        let peer = request.extensions().get::<Peer>().cloned();
        let request_id = request
            .extensions()
            .get::<super::correlation::RequestId>()
            .map_or_else(String::new, |id| id.0.clone());
        let mut inbound = request.into_inner();
        let hello = match tokio::time::timeout(HELLO_TIMEOUT, inbound.message()).await {
            Ok(Ok(Some(ConnectRequest {
                message: Some(Message::Hello(hello)),
            }))) => hello,
            Ok(Err(status)) => return Err(status),
            _ => return Err(invalid_field("hello", "the first message must be a hello")),
        };
        let (outbound, receiver) = mpsc::channel(16);
        let (id, generation, mut replaced) = self
            .admit(peer.as_ref(), &hello, &request_id, outbound.clone())
            .await?;
        let runners = self.clone();
        tokio::spawn(async move {
            tokio::select! {
                _ = &mut replaced => {
                    let _ = outbound.try_send(Err(Status::aborted("runner connection ended")));
                }
                () = async {
                    let mut open = answer(
                        &outbound,
                        runners.resume(id, &hello.in_flight_attempt_ids).await,
                    )
                    .await;
                    while open {
                        tokio::select! {
                            message = inbound.message() => match message {
                                Ok(Some(ConnectRequest { message: Some(message) })) => {
                                    let answers = runners.handle((id, &hello.name), &request_id, message).await;
                                    open = answer(&outbound, answers).await;
                                }
                                Ok(Some(_)) => {}
                                Ok(None) | Err(_) => break,
                            },
                            () = outbound.closed() => break,
                        }
                    }
                } => {}
            }
            runners
                .disconnected(id, &hello.name, generation, &request_id)
                .await;
        });
        Ok(Response::new(Box::pin(ReceiverStream::new(receiver))))
    }

    async fn fetch_run_bundle(
        &self,
        request: Request<FetchRunBundleRequest>,
    ) -> Result<Response<Self::FetchRunBundleStream>, Status> {
        let peer = request.extensions().get::<Peer>().cloned();
        let FetchRunBundleRequest {
            attempt_id,
            mut have,
        } = request.into_inner();
        let ((runner, name), (repo, sha, run, trigger)) =
            self.leased(peer.as_ref(), &attempt_id).await?;
        let max_jobs = self
            .health
            .runners
            .get()
            .and_then(|declared| declared.runners.get(&name))
            .map_or(0, |declaration| declaration.capabilities.max_jobs as usize);
        let slot: Slot<String> = self.transfers.take(name, max_jobs).ok_or_else(|| {
            refused(
                tonic::Code::ResourceExhausted,
                "BUNDLE_TRANSFERS_LIMIT",
                format!(
                    "at most max_jobs transfers per runner and {MAX_TRANSFERS} in all may run at once"
                ),
            )
        })?;
        // The runner's cache keeps 16 commits (0106): more ids only make a smaller bundle.
        have.truncate(MAX_HAVE);
        let repos = self.health.git_state_dir.join("repos");
        // A worktree run's commit is only in its stored bundle (0097).
        let stored = (trigger == "worktree").then(|| {
            self.health
                .state_dir
                .join(format!("ci/bundles/{run}.bundle"))
        });
        let (sender, produced) = mpsc::channel(4);
        let abort = Arc::new(Abort::default());
        let aborted = abort.clone();
        tokio::task::spawn_blocking(move || {
            // Held until git is reaped (0119).
            let _slot = slot;
            if let Err(err) = sources(
                &repos,
                &repo,
                &sha,
                stored.as_deref(),
                &have,
                &sender,
                &aborted,
            ) {
                // A runner gone mid-transfer closed the channel: nothing to report to it.
                let _ = sender.blocking_send(Err(internal(format!("bundle of {repo}: {err}"))));
            }
        });
        let (out, receiver) = mpsc::channel(2);
        let runners = self.clone();
        tokio::spawn(async move {
            runners.relay(runner, &attempt_id, produced, out).await;
            abort.abort();
        });
        Ok(Response::new(Box::pin(ReceiverStream::new(receiver))))
    }
}

type Bundled = Result<FetchRunBundleResponse, Status>;

/// Lease checks of a running transfer happen this often.
const TRANSFER_LEASE_CHECK: Duration = Duration::from_secs(1);

impl Runners {
    /// Relay a transfer to the runner until it is complete, the stream ends, the attempt's
    /// lease is lost or nothing is delivered for [`TRANSFER_IDLE`] (0119).
    async fn relay(
        &self,
        runner: i64,
        attempt: &str,
        mut produced: mpsc::Receiver<Bundled>,
        out: mpsc::Sender<Bundled>,
    ) {
        let mut lease = tokio::time::interval(TRANSFER_LEASE_CHECK);
        let mut deadline = tokio::time::Instant::now() + TRANSFER_IDLE;
        let mut pending = None;
        loop {
            tokio::select! {
                // Two free places, one used: a final status always fits behind the data.
                permits = out.reserve_many(2), if pending.is_some() => {
                    let Some(permit) = permits.ok().and_then(|mut permits| permits.next()) else {
                        return;
                    };
                    permit.send(pending.take().expect("pending"));
                    deadline = tokio::time::Instant::now() + TRANSFER_IDLE;
                }
                item = produced.recv(), if pending.is_none() => match item {
                    Some(item) => pending = Some(item),
                    None => return,
                },
                () = out.closed() => return,
                _ = lease.tick() => {
                    let leased = match self.health.database.get() {
                        Some(database) => match database.acquire().await {
                            Ok(mut connection) => {
                                current_lease(&mut connection, runner, attempt).await
                            }
                            Err(err) => Err(err),
                        },
                        None => Ok(None),
                    };
                    if !matches!(leased, Ok(Some(_))) {
                        let _ = out.try_send(Err(not_leased()));
                        return;
                    }
                }
                () = tokio::time::sleep_until(deadline) => {
                    let _ = out.try_send(Err(Status::deadline_exceeded("no transfer progress")));
                    return;
                }
            }
        }
    }
}

fn not_leased() -> Status {
    refused(
        tonic::Code::FailedPrecondition,
        "INVALID_STATE",
        "the attempt is not current and leased".into(),
    )
}

/// The attempt when it is this runner's current one, running and leased.
async fn current_lease(
    connection: &mut SqliteConnection,
    runner: i64,
    attempt: &str,
) -> Result<Option<attempts::Current>, sqlx::Error> {
    Ok(attempts::current(connection, runner, attempt)
        .await?
        .filter(|current| {
            current.state == "running" && current.lease_expires_at > crate::db::runs::now()
        }))
}

/// Send an attempt's bundles. A push or manual run: its commit minus `have` (bundle 0).
/// A worktree run: the commits its stored bundle requires, minus `have` (bundle 0, omitted
/// when `have` holds them), then the stored bundle with its head renamed to
/// [`kajiba_gitstore::BUNDLE_REF`], so the runner never reads a client's ref name (bundle 1).
fn sources(
    repos: &std::path::Path,
    repo: &str,
    sha: &str,
    stored: Option<&std::path::Path>,
    have: &[String],
    sender: &mpsc::Sender<Bundled>,
    abort: &Abort,
) -> Result<(), String> {
    use std::io::Write;
    let path = repos.join(format!("{repo}.git"));
    let gix = Gix::shared(repos);
    let text = |err: &dyn std::fmt::Display| err.to_string();
    let open =
        |bundle| std::io::BufWriter::with_capacity(MAX_CHUNK, Chunks(sender.clone(), bundle));
    let Some(stored) = stored else {
        let mut out = open(0);
        gix.bundle(&path, &[sha.into()], have, &mut out, abort)
            .map_err(|err| text(&err))?;
        return out.flush().map_err(|err| text(&err));
    };
    let (prerequisites, head, length) =
        super::runs::bundle_header(stored).map_err(|status| status.message().to_owned())?;
    if !gix
        .covered(&path, &prerequisites, have)
        .map_err(|err| text(&err))?
    {
        let mut out = open(0);
        gix.bundle(&path, &prerequisites, have, &mut out, abort)
            .map_err(|err| text(&err))?;
        out.flush().map_err(|err| text(&err))?;
    }
    let mut header = String::from("# v2 git bundle\n");
    for id in &prerequisites {
        header.push_str(&format!("-{id}\n"));
    }
    header.push_str(&format!("{head} {}\n\n", kajiba_gitstore::BUNDLE_REF));
    let mut file = std::fs::File::open(stored).map_err(|err| text(&err))?;
    std::io::Seek::seek(&mut file, std::io::SeekFrom::Start(length)).map_err(|err| text(&err))?;
    let mut out = open(1);
    out.write_all(header.as_bytes())
        .and_then(|()| std::io::copy(&mut file, &mut out).map(|_| ()))
        .and_then(|()| out.flush())
        .map_err(|err| text(&err))
}

/// The bytes of bundle `.1` as consecutive `FetchRunBundleResponse` messages.
struct Chunks(mpsc::Sender<Bundled>, u32);

impl std::io::Write for Chunks {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.0
            .blocking_send(Ok(FetchRunBundleResponse {
                data: data.to_vec(),
                bundle: self.1,
            }))
            .map_err(|_| std::io::ErrorKind::BrokenPipe)?;
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Runners {
    /// The runner (id, name), then the repository public id, commit, run public id and
    /// trigger of an attempt that is its job's current one, still leased and held by the
    /// runner whose certificate is presented (0106, 0124).
    async fn leased(
        &self,
        peer: Option<&Peer>,
        attempt: &str,
    ) -> Result<((i64, String), (String, String, String, String)), Status> {
        let (Some(declared), Some(database)) =
            (self.health.runners.get(), self.health.database.get())
        else {
            return Err(refused(
                tonic::Code::Unavailable,
                "MAINTENANCE",
                "starting".into(),
            ));
        };
        let mut connection = database.acquire().await.map_err(internal)?;
        let holder: Option<(i64, String)> = sqlx::query_as(
            "SELECT n.id, n.name FROM job_attempt a JOIN runner n ON n.id = a.runner_id WHERE a.public_id = ?",
        )
        .bind(attempt)
        .fetch_optional(&mut *connection)
        .await
        .map_err(internal)?;
        let caller = declared_by(&declared.runners, peer).map(|(name, _)| name);
        let Some((id, name)) = holder.filter(|(_, name)| caller == Some(name.as_str())) else {
            self.refuse_identity(caller.unwrap_or(""), peer);
            return Err(unauthenticated());
        };
        let current = current_lease(&mut connection, id, attempt)
            .await
            .map_err(internal)?
            .ok_or_else(not_leased)?;
        let source = sqlx::query_as(
            "SELECT repo.public_id, run.sha, run.public_id, run.trigger FROM run JOIN repo ON repo.id = run.repo_id WHERE run.id = ?",
        )
        .bind(current.run_id)
        .fetch_one(&mut *connection)
        .await
        .map_err(internal)?;
        Ok(((id, name), source))
    }
}

#[cfg(test)]
mod tests;
