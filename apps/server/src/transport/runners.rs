//! `kajiba.runner.v1` admission (0008, 0026, 0105, 0108): served on the public port,
//! outside the `kajiba.v1` auth layer and the request timeout.

use super::{Health, auth::compatible, cursor::invalid_field, local::refused};
use crate::{
    db::events::{EventBus, NewEvent, append_event},
    metrics::Registry,
};
use kajiba_core::config::{Capabilities, RunnerDeclaration};
use kajiba_proto::runner::v1::{
    ConnectRequest, ConnectResponse, FetchRunBundleRequest, FetchRunBundleResponse, Hello,
    connect_request::Message, runner_service_server::RunnerService,
};
use sqlx::{Connection, SqliteConnection};
use std::{
    collections::BTreeMap,
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::{mpsc, oneshot};
use tokio_stream::{Stream, wrappers::ReceiverStream};
use tonic::{Request, Response, Status, Streaming, metadata::MetadataMap};

/// Declared runners and the bus their events wake, set once startup has reconciled them.
pub struct Declared {
    pub runners: BTreeMap<String, RunnerDeclaration>,
    pub bus: Arc<EventBus>,
}

/// A runner silent this long after opening `Connect` is dropped: one heartbeat period (0105).
const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_HOSTNAME: usize = 253;

struct Live {
    generation: u64,
    replaced: oneshot::Sender<()>,
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

type Outbound = Pin<Box<dyn Stream<Item = Result<ConnectResponse, Status>> + Send>>;

#[tonic::async_trait]
impl RunnerService for Runners {
    type ConnectStream = Outbound;
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
        let (id, generation, mut replaced) = self.admit(secret, &hello, &request_id).await?;
        let (outbound, receiver) = mpsc::channel(16);
        let runners = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut replaced => {
                        let _ = outbound
                            .send(Err(Status::aborted("replaced by a newer connection of this runner")))
                            .await;
                        return;
                    }
                    message = inbound.message() => match message {
                        // Execution messages are handled from J6 task 8.
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
        _: Request<FetchRunBundleRequest>,
    ) -> Result<Response<Self::FetchRunBundleStream>, Status> {
        // Delivered by J6 task 10.
        Err(Status::unimplemented("FetchRunBundle"))
    }
}

#[cfg(test)]
mod tests;
