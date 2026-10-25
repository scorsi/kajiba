use super::*;
use crate::transport::{local, tests::State};
use kajiba_proto::{gate::v1::gate_service_server::GateService, git::v1::Session};
use tonic::Request;

const A: &str = "1111111111111111111111111111111111111111";
const B: &str = "2222222222222222222222222222222222222222";

fn update(name: &str, old: &str, new: &str, fast_forward: bool) -> RefUpdate {
    RefUpdate {
        ref_name: name.into(),
        old_id: old.into(),
        new_id: new.into(),
        fast_forward: Some(fast_forward),
        signature: SignatureVerdict::Verified.into(),
        signature_reason: String::new(),
    }
}

#[test]
fn creation_fast_forward_force_and_deletion_role_matrix() {
    for name in ["refs/heads/topic", "refs/tags/v1"] {
        for (old, new, fast_forward, admin_only) in [
            (ZERO, A, false, false),
            (A, B, true, false),
            (A, B, false, true),
            (A, ZERO, false, true),
        ] {
            for role in [None, Some(Role::Read), Some(Role::Write), Some(Role::Admin)] {
                let mut change = update(name, old, new, fast_forward);
                if new == ZERO {
                    change.signature = SignatureVerdict::Unspecified.into();
                }
                let accepted =
                    role == Some(Role::Admin) || (role == Some(Role::Write) && !admin_only);
                assert_eq!(
                    refusal(&change, role, "main").is_none(),
                    accepted,
                    "{change:?}, {role:?}"
                );
            }
        }
    }
    for role in [Role::Write, Role::Admin] {
        assert!(
            refusal(
                &update("refs/heads/main", ZERO, A, false),
                Some(role),
                "main"
            )
            .is_none()
        );
        assert!(
            refusal(
                &update("refs/heads/main", A, ZERO, true),
                Some(role),
                "main"
            )
            .unwrap()
            .contains("default branch")
        );
    }
    assert!(
        refusal(
            &update("refs/heads/release/main", A, ZERO, false),
            Some(Role::Admin),
            "release/main"
        )
        .unwrap()
        .contains("default branch")
    );
    assert!(
        refusal(
            &update("refs/tags/main", A, ZERO, false),
            Some(Role::Admin),
            "main"
        )
        .is_none()
    );
}

#[test]
fn trusted_prefixes_and_shared_ref_syntax_are_required() {
    for name in [
        "refs/contrib/alice/topic",
        "refs/foo",
        "refs/notes/commits",
        "refs/heads-evil/topic",
    ] {
        assert!(
            refusal(&update(name, ZERO, A, false), Some(Role::Admin), "main")
                .unwrap()
                .contains("only refs/heads/*")
        );
    }
    for name in [
        "refs/heads/",
        "refs/heads/a..b",
        "refs/tags/x.lock",
        "refs/heads/a b",
    ] {
        assert_eq!(
            refusal(&update(name, ZERO, A, false), Some(Role::Admin), "main"),
            Some(validate_ref(name).unwrap_err().to_string())
        );
    }
    for id in [
        "",
        "0",
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        "../bad",
    ] {
        for (old, new) in [(id, A), (A, id)] {
            assert_eq!(
                refusal(
                    &update("refs/heads/topic", old, new, true),
                    Some(Role::Admin),
                    "main"
                ),
                Some("invalid object id".into())
            );
        }
    }
}

#[test]
fn missing_facts_and_unverified_signatures_fail_closed_even_for_admins() {
    for name in ["refs/heads/topic", "refs/tags/v1"] {
        for (old, new) in [(ZERO, A), (A, B), (A, ZERO)] {
            let mut change = update(name, old, new, false);
            change.fast_forward = None;
            assert_eq!(
                refusal(&change, Some(Role::Admin), "main"),
                Some("fast-forward fact required".into())
            );
            change.fast_forward = Some(false);
            for verdict in [
                SignatureVerdict::Unspecified as i32,
                SignatureVerdict::Refused as i32,
                99,
            ] {
                change.signature = verdict;
                assert_eq!(
                    refusal(&change, Some(Role::Admin), "main").is_none(),
                    new == ZERO
                );
            }
        }
    }
    let mut change = update("refs/heads/topic", A, B, true);
    change.signature = SignatureVerdict::Refused.into();
    change.signature_reason = format!("commit {B}: wrong signer");
    assert_eq!(
        refusal(&change, Some(Role::Admin), "main"),
        Some(format!(
            "signature verification failed: commit {B}: wrong signer"
        ))
    );
}

async fn fixture() -> (State, Gate, Session) {
    let state = State::new();
    let admin = local::tests::admin(
        &state.health.state_dir,
        "[users.alice]\n[users.bob]\n[users.carol]\nis_admin=true\n",
    )
    .await;
    let gate = Gate::new(&admin);
    let mut writer = gate.writer.lock().await;
    let repo = kajiba_core::new_id().unwrap();
    sqlx::query("INSERT INTO repo (public_id, owner_id, name, config_key, visibility, ci_enabled, default_branch, description, created_at)
        SELECT ?, id, 'demo', 'demo', 'public', 0, 'main', '', 0 FROM user WHERE name = 'alice'")
        .bind(&repo).execute(&mut *writer).await.unwrap();
    let generation = kajiba_core::new_id().unwrap();
    gate.admission
        .rebuild(&mut writer, &generation, vec![])
        .await
        .unwrap();
    let user = sqlx::query_scalar("SELECT public_id FROM user WHERE name = 'bob'")
        .fetch_one(&mut *writer)
        .await
        .unwrap();
    let session = Session {
        session_id: kajiba_core::new_id().unwrap(),
        kind: SessionKind::ReceivePack.into(),
        repo_public_id: repo,
        user_public_id: user,
        correlation_id: "policy-test".into(),
        generation,
    };
    gate.admission.opened(1, session.clone());
    drop(writer);
    (state, gate, session)
}

async fn sql(gate: &Gate, statement: &str) {
    sqlx::query(statement)
        .execute(&mut *gate.writer.lock().await)
        .await
        .unwrap();
}

#[tokio::test]
async fn pre_receive_rechecks_local_roles_and_default_branch_for_owner_and_global_admin() {
    let (_state, gate, mut session) = fixture().await;
    let mut request = PreReceiveRequest {
        session_id: session.session_id.clone(),
        updates: vec![update("refs/heads/topic", A, B, true)],
    };
    for (statement, accepted) in [
        ("DELETE FROM repo_permission", false),
        (
            "INSERT INTO repo_permission SELECT id, 1, 'write' FROM user WHERE name = 'bob'",
            true,
        ),
        (
            "UPDATE user SET kind = 'external' WHERE name = 'bob'",
            false,
        ),
        ("UPDATE repo_permission SET role = 'admin'", false),
        (
            "UPDATE user SET kind = 'local', disabled_at = 1 WHERE name = 'bob'",
            false,
        ),
        (
            "UPDATE user SET disabled_at = NULL WHERE name = 'bob'",
            true,
        ),
        ("UPDATE repo SET archived_at = 1", false),
        ("UPDATE repo SET archived_at = NULL", true),
        ("DELETE FROM repo_permission", false),
    ] {
        sql(&gate, statement).await;
        let refusals = gate.refusals(&request).await.unwrap();
        assert_eq!(refusals.is_empty(), accepted, "{statement}: {refusals:?}");
        let response = gate
            .pre_receive(Request::new(request.clone()))
            .await
            .unwrap()
            .into_inner();
        assert!(!response.allowed);
        if accepted {
            assert_eq!(response.refusals[0].reason, "pushes are not accepted yet");
        } else {
            assert_eq!(response.refusals[0].ref_name, "refs/heads/topic");
            assert!(response.refusals[0].reason.contains("local user"));
        }
    }
    for user in ["alice", "carol"] {
        session.user_public_id = sqlx::query_scalar("SELECT public_id FROM user WHERE name = ?")
            .bind(user)
            .fetch_one(&mut *gate.writer.lock().await)
            .await
            .unwrap();
        gate.admission.opened(1, session.clone());
        for change in [
            update("refs/heads/topic", A, B, false),
            update("refs/tags/v1", A, ZERO, false),
        ] {
            request.updates = vec![change];
            assert!(gate.refusals(&request).await.unwrap().is_empty(), "{user}");
        }
        request.updates = vec![update("refs/heads/main", A, ZERO, false)];
        let response = gate
            .pre_receive(Request::new(request.clone()))
            .await
            .unwrap()
            .into_inner();
        assert!(!response.allowed);
        assert_eq!(response.refusals[0].ref_name, "refs/heads/main");
        assert!(
            response.refusals[0].reason.contains("default branch"),
            "{user}"
        );
    }
    let pending: i64 = sqlx::query_scalar("SELECT count(*) FROM pending_push")
        .fetch_one(&mut *gate.writer.lock().await)
        .await
        .unwrap();
    assert_eq!(pending, 0);
}

#[tokio::test]
async fn policy_refusals_keep_ref_order_and_require_live_receive_pack_session() {
    let (_state, gate, session) = fixture().await;
    sql(
        &gate,
        "INSERT INTO repo_permission SELECT id, 1, 'write' FROM user WHERE name = 'bob'",
    )
    .await;
    let request = PreReceiveRequest {
        session_id: session.session_id.clone(),
        updates: vec![
            update("refs/contrib/bob/topic", ZERO, A, false),
            update("refs/heads/good", ZERO, A, false),
            RefUpdate {
                signature: SignatureVerdict::Unspecified.into(),
                ..update("refs/tags/v1", ZERO, B, false)
            },
        ],
    };
    let response = gate
        .pre_receive(Request::new(request.clone()))
        .await
        .unwrap()
        .into_inner();
    assert!(!response.allowed);
    assert_eq!(
        response
            .refusals
            .iter()
            .map(|r| r.ref_name.as_str())
            .collect::<Vec<_>>(),
        ["refs/contrib/bob/topic", "refs/tags/v1"]
    );
    for kind in [
        SessionKind::UploadPack,
        SessionKind::Import,
        SessionKind::Maintenance,
    ] {
        gate.admission.opened(
            1,
            Session {
                kind: kind.into(),
                ..session.clone()
            },
        );
        assert!(
            gate.refusals(&request)
                .await
                .unwrap()
                .iter()
                .all(|r| r.reason.contains("live receive-pack"))
        );
    }
    gate.admission.opened(
        1,
        Session {
            generation: kajiba_core::new_id().unwrap(),
            ..session.clone()
        },
    );
    assert_eq!(gate.refusals(&request).await.unwrap().len(), 3);
    gate.admission
        .session_exited(&mut *gate.writer.lock().await, &session.session_id)
        .await
        .unwrap();
    let refused = gate.refusals(&request).await.unwrap();
    assert_eq!(refused.len(), 3);
    assert!(
        refused
            .iter()
            .all(|r| r.reason.contains("live receive-pack"))
    );
    assert_eq!(
        gate.refusals(&PreReceiveRequest::default()).await.unwrap()[0].reason,
        "no ref updates"
    );
}
