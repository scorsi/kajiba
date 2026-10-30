//! WebAuthn relying party, in-memory ceremony state and the JSON ceremony routes under
//! `/auth/webauthn/` (ADR 0014, 0019, 0020, 0083, 0084, 0085).
// Refusals are whole responses, returned by value to the handlers anyway.
#![allow(clippy::result_large_err)]
use super::{
    Health,
    auth::{now, set_cookie},
};
use axum::{
    Json, Router,
    extract::State,
    http::{StatusCode, header::SET_COOKIE},
    response::{IntoResponse, Response},
    routing::post,
};
use serde::Deserialize;
use sqlx::Connection;
use std::{
    collections::HashMap,
    io,
    sync::Mutex,
    time::{Duration, Instant},
};
use webauthn_rs::prelude::{
    DiscoverableAuthentication, DiscoverableKey, Passkey, PasskeyRegistration, PublicKeyCredential,
    RegisterPublicKeyCredential, Url, Uuid, Webauthn, WebauthnBuilder,
};

/// A challenge is valid for five minutes (ADR 0014).
pub const CEREMONY_TTL: Duration = Duration::from_secs(5 * 60);
/// Outstanding ceremonies held in memory at most; a new one beyond is refused (ADR 0083).
pub const MAX_CEREMONIES: usize = 64;
/// Longest passkey name, in characters (ADR 0084).
const MAX_NAME: usize = 64;

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
    Router::new()
        .route("/auth/webauthn/enroll/start", post(enroll_start))
        .route("/auth/webauthn/enroll/finish", post(enroll_finish))
        .route("/auth/webauthn/login/start", post(login_start))
        .route("/auth/webauthn/login/finish", post(login_finish))
        .with_state(health)
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
    if body.name.chars().count() > MAX_NAME || body.name.chars().any(char::is_control) {
        return refusal(StatusCode::BAD_REQUEST, "NAME_INVALID");
    }
    let (state, user, token_hash) = match passkeys.finish(&body.ceremony) {
        Ok(Ceremony::Enroll {
            state,
            user,
            token_hash,
        }) => (state, user, token_hash),
        Ok(Ceremony::Login(_)) => return refused("CEREMONY_INVALID"),
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
        Ok(Ceremony::Enroll { .. }) => return refused("CEREMONY_INVALID"),
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

#[cfg(test)]
mod tests;
