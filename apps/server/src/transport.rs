use axum::{Router, http::StatusCode, routing::get};
use kajiba_core::config::HttpConfig;
use kajiba_proto::v1::{
    CheckRequest, CheckResponse,
    check_response::ServingStatus,
    health_service_server::{HealthService, HealthServiceServer},
};
use sqlx::SqlitePool;
use std::{
    path::PathBuf,
    sync::{Arc, OnceLock},
};
use tonic::{Request, Response, Status, service::Routes};

#[derive(Clone)]
pub struct Health {
    database: Arc<OnceLock<SqlitePool>>,
    state_dir: PathBuf,
}

impl Health {
    pub fn new(state_dir: PathBuf) -> Self {
        Self {
            database: Arc::default(),
            state_dir,
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
        ["repos", "db", "ci/logs", "ci/artifacts", "tmp"]
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

fn router(health: Health, reflection: bool) -> Result<Router, tonic_reflection::server::Error> {
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
        ))
}

pub async fn serve(config: &HttpConfig, health: Health) -> std::io::Result<()> {
    let app = router(health, config.reflection).map_err(std::io::Error::other)?;
    let listener = tokio::net::TcpListener::bind(config.listen).await?;
    tracing::info!(event = "listening", addr = %listener.local_addr()?);
    tracing::info!(event = "ready");
    axum::serve(listener, app).await
}

#[cfg(test)]
mod tests;
