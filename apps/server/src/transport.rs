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
use kajiba_core::config::ServerConfig;
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
    time::{Duration, Instant},
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
    state_dir: PathBuf,
    git_state_dir: PathBuf,
    local_serving: Arc<local::Serving>,
    public_reads: Arc<Semaphore>,
    webauthn_requests: Arc<Semaphore>,
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
            git_state_dir,
            state_dir,
            local_serving: Arc::default(),
            public_reads: Arc::new(Semaphore::new(PUBLIC_READS)),
            webauthn_requests: Arc::new(Semaphore::new(webauthn::MAX_REQUESTS)),
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
            && ["db", "ci/logs", "ci/artifacts", "tmp"]
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
    let runner_service = runners::Runners::new(health.clone(), registry.clone());
    if health.runners.get().is_some() {
        tokio::spawn(runner_service.clone().schedule());
    }
    let api = Routes::new(HealthServiceServer::new(health.clone()))
        .add_service(AuthServiceServer::new(auth::Sessions(health.clone())))
        .add_service(TokenServiceServer::new(tokens::Tokens(health.clone())))
        .add_service(
            RepoServiceServer::new(repos::Repos(health.clone()))
                // Bound inbound work before handler admission (ADR 0057).
                .max_decoding_message_size(16 * 1024),
        )
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
        .route_layer(middleware::from_fn_with_state(
            REQUEST_TIMEOUT,
            request_timeout,
        ));
    if reflection {
        let reflection = tonic_reflection::server::Builder::configure()
            .register_encoded_file_descriptor_set(kajiba_proto::FILE_DESCRIPTOR_SET)
            .build_v1()
            .map_err(|err| err.to_string())?;
        app = Routes::from(app).add_service(reflection).into_axum_router();
    }
    // A runner's stream lasts as long as the runner: no `kajiba.v1` layer, no timeout (0105).
    app = Routes::from(app)
        .add_service(RunnerServiceServer::new(runner_service))
        .into_axum_router();
    Ok(app
        .fallback(|request: HttpRequest| async move { ui_response(ui_dir.get(), request).await })
        .layer(middleware::from_fn_with_state(
            registry,
            correlation::request,
        )))
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
/// Time to produce a response head on non-streaming routes, request body included.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
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
    State(limit): State<Duration>,
    request: HttpRequest,
    next: Next,
) -> HttpResponse {
    let refused = refusal(
        &request,
        StatusCode::REQUEST_TIMEOUT,
        tonic::Code::DeadlineExceeded,
    );
    tokio::time::timeout(limit, next.run(request))
        .await
        .unwrap_or(refused)
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
    mut stopped: watch::Receiver<bool>,
) -> io::Result<()> {
    use hyper_util::{
        rt::{TokioExecutor, TokioIo, TokioTimer},
        server::conn::auto::Builder,
        service::TowerToHyperService,
    };
    let mut builder = Builder::new(TokioExecutor::new());
    builder
        .http1()
        .timer(TokioTimer::new())
        .header_read_timeout(limits.header_read);
    builder
        .http2()
        .timer(TokioTimer::new())
        .keep_alive_interval(limits.header_read);
    // Each connection holds a permit; holding them all again means every connection ended.
    let permits = Arc::new(Semaphore::new(limits.connections));
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
        let service = TowerToHyperService::new(app.clone());
        let mut stopped = stopped.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let connection =
                connection.serve_connection_with_upgrades(TokioIo::new(stream), service);
            tokio::pin!(connection);
            tokio::select! {
                _ = connection.as_mut() => return,
                () = stopping(&mut stopped) => connection.as_mut().graceful_shutdown(),
            }
            let _ = connection.await;
        });
    }
    drop(listener);
    let _ = permits.acquire_many(limits.connections as u32).await;
    Ok(())
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
        result = serve_listeners(listener, app, metrics, (local, admin), (gate_local, gate), registry) => result,
        () = git_side.run(&git_config.socket, Some(&recovery)) => unreachable!("connection loop runs until shutdown"),
    }
}

async fn serve_listeners(
    listener: tokio::net::TcpListener,
    app: Router,
    metrics: Option<(tokio::net::TcpListener, Router)>,
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
mod stream;
mod tokens;
mod ui;
mod web;
pub mod webauthn;

#[cfg(test)]
mod tests;
