//! The single authentication layer of the `kajiba.v1` routes (ADR 0014, 0020): resolves
//! a bearer PAT or the session cookie, checks the method's declared scope, and puts the
//! caller in the request extensions. Smart HTTP, `/raw/`, probes and reflection never
//! reach it; the local sockets keep their peer-credential checks.
// These feed `?` in the layer, which returns `Status` by value anyway.
#![allow(clippy::result_large_err)]
use super::{Health, local::refused};
use crate::{db::credentials::Found, metrics::Registry};
use axum::{
    body::Body,
    extract::{Request, State},
    http::HeaderMap,
    middleware::Next,
    response::Response,
};
use kajiba_core::version::semver;
use kajiba_proto::{
    Access,
    v1::{
        GetViewerRequest, GetViewerResponse, LogoutRequest, LogoutResponse, Scope, SshKey,
        SshKeyUsage, auth_service_server::AuthService,
    },
};
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use tonic::Status;

/// The session cookie (ADR 0079).
pub const SESSION_COOKIE: &str = "kajiba_session";
/// The `Set-Cookie` value of a session cookie living `max_age` (ADR 0014, 0020); a zero
/// `max_age` clears it.
pub fn set_cookie(value: &str, max_age: Duration) -> String {
    format!(
        "{SESSION_COOKIE}={value}; Path=/; Max-Age={}; HttpOnly; Secure; SameSite=Strict",
        max_age.as_secs()
    )
}

/// The client version a CLI sends on every call (ADR 0080).
pub const CLIENT_VERSION: &str = "kajiba-client-version";
/// The server version on every `kajiba.v1` response, for the CLI's warning (ADR 0090).
pub const SERVER_VERSION: &str = "kajiba-server-version";

/// ADR 0023's server ↔ CLI row: before 1.0 the same `0.minor`, from 1.0 the same major
/// (the CLI warns about another minor); patch may differ. A malformed version is refused.
pub(super) fn compatible(server: &str, client: &str) -> bool {
    match (semver(server), semver(client)) {
        (Some((0, server, _)), Some((0, client, _))) => server == client,
        (Some((server, ..)), Some((client, ..))) => server != 0 && server == client,
        _ => false,
    }
}

/// A call without the header (browser, grpcurl) passes (ADR 0080).
fn version_check(headers: &HeaderMap, server: &str) -> Result<(), Status> {
    let Some(value) = headers.get(CLIENT_VERSION) else {
        return Ok(());
    };
    let client = String::from_utf8_lossy(value.as_bytes());
    if compatible(server, &client) {
        return Ok(());
    }
    Err(refused(
        tonic::Code::FailedPrecondition,
        "VERSION_INCOMPATIBLE",
        format!("client version {client} is incompatible with server version {server}"),
    ))
}

/// The authenticated caller of a scoped method, in its request extensions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Principal {
    pub user_id: i64,
    pub scopes: Vec<Scope>,
    /// Session token hash, absent for PATs; only a session manages PATs (ADR 0081).
    pub session: Option<[u8; 32]>,
}

#[derive(Clone)]
pub(super) struct Auth {
    health: Health,
    registry: Arc<Registry>,
    access: Arc<BTreeMap<String, Access>>,
}

impl Auth {
    pub(super) fn new(health: Health, registry: Arc<Registry>) -> Result<Self, String> {
        Ok(Self {
            health,
            registry,
            access: Arc::new(kajiba_proto::method_access()?),
        })
    }
}

/// Why a credential was refused: the ADR 0020 reason, also the metrics label.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Refusal {
    Invalid,
    Expired,
    ScopeMissing,
}

impl Refusal {
    fn reason(self) -> &'static str {
        match self {
            Self::Invalid => "CREDENTIAL_INVALID",
            Self::Expired => "CREDENTIAL_EXPIRED",
            Self::ScopeMissing => "SCOPE_MISSING",
        }
    }

    pub(super) fn status(self) -> Status {
        let (code, message) = match self {
            Self::Invalid => (
                tonic::Code::Unauthenticated,
                "missing or invalid credential",
            ),
            Self::Expired => (tonic::Code::Unauthenticated, "credential expired"),
            Self::ScopeMissing => (
                tonic::Code::PermissionDenied,
                "the credential lacks this method's scope",
            ),
        };
        refused(code, self.reason(), message.into())
    }
}

/// `repo:read` for `SCOPE_REPO_READ`: the configuration and `api_token.scopes` names.
pub(super) fn scope_name(scope: Scope) -> String {
    scope
        .as_str_name()
        .trim_start_matches("SCOPE_")
        .to_lowercase()
        .replace('_', ":")
}

enum Presented<'a> {
    Token(&'a str),
    Session(&'a str),
}

/// The bearer token wins over the cookie; a malformed authorization header never falls
/// back to the cookie.
fn presented(headers: &HeaderMap) -> Option<Presented<'_>> {
    if let Some(value) = headers.get(axum::http::header::AUTHORIZATION) {
        let token = value.to_str().ok()?.strip_prefix("Bearer ")?;
        return token.starts_with("kjb_").then_some(Presented::Token(token));
    }
    cookie(headers).map(Presented::Session)
}

/// The session cookie's value, whatever the authorization header.
fn cookie(headers: &HeaderMap) -> Option<&str> {
    headers
        .get_all(axum::http::header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .find_map(|pair| pair.trim().strip_prefix(SESSION_COOKIE)?.strip_prefix('='))
}

/// The valid session of a request's cookie, resolved and admitted as the interceptor does:
/// for the ceremony routes that act for a signed-in user (ADR 0143).
pub(super) async fn session(
    connection: impl sqlx::SqliteExecutor<'_>,
    headers: &HeaderMap,
    now: i64,
) -> Result<Option<Found>, sqlx::Error> {
    let Some(value) = cookie(headers) else {
        return Ok(None);
    };
    let found =
        crate::db::sessions::find(connection, &kajiba_core::sha256(value.as_bytes())).await?;
    Ok(admit(found, now).ok())
}

/// Unknown, revoked or disabled-user credentials are invalid; a credential is expired
/// from its `expires_at` instant on. `now` is UTC epoch milliseconds.
pub(super) fn admit(found: Option<Found>, now: i64) -> Result<Found, Refusal> {
    let found = found.ok_or(Refusal::Invalid)?;
    if found.revoked || found.disabled {
        return Err(Refusal::Invalid);
    }
    if found.expires_at.is_some_and(|expires| expires <= now) {
        return Err(Refusal::Expired);
    }
    Ok(found)
}

/// The scopes a credential holds: a session (`None`) every scope, a token those it lists.
pub(super) fn scopes(listed: Option<&str>) -> Vec<Scope> {
    (1..)
        .map_while(|value| Scope::try_from(value).ok())
        .filter(|scope| {
            listed.is_none_or(|listed| {
                listed
                    .split_whitespace()
                    .any(|name| name == scope_name(*scope))
            })
        })
        .collect()
}

pub(super) fn now() -> Result<i64, Status> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .map_err(|err| Status::internal(format!("clock: {err}")))
}

impl Auth {
    async fn principal(
        &self,
        headers: &HeaderMap,
        scope: Scope,
        path: &str,
    ) -> Result<Principal, Status> {
        let internal = |err: sqlx::Error| Status::internal(format!("credentials: {err}"));
        let (Some(readers), Some(writer)) = (self.health.database.get(), self.health.writer.get())
        else {
            return Err(Status::unavailable("starting"));
        };
        let now = now()?;
        let refuse = |refusal: Refusal| {
            let reason = refusal.reason();
            // ADR 0022: the reason only, never the credential.
            tracing::warn!(event = "auth-refused", reason);
            self.registry.auth_refused(reason);
            refusal.status()
        };
        let (found, session) = match presented(headers) {
            None => return Err(refuse(Refusal::Invalid)),
            Some(Presented::Token(token)) => (
                crate::db::credentials::find_token(readers, &kajiba_core::sha256(token.as_bytes()))
                    .await
                    .map_err(internal)?,
                None,
            ),
            Some(Presented::Session(value)) => {
                let hash = kajiba_core::sha256(value.as_bytes());
                (
                    crate::db::sessions::find(readers, &hash)
                        .await
                        .map_err(internal)?,
                    Some(hash),
                )
            }
        };
        let found = admit(found, now).map_err(refuse)?;
        // ADR 0081: a valid PAT is refused regardless of its scopes.
        if session.is_none()
            && (path.starts_with("/kajiba.v1.TokenService/")
                || path.starts_with("/kajiba.v1.PasskeyService/"))
        {
            return Err(refused(
                tonic::Code::PermissionDenied,
                "SESSION_REQUIRED",
                "personal access tokens are managed from a browser session".into(),
            ));
        }
        let scopes = scopes(found.scopes.as_deref());
        if !scopes.contains(&scope) {
            return Err(refuse(Refusal::ScopeMissing));
        }
        // ADR 0078: at most one usage write per minute and credential, through the writer.
        if found.last_used_at.is_none_or(|used| now - used >= 60_000) {
            if let Ok(_admitted) = self.health.maintenance.admit() {
                let mut writer = writer.lock().await;
                if session.is_some() {
                    crate::db::sessions::seen(&mut writer, found.id, now).await
                } else {
                    crate::db::credentials::token_used(&mut writer, found.id, now).await
                }
                .map_err(internal)?;
            }
        }
        Ok(Principal {
            user_id: found.user_id,
            scopes,
            session,
        })
    }
}

pub(super) async fn authenticate(
    State(auth): State<Auth>,
    request: Request,
    next: Next,
) -> Response {
    let mut response = guard(auth, request, next).await;
    response.headers_mut().insert(
        SERVER_VERSION,
        axum::http::HeaderValue::from_static(env!("CARGO_PKG_VERSION")),
    );
    response
}

async fn guard(auth: Auth, mut request: Request, next: Next) -> Response {
    // Before credential resolution: an incompatible client learns why, whatever its token.
    if let Err(status) = version_check(request.headers(), env!("CARGO_PKG_VERSION")) {
        return status.into_http().map(Body::new);
    }
    // Public methods pass; an unknown method reaches tonic, which answers UNIMPLEMENTED.
    let Some(&Access::Scope(scope)) = auth.access.get(request.uri().path()) else {
        return next.run(request).await;
    };
    match auth
        .principal(request.headers(), scope, request.uri().path())
        .await
    {
        Ok(principal) => {
            request.extensions_mut().insert(principal);
            next.run(request).await
        }
        Err(status) => status.into_http().map(Body::new),
    }
}

/// `AuthService` (ADR 0079).
pub(super) struct Sessions(pub(super) Health);

#[tonic::async_trait]
impl AuthService for Sessions {
    /// Public (proto): deletes the cookie's session, if any, and always clears the cookie.
    async fn logout(
        &self,
        request: tonic::Request<LogoutRequest>,
    ) -> Result<tonic::Response<LogoutResponse>, Status> {
        let _admitted = self.0.maintenance.admit()?;
        let headers = request.metadata().clone().into_headers();
        if let Some(value) = cookie(&headers) {
            let writer = self
                .0
                .writer
                .get()
                .ok_or_else(|| Status::unavailable("starting"))?;
            crate::db::sessions::delete(
                &mut *writer.lock().await,
                &kajiba_core::sha256(value.as_bytes()),
            )
            .await
            .map_err(|err| Status::internal(format!("sessions: {err}")))?;
        }
        let mut response = tonic::Response::new(LogoutResponse {});
        response.metadata_mut().insert(
            "set-cookie",
            set_cookie("", Duration::ZERO)
                .parse()
                .map_err(|_| Status::internal("cookie"))?,
        );
        Ok(response)
    }

    /// Scope `repo:read` (proto): the interceptor has authenticated the caller (ADR 0087).
    async fn get_viewer(
        &self,
        request: tonic::Request<GetViewerRequest>,
    ) -> Result<tonic::Response<GetViewerResponse>, Status> {
        let principal = request
            .extensions()
            .get::<Principal>()
            .ok_or_else(|| Status::internal("viewer: no authenticated caller"))?;
        let readers = self
            .0
            .database
            .get()
            .ok_or_else(|| Status::unavailable("starting"))?;
        let (user, is_admin): (String, bool) =
            sqlx::query_as("SELECT name, is_admin FROM user WHERE id = ?")
                .bind(principal.user_id)
                .fetch_one(readers)
                .await
                .map_err(|err| Status::internal(format!("viewer: {err}")))?;
        // Declared keys only, read-only (ADR 0143); `auth` sorts before `signing`.
        let keys: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT usage, key_type, fingerprint FROM ssh_key
             WHERE user_id = ? AND source = 'config' ORDER BY usage, fingerprint",
        )
        .bind(principal.user_id)
        .fetch_all(readers)
        .await
        .map_err(|err| Status::internal(format!("viewer: {err}")))?;
        Ok(tonic::Response::new(GetViewerResponse {
            user,
            // The `admin` scope never grants more than the user's real rights (ADR 0081).
            admin: is_admin && principal.scopes.contains(&Scope::Admin),
            keys: keys
                .into_iter()
                .map(|(usage, key_type, fingerprint)| SshKey {
                    usage: if usage == "signing" {
                        SshKeyUsage::Signing
                    } else {
                        SshKeyUsage::Auth
                    }
                    .into(),
                    key_type,
                    fingerprint,
                })
                .collect(),
        }))
    }
}

#[cfg(test)]
pub(super) mod tests;
