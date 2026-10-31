//! `kajiba login` (0007, ADR 0089): checks a PAT read from stdin, then saves it with the
//! forge URL in one credential file (ADR 0098).
use crate::client::{self, Env, Error};
use kajiba_proto::v1::{GetViewerRequest, auth_service_client::AuthServiceClient};
use std::{
    fs,
    io::{self, BufRead, Write},
    os::unix::fs::DirBuilderExt,
    path::Path,
};

pub async fn run(
    token_file: &Path,
    url: Option<&str>,
    development: bool,
    env: Env<'_>,
    input: &mut dyn BufRead,
    tty: bool,
    out: &mut dyn Write,
) -> Result<(), Error> {
    let token = read(input, tty).map_err(|err| Error(format!("cannot read the token: {err}")))?;
    if !well_formed(&token) {
        return Err(Error(
            "not a personal access token: expected kjb_ and 43 base64url characters".into(),
        ));
    }
    let url = match client::requested_url(url, env)? {
        Some(url) => url,
        None => {
            client::read_credentials(token_file)?
                .ok_or_else(|| Error("no forge URL: pass --url or set KAJIBA_URL".into()))?
                .url
        }
    };
    let viewer = client::answer(
        AuthServiceClient::new(client::connect(&url, &token, development)?)
            .get_viewer(GetViewerRequest {})
            .await,
    )?;
    // One file for the pair: the token can never be read back with another forge's URL.
    save(token_file, &format!("{url}\n{token}"))?;
    writeln!(out, "logged in to {url} as {}", viewer.user)
        .map_err(|err| Error(format!("stdout: {err}")))
}

/// One line; on a terminal, prompted on stderr and not echoed.
fn read(input: &mut dyn BufRead, tty: bool) -> io::Result<String> {
    let mut line = String::new();
    if tty {
        let _ = write!(io::stderr(), "Token: ");
        let _quiet = NoEcho::new()?;
        input.read_line(&mut line)?;
        report!("");
    } else {
        input.read_line(&mut line)?;
    }
    Ok(line.trim().into())
}

/// `kjb_` + 32 bytes in unpadded base64url (0014).
fn well_formed(token: &str) -> bool {
    token.strip_prefix("kjb_").is_some_and(|secret| {
        secret.len() == 43
            && secret
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    })
}

/// Written beside, then renamed: a failure leaves the previous file whole. Mode 0600.
fn save(path: &Path, content: &str) -> Result<(), Error> {
    let failed = |err: io::Error| Error(format!("cannot write {}: {err}", path.display()));
    if let Some(dir) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .map_err(failed)?;
    }
    let dir = path
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut file = tempfile::NamedTempFile::new_in(dir).map_err(failed)?;
    writeln!(file, "{content}")
        .and_then(|()| file.as_file().sync_all())
        .map_err(failed)?;
    file.persist(path).map_err(|err| failed(err.error))?;
    Ok(())
}

/// Terminal echo off on stdin until dropped.
// shortcut: a signal while reading leaves echo off (`stty echo` restores it); handle SIGINT if users hit it.
struct NoEcho(libc::termios);

impl NoEcho {
    fn new() -> io::Result<Self> {
        let mut saved = std::mem::MaybeUninit::<libc::termios>::uninit();
        // SAFETY: tcgetattr fills the termios it is given when it returns 0.
        if unsafe { libc::tcgetattr(libc::STDIN_FILENO, saved.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: initialised by the successful tcgetattr above.
        let saved = unsafe { saved.assume_init() };
        let mut quiet = saved;
        quiet.c_lflag &= !libc::ECHO;
        // SAFETY: a valid termios for stdin.
        if unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &quiet) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(saved))
    }
}

impl Drop for NoEcho {
    fn drop(&mut self) {
        // SAFETY: restores the attributes read in `new`.
        unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &self.0) };
    }
}

#[cfg(test)]
mod tests;
