use super::*;
use webauthn_rs::prelude::Uuid;

/// The RP ID a browser is told, read from a registration challenge.
fn rp_id(webauthn: &Webauthn) -> String {
    let (challenge, _) = webauthn
        .start_passkey_registration(Uuid::nil(), "alice", "alice", None)
        .unwrap();
    challenge.public_key.rp.id
}

#[test]
fn relying_party_follows_the_public_url() {
    for (url, id, origin) in [
        (
            "https://forge.example.org",
            "forge.example.org",
            "https://forge.example.org/",
        ),
        (
            "https://Forge.Example.org:8443/",
            "forge.example.org",
            "https://forge.example.org:8443/",
        ),
        // Browsers treat http://localhost as a secure context (ADR 0006).
        (
            "http://localhost:5173",
            "localhost",
            "http://localhost:5173/",
        ),
    ] {
        let webauthn = relying_party(url).unwrap();
        assert_eq!(rp_id(&webauthn), id, "{url}");
        let origins: Vec<_> = webauthn
            .get_allowed_origins()
            .iter()
            .map(Url::as_str)
            .collect();
        assert_eq!(origins, [origin], "{url}");
    }
    for url in ["http://127.0.0.1:8080", "http://[::1]:8080"] {
        let err = relying_party(url).err().unwrap();
        assert!(err.starts_with("http.public_url: "), "{url}: {err}");
    }
}

#[test]
fn a_ceremony_finishes_once_within_its_ttl() {
    let start = Instant::now();
    let mut ceremonies = Ceremonies::default();
    let id = ceremonies.begin("state", start).unwrap();
    assert_eq!(id.len(), 64);
    assert_eq!(ceremonies.finish("unknown", start), None);
    let just_in_time = start + CEREMONY_TTL - Duration::from_millis(1);
    assert_eq!(ceremonies.finish(&id, just_in_time), Some("state"));
    // Replay: the state is gone after its first finish.
    assert_eq!(ceremonies.finish(&id, just_in_time), None);

    let id = ceremonies.begin("late", start).unwrap();
    assert_eq!(ceremonies.finish(&id, start + CEREMONY_TTL), None);
}

#[test]
fn outstanding_ceremonies_are_bounded() {
    let start = Instant::now();
    let mut ceremonies = Ceremonies::default();
    let first = ceremonies.begin(0, start).unwrap();
    for n in 1..MAX_CEREMONIES {
        ceremonies.begin(n, start + Duration::from_secs(1)).unwrap();
    }
    let full = ceremonies.begin(MAX_CEREMONIES, start + Duration::from_secs(1));
    assert_eq!(full.unwrap_err().kind(), io::ErrorKind::QuotaExceeded);
    // The refusal evicted nothing in progress.
    assert_eq!(ceremonies.finish(&first, start), Some(0));
    ceremonies.begin(MAX_CEREMONIES, start).unwrap();
    // An expired ceremony no longer counts.
    ceremonies.begin(99, start + CEREMONY_TTL).unwrap();
}
