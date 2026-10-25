use super::*;
use crate::transport::{local, tests::State};
use kajiba_core::local::{channel, effective_uid};
use kajiba_proto::gate::v1::gate_service_client::GateServiceClient;
use std::{fs, os::unix::fs::MetadataExt, time::Duration};
use tonic::Code;
use tonic_types::StatusExt;

#[tokio::test]
async fn hello_checks_exact_version_before_recording_a_canonical_generation() {
    let state = State::new();
    let admin = local::tests::admin(&state.health.state_dir, "").await;
    let gate = Gate {
        writer: admin.writer,
        generation: Mutex::default(),
    };
    let generation = kajiba_core::new_id().unwrap();
    for version in [
        String::new(),
        format!("{}-other", env!("CARGO_PKG_VERSION")),
        format!("{}+build", env!("CARGO_PKG_VERSION")),
    ] {
        let err = gate
            .hello(Request::new(HelloRequest {
                version: version.clone(),
                generation: generation.clone(),
            }))
            .await
            .unwrap_err();
        assert_eq!(err.code(), Code::FailedPrecondition);
        let details = err.get_error_details();
        let info = details.error_info().unwrap();
        assert_eq!(info.reason, "VERSION_INCOMPATIBLE");
        assert_eq!(info.domain, "kajiba");
        assert_eq!(info.metadata["request_id"].len(), 26);
        assert!(err.message().contains(&version));
        assert!(err.message().contains(env!("CARGO_PKG_VERSION")));
        assert!(gate.generation.lock().unwrap().is_none());
    }
    for invalid in [
        "",
        "../bad",
        "8ZZZZZZZZZZZZZZZZZZZZZZZZZ",
        "01arz3ndektsv4rrffq69g5fav",
    ] {
        let err = gate
            .hello(Request::new(HelloRequest {
                version: env!("CARGO_PKG_VERSION").into(),
                generation: invalid.into(),
            }))
            .await
            .unwrap_err();
        assert_eq!(err.code(), Code::InvalidArgument);
        assert_eq!(
            err.get_error_details()
                .bad_request()
                .unwrap()
                .field_violations[0]
                .field,
            "generation"
        );
        assert!(gate.generation.lock().unwrap().is_none());
    }
    let hello = gate
        .hello(Request::new(HelloRequest {
            version: env!("CARGO_PKG_VERSION").into(),
            generation: generation.clone(),
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(hello.version, env!("CARGO_PKG_VERSION"));
    assert_eq!(
        gate.generation.lock().unwrap().as_deref(),
        Some(generation.as_str())
    );
}

#[tokio::test]
async fn git_socket_has_its_own_peer_policy_mode_and_services() {
    let state = State::new();
    let dir = &state.health.state_dir;
    let path = dir.join("git.sock");
    let gid = fs::metadata(dir).unwrap().gid();
    for uid in [effective_uid(), effective_uid().wrapping_add(1)] {
        let local = bind(&path, Arc::default(), uid, gid).unwrap();
        let metadata = fs::metadata(&path).unwrap();
        assert_eq!(metadata.mode() & 0o777, 0o660);
        assert_eq!(metadata.uid(), effective_uid());
        assert_eq!(metadata.gid(), gid);
        let admin = local::tests::admin(&dir.join("db"), "").await;
        let gate = Gate {
            writer: admin.writer,
            generation: Mutex::default(),
        };
        let server = tokio::spawn(serve(local, gate, Arc::default(), std::future::pending()));
        let result = tokio::time::timeout(Duration::from_secs(2), async {
            let channel = channel(&path, Some(Duration::from_secs(1)))
                .await
                .map_err(|err| err.to_string())?;
            GateServiceClient::new(channel)
                .hello(HelloRequest {
                    version: env!("CARGO_PKG_VERSION").into(),
                    generation: kajiba_core::new_id().unwrap(),
                })
                .await
                .map_err(|err| err.to_string())
        })
        .await
        .unwrap();
        assert_eq!(result.is_ok(), uid == effective_uid(), "{result:?}");
        if uid == effective_uid() {
            let err = local::enroll(&path, "alice".into()).await.unwrap_err();
            assert_eq!(
                err.downcast_ref::<Status>().unwrap().code(),
                Code::Unimplemented
            );
        }
        server.abort();
        assert!(server.await.unwrap_err().is_cancelled());
        assert!(!path.exists());
    }
}

#[tokio::test]
async fn gate_auth_and_closed_hooks_leave_admin_socket_unchanged() {
    const KEY: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAINdamAGCsQq31Uv+08lkBzoO4XLz2qYjJa8CGmj3B1Ea";
    let state = State::new();
    let dir = &state.health.state_dir;
    let admin = local::tests::admin(
        dir,
        &format!("[users.alice]\nssh_keys=['{KEY} ignored comment']"),
    )
    .await;
    let gate = Gate {
        writer: admin.writer.clone(),
        generation: Mutex::default(),
    };
    let admin_path = dir.join("local.sock");
    let admin_server = tokio::spawn(local::serve(
        local::bind(&admin_path, Arc::default()).unwrap(),
        admin,
        Arc::default(),
        std::future::pending(),
    ));
    let gate_path = dir.join("git.sock");
    let gate_local = bind(
        &gate_path,
        Arc::default(),
        effective_uid(),
        fs::metadata(dir).unwrap().gid(),
    )
    .unwrap();
    let gate_server = tokio::spawn(serve(
        gate_local,
        gate,
        Arc::default(),
        std::future::pending(),
    ));
    let mut client = GateServiceClient::new(
        channel(&gate_path, Some(Duration::from_secs(2)))
            .await
            .unwrap(),
    );
    let found = client
        .get_auth_key(GetAuthKeyRequest {
            fingerprint: "SHA256:bbXpuKG6zhzdmnxq256TlqzFBzRl2f6OOg722cYNbU8".into(),
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(found.public_key, KEY);
    assert!(kajiba_core::names::is_public_id(&found.user_public_id));
    let err = client
        .get_auth_key(GetAuthKeyRequest {
            fingerprint: "missing".into(),
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::NotFound);
    assert_eq!(
        err.get_error_details().error_info().unwrap().reason,
        "KEY_NOT_FOUND"
    );
    assert!(
        !client
            .pre_receive(PreReceiveRequest::default())
            .await
            .unwrap()
            .into_inner()
            .allowed
    );
    client
        .post_receive(PostReceiveRequest::default())
        .await
        .unwrap();
    assert_eq!(
        client
            .open_session(OpenSessionRequest::default())
            .await
            .unwrap_err()
            .code(),
        Code::Unimplemented
    );
    assert_eq!(fs::metadata(&admin_path).unwrap().mode() & 0o777, 0o600);
    assert!(
        local::enroll(&admin_path, "alice".into())
            .await
            .unwrap()
            .contains("/auth/enroll#")
    );
    let mut wrong = GateServiceClient::new(
        channel(&admin_path, Some(Duration::from_secs(2)))
            .await
            .unwrap(),
    );
    assert_eq!(
        wrong
            .hello(HelloRequest::default())
            .await
            .unwrap_err()
            .code(),
        Code::Unimplemented
    );
    admin_server.abort();
    gate_server.abort();
    let _ = admin_server.await;
    let _ = gate_server.await;
}
