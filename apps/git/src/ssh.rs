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
mod session;

use kajiba_proto::{gate::v1 as gate, git::v1::SessionKind};
use std::{io, os::fd::OwnedFd, path::Path, process::ExitCode, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
};

const SESSIONS: usize = 32;
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);
const LIFETIME: Duration = Duration::from_secs(60 * 60);

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
        // The push's CI runs (ADR 0132), on the session's stderr; a daemon from before
        // them sends nothing.
        if let Ok(notice) = handoff::read_text(&mut stream, u16::MAX.into()).await {
            let _ = std::io::Write::write_all(&mut std::io::stderr(), notice.as_bytes());
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
    mut listener: impl tonic::codegen::tokio_stream::Stream<Item = io::Result<UnixStream>> + Unpin,
    git: Arc<crate::service::Git>,
    hooks: &Path,
    session_socket: &Path,
    shutdown: impl std::future::Future<Output = ()>,
) -> io::Result<()> {
    use tonic::codegen::tokio_stream::StreamExt;
    let permits = Arc::new(tokio::sync::Semaphore::new(SESSIONS));
    let mut sessions = tokio::task::JoinSet::new();
    tokio::pin!(shutdown);
    loop {
        let stream = tokio::select! {
            biased;
            () = &mut shutdown => break,
            Some(_) = sessions.join_next(), if !sessions.is_empty() => continue,
            stream = listener.next() => match stream { Some(stream) => stream, None => break },
        };
        let mut stream = stream?;
        let Ok(permit) = permits.clone().try_acquire_owned() else {
            // Never queue or spawn work for excess connections, even incomplete hand-offs.
            let message = b"SSH session limit reached; retry later";
            let mut response = vec![1];
            response.extend_from_slice(&(message.len() as u16).to_be_bytes());
            response.extend_from_slice(message);
            let _ = stream.try_write(&response);
            continue;
        };
        let git = git.clone();
        let hooks = hooks.to_owned();
        let session_socket = session_socket.to_owned();
        // The supervisor owns the child, independently of the helper's connection.
        sessions.spawn(async move {
            let _permit = permit;
            let result = async {
                let (fds, user, command) =
                    tokio::time::timeout(Duration::from_secs(5), receive(&mut stream))
                        .await
                        .map_err(|_| "SSH hand-off timed out".to_owned())?
                        .map_err(|e| e.to_string())?;
                spawn(&git, &hooks, &session_socket, fds, user, command)
                    .await
                    .map_err(|e| e.message().to_owned())
            }
            .await;
            let (status, message, runs) = match result {
                Ok((status, runs)) => (status, String::new(), runs),
                Err(message) => (1, message, Vec::new()),
            };
            let _ = tokio::time::timeout(Duration::from_secs(5), async {
                stream.write_u8(status).await?;
                handoff::write_text(&mut stream, &message).await?;
                handoff::write_text(&mut stream, &notice(&runs)).await
            })
            .await;
        });
    }
    drop(listener);
    while sessions.join_next().await.is_some() {}
    Ok(())
}

/// One line per CI run a push created (ADR 0132), within a hand-off message's bound.
fn notice(runs: &[gate::PushRun]) -> String {
    let mut text = String::new();
    for run in runs {
        let line = format!(
            "kajiba: CI run {id} on {reference} — kajiba ci watch {id}\n",
            id = run.run_public_id,
            reference = run.ref_name
        );
        if text.len() + line.len() > usize::from(u16::MAX) {
            break;
        }
        text += &line;
    }
    text
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
    session_socket: &Path,
    fds: [OwnedFd; 3],
    user: String,
    command: GitCommand,
) -> Result<(u8, Vec<gate::PushRun>), tonic::Status> {
    use kajiba_gitstore::{GitStore, Gix};
    use std::os::unix::process::ExitStatusExt;
    let mut server = crate::service::server_client(&git.server_socket, &git.generation).await?;
    let pending = kajiba_proto::git::v1::Session {
        session_id: kajiba_core::new_id().map_err(|e| tonic::Status::internal(e.to_string()))?,
        kind: command.service.kind().into(),
        user_public_id: user,
        correlation_id: kajiba_core::new_id()
            .map_err(|e| tonic::Status::internal(e.to_string()))?,
        generation: git.generation.clone(),
        ..Default::default()
    };
    let opened = async {
        let opened = server
            .open_session(gate::OpenSessionRequest {
                user_public_id: pending.user_public_id.clone(),
                kind: pending.kind,
                repository: Some(gate::open_session_request::Repository::RepoAddress(
                    format!("{}/{}", command.owner, command.name),
                )),
                correlation_id: pending.correlation_id.clone(),
                session_id: pending.session_id.clone(),
                generation: pending.generation.clone(),
            })
            .await?
            .into_inner();
        let session = opened
            .session
            .ok_or_else(|| tonic::Status::internal("server returned no session"))?;
        if session.session_id != pending.session_id {
            return Err(tonic::Status::internal(
                "server returned a different session",
            ));
        }
        git.repositories
            .register(session.clone(), opened.allowed_signers)?;
        Ok::<_, tonic::Status>(session)
    }
    .await;
    let session = match opened {
        Ok(session) => session,
        Err(err) => {
            git.repositories.cancelled(pending);
            return Err(err);
        }
    };
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
        let mut process =
            tokio::process::Command::from(Gix.transport(service, &path, &[], Some(hooks)));
        process
            .env(crate::local::SESSION_SOCKET_ENV, session_socket)
            .env("KAJIBA_USER", &session.user_public_id)
            .env("KAJIBA_REPO", &session.repo_public_id)
            .env("KAJIBA_SESSION", &session.session_id);
        let status = session::run(
            &mut process,
            fds,
            &session,
            IDLE_TIMEOUT,
            LIFETIME,
            &mut exited,
        )
        .await
        .map_err(|e| tonic::Status::internal(e.to_string()))?;
        Ok(status
            .code()
            .unwrap_or_else(|| 128 + status.signal().unwrap_or(1)) as u8)
    }
    .await;
    // A wait error is not proof of exit: retain the session until a new generation.
    if !exited {
        return result.map(|status| (status, Vec::new()));
    }
    git.repositories.finished(
        &session.session_id,
        result.as_ref().copied().unwrap_or(1).into(),
    );
    // Retained by finished() for retry if the server is restarting.
    let mut runs = Vec::new();
    if let Ok(response) = server
        .session_exited(gate::SessionExitedRequest {
            session_id: session.session_id.clone(),
            exit_status: result.as_ref().copied().unwrap_or(1).into(),
        })
        .await
    {
        git.repositories.acknowledged(&session.session_id);
        runs = response.into_inner().runs;
    }
    result.map(|status| (status, runs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_notice_names_each_run_within_the_hand_off_bound() {
        let run = |id: &str, reference: &str| gate::PushRun {
            run_public_id: id.into(),
            ref_name: reference.into(),
        };
        assert_eq!(notice(&[]), "");
        assert_eq!(
            notice(&[
                run("01JBRUN0000000000000000001", "refs/heads/main"),
                run("01JBRUN0000000000000000002", "refs/tags/v1"),
            ]),
            "kajiba: CI run 01JBRUN0000000000000000001 on refs/heads/main — kajiba ci watch 01JBRUN0000000000000000001\n\
             kajiba: CI run 01JBRUN0000000000000000002 on refs/tags/v1 — kajiba ci watch 01JBRUN0000000000000000002\n"
        );
        let many = vec![
            run(
                "01JBRUN0000000000000000001",
                &format!("refs/tags/{}", "x".repeat(200))
            );
            1000
        ];
        let text = notice(&many);
        assert!(text.len() <= usize::from(u16::MAX) && text.ends_with('\n'));
    }

    fn command(service: Service, owner: &str, name: &str) -> Option<GitCommand> {
        Some(GitCommand {
            service,
            owner: owner.into(),
            name: name.into(),
        })
    }

    #[tokio::test]
    async fn stopping_accepts_no_more_handoffs_and_waits_for_the_active_one() {
        use tonic::codegen::tokio_stream;
        let (mut peer, stream) = UnixStream::pair().unwrap();
        let git = Arc::new(crate::service::Git {
            generation: kajiba_core::new_id().unwrap(),
            server_socket: "/absent.sock".into(),
            repositories: Arc::new(crate::repositories::Repositories::new("/absent".into())),
        });
        let mut daemon = tokio::spawn(async move {
            serve(
                tokio_stream::iter([Ok(stream)]),
                git,
                Path::new("/absent"),
                Path::new("/absent.sock"),
                std::future::pending(),
            )
            .await
        });
        // Input exhaustion stops accepting; the accepted handoff still owns its response.
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut daemon)
                .await
                .is_err()
        );
        peer.write_u8(0).await.unwrap();
        assert_eq!(peer.read_u8().await.unwrap(), 1);
        assert!(
            handoff::read_text(&mut peer, u16::MAX.into())
                .await
                .unwrap()
                .contains("descriptor")
        );
        tokio::time::timeout(Duration::from_secs(1), daemon)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn session_cap_refuses_excess_handoffs_and_releases_slots() {
        use tonic::codegen::tokio_stream::wrappers::UnboundedReceiverStream;
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let git = Arc::new(crate::service::Git {
            generation: kajiba_core::new_id().unwrap(),
            server_socket: "/absent.sock".into(),
            repositories: Arc::new(crate::repositories::Repositories::new("/absent".into())),
        });
        let daemon = tokio::spawn(async move {
            serve(
                UnboundedReceiverStream::new(receiver),
                git,
                Path::new("/absent"),
                Path::new("/absent.sock"),
                std::future::pending(),
            )
            .await
        });
        tokio::time::timeout(Duration::from_secs(3), async {
            let mut held = Vec::new();
            for _ in 0..SESSIONS {
                let (peer, stream) = UnixStream::pair().unwrap();
                sender.send(Ok(stream)).unwrap();
                held.push(peer);
            }
            let (mut excess, stream) = UnixStream::pair().unwrap();
            sender.send(Ok(stream)).unwrap();
            let mut refusal = Vec::new();
            excess.read_to_end(&mut refusal).await.unwrap();
            assert!(refusal.is_empty() || refusal[0] == 1);
            // Invalid hand-offs release every slot without reaching the absent gate.
            for mut peer in held {
                peer.shutdown().await.unwrap();
                peer.read_to_end(&mut Vec::new()).await.unwrap();
            }
            let (mut peer, stream) = UnixStream::pair().unwrap();
            sender.send(Ok(stream)).unwrap();
            peer.write_u8(0).await.unwrap(); // Missing SCM_RIGHTS: admitted, then rejected.
            assert_eq!(peer.read_u8().await.unwrap(), 1);
            let message = handoff::read_text(&mut peer, u16::MAX.into())
                .await
                .unwrap();
            assert!(!message.contains("limit"), "{message}");
            assert!(message.contains("descriptor"), "{message}");
            drop(sender);
            daemon.await.unwrap().unwrap();
        })
        .await
        .expect("session cap queued excess work or leaked permits");
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

#[cfg(test)]
mod opening_tests {
    use super::*;
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };
    use tonic::{Request, Response, Status};

    #[derive(Default)]
    struct LostReply {
        opened: Mutex<Option<String>>,
        exits: AtomicUsize,
    }

    #[tonic::async_trait]
    impl gate::gate_service_server::GateService for LostReply {
        async fn hello(
            &self,
            _: Request<gate::HelloRequest>,
        ) -> Result<Response<gate::HelloResponse>, Status> {
            Ok(Response::new(gate::HelloResponse {
                version: env!("CARGO_PKG_VERSION").into(),
            }))
        }
        async fn open_session(
            &self,
            request: Request<gate::OpenSessionRequest>,
        ) -> Result<Response<gate::OpenSessionResponse>, Status> {
            let request = request.into_inner();
            assert!(kajiba_core::names::is_public_id(&request.session_id));
            assert!(kajiba_core::names::is_public_id(&request.generation));
            *self.opened.lock().unwrap() = Some(request.session_id);
            Err(Status::unavailable("reply lost after registration"))
        }
        async fn session_exited(
            &self,
            request: Request<gate::SessionExitedRequest>,
        ) -> Result<Response<gate::SessionExitedResponse>, Status> {
            let mut opened = self.opened.lock().unwrap();
            assert_eq!(opened.as_ref(), Some(&request.into_inner().session_id));
            if self.exits.fetch_add(1, Ordering::SeqCst) == 0 {
                return Err(Status::unavailable("cancellation reply lost"));
            }
            *opened = None;
            Ok(Response::new(gate::SessionExitedResponse::default()))
        }
        async fn get_auth_key(
            &self,
            _: Request<gate::GetAuthKeyRequest>,
        ) -> Result<Response<gate::GetAuthKeyResponse>, Status> {
            Err(Status::unimplemented("unused"))
        }
        async fn pre_receive(
            &self,
            _: Request<gate::PreReceiveRequest>,
        ) -> Result<Response<gate::PreReceiveResponse>, Status> {
            panic!("no child may start without an open response")
        }
        async fn post_receive(
            &self,
            _: Request<gate::PostReceiveRequest>,
        ) -> Result<Response<gate::PostReceiveResponse>, Status> {
            panic!("no child may start without an open response")
        }
    }

    #[tokio::test]
    async fn lost_open_reply_queues_cancellation_until_acknowledged() {
        let dir = crate::state::tests::TempDir::new();
        let git = Arc::new(crate::service::Git {
            repositories: Arc::new(crate::repositories::Repositories::new(dir.0.clone())),
            generation: kajiba_core::new_id().unwrap(),
            server_socket: dir.0.join("gate.sock"),
        });
        let local = kajiba_core::local::Local::bind(
            &git.server_socket,
            Arc::default(),
            0o600,
            vec![kajiba_core::local::effective_uid()],
        )
        .unwrap();
        let gate = Arc::new(LostReply::default());
        let server = tokio::spawn(local.serve(
            tonic::service::Routes::new(gate::gate_service_server::GateServiceServer::from_arc(
                gate.clone(),
            )),
            std::future::pending(),
        ));
        let fds = std::array::from_fn(|_| std::fs::File::open("/dev/null").unwrap().into());
        let result = spawn(
            &git,
            &dir.0,
            &dir.0.join("session.sock"),
            fds,
            kajiba_core::new_id().unwrap(),
            GitCommand {
                service: Service::ReceivePack,
                owner: "alice".into(),
                name: "demo".into(),
            },
        )
        .await;
        assert_eq!(result.unwrap_err().code(), tonic::Code::Unavailable);
        assert!(git.repositories.sessions().is_empty());
        let exits = git.repositories.exits();
        assert_eq!(exits.len(), 1);
        assert_eq!(
            Some(&exits[0].0.session_id),
            gate.opened.lock().unwrap().as_ref()
        );
        let monitor = tokio::spawn({
            let git = git.clone();
            async move {
                crate::service::connect_server(
                    &git.server_socket,
                    &git.generation,
                    &git.repositories,
                )
                .await
            }
        });
        tokio::time::timeout(Duration::from_secs(5), async {
            while !git.repositories.exits().is_empty() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert!(gate.opened.lock().unwrap().is_none());
        assert_eq!(gate.exits.load(Ordering::SeqCst), 2);
        monitor.abort();
        server.abort();
        let _ = monitor.await;
        let _ = server.await;
    }
}
