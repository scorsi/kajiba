use crate::{
    db::events::EventBus,
    metrics::{self, Registry},
};
use axum::{
    Router,
    extract::{Request as HttpRequest, State},
    http::{StatusCode, header::CONTENT_TYPE},
    middleware::{self, Next},
    response::{IntoResponse, Response as HttpResponse},
    routing::get,
};
use hyper_util::{
    rt::{TokioExecutor, TokioIo, TokioTimer},
    server::conn::auto::Builder,
    service::TowerToHyperService,
};
use kajiba_core::{config::ServerConfig, svid};
use kajiba_proto::runner::v1::runner_service_server::RunnerServiceServer;
use kajiba_proto::v1::{
    CheckRequest, CheckResponse,
    auth_service_server::AuthServiceServer,
    check_response::ServingStatus,
    health_service_server::{HealthService, HealthServiceServer},
    repo_service_server::RepoServiceServer,
    run_service_server::RunServiceServer,
    token_service_server::TokenServiceServer,
};
use sqlx::SqlitePool;
use std::{
    io,
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, OnceLock},
    time::{Duration, Instant, SystemTime},
};
use tokio::sync::{Mutex, Semaphore, watch};
use tonic::{Request, Response, Status, service::Routes};

#[derive(Clone)]
pub struct Health {
    database: Arc<OnceLock<SqlitePool>>,
    writer: Arc<OnceLock<crate::db::Writer>>,
    passkeys: Arc<OnceLock<webauthn::Passkeys>>,
    ui_dir: Arc<OnceLock<PathBuf>>,
    runners: Arc<OnceLock<runners::Declared>>,
    /// Bumped after each commit that stores log bytes or ends attempts: wakes run watchers.
    ci_progress: Arc<watch::Sender<()>>,
    state_dir: PathBuf,
    git_state_dir: PathBuf,
    local_serving: Arc<local::Serving>,
    public_reads: Arc<Semaphore>,
    webauthn_requests: Arc<Semaphore>,
    /// `WatchRun` streams per principal (0120).
    watchers: Arc<slots::Slots<i64>>,
    /// Worktree bundle uploads in progress per user (0122).
    uploads: Arc<slots::Slots<i64>>,
    gate_serving: Arc<local::Serving>,
    git_side: Arc<git_side::Link>,
    ssh_probe: Option<SocketAddr>,
    ssh_status: Arc<Mutex<Option<(Instant, bool)>>>,
}

const SSH_PROBE_TTL: Duration = Duration::from_secs(3);
const SSH_PROBE_TIMEOUT: Duration = Duration::from_secs(3);

impl Health {
    pub fn new(state_dir: PathBuf, git_state_dir: PathBuf, ssh_probe: Option<SocketAddr>) -> Self {
        Self {
            database: Arc::default(),
            writer: Arc::default(),
            passkeys: Arc::default(),
            ui_dir: Arc::default(),
            runners: Arc::default(),
            ci_progress: Arc::new(watch::Sender::new(())),
            git_state_dir,
            state_dir,
            local_serving: Arc::default(),
            public_reads: Arc::new(Semaphore::new(PUBLIC_READS)),
            webauthn_requests: Arc::new(Semaphore::new(webauthn::MAX_REQUESTS)),
            watchers: slots::Slots::new(runs::MAX_WATCHERS),
            uploads: slots::Slots::new(usize::MAX),
            gate_serving: Arc::default(),
            git_side: Arc::default(),
            ssh_probe,
            ssh_status: Arc::default(),
        }
    }

    // Called only after migrations, reconciliation and event bus initialization.
    pub fn started(&self, database: SqlitePool, writer: crate::db::Writer) {
        assert!(
            self.database.set(database).is_ok() && self.writer.set(writer).is_ok(),
            "startup completed twice"
        );
    }

    /// The WebAuthn relying party; the ceremony routes answer 503 until it is set.
    /// `serve --ui-dir`: page and asset requests no route answers read this bundle.
    pub fn set_ui_dir(&self, dir: PathBuf) {
        let _ = self.ui_dir.set(dir);
    }

    /// The declared runners; `Connect` answers `UNAVAILABLE` until they are set.
    pub fn set_runners(&self, declared: runners::Declared) {
        assert!(self.runners.set(declared).is_ok(), "runners set twice");
    }

    pub fn set_passkeys(&self, passkeys: webauthn::Passkeys) {
        assert!(self.passkeys.set(passkeys).is_ok(), "passkeys set twice");
    }

    async fn ready(&self) -> bool {
        let Some(database) = self.database.get() else {
            return false;
        };
        self.local_serving.ready()
            && self.gate_serving.ready()
            && self.git_side.ready()
            && self.git_state_dir.join("repos").is_dir()
            && ["db", "ci/logs", "ci/artifacts", "ci/bundles", "tmp"]
                .iter()
                .all(|name| self.state_dir.join(name).is_dir())
            && sqlx::query_scalar::<_, i64>("SELECT 1")
                .fetch_one(database)
                .await
                .is_ok()
            && self.ssh_ready().await
    }

    async fn ssh_ready(&self) -> bool {
        use tokio::io::AsyncReadExt;

        let Some(address) = self.ssh_probe else {
            return true;
        };
        // Coalesce HTTP and RPC probes, including concurrent cache misses.
        let mut status = self.ssh_status.lock().await;
        if let Some((checked, ready)) = *status {
            if checked.elapsed() < SSH_PROBE_TTL {
                return ready;
            }
        }
        // A canceled request must not let the next caller immediately probe again.
        *status = Some((Instant::now(), false));
        // Bound both connect and banner read; a listening non-SSH service is not ready.
        let ready = tokio::time::timeout(SSH_PROBE_TIMEOUT, async {
            let mut stream = tokio::net::TcpStream::connect(address).await?;
            let mut banner = [0; 8];
            stream.read_exact(&mut banner).await?;
            Ok::<_, io::Error>(&banner == b"SSH-2.0-")
        })
        .await
        .is_ok_and(|result| result.unwrap_or(false));
        *status = Some((Instant::now(), ready));
        ready
    }
}

#[tonic::async_trait]
impl HealthService for Health {
    async fn check(&self, _: Request<CheckRequest>) -> Result<Response<CheckResponse>, Status> {
        let status = if self.ready().await {
            ServingStatus::Serving
        } else {
            ServingStatus::NotServing
        };
        Ok(Response::new(CheckResponse {
            status: status.into(),
        }))
    }
}

fn router(
    health: Health,
    reflection: bool,
    registry: Arc<Registry>,
    public_url: &str,
) -> Result<Router, String> {
    let api = Routes::new(HealthServiceServer::new(health.clone()))
        .add_service(AuthServiceServer::new(auth::Sessions(health.clone())))
        .add_service(TokenServiceServer::new(tokens::Tokens(health.clone())))
        .add_service(
            RepoServiceServer::new(repos::Repos(health.clone()))
                // Bound inbound work before handler admission (ADR 0057).
                .max_decoding_message_size(16 * 1024),
        )
        // WatchRun's stream outlives the request timeout, which bounds response heads only.
        .add_service(
            RunServiceServer::new(runs::Runs(health.clone())).max_decoding_message_size(16 * 1024),
        )
        .into_axum_router()
        // Browsing responses keep their permit through delivery (ADR 0050).
        .route_layer(middleware::from_fn(stream::guarded))
        // Every `kajiba.v1` method, and only those, passes the single auth layer (0014).
        .route_layer(middleware::from_fn_with_state(
            auth::Auth::new(health.clone(), registry.clone())?,
            auth::authenticate,
        ));
    // grpc-web is translated before the auth layer, so it sees one protocol (0005).
    let ui_dir = health.ui_dir.clone();
    let mut app = api
        .clone()
        .route_layer(middleware::from_fn_with_state(
            web::grpc_web(api),
            web::translate,
        ))
        .route_layer(middleware::from_fn_with_state(
            web::origin(public_url)?,
            web::cors,
        ))
        // Ceremony routes: JSON, outside the gRPC auth layer, protected by their challenge (0020).
        .merge(webauthn::routes(health.clone()))
        .merge(git_http::routes(repos::Repos(health.clone())))
        .merge(raw::routes(repos::Repos(health.clone())))
        .route("/healthz", get(|| async { "OK" }))
        .route(
            "/readyz",
            get(move || {
                let health = health.clone();
                async move {
                    if health.ready().await {
                        StatusCode::OK
                    } else {
                        StatusCode::SERVICE_UNAVAILABLE
                    }
                }
            }),
        )
        // Only the routes added so far: reflection is a bidirectional stream.
        .route_layer(middleware::from_fn_with_state(TIMEOUTS, request_timeout));
    if reflection {
        let reflection = tonic_reflection::server::Builder::configure()
            .register_encoded_file_descriptor_set(kajiba_proto::FILE_DESCRIPTOR_SET)
            .build_v1()
            .map_err(|err| err.to_string())?;
        app = Routes::from(app).add_service(reflection).into_axum_router();
    }
    Ok(app
        .fallback(|request: HttpRequest| async move { ui_response(ui_dir.get(), request).await })
        .layer(middleware::from_fn_with_state(
            registry,
            correlation::request,
        )))
}

/// The runner listener's routes: `kajiba.runner.v1` only (0124). A runner's stream lasts as
/// long as the runner: no `kajiba.v1` layer, no timeout (0105); only its admission is bounded
/// (0119). Matching runs whether or not the listener is configured.
fn runner_router(health: Health, registry: Arc<Registry>) -> Router {
    let runner_service = runners::Runners::new(health.clone(), registry.clone());
    if health.runners.get().is_some() {
        tokio::spawn(runner_service.clone().schedule());
    }
    Routes::new(
        RunnerServiceServer::new(runner_service).max_decoding_message_size(runners::MAX_MESSAGE),
    )
    .into_axum_router()
    .route_layer(middleware::from_fn_with_state(
        Arc::new(Semaphore::new(runners::MAX_PENDING)),
        runners::admission,
    ))
    .layer(middleware::from_fn_with_state(
        registry,
        correlation::request,
    ))
}

/// Shared page fallback for unmatched paths and the two-segment repository route (0094).
async fn ui_response(dir: Option<&PathBuf>, request: HttpRequest) -> HttpResponse {
    let page = match (dir, request.headers().contains_key(CONTENT_TYPE)) {
        // A body-less GET or HEAD: a page or asset, never an API call.
        (Some(dir), false) => ui::file(dir, request.method(), request.uri().path()).await,
        _ => None,
    };
    page.unwrap_or_else(|| refusal(&request, StatusCode::NOT_FOUND, tonic::Code::Unimplemented))
}

/// Hardcoded public listener limits (ADR 0022 sets no configuration key for them).
#[derive(Clone, Copy)]
struct Limits {
    connections: usize,
    header_read: Duration,
}

const PUBLIC: Limits = Limits {
    connections: 1024,
    header_read: Duration::from_secs(30),
};
/// The runner listener (0119 as amended by 0126): connections, handshakes included, and the
/// time a TLS handshake may take.
const RUNNER_CONNECTIONS: usize = 64;
const RUNNER_HANDSHAKE: Duration = Duration::from_secs(10);
/// Time to produce a response head on non-streaming routes, request body included; a
/// worktree upload is bounded by its own idle and total times instead (0120).
#[derive(Clone, Copy)]
struct Timeouts {
    request: Duration,
    upload_idle: Duration,
    upload: Duration,
}

const TIMEOUTS: Timeouts = Timeouts {
    request: Duration::from_secs(30),
    upload_idle: Duration::from_secs(30),
    upload: Duration::from_secs(10 * 60),
};

/// The client-streaming upload of a worktree run's bundle (0097, 0120).
const UPLOAD_PATH: &str = "/kajiba.v1.RunService/CreateWorktreeRun";
/// Shared by public reads; gzip upload-pack costs eight permits (ADR 0058).
const PUBLIC_READS: usize = 32;

/// gRPC callers get a gRPC status (HTTP 200), everything else a plain HTTP status.
fn refusal(request: &HttpRequest, http: StatusCode, grpc: tonic::Code) -> HttpResponse {
    let is_grpc = request
        .headers()
        .get(CONTENT_TYPE)
        .is_some_and(|value| value.as_bytes().starts_with(b"application/grpc"));
    if is_grpc {
        tonic::Status::new(grpc, "")
            .into_http()
            .map(axum::body::Body::new)
    } else {
        http.into_response()
    }
}

async fn request_timeout(
    State(timeouts): State<Timeouts>,
    request: HttpRequest,
    next: Next,
) -> HttpResponse {
    let refused = refusal(
        &request,
        StatusCode::REQUEST_TIMEOUT,
        tonic::Code::DeadlineExceeded,
    );
    let (limit, request) = if request.uri().path() == UPLOAD_PATH {
        let idle = timeouts.upload_idle;
        let request = request.map(|body| axum::body::Body::new(Idle::new(body, idle)));
        (timeouts.upload, request)
    } else {
        (timeouts.request, request)
    };
    tokio::time::timeout(limit, next.run(request))
        .await
        .unwrap_or(refused)
}

/// A request body that fails once no byte arrived for its idle time (0120).
struct Idle {
    body: axum::body::Body,
    idle: Duration,
    deadline: std::pin::Pin<Box<tokio::time::Sleep>>,
}

impl Idle {
    fn new(body: axum::body::Body, idle: Duration) -> Self {
        Self {
            body,
            idle,
            deadline: Box::pin(tokio::time::sleep(idle)),
        }
    }
}

impl http_body::Body for Idle {
    type Data = axum::body::Bytes;
    type Error = axum::Error;

    fn poll_frame(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        use std::{future::Future, task::Poll};
        if self.deadline.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Some(Err(axum::Error::new(Status::deadline_exceeded(
                "no bytes received for the upload's idle time",
            )))));
        }
        if let Poll::Ready(frame) = std::pin::Pin::new(&mut self.body).poll_frame(cx) {
            if matches!(&frame, Some(Ok(frame)) if frame.data_ref().is_some_and(|data| !data.is_empty()))
            {
                let next = tokio::time::Instant::now() + self.idle;
                self.deadline.as_mut().reset(next);
            }
            return Poll::Ready(frame);
        }
        Poll::Pending
    }

    fn is_end_stream(&self) -> bool {
        self.body.is_end_stream()
    }
}

use kajiba_core::local::accept_backoff;

/// Resolve once stop is requested (or the sender is gone), holding no watch borrow.
async fn stopping(stopped: &mut watch::Receiver<bool>) {
    let _ = stopped.wait_for(|stop| *stop).await;
}

/// Serve HTTP/1.1 and HTTP/2 with a header read timeout and a connection cap. On stop,
/// close the listener, then let accepted connections finish their in-flight requests.
async fn serve_tcp(
    listener: tokio::net::TcpListener,
    app: Router,
    limits: Limits,
    stopped: watch::Receiver<bool>,
) -> io::Result<()> {
    let mut builder = Builder::new(TokioExecutor::new());
    builder
        .http1()
        .timer(TokioTimer::new())
        .header_read_timeout(limits.header_read);
    builder
        .http2()
        .timer(TokioTimer::new())
        .keep_alive_interval(limits.header_read);
    serve_with(
        listener,
        limits.connections,
        builder,
        stopped,
        move |stream| {
            let app = app.clone();
            async move { Some((stream, app, None)) }
        },
    )
    .await
}

/// Serve `kajiba.runner.v1` over TLS 1.3 with a verified client SVID, HTTP/2 only (0124):
/// each request carries the peer's [`svid::Peer`]; a handshake that fails or takes longer
/// than [`RUNNER_HANDSHAKE`] closes the connection.
async fn serve_runners(
    listener: tokio::net::TcpListener,
    source: Arc<svid::Source>,
    app: Router,
    stopped: watch::Receiver<bool>,
) -> io::Result<()> {
    let mut builder = Builder::new(TokioExecutor::new()).http2_only();
    builder
        .http2()
        .timer(TokioTimer::new())
        .keep_alive_interval(PUBLIC.header_read);
    serve_with(
        listener,
        RUNNER_CONNECTIONS,
        builder,
        stopped,
        move |stream| {
            let (app, source) = (app.clone(), source.clone());
            async move {
                let (tls, peer) = accept_runner(stream, &source).await?;
                let expiry = peer.not_after;
                Some((tls, app.layer(axum::Extension(peer)), Some(expiry)))
            }
        },
    )
    .await
}

async fn accept_runner<S>(
    stream: S,
    source: &Arc<svid::Source>,
) -> Option<(tokio_rustls::server::TlsStream<S>, svid::Peer)>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(source.server_config()));
    let tls = tokio::time::timeout(RUNNER_HANDSHAKE, acceptor.accept(stream))
        .await
        .ok()?
        .ok()?;
    if tls.get_ref().1.alpn_protocol() != Some(b"h2") {
        return None;
    }
    // The verifier accepted this leaf: it parses as a SPIFFE ID in the trust domain.
    let leaf = tls.get_ref().1.peer_certificates()?.first()?;
    let peer = svid::peer(leaf, source.trust_domain()).ok()?;
    Some((tls, peer))
}

/// Accept connections, each holding one of `connections` permits from its acceptance
/// (`accept`, e.g. a TLS handshake, included) to its end; serve them with `builder`.
async fn serve_with<S, F>(
    listener: tokio::net::TcpListener,
    connections: usize,
    builder: Builder<TokioExecutor>,
    mut stopped: watch::Receiver<bool>,
    accept: impl Fn(tokio::net::TcpStream) -> F,
) -> io::Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    F: std::future::Future<Output = Option<(S, Router, Option<SystemTime>)>> + Send + 'static,
{
    // Each connection holds a permit; holding them all again means every connection ended.
    let permits = Arc::new(Semaphore::new(connections));
    loop {
        let permit = tokio::select! {
            permit = permits.clone().acquire_owned() => permit.map_err(io::Error::other)?,
            () = stopping(&mut stopped) => break,
        };
        let stream = tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => stream,
                Err(err) => {
                    if let Some(delay) = accept_backoff(&err) {
                        tokio::time::sleep(delay).await;
                    }
                    continue;
                }
            },
            () = stopping(&mut stopped) => break,
        };
        let connection = builder.clone();
        let accepted = accept(stream);
        let stopped = stopped.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let Some((stream, app, expiry)) = accepted.await else {
                return;
            };
            serve_connection(stream, app, connection, stopped, expiry).await;
        });
    }
    drop(listener);
    let _ = permits.acquire_many(connections as u32).await;
    Ok(())
}

/// Drop the entire connection at the handshake's expiry so persistent bundle channels
/// reconnect with renewed credentials too (0124, 0125).
async fn serve_connection<S>(
    stream: S,
    app: Router,
    builder: Builder<TokioExecutor>,
    mut stopped: watch::Receiver<bool>,
    expiry: Option<SystemTime>,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let service = TowerToHyperService::new(app);
    let connection = builder.serve_connection_with_upgrades(TokioIo::new(stream), service);
    tokio::pin!(connection);
    let serve = async {
        tokio::select! {
            _ = connection.as_mut() => return,
            () = stopping(&mut stopped) => connection.as_mut().graceful_shutdown(),
        }
        let _ = connection.await;
    };
    match expiry {
        Some(expiry) => {
            let remaining = expiry.duration_since(SystemTime::now()).unwrap_or_default();
            let _ = tokio::time::timeout(remaining, serve).await;
        }
        None => serve.await,
    }
}

pub async fn serve(
    server: &ServerConfig,
    health: Health,
    readers: SqlitePool,
    bus: Arc<EventBus>,
    admin: local::Admin,
) -> std::io::Result<()> {
    let (config, metrics_config, git_config, run_dir) =
        (&server.http, &server.metrics, &server.git, &server.run_dir);
    let registry = Arc::new(Registry::default());
    let local = local::bind(
        &run_dir.join(local::SOCKET_NAME),
        health.local_serving.clone(),
    )?;
    let (git_uid, git_gid) = kajiba_core::local::named_user(&git_config.user)?;
    let gate_local = gate::bind(
        &run_dir.join(gate::SOCKET_NAME),
        health.gate_serving.clone(),
        git_uid,
        git_gid,
    )?;
    let gate = gate::Gate::new(&admin, registry.clone());
    let git_side = health.git_side.clone();
    let runner_app = runner_router(health.clone(), registry.clone());
    let runners = match &server.runner_listener {
        Some(listener) => {
            let source = svid::Source::load(svid::Files {
                trust_domain: listener.trust_domain.clone(),
                svid_file: listener.svid_file.clone(),
                key_file: listener.key_file.clone(),
                bundle_file: listener.bundle_file.clone(),
            })
            .map_err(|err| std::io::Error::other(format!("runner_listener: {err}")))?;
            let bound = tokio::net::TcpListener::bind(listener.listen).await?;
            Some((bound, source, runner_app))
        }
        None => None,
    };
    let app = router(
        health,
        config.reflection,
        registry.clone(),
        &config.public_url,
    )
    .map_err(std::io::Error::other)?;
    let listener = tokio::net::TcpListener::bind(config.listen).await?;
    let metrics = metrics::bind(metrics_config.listen)
        .await?
        .map(|listener| (listener, metrics::router(registry.clone(), readers, bus)));
    let recovery = admin.clone();
    tokio::select! {
        result = serve_listeners(listener, app, metrics, runners, (local, admin), (gate_local, gate), registry) => result,
        () = git_side.run(&git_config.socket, Some(&recovery)) => unreachable!("connection loop runs until shutdown"),
    }
}

async fn serve_listeners(
    listener: tokio::net::TcpListener,
    app: Router,
    metrics: Option<(tokio::net::TcpListener, Router)>,
    runners: Option<(tokio::net::TcpListener, Arc<svid::Source>, Router)>,
    (local, admin): (local::Local, local::Admin),
    (gate_local, gate): (local::Local, gate::Gate),
    registry: Arc<Registry>,
) -> std::io::Result<()> {
    let shutdown = shutdown_signal()?;
    let (stop, stopped) = watch::channel(false);
    tracing::info!(event = "listening", addr = %listener.local_addr()?);
    if let Some((listener, _)) = &metrics {
        tracing::info!(event = "metrics-listening", addr = %listener.local_addr()?);
    }
    tracing::info!(event = "ready");
    let http_stopped = stopped.clone();
    let metrics_stopped = stopped.clone();
    let runners_stopped = stopped.clone();
    let mut gate_stopped = stopped.clone();
    let mut local_stopped = stopped;
    let servers = async move {
        tokio::try_join!(
            gate::serve(gate_local, gate, registry.clone(), async move {
                let _ = gate_stopped.changed().await;
            }),
            local::serve(local, admin, registry, async move {
                let _ = local_stopped.changed().await;
            }),
            serve_tcp(listener, app, PUBLIC, http_stopped),
            async move {
                if let Some((listener, app)) = metrics {
                    serve_tcp(listener, app, PUBLIC, metrics_stopped).await?;
                }
                Ok(())
            },
            async move {
                if let Some((listener, source, app)) = runners {
                    serve_runners(listener, source, app, runners_stopped).await?;
                }
                Ok(())
            }
        )?;
        Ok(())
    };
    tokio::pin!(servers);
    tokio::select! {
        result = &mut servers => result,
        () = shutdown => {
            stop.send_replace(true);
            servers.await
        }
    }
}

fn shutdown_signal() -> std::io::Result<impl std::future::Future<Output = ()>> {
    use tokio::signal::unix::{SignalKind, signal};
    let mut terminate = signal(SignalKind::terminate())?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    Ok(async move {
        tokio::select! {
            _ = terminate.recv() => {},
            _ = interrupt.recv() => {},
        }
        tracing::info!(event = "stopping");
    })
}

pub mod auth;
pub mod correlation;
pub mod cursor;
mod gate;
pub mod git_http;
pub(crate) mod git_side;
pub mod local;
pub mod raw;
pub mod repos;
pub mod runners;
mod runs;
mod slots;
mod stream;
mod tokens;
mod ui;
mod web;
pub mod webauthn;

#[cfg(test)]
mod tests;
