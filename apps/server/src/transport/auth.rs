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
use kajiba_proto::{Access, v1::Scope};
use std::{collections::BTreeMap, sync::Arc};
use tonic::Status;

/// The session cookie (ADR 0079).
pub const SESSION_COOKIE: &str = "kajiba_session";

/// The authenticated caller of a scoped method, in its request extensions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Principal {
    pub user_id: i64,
    pub scopes: Vec<Scope>,
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

    fn status(self) -> Status {
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
    headers
        .get_all(axum::http::header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .find_map(|pair| {
            pair.trim()
                .strip_prefix(SESSION_COOKIE)?
                .strip_prefix('=')
                .map(Presented::Session)
        })
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

/// The scopes a credential holds: a session every scope, a token those it lists.
fn scopes(found: &Found) -> Vec<Scope> {
    (1..)
        .map_while(|value| Scope::try_from(value).ok())
        .filter(|scope| {
            found.scopes.as_deref().is_none_or(|listed| {
                listed
                    .split_whitespace()
                    .any(|name| name == scope_name(*scope))
            })
        })
        .collect()
}

fn now() -> Result<i64, Status> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .map_err(|err| Status::internal(format!("clock: {err}")))
}

impl Auth {
    async fn principal(&self, headers: &HeaderMap, scope: Scope) -> Result<Principal, Status> {
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
                false,
            ),
            Some(Presented::Session(value)) => (
                crate::db::sessions::find(readers, &kajiba_core::sha256(value.as_bytes()))
                    .await
                    .map_err(internal)?,
                true,
            ),
        };
        let found = admit(found, now).map_err(refuse)?;
        let scopes = scopes(&found);
        if !scopes.contains(&scope) {
            return Err(refuse(Refusal::ScopeMissing));
        }
        // ADR 0078: at most one usage write per minute and credential, through the writer.
        if found.last_used_at.is_none_or(|used| now - used >= 60_000) {
            let mut writer = writer.lock().await;
            if session {
                crate::db::sessions::seen(&mut writer, found.id, now).await
            } else {
                crate::db::credentials::token_used(&mut writer, found.id, now).await
            }
            .map_err(internal)?;
        }
        Ok(Principal {
            user_id: found.user_id,
            scopes,
        })
    }
}

pub(super) async fn authenticate(
    State(auth): State<Auth>,
    mut request: Request,
    next: Next,
) -> Response {
    // Public methods pass; an unknown method reaches tonic, which answers UNIMPLEMENTED.
    let Some(&Access::Scope(scope)) = auth.access.get(request.uri().path()) else {
        return next.run(request).await;
    };
    match auth.principal(request.headers(), scope).await {
        Ok(principal) => {
            request.extensions_mut().insert(principal);
            next.run(request).await
        }
        Err(status) => status.into_http().map(Body::new),
    }
}

#[cfg(test)]
pub(super) mod tests;
