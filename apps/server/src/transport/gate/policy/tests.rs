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
    let gate = Gate::new(&admin, Default::default());
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
        let refusals = gate
            .refusals(&mut *gate.writer.lock().await, &request)
            .await
            .unwrap();
        assert_eq!(refusals.is_empty(), accepted, "{statement}: {refusals:?}");
        let response = gate
            .pre_receive(Request::new(request.clone()))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(response.allowed, accepted);
        if accepted {
            assert!(response.refusals.is_empty());
            sql(&gate, "DELETE FROM pending_push").await;
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
            assert!(
                gate.refusals(&mut *gate.writer.lock().await, &request)
                    .await
                    .unwrap()
                    .is_empty(),
                "{user}"
            );
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
        gate.admission
            .session_exited(&mut *gate.writer.lock().await, &session.session_id)
            .await
            .unwrap();
        gate.admission.opened(
            1,
            Session {
                kind: kind.into(),
                ..session.clone()
            },
        );
        assert!(
            gate.refusals(&mut *gate.writer.lock().await, &request)
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
    assert_eq!(
        gate.refusals(&mut *gate.writer.lock().await, &request)
            .await
            .unwrap()
            .len(),
        3
    );
    gate.admission
        .session_exited(&mut *gate.writer.lock().await, &session.session_id)
        .await
        .unwrap();
    let refused = gate
        .refusals(&mut *gate.writer.lock().await, &request)
        .await
        .unwrap();
    assert_eq!(refused.len(), 3);
    assert!(
        refused
            .iter()
            .all(|r| r.reason.contains("live receive-pack"))
    );
    assert_eq!(
        gate.refusals(
            &mut *gate.writer.lock().await,
            &PreReceiveRequest::default()
        )
        .await
        .unwrap()[0]
            .reason,
        "no ref updates"
    );
}

async fn writable_fixture() -> (State, Gate, Session, PreReceiveRequest) {
    let (state, gate, session) = fixture().await;
    sql(
        &gate,
        "INSERT INTO repo_permission SELECT id, 1, 'admin' FROM user WHERE name = 'bob'",
    )
    .await;
    let request = PreReceiveRequest {
        session_id: session.session_id.clone(),
        updates: vec![
            update("refs/heads/z", ZERO, A, false),
            update("refs/heads/a", A, B, true),
            update("refs/tags/gone", B, ZERO, false),
        ],
    };
    (state, gate, session, request)
}

#[tokio::test]
async fn allowed_means_committed_identity_order_and_session_evidence() {
    use sqlx::Connection;
    let (state, gate, session, request) = writable_fixture().await;
    let mut reader = sqlx::SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new()
            .filename(state.health.state_dir.join("kajiba.sqlite"))
            .read_only(true),
    )
    .await
    .unwrap();
    // Concurrent duplicate calls must not admit twice or overwrite the evidence.
    let (first, second) = tokio::join!(
        gate.pre_receive(Request::new(request.clone())),
        gate.pre_receive(Request::new(request.clone())),
    );
    let responses = [first.unwrap().into_inner(), second.unwrap().into_inner()];
    assert_eq!(responses.iter().filter(|r| r.allowed).count(), 1);
    assert!(
        responses.iter().find(|r| !r.allowed).unwrap().refusals[0]
            .reason
            .contains("retry")
    );
    let (repo, user, correlation, updates, stored_session, generation): (String, String, String, String, String, String) = sqlx::query_as(
        "SELECT repo.public_id, user.public_id, correlation_id, updates, git_session_id, git_generation
         FROM pending_push JOIN repo ON repo.id = repo_id JOIN user ON user.id = principal_id"
    ).fetch_one(&mut reader).await.unwrap();
    assert_eq!(repo, session.repo_public_id);
    assert_eq!(user, session.user_public_id);
    assert_eq!(correlation, session.correlation_id);
    assert_eq!(stored_session, session.session_id);
    assert_eq!(generation, session.generation);
    let expected: Vec<_> = request
        .updates
        .iter()
        .map(|u| {
            serde_json::json!({
                "ref": u.ref_name, "old": u.old_id, "new": u.new_id
            })
        })
        .collect();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&updates).unwrap(),
        serde_json::json!(expected)
    );
    let events: i64 = sqlx::query_scalar("SELECT count(*) FROM event")
        .fetch_one(&mut reader)
        .await
        .unwrap();
    assert_eq!(events, 0);
    reader.close().await.unwrap();
}

#[tokio::test]
async fn one_bad_ref_or_signature_admits_nothing() {
    let (_state, gate, _session, request) = writable_fixture().await;
    for bad in [
        update("refs/contrib/bob/topic", ZERO, A, false),
        update("refs/heads/main", A, ZERO, false),
        RefUpdate {
            signature: SignatureVerdict::Refused.into(),
            signature_reason: format!("commit {A}: wrong signer"),
            ..request.updates[0].clone()
        },
        RefUpdate {
            signature: SignatureVerdict::Unspecified.into(),
            ..request.updates[0].clone()
        },
    ] {
        let mut request = request.clone();
        request.updates[1] = bad.clone();
        let response = gate
            .pre_receive(Request::new(request))
            .await
            .unwrap()
            .into_inner();
        assert!(!response.allowed);
        assert_eq!(response.refusals.len(), 1);
        assert_eq!(response.refusals[0].ref_name, bad.ref_name);
        let pending: i64 = sqlx::query_scalar("SELECT count(*) FROM pending_push")
            .fetch_one(&mut *gate.writer.lock().await)
            .await
            .unwrap();
        assert_eq!(pending, 0);
    }
}

#[tokio::test]
async fn upload_pack_does_not_block_mutation_admission_or_recovery() {
    for rebuild in [false, true] {
        let (_state, gate, session, _) = writable_fixture().await;
        let reader = Session {
            session_id: kajiba_core::new_id().unwrap(),
            kind: SessionKind::UploadPack.into(),
            ..session.clone()
        };
        let mut writer = gate.writer.lock().await;
        gate.admission
            .session_exited(&mut writer, &session.session_id)
            .await
            .unwrap();
        if rebuild {
            gate.admission
                .rebuild(&mut writer, &session.generation, vec![reader.clone()])
                .await
                .unwrap();
        } else {
            gate.admission.opened(1, reader.clone());
        }
        assert!(
            !gate.admission.running(1),
            "a clone must not block a writer"
        );
        gate.admission.opened(1, session.clone());
        let later_reader = Session {
            session_id: kajiba_core::new_id().unwrap(),
            ..reader.clone()
        };
        gate.admission.opened(1, later_reader.clone());
        if rebuild {
            gate.admission
                .rebuild(
                    &mut writer,
                    &session.generation,
                    vec![reader.clone(), session.clone(), later_reader.clone()],
                )
                .await
                .unwrap();
        }
        drop(writer);
        assert!(
            gate.pre_receive(Request::new(PreReceiveRequest {
                session_id: session.session_id.clone(),
                updates: vec![update("refs/heads/topic", ZERO, A, false)],
            }))
            .await
            .unwrap()
            .into_inner()
            .allowed,
            "a clone opened during a push must not prevent admission"
        );
        let mut writer = gate.writer.lock().await;
        gate.admission
            .session_exited(&mut writer, &reader.session_id)
            .await
            .unwrap();
        assert!(gate.admission.running(1), "the writer is still live");
        gate.admission
            .reconcile(
                &mut writer,
                &gate.bus,
                &gate.state_dir,
                &kajiba_gitstore::Gix,
            )
            .await
            .unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM pending_push")
                .fetch_one(&mut *writer)
                .await
                .unwrap(),
            1,
            "a reader exiting must not trigger premature recovery"
        );
        gate.admission
            .session_exited(&mut writer, &session.session_id)
            .await
            .unwrap();
        assert!(
            !gate.admission.running(1),
            "the remaining clone is not a writer"
        );

        // An empty bare repository: the admitted creation was never applied.
        let path = gate
            .state_dir
            .join(format!("repos/{}.git", session.repo_public_id));
        std::fs::create_dir_all(path.join("objects")).unwrap();
        std::fs::create_dir_all(path.join("refs/heads")).unwrap();
        std::fs::write(path.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        gate.admission
            .reconcile(
                &mut writer,
                &gate.bus,
                &gate.state_dir,
                &kajiba_gitstore::Gix,
            )
            .await
            .unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM pending_push")
                .fetch_one(&mut *writer)
                .await
                .unwrap(),
            0,
            "recovery must finish while the remaining clone is live"
        );
        gate.admission
            .admit(&mut writer, 1, None, "next-import", &[])
            .await
            .unwrap();
        gate.admission
            .session_exited(&mut writer, &later_reader.session_id)
            .await
            .unwrap();
        assert!(
            gate.admission.running(1),
            "the import reservation is still live"
        );
    }
}

#[tokio::test]
async fn competing_session_and_retained_unexpected_ref_block_admission() {
    use kajiba_gitstore::GitStore;
    let (_state, gate, session, request) = writable_fixture().await;
    let other = Session {
        session_id: kajiba_core::new_id().unwrap(),
        ..session.clone()
    };
    gate.admission.opened(1, other.clone());
    let response = gate
        .pre_receive(Request::new(request.clone()))
        .await
        .unwrap()
        .into_inner();
    assert!(!response.allowed);
    assert!(response.refusals[0].reason.contains("retry"));
    let mut writer = gate.writer.lock().await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM pending_push")
            .fetch_one(&mut *writer)
            .await
            .unwrap(),
        0
    );
    gate.admission
        .session_exited(&mut writer, &other.session_id)
        .await
        .unwrap();
    drop(writer);
    assert!(
        gate.pre_receive(Request::new(request.clone()))
            .await
            .unwrap()
            .into_inner()
            .allowed
    );
    let path = gate
        .state_dir
        .join(format!("repos/{}.git", session.repo_public_id));
    kajiba_gitstore::Gix.init_bare(&path, "main").unwrap();
    let mut writer = gate.writer.lock().await;
    gate.admission
        .session_exited(&mut writer, &session.session_id)
        .await
        .unwrap();
    // refs/heads/a is absent, neither its recorded old nor new value: retain the row.
    gate.admission
        .reconcile(
            &mut writer,
            &gate.bus,
            &gate.state_dir,
            &kajiba_gitstore::Gix,
        )
        .await
        .unwrap();
    let evidence: String = sqlx::query_scalar("SELECT updates FROM pending_push")
        .fetch_one(&mut *writer)
        .await
        .unwrap();
    gate.admission.opened(1, session);
    drop(writer);
    let response = gate
        .pre_receive(Request::new(request))
        .await
        .unwrap()
        .into_inner();
    assert!(!response.allowed);
    assert!(response.refusals[0].reason.contains("unexpected ref value"));
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT updates FROM pending_push")
            .fetch_one(&mut *gate.writer.lock().await)
            .await
            .unwrap(),
        evidence
    );
}

#[tokio::test]
async fn insert_and_commit_failures_never_allow_or_leave_a_pending_row() {
    let (_state, gate, session, request) = writable_fixture().await;
    for injection in [
        "CREATE TEMP TRIGGER fail_admission BEFORE INSERT ON pending_push BEGIN SELECT RAISE(ABORT, 'injected insert failure'); END",
        "CREATE TABLE admission_failure (user_id INTEGER REFERENCES user(id) DEFERRABLE INITIALLY DEFERRED); CREATE TEMP TRIGGER fail_admission AFTER INSERT ON pending_push BEGIN INSERT INTO admission_failure VALUES (999999); END",
    ] {
        sql(&gate, injection).await;
        let error = gate
            .pre_receive(Request::new(request.clone()))
            .await
            .unwrap_err();
        assert_eq!(error.code(), tonic::Code::Internal);
        // A new query also drains sqlx's queued rollback after a failed commit.
        let pending: i64 = sqlx::query_scalar("SELECT count(*) FROM pending_push")
            .fetch_one(&mut *gate.writer.lock().await)
            .await
            .unwrap();
        assert_eq!(pending, 0);
        assert!(gate.admission.session(&session.session_id).is_some());
        sql(&gate, "DROP TRIGGER fail_admission").await;
    }
    assert!(
        gate.pre_receive(Request::new(request))
            .await
            .unwrap()
            .into_inner()
            .allowed
    );
}
