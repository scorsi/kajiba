//! TOML configuration shared by the server, git side, runner and offline validation.

use std::{
    collections::BTreeMap,
    fs::{File, Metadata, OpenOptions},
    io::Read,
    net::SocketAddr,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    time::Duration,
};

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
    pub git: GitSideConfig,
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
    pub repos: BTreeMap<String, RepoConfig>,
    #[serde(default)]
    pub ci: CiConfig,
    #[serde(default)]
    pub dump: DumpConfig,
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GitSideConfig {
    pub state_dir: PathBuf,
    pub socket: PathBuf,
}

impl Default for GitSideConfig {
    fn default() -> Self {
        Self {
            state_dir: git_state_dir(),
            socket: "/run/kajiba-git/local.sock".into(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitConfig {
    #[serde(default = "git_state_dir")]
    pub state_dir: PathBuf,
    #[serde(default = "server_socket")]
    pub server_socket: PathBuf,
    pub hooks_path: PathBuf,
    #[serde(default)]
    pub log: LogConfig,
    #[serde(default)]
    pub metrics: MetricsConfig,
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
    /// Local loopback SSH banner probe; absent disables it (ADR 0072).
    pub probe: Option<SocketAddr>,
}

impl Default for SshConfig {
    fn default() -> Self {
        Self {
            host: None,
            port: 2222,
            probe: None,
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

/// A declared personal access token. The token must carry at least 128 bits of randomness
/// (ADR 0028): only its hash is declared, so validation cannot check it. ADR 0014's format
/// (`kjb_` + 32 random bytes in base64url) has 256; generate one and its hash with
/// `t="kjb_$(head -c 32 /dev/urandom | basenc --base64url | tr -d '=')"; echo "$t"; printf %s "$t" | sha256sum`.
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

/// A declared repository (0026); its map key is its stable identity.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepoConfig {
    pub owner: String,
    /// Empty when absent; `ServerConfig::parse` replaces it with the key.
    #[serde(default, deserialize_with = "explicit_name")]
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub visibility: Visibility,
    #[serde(default = "default_branch")]
    pub default_branch: String,
    #[serde(default)]
    pub ci_enabled: bool,
    /// `<owner>/<name>` pairs, lowercase after `ServerConfig::parse`.
    #[serde(default)]
    pub previous_names: Vec<String>,
    #[serde(default)]
    pub permissions: BTreeMap<String, Role>,
    #[serde(default)]
    pub mirrors: Vec<RepoMirrorConfig>,
}

#[derive(Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Visibility {
    #[default]
    Public,
}

/// Ordered from least to most privileged.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Read,
    Write,
    Admin,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepoMirrorConfig {
    pub remote_url: String,
    pub credential: String,
    #[serde(default = "enabled")]
    pub enabled: bool,
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
    "/var/lib/kajiba-server".into()
}
fn git_state_dir() -> PathBuf {
    "/var/lib/kajiba-git".into()
}
fn server_socket() -> PathBuf {
    "/run/kajiba/git.sock".into()
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
fn default_branch() -> String {
    "main".into()
}
fn enabled() -> bool {
    true
}

/// An explicit empty `name` must not silently mean "the key".
fn explicit_name<'de, D: Deserializer<'de>>(de: D) -> Result<String, D::Error> {
    let name = String::deserialize(de)?;
    if name.is_empty() {
        return Err(serde::de::Error::custom(crate::names::NameError::Length));
    }
    Ok(name)
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
        // Never echo a rejected value: it may be a misplaced secret. Keep what was expected.
        if err.message().starts_with("unknown variant")
            || err.message().starts_with("invalid value")
        {
            let expected = err
                .message()
                .split_once(", expected ")
                .map_or(String::new(), |(_, expected)| {
                    format!(", expected {expected}")
                });
            message = message.replace(err.message(), &format!("invalid value{expected}"));
        } else if err.message().starts_with("invalid type:") {
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

/// Mirror remotes are public configuration; authentication belongs in credential files.
pub fn validate_mirror_url(value: &str, field: &str) -> Result<(), String> {
    let error = || {
        format!(
            "{field}: expected an absolute HTTPS or SSH URL with a host and valid port, without userinfo, query or fragment"
        )
    };
    let uri: Uri = value.parse().map_err(|_| error())?;
    let authority = uri.authority().ok_or_else(error)?;
    let host = authority.host();
    if !matches!(uri.scheme_str(), Some("https" | "ssh"))
        || host.is_empty()
        || authority.as_str().contains('@')
        || uri.query().is_some()
        || value.contains('#')
        || (authority.as_str().len() > host.len() && authority.port_u16().is_none())
        || authority.port_u16() == Some(0)
    {
        return Err(error());
    }
    validate_host(host, field)
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
    for (index, (name, value)) in std::mem::take(map).into_iter().enumerate() {
        // A rejected name is not echoed (it may be a misplaced secret); its sorted position is.
        let key = validate_name(&name)
            .map_err(|err| format!("{field}: name #{} in sorted order: {err}", index + 1))?;
        if normalized.insert(key, value).is_some() {
            return Err(format!(
                "{field}.{name}: duplicate name after lowercase normalization"
            ));
        }
    }
    *map = normalized;
    Ok(())
}

fn validate_path(path: &Path, field: &str) -> Result<(), String> {
    if path.as_os_str().is_empty() {
        return Err(format!("{field}: path must not be empty"));
    }
    Ok(())
}

/// Open a regular file, following symlinks, without blocking on a FIFO or a device.
fn open_regular(path: &Path, field: &str) -> Result<(File, Metadata), String> {
    let error = |err: std::io::Error| format!("{field} ({}): {err}", path.display());
    // O_NONBLOCK makes opening a FIFO return at once; it has no effect on a regular file.
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .map_err(error)?;
    let metadata = file.metadata().map_err(error)?;
    if !metadata.is_file() {
        return Err(format!(
            "{field} ({}): expected a regular file",
            path.display()
        ));
    }
    Ok((file, metadata))
}

/// Largest configuration file read; a generated file is a few KiB.
pub const MAX_CONFIG_BYTES: u64 = 1 << 20;

/// Read a configuration file: a regular file of at most `MAX_CONFIG_BYTES`.
pub fn read_config(path: &Path) -> Result<String, String> {
    let (file, _) = open_regular(path, "configuration")?;
    let mut input = String::new();
    file.take(MAX_CONFIG_BYTES + 1)
        .read_to_string(&mut input)
        .map_err(|err| format!("configuration ({}): {err}", path.display()))?;
    if input.len() as u64 > MAX_CONFIG_BYTES {
        return Err(format!(
            "configuration ({}): larger than {MAX_CONFIG_BYTES} bytes",
            path.display()
        ));
    }
    Ok(input)
}

fn open_file(path: &Path, field: &str) -> Result<File, String> {
    let (file, metadata) = open_regular(path, field)?;
    // A symlink (secret manager) is followed: the mode checked is the secret's own.
    if metadata.permissions().mode() & 0o006 != 0 {
        return Err(format!(
            "{field} ({}): must not be readable or writable by others",
            path.display()
        ));
    }
    Ok(file)
}

/// Largest secret file read (a credential is a few hundred bytes).
pub const MAX_SECRET_BYTES: u64 = 4096;

/// Reads a secret file under the same rules as the configuration's secret files
/// (`open_file`), without its trailing newline.
pub fn read_secret(path: &Path, field: &str) -> Result<String, String> {
    let file = open_file(path, field)?;
    let mut secret = String::new();
    file.take(MAX_SECRET_BYTES + 1)
        .read_to_string(&mut secret)
        .map_err(|err| format!("{field} ({}): {err}", path.display()))?;
    if secret.len() as u64 > MAX_SECRET_BYTES {
        return Err(format!(
            "{field} ({}): larger than {MAX_SECRET_BYTES} bytes",
            path.display()
        ));
    }
    Ok(secret.trim_end_matches(['\n', '\r']).to_owned())
}

/// ADR 0026's `config check` rules; names are normalized to lowercase in place.
fn validate_repos(config: &mut ServerConfig) -> Result<(), String> {
    // Rejected values are not echoed (they may be misplaced secrets); fields are.
    let mut current = BTreeMap::new();
    for (key, repo) in &mut config.repos {
        let field = format!("repos.{key}");
        repo.owner = validate_name(&repo.owner).map_err(|err| format!("{field}.owner: {err}"))?;
        if !config.users.contains_key(&repo.owner) {
            return Err(format!("{field}.owner: not a declared user"));
        }
        repo.name = if repo.name.is_empty() {
            key.clone()
        } else {
            validate_name(&repo.name).map_err(|err| format!("{field}.name: {err}"))?
        };
        if let Some(first) = current.insert(format!("{}/{}", repo.owner, repo.name), key.clone()) {
            return Err(format!(
                "{field}.name: same current owner and name as repos.{first}"
            ));
        }
        crate::names::validate_ref(&format!("refs/heads/{}", repo.default_branch))
            .map_err(|err| format!("{field}.default_branch: {err}"))?;
        normalize(&mut repo.permissions, &format!("{field}.permissions"))?;
        if let Some(user) = repo
            .permissions
            .keys()
            .find(|user| !config.users.contains_key(*user))
        {
            return Err(format!("{field}.permissions.{user}: not a declared user"));
        }
        let mut remotes = BTreeMap::new();
        for (index, mirror) in repo.mirrors.iter_mut().enumerate() {
            let mirror_field = format!("{field}.mirrors[{index}]");
            validate_mirror_url(&mirror.remote_url, &format!("{mirror_field}.remote_url"))?;
            mirror.credential = validate_name(&mirror.credential)
                .ok()
                .filter(|name| config.mirror.credentials.contains_key(name))
                .ok_or_else(|| {
                    format!("{mirror_field}.credential: not a declared mirror credential")
                })?;
            if let Some(first) = remotes.insert(mirror.remote_url.clone(), index) {
                return Err(format!(
                    "{mirror_field}.remote_url: already declared by {field}.mirrors[{first}].remote_url"
                ));
            }
        }
    }
    let mut previous = BTreeMap::new();
    for (key, repo) in &mut config.repos {
        for (index, name) in repo.previous_names.iter_mut().enumerate() {
            let field = format!("repos.{key}.previous_names[{index}]");
            let pair = name
                .split_once('/')
                .ok_or(crate::names::NameError::InvalidCharacters)
                .and_then(|(owner, name)| {
                    Ok(format!(
                        "{}/{}",
                        validate_name(owner)?,
                        validate_name(name)?
                    ))
                })
                .map_err(|err| format!("{field}: expected <owner>/<name>: {err}"))?;
            if let Some(holder) = current.get(&pair) {
                return Err(format!("{field}: current name of repos.{holder}"));
            }
            if let Some(first) = previous.insert(pair.clone(), field.clone()) {
                return Err(format!("{field}: already declared by {first}"));
            }
            *name = pair;
        }
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
        if let Some(probe) = config.ssh.probe {
            if !probe.ip().is_loopback() || probe.port() == 0 {
                return Err("ssh.probe: must be a loopback address with a nonzero port".into());
            }
        }
        if config.http.listen.port() == 0 {
            return Err("http.listen: port must be nonzero".into());
        }
        validate_metrics(&config.metrics)?;
        validate_path(&config.state_dir, "state_dir")?;
        validate_path(&config.dump.dir, "dump.dir")?;
        for (path, field) in [
            (&config.git.state_dir, "git.state_dir"),
            (&config.git.socket, "git.socket"),
        ] {
            if !path.is_absolute() {
                return Err(format!("{field}: expected an absolute path"));
            }
        }
        normalize(&mut config.users, "users")?;
        normalize(&mut config.runners, "runners")?;
        normalize(&mut config.mirror.credentials, "mirror.credentials")?;
        normalize(&mut config.repos, "repos")?;
        for (field, ttl, max, max_text) in [
            (
                "auth.enrollment_ttl",
                config.auth.enrollment_ttl,
                86400,
                "24h",
            ),
            (
                "auth.session_ttl",
                config.auth.session_ttl,
                90 * 86400,
                "90d",
            ),
        ] {
            if !(60..=max).contains(&ttl.as_secs()) {
                return Err(format!("{field}: must be between 1m and {max_text}"));
            }
        }
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
        validate_repos(&mut config)?;
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

impl GitConfig {
    pub fn parse(input: &str) -> Result<Self, String> {
        let config: Self = parse(input)?;
        crate::log::filter(&config.log.level, None)?;
        validate_metrics(&config.metrics)?;
        for (path, field) in [
            (&config.state_dir, "state_dir"),
            (&config.server_socket, "server_socket"),
            (&config.hooks_path, "hooks_path"),
        ] {
            if !path.is_absolute() {
                return Err(format!("{field}: expected an absolute path"));
            }
        }
        Ok(config)
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
        open_file(&self.token_file, "token_file").map(drop)
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
