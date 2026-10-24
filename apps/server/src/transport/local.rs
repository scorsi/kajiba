use super::correlation;
use crate::metrics::Registry;
use axum::middleware;
use hyper_util::rt::TokioIo;
use kajiba_proto::admin::v1::{
    EnrollRequest, EnrollResponse, ImportRequest, ImportResponse, ListReposRequest,
    ListReposResponse, Repo,
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
        Arc, OnceLock,
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

/// Readiness of the local listener: serving, and its path still names the socket it bound.
#[derive(Default)]
pub struct Serving {
    pub(super) on: AtomicBool,
    socket: OnceLock<(PathBuf, (u64, u64))>,
}

impl Serving {
    pub(super) fn ready(&self) -> bool {
        self.on.load(Ordering::Acquire)
            && self.socket.get().is_none_or(|(path, identity)| {
                fs::symlink_metadata(path).is_ok_and(|metadata| {
                    metadata.file_type().is_socket()
                        && (metadata.dev(), metadata.ino()) == *identity
                })
            })
    }
}

pub struct Admin {
    pub writer: crate::db::Writer,
    pub public_url: String,
    pub enrollment_ttl: Duration,
    pub gate: Arc<crate::db::pushes::Gate>,
    pub bus: Arc<crate::db::events::EventBus>,
    pub state_dir: PathBuf,
}

pub(super) fn refused(code: tonic::Code, reason: &str, message: String) -> Status {
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
        // Validated before taking the writer; a rejected name is never echoed (ADR 0022).
        let user =
            kajiba_core::names::validate_name(&request.into_inner().user).map_err(|err| {
                let mut details = tonic_types::ErrorDetails::new();
                details.set_error_info("INVALID_FIELD", "kajiba", std::collections::HashMap::new());
                details.add_bad_request_violation("user", err.to_string());
                <Status as tonic_types::StatusExt>::with_error_details(
                    tonic::Code::InvalidArgument,
                    format!("invalid user name: {err}"),
                    details,
                )
            })?;
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
            Error::Expiry => Status::internal("enrollment: expiry out of range"),
            Error::Database(err) => Status::internal(format!("enrollment: {err}")),
        })?;
        // ADR 0022: never log the link or its token.
        tracing::info!(event = "enrollment-issued", user);
        Ok(Response::new(EnrollResponse {
            // The fragment keeps the token out of proxy and request logs; J4's page reads it.
            url: format!(
                "{}/auth/enroll#{token}",
                self.public_url.trim_end_matches('/')
            ),
        }))
    }

    async fn list_repos(
        &self,
        request: Request<ListReposRequest>,
    ) -> Result<Response<ListReposResponse>, Status> {
        use super::cursor;
        const METHOD: &str = "admin.ListRepos";
        let request = request.into_inner();
        let size = cursor::page_size(request.page_size)?;
        let (key, id) = cursor::decode(METHOD, &request.page_token)?.unwrap_or_default();
        let mut rows: Vec<(i64, String, String, String, bool)> = sqlx::query_as(
            "SELECT repo.id, repo.public_id, owner.name, repo.name, repo.archived_at IS NOT NULL
             FROM repo JOIN user owner ON owner.id = repo.owner_id
             WHERE (owner.name || '/' || repo.name, repo.id) > (?1, ?2)
             ORDER BY owner.name || '/' || repo.name, repo.id LIMIT ?3",
        )
        .bind(key)
        .bind(id)
        .bind(size + 1)
        .fetch_all(&mut *self.writer.lock().await)
        .await
        .map_err(|err| Status::internal(format!("list repositories: {err}")))?;
        let more = rows.len() as i64 > size;
        rows.truncate(size as usize);
        let next_page_token = match rows.last() {
            Some((id, _, owner, name, _)) if more => {
                cursor::encode(METHOD, &format!("{owner}/{name}"), *id)
            }
            _ => String::new(),
        };
        Ok(Response::new(ListReposResponse {
            repos: rows
                .into_iter()
                .map(|(_, public_id, owner, name, archived)| Repo {
                    public_id,
                    owner,
                    name,
                    archived,
                })
                .collect(),
            next_page_token,
        }))
    }

    async fn import(
        &self,
        request: Request<ImportRequest>,
    ) -> Result<Response<ImportResponse>, Status> {
        use crate::import::Error;
        let correlation_id = request
            .extensions()
            .get::<correlation::RequestId>()
            .map(|id| id.0.clone())
            .unwrap_or_default();
        let ImportRequest {
            repo,
            source,
            credentials,
        } = request.into_inner();
        let refs = crate::import::import(
            &self.writer,
            &self.gate,
            &self.bus,
            &self.state_dir,
            &repo,
            &source,
            &credentials,
            &correlation_id,
        )
        .await
        .map_err(|err| match err {
            // The repository argument is never echoed (ADR 0022); the CLI shows it.
            Error::NotFound => refused(
                tonic::Code::NotFound,
                "REPO_NOT_FOUND",
                "no declared repository by this name".into(),
            ),
            Error::NotEmpty(message) => {
                refused(tonic::Code::FailedPrecondition, "INVALID_STATE", message)
            }
            Error::NoDefaultBranch(head) => refused(
                tonic::Code::FailedPrecondition,
                "INVALID_STATE",
                format!("the source has no {head}"),
            ),
            Error::Source(message) => refused(
                tonic::Code::FailedPrecondition,
                "INVALID_STATE",
                format!("the source cannot be read: {message}"),
            ),
            Error::Internal(message) => Status::internal(format!("import: {message}")),
        })?;
        Ok(Response::new(ImportResponse { refs: refs as u32 }))
    }
}

pub(super) struct Local {
    listener: UnixListener,
    path: PathBuf,
    identity: (u64, u64),
    allowed_uids: Vec<u32>,
    serving: Arc<Serving>,
    backoff: Option<Pin<Box<tokio::time::Sleep>>>,
}

impl Local {
    pub(super) fn bind(path: &Path, serving: Arc<Serving>) -> io::Result<Self> {
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
        let _ = serving
            .socket
            .set((path.into(), (metadata.dev(), metadata.ino())));
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
        // An enroll request is a user name: a few bytes, never tonic's 4 MiB default.
        let routes = Routes::new(AdminServiceServer::new(admin).max_decoding_message_size(4096))
            .into_axum_router()
            .layer(middleware::from_fn_with_state(
                registry,
                correlation::request,
            ));
        self.serving.on.store(true, Ordering::Release);
        let serving = self.serving.clone();
        tonic::transport::Server::builder()
            .add_routes(routes.into())
            .serve_with_incoming_shutdown(self, async move {
                shutdown.await;
                serving.on.store(false, Ordering::Release);
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
        self.serving.on.store(false, Ordering::Release);
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

async fn client(
    path: &Path,
    timeout: Option<Duration>,
) -> Result<AdminServiceClient<tonic::transport::Channel>, Box<dyn std::error::Error>> {
    let mut endpoint = tonic::transport::Endpoint::from_static("http://localhost")
        .connect_timeout(Duration::from_secs(5));
    if let Some(timeout) = timeout {
        endpoint = endpoint.timeout(timeout);
    }
    let channel = endpoint
        .connect_with_connector(Connector(path.into()))
        .await?;
    Ok(AdminServiceClient::new(channel))
}

pub async fn enroll(path: &Path, user: String) -> Result<String, Box<dyn std::error::Error>> {
    Ok(client(path, Some(Duration::from_secs(5)))
        .await?
        .enroll(EnrollRequest { user })
        .await?
        .into_inner()
        .url)
}

/// No request timeout: fetching a large repository takes as long as it takes.
pub async fn import(
    path: &Path,
    repo: String,
    source: String,
    credentials: String,
) -> Result<u32, Box<dyn std::error::Error>> {
    Ok(client(path, None)
        .await?
        .import(ImportRequest {
            repo,
            source,
            credentials,
        })
        .await?
        .into_inner()
        .refs)
}

/// Every page, in name order.
pub async fn list_repos(path: &Path) -> Result<Vec<Repo>, Box<dyn std::error::Error>> {
    let mut client = client(path, Some(Duration::from_secs(5))).await?;
    let mut repos = Vec::new();
    let mut page_token = String::new();
    loop {
        let page = client
            .list_repos(ListReposRequest {
                page_size: 200,
                page_token,
            })
            .await?
            .into_inner();
        repos.extend(page.repos);
        if page.next_page_token.is_empty() {
            return Ok(repos);
        }
        page_token = page.next_page_token;
    }
}

#[cfg(test)]
pub(super) mod tests;
