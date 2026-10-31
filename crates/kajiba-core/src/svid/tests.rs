//! Fixtures in tests/fixtures/svid: a test CA (`ca.pem`) and another one (`other-ca.pem`), each
//! leaf with its key, ECDSA P-256, valid for 100 years, made once with `openssl req`/`x509`.

use super::*;
use rustls::{ClientConnection, Connection, ServerConnection};
use std::path::Path;

const FORGE: &str = "spiffe://example.org/forge/kajiba-server";
const RUNNER: &str = "spiffe://example.org/build-1/kajiba-runner";

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/svid")
        .join(name)
}

fn files(dir: &Path, leaf: &str, bundle: &str) -> Files {
    Files {
        trust_domain: "example.org".into(),
        svid_file: dir.join(format!("{leaf}.pem")),
        key_file: dir.join(format!("{leaf}.key")),
        bundle_file: dir.join(bundle),
    }
}

fn source(leaf: &str) -> Arc<Source> {
    Source::load(files(&fixture(""), leaf, "ca.pem")).unwrap()
}

fn leaf(name: &str) -> CertificateDer<'static> {
    CertificateDer::from_pem_file(fixture(&format!("{name}.pem"))).unwrap()
}

/// Runs a handshake in memory (no socket); the first error of either side ends it.
fn handshake(
    client: ClientConfig,
    server: ServerConfig,
) -> Result<ServerConnection, rustls::Error> {
    let name = ServerName::try_from("runners.example.org").unwrap();
    let mut client = Connection::Client(ClientConnection::new(Arc::new(client), name)?);
    let mut server = Connection::Server(ServerConnection::new(Arc::new(server))?);
    fn pump(from: &mut Connection, to: &mut Connection) -> Result<bool, rustls::Error> {
        let mut bytes = Vec::new();
        while from.wants_write() {
            from.write_tls(&mut bytes).unwrap();
        }
        let mut rest = &bytes[..];
        while !rest.is_empty() {
            to.read_tls(&mut rest).unwrap();
            to.process_new_packets()?;
        }
        Ok(!bytes.is_empty())
    }
    while client.is_handshaking() || server.is_handshaking() {
        let sent = pump(&mut client, &mut server)?;
        if !pump(&mut server, &mut client)? && !sent {
            panic!("handshake stalled");
        }
    }
    match server {
        Connection::Server(server) => Ok(server),
        Connection::Client(_) => unreachable!(),
    }
}

#[test]
fn ids_follow_the_spiffe_rules() {
    assert!(check_id(RUNNER, "example.org").is_ok());
    assert!(check_id("spiffe://example.org/h_1.a-b/S", "example.org").is_ok());
    for bad in [
        "spiffe://other.org/build-1/kajiba-runner",
        "spiffe://example.org:443/build-1/kajiba-runner",
        "spiffe://user@example.org/build-1/kajiba-runner",
        "https://example.org/build-1/kajiba-runner",
        "spiffe://example.org/build-1",
        "spiffe://example.org/build-1/kajiba-runner/x",
        "spiffe://example.org//kajiba-runner",
        "spiffe://example.org/../kajiba-runner",
        "spiffe://example.org/build-1/kajiba-runner?x",
        "spiffe://example.org/build-1/kajiba-runner#x",
        "spiffe://example.org/build%31/kajiba-runner",
        "spiffe://example.org/build-1/kajiba-runner/",
    ] {
        assert!(check_id(bad, "example.org").is_err(), "{bad}");
    }
    assert!(check_trust_domain("example.org").is_ok());
    for bad in ["", "Example.org", "example.org:1", "a/b"] {
        assert!(check_trust_domain(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn reconnects_verify_current_svids_and_bundles_without_resumption() {
    // Exercise each side's policy even when its peer permits resumption.
    for server_policy in [true, false] {
        for revoke_on_server in [true, false] {
            let dir = std::env::temp_dir().join(format!("kj-svid-{}", crate::new_id().unwrap()));
            std::fs::create_dir(&dir).unwrap();
            let put = |from: &str, to: &str| {
                let staged = dir.join("staged");
                std::fs::copy(fixture(from), &staged).unwrap();
                std::fs::rename(&staged, dir.join(to)).unwrap();
            };
            for name in ["runner.pem", "runner.key", "forge.pem", "forge.key"] {
                put(name, name);
            }
            put("ca.pem", "runner-bundle.pem");
            put("ca.pem", "forge-bundle.pem");
            let runner = Source::load(files(&dir, "runner", "runner-bundle.pem")).unwrap();
            let forge = Source::load(files(&dir, "forge", "forge-bundle.pem")).unwrap();
            let mut client = runner.client_config(FORGE);
            let mut server = forge.server_config();
            if server_policy {
                client.resumption = rustls::client::Resumption::default();
            } else {
                server.session_storage = rustls::server::ServerSessionMemoryCache::new(256);
                server.send_tls13_tickets = 2;
            }
            handshake(client.clone(), server.clone()).unwrap();

            put("renewed.pem", "runner.pem");
            put("renewed.key", "runner.key");
            let reconnected = handshake(client.clone(), server.clone()).unwrap();
            assert_eq!(
                reconnected.handshake_kind(),
                Some(rustls::HandshakeKind::Full)
            );
            assert_eq!(reconnected.peer_certificates().unwrap()[0], leaf("renewed"));

            put(
                "other-ca.pem",
                if revoke_on_server {
                    "forge-bundle.pem"
                } else {
                    "runner-bundle.pem"
                },
            );
            assert!(handshake(client, server).is_err());
            std::fs::remove_dir_all(&dir).unwrap();
        }
    }
}

#[test]
fn mutual_handshake_yields_the_runner_identity() {
    let forge = source("forge");
    let runner = source("runner");
    let server = handshake(runner.client_config(FORGE), forge.server_config()).unwrap();
    assert_eq!(server.alpn_protocol(), Some(&b"h2"[..]));
    let peer = peer(&server.peer_certificates().unwrap()[0], "example.org").unwrap();
    assert_eq!(peer.id, RUNNER);
    assert!(peer.not_after > SystemTime::now());
}

#[test]
fn handshakes_refuse_the_wrong_peer() {
    let forge = source("forge");
    // A runner whose SVID another CA signed.
    assert!(
        handshake(
            source("foreign").client_config(FORGE),
            forge.server_config()
        )
        .is_err()
    );
    // A forge whose ID is not the expected one.
    assert!(
        handshake(
            source("runner").client_config(RUNNER),
            forge.server_config()
        )
        .is_err()
    );
    // A forge without serverAuth.
    assert!(
        handshake(
            source("runner").client_config(FORGE),
            source("client-only").server_config()
        )
        .is_err()
    );
}

#[test]
fn runner_certificates_are_verified_against_the_bundle_and_the_spiffe_rules() {
    let verifier = Verifier(source("forge"), None);
    let verify = |name: &str, now: UnixTime| verifier.verify_client_cert(&leaf(name), &[], now);
    assert!(verify("runner", UnixTime::now()).is_ok());
    for bad in ["foreign", "ca-leaf", "two-uris", "other-domain"] {
        assert!(verify(bad, UnixTime::now()).is_err(), "{bad}");
    }
    let expired = UnixTime::since_unix_epoch(Duration::from_secs(200 * 365 * 86_400));
    assert!(verify("runner", expired).is_err());
}

#[test]
fn the_forge_certificate_must_carry_server_auth_and_the_expected_id() {
    let verifier = Verifier(source("runner"), Some(FORGE.into()));
    let name = ServerName::try_from("runners.example.org").unwrap();
    let verify = |leaf_name: &str| {
        verifier.verify_server_cert(&leaf(leaf_name), &[], &name, &[], UnixTime::now())
    };
    assert!(verify("forge").is_ok());
    for bad in [
        "client-only",
        "runner",
        "foreign",
        "ca-leaf",
        "other-domain",
    ] {
        assert!(verify(bad).is_err(), "{bad}");
    }
}

#[test]
fn own_material_must_load_and_match() {
    for bad in ["ca-leaf", "two-uris", "other-domain"] {
        assert!(
            Source::load(files(&fixture(""), bad, "ca.pem")).is_err(),
            "{bad}"
        );
    }
    let mut mismatched = files(&fixture(""), "runner", "ca.pem");
    mismatched.key_file = fixture("renewed.key");
    assert!(Source::load(mismatched).is_err());
    assert!(Source::load(files(&fixture(""), "runner", "missing.pem")).is_err());
}

#[test]
fn renewed_files_are_picked_up_and_a_broken_pair_keeps_the_previous_one() {
    let dir = std::env::temp_dir().join(format!("kj-svid-{}", crate::new_id().unwrap()));
    std::fs::create_dir(&dir).unwrap();
    // Replaced as the CA does: a new file renamed over the old one.
    let put = |from: &str, to: &str| {
        let staged = dir.join("staged");
        std::fs::copy(fixture(from), &staged).unwrap();
        std::fs::rename(&staged, dir.join(to)).unwrap();
    };
    put("runner.pem", "svid.pem");
    put("runner.key", "svid.key");
    put("ca.pem", "bundle.pem");
    let source = Source::load(files(&dir, "svid", "bundle.pem")).unwrap();
    let served = |s: &Source| s.refresh().key.cert[0].clone();
    assert_eq!(served(&source), leaf("runner"));

    // Caught between two renames: the new SVID with the old key.
    put("renewed.pem", "svid.pem");
    assert_eq!(served(&source), leaf("runner"));
    assert!(source.lock().failed.is_some());

    put("renewed.key", "svid.key");
    assert_eq!(served(&source), leaf("renewed"));
    assert!(source.lock().failed.is_none());

    std::fs::remove_file(dir.join("bundle.pem")).unwrap();
    assert_eq!(served(&source), leaf("renewed"));
    put("other-ca.pem", "bundle.pem");
    assert_eq!(served(&source), leaf("renewed"));
    // The new bundle verifies new handshakes: the forge's SVID no longer does.
    let name = ServerName::try_from("runners.example.org").unwrap();
    assert!(
        Verifier(source.clone(), Some(FORGE.into()))
            .verify_server_cert(&leaf("forge"), &[], &name, &[], UnixTime::now())
            .is_err()
    );
    std::fs::remove_dir_all(&dir).unwrap();
}
