//! WebAuthn relying party, in-memory ceremony state and the JSON ceremony routes under
//! `/auth/webauthn/` (ADR 0014, 0019, 0020, 0083, 0084).
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
    PasskeyRegistration, RegisterPublicKeyCredential, Url, Uuid, Webauthn, WebauthnBuilder,
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
    let Ceremony::Enroll {
        state,
        user,
        token_hash,
    } = match passkeys.finish(&body.ceremony) {
        Ok(ceremony) => ceremony,
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
    let Ok(now) = now() else {
        return internal("clock");
    };
    let serialized = match serde_json::to_string(&passkey) {
        Ok(serialized) => serialized,
        Err(err) => return internal(err),
    };
    let mut writer = writer.lock().await;
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
        Ok(Some(cookie)) => (
            StatusCode::NO_CONTENT,
            [(SET_COOKIE, set_cookie(&cookie, passkeys.session_ttl))],
        )
            .into_response(),
        Ok(None) => refused("ENROLLMENT_INVALID"),
        Err(err) => internal(err),
    }
}

#[cfg(test)]
mod tests;
