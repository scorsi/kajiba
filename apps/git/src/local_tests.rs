use crate::state::tests::TempDir;
use kajiba_core::local::{Local, effective_uid};
use kajiba_proto::hook::v1::{
    hook_service_server::{HookService, HookServiceServer},
    *,
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tonic::{Request, Response, Status};

/// Counts calls and answers PreReceive with a fixed verdict.
struct FakeHook {
    calls: Arc<AtomicUsize>,
    allowed: bool,
    seen: Arc<std::sync::Mutex<Vec<PreReceiveRequest>>>,
}

#[tonic::async_trait]
impl HookService for FakeHook {
    async fn get_auth_key(
        &self,
        _: Request<GetAuthKeyRequest>,
    ) -> Result<Response<GetAuthKeyResponse>, Status> {
        Err(Status::unimplemented("fake"))
    }

    async fn pre_receive(
        &self,
        request: Request<PreReceiveRequest>,
    ) -> Result<Response<PreReceiveResponse>, Status> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.seen.lock().unwrap().push(request.into_inner());
        Ok(Response::new(PreReceiveResponse {
            allowed: self.allowed,
            refusals: vec![Refusal {
                ref_name: "refs/heads/main".into(),
                reason: "unsigned commit".into(),
            }],
        }))
    }

    async fn post_receive(
        &self,
        request: Request<PostReceiveRequest>,
    ) -> Result<Response<PostReceiveResponse>, Status> {
        let request = request.into_inner();
        assert_eq!(request.user_public_id, "01ARZ3NDEKTSV4RRFFQ69G5FAV");
        assert_eq!(request.repo_public_id, "01BX5ZZKBKACTAV9WEVGEMMVRZ");
        assert_eq!(request.session_id, "01ARZ3NDEKTSV4RRFFQ69G5FAW");
        assert_eq!(
            request.updates,
            crate::hook::updates(HOOK_INPUT.as_bytes()).unwrap()
        );
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(Response::new(PostReceiveResponse {}))
    }
}

const HOOK_INPUT: &str = "0000000000000000000000000000000000000000 1111111111111111111111111111111111111111 refs/heads/main\n";

fn hook_env(var: &str) -> Option<String> {
    match var {
        "KAJIBA_USER" => Some("01ARZ3NDEKTSV4RRFFQ69G5FAV".into()),
        "KAJIBA_SESSION" => Some("01ARZ3NDEKTSV4RRFFQ69G5FAW".into()),
        "KAJIBA_REPO" => Some("01BX5ZZKBKACTAV9WEVGEMMVRZ".into()),
        "GIT_OBJECT_DIRECTORY" | "GIT_QUARANTINE_PATH" => {
            Some("/srv/r.git/objects/incoming-x".into())
        }
        "GIT_ALTERNATE_OBJECT_DIRECTORIES" => Some("/srv/r.git/objects".into()),
        _ => None,
    }
}

#[tokio::test]
async fn hooks_never_fall_back_to_the_activated_socket_when_their_generation_is_gone() {
    let dir = TempDir::new();
    let socket = dir.0.join("local.sock");
    // Leave this listener unaccepted, as while systemd waits for the next instance.
    let _activated = tokio::net::UnixListener::bind(&socket).unwrap();
    let missing = dir.0.join("gone.sock");
    let env = |var: &str| {
        if var == crate::local::SESSION_SOCKET_ENV {
            Some(missing.to_str().unwrap().to_owned())
        } else {
            hook_env(var)
        }
    };
    for kind in [
        crate::hook::Kind::PreReceive,
        crate::hook::Kind::PostReceive,
    ] {
        assert_eq!(
            tokio::time::timeout(
                std::time::Duration::from_secs(1),
                crate::hook::run(&socket, kind, HOOK_INPUT.as_bytes(), &env),
            )
            .await
            .expect("hook waited for another generation"),
            std::process::ExitCode::FAILURE
        );
    }
}

#[tokio::test]
async fn hooks_make_one_call_per_invocation_and_send_nothing_when_malformed() {
    use crate::hook::{Kind, run};
    use std::process::ExitCode;
    for allowed in [true, false] {
        let dir = TempDir::new();
        let local = Local::bind(
            &dir.0.join("local.sock"),
            Arc::default(),
            0o660,
            vec![effective_uid()],
        )
        .unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = Arc::default();
        let fake = FakeHook {
            calls: calls.clone(),
            allowed,
            seen: Arc::clone(&seen),
        };
        let server = tokio::spawn(
            tonic::transport::Server::builder()
                .add_service(HookServiceServer::new(fake))
                .serve_with_incoming(local),
        );
        let socket = dir.0.join("local.sock");
        let expected = if allowed {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        };
        assert_eq!(
            run(&socket, Kind::PreReceive, HOOK_INPUT.as_bytes(), &hook_env).await,
            expected
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        {
            let seen = seen.lock().unwrap();
            let request = &seen[0];
            assert_eq!(request.user_public_id, "01ARZ3NDEKTSV4RRFFQ69G5FAV");
            assert_eq!(request.repo_public_id, "01BX5ZZKBKACTAV9WEVGEMMVRZ");
            assert_eq!(request.session_id, "01ARZ3NDEKTSV4RRFFQ69G5FAW");
            assert_eq!(request.updates.len(), 1);
            assert_eq!(request.updates[0].ref_name, "refs/heads/main");
            let quarantine = request.quarantine.as_ref().unwrap();
            assert_eq!(quarantine.quarantine_path, "/srv/r.git/objects/incoming-x");
            assert_eq!(
                quarantine.alternate_object_directories,
                "/srv/r.git/objects"
            );
        }
        assert_eq!(
            run(&socket, Kind::PostReceive, HOOK_INPUT.as_bytes(), &hook_env).await,
            ExitCode::SUCCESS
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        // Malformed stdin or context: refused before any call.
        for kind in [Kind::PreReceive, Kind::PostReceive] {
            assert_eq!(
                run(&socket, kind, "garbage\n".as_bytes(), &hook_env).await,
                ExitCode::FAILURE
            );
            assert_eq!(
                run(&socket, kind, HOOK_INPUT.as_bytes(), &|_| None).await,
                ExitCode::FAILURE
            );
        }
        for kind in [Kind::PreReceive, Kind::PostReceive] {
            for session in [
                None,
                Some(""),
                Some("invalid"),
                Some("01arz3ndektsv4rrffq69g5faw"),
            ] {
                let env = |var: &str| {
                    if var == "KAJIBA_SESSION" {
                        session.map(String::from)
                    } else {
                        hook_env(var)
                    }
                };
                assert_eq!(
                    run(&socket, kind, HOOK_INPUT.as_bytes(), &env).await,
                    ExitCode::FAILURE
                );
            }
        }
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        server.abort();
    }
}
