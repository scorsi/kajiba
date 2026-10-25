use super::correlation;
use crate::metrics::Registry;
use axum::middleware;
pub(super) use kajiba_core::local::Local;
pub use kajiba_core::local::Serving;
use kajiba_core::local::channel;
use kajiba_proto::admin::v1::{
    EnrollRequest, EnrollResponse, ImportRequest, ImportResponse, ListReposRequest,
    ListReposResponse, Repo,
    admin_service_client::AdminServiceClient,
    admin_service_server::{AdminService, AdminServiceServer},
};
use kajiba_proto::hook::v1::{
    GetAuthKeyRequest, GetAuthKeyResponse, PostReceiveRequest, PostReceiveResponse,
    PreReceiveRequest, PreReceiveResponse, Refusal,
    hook_service_client::HookServiceClient,
    hook_service_server::{HookService, HookServiceServer},
};
use std::{
    io,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tonic::{Request, Response, Status, service::Routes};

pub const SOCKET_PATH: &str = "/run/kajiba/local.sock";

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

/// The SSH integration and the hooks share the admin service's state (ADR 0003).
#[tonic::async_trait]
impl HookService for Admin {
    async fn get_auth_key(
        &self,
        request: Request<GetAuthKeyRequest>,
    ) -> Result<Response<GetAuthKeyResponse>, Status> {
        let found: Option<(String, String)> = sqlx::query_as(
            "SELECT user.public_id, ssh_key.public_key FROM ssh_key
             JOIN user ON user.id = ssh_key.user_id
             WHERE ssh_key.fingerprint = ? AND ssh_key.usage = 'auth' AND user.disabled_at IS NULL",
        )
        .bind(request.into_inner().fingerprint)
        .fetch_optional(&mut *self.writer.lock().await)
        .await
        .map_err(|err| Status::internal(format!("auth key lookup: {err}")))?;
        let (user_public_id, public_key) = found.ok_or_else(|| {
            refused(
                tonic::Code::NotFound,
                "KEY_NOT_FOUND",
                "no active auth key".into(),
            )
        })?;
        Ok(Response::new(GetAuthKeyResponse {
            user_public_id,
            public_key,
        }))
    }

    async fn pre_receive(
        &self,
        _: Request<PreReceiveRequest>,
    ) -> Result<Response<PreReceiveResponse>, Status> {
        // Refused until the ref, signature and admission checks are wired (J3 task 12).
        Ok(Response::new(PreReceiveResponse {
            allowed: false,
            refusals: vec![Refusal {
                ref_name: String::new(),
                reason: "pushes are not accepted yet".into(),
            }],
        }))
    }

    async fn post_receive(
        &self,
        _: Request<PostReceiveRequest>,
    ) -> Result<Response<PostReceiveResponse>, Status> {
        // No push is admitted yet, so nothing waits for reconciliation (J3 task 13).
        Ok(Response::new(PostReceiveResponse {}))
    }
}

pub(super) fn bind(path: &Path, serving: Arc<Serving>) -> io::Result<Local> {
    Local::bind(
        path,
        serving,
        0o600,
        vec![0, kajiba_core::local::effective_uid()],
    )
}

pub(super) async fn serve(
    local: Local,
    admin: Admin,
    registry: Arc<Registry>,
    shutdown: impl std::future::Future<Output = ()>,
) -> io::Result<()> {
    let admin = Arc::new(admin);
    // An enroll request is a user name: a few bytes, never tonic's 4 MiB default.
    let routes =
        Routes::new(AdminServiceServer::from_arc(admin.clone()).max_decoding_message_size(4096))
            .add_service(HookServiceServer::from_arc(admin))
            .into_axum_router()
            .layer(middleware::from_fn_with_state(
                registry,
                correlation::request,
            ));
    local.serve(routes.into(), shutdown).await
}

async fn client(
    path: &Path,
    timeout: Option<Duration>,
) -> Result<AdminServiceClient<tonic::transport::Channel>, Box<dyn std::error::Error>> {
    Ok(AdminServiceClient::new(channel(path, timeout).await?))
}

/// The user public id and canonical key of an active user's auth key; `None` when the
/// fingerprint names no such key.
pub async fn auth_key(
    path: &Path,
    fingerprint: String,
) -> Result<Option<(String, String)>, Box<dyn std::error::Error>> {
    let mut client = HookServiceClient::new(channel(path, Some(Duration::from_secs(5))).await?);
    match client.get_auth_key(GetAuthKeyRequest { fingerprint }).await {
        Ok(response) => {
            let response = response.into_inner();
            Ok(Some((response.user_public_id, response.public_key)))
        }
        Err(status) if status.code() == tonic::Code::NotFound => Ok(None),
        Err(status) => Err(status.into()),
    }
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

/// One pre-receive check; no timeout: checking a large push takes as long as it takes.
pub async fn pre_receive(
    path: &Path,
    request: PreReceiveRequest,
) -> Result<PreReceiveResponse, Box<dyn std::error::Error>> {
    let mut client = HookServiceClient::new(channel(path, None).await?);
    Ok(client.pre_receive(request).await?.into_inner())
}

pub async fn post_receive(
    path: &Path,
    request: PostReceiveRequest,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut client = HookServiceClient::new(channel(path, Some(Duration::from_secs(5))).await?);
    client.post_receive(request).await?;
    Ok(())
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
