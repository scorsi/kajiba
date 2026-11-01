//! WebAuthn relying party, in-memory ceremony state and the JSON ceremony routes under
//! `/auth/webauthn/` (ADR 0014, 0019, 0020, 0083, 0084, 0085, 0143).
// Refusals are whole responses, returned by value to the handlers anyway.
#![allow(clippy::result_large_err)]
use super::{
    Health,
    auth::{now, set_cookie},
};
use crate::db::credentials::Found;
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Request, State},
    http::{
        HeaderMap, StatusCode,
        header::{RETRY_AFTER, SET_COOKIE},
    },
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::post,
};
use serde::Deserialize;
use sqlx::Connection;
use std::{
    collections::HashMap,
    io,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::Semaphore;
use webauthn_rs::prelude::{
    DiscoverableAuthentication, DiscoverableKey, Passkey, PasskeyAuthentication,
    PasskeyRegistration, PublicKeyCredential, RegisterPublicKeyCredential, Url, Uuid, Webauthn,
    WebauthnBuilder,
};

/// A challenge is valid for five minutes (ADR 0014).
pub const CEREMONY_TTL: Duration = Duration::from_secs(5 * 60);
/// Outstanding ceremonies held in memory at most; a new one beyond is refused (ADR 0083).
pub const MAX_CEREMONIES: usize = 64;
/// Largest ceremony request body (ADR 0100).
pub const MAX_BODY: usize = 64 * 1024;
/// Ceremony requests in flight at most, admitted before their body is read (ADR 0100).
pub const MAX_REQUESTS: usize = 32;
/// Longest passkey name, in characters (ADR 0084).
const MAX_NAME: usize = 64;
/// A step-up lets its session add or remove passkeys for five minutes (ADR 0143).
pub const STEP_UP_TTL: Duration = Duration::from_secs(5 * 60);

/// RP ID = host of `http.public_url`, origin = the URL itself, port included (ADR 0014,
/// 0019). A host that is not a domain name (an IP address) cannot be an RP ID.
pub fn relying_party(public_url: &str) -> Result<Webauthn, String> {
    let error =
        |err: &dyn std::fmt::Display| format!("http.public_url: no WebAuthn relying party: {err}");
    let origin = Url::parse(public_url).map_err(|err| error(&err))?;
    let rp_id = origin
        .domain()
        .ok_or_else(|| error(&"the host is not a domain name"))?;
    WebauthnBuilder::new(rp_id, &origin)
        .and_then(WebauthnBuilder::build)
        .map_err(|err| error(&err))
}

/// The WebAuthn user handle of a user: the 128 bits of its ULID public id (ADR 0084).
pub fn user_handle(public_id: &str) -> Uuid {
    const ALPHABET: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    Uuid::from_u128(public_id.bytes().fold(0, |value, byte| {
        // Public ids are canonical (checked by the schema): the first character holds 3 bits.
        let digit = ALPHABET.iter().position(|&c| c == byte).unwrap_or(0);
        (value << 5) | digit as u128
    }))
}

/// Ceremony states between start and finish, keyed by a random id handed to the client.
/// Lost on restart (docs/schema.md); an id is usable once, within `CEREMONY_TTL`.
pub struct Ceremonies<T> {
    pending: HashMap<String, (Instant, T)>,
}

impl<T> Default for Ceremonies<T> {
    fn default() -> Self {
        Self {
            pending: HashMap::new(),
        }
    }
}

impl<T> Ceremonies<T> {
    /// Keeps `state` and returns its id; `QuotaExceeded` when `MAX_CEREMONIES` unexpired
    /// ceremonies are outstanding (refused rather than evicting one in progress).
    pub fn begin(&mut self, state: T, now: Instant) -> io::Result<String> {
        self.pending
            .retain(|_, (started, _)| now.saturating_duration_since(*started) < CEREMONY_TTL);
        if self.pending.len() >= MAX_CEREMONIES {
            tracing::warn!(event = "webauthn-ceremonies-full", limit = MAX_CEREMONIES);
            return Err(io::ErrorKind::QuotaExceeded.into());
        }
        let id: String = crate::db::secret_bytes()?
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        self.pending.insert(id.clone(), (now, state));
        Ok(id)
    }

    /// Removes and returns the state of `id`; `None` when unknown, already finished or
    /// expired.
    pub fn finish(&mut self, id: &str, now: Instant) -> Option<T> {
        self.pending
            .remove(id)
            .filter(|(started, _)| now.saturating_duration_since(*started) < CEREMONY_TTL)
            .map(|(_, state)| state)
    }
}

enum Ceremony {
    Enroll {
        state: PasskeyRegistration,
        user: i64,
        token_hash: [u8; 32],
    },
    Login(DiscoverableAuthentication),
    /// Re-authentication of a signed-in session with one of its user's passkeys.
    StepUp {
        state: PasskeyAuthentication,
        session: i64,
    },
    /// An additional passkey for a session that stepped up.
    Register {
        state: PasskeyRegistration,
        session: i64,
    },
}

/// The relying party, the outstanding ceremonies and the lifetime of the sessions the
/// ceremonies open.
pub struct Passkeys {
    webauthn: Webauthn,
    session_ttl: Duration,
    ceremonies: Mutex<Ceremonies<Ceremony>>,
}

impl Passkeys {
    pub fn new(public_url: &str, session_ttl: Duration) -> Result<Self, String> {
        Ok(Self {
            webauthn: relying_party(public_url)?,
            session_ttl,
            ceremonies: Mutex::default(),
        })
    }

    fn begin(&self, ceremony: Ceremony) -> Result<String, Response> {
        let mut ceremonies = self
            .ceremonies
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        ceremonies
            .begin(ceremony, Instant::now())
            .map_err(|err| match err.kind() {
                io::ErrorKind::QuotaExceeded => {
                    refusal(StatusCode::SERVICE_UNAVAILABLE, "CEREMONIES_FULL")
                }
                _ => internal(err),
            })
    }

    fn finish(&self, id: &str) -> Result<Ceremony, Response> {
        let mut ceremonies = self
            .ceremonies
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        ceremonies
            .finish(id, Instant::now())
            .ok_or_else(|| refused("CEREMONY_INVALID"))
    }
}

/// A refused ceremony step: `{"reason": …}`, logged without any secret (ADR 0022).
fn refusal(status: StatusCode, reason: &'static str) -> Response {
    tracing::warn!(event = "webauthn-refused", reason);
    (status, Json(serde_json::json!({ "reason": reason }))).into_response()
}

fn refused(reason: &'static str) -> Response {
    refusal(StatusCode::FORBIDDEN, reason)
}

fn internal(err: impl std::fmt::Display) -> Response {
    tracing::error!(event = "webauthn-failed", err = %err);
    StatusCode::INTERNAL_SERVER_ERROR.into_response()
}

fn starting() -> Response {
    StatusCode::SERVICE_UNAVAILABLE.into_response()
}

pub(super) fn routes(health: Health) -> Router {
    let budget = health.webauthn_requests.clone();
    Router::new()
        .route("/auth/webauthn/enroll/start", post(enroll_start))
        .route("/auth/webauthn/enroll/finish", post(enroll_finish))
        .route("/auth/webauthn/login/start", post(login_start))
        .route("/auth/webauthn/login/finish", post(login_finish))
        .route("/auth/webauthn/step-up/start", post(step_up_start))
        .route("/auth/webauthn/step-up/finish", post(step_up_finish))
        .route("/auth/webauthn/register/start", post(register_start))
        .route("/auth/webauthn/register/finish", post(register_finish))
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .route_layer(middleware::from_fn_with_state(budget, admit))
        .route_layer(middleware::from_fn_with_state(health.clone(), maintenance))
        .with_state(health)
}

async fn maintenance(State(health): State<Health>, request: Request, next: Next) -> Response {
    let Ok(_admitted) = health.maintenance.admit() else {
        return refusal(StatusCode::SERVICE_UNAVAILABLE, "MAINTENANCE");
    };
    next.run(request).await
}

/// A permit of the shared ceremony budget, held until the handler answers; taken before the
/// body is read, so at most `MAX_REQUESTS` bodies are buffered at once (ADR 0100).
async fn admit(State(budget): State<Arc<Semaphore>>, request: Request, next: Next) -> Response {
    let Ok(_permit) = budget.try_acquire_owned() else {
        let mut busy = refusal(StatusCode::SERVICE_UNAVAILABLE, "BUSY");
        busy.headers_mut()
            .insert(RETRY_AFTER, axum::http::HeaderValue::from_static("1"));
        return busy;
    };
    next.run(request).await
}

#[derive(Deserialize)]
struct EnrollStart {
    /// The secret of the `admin enroll` link, read by the page from its fragment.
    token: String,
}

async fn enroll_start(State(health): State<Health>, Json(body): Json<EnrollStart>) -> Response {
    let (Some(passkeys), Some(readers)) = (health.passkeys.get(), health.database.get()) else {
        return starting();
    };
    let Ok(now) = now() else {
        return internal("clock");
    };
    let token_hash = kajiba_core::sha256(body.token.as_bytes());
    let user = match crate::db::enrollment::find(readers, &token_hash, now).await {
        Ok(Some(user)) => user,
        Ok(None) => return refused("ENROLLMENT_INVALID"),
        Err(err) => return internal(err),
    };
    let (user, public_id, name) = user;
    // Passkeys were deleted when the link was issued: nothing to exclude.
    let (options, state) = match passkeys.webauthn.start_passkey_registration(
        user_handle(&public_id),
        &name,
        &name,
        None,
    ) {
        Ok(started) => started,
        Err(err) => return internal(err),
    };
    // Login has no username step (ADR 0085): the credential must be discoverable, which
    // webauthn-rs leaves optional (its proto type is not re-exported: set it in JSON).
    let mut options = serde_json::json!(options);
    options["publicKey"]["authenticatorSelection"]["residentKey"] = "required".into();
    match passkeys.begin(Ceremony::Enroll {
        state,
        user,
        token_hash,
    }) {
        Ok(ceremony) => {
            Json(serde_json::json!({ "ceremony": ceremony, "options": options })).into_response()
        }
        Err(response) => response,
    }
}

#[derive(Deserialize)]
struct EnrollFinish {
    ceremony: String,
    credential: RegisterPublicKeyCredential,
    /// Optional label of the passkey (ADR 0084).
    #[serde(default)]
    name: String,
}

async fn enroll_finish(State(health): State<Health>, Json(body): Json<EnrollFinish>) -> Response {
    let (Some(passkeys), Some(writer)) = (health.passkeys.get(), health.writer.get()) else {
        return starting();
    };
    // Checked first: a bad name must not burn the ceremony.
    if let Err(response) = valid_name(&body.name) {
        return response;
    }
    let (state, user, token_hash) = match passkeys.finish(&body.ceremony) {
        Ok(Ceremony::Enroll {
            state,
            user,
            token_hash,
        }) => (state, user, token_hash),
        Ok(_) => return refused("CEREMONY_INVALID"),
        Err(response) => return response,
    };
    // Checks the challenge, the origin and the RP ID among others.
    let passkey = match passkeys
        .webauthn
        .finish_passkey_registration(&body.credential, &state)
    {
        Ok(passkey) => passkey,
        Err(_) => return refused("VERIFICATION_FAILED"),
    };
    let serialized = match serde_json::to_string(&passkey) {
        Ok(serialized) => serialized,
        Err(err) => return internal(err),
    };
    let mut writer = writer.lock().await;
    let Ok(now) = now() else {
        return internal("clock");
    };
    let opened = async {
        let mut tx = writer.begin().await?;
        if !crate::db::enrollment::consume(&mut tx, user, &token_hash, now).await? {
            return Ok(None);
        }
        crate::db::passkeys::insert(
            &mut tx,
            user,
            passkey.cred_id().as_ref(),
            &serialized,
            &body.name,
            now,
        )
        .await?;
        let cookie = crate::db::sessions::create(&mut tx, user, passkeys.session_ttl, now).await?;
        tx.commit().await?;
        Ok::<_, sqlx::Error>(Some(cookie))
    }
    .await;
    match opened {
        Ok(Some(cookie)) => session_opened(&cookie, passkeys.session_ttl),
        Ok(None) => refused("ENROLLMENT_INVALID"),
        Err(err) => internal(err),
    }
}

/// The answer of a ceremony that opened a session: `204` and the cookie (ADR 0079).
fn session_opened(cookie: &str, ttl: Duration) -> Response {
    (
        StatusCode::NO_CONTENT,
        [(SET_COOKIE, set_cookie(cookie, ttl))],
    )
        .into_response()
}

/// Discoverable login (ADR 0085): no body, the authenticator picks the credential.
async fn login_start(State(health): State<Health>) -> Response {
    let Some(passkeys) = health.passkeys.get() else {
        return starting();
    };
    let (mut options, state) = match passkeys.webauthn.start_discoverable_authentication() {
        Ok(started) => started,
        Err(err) => return internal(err),
    };
    // webauthn-rs asks for conditional (autofill) mediation; the page decides how to prompt.
    options.mediation = None;
    match passkeys.begin(Ceremony::Login(state)) {
        Ok(ceremony) => {
            Json(serde_json::json!({ "ceremony": ceremony, "options": options })).into_response()
        }
        Err(response) => response,
    }
}

#[derive(Deserialize)]
struct LoginFinish {
    ceremony: String,
    credential: PublicKeyCredential,
}

async fn login_finish(State(health): State<Health>, Json(body): Json<LoginFinish>) -> Response {
    let (Some(passkeys), Some(writer)) = (health.passkeys.get(), health.writer.get()) else {
        return starting();
    };
    let state = match passkeys.finish(&body.ceremony) {
        Ok(Ceremony::Login(state)) => state,
        Ok(_) => return refused("CEREMONY_INVALID"),
        Err(response) => return response,
    };
    let Ok((handle, credential_id)) = passkeys
        .webauthn
        .identify_discoverable_authentication(&body.credential)
    else {
        return refused("VERIFICATION_FAILED");
    };
    // Serialize lookup and verification with recovery and other logins.
    let mut writer = writer.lock().await;
    let mut tx = match writer.begin().await {
        Ok(tx) => tx,
        Err(err) => return internal(err),
    };
    // Unknown credential, deleted passkey (admin enroll, removal) or disabled user.
    let (id, user, public_id, stored) =
        match crate::db::passkeys::find(&mut tx, credential_id).await {
            Ok(Some(found)) => found,
            Ok(None) => return refused("PASSKEY_INVALID"),
            Err(err) => return internal(err),
        };
    if handle != user_handle(&public_id) {
        return refused("PASSKEY_INVALID");
    }
    let mut passkey: Passkey = match serde_json::from_str(&stored) {
        Ok(passkey) => passkey,
        Err(err) => return internal(err),
    };
    // Checks the challenge, the origin, the signature and user verification.
    let Ok(result) = passkeys.webauthn.finish_discoverable_authentication(
        &body.credential,
        state,
        &[DiscoverableKey::from(&passkey)],
    ) else {
        return refused("VERIFICATION_FAILED");
    };
    passkey.update_credential(&result);
    let (Ok(serialized), Ok(now)) = (serde_json::to_string(&passkey), now()) else {
        return internal("passkey state");
    };
    let opened = async {
        if !crate::db::passkeys::used(&mut tx, &id, user, &serialized, now).await? {
            return Ok(None);
        }
        let cookie = crate::db::sessions::create(&mut tx, user, passkeys.session_ttl, now).await?;
        tx.commit().await?;
        Ok::<_, sqlx::Error>(Some(cookie))
    }
    .await;
    match opened {
        Ok(Some(cookie)) => session_opened(&cookie, passkeys.session_ttl),
        Ok(None) => refused("PASSKEY_INVALID"),
        Err(err) => internal(err),
    }
}

fn valid_name(name: &str) -> Result<(), Response> {
    if name.chars().count() > MAX_NAME || name.chars().any(char::is_control) {
        return Err(refusal(StatusCode::BAD_REQUEST, "NAME_INVALID"));
    }
    Ok(())
}

/// The request's valid session; `401` `SESSION_INVALID` without one (ADR 0143).
async fn signed_in(
    connection: impl sqlx::SqliteExecutor<'_>,
    headers: &HeaderMap,
    now: i64,
) -> Result<Found, Response> {
    match super::auth::session(connection, headers, now).await {
        Ok(Some(found)) => Ok(found),
        Ok(None) => Err(refusal(StatusCode::UNAUTHORIZED, "SESSION_INVALID")),
        Err(err) => Err(internal(err)),
    }
}

/// The request's valid session, the one `ceremony` was started by (a cookie of another
/// session, even of the same user, cannot finish it).
async fn same_session(
    connection: impl sqlx::SqliteExecutor<'_>,
    headers: &HeaderMap,
    now: i64,
    ceremony: i64,
) -> Result<Found, Response> {
    let found = signed_in(connection, headers, now).await?;
    if found.id != ceremony {
        return Err(refused("CEREMONY_INVALID"));
    }
    Ok(found)
}

/// Refuses with `403` `STEP_UP_REQUIRED` unless session `id` stepped up within
/// `STEP_UP_TTL` (ADR 0143).
async fn stepped_up(
    connection: impl sqlx::SqliteExecutor<'_>,
    id: i64,
    now: i64,
) -> Result<(), Response> {
    match crate::db::sessions::step_up_fresh(connection, id, STEP_UP_TTL.as_millis() as i64, now)
        .await
    {
        Ok(true) => Ok(()),
        Ok(false) => Err(refused("STEP_UP_REQUIRED")),
        Err(err) => Err(internal(err)),
    }
}

/// The passkeys of `user`, deserialized.
async fn passkeys_of(
    connection: impl sqlx::SqliteExecutor<'_>,
    user: i64,
) -> Result<Vec<Passkey>, Response> {
    crate::db::passkeys::of_user(connection, user)
        .await
        .map_err(internal)?
        .iter()
        .map(|stored| serde_json::from_str(stored).map_err(internal))
        .collect()
}

/// Step-up (ADR 0143): a signed-in session asks for an assertion by one of its user's
/// passkeys, named in the request options.
async fn step_up_start(State(health): State<Health>, headers: HeaderMap) -> Response {
    let (Some(passkeys), Some(readers)) = (health.passkeys.get(), health.database.get()) else {
        return starting();
    };
    let Ok(now) = now() else {
        return internal("clock");
    };
    let session = match signed_in(readers, &headers, now).await {
        Ok(session) => session,
        Err(response) => return response,
    };
    let allowed = match passkeys_of(readers, session.user_id).await {
        Ok(allowed) if !allowed.is_empty() => allowed,
        Ok(_) => return refused("PASSKEY_INVALID"),
        Err(response) => return response,
    };
    let (options, state) = match passkeys.webauthn.start_passkey_authentication(&allowed) {
        Ok(started) => started,
        Err(err) => return internal(err),
    };
    match passkeys.begin(Ceremony::StepUp {
        state,
        session: session.id,
    }) {
        Ok(ceremony) => {
            Json(serde_json::json!({ "ceremony": ceremony, "options": options })).into_response()
        }
        Err(response) => response,
    }
}

async fn step_up_finish(
    State(health): State<Health>,
    headers: HeaderMap,
    Json(body): Json<LoginFinish>,
) -> Response {
    let (Some(passkeys), Some(writer)) = (health.passkeys.get(), health.writer.get()) else {
        return starting();
    };
    let (state, session) = match passkeys.finish(&body.ceremony) {
        Ok(Ceremony::StepUp { state, session }) => (state, session),
        Ok(_) => return refused("CEREMONY_INVALID"),
        Err(response) => return response,
    };
    let Ok(now) = now() else {
        return internal("clock");
    };
    // Serialized with logins, removals and recovery, as login is.
    let mut writer = writer.lock().await;
    let mut tx = match writer.begin().await {
        Ok(tx) => tx,
        Err(err) => return internal(err),
    };
    let session = match same_session(&mut *tx, &headers, now, session).await {
        Ok(session) => session,
        Err(response) => return response,
    };
    // A passkey removed since the start, or another user's credential.
    let (id, stored) =
        match crate::db::passkeys::find(&mut tx, body.credential.raw_id.as_ref()).await {
            Ok(Some((id, user, _, stored))) if user == session.user_id => (id, stored),
            Ok(_) => return refused("PASSKEY_INVALID"),
            Err(err) => return internal(err),
        };
    let mut passkey: Passkey = match serde_json::from_str(&stored) {
        Ok(passkey) => passkey,
        Err(err) => return internal(err),
    };
    // Checks the challenge, the origin, the allowed credentials, the signature and user
    // verification.
    let Ok(result) = passkeys
        .webauthn
        .finish_passkey_authentication(&body.credential, &state)
    else {
        return refused("VERIFICATION_FAILED");
    };
    passkey.update_credential(&result);
    let Ok(serialized) = serde_json::to_string(&passkey) else {
        return internal("passkey state");
    };
    let recorded = async {
        if !crate::db::passkeys::used(&mut tx, &id, session.user_id, &serialized, now).await?
            || !crate::db::sessions::stepped_up(&mut tx, session.id, now).await?
        {
            return Ok(false);
        }
        tx.commit().await?;
        Ok::<_, sqlx::Error>(true)
    }
    .await;
    match recorded {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => refused("PASSKEY_INVALID"),
        Err(err) => internal(err),
    }
}

/// An additional passkey (ADR 0143): the session must have stepped up; the user's
/// passkeys are excluded so that an authenticator does not register twice.
async fn register_start(State(health): State<Health>, headers: HeaderMap) -> Response {
    let (Some(passkeys), Some(readers)) = (health.passkeys.get(), health.database.get()) else {
        return starting();
    };
    let Ok(now) = now() else {
        return internal("clock");
    };
    let session = match signed_in(readers, &headers, now).await {
        Ok(session) => session,
        Err(response) => return response,
    };
    if let Err(response) = stepped_up(readers, session.id, now).await {
        return response;
    }
    let user: Result<(String, String), _> =
        sqlx::query_as("SELECT public_id, name FROM user WHERE id = ?")
            .bind(session.user_id)
            .fetch_one(readers)
            .await;
    let (public_id, name) = match user {
        Ok(user) => user,
        Err(err) => return internal(err),
    };
    let excluded = match passkeys_of(readers, session.user_id).await {
        Ok(existing) => existing.iter().map(|p| p.cred_id().clone()).collect(),
        Err(response) => return response,
    };
    let (options, state) = match passkeys.webauthn.start_passkey_registration(
        user_handle(&public_id),
        &name,
        &name,
        Some(excluded),
    ) {
        Ok(started) => started,
        Err(err) => return internal(err),
    };
    // Discoverable, as at enrollment (ADR 0085).
    let mut options = serde_json::json!(options);
    options["publicKey"]["authenticatorSelection"]["residentKey"] = "required".into();
    match passkeys.begin(Ceremony::Register {
        state,
        session: session.id,
    }) {
        Ok(ceremony) => {
            Json(serde_json::json!({ "ceremony": ceremony, "options": options })).into_response()
        }
        Err(response) => response,
    }
}

#[derive(Deserialize)]
struct RegisterFinish {
    ceremony: String,
    credential: RegisterPublicKeyCredential,
    #[serde(default)]
    name: String,
}

/// Stores the additional passkey; the session and its cookie stay as they are.
async fn register_finish(
    State(health): State<Health>,
    headers: HeaderMap,
    Json(body): Json<RegisterFinish>,
) -> Response {
    let (Some(passkeys), Some(writer)) = (health.passkeys.get(), health.writer.get()) else {
        return starting();
    };
    if let Err(response) = valid_name(&body.name) {
        return response;
    }
    let (state, session) = match passkeys.finish(&body.ceremony) {
        Ok(Ceremony::Register { state, session }) => (state, session),
        Ok(_) => return refused("CEREMONY_INVALID"),
        Err(response) => return response,
    };
    let Ok(passkey) = passkeys
        .webauthn
        .finish_passkey_registration(&body.credential, &state)
    else {
        return refused("VERIFICATION_FAILED");
    };
    let Ok(serialized) = serde_json::to_string(&passkey) else {
        return internal("passkey state");
    };
    let Ok(now) = now() else {
        return internal("clock");
    };
    let mut writer = writer.lock().await;
    let mut tx = match writer.begin().await {
        Ok(tx) => tx,
        Err(err) => return internal(err),
    };
    // Logout, recovery or an expired step-up since the start.
    let session = match same_session(&mut *tx, &headers, now, session).await {
        Ok(session) => session,
        Err(response) => return response,
    };
    if let Err(response) = stepped_up(&mut *tx, session.id, now).await {
        return response;
    }
    let stored = async {
        crate::db::passkeys::insert(
            &mut tx,
            session.user_id,
            passkey.cred_id().as_ref(),
            &serialized,
            &body.name,
            now,
        )
        .await?;
        tx.commit().await
    }
    .await;
    match stored {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        // A credential already registered, by this user or another.
        Err(sqlx::Error::Database(err)) if err.is_unique_violation() => {
            refused("VERIFICATION_FAILED")
        }
        Err(err) => internal(err),
    }
}

#[cfg(test)]
mod tests;
