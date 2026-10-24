use super::correlation;
use crate::metrics::Registry;
use axum::middleware;
use hyper_util::rt::TokioIo;
use kajiba_proto::admin::v1::{
    EnrollRequest, EnrollResponse,
    admin_service_client::AdminServiceClient,
    admin_service_server::{AdminService, AdminServiceServer},
};
use std::{
    fs,
    future::Future,
    io,
    os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll, ready},
    time::Duration,
};
use tokio::net::{UnixListener, UnixStream};
use tonic::{
    Request, Response, Status,
    codegen::{Service, tokio_stream::Stream},
    service::Routes,
};

pub const SOCKET_PATH: &str = "/run/kajiba/local.sock";

pub struct Admin {
    pub writer: crate::db::Writer,
    pub public_url: String,
    pub enrollment_ttl: Duration,
}

fn refused(code: tonic::Code, reason: &str, message: String) -> Status {
    let mut details = tonic_types::ErrorDetails::new();
    details.set_error_info(reason, "kajiba", std::collections::HashMap::new());
    <Status as tonic_types::StatusExt>::with_error_details(code, message, details)
}

#[tonic::async_trait]
impl AdminService for Admin {
    async fn enroll(
        &self,
        request: Request<EnrollRequest>,
    ) -> Result<Response<EnrollResponse>, Status> {
        use crate::db::enrollment::{Error, issue};
        let user = request.into_inner().user;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|err| Status::internal(err.to_string()))?
            .as_millis() as i64;
        let token = issue(
            &mut *self.writer.lock().await,
            &user,
            self.enrollment_ttl,
            now,
        )
        .await
        .map_err(|err| match err {
            Error::UnknownUser => refused(
                tonic::Code::NotFound,
                "USER_NOT_FOUND",
                format!("unknown user {user}"),
            ),
            Error::Disabled => refused(
                tonic::Code::FailedPrecondition,
                "INVALID_STATE",
                format!("user {user} is disabled"),
            ),
            Error::Database(err) => Status::internal(format!("enrollment: {err}")),
        })?;
        // ADR 0022: never log the link or its token.
        tracing::info!(
            event = "enrollment-issued",
            user = user.to_ascii_lowercase()
        );
        Ok(Response::new(EnrollResponse {
            // The fragment keeps the token out of proxy and request logs; J4's page reads it.
            url: format!(
                "{}/auth/enroll#{token}",
                self.public_url.trim_end_matches('/')
            ),
        }))
    }
}

pub(super) struct Local {
    listener: UnixListener,
    path: PathBuf,
    identity: (u64, u64),
    allowed_uids: Vec<u32>,
    serving: Arc<AtomicBool>,
    backoff: Option<Pin<Box<tokio::time::Sleep>>>,
}

impl Local {
    pub(super) fn bind(path: &Path, serving: Arc<AtomicBool>) -> io::Result<Self> {
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_socket() => {
                match std::os::unix::net::UnixStream::connect(path) {
                    Err(err) if err.kind() == io::ErrorKind::ConnectionRefused => {
                        fs::remove_file(path)?
                    }
                    Err(err) => return Err(err),
                    Ok(_) => {
                        return Err(io::Error::new(
                            io::ErrorKind::AddrInUse,
                            "local socket is already serving",
                        ));
                    }
                }
            }
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "local socket path is not a socket",
                ));
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(err),
        }
        let listener = UnixListener::bind(path)?;
        let metadata = fs::symlink_metadata(path)?;
        let local = Self {
            listener,
            path: path.into(),
            identity: (metadata.dev(), metadata.ino()),
            // The service runs as kajiba; use its socket's UID without a user database lookup.
            allowed_uids: vec![0, metadata.uid()],
            serving,
            backoff: None,
        };
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        Ok(local)
    }

    fn allows_uid(&self, uid: u32) -> bool {
        self.allowed_uids.contains(&uid)
    }

    pub(super) async fn serve(
        self,
        admin: Admin,
        registry: Arc<Registry>,
        shutdown: impl std::future::Future<Output = ()>,
    ) -> io::Result<()> {
        let routes = Routes::new(AdminServiceServer::new(admin))
            .into_axum_router()
            .layer(middleware::from_fn_with_state(
                registry,
                correlation::request,
            ));
        self.serving.store(true, Ordering::Release);
        let serving = self.serving.clone();
        tonic::transport::Server::builder()
            .add_routes(routes.into())
            .serve_with_incoming_shutdown(self, async move {
                shutdown.await;
                serving.store(false, Ordering::Release);
            })
            .await
            .map_err(io::Error::other)
    }
}

impl Stream for Local {
    type Item = io::Result<UnixStream>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        loop {
            if let Some(backoff) = &mut this.backoff {
                ready!(backoff.as_mut().poll(cx));
                this.backoff = None;
            }
            // Never yield Err: tonic would re-poll a still-failing listener at once.
            let stream = match ready!(this.listener.poll_accept(cx)) {
                Ok((stream, _)) => stream,
                Err(err) => {
                    this.backoff = super::accept_backoff(&err)
                        .map(|delay| Box::pin(tokio::time::sleep(delay)));
                    continue;
                }
            };
            // Fail closed before any HTTP/2 or RPC processing, including credential lookup errors.
            if stream
                .peer_cred()
                .is_ok_and(|cred| this.allows_uid(cred.uid()))
            {
                return Poll::Ready(Some(Ok(stream)));
            }
        }
    }
}

impl Drop for Local {
    fn drop(&mut self) {
        self.serving.store(false, Ordering::Release);
        if fs::symlink_metadata(&self.path)
            .is_ok_and(|metadata| (metadata.dev(), metadata.ino()) == self.identity)
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[derive(Clone)]
struct Connector(PathBuf);

impl Service<axum::http::Uri> for Connector {
    type Response = TokioIo<UnixStream>;
    type Error = io::Error;
    type Future = Pin<Box<dyn std::future::Future<Output = io::Result<Self::Response>> + Send>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, _: axum::http::Uri) -> Self::Future {
        let path = self.0.clone();
        Box::pin(async move { UnixStream::connect(path).await.map(TokioIo::new) })
    }
}

pub async fn enroll(path: &Path, user: String) -> Result<String, Box<dyn std::error::Error>> {
    let channel = tonic::transport::Endpoint::from_static("http://localhost")
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(5))
        .connect_with_connector(Connector(path.into()))
        .await?;
    Ok(AdminServiceClient::new(channel)
        .enroll(EnrollRequest { user })
        .await?
        .into_inner()
        .url)
}

#[cfg(test)]
pub(super) mod tests;
