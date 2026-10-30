//! `TokenService` (ADR 0014, 0020, 0081): the caller's personal access tokens, managed
//! from a browser session only; declared tokens are listed but read-only.
// These feed `?` in tonic handlers, which return `Status` by value anyway.
#![allow(clippy::result_large_err)]

use super::{
    Health,
    auth::{Principal, now, scope_name, scopes},
    cursor::{self, invalid_field},
    local::refused,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use kajiba_proto::{
    google::protobuf::Timestamp,
    v1::{
        CreateTokenRequest, CreateTokenResponse, ListTokensRequest, ListTokensResponse,
        RevokeTokenRequest, RevokeTokenResponse, Scope, Token, TokenSource,
        token_service_server::TokenService,
    },
};
use sqlx::SqliteConnection;
use tonic::{Request, Response, Status};

pub struct Tokens(pub Health);

fn internal(err: sqlx::Error) -> Status {
    Status::internal(format!("tokens: {err}"))
}

/// The session caller: a PAT never manages PATs (ADR 0081).
fn session_caller<T>(request: &Request<T>) -> Result<i64, Status> {
    let principal = request
        .extensions()
        .get::<Principal>()
        .ok_or_else(|| Status::internal("tokens: no authenticated caller"))?;
    if !principal.session {
        return Err(refused(
            tonic::Code::PermissionDenied,
            "SESSION_REQUIRED",
            "personal access tokens are managed from a browser session".into(),
        ));
    }
    Ok(principal.user_id)
}

type Row = (
    i64,
    String,
    String,
    String,
    String,
    i64,
    Option<i64>,
    Option<i64>,
    Option<i64>,
);

const SELECT: &str = "SELECT id, public_id, name, scopes, source, created_at, expires_at,
    last_used_at, revoked_at FROM api_token";

fn timestamp(millis: i64) -> Timestamp {
    Timestamp {
        seconds: millis.div_euclid(1000),
        nanos: (millis.rem_euclid(1000) * 1_000_000) as i32,
    }
}

/// Epoch milliseconds of a valid timestamp, with checked arithmetic (ADR 0029).
fn millis(timestamp: &Timestamp) -> Option<i64> {
    (0..1_000_000_000)
        .contains(&timestamp.nanos)
        .then_some(())?;
    timestamp
        .seconds
        .checked_mul(1000)?
        .checked_add(i64::from(timestamp.nanos / 1_000_000))
}

/// Metadata only: the hash never leaves the database.
fn token(row: Row) -> Token {
    let (_, public_id, name, listed, source, created, expires, used, revoked) = row;
    Token {
        public_id,
        name,
        scopes: scopes(Some(&listed)).into_iter().map(i32::from).collect(),
        source: if source == "config" {
            TokenSource::Config
        } else {
            TokenSource::Runtime
        }
        .into(),
        created_at: Some(timestamp(created)),
        expires_at: expires.map(timestamp),
        last_used_at: used.map(timestamp),
        revoked_at: revoked.map(timestamp),
    }
}

async fn row(connection: &mut SqliteConnection, id: i64) -> Result<Row, Status> {
    sqlx::query_as(&format!("{SELECT} WHERE id = ?"))
        .bind(id)
        .fetch_one(connection)
        .await
        .map_err(internal)
}

impl Tokens {
    fn writer(&self) -> Result<&crate::db::Writer, Status> {
        self.0
            .writer
            .get()
            .ok_or_else(|| Status::unavailable("starting"))
    }
}

#[tonic::async_trait]
impl TokenService for Tokens {
    async fn create_token(
        &self,
        request: Request<CreateTokenRequest>,
    ) -> Result<Response<CreateTokenResponse>, Status> {
        let user = session_caller(&request)?;
        let request = request.into_inner();
        let name = kajiba_core::names::validate_name(&request.name)
            .map_err(|err| invalid_field("name", &err.to_string()))?;
        let mut requested = request
            .scopes
            .iter()
            .map(|value| {
                Scope::try_from(*value)
                    .ok()
                    .filter(|scope| *scope != Scope::Unspecified)
            })
            .collect::<Option<Vec<_>>>()
            .filter(|scopes| !scopes.is_empty())
            .ok_or_else(|| invalid_field("scopes", "at least one known scope"))?;
        requested.sort();
        requested.dedup();
        let listed = requested
            .into_iter()
            .map(scope_name)
            .collect::<Vec<_>>()
            .join(" ");
        let now = now()?;
        let expires_at = match &request.expires_at {
            None => None,
            Some(at) => Some(
                millis(at)
                    .filter(|at| *at > now)
                    .ok_or_else(|| invalid_field("expires_at", "must be a future instant"))?,
            ),
        };
        let secret = format!(
            "kjb_{}",
            URL_SAFE_NO_PAD
                .encode(crate::db::secret_bytes().map_err(|err| internal(sqlx::Error::Io(err)))?)
        );
        // The single writer serializes creations: the name check cannot race.
        let mut writer = self.writer()?.lock().await;
        let taken: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM api_token WHERE user_id = ? AND name = ?)",
        )
        .bind(user)
        .bind(&name)
        .fetch_one(&mut *writer)
        .await
        .map_err(internal)?;
        if taken {
            return Err(refused(
                tonic::Code::AlreadyExists,
                "NAME_TAKEN",
                format!("a token named {name} already exists"),
            ));
        }
        let public_id = crate::db::users::new_public_id(&mut writer, now as u128)
            .await
            .map_err(internal)?;
        let id = sqlx::query(
            "INSERT INTO api_token (public_id, user_id, name, token_hash, scopes, source, created_at, expires_at)
             VALUES (?, ?, ?, ?, ?, 'runtime', ?, ?)",
        )
        .bind(public_id)
        .bind(user)
        .bind(&name)
        .bind(kajiba_core::sha256(secret.as_bytes()).as_slice())
        .bind(listed)
        .bind(now)
        .bind(expires_at)
        .execute(&mut *writer)
        .await
        .map_err(internal)?
        .last_insert_rowid();
        Ok(Response::new(CreateTokenResponse {
            token: Some(token(row(&mut writer, id).await?)),
            secret,
        }))
    }

    async fn list_tokens(
        &self,
        request: Request<ListTokensRequest>,
    ) -> Result<Response<ListTokensResponse>, Status> {
        const METHOD: &str = "v1.ListTokens";
        let user = session_caller(&request)?;
        let request = request.into_inner();
        let size = cursor::page_size(request.page_size)?;
        let (key, id) = cursor::decode(METHOD, &request.page_token)?.unwrap_or_default();
        let readers = self
            .0
            .database
            .get()
            .ok_or_else(|| Status::unavailable("starting"))?;
        let mut rows: Vec<Row> = sqlx::query_as(&format!(
            "{SELECT} WHERE user_id = ?1 AND (name, id) > (?2, ?3) ORDER BY name, id LIMIT ?4"
        ))
        .bind(user)
        .bind(key)
        .bind(id)
        .bind(size + 1)
        .fetch_all(readers)
        .await
        .map_err(internal)?;
        let more = rows.len() as i64 > size;
        rows.truncate(size as usize);
        let next_page_token = match rows.last() {
            Some((id, _, name, ..)) if more => cursor::encode(METHOD, name, *id),
            _ => String::new(),
        };
        Ok(Response::new(ListTokensResponse {
            tokens: rows.into_iter().map(token).collect(),
            next_page_token,
        }))
    }

    async fn revoke_token(
        &self,
        request: Request<RevokeTokenRequest>,
    ) -> Result<Response<RevokeTokenResponse>, Status> {
        let user = session_caller(&request)?;
        let public_id = request.into_inner().public_id;
        let mut writer = self.writer()?.lock().await;
        let found: Option<(i64, String)> =
            sqlx::query_as("SELECT id, source FROM api_token WHERE public_id = ? AND user_id = ?")
                .bind(&public_id)
                .bind(user)
                .fetch_optional(&mut *writer)
                .await
                .map_err(internal)?;
        let id = match found {
            None => {
                return Err(refused(
                    tonic::Code::NotFound,
                    "TOKEN_NOT_FOUND",
                    "no token of yours by this id".into(),
                ));
            }
            Some((_, source)) if source == "config" => {
                return Err(refused(
                    tonic::Code::FailedPrecondition,
                    "DECLARED_IN_CONFIG",
                    "a declared token changes only in the configuration".into(),
                ));
            }
            Some((id, _)) => id,
        };
        sqlx::query("UPDATE api_token SET revoked_at = ? WHERE id = ? AND revoked_at IS NULL")
            .bind(now()?)
            .bind(id)
            .execute(&mut *writer)
            .await
            .map_err(internal)?;
        Ok(Response::new(RevokeTokenResponse {
            token: Some(token(row(&mut writer, id).await?)),
        }))
    }
}

#[cfg(test)]
mod tests;
