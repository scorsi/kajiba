//! `serve-ssh`'s reading of `SSH_ORIGINAL_COMMAND` (ADR 0004, 0021). Nothing here
//! evaluates the command through a shell.

#[derive(Debug, PartialEq, Eq)]
pub enum Service {
    UploadPack,
    ReceivePack,
}

/// A Git transport command on a repository address, names validated and lowercase.
#[derive(Debug, PartialEq, Eq)]
pub struct GitCommand {
    pub service: Service,
    pub owner: String,
    pub name: String,
}

/// Accepts exactly `git-upload-pack '<path>'` or `git-receive-pack '<path>'`, the form
/// Git's SSH transport sends, where `<path>` is `<owner>/<name>` with an optional leading
/// `/` (`ssh://` URLs) and an optional `.git`. Anything else is `None`.
pub fn parse(command: &str) -> Option<GitCommand> {
    let (program, argument) = command.split_once(' ')?;
    let service = match program {
        "git-upload-pack" => Service::UploadPack,
        "git-receive-pack" => Service::ReceivePack,
        _ => return None,
    };
    // Valid names contain no quote, so a valid path needs no unquoting: any quote,
    // space or other shell character left inside fails name validation below.
    let path = argument.strip_prefix('\'')?.strip_suffix('\'')?;
    let (owner, name) = kajiba_core::names::repo_address(path)?;
    Some(GitCommand {
        service,
        owner,
        name,
    })
}

mod handoff;

use kajiba_core::local::Local;
use kajiba_proto::{gate::v1 as gate, git::v1::SessionKind};
use std::{io, os::fd::OwnedFd, path::Path, process::ExitCode, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
};

pub const SOCKET_PATH: &str = "/run/kajiba-git/ssh.sock";

impl Service {
    fn name(&self) -> &'static str {
        match self {
            Self::UploadPack => "upload-pack",
            Self::ReceivePack => "receive-pack",
        }
    }
    fn kind(&self) -> SessionKind {
        match self {
            Self::UploadPack => SessionKind::UploadPack,
            Self::ReceivePack => SessionKind::ReceivePack,
        }
    }
}

pub async fn run(path: &Path, user: &str, original: &str) -> ExitCode {
    let result = async {
        if !kajiba_core::names::is_public_id(user) {
            return Err("invalid user public id".to_owned());
        }
        let command = parse(original).ok_or("invalid SSH command")?;
        let mut stream = UnixStream::connect(path)
            .await
            .map_err(|_| "git daemon is unavailable")?;
        handoff::send_fds(&stream, &[0, 1, 2])
            .await
            .map_err(|e| e.to_string())?;
        handoff::write_text(
            &mut stream,
            &format!(
                "{user}\n{}\n{}/{}",
                command.service.name(),
                command.owner,
                command.name
            ),
        )
        .await
        .map_err(|e| e.to_string())?;
        let status = stream
            .read_u8()
            .await
            .map_err(|_| "git daemon disconnected before session exit")?;
        let message = handoff::read_text(&mut stream, u16::MAX.into())
            .await
            .map_err(|e| e.to_string())?;
        if !message.is_empty() {
            return Err(message);
        }
        Ok(status)
    }
    .await;
    match result {
        Ok(code) => ExitCode::from(code),
        Err(message) => {
            kajiba_core::log::error("fatal", &format!("serve-ssh: {message}"));
            ExitCode::FAILURE
        }
    }
}

pub async fn serve(
    mut listener: Local,
    git: Arc<crate::service::Git>,
    hooks: &Path,
) -> io::Result<()> {
    use tonic::codegen::tokio_stream::StreamExt;
    while let Some(stream) = listener.next().await {
        let mut stream = stream?;
        let git = git.clone();
        let hooks = hooks.to_owned();
        // The supervisor owns the child, independently of the helper's connection.
        tokio::spawn(async move {
            let result = async {
                let (fds, user, command) =
                    tokio::time::timeout(Duration::from_secs(5), receive(&mut stream))
                        .await
                        .map_err(|_| "SSH hand-off timed out".to_owned())?
                        .map_err(|e| e.to_string())?;
                spawn(&git, &hooks, fds, user, command)
                    .await
                    .map_err(|e| e.message().to_owned())
            }
            .await;
            let (status, message) = match result {
                Ok(status) => (status, String::new()),
                Err(message) => (1, message),
            };
            let _ = stream.write_u8(status).await;
            let _ = handoff::write_text(&mut stream, &message).await;
        });
    }
    Ok(())
}

async fn receive(stream: &mut UnixStream) -> io::Result<([OwnedFd; 3], String, GitCommand)> {
    let fds = handoff::receive_fds(stream).await?;
    // 26-byte ULID, two newlines, service and two 63-byte names plus slash.
    let message = handoff::read_text(stream, 167).await?;
    let fields: Vec<_> = message.split('\n').collect();
    if fields.len() != 3 || !kajiba_core::names::is_public_id(fields[0]) {
        return Err(io::Error::other("invalid SSH hand-off"));
    }
    let command = parse(&format!("git-{} '{}'", fields[1], fields[2]))
        .ok_or_else(|| io::Error::other("invalid SSH command"))?;
    Ok((fds, fields[0].into(), command))
}

async fn spawn(
    git: &crate::service::Git,
    hooks: &Path,
    fds: [OwnedFd; 3],
    user: String,
    command: GitCommand,
) -> Result<u8, tonic::Status> {
    use kajiba_gitstore::{GitStore, Gix};
    use std::os::unix::process::ExitStatusExt;
    let mut server = crate::service::server_client(&git.server_socket, &git.generation).await?;
    let opened = server
        .open_session(gate::OpenSessionRequest {
            user_public_id: user,
            kind: command.service.kind().into(),
            repository: Some(gate::open_session_request::Repository::RepoAddress(
                format!("{}/{}", command.owner, command.name),
            )),
            correlation_id: kajiba_core::new_id()
                .map_err(|e| tonic::Status::internal(e.to_string()))?,
            import_session_id: String::new(),
        })
        .await?
        .into_inner();
    let session = opened
        .session
        .ok_or_else(|| tonic::Status::internal("server returned no session"))?;
    git.repositories
        .register(session.clone(), opened.allowed_signers)?;
    let mut exited = true;
    let result = async {
        if session.generation != git.generation
            || session.kind != command.service.kind() as i32
            || !kajiba_core::names::is_public_id(&session.session_id)
        {
            return Err(tonic::Status::internal(
                "server returned an invalid session",
            ));
        }
        if command.service == Service::ReceivePack {
            use std::os::unix::fs::PermissionsExt;
            for name in ["pre-receive", "post-receive"] {
                if !std::fs::metadata(hooks.join(name))
                    .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
                {
                    return Err(tonic::Status::failed_precondition(format!(
                        "Git hook {name} is unavailable"
                    )));
                }
            }
        }
        let path = git.repositories.present(&session.repo_public_id)?;
        let service = match command.service {
            Service::UploadPack => kajiba_gitstore::Service::UploadPack,
            Service::ReceivePack => kajiba_gitstore::Service::ReceivePack,
        };
        let [stdin, stdout, stderr] = fds;
        let mut process =
            tokio::process::Command::from(Gix.transport(service, &path, &[], Some(hooks)));
        process
            .stdin(stdin)
            .stdout(stdout)
            .stderr(stderr)
            .env("KAJIBA_USER", &session.user_public_id)
            .env("KAJIBA_REPO", &session.repo_public_id)
            .env("KAJIBA_SESSION", &session.session_id)
            .kill_on_drop(true);
        let mut child = process
            .spawn()
            .map_err(|e| tonic::Status::internal(e.to_string()))?;
        drop(process);
        exited = false;
        tracing::info!(
            event = "session-started",
            request_id = session.correlation_id,
            session = session.session_id,
            kind = crate::metrics::kind_name(session.kind),
            repo = session.repo_public_id
        );
        let status = child
            .wait()
            .await
            .map_err(|e| tonic::Status::internal(e.to_string()))?;
        exited = true;
        Ok(status
            .code()
            .unwrap_or_else(|| 128 + status.signal().unwrap_or(1)) as u8)
    }
    .await;
    // A wait error is not proof of exit: retain the session until a new generation.
    if !exited {
        return result;
    }
    git.repositories.finished(
        &session.session_id,
        result.as_ref().copied().unwrap_or(1).into(),
    );
    // Retained by finished() for retry if the server is restarting.
    if server
        .session_exited(gate::SessionExitedRequest {
            session_id: session.session_id.clone(),
            exit_status: result.as_ref().copied().unwrap_or(1).into(),
        })
        .await
        .is_ok()
    {
        git.repositories.acknowledged(&session.session_id);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(service: Service, owner: &str, name: &str) -> Option<GitCommand> {
        Some(GitCommand {
            service,
            owner: owner.into(),
            name: name.into(),
        })
    }

    #[tokio::test]
    async fn helper_fails_promptly_when_daemon_disconnects_before_session_exit() {
        let dir = crate::state::tests::TempDir::new();
        let path = dir.0.join("ssh.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let user = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
        let daemon = async {
            let (mut stream, _) = listener.accept().await.unwrap();
            let (fds, received_user, command) = receive(&mut stream).await.unwrap();
            assert_eq!(received_user, user);
            assert_eq!(command.service, Service::ReceivePack);
            drop(stream);
            // Keep the SSH descriptors open: the helper must detect hand-off EOF itself.
            fds
        };
        let (status, _fds) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(run(&path, user, "git-receive-pack 'alice/demo'"), daemon)
        })
        .await
        .expect("serve-ssh hung after the daemon disconnected");
        assert_eq!(status, ExitCode::FAILURE);
    }

    #[tokio::test]
    async fn handoff_revalidates_user_service_and_address_before_admission() {
        use std::{fs::File, os::fd::AsRawFd};
        let user = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
        let name = "a".repeat(63);
        for (message, valid) in [
            (format!("{user}\nreceive-pack\n{name}/{name}"), true),
            (format!("{user}\nupload-pack\nalice/demo"), true),
            ("bad-user\nupload-pack\nalice/demo".into(), false),
            (format!("{user}\ngc\nalice/demo"), false),
            (format!("{user}\nreceive-pack\n../demo"), false),
            (format!("{user}\nreceive-pack\nalice/demo\nextra"), false),
        ] {
            let (mut sender, mut receiver) = UnixStream::pair().unwrap();
            let fd = File::open("/dev/null").unwrap();
            handoff::send_fds(&sender, &[fd.as_raw_fd(); 3])
                .await
                .unwrap();
            handoff::write_text(&mut sender, &message).await.unwrap();
            assert_eq!(receive(&mut receiver).await.is_ok(), valid, "{message}");
        }
    }

    #[test]
    fn accepts_both_git_commands_and_documented_path_forms() {
        for path in [
            "alice/demo",
            "/alice/demo",
            "alice/demo.git",
            "/Alice/Demo.git",
        ] {
            assert_eq!(
                parse(&format!("git-upload-pack '{path}'")),
                command(Service::UploadPack, "alice", "demo"),
                "{path}"
            );
            assert_eq!(
                parse(&format!("git-receive-pack '{path}'")),
                command(Service::ReceivePack, "alice", "demo"),
                "{path}"
            );
        }
        // Only one `.git` is optional: the name itself must not end in it.
        assert_eq!(parse("git-upload-pack 'alice/demo.git.git'"), None);
        assert_eq!(
            parse("git-upload-pack 'a.b/c-d_e.f'"),
            command(Service::UploadPack, "a.b", "c-d_e.f")
        );
    }

    #[test]
    fn refuses_every_other_command_and_malformed_arguments() {
        for refused in [
            "",
            "git-upload-pack",
            "git-upload-pack ",
            "git-upload-archive 'alice/demo'",
            "git upload-pack 'alice/demo'",
            "git-upload-pack-x 'alice/demo'",
            "GIT-UPLOAD-PACK 'alice/demo'",
            "sh -c 'id'",
            "ls",
            "scp -t alice/demo",
            // Malformed quoting.
            "git-upload-pack alice/demo",
            "git-upload-pack \"alice/demo\"",
            "git-upload-pack 'alice/demo",
            "git-upload-pack alice/demo'",
            "git-upload-pack ''",
            "git-upload-pack 'alice/de'\\''mo'",
            "git-upload-pack  'alice/demo'",
            "git-upload-pack\t'alice/demo'",
            // Extra arguments.
            "git-upload-pack 'alice/demo' 'bob/x'",
            "git-upload-pack --strict 'alice/demo'",
            "git-upload-pack 'alice/demo' ",
            // Paths outside <owner>/<name>.
            "git-upload-pack 'alice'",
            "git-upload-pack '/alice/demo/'",
            "git-upload-pack 'alice/demo/x'",
            "git-upload-pack '//alice/demo'",
            "git-upload-pack '~alice/demo'",
            "git-upload-pack '../alice/demo'",
            "git-upload-pack 'alice/../demo'",
            "git-upload-pack '/srv/repos/x.git'",
            "git-upload-pack 'raw/demo'",
            // Shell metacharacters.
            "git-upload-pack 'alice/demo;id'",
            "git-upload-pack 'alice/$(id)'",
            "git-upload-pack 'alice/`id`'",
            "git-upload-pack 'alice/demo|id'",
            "git-upload-pack 'alice/demo&&id'",
            "git-upload-pack 'alice/demo\nid'",
            "git-upload-pack 'alice/demo\0'",
            "git-upload-pack 'alice/*'",
            "git-upload-pack 'alice/demo > x'",
        ] {
            assert_eq!(parse(refused), None, "{refused:?}");
        }
    }
}
