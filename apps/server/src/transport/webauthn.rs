//! WebAuthn relying party and in-memory ceremony state (ADR 0014, 0019, 0083).
// Used by the enrollment and login routes (J4 tasks 9-10).
#![cfg_attr(not(test), allow(dead_code))]

use std::{
    collections::HashMap,
    io,
    time::{Duration, Instant},
};
use webauthn_rs::prelude::{Url, Webauthn, WebauthnBuilder};

/// A challenge is valid for five minutes (ADR 0014).
pub const CEREMONY_TTL: Duration = Duration::from_secs(5 * 60);
/// Outstanding ceremonies held in memory at most; a new one beyond is refused (ADR 0083).
pub const MAX_CEREMONIES: usize = 64;

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

#[cfg(test)]
mod tests;
