use crate::{
    db::events::EventBus,
    metrics::{self, Registry},
};
use axum::{Router, http::StatusCode, middleware, routing::get};
use kajiba_core::config::{HttpConfig, MetricsConfig};
use kajiba_proto::v1::{
    CheckRequest, CheckResponse,
    check_response::ServingStatus,
    health_service_server::{HealthService, HealthServiceServer},
};
use sqlx::SqlitePool;
use std::{
    path::PathBuf,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};
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
    let mut grpc = Routes::new(HealthServiceServer::new(health.clone()));
    if reflection {
        let reflection = tonic_reflection::server::Builder::configure()
            .register_encoded_file_descriptor_set(kajiba_proto::FILE_DESCRIPTOR_SET)
            .build_v1()?;
        grpc = grpc.add_service(reflection);
        let reflection = tonic_reflection::server::Builder::configure()
            .register_encoded_file_descriptor_set(kajiba_proto::FILE_DESCRIPTOR_SET)
            .build_v1alpha()?;
        grpc = grpc.add_service(reflection);
    }
    Ok(grpc
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
        .layer(middleware::from_fn_with_state(
            registry,
            correlation::request,
        )))
}

pub async fn serve(
    config: &HttpConfig,
    metrics_config: &MetricsConfig,
    health: Health,
    readers: SqlitePool,
    bus: Arc<EventBus>,
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
    serve_listeners(listener, app, metrics, local, registry).await
}

async fn serve_listeners(
    listener: tokio::net::TcpListener,
    app: Router,
    metrics: Option<(tokio::net::TcpListener, Router)>,
    local: local::Local,
    registry: Arc<Registry>,
) -> std::io::Result<()> {
    use std::future::IntoFuture;
    let shutdown = shutdown_signal()?;
    let (stop, stopped) = tokio::sync::watch::channel(false);
    tracing::info!(event = "listening", addr = %listener.local_addr()?);
    if let Some((listener, _)) = &metrics {
        tracing::info!(event = "metrics-listening", addr = %listener.local_addr()?);
    }
    tracing::info!(event = "ready");
    let mut http_stopped = stopped.clone();
    let mut metrics_stopped = stopped.clone();
    let mut local_stopped = stopped;
    let servers = async move {
        tokio::try_join!(
            local.serve(registry, async move {
                let _ = local_stopped.changed().await;
            }),
            axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = http_stopped.changed().await;
                })
                .into_future(),
            async move {
                if let Some((listener, app)) = metrics {
                    axum::serve(listener, app)
                        .with_graceful_shutdown(async move {
                            let _ = metrics_stopped.changed().await;
                        })
                        .await?;
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
