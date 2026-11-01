use super::*;
use crate::transport::{
    auth::tests::{READ, call, reason, tokens},
    repos::tests::{DEMO, Fixture},
    router,
};
use std::sync::Arc;
use tonic::Code;

const ROOT: &str = "kjb_declared-root";
const RUNNERS: &str = "/kajiba.v1.AdministrationService/ListRunners";

/// `alice` (row 1) is an administrator with an `admin` PAT and a `repo:read` one; `bob`
/// (row 2) is not.
async fn fixture() -> Fixture {
    let hex: String = kajiba_core::sha256(ROOT.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let fixture = Fixture::new(&format!(
        "{DEMO}{}[users.alice.tokens.root]\nsha256='{hex}'\nscopes=['admin']\n",
        tokens()
    ))
    .await;
    sql(
        &fixture,
        "UPDATE user SET is_admin = 1 WHERE name = 'alice'",
    )
    .await;
    fixture
}

fn from<T>(user_id: i64, message: T) -> Request<T> {
    let mut request = Request::new(message);
    request.extensions_mut().insert(Principal {
        user_id,
        scopes: vec![kajiba_proto::v1::Scope::Admin],
        session: None,
    });
    request
}

async fn sql(fixture: &Fixture, query: &str) {
    sqlx::query(query)
        .execute(&mut *fixture.writer.lock().await)
        .await
        .unwrap();
}

async fn runners(service: &Administration) -> Vec<Runner> {
    service
        .list_runners(from(1, ListRunnersRequest {}))
        .await
        .unwrap()
        .into_inner()
        .runners
}

async fn mirrors(service: &Administration) -> Vec<MirrorStatus> {
    service
        .list_mirrors(from(1, ListMirrorsRequest {}))
        .await
        .unwrap()
        .into_inner()
        .mirrors
}

#[tokio::test]
async fn only_administrators_with_the_admin_scope_are_answered() {
    let fixture = fixture().await;
    let service = Administration(fixture.repos.0.clone());
    // Bob holds the scope but is no administrator.
    for status in [
        service
            .list_runners(from(2, ListRunnersRequest {}))
            .await
            .unwrap_err(),
        service
            .list_mirrors(from(2, ListMirrorsRequest {}))
            .await
            .unwrap_err(),
        service
            .get_server_info(from(2, GetServerInfoRequest {}))
            .await
            .unwrap_err(),
    ] {
        assert_eq!(status.code(), Code::PermissionDenied);
        assert_eq!(reason(&status), "ADMIN_REQUIRED");
    }
    // Through the interceptor: a PAT without `admin` is refused, the one with it is answered.
    let mut app = router(
        fixture.repos.0.clone(),
        false,
        Arc::default(),
        "https://example.org",
    )
    .unwrap();
    let read = format!("Bearer {READ}");
    let status = call(&mut app, RUNNERS, &[("authorization", &read)])
        .await
        .unwrap();
    assert_eq!(reason(&status), "SCOPE_MISSING");
    let root = format!("Bearer {ROOT}");
    assert!(
        call(&mut app, RUNNERS, &[("authorization", &root)])
            .await
            .is_none()
    );
}

#[tokio::test]
async fn runners_show_state_and_all_attempts_running_now() {
    let fixture = fixture().await;
    let config = kajiba_core::config::ServerConfig::parse(
        "[http]\npublic_url = 'https://example.org'\n\
         [runner_listener]\nlisten = '127.0.0.1:8443'\ntrust_domain = 'example.org'\nsvid_file = '/svid.pem'\nkey_file = '/key.pem'\nbundle_file = '/bundle.pem'\n\
         [runners.build-1]\nspiffe_id = 'spiffe://example.org/build-1/kajiba-runner'\n\
         [runners.build-1.capabilities]\narch = 'aarch64'\nos = 'linux'\nbackends = ['nix']\ntags = ['kvm']\nmax_jobs = 2\n\
         [runners.build-2]\nspiffe_id = 'spiffe://example.org/build-2/kajiba-runner'\n\
         [runners.build-2.capabilities]\narch = 'x86_64'\nos = 'linux'\n",
    )
    .unwrap();
    crate::db::runners::reconcile_runners(&mut *fixture.writer.lock().await, &config.runners, 1)
        .await
        .unwrap();
    sql(
        &fixture,
        "UPDATE runner SET connected = 1, version = '1.2.3', hostname = 'host-1', last_contact_at = 5000 WHERE name = 'build-1'",
    )
    .await;
    sql(
        &fixture,
        "INSERT INTO run (public_id, repo_id, sha, ref, trigger, state, created_at, started_at)
         VALUES ('01ARZ3NDEKTSV4RRFFQ69G5FA1', 1, 'abc', 'refs/heads/main', 'push', 'running', 1000, 2000)",
    )
    .await;
    sql(
        &fixture,
        "INSERT INTO job (public_id, run_id, name, requirements, replayable, max_attempts, state, created_at)
         VALUES ('01ARZ3NDEKTSV4RRFFQ69G5FA2', 1, 'check', '{}', 0, 1, 'running', 1000)",
    )
    .await;
    sql(
        &fixture,
        "INSERT INTO job_attempt (public_id, job_id, number, runner_id, state, lease_expires_at, started_at)
         SELECT '01ARZ3NDEKTSV4RRFFQ69G5FA3', 1, 1, id, 'running', 9000, 2000 FROM runner WHERE name = 'build-1'",
    )
    .await;
    sql(
        &fixture,
        "INSERT INTO run (public_id, repo_id, sha, trigger, triggered_by, state, created_at, started_at)
         VALUES ('01ARZ3NDEKTSV4RRFFQ69G5FB1', 1, 'def', 'worktree', 1, 'running', 1000, 3000)",
    )
    .await;
    sql(
        &fixture,
        "INSERT INTO job (public_id, run_id, name, requirements, replayable, max_attempts, state, created_at)
         VALUES ('01ARZ3NDEKTSV4RRFFQ69G5FB2', 2, 'check', '{}', 0, 1, 'running', 1000)",
    )
    .await;
    sql(
        &fixture,
        "INSERT INTO job_attempt (public_id, job_id, number, runner_id, state, lease_expires_at, started_at)
         SELECT '01ARZ3NDEKTSV4RRFFQ69G5FB3', 2, 1, id, 'running', 9000, 3000 FROM runner WHERE name = 'build-1'",
    )
    .await;
    let service = Administration(fixture.repos.0.clone());
    let listed = runners(&service).await;
    assert_eq!(
        listed.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
        ["build-1", "build-2"]
    );
    let busy = &listed[0];
    assert_eq!(
        (busy.arch.as_str(), busy.os.as_str(), busy.max_jobs),
        ("aarch64", "linux", 2)
    );
    assert_eq!(busy.backends, ["nix"]);
    assert_eq!(busy.tags, ["kvm"]);
    assert!(busy.connected && !busy.disabled);
    assert_eq!(
        (busy.version.as_str(), busy.hostname.as_str()),
        ("1.2.3", "host-1")
    );
    assert_eq!(busy.last_contact_at.as_ref().unwrap().seconds, 5);
    assert_eq!(
        busy.current,
        [
            RunnerAttempt {
                run_id: "01ARZ3NDEKTSV4RRFFQ69G5FA1".into(),
                repo: "alice/demo".into(),
                r#ref: "refs/heads/main".into(),
                started_at: Some(timestamp(2000)),
            },
            RunnerAttempt {
                run_id: "01ARZ3NDEKTSV4RRFFQ69G5FB1".into(),
                repo: "alice/demo".into(),
                r#ref: String::new(),
                started_at: Some(timestamp(3000)),
            }
        ]
    );
    let idle = &listed[1];
    assert!(!idle.connected && idle.current.is_empty() && idle.last_contact_at.is_none());

    // Completing one attempt frees one slot without hiding the other.
    sql(
        &fixture,
        "UPDATE job_attempt SET state = 'succeeded' WHERE job_id = 1",
    )
    .await;
    let partial = runners(&service).await;
    assert_eq!(partial[0].current.as_slice(), &busy.current[1..]);

    // Attempts show only while they run.
    sql(&fixture, "UPDATE job_attempt SET state = 'succeeded'").await;
    assert!(runners(&service).await.iter().all(|r| r.current.is_empty()));
}

#[tokio::test]
async fn mirrors_are_never_pushed_until_a_push_is_recorded_and_carry_no_secret() {
    let fixture = fixture().await;
    let service = Administration(fixture.repos.0.clone());
    assert_eq!(
        mirrors(&service).await,
        [MirrorStatus {
            repo: "alice/demo".into(),
            remote_url: "https://example.org/demo.git".into(),
            credential: "github".into(),
            enabled: true,
            state: MirrorStatusState::Idle.into(),
            ..Default::default()
        }]
    );
    sql(
        &fixture,
        "UPDATE mirror SET attempts = 2, last_error = 'refused', last_error_at = 7000, next_attempt_at = 9000",
    )
    .await;
    let mirror = &mirrors(&service).await[0];
    assert_eq!(
        (mirror.attempts, mirror.last_error.as_str()),
        (2, "refused")
    );
    assert!(mirror.last_success_at.is_none());
    assert_eq!(mirror.next_attempt_at.as_ref().unwrap().seconds, 9);
}

#[tokio::test]
async fn server_information_counts_users_tokens_and_storage() {
    let fixture = fixture().await;
    fixture.fill("alice/demo", &[("a.txt", "a")], &[]).await;
    fixture.repos.0.set_config_generation(3);
    let service = Administration(fixture.repos.0.clone());
    let info = service
        .get_server_info(from(1, GetServerInfoRequest {}))
        .await
        .unwrap()
        .into_inner()
        .server
        .unwrap();
    assert_eq!(info.version, env!("CARGO_PKG_VERSION"));
    assert!(info.started_at.unwrap().seconds > 0);
    assert_eq!(
        info.users,
        [
            ServerUser {
                name: "alice".into(),
                admin: true,
                disabled: false
            },
            ServerUser {
                name: "bob".into(),
                admin: false,
                disabled: false
            }
        ]
    );
    // read, ci, past (expired, not revoked) and root.
    assert_eq!(info.declared_tokens, 4);
    assert!(info.database_bytes > 0 && info.storage_bytes > 0);
    assert_eq!(info.config_generation, 3);
}
