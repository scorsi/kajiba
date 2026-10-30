//! The tonic client of the forge's `kajiba.v1` API (0007): token file, forge URL, the
//! version header on every call (0080, 0090) and errors carrying the gRPC reason (0020).
use kajiba_core::version::semver;
use std::{
    ffi::OsString,
    fmt,
    path::{Path, PathBuf},
};
use tonic::{
    Request, Status,
    metadata::{Ascii, MetadataMap, MetadataValue},
    service::{Interceptor, interceptor::InterceptedService},
    transport::{Channel, ClientTlsConfig, Endpoint},
};
use tonic_types::StatusExt;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
const CLIENT_VERSION: &str = "kajiba-client-version";
const SERVER_VERSION: &str = "kajiba-server-version";

/// A command failure, printed as `kajiba: <message>` with exit code 1 (0007).
#[derive(Debug)]
pub struct Error(pub String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<Status> for Error {
    /// `<message> (<REASON>, request <id>)`; without ErrorInfo, the gRPC code stands in.
    fn from(status: Status) -> Self {
        let message = match status.message() {
            "" => status.code().description(),
            message => message,
        };
        let details = status.get_error_details();
        Error(match details.error_info() {
            Some(info) => match info.metadata.get("request_id") {
                Some(id) => format!("{message} ({}, request {id})", info.reason),
                None => format!("{message} ({})", info.reason),
            },
            None => format!("{message} ({:?})", status.code()),
        })
    }
}

/// The process environment, or a fixed one in tests.
pub type Env<'a> = &'a dyn Fn(&str) -> Option<OsString>;

/// `--token-file`, else `$XDG_CONFIG_HOME/kajiba/token` (`~/.config` when unset).
pub fn token_path(option: Option<&Path>, env: Env) -> Result<PathBuf, Error> {
    if let Some(path) = option {
        return Ok(path.into());
    }
    let config = match env("XDG_CONFIG_HOME").filter(|dir| !dir.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => PathBuf::from(
            env("HOME").ok_or_else(|| Error("HOME is not set; pass --token-file".into()))?,
        )
        .join(".config"),
    };
    Ok(config.join("kajiba/token"))
}

/// A credential file (ADR 0098): the forge URL on the first line, its token on the second.
pub struct Credentials {
    pub url: String,
    pub token: String,
}

/// The credential file at `path`; `None` when it does not exist.
pub fn read_credentials(path: &Path) -> Result<Option<Credentials>, Error> {
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(Error(format!(
                "cannot read token file {}: {err}",
                path.display()
            )));
        }
    };
    match content.split_once('\n') {
        Some((url, token)) if url.contains("://") => Ok(Some(Credentials {
            url: url.trim().into(),
            token: token.trim().into(),
        })),
        _ => Err(Error(format!(
            "token file {} does not hold a forge URL and a token; run kajiba login",
            path.display()
        ))),
    }
}

/// `--url`, else `KAJIBA_URL`: the forge a command asks for, if any.
pub fn requested_url(option: Option<&str>, env: Env) -> Result<Option<String>, Error> {
    if let Some(url) = option {
        return Ok(Some(url.into()));
    }
    env("KAJIBA_URL")
        .map(|url| {
            url.into_string()
                .map_err(|_| Error("KAJIBA_URL is not UTF-8".into()))
        })
        .transpose()
}

/// The saved credentials; a `--url` or `KAJIBA_URL` naming another forge is refused, so a
/// token only ever goes to the forge it was saved for (ADR 0098).
pub fn credentials(option: Option<&str>, path: &Path, env: Env) -> Result<Credentials, Error> {
    let saved = read_credentials(path)?.ok_or_else(|| {
        Error(format!(
            "no token file at {}; run kajiba login",
            path.display()
        ))
    })?;
    match requested_url(option, env)? {
        Some(url) if url != saved.url => Err(Error(format!(
            "token file {} is for {}, not {url}; log in to {url} with another --token-file",
            path.display(),
            saved.url
        ))),
        _ => Ok(saved),
    }
}

/// Adds the bearer token and the client version to every call.
#[derive(Clone)]
pub struct Headers(MetadataValue<Ascii>);

impl Interceptor for Headers {
    fn call(&mut self, mut request: Request<()>) -> Result<Request<()>, Status> {
        let metadata = request.metadata_mut();
        metadata.insert("authorization", self.0.clone());
        metadata.insert(CLIENT_VERSION, MetadataValue::from_static(VERSION));
        Ok(request)
    }
}

pub type Api = InterceptedService<Channel, Headers>;

/// A lazily connected channel to `url` carrying `token`. The URL must be `https`; plain
/// `http` to loopback only with `--development`, warned once (ADRs 0099, 0104). A refused
/// URL fails before anything is sent.
pub fn connect(url: &str, token: &str, development: bool) -> Result<Api, Error> {
    let bearer = format!("Bearer {token}")
        .parse()
        .map_err(|_| Error("the token file holds characters a header cannot carry".into()))?;
    let tls = kajiba_core::transport::credential_tls(url, development).map_err(Error)?;
    let mut endpoint = Endpoint::from_shared(url.to_string())
        .map_err(|err| Error(format!("invalid forge URL {url}: {err}")))?;
    if tls {
        endpoint = endpoint
            .tls_config(ClientTlsConfig::new().with_native_roots())
            .map_err(|err| Error(format!("TLS for {url}: {err}")))?;
    } else {
        eprintln!("kajiba: warning: development mode: sending the token over plain http to {url}");
    }
    Ok(InterceptedService::new(
        endpoint.connect_lazy(),
        Headers(bearer),
    ))
}

/// A call's outcome: the 0023 warning on stderr, then the message or the error.
pub fn answer<T>(result: Result<tonic::Response<T>, Status>) -> Result<T, Error> {
    let metadata = match &result {
        Ok(response) => response.metadata(),
        Err(status) => status.metadata(),
    };
    if let Some(warning) = server_version(metadata).and_then(|server| warning(VERSION, &server)) {
        eprintln!("kajiba: warning: {warning}");
    }
    Ok(result?.into_inner())
}

fn server_version(metadata: &MetadataMap) -> Option<String> {
    Some(metadata.get(SERVER_VERSION)?.to_str().ok()?.into())
}

/// ADR 0023's server ↔ CLI row from 1.0: the server accepts another minor of the same
/// major, and the CLI warns. Before 1.0 the server refuses it (VERSION_INCOMPATIBLE).
pub fn warning(client: &str, server: &str) -> Option<String> {
    let ((major, minor, _), (server_major, server_minor, _)) = (semver(client)?, semver(server)?);
    (major >= 1 && major == server_major && minor != server_minor)
        .then(|| format!("server version {server} differs from kajiba {client}"))
}

#[cfg(test)]
pub mod tests;
