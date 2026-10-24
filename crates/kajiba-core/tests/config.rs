use std::{path::Path, time::Duration};

use kajiba_core::config::{Backend, Role, RunnerConfig, Scope, ServerConfig, Visibility};

const SERVER: &str = include_str!("fixtures/server.toml");
const RUNNER: &str = include_str!("fixtures/runner.toml");
// ADR 0026's repository fields, declared on top of the ADR 0019 example.
const REPOS: &str = r#"
[users.bob]

[repos.demo]
owner = "Alice"
name = "Demo"
description = "A demonstration repository"
visibility = "public"
default_branch = "trunk"
ci_enabled = true
previous_names = ["alice/old-demo", "Bob/Demo"]
permissions = { bob = "write", ALICE = "admin" }

[[repos.demo.mirrors]]
remote_url = "https://github.com/example/demo.git"
credential = "GitHub"

[[repos.demo.mirrors]]
remote_url = "https://github.com/example/demo-copy.git"
credential = "github"
enabled = false

[repos.tools]
owner = "bob"
description = ""
"#;
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
        hash.to_ascii_uppercase(),
    ] {
        server_error(
            &SERVER.replace(hash, &value),
            "users.alice.tokens.ci.sha256",
        );
    }
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
        let input = SERVER.replace(
            &format!("[{table}.{name}]"),
            &format!("[{table}.\"bad/name\"]"),
        );
        server_error(&input, table);
        // A rejected name may be a misplaced secret: never echoed.
        assert!(
            !ServerConfig::parse(&input)
                .unwrap_err()
                .contains("bad/name")
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
        let config = ServerConfig::parse(&format!(
            "{MINIMAL}\n[mirror]\nreconcile_interval='{value}'"
        ))
        .unwrap();
        assert_eq!(
            config.mirror.reconcile_interval,
            Duration::from_secs(seconds)
        );
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
fn auth_ttls_are_bounded() {
    for (field, accepted, refused) in [
        (
            "enrollment_ttl",
            ["1m", "60s", "24h", "1d"],
            ["0s", "59s", "86401s", "2d"],
        ),
        (
            "session_ttl",
            ["1m", "7d", "90d", "2160h"],
            ["0s", "59s", "91d", "7776001s"],
        ),
    ] {
        for value in accepted {
            assert!(
                ServerConfig::parse(&format!("{MINIMAL}\n[auth]\n{field}='{value}'")).is_ok(),
                "{field}={value}"
            );
        }
        for value in refused {
            server_error(
                &format!("{MINIMAL}\n[auth]\n{field}='{value}'"),
                &format!("auth.{field}: must be between 1m and"),
            );
        }
    }
}

#[test]
fn rejected_values_are_not_echoed() {
    let error =
        ServerConfig::parse(&SERVER.replace("\"ci:trigger\"", "\"DO-NOT-PRINT\"")).unwrap_err();
    assert!(error.contains("users.alice.tokens.ci.scopes"), "{error}");
    assert!(error.contains("expected one of"), "{error}");
    assert!(!error.contains("DO-NOT-PRINT"), "{error}");
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
    for value in [
        "https://forge.example.org/x",
        "https://forge.example.org/x/",
        "https://forge.example.org?y",
        "https://forge.example.org/?",
    ] {
        server_error(
            &SERVER.replace("https://forge.example.org", value),
            "http.public_url",
        );
    }
    ServerConfig::parse(&MINIMAL.replace("example.org", "example.org/")).unwrap();
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
            // TOML escapes: a newline/CR/DEL in the comment must never reach the store.
            format!("{original} a\\nssh-ed25519 evil").as_str(),
            format!("{original} a\\rb").as_str(),
            format!("{original} a\\u007Fb").as_str(),
        ] {
            let error = ServerConfig::parse(&SERVER.replace(original, value)).unwrap_err();
            assert!(error.contains(&format!("users.alice.{kind}[0]")), "{error}");
            assert!(!error.contains(value));
        }
    }
}

struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("kajiba-core-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn private_file(&self, name: &str) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = self.0.join(name);
        std::fs::write(&path, "DO-NOT-PRINT").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn secret_files_must_not_be_accessible_to_others_and_symlinks_are_followed() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let dir = TempDir::new("modes");
    let secret = dir.private_file("secret");
    // A secret manager links a stable path to the decrypted file.
    let link = dir.0.join("link");
    symlink(&secret, &link).unwrap();
    let runner = |path: &Path| {
        RunnerConfig::parse(
            &RUNNER.replace("/run/secrets/kajiba-runner-build-1", path.to_str().unwrap()),
        )
        .unwrap()
        .check_files()
    };
    for mode in [0o600, 0o400, 0o640, 0o660] {
        std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(mode)).unwrap();
        assert!(runner(&secret).is_ok(), "{mode:o}");
        assert!(runner(&link).is_ok(), "{mode:o}");
    }
    for mode in [0o604, 0o602, 0o644, 0o666] {
        std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(mode)).unwrap();
        for path in [&secret, &link] {
            let error = runner(path).unwrap_err();
            assert!(error.contains("token_file"), "{error}");
            assert!(error.contains("readable or writable by others"), "{error}");
            assert!(!error.contains("DO-NOT-PRINT"), "{error}");
        }
    }
}

#[test]
fn config_reads_are_bounded_and_refuse_special_files_without_blocking() {
    use kajiba_core::config::{MAX_CONFIG_BYTES, read_config};
    let dir = TempDir::new("read");
    let path = dir.0.join("server.toml");
    std::fs::write(&path, MINIMAL).unwrap();
    assert_eq!(read_config(&path).unwrap(), MINIMAL);
    std::fs::write(&path, " ".repeat(MAX_CONFIG_BYTES as usize)).unwrap();
    assert!(read_config(&path).is_ok());
    std::fs::write(&path, " ".repeat(MAX_CONFIG_BYTES as usize + 1)).unwrap();
    assert!(read_config(&path).unwrap_err().contains("larger than"));
    assert!(
        read_config(&dir.0)
            .unwrap_err()
            .contains("expected a regular file")
    );
    // Opening a FIFO with no writer would block forever without O_NONBLOCK.
    let fifo = dir.0.join("fifo");
    let c_path = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
    assert!(
        read_config(&fifo)
            .unwrap_err()
            .contains("expected a regular file")
    );
    let runner = RunnerConfig::parse(
        &RUNNER.replace("/run/secrets/kajiba-runner-build-1", fifo.to_str().unwrap()),
    )
    .unwrap();
    assert!(
        runner
            .check_files()
            .unwrap_err()
            .contains("expected a regular file")
    );
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
    let dir = TempDir::new("files");
    let existing = dir.private_file("secret");
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

#[test]
fn token_expiry_converts_offsets_leap_days_and_subseconds_to_epoch_millis() {
    use kajiba_core::config::TokenConfig;
    let mut token = TokenConfig {
        sha256: "00".repeat(32),
        scopes: vec![],
        expires: None,
    };
    assert_eq!(token.expires_at_millis().unwrap(), None);
    for (expires, expected) in [
        ("1970-01-01T00:00:00Z", 0),
        ("1969-12-31T23:59:59.999999999Z", -1),
        ("1970-01-01t01:00:00+01:00", 0),
        ("1969-12-31T23:00:00-01:00", 0),
        ("2000-02-29T00:00:00Z", 951_782_400_000),
        ("1900-03-01T00:00:00Z", -2_203_891_200_000),
        ("2100-03-01T00:00:00Z", 4_107_542_400_000),
        ("2028-02-29T01:02:03.456789+01:00", 1_835_395_323_456),
    ] {
        token.expires = Some(expires.into());
        assert_eq!(
            token.expires_at_millis().unwrap(),
            Some(expected),
            "{expires}"
        );
    }
    for invalid in ["bad", "2028-02-29", "2028-02-29T01:02:03"] {
        token.expires = Some(invalid.into());
        assert!(token.expires_at_millis().is_err(), "{invalid}");
    }
}

#[test]
fn credentials_unique_in_the_schema_are_refused_naming_both_fields() {
    const KEY: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAINdamAGCsQq31Uv+08lkBzoO4XLz2qYjJa8CGmj3B1Ea";
    let hash = "ab".repeat(32);
    for (input, field, first) in [
        (
            format!(
                "[users.alice]\nssh_keys=['{KEY}']\n[users.bob]\nssh_keys=['{KEY} bob@example.org']"
            ),
            "users.bob.ssh_keys[0]",
            "users.alice.ssh_keys[0]",
        ),
        (
            format!("[users.alice]\nsigning_keys=['{KEY}']\n[users.bob]\nsigning_keys=['{KEY}']"),
            "users.bob.signing_keys[0]",
            "users.alice.signing_keys[0]",
        ),
        (
            format!(
                "[users.alice.tokens.one]\nsha256='{hash}'\nscopes=[]\n[users.alice.tokens.two]\nsha256='{hash}'\nscopes=[]"
            ),
            "users.alice.tokens.two.sha256",
            "users.alice.tokens.one.sha256",
        ),
        (
            format!(
                "[users.alice.tokens.one]\nsha256='{hash}'\nscopes=[]\n[users.bob.tokens.one]\nsha256='{hash}'\nscopes=[]"
            ),
            "users.bob.tokens.one.sha256",
            "users.alice.tokens.one.sha256",
        ),
    ] {
        let error = ServerConfig::parse(&format!("{MINIMAL}{input}")).unwrap_err();
        assert!(error.contains(field) && error.contains(first), "{error}");
    }
    // The same key for authentication and signing, or listed twice by one user, is allowed.
    for input in [
        format!("[users.alice]\nssh_keys=['{KEY}']\n[users.bob]\nsigning_keys=['{KEY}']"),
        format!("[users.alice]\nssh_keys=['{KEY}', '{KEY} again']"),
    ] {
        ServerConfig::parse(&format!("{MINIMAL}{input}")).unwrap();
    }
}

#[test]
fn a_full_repository_example_loads_with_defaults_and_normalized_names() {
    let config = ServerConfig::parse(&format!("{SERVER}{REPOS}")).unwrap();
    let demo = &config.repos["demo"];
    assert_eq!((demo.owner.as_str(), demo.name.as_str()), ("alice", "demo"));
    assert_eq!(demo.description, "A demonstration repository");
    assert_eq!(demo.visibility, Visibility::Public);
    assert_eq!(demo.default_branch, "trunk");
    assert!(demo.ci_enabled);
    assert_eq!(demo.previous_names, ["alice/old-demo", "bob/demo"]);
    assert_eq!(demo.permissions["bob"], Role::Write);
    assert_eq!(demo.permissions["alice"], Role::Admin);
    assert_eq!(demo.mirrors.len(), 2);
    assert_eq!(demo.mirrors[0].credential, "github");
    assert!(demo.mirrors[0].enabled);
    assert!(!demo.mirrors[1].enabled);
    let tools = &config.repos["tools"];
    assert_eq!(tools.name, "tools");
    assert_eq!(tools.visibility, Visibility::Public);
    assert_eq!(tools.default_branch, "main");
    assert!(!tools.ci_enabled);
    assert!(tools.previous_names.is_empty() && tools.permissions.is_empty());
    assert!(tools.mirrors.is_empty());
    assert!(ServerConfig::parse(MINIMAL).unwrap().repos.is_empty());
    for header in ["[repos.tools]", "[[repos.demo.mirrors]]"] {
        let input =
            format!("{SERVER}{REPOS}").replacen(header, &format!("{header}\ntypo = true"), 1);
        server_error(&input, "typo");
    }
}

#[test]
fn each_repository_refusal_names_its_field() {
    let full = format!("{SERVER}{REPOS}");
    for (from, to, field) in [
        (
            "owner = \"bob\"",
            "owner = \"carol\"",
            "repos.tools.owner: not a declared user",
        ),
        ("owner = \"bob\"", "owner = \"b/c\"", "repos.tools.owner"),
        (
            "bob = \"write\"",
            "carol = \"write\"",
            "repos.demo.permissions.carol: not a declared user",
        ),
        ("bob = \"write\"", "bob = \"owner\"", "permissions"),
        (
            "owner = \"bob\"",
            "owner = \"alice\"\nname = \"DEMO\"",
            "repos.tools.name: same current owner and name as repos.demo",
        ),
        (
            "owner = \"bob\"",
            "owner = \"bob\"\nname = \"\"",
            "name must contain",
        ),
        (
            "owner = \"bob\"",
            "owner = \"bob\"\nname = \"raw\"",
            "repos.tools.name",
        ),
        (
            "\"Bob/Demo\"",
            "\"Bob/Tools\"",
            "repos.demo.previous_names[1]: current name of repos.tools",
        ),
        (
            "\"Bob/Demo\"",
            "\"alice/OLD-demo\"",
            "repos.demo.previous_names[1]: already declared by repos.demo.previous_names[0]",
        ),
        (
            "owner = \"bob\"",
            "owner = \"bob\"\nprevious_names = [\"alice/old-demo\"]",
            "repos.tools.previous_names[0]: already declared by repos.demo.previous_names[0]",
        ),
        (
            "\"Bob/Demo\"",
            "\"bob\"",
            "repos.demo.previous_names[1]: expected <owner>/<name>",
        ),
        (
            "\"Bob/Demo\"",
            "\"bob/a/b\"",
            "repos.demo.previous_names[1]: expected <owner>/<name>",
        ),
        (
            "credential = \"GitHub\"",
            "credential = \"gitlab\"",
            "repos.demo.mirrors[0].credential: not a declared mirror credential",
        ),
        (
            "demo-copy.git",
            "demo.git",
            "repos.demo.mirrors[1].remote_url: already declared by repos.demo.mirrors[0].remote_url",
        ),
        (
            "visibility = \"public\"",
            "visibility = \"private\"",
            "visibility",
        ),
        (
            "[repos.tools]",
            "[repos.\"Demo\"]",
            "duplicate name after lowercase normalization",
        ),
        ("[repos.tools]", "[repos.raw]", "repos: name #"),
        ("description = \"\"\n", "", "description"),
        ("owner = \"bob\"\n", "", "owner"),
    ] {
        assert!(full.contains(from), "{from}");
        server_error(&full.replacen(from, to, 1), field);
    }
}
