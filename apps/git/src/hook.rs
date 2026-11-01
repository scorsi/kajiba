//! `kajiba-git hook pre-receive|post-receive` (ADR 0003): validate Git's input and
//! serve-ssh's context, make one call over the local socket, and decide nothing itself.

use kajiba_core::log;
use kajiba_proto::hook::v1::{PostReceiveRequest, PreReceiveRequest, Quarantine, RefUpdate};
use std::{io::Read, path::Path, process::ExitCode};

// ADR 0071: bound raw stdin before parsing or sending the hook RPC.
const MAX_INPUT: u64 = 1 << 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    PreReceive,
    PostReceive,
}

/// Runs one hook: exit 0 when the git daemon allows (or acknowledges) the push, 1 otherwise.
/// Refusal reasons go to stderr as plain lines, which Git shows the pusher (ADR 0071).
pub async fn run(
    socket: &Path,
    kind: Kind,
    input: impl Read,
    env: &dyn Fn(&str) -> Option<String>,
) -> ExitCode {
    let name = match kind {
        Kind::PreReceive => "pre-receive",
        Kind::PostReceive => "post-receive",
    };
    match call(socket, kind, input, env).await {
        Ok(refusals) if refusals.is_empty() => ExitCode::SUCCESS,
        Ok(refusals) => {
            for (ref_name, reason) in refusals {
                if ref_name.is_empty() {
                    eprintln!("kajiba: push refused: {reason}");
                } else {
                    eprintln!("kajiba: {ref_name}: {reason}");
                }
            }
            ExitCode::FAILURE
        }
        Err(err) => {
            log::error("fatal", &format!("hook {name}: {err}"));
            ExitCode::FAILURE
        }
    }
}

/// The refusals of the one RPC this invocation makes; nothing is sent if the input or the
/// context is malformed.
async fn call(
    socket: &Path,
    kind: Kind,
    input: impl Read,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<Vec<(String, String)>, String> {
    let user_public_id = public_id(env, "KAJIBA_USER")?;
    let repo_public_id = public_id(env, "KAJIBA_REPO")?;
    let session_id = public_id(env, "KAJIBA_SESSION")?;
    let session_socket = env(crate::local::SESSION_SOCKET_ENV);
    let socket = session_socket.as_deref().map(Path::new).unwrap_or(socket);
    let mut buffer = Vec::new();
    input
        .take(MAX_INPUT + 1)
        .read_to_end(&mut buffer)
        .map_err(|err| format!("cannot read ref updates: {err}"))?;
    if buffer.len() as u64 > MAX_INPUT {
        return Err("too many ref updates".into());
    }
    let updates = updates(&buffer)?;
    let unavailable = |err: Box<dyn std::error::Error>| match err.downcast_ref::<tonic::Status>() {
        Some(status) => format!("the git daemon refused the check: {}", status.message()),
        None => format!("the git daemon is unavailable, retry later ({err})"),
    };
    match kind {
        Kind::PreReceive => {
            let response = crate::local::pre_receive(
                socket,
                PreReceiveRequest {
                    user_public_id,
                    repo_public_id,
                    session_id,
                    updates,
                    quarantine: Some(quarantine(env)?),
                },
            )
            .await
            .map_err(unavailable)?;
            if response.allowed {
                return Ok(Vec::new());
            }
            let mut refusals: Vec<_> = response
                .refusals
                .into_iter()
                .map(|refusal| (refusal.ref_name, refusal.reason))
                .collect();
            if refusals.is_empty() {
                refusals.push((String::new(), "refused by the git daemon".into()));
            }
            Ok(refusals)
        }
        Kind::PostReceive => {
            crate::local::post_receive(
                socket,
                PostReceiveRequest {
                    user_public_id,
                    repo_public_id,
                    session_id,
                    updates,
                },
            )
            .await
            .map_err(unavailable)?;
            Ok(Vec::new())
        }
    }
}

/// A canonical public id (ULID) set by serve-ssh.
fn public_id(env: &dyn Fn(&str) -> Option<String>, var: &str) -> Result<String, String> {
    let value = env(var).ok_or_else(|| format!("{var} is not set (hooks run under serve-ssh)"))?;
    if kajiba_core::names::is_public_id(&value) {
        Ok(value)
    } else {
        Err(format!("{var} is not a public id"))
    }
}

/// Git's quarantine variables, verbatim; absent for a push that only deletes refs.
fn quarantine(env: &dyn Fn(&str) -> Option<String>) -> Result<Quarantine, String> {
    let [
        object_directory,
        alternate_object_directories,
        quarantine_path,
    ] = [
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_QUARANTINE_PATH",
    ]
    .map(|var| env(var).unwrap_or_default());
    for (var, path) in [
        ("GIT_OBJECT_DIRECTORY", &object_directory),
        ("GIT_QUARANTINE_PATH", &quarantine_path),
    ] {
        if !path.is_empty() && !path.starts_with('/') {
            return Err(format!("{var} is not an absolute path"));
        }
    }
    Ok(Quarantine {
        object_directory,
        alternate_object_directories,
        quarantine_path,
    })
}

/// Git's hook input: one `<old> <new> <ref>` line per update, in order, each ending in a
/// newline; ids are 40 lowercase hex digits, zeros for an absent ref.
pub fn updates(input: &[u8]) -> Result<Vec<RefUpdate>, String> {
    let input = std::str::from_utf8(input).map_err(|_| "ref updates are not UTF-8")?;
    let input = input
        .strip_suffix('\n')
        .ok_or("ref updates must be newline-terminated lines")?;
    input
        .split('\n')
        .zip(1..)
        .map(|(line, number)| {
            let fields: Vec<&str> = line.split(' ').collect();
            let [old, new, name] = fields[..] else {
                return Err(format!("line {number}: expected <old> <new> <ref>"));
            };
            for id in [old, new] {
                if id.len() != 40 || !id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
                    return Err(format!("line {number}: invalid object id"));
                }
            }
            kajiba_core::names::validate_ref(name)
                .map_err(|err| format!("line {number}: {err}"))?;
            Ok(RefUpdate {
                ref_name: name.into(),
                old_id: old.into(),
                new_id: new.into(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ZERO: &str = "0000000000000000000000000000000000000000";
    const A: &str = "1111111111111111111111111111111111111111";
    const B: &str = "abcdef0123456789abcdef0123456789abcdef01";

    fn update(name: &str, old: &str, new: &str) -> RefUpdate {
        RefUpdate {
            ref_name: name.into(),
            old_id: old.into(),
            new_id: new.into(),
        }
    }

    #[test]
    fn parses_updates_in_order_including_zero_ids() {
        let input = format!(
            "{ZERO} {A} refs/heads/main\n{B} {ZERO} refs/tags/v1\n{A} {B} refs/heads/feature/x\n"
        );
        assert_eq!(
            updates(input.as_bytes()).unwrap(),
            [
                update("refs/heads/main", ZERO, A),
                update("refs/tags/v1", B, ZERO),
                update("refs/heads/feature/x", A, B),
            ]
        );
    }

    #[tokio::test]
    async fn input_limit_accepts_one_mib_and_refuses_one_byte_more_before_connecting() {
        let limit = MAX_INPUT as usize;
        assert_eq!(limit, 1_048_576);
        let line = format!("{ZERO} {A} refs/heads/main\n");
        let mut input = line.repeat(limit / line.len());
        input.insert_str(input.len() - 1, &"x".repeat(limit - input.len()));
        assert_eq!(input.len(), limit);
        let mut oversized = input.clone();
        oversized.insert(oversized.len() - 1, 'x');
        assert_eq!(oversized.len(), limit + 1);
        let env = |var: &str| match var {
            "KAJIBA_USER" | "KAJIBA_REPO" | "KAJIBA_SESSION" => {
                Some("01ARZ3NDEKTSV4RRFFQ69G5FAV".into())
            }
            _ => None,
        };
        let temp = crate::state::tests::TempDir::new();
        let missing = temp.0.join("missing.sock");
        for kind in [Kind::PreReceive, Kind::PostReceive] {
            // Valid input reaches the connection attempt; oversized input does not.
            assert!(
                call(&missing, kind, input.as_bytes(), &env)
                    .await
                    .unwrap_err()
                    .contains("the git daemon is unavailable")
            );
            assert_eq!(
                call(&missing, kind, oversized.as_bytes(), &env)
                    .await
                    .unwrap_err(),
                "too many ref updates"
            );
        }
    }

    #[test]
    fn refuses_malformed_updates() {
        let upper = A.replace('1', "A");
        for input in [
            String::new(),
            "\n".into(),
            format!("{ZERO} {A} refs/heads/main"),
            format!("{ZERO} {A} refs/heads/main\n\n"),
            format!("{ZERO} {A}\n"),
            format!("{ZERO} {A} refs/heads/main extra\n"),
            format!("{ZERO}  {A} refs/heads/main\n"),
            format!("{ZERO}\t{A} refs/heads/main\n"),
            format!("{ZERO} {A} refs/heads/main\r\n"),
            format!("{ZERO} {} refs/heads/main\n", &A[1..]),
            format!("{ZERO} {upper} refs/heads/main\n"),
            format!("{ZERO} {A}{A} refs/heads/main\n"),
            format!("{ZERO} {A} main\n"),
            format!("{ZERO} {A} refs/heads/../x\n"),
            format!("{ZERO} {A} refs/heads/a..b\n"),
            format!("{ZERO} {A} refs/heads/x\0\n"),
        ] {
            assert!(updates(input.as_bytes()).is_err(), "{input:?}");
        }
        assert!(updates(&[0xff, b'\n']).is_err());
    }

    #[test]
    fn context_requires_public_ids_and_absolute_quarantine_paths() {
        let id = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
        for value in [
            None,
            Some(""),
            Some("alice"),
            Some("81ARZ3NDEKTSV4RRFFQ69G5FAV"),
        ]
        .into_iter()
        .chain([
            Some("01arz3ndektsv4rrffq69g5fav"),
            Some("01ARZ3NDEKTSV4RRFFQ69G5FAU7"),
        ]) {
            let env = |_: &str| value.map(String::from);
            assert!(public_id(&env, "KAJIBA_USER").is_err(), "{value:?}");
        }
        assert_eq!(public_id(&|_| Some(id.into()), "KAJIBA_USER").unwrap(), id);
        let absent = quarantine(&|_| None).unwrap();
        assert_eq!(absent, Quarantine::default());
        let env = |var: &str| {
            Some(match var {
                "GIT_OBJECT_DIRECTORY" => "/srv/r.git/objects/incoming-x".into(),
                "GIT_ALTERNATE_OBJECT_DIRECTORIES" => "/srv/r.git/objects".into(),
                _ => "/srv/r.git/objects/incoming-x".into(),
            })
        };
        assert_eq!(
            quarantine(&env).unwrap().alternate_object_directories,
            "/srv/r.git/objects"
        );
        for var in ["GIT_OBJECT_DIRECTORY", "GIT_QUARANTINE_PATH"] {
            let env = |name: &str| Some(if name == var { "objects/x" } else { "/x" }.into());
            assert!(quarantine(&env).is_err(), "{var}");
        }
    }
}
