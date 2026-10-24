//! TOML configuration shared by the server, runner and offline validation.

use std::{collections::BTreeMap, fs::File, net::SocketAddr, path::PathBuf, time::Duration};

use http::Uri;
use serde::{Deserialize, Deserializer, de::DeserializeOwned};

use crate::names::validate_name;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    #[serde(default = "server_state_dir")]
    pub state_dir: PathBuf,
    pub http: HttpConfig,
    #[serde(default)]
    pub ssh: SshConfig,
    #[serde(default)]
    pub log: LogConfig,
    #[serde(default)]
    pub metrics: MetricsConfig,
    #[serde(default)]
    pub auth: AuthConfig,
    #[serde(default)]
    pub users: BTreeMap<String, UserConfig>,
    #[serde(default)]
    pub runners: BTreeMap<String, CredentialConfig>,
    #[serde(default)]
    pub mirror: MirrorConfig,
    #[serde(default)]
    pub ci: CiConfig,
    #[serde(default)]
    pub dump: DumpConfig,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HttpConfig {
    #[serde(default = "http_listen")]
    pub listen: SocketAddr,
    pub public_url: String,
    #[serde(default)]
    pub reflection: bool,
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SshConfig {
    pub host: Option<String>,
    pub port: u16,
}

impl Default for SshConfig {
    fn default() -> Self {
        Self {
            host: None,
            port: 2222,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LogConfig {
    pub level: String,
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            level: "info".into(),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MetricsConfig {
    pub listen: Option<SocketAddr>,
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AuthConfig {
    #[serde(deserialize_with = "duration")]
    pub session_ttl: Duration,
    #[serde(deserialize_with = "duration")]
    pub enrollment_ttl: Duration,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            session_ttl: Duration::from_secs(7 * 86400),
            enrollment_ttl: Duration::from_secs(15 * 60),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UserConfig {
    pub is_admin: bool,
    pub ssh_keys: Vec<String>,
    pub signing_keys: Vec<String>,
    pub tokens: BTreeMap<String, TokenConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TokenConfig {
    pub sha256: String,
    pub scopes: Vec<Scope>,
    pub expires: Option<String>,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
pub enum Scope {
    #[serde(rename = "repo:read")]
    RepoRead,
    #[serde(rename = "repo:write")]
    RepoWrite,
    #[serde(rename = "ci:trigger")]
    CiTrigger,
    #[serde(rename = "admin")]
    Admin,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CredentialConfig {
    pub token_file: PathBuf,
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MirrorConfig {
    pub credentials: BTreeMap<String, CredentialConfig>,
    #[serde(deserialize_with = "duration")]
    pub reconcile_interval: Duration,
}

impl Default for MirrorConfig {
    fn default() -> Self {
        Self {
            credentials: BTreeMap::new(),
            reconcile_interval: Duration::from_secs(3600),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CiConfig {
    #[serde(deserialize_with = "duration")]
    pub log_retention: Duration,
    #[serde(deserialize_with = "duration")]
    pub artifact_retention: Duration,
}

impl Default for CiConfig {
    fn default() -> Self {
        Self {
            log_retention: Duration::from_secs(14 * 86400),
            artifact_retention: Duration::from_secs(14 * 86400),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DumpConfig {
    pub dir: PathBuf,
    pub keep: u32,
}

impl Default for DumpConfig {
    fn default() -> Self {
        Self {
            dir: "/var/lib/kajiba-dumps".into(),
            keep: 7,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunnerConfig {
    pub name: String,
    pub url: String,
    pub token_file: PathBuf,
    #[serde(default = "backends")]
    pub backends: Vec<Backend>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default = "max_jobs")]
    pub max_jobs: u32,
    #[serde(default = "runner_state_dir")]
    pub state_dir: PathBuf,
    #[serde(default)]
    pub log: LogConfig,
    #[serde(default)]
    pub metrics: MetricsConfig,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    Nix,
    Nspawn,
    Microvm,
    Process,
}

fn server_state_dir() -> PathBuf {
    "/var/lib/kajiba".into()
}
fn runner_state_dir() -> PathBuf {
    "/var/lib/kajiba-runner".into()
}
fn http_listen() -> SocketAddr {
    "127.0.0.1:8080".parse().unwrap()
}
fn backends() -> Vec<Backend> {
    vec![Backend::Nix]
}
fn max_jobs() -> u32 {
    1
}

fn duration<'de, D: Deserializer<'de>>(de: D) -> Result<Duration, D::Error> {
    let value = String::deserialize(de)?;
    let parsed = value.len().checked_sub(1).and_then(|end| {
        let (number, unit) = value.split_at_checked(end)?;
        let multiplier = match unit {
            "s" => 1,
            "m" => 60,
            "h" => 3600,
            "d" => 86400,
            _ => return None,
        };
        if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        number.parse::<u64>().ok()?.checked_mul(multiplier)
    });
    parsed
        .map(Duration::from_secs)
        .ok_or_else(|| serde::de::Error::custom("expected duration <n>s|m|h|d within u64 seconds"))
}

fn parse<T: DeserializeOwned>(input: &str) -> Result<T, String> {
    // Deserialize from a value so diagnostics never echo a source line containing a secret or key.
    let value: toml::Value = toml::from_str(input).map_err(|err| {
        let line = err
            .span()
            .map(|span| input[..span.start].bytes().filter(|b| *b == b'\n').count() + 1);
        format!("invalid TOML at line {}", line.unwrap_or(1))
    })?;
    value.try_into().map_err(|err: toml::de::Error| {
        let mut message = err.to_string();
        if err.message().starts_with("invalid type:") {
            if let Some((_, expected)) = err.message().rsplit_once(", expected ") {
                message =
                    message.replace(err.message(), &format!("invalid type, expected {expected}"));
            }
        }
        for secret in ["token", "secret", "password", "key"] {
            if err
                .message()
                .starts_with(&format!("unknown field `{secret}`"))
            {
                message.push_str(&format!("; use {secret}_file instead"));
            }
        }
        message
    })
}

fn url_host(value: &str, field: &str) -> Result<String, String> {
    let error =
        || format!("{field}: expected an absolute HTTP or HTTPS URL with a host and valid port");
    let uri: Uri = value.parse().map_err(|_| error())?;
    let authority = uri.authority().ok_or_else(error)?;
    let host = authority.host();
    if !matches!(uri.scheme_str(), Some("http" | "https"))
        || host.is_empty()
        || authority.as_str().contains('@')
        || value.contains('#')
        || (authority.as_str().len() > host.len() && authority.port_u16().is_none())
        || authority.port_u16() == Some(0)
    {
        return Err(error());
    }
    validate_host(host, field)?;
    Ok(host.to_ascii_lowercase())
}

fn validate_host(host: &str, field: &str) -> Result<(), String> {
    let ip = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host);
    if ip.parse::<std::net::IpAddr>().is_ok() {
        return Ok(());
    }
    if host.len() <= 253
        && host.trim_end_matches('.').split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
    {
        return Ok(());
    }
    Err(format!("{field}: invalid host"))
}

fn validate_metrics(metrics: &MetricsConfig) -> Result<(), String> {
    if let Some(addr) = metrics.listen {
        if !addr.ip().is_loopback() {
            return Err("metrics.listen: must be a loopback address".into());
        }
        if addr.port() == 0 {
            return Err("metrics.listen: port must be nonzero".into());
        }
    }
    Ok(())
}

fn normalize<T>(map: &mut BTreeMap<String, T>, field: &str) -> Result<(), String> {
    let mut normalized = BTreeMap::new();
    for (name, value) in std::mem::take(map) {
        let key = validate_name(&name).map_err(|err| format!("{field}.{name}: {err}"))?;
        if normalized.insert(key, value).is_some() {
            return Err(format!(
                "{field}.{name}: duplicate name after lowercase normalization"
            ));
        }
    }
    *map = normalized;
    Ok(())
}

fn validate_path(path: &std::path::Path, field: &str) -> Result<(), String> {
    if path.as_os_str().is_empty() {
        return Err(format!("{field}: path must not be empty"));
    }
    Ok(())
}

fn open_file(path: &std::path::Path, field: &str) -> Result<(), String> {
    let file = File::open(path).map_err(|err| format!("{field} ({}): {err}", path.display()))?;
    if !file
        .metadata()
        .map_err(|err| format!("{field} ({}): {err}", path.display()))?
        .is_file()
    {
        return Err(format!(
            "{field} ({}): expected a regular file",
            path.display()
        ));
    }
    Ok(())
}

impl ServerConfig {
    /// Parse and structurally validate without opening any referenced file.
    pub fn parse(input: &str) -> Result<Self, String> {
        let mut config: Self = parse(input)?;
        crate::log::filter(&config.log.level, None)?;
        let host = url_host(&config.http.public_url, "http.public_url")?;
        // Links are built as `<public_url>/auth/...`: a path or query would break them.
        let public_url = &config.http.public_url;
        if public_url.contains('?')
            || !matches!(
                public_url
                    .parse::<Uri>()
                    .map(|uri| uri.path().to_owned())
                    .as_deref(),
                Ok("" | "/")
            )
        {
            return Err("http.public_url: must not contain a path or query".into());
        }
        if let Some(host) = &config.ssh.host {
            validate_host(host, "ssh.host")?;
        } else {
            config.ssh.host = Some(host);
        }
        if config.ssh.port == 0 {
            return Err("ssh.port: must be nonzero".into());
        }
        if config.http.listen.port() == 0 {
            return Err("http.listen: port must be nonzero".into());
        }
        validate_metrics(&config.metrics)?;
        validate_path(&config.state_dir, "state_dir")?;
        validate_path(&config.dump.dir, "dump.dir")?;
        normalize(&mut config.users, "users")?;
        normalize(&mut config.runners, "runners")?;
        normalize(&mut config.mirror.credentials, "mirror.credentials")?;
        // The schema has UNIQUE(fingerprint, usage) and UNIQUE(token_hash): refuse here,
        // naming both fields, rather than fail at startup.
        let mut fingerprints = BTreeMap::new();
        let mut hashes = BTreeMap::new();
        for (name, user) in &mut config.users {
            for (kind, usage, keys) in [
                ("ssh_keys", "auth", &user.ssh_keys),
                ("signing_keys", "signing", &user.signing_keys),
            ] {
                for (index, key) in keys.iter().enumerate() {
                    let field = format!("users.{name}.{kind}[{index}]");
                    let fingerprint = parse_ssh_key(key)
                        .map_err(|err| format!("{field}: {err}"))?
                        .fingerprint(Default::default())
                        .to_string();
                    let (owner, first) = fingerprints
                        .entry((usage, fingerprint))
                        .or_insert_with(|| (name.clone(), field.clone()));
                    // One user listing a key twice is harmless; two users cannot share it.
                    if owner != name {
                        return Err(format!(
                            "{field}: key fingerprint already declared by {first}"
                        ));
                    }
                }
            }
            normalize(&mut user.tokens, &format!("users.{name}.tokens"))?;
            for (token_name, token) in &user.tokens {
                let field = format!("users.{name}.tokens.{token_name}");
                if token.sha256.len() != 64
                    || !token
                        .sha256
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                {
                    return Err(format!(
                        "{field}.sha256: expected 64 lowercase hexadecimal characters"
                    ));
                }
                if let Some(first) = hashes.insert(token.sha256.clone(), field.clone()) {
                    return Err(format!(
                        "{field}.sha256: hash already declared by {first}.sha256"
                    ));
                }
                if let Some(expires) = &token.expires {
                    let valid = expires
                        .parse::<toml::value::Datetime>()
                        .ok()
                        .is_some_and(|date| {
                            date.date.is_some()
                                && date.time.is_some()
                                && date.offset.is_some()
                                && matches!(expires.as_bytes().get(10), Some(b'T' | b't'))
                        });
                    if !valid {
                        return Err(format!(
                            "{field}.expires: expected RFC 3339 timestamp with offset"
                        ));
                    }
                }
            }
        }
        for (name, credential) in &config.runners {
            validate_path(
                &credential.token_file,
                &format!("runners.{name}.token_file"),
            )?;
        }
        for (name, credential) in &config.mirror.credentials {
            validate_path(
                &credential.token_file,
                &format!("mirror.credentials.{name}.token_file"),
            )?;
        }
        Ok(config)
    }

    /// Check access as the current user, without reading or returning secret contents.
    pub fn check_files(&self) -> Result<(), String> {
        for (name, credential) in &self.runners {
            open_file(
                &credential.token_file,
                &format!("runners.{name}.token_file"),
            )?;
        }
        for (name, credential) in &self.mirror.credentials {
            open_file(
                &credential.token_file,
                &format!("mirror.credentials.{name}.token_file"),
            )?;
        }
        Ok(())
    }
}

impl RunnerConfig {
    pub fn parse(input: &str) -> Result<Self, String> {
        let mut config: Self = parse(input)?;
        crate::log::filter(&config.log.level, None)?;
        config.name = validate_name(&config.name).map_err(|err| format!("name: {err}"))?;
        url_host(&config.url, "url")?;
        validate_metrics(&config.metrics)?;
        validate_path(&config.token_file, "token_file")?;
        validate_path(&config.state_dir, "state_dir")?;
        if config.max_jobs == 0 {
            return Err("max_jobs: must be greater than zero".into());
        }
        Ok(config)
    }

    pub fn check_files(&self) -> Result<(), String> {
        open_file(&self.token_file, "token_file")
    }
}

/// Parse the same OpenSSH key types accepted by configuration validation.
pub fn parse_ssh_key(key: &str) -> Result<ssh_key::PublicKey, String> {
    // A newline in the comment would inject a second authorized_keys line.
    if key.bytes().any(|byte| byte < 0x20 || byte == 0x7f) {
        return Err("OpenSSH public key contains a control character".into());
    }
    if !matches!(
        key.split_whitespace().next(),
        Some(
            "ssh-ed25519"
                | "sk-ssh-ed25519@openssh.com"
                | "ssh-rsa"
                | "ecdsa-sha2-nistp256"
                | "ecdsa-sha2-nistp384"
                | "ecdsa-sha2-nistp521"
                | "sk-ecdsa-sha2-nistp256@openssh.com"
        )
    ) {
        return Err("unsupported OpenSSH public key type".into());
    }
    ssh_key::PublicKey::from_openssh(key).map_err(|_| "invalid OpenSSH public key".into())
}

impl TokenConfig {
    /// Convert the validated RFC 3339 expiry to the schema's UTC milliseconds.
    pub fn expires_at_millis(&self) -> Result<Option<i64>, String> {
        let Some(expires) = &self.expires else {
            return Ok(None);
        };
        let date = expires
            .parse::<toml::value::Datetime>()
            .map_err(|_| "invalid token expiry")?;
        let day = date.date.ok_or("missing token expiry date")?;
        let time = date.time.ok_or("missing token expiry time")?;
        let offset = match date.offset.ok_or("missing token expiry offset")? {
            toml::value::Offset::Z => 0,
            toml::value::Offset::Custom { minutes } => i64::from(minutes) * 60,
        };
        let year = i64::from(day.year);
        let leap = |y: i64| y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
        let days_before_year = |y: i64| {
            let y = y - 1;
            365 * y + y.div_euclid(4) - y.div_euclid(100) + y.div_euclid(400)
        };
        let months = [
            31,
            if leap(year) { 29 } else { 28 },
            31,
            30,
            31,
            30,
            31,
            31,
            30,
            31,
            30,
            31,
        ];
        let days = days_before_year(year) - days_before_year(1970)
            + months[..usize::from(day.month - 1)].iter().sum::<i64>()
            + i64::from(day.day)
            - 1;
        Ok(Some(
            (days * 86400
                + i64::from(time.hour) * 3600
                + i64::from(time.minute) * 60
                + i64::from(time.second)
                - offset)
                * 1000
                + i64::from(time.nanosecond / 1_000_000),
        ))
    }
}
