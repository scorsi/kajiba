//! Shared import input rules and safe error classes (0041, 0044).
use std::path::Path;

/// An absolute path, or a URL that cannot carry a credential: no userinfo (an ssh
/// user name excepted), query or fragment (ADR 0041, 0044). A token in the path is
/// indistinguishable from a path segment and is not protected: `--source-file` keeps
/// it out of the CLI's arguments (ADR 0048), but git's transport helper receives the
/// full source as an argument (ADR 0049).
pub fn check_source(source: &str) -> Result<(), &'static str> {
    let Some((scheme, rest)) = source.split_once("://") else {
        // The daemon would resolve a relative path from its own working directory.
        return if Path::new(source).is_absolute() {
            Ok(())
        } else {
            Err("not an absolute path or a URL")
        };
    };
    if rest.contains(['?', '#']) {
        return Err("a query or fragment in the source is refused; use --credentials-file");
    }
    let authority = rest.split('/').next().unwrap_or(rest);
    match authority.rsplit_once('@') {
        Some((user, _)) if scheme != "ssh" || user.contains(':') => {
            Err("credentials in the source are refused; use --credentials-file")
        }
        _ => Ok(()),
    }
}

/// Classifies a git failure from its stderr. Git echoes the source, remote banners
/// and sometimes a credential: none of that text ever leaves the daemon, only one
/// of these fixed classes does (ADR 0022, 0041).
pub fn reason(stderr: &str) -> &'static str {
    let text = stderr.to_ascii_lowercase();
    let any = |needles: &[&str]| needles.iter().any(|needle| text.contains(needle));
    if any(&["did not finish within", "timed out", "operation too slow"]) {
        "timed out"
    } else if any(&[
        "authentication failed",
        "error: 401",
        "error: 403",
        "could not read username",
        "could not read password",
        "terminal prompts disabled",
        "permission denied",
        "host key verification failed",
        "access denied",
    ]) {
        "authentication failed"
    } else if any(&[
        "not found",
        "error: 404",
        "no such file or directory",
        "does not exist",
    ]) {
        "source not found"
    } else if any(&[
        "not appear to be a git repository",
        "not a git repository",
        "bundle file",
    ]) {
        "not a git repository"
    } else if any(&[
        "could not resolve host",
        "failed to connect",
        "connection refused",
        "connection reset",
        "unable to look up",
        "unable to access",
        "unable to connect",
        "could not read from remote repository",
        "remote end hung up",
        "early eof",
    ]) {
        "network error"
    } else {
        "other error"
    }
}
