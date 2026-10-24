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
    fs, io,
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

#[derive(Default)]
struct Admin;

#[tonic::async_trait]
impl AdminService for Admin {
    async fn enroll(&self, _: Request<EnrollRequest>) -> Result<Response<EnrollResponse>, Status> {
        Err(Status::unimplemented("admin enroll: not implemented yet"))
    }
}

pub(super) struct Local {
    listener: UnixListener,
    path: PathBuf,
    identity: (u64, u64),
    allowed_uids: Vec<u32>,
    serving: Arc<AtomicBool>,
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
        };
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        Ok(local)
    }

    fn allows_uid(&self, uid: u32) -> bool {
        self.allowed_uids.contains(&uid)
    }

    pub(super) async fn serve(
        self,
        registry: Arc<Registry>,
        shutdown: impl std::future::Future<Output = ()>,
    ) -> io::Result<()> {
        let routes = Routes::new(AdminServiceServer::new(Admin))
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
        loop {
            let (stream, _) = ready!(self.listener.poll_accept(cx))?;
            // Fail closed before any HTTP/2 or RPC processing, including credential lookup errors.
            if stream
                .peer_cred()
                .is_ok_and(|cred| self.allows_uid(cred.uid()))
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
mod tests;
