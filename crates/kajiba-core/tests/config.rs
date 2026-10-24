use std::{path::Path, time::Duration};

use kajiba_core::config::{Backend, RunnerConfig, Scope, ServerConfig};

const SERVER: &str = include_str!("fixtures/server.toml");
const RUNNER: &str = include_str!("fixtures/runner.toml");
const MINIMAL: &str = "[http]\npublic_url = 'https://forge.example.org'\n";

fn server_error(input: &str, field: &str) {
    let error = ServerConfig::parse(input).unwrap_err();
    assert!(error.contains(field), "expected {field:?} in {error:?}");
}

fn runner_error(input: &str, field: &str) {
    let error = RunnerConfig::parse(input).unwrap_err();
    assert!(error.contains(field), "expected {field:?} in {error:?}");
}

#[test]
fn adr_examples_load_verbatim() {
    let server = ServerConfig::parse(SERVER).unwrap();
    assert!(server.users["alice"].is_admin);
    assert_eq!(
        server.users["alice"].tokens["ci"].scopes,
        [Scope::RepoRead, Scope::CiTrigger]
    );
    assert_eq!(server.ssh.host.as_deref(), Some("forge.example.org"));
    assert_eq!(server.metrics.listen.unwrap().port(), 9431);
    let runner = RunnerConfig::parse(RUNNER).unwrap();
    assert_eq!(runner.name, "build-1");
    assert_eq!(runner.backends, [Backend::Nix]);
    assert_eq!(runner.metrics.listen.unwrap().port(), 9432);
}

#[test]
fn defaults_match_adr_tables() {
    let server = ServerConfig::parse(MINIMAL).unwrap();
    assert_eq!(server.state_dir, Path::new("/var/lib/kajiba"));
    assert_eq!(server.http.listen.to_string(), "127.0.0.1:8080");
    assert!(!server.http.reflection);
    assert_eq!(server.ssh.host.as_deref(), Some("forge.example.org"));
    assert_eq!(server.ssh.port, 2222);
    assert_eq!(server.log.level, "info");
    assert!(server.metrics.listen.is_none());
    assert_eq!(server.auth.session_ttl, Duration::from_secs(7 * 86400));
    assert_eq!(server.auth.enrollment_ttl, Duration::from_secs(15 * 60));
    assert!(server.users.is_empty());
    assert!(server.runners.is_empty());
    assert!(server.mirror.credentials.is_empty());
    assert_eq!(server.mirror.reconcile_interval, Duration::from_secs(3600));
    assert_eq!(server.ci.log_retention, Duration::from_secs(14 * 86400));
    assert_eq!(
        server.ci.artifact_retention,
        Duration::from_secs(14 * 86400)
    );
    assert_eq!(server.dump.dir, Path::new("/var/lib/kajiba-dumps"));
    assert_eq!(server.dump.keep, 7);
    let user = ServerConfig::parse(&format!("{MINIMAL}\n[users.alice]")).unwrap();
    let user = &user.users["alice"];
    assert!(!user.is_admin);
    assert!(user.ssh_keys.is_empty() && user.signing_keys.is_empty() && user.tokens.is_empty());
    let runner =
        RunnerConfig::parse("name='build-1'\nurl='https://example.org'\ntoken_file='/missing'")
            .unwrap();
    assert_eq!(runner.backends, [Backend::Nix]);
    assert!(runner.tags.is_empty());
    assert_eq!(runner.max_jobs, 1);
    assert_eq!(runner.state_dir, Path::new("/var/lib/kajiba-runner"));
    assert_eq!(runner.log.level, "info");
    assert!(runner.metrics.listen.is_none());
}

#[test]
fn unknown_fields_are_rejected_at_every_table() {
    for table in [
        "",
        "http",
        "ssh",
        "log",
        "metrics",
        "auth",
        "users.alice",
        "users.alice.tokens.ci",
        "runners.build-1",
        "mirror",
        "mirror.credentials.github",
        "ci",
        "dump",
    ] {
        let extra = if table.is_empty() {
            "typo = true\n".to_owned()
        } else {
            format!("[{table}]\ntypo = true\n")
        };
        // Insert into existing tables instead of accidentally testing TOML's duplicate-table refusal.
        let input = if table.is_empty() {
            format!("{extra}{SERVER}")
        } else if SERVER.contains(&format!("[{table}]")) {
            SERVER.replace(&format!("[{table}]"), &extra)
        } else {
            format!("{SERVER}\n{extra}")
        };
        server_error(&input, "typo");
    }
    for input in [
        format!("typo=true\n{RUNNER}"),
        format!("{RUNNER}\n[log.extra]\ntypo=true"),
    ] {
        runner_error(
            &input,
            if input.starts_with("typo") {
                "typo"
            } else {
                "extra"
            },
        );
    }
}

#[test]
fn inline_secrets_name_the_file_alternative_without_echoing_the_value() {
    for secret in ["token", "secret", "password", "key"] {
        for input in [
            format!("{secret}='DO-NOT-PRINT'\n{SERVER}"),
            SERVER.replace(
                "[runners.build-1]",
                &format!("[runners.build-1]\n{secret}='DO-NOT-PRINT'"),
            ),
        ] {
            let error = ServerConfig::parse(&input).unwrap_err();
            assert!(error.contains(&format!("{secret}_file")), "{error}");
            assert!(!error.contains("DO-NOT-PRINT"), "{error}");
        }
        let error = RunnerConfig::parse(&format!("{secret}='DO-NOT-PRINT'\n{RUNNER}")).unwrap_err();
        assert!(error.contains(&format!("{secret}_file")), "{error}");
        assert!(!error.contains("DO-NOT-PRINT"));
    }
}

#[test]
fn metrics_require_literal_loopback_addresses() {
    for addr in [
        "0.0.0.0:9431",
        "192.0.2.1:9431",
        "[::]:9431",
        "[2001:db8::1]:9431",
        "localhost:9431",
        "127.0.0.1:65536",
    ] {
        server_error(&SERVER.replace("127.0.0.1:9431", addr), "metrics.listen");
        runner_error(&RUNNER.replace("127.0.0.1:9432", addr), "metrics.listen");
    }
    for addr in ["127.0.0.2:9431", "[::1]:9431"] {
        assert!(ServerConfig::parse(&SERVER.replace("127.0.0.1:9431", addr)).is_ok());
        assert!(RunnerConfig::parse(&RUNNER.replace("127.0.0.1:9432", addr)).is_ok());
    }
}

#[test]
fn hashes_scopes_and_expirations_are_validated() {
    let hash = "9f2c1e0a4b7d3f8e6a5c2b1d0e9f8a7b6c5d4e3f2a1b0c9d8e7f6a5b4c3d2e1f";
    for value in [
        "".to_owned(),
        "a".repeat(63),
        "a".repeat(65),
        "g".repeat(64),
        "é".repeat(32),
    ] {
        server_error(
            &SERVER.replace(hash, &value),
            "users.alice.tokens.ci.sha256",
        );
    }
    assert!(ServerConfig::parse(&SERVER.replace(hash, &hash.to_ascii_uppercase())).is_ok());
    server_error(
        &SERVER.replace("repo:read", "repo:unknown"),
        "users.alice.tokens.ci.scopes",
    );
    for value in [
        "2026-10-24",
        "2026-10-24T00:00:00",
        "2026-13-24T00:00:00Z",
        "2026-10-24 00:00:00Z",
        "garbage",
    ] {
        server_error(
            &SERVER.replace(
                "[users.alice.tokens.ci]",
                &format!("[users.alice.tokens.ci]\nexpires='{value}'"),
            ),
            "users.alice.tokens.ci.expires",
        );
    }
    for value in ["2026-10-24T00:00:00Z", "2026-10-24T12:34:56.123+02:00"] {
        assert!(
            ServerConfig::parse(&SERVER.replace(
                "[users.alice.tokens.ci]",
                &format!("[users.alice.tokens.ci]\nexpires='{value}'")
            ))
            .is_ok()
        );
    }
}

#[test]
fn all_identity_names_use_shared_validation_and_normalization() {
    for (table, name) in [
        ("users", "alice"),
        ("runners", "build-1"),
        ("mirror.credentials", "github"),
        ("users.alice.tokens", "ci"),
    ] {
        server_error(
            &SERVER.replace(
                &format!("[{table}.{name}]"),
                &format!("[{table}.\"bad/name\"]"),
            ),
            table,
        );
        let normalized = SERVER.replace(
            &format!("{table}.{name}"),
            &format!("{table}.{}", name.to_ascii_uppercase()),
        );
        assert!(ServerConfig::parse(&normalized).is_ok(), "{normalized}");
    }
    let upper = SERVER.replace("users.alice", "users.ALICE");
    assert!(
        ServerConfig::parse(&upper)
            .unwrap()
            .users
            .contains_key("alice")
    );
    let duplicate = format!("{SERVER}\n[users.ALICE]\n");
    let error = ServerConfig::parse(&duplicate).unwrap_err();
    assert!(
        error.contains("users.") && error.contains("duplicate name"),
        "{error}"
    );
    runner_error(&RUNNER.replace("build-1", "bad/name"), "name");
    assert_eq!(
        RunnerConfig::parse(&RUNNER.replace("build-1", "BUILD-1"))
            .unwrap()
            .name,
        "build-1"
    );
}

#[test]
fn durations_accept_each_unit_and_reject_bad_or_overflowing_values() {
    for (value, seconds) in [("2s", 2), ("2m", 120), ("2h", 7200), ("2d", 172800)] {
        let config =
            ServerConfig::parse(&format!("{MINIMAL}\n[auth]\nsession_ttl='{value}'")).unwrap();
        assert_eq!(config.auth.session_ttl, Duration::from_secs(seconds));
        assert_eq!(config.auth.enrollment_ttl, Duration::from_secs(900));
    }
    for value in [
        "",
        "s",
        "-1s",
        "+1s",
        "1.5h",
        "1ms",
        "1w",
        " 1s",
        "1 s",
        "1é",
        "18446744073709551616s",
        "18446744073709551615d",
    ] {
        server_error(
            &format!("{MINIMAL}\n[auth]\nsession_ttl='{value}'"),
            "auth.session_ttl",
        );
    }
}

#[test]
fn required_fields_urls_and_ranges_are_checked() {
    for (input, field) in [("", "http"), ("[http]", "public_url")] {
        server_error(input, field);
    }
    for field in ["name", "url", "token_file"] {
        let input = RUNNER
            .lines()
            .filter(|line| !line.starts_with(&format!("{field} =")))
            .collect::<Vec<_>>()
            .join("\n");
        runner_error(&input, field);
    }
    for value in [
        "example.org",
        "/relative",
        "ftp://example.org",
        "https://",
        "https://user:password@example.org",
        "https://example.org:65536",
        "https://[invalid]",
        "https://bad_host",
        "https://example.org/#fragment",
    ] {
        server_error(
            &SERVER.replace("https://forge.example.org", value),
            "http.public_url",
        );
        runner_error(&RUNNER.replace("https://forge.example.org", value), "url");
    }
    let ipv6 = ServerConfig::parse(&MINIMAL.replace("forge.example.org", "[::1]:8080")).unwrap();
    assert_eq!(ipv6.ssh.host.as_deref(), Some("[::1]"));
    server_error(&SERVER.replace("port = 2222", "port = 65536"), "ssh.port");
    server_error(&SERVER.replace("port = 2222", "port = 0"), "ssh.port");
    server_error(
        &SERVER.replace("[ssh]", "[ssh]\nhost = 'bad/host'"),
        "ssh.host",
    );
    runner_error(&RUNNER.replace("max_jobs = 1", "max_jobs = 0"), "max_jobs");
    runner_error(&RUNNER.replace("max_jobs = 1", "max_jobs = -1"), "max_jobs");
    runner_error(&RUNNER.replace("[\"nix\"]", "[\"unknown\"]"), "backends");
    server_error(&format!("{MINIMAL}\n[dump]\nkeep=-1"), "dump.keep");
    server_error(
        &SERVER.replace(
            "token_file = \"/run/secrets/kajiba-runner-build-1\"",
            "token_file = ''",
        ),
        "runners.build-1.token_file",
    );
}

#[test]
fn openssh_keys_parse_and_can_produce_sha256_fingerprints() {
    let server = ServerConfig::parse(SERVER).unwrap();
    let ed25519 = &server.users["alice"].ssh_keys[0];
    let sk = include_str!("fixtures/sk-ed25519.pub").trim();
    for (value, fingerprint) in [
        (
            ed25519.as_str(),
            "SHA256:bbXpuKG6zhzdmnxq256TlqzFBzRl2f6OOg722cYNbU8",
        ),
        (sk, "SHA256:eSRWwRIXOUcuLMrn+Ct3aWn2AWaVDybf1hqniqm1p2c"),
    ] {
        let config = SERVER.replace(ed25519, value);
        assert!(ServerConfig::parse(&config).is_ok());
        let key = ssh_key::PublicKey::from_openssh(value).unwrap();
        assert_eq!(
            key.fingerprint(ssh_key::HashAlg::Sha256).to_string(),
            fingerprint
        );
    }
    for kind in ["ssh_keys", "signing_keys"] {
        let original = if kind == "ssh_keys" {
            ed25519
        } else {
            &server.users["alice"].signing_keys[0]
        };
        for value in [
            "garbage",
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAA",
            "ssh-unknown AAAAC3NzaC1lZDI1NTE5AAAA",
            "ssh-ed25519 !!!!",
        ] {
            let error = ServerConfig::parse(&SERVER.replace(original, value)).unwrap_err();
            assert!(error.contains(&format!("users.alice.{kind}[0]")), "{error}");
            assert!(!error.contains(value));
        }
    }
}

#[test]
fn file_checks_open_every_secret_path_and_report_the_field() {
    // These paths deliberately do not exist: structural validation must never open them.
    let config =
        ServerConfig::parse(&SERVER.replace("/run/secrets/", "/kajiba-test-missing/")).unwrap();
    assert!(
        config
            .check_files()
            .unwrap_err()
            .contains("runners.build-1.token_file")
    );
    let existing = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/server.toml");
    let input = SERVER.replace(
        "/run/secrets/kajiba-runner-build-1",
        existing.to_str().unwrap(),
    );
    let config = ServerConfig::parse(&input).unwrap();
    assert!(
        config
            .check_files()
            .unwrap_err()
            .contains("mirror.credentials.github.token_file")
    );
    let input = input.replace(
        "/run/secrets/kajiba-github-token",
        existing.to_str().unwrap(),
    );
    assert!(ServerConfig::parse(&input).unwrap().check_files().is_ok());
    let runner = RUNNER.replace(
        "/run/secrets/kajiba-runner-build-1",
        existing.to_str().unwrap(),
    );
    assert!(RunnerConfig::parse(&runner).unwrap().check_files().is_ok());
    let runner = runner.replace(existing.to_str().unwrap(), env!("CARGO_MANIFEST_DIR"));
    assert!(
        RunnerConfig::parse(&runner)
            .unwrap()
            .check_files()
            .unwrap_err()
            .contains("token_file")
    );
}

#[test]
fn malformed_key_types_and_toml_do_not_echo_material() {
    let server = ServerConfig::parse(SERVER).unwrap();
    let key = &server.users["alice"].ssh_keys[0];
    let malformed = SERVER.replace(
        &format!("ssh_keys = [\"{key}\"]"),
        &format!("ssh_keys = \"{key}\""),
    );
    let error = ServerConfig::parse(&malformed).unwrap_err();
    assert!(error.contains("users.alice.ssh_keys"), "{error}");
    assert!(!error.contains(key), "{error}");
    let error = ServerConfig::parse("token='DO-NOT-PRINT").unwrap_err();
    assert!(error.contains("invalid TOML"));
    assert!(!error.contains("DO-NOT-PRINT"));
}

#[test]
fn declared_credentials_and_tokens_require_their_fields() {
    for field in ["sha256", "scopes"] {
        let input = SERVER
            .lines()
            .filter(|line| !line.starts_with(&format!("{field} =")))
            .collect::<Vec<_>>()
            .join("\n");
        server_error(&input, field);
    }
    for table in ["runners.build-1", "mirror.credentials.github"] {
        server_error(&format!("{MINIMAL}\n[{table}]"), "token_file");
    }
}

#[test]
fn log_levels_validate_target_directives_for_both_components() {
    for level in ["info", "warn,kajiba_server=debug", "off"] {
        let section = format!("\n[log]\nlevel={level:?}\n");
        assert!(
            ServerConfig::parse(&format!(
                "[http]\npublic_url='https://example.org'\n{section}"
            ))
            .is_ok()
        );
        assert!(RunnerConfig::parse(&format!("name='build-1'\nurl='https://example.org'\ntoken_file='/run/secrets/token'\n{section}")).is_ok());
    }
    for input in [
        "[http]\npublic_url='https://example.org'\n[log]\nlevel='kajiba=verbose'",
        "name='build-1'\nurl='https://example.org'\ntoken_file='/run/secrets/token'\n[log]\nlevel='kajiba=verbose'",
    ] {
        let error = if input.starts_with("[http]") {
            ServerConfig::parse(input).unwrap_err()
        } else {
            RunnerConfig::parse(input).unwrap_err()
        };
        assert!(error.contains("log.level"), "{error}");
    }
}
