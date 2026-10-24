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
use kajiba_core::config::{HttpConfig, MetricsConfig};
use kajiba_proto::v1::{
    CheckRequest, CheckResponse,
    check_response::ServingStatus,
    health_service_server::{HealthService, HealthServiceServer},
};
use sqlx::SqlitePool;
use std::{
    io,
    path::PathBuf,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Semaphore, watch};
use tonic::{Request, Response, Status, service::Routes};

#[derive(Clone)]
pub struct Health {
    database: Arc<OnceLock<SqlitePool>>,
    state_dir: PathBuf,
    local_serving: Arc<AtomicBool>,
}

impl Health {
    pub fn new(state_dir: PathBuf) -> Self {
        Self {
            database: Arc::default(),
            state_dir,
            local_serving: Arc::default(),
        }
    }

    // Called only after migrations, reconciliation and event bus initialization.
    pub fn started(&self, database: SqlitePool) {
        assert!(
            self.database.set(database).is_ok(),
            "startup completed twice"
        );
    }

    async fn ready(&self) -> bool {
        let Some(database) = self.database.get() else {
            return false;
        };
        self.local_serving.load(Ordering::Acquire)
            && ["repos", "db", "ci/logs", "ci/artifacts", "tmp"]
                .iter()
                .all(|name| self.state_dir.join(name).is_dir())
            && sqlx::query_scalar::<_, i64>("SELECT 1")
                .fetch_one(database)
                .await
                .is_ok()
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
) -> Result<Router, tonic_reflection::server::Error> {
    let mut app = Routes::new(HealthServiceServer::new(health.clone()))
        .into_axum_router()
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
            .build_v1()?;
        app = Routes::from(app).add_service(reflection).into_axum_router();
    }
    Ok(app
        .fallback(|request: HttpRequest| async move {
            refusal(&request, StatusCode::NOT_FOUND, tonic::Code::Unimplemented)
        })
        .layer(middleware::from_fn_with_state(
            registry,
            correlation::request,
        )))
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

/// Accept errors other than a peer giving up (EMFILE, ENFILE, ENOBUFS) persist:
/// back off instead of spinning on a listener that keeps failing.
fn accept_backoff(err: &io::Error) -> Option<Duration> {
    use io::ErrorKind::{ConnectionAborted, ConnectionRefused, ConnectionReset};
    if matches!(
        err.kind(),
        ConnectionAborted | ConnectionRefused | ConnectionReset
    ) {
        return None;
    }
    tracing::warn!(event = "accept-failed", err = %err);
    Some(Duration::from_secs(1))
}

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
    config: &HttpConfig,
    metrics_config: &MetricsConfig,
    health: Health,
    readers: SqlitePool,
    bus: Arc<EventBus>,
    admin: local::Admin,
) -> std::io::Result<()> {
    let registry = Arc::new(Registry::default());
    let local = local::Local::bind(
        std::path::Path::new(local::SOCKET_PATH),
        health.local_serving.clone(),
    )?;
    let app = router(health, config.reflection, registry.clone()).map_err(std::io::Error::other)?;
    let listener = tokio::net::TcpListener::bind(config.listen).await?;
    let metrics = metrics::bind(metrics_config.listen)
        .await?
        .map(|listener| (listener, metrics::router(registry.clone(), readers, bus)));
    serve_listeners(listener, app, metrics, (local, admin), registry).await
}

async fn serve_listeners(
    listener: tokio::net::TcpListener,
    app: Router,
    metrics: Option<(tokio::net::TcpListener, Router)>,
    (local, admin): (local::Local, local::Admin),
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
    let mut local_stopped = stopped;
    let servers = async move {
        tokio::try_join!(
            local.serve(admin, registry, async move {
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

mod correlation;
pub mod local;

#[cfg(test)]
mod tests;
