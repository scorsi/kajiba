//! `kajiba.runner.v1` admission (0008, 0026, 0105, 0108): served on the public port,
//! outside the `kajiba.v1` auth layer and the request timeout.

use super::{Health, auth::compatible, cursor::invalid_field, local::refused};
use crate::{
    db::{
        attempts::{self, Offer, Outcome},
        events::{EventBus, NewEvent, append_event},
        runs::{AttemptState, RunState},
    },
    metrics::Registry,
};
use kajiba_core::config::{Capabilities, RunnerDeclaration};
use kajiba_gitstore::{GitStore, Gix};
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
use tokio::sync::{Notify, mpsc, oneshot};
use tokio_stream::{Stream, wrappers::ReceiverStream};
use tonic::{Request, Response, Status, Streaming, metadata::MetadataMap};

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
}

impl Runners {
    pub fn new(health: Health, registry: Arc<Registry>) -> Self {
        Self {
            health,
            registry,
            live: Arc::default(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Reason {
    Unknown,
    Disabled,
    Secret,
    Version,
    Capabilities,
}

impl Reason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Disabled => "disabled",
            Self::Secret => "secret",
            Self::Version => "version",
            Self::Capabilities => "capabilities",
        }
    }
}

fn bearer(metadata: &MetadataMap) -> Option<&[u8]> {
    metadata
        .get("authorization")?
        .as_bytes()
        .strip_prefix(b"Bearer ")
}

/// Compare digests so the time taken does not depend on where the secrets differ.
fn same_secret(a: &[u8], b: &[u8]) -> bool {
    let (a, b) = (kajiba_core::sha256(a), kajiba_core::sha256(b));
    a.iter().zip(b).fold(0, |diff, (x, y)| diff | (x ^ y)) == 0
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

    /// Check the hello against the declaration; on success the row is connected and the
    /// stream registered, replacing an older one under the same name.
    async fn admit(
        &self,
        secret: Option<Vec<u8>>,
        hello: &Hello,
        request_id: &str,
        outbound: Outbound,
    ) -> Result<(i64, u64, oneshot::Receiver<()>), Status> {
        let unauthenticated = || {
            refused(
                tonic::Code::Unauthenticated,
                "CREDENTIAL_INVALID",
                "unknown runner or wrong secret".into(),
            )
        };
        let declared =
            self.health.runners.get().ok_or_else(|| {
                refused(tonic::Code::Unavailable, "MAINTENANCE", "starting".into())
            })?;
        let writer = self
            .health
            .writer
            .get()
            .ok_or_else(|| internal("no writer"))?;
        // A rejected name may be a misplaced secret: logged only once known (0022).
        let Some(declaration) = declared.runners.get(&hello.name) else {
            let mut writer = writer.lock().await;
            let disabled: Option<i64> = sqlx::query_scalar(
                "SELECT 1 FROM runner WHERE name = ? AND disabled_at IS NOT NULL",
            )
            .bind(&hello.name)
            .fetch_optional(&mut *writer)
            .await
            .map_err(internal)?;
            drop(writer);
            match disabled {
                Some(_) => self.refuse(&hello.name, Reason::Disabled),
                None => self.refuse("", Reason::Unknown),
            }
            return Err(unauthenticated());
        };
        let name = hello.name.as_str();
        let expected = tokio::fs::read(&declaration.token_file)
            .await
            .map_err(|err| {
                internal(format!(
                    "runners.{name}.token_file ({}): {err}",
                    declaration.token_file.display()
                ))
            })?;
        // A secret file usually ends with a newline; metadata cannot carry one.
        if !secret.is_some_and(|secret| same_secret(&secret, expected.trim_ascii())) {
            self.refuse(name, Reason::Secret);
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
            // The secret was proven: the refusal is journaled (0108).
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
        // Read under the writer lock, like admission, so it agrees with `runner.connected`.
        let offers: Vec<Offer> = self
            .live
            .lock()
            .unwrap()
            .1
            .values()
            .map(|live| live.offer.clone())
            .collect();
        let result = async {
            let mut tx = writer.begin().await?;
            // Expiry first: a replayed job is matched in the same pass.
            let changes = attempts::expire(&mut tx, now).await?;
            let matched = attempts::assign(&mut tx, &offers, now).await?;
            bus.commit(tx).await?;
            Ok::<_, sqlx::Error>((changes, matched))
        }
        .await;
        drop(writer);
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
                .values()
                .find(|live| live.offer.runner_id == assigned.runner_id)
                .map(|live| live.outbound.clone());
            // A runner gone since leaves its attempt to lease expiry (0008).
            if let Some(outbound) = outbound {
                let message = connect_response::Message::Assignment(Assignment {
                    attempt_id: assigned.attempt,
                    repo_public_id: assigned.repo,
                    sha: assigned.sha,
                    system: assigned.system,
                });
                let _ = outbound.send(Ok(response(message))).await;
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
        let now = crate::db::runs::now();
        let mut writer = writer.lock().await;
        let mut tx = writer.begin().await.map_err(internal)?;
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
                let highest = attempts::append_log(
                    &mut tx,
                    &self.health.state_dir.join("ci/logs"),
                    runner.0,
                    &attempts::Chunk {
                        attempt: &chunk.attempt_id,
                        sequence: chunk.sequence,
                        step: chunk.step_index,
                        data: &chunk.data,
                    },
                )
                .await
                .map_err(internal)?;
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
        let secret = bearer(request.metadata()).map(<[u8]>::to_vec);
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
            .admit(secret, &hello, &request_id, outbound.clone())
            .await?;
        let runners = self.clone();
        tokio::spawn(async move {
            let mut open = answer(
                &outbound,
                runners.resume(id, &hello.in_flight_attempt_ids).await,
            )
            .await;
            while open {
                tokio::select! {
                    _ = &mut replaced => {
                        let _ = outbound
                            .send(Err(Status::aborted("replaced by a newer connection of this runner")))
                            .await;
                        return;
                    }
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
        let secret = bearer(request.metadata()).map(<[u8]>::to_vec);
        let FetchRunBundleRequest {
            attempt_id,
            mut have,
        } = request.into_inner();
        let (repo, sha) = self.leased(secret, &attempt_id).await?;
        // The runner's cache keeps 16 commits (0106): more ids only make a smaller bundle.
        have.truncate(MAX_HAVE);
        let repos = self.health.git_state_dir.join("repos");
        let (sender, receiver) = mpsc::channel(4);
        tokio::task::spawn_blocking(move || {
            let mut out = std::io::BufWriter::with_capacity(MAX_CHUNK, Chunks(sender.clone()));
            let path = repos.join(format!("{repo}.git"));
            let written = Gix::shared(&repos)
                .bundle(&path, &sha, &have, &mut out)
                .map_err(|err| err.to_string())
                .and_then(|()| std::io::Write::flush(&mut out).map_err(|err| err.to_string()));
            if let Err(err) = written {
                // A runner gone mid-transfer closed the channel: nothing to report to it.
                let _ = sender.blocking_send(Err(internal(format!("bundle of {repo}: {err}"))));
            }
        });
        Ok(Response::new(Box::pin(ReceiverStream::new(receiver))))
    }
}

/// Bundle bytes as consecutive `FetchRunBundleResponse` messages.
struct Chunks(mpsc::Sender<Result<FetchRunBundleResponse, Status>>);

impl std::io::Write for Chunks {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.0
            .blocking_send(Ok(FetchRunBundleResponse {
                data: data.to_vec(),
            }))
            .map_err(|_| std::io::ErrorKind::BrokenPipe)?;
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Runners {
    /// The repository public id and commit of an attempt that is its job's current one,
    /// still leased and held by the runner whose secret is presented (0106).
    async fn leased(
        &self,
        secret: Option<Vec<u8>>,
        attempt: &str,
    ) -> Result<(String, String), Status> {
        let unauthenticated = || {
            refused(
                tonic::Code::Unauthenticated,
                "CREDENTIAL_INVALID",
                "unknown runner or wrong secret".into(),
            )
        };
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
        let Some((id, name)) = holder.filter(|(_, name)| declared.runners.contains_key(name))
        else {
            self.refuse("", Reason::Unknown);
            return Err(unauthenticated());
        };
        let declaration = &declared.runners[&name];
        let expected = tokio::fs::read(&declaration.token_file)
            .await
            .map_err(|err| internal(format!("runners.{name}.token_file: {err}")))?;
        if !secret.is_some_and(|secret| same_secret(&secret, expected.trim_ascii())) {
            self.refuse(&name, Reason::Secret);
            return Err(unauthenticated());
        }
        let current = attempts::current(&mut connection, id, attempt)
            .await
            .map_err(internal)?
            .filter(|current| {
                current.state == "running" && current.lease_expires_at > crate::db::runs::now()
            })
            .ok_or_else(|| {
                refused(
                    tonic::Code::FailedPrecondition,
                    "INVALID_STATE",
                    "the attempt is not current and leased".into(),
                )
            })?;
        sqlx::query_as(
            "SELECT repo.public_id, run.sha FROM run JOIN repo ON repo.id = run.repo_id WHERE run.id = ?",
        )
        .bind(current.run_id)
        .fetch_one(&mut *connection)
        .await
        .map_err(internal)
    }
}

#[cfg(test)]
mod tests;
