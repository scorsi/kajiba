//! Runner-host requirements of the `nix` backend (0009), checked at startup.

use serde_json::Value;
use std::{os::unix::fs::MetadataExt, path::Path};

/// Check `nix config show --json` for the runner `user`, member of `groups`: the build
/// sandbox is on, the user is not trusted by the daemon, a flake's `nixConfig` is not
/// applied. Returns the build users' group (empty when builds run as the caller).
pub fn check_nix(json: &str, user: &str, groups: &[String]) -> Result<String, String> {
    let config: Value =
        serde_json::from_str(json).map_err(|err| format!("nix config show: {err}"))?;
    let value = |key: &str| {
        config
            .get(key)
            .and_then(|setting| setting.get("value"))
            .ok_or_else(|| format!("nix config show: no {key} setting"))
    };
    // `relaxed` lets derivations opt out of the sandbox.
    if value("sandbox")? != &Value::Bool(true) {
        return Err("nix: sandbox must be true".into());
    }
    if value("accept-flake-config")? != &Value::Bool(false) {
        return Err("nix: accept-flake-config must be false".into());
    }
    let trusted = value("trusted-users")?
        .as_array()
        .ok_or("nix config show: trusted-users is not a list")?;
    for entry in trusted.iter().filter_map(Value::as_str) {
        if entry == "*"
            || entry == user
            || entry
                .strip_prefix('@')
                .is_some_and(|group| groups.iter().any(|g| g == group))
        {
            return Err(format!(
                "nix: the runner user {user} must not be in trusted-users (through {entry})"
            ));
        }
    }
    Ok(value("build-users-group")?
        .as_str()
        .unwrap_or_default()
        .to_owned())
}

/// The token file is not readable by build users: `config check --files` already refuses
/// access by others, so its group must not be theirs when it is group-readable.
pub fn check_token(mode: u32, gid: u32, build_gid: Option<u32>) -> Result<(), String> {
    if mode & 0o040 != 0 && build_gid == Some(gid) {
        return Err("token_file: must not be readable by the Nix build users' group".into());
    }
    Ok(())
}

async fn output(program: &str, args: &[&str]) -> Result<String, String> {
    let output = tokio::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .map_err(|err| format!("{program}: {err}"))?;
    if !output.status.success() {
        return Err(format!(
            "{program} {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    String::from_utf8(output.stdout).map_err(|err| format!("{program}: {err}"))
}

/// Every requirement of 0009 on this host, for the token file at `token`.
pub async fn check(token: &Path) -> Result<(), String> {
    let json = output("nix", &["config", "show", "--json"]).await?;
    let user = output("id", &["-un"]).await?;
    let groups: Vec<String> = output("id", &["-Gn"])
        .await?
        .split_whitespace()
        .map(str::to_owned)
        .collect();
    let group = check_nix(&json, user.trim(), &groups)?;
    let build_gid = match group.as_str() {
        "" => None,
        name => Some(
            std::ffi::CString::new(name)
                .ok()
                .and_then(|name| kajiba_core::local::group_id(&name).ok())
                .ok_or_else(|| format!("nix: build-users-group {name} does not resolve"))?,
        ),
    };
    let metadata = std::fs::metadata(token)
        .map_err(|err| format!("token_file ({}): {err}", token.display()))?;
    check_token(metadata.mode(), metadata.gid(), build_gid)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nix(sandbox: &str, accept: &str, trusted: &str) -> String {
        format!(
            r#"{{"sandbox": {{"value": {sandbox}}}, "accept-flake-config": {{"value": {accept}}},
                "trusted-users": {{"value": {trusted}}}, "build-users-group": {{"value": "nixbld"}}}}"#
        )
    }

    #[test]
    fn each_host_requirement_failure_is_named() {
        let groups = ["kajiba-runner".to_owned(), "builders".to_owned()];
        let check = |json: &str| check_nix(json, "kajiba-runner", &groups);
        assert_eq!(
            check(&nix("true", "false", r#"["root", "@wheel"]"#)),
            Ok("nixbld".into())
        );
        for (json, expected) in [
            (nix("false", "false", "[]"), "nix: sandbox must be true"),
            (
                nix(r#""relaxed""#, "false", "[]"),
                "nix: sandbox must be true",
            ),
            (
                nix("true", "true", "[]"),
                "nix: accept-flake-config must be false",
            ),
            (
                nix("true", "false", r#"["root", "kajiba-runner"]"#),
                "nix: the runner user kajiba-runner must not be in trusted-users (through kajiba-runner)",
            ),
            (
                nix("true", "false", r#"["@builders"]"#),
                "nix: the runner user kajiba-runner must not be in trusted-users (through @builders)",
            ),
            (
                nix("true", "false", r#"["*"]"#),
                "nix: the runner user kajiba-runner must not be in trusted-users (through *)",
            ),
            (
                r#"{"sandbox": {"value": true}}"#.into(),
                "nix config show: no accept-flake-config setting",
            ),
        ] {
            assert_eq!(check(&json).unwrap_err(), expected, "{json}");
        }
        assert!(
            check("not json")
                .unwrap_err()
                .starts_with("nix config show: ")
        );
    }

    #[test]
    fn the_token_file_is_not_readable_by_the_build_group() {
        assert!(check_token(0o600, 30000, Some(30000)).is_ok());
        assert!(check_token(0o640, 100, Some(30000)).is_ok());
        assert!(check_token(0o640, 100, None).is_ok());
        assert_eq!(
            check_token(0o640, 30000, Some(30000)).unwrap_err(),
            "token_file: must not be readable by the Nix build users' group"
        );
    }
}
