use super::*;
use crate::transport::{
    repos::tests::{DEMO, Fixture},
    router,
};
use axum::http::{self, StatusCode};
use tonic::{Code, codegen::Service};
use tonic_types::StatusExt;

pub(in crate::transport) const READ: &str = "kjb_declared-read";
const CI: &str = "kjb_declared-ci";
const PAST: &str = "kjb_declared-past";
const RUNTIME: &str = "kjb_runtime";
const SESSION: &str = "session-secret";

fn hex(secret: &str) -> String {
    kajiba_core::sha256(secret.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// `alice` holds a `repo:read` PAT; `bob` a `ci:trigger` one and an expired one.
pub(in crate::transport) fn tokens() -> String {
    format!(
        "[users.alice.tokens.read]\nsha256='{}'\nscopes=['repo:read']\n\
         [users.bob.tokens.ci]\nsha256='{}'\nscopes=['ci:trigger']\n\
         [users.bob.tokens.past]\nsha256='{}'\nscopes=['repo:read']\nexpires='2020-01-01T00:00:00Z'\n",
        hex(READ),
        hex(CI),
        hex(PAST)
    )
}

fn now() -> i64 {
    super::now().unwrap()
}

/// A runtime PAT of `user` named after itself, `id` (0-9) making its public id.
async fn runtime(fixture: &Fixture, id: u8, user: &str, token: &str, expires_at: i64) {
    let mut writer = fixture.writer.lock().await;
    sqlx::query(
        "INSERT INTO api_token (public_id, user_id, name, token_hash, scopes, source, created_at, expires_at)
         SELECT ?, id, ?, ?, 'repo:read repo:write', 'runtime', 0, ? FROM user WHERE name = ?",
    )
    .bind(format!("01ARZ3NDEKTSV4RRFFQ69G5FA{id}"))
    .bind(token)
    .bind(kajiba_core::sha256(token.as_bytes()).as_slice())
    .bind(expires_at)
    .bind(user)
    .execute(&mut *writer)
    .await
    .unwrap();
}

async fn session(fixture: &Fixture, user: &str, value: &str, expires_at: i64) {
    sqlx::query(
        "INSERT INTO session (user_id, token_hash, created_at, expires_at, last_seen_at)
         SELECT id, ?, 0, ?, 0 FROM user WHERE name = ?",
    )
    .bind(kajiba_core::sha256(value.as_bytes()).as_slice())
    .bind(expires_at)
    .bind(user)
    .execute(&mut *fixture.writer.lock().await)
    .await
    .unwrap();
}

/// The gRPC status a call ends with in its headers (refusals are trailers-only), or
/// `None` when the handler answered with a body.
pub(in crate::transport) async fn call(
    app: &mut axum::Router,
    path: &str,
    headers: &[(&str, &str)],
) -> Option<Status> {
    let mut request = http::Request::post(path).header("content-type", "application/grpc");
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = app
        .call(request.body(Body::from(vec![0, 0, 0, 0, 0])).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    Status::from_header_map(response.headers()).filter(|status| status.code() != Code::Ok)
}

fn reason(status: &Status) -> String {
    status
        .get_error_details()
        .error_info()
        .unwrap()
        .reason
        .clone()
}

const LIST: &str = "/kajiba.v1.RepoService/ListRepos";

async fn last_used(fixture: &Fixture, token: &str) -> Option<i64> {
    sqlx::query_scalar("SELECT last_used_at FROM api_token WHERE token_hash = ?")
        .bind(kajiba_core::sha256(token.as_bytes()).as_slice())
        .fetch_one(&mut *fixture.writer.lock().await)
        .await
        .unwrap()
}

#[test]
fn expiry_is_reached_at_its_instant_and_revocation_or_disabled_user_invalidates() {
    let found = |expires_at, revoked, disabled| {
        Some(Found {
            id: 1,
            user_id: 1,
            scopes: None,
            expires_at,
            revoked,
            disabled,
            last_used_at: None,
        })
    };
    assert!(admit(found(None, false, false), 1000).is_ok());
    assert!(admit(found(Some(1001), false, false), 1000).is_ok());
    assert_eq!(
        admit(found(Some(1000), false, false), 1000).unwrap_err(),
        Refusal::Expired
    );
    for (revoked, disabled) in [(true, false), (false, true)] {
        // Invalid wins over expired: nothing tells a client a revoked secret was once real.
        assert_eq!(
            admit(found(Some(0), revoked, disabled), 1000).unwrap_err(),
            Refusal::Invalid
        );
    }
    assert_eq!(admit(None, 1000).unwrap_err(), Refusal::Invalid);
}

#[tokio::test]
async fn credential_matrix() {
    let fixture = Fixture::new(&format!("{DEMO}{}", tokens())).await;
    let registry = Arc::new(Registry::default());
    let mut app = router(fixture.repos.0.clone(), false, registry.clone()).unwrap();
    let later = now() + 3_600_000;
    runtime(&fixture, 1, "alice", RUNTIME, later).await;
    runtime(&fixture, 2, "alice", "kjb_revoked", later).await;
    sqlx::query("UPDATE api_token SET revoked_at = 1 WHERE name = 'kjb_revoked'")
        .execute(&mut *fixture.writer.lock().await)
        .await
        .unwrap();
    runtime(&fixture, 3, "bob", "kjb_stale", now() - 1).await;
    session(&fixture, "alice", SESSION, later).await;
    session(&fixture, "alice", "session-expired", now() - 1).await;

    let bearer = |token: &str| format!("Bearer {token}");
    for (headers, expected) in [
        (vec![], Some((Code::Unauthenticated, "CREDENTIAL_INVALID"))),
        (
            vec![("authorization", "Bearer ".to_owned())],
            Some((Code::Unauthenticated, "CREDENTIAL_INVALID")),
        ),
        (
            vec![("authorization", format!("Basic {READ}"))],
            Some((Code::Unauthenticated, "CREDENTIAL_INVALID")),
        ),
        (
            vec![("authorization", "Bearer not-a-pat".to_owned())],
            Some((Code::Unauthenticated, "CREDENTIAL_INVALID")),
        ),
        (
            vec![("authorization", bearer("kjb_unknown"))],
            Some((Code::Unauthenticated, "CREDENTIAL_INVALID")),
        ),
        (
            vec![("authorization", bearer("kjb_revoked"))],
            Some((Code::Unauthenticated, "CREDENTIAL_INVALID")),
        ),
        (
            vec![("authorization", bearer(PAST))],
            Some((Code::Unauthenticated, "CREDENTIAL_EXPIRED")),
        ),
        (
            vec![("authorization", bearer("kjb_stale"))],
            Some((Code::Unauthenticated, "CREDENTIAL_EXPIRED")),
        ),
        (
            vec![("authorization", bearer(CI))],
            Some((Code::PermissionDenied, "SCOPE_MISSING")),
        ),
        (
            vec![("cookie", "kajiba_session=session-expired".to_owned())],
            Some((Code::Unauthenticated, "CREDENTIAL_EXPIRED")),
        ),
        (
            vec![("cookie", "kajiba_session=unknown".to_owned())],
            Some((Code::Unauthenticated, "CREDENTIAL_INVALID")),
        ),
        // A malformed bearer never falls back to a valid cookie.
        (
            vec![
                ("authorization", "Bearer x".to_owned()),
                ("cookie", format!("kajiba_session={SESSION}")),
            ],
            Some((Code::Unauthenticated, "CREDENTIAL_INVALID")),
        ),
        (vec![("authorization", bearer(READ))], None),
        (vec![("authorization", bearer(RUNTIME))], None),
        (
            vec![("cookie", format!("theme=dark; kajiba_session={SESSION}"))],
            None,
        ),
    ] {
        let headers: Vec<_> = headers.iter().map(|(n, v)| (*n, v.as_str())).collect();
        let status = call(&mut app, LIST, &headers).await;
        assert_eq!(
            status.as_ref().map(|s| (s.code(), reason(s))),
            expected.map(|(code, reason)| (code, reason.to_owned())),
            "{headers:?}: {status:?}"
        );
    }
    let exposition = registry.exposition("test", [0; 4], [0; 4]);
    for (reason, count) in [
        ("CREDENTIAL_INVALID", 8),
        ("CREDENTIAL_EXPIRED", 3),
        ("SCOPE_MISSING", 1),
    ] {
        assert!(
            exposition.contains(&format!(
                "kajiba_auth_refusals_total{{reason=\"{reason}\"}} {count}\n"
            )),
            "{reason}"
        );
    }

    // HealthService.Check is public.
    assert!(
        call(&mut app, "/kajiba.v1.HealthService/Check", &[])
            .await
            .is_none()
    );

    // A disabled user's credentials stop working at once.
    sqlx::query("UPDATE user SET disabled_at = 1 WHERE name = 'alice'")
        .execute(&mut *fixture.writer.lock().await)
        .await
        .unwrap();
    for headers in [
        ("authorization", bearer(READ)),
        ("authorization", bearer(RUNTIME)),
        ("cookie", format!("kajiba_session={SESSION}")),
    ] {
        let status = call(&mut app, LIST, &[(headers.0, &headers.1)])
            .await
            .unwrap();
        assert_eq!(reason(&status), "CREDENTIAL_INVALID", "{headers:?}");
    }
}

#[tokio::test]
async fn usage_is_written_at_most_once_a_minute() {
    let fixture = Fixture::new(&format!("{DEMO}{}", tokens())).await;
    let mut app = router(fixture.repos.0.clone(), false, Arc::default()).unwrap();
    let auth = format!("Bearer {READ}");
    assert_eq!(last_used(&fixture, READ).await, None);
    let before = now();
    assert!(
        call(&mut app, LIST, &[("authorization", &auth)])
            .await
            .is_none()
    );
    let first = last_used(&fixture, READ).await.unwrap();
    assert!((before..=now()).contains(&first));
    assert!(
        call(&mut app, LIST, &[("authorization", &auth)])
            .await
            .is_none()
    );
    assert_eq!(last_used(&fixture, READ).await, Some(first));
    sqlx::query("UPDATE api_token SET last_used_at = last_used_at - 60000")
        .execute(&mut *fixture.writer.lock().await)
        .await
        .unwrap();
    assert!(
        call(&mut app, LIST, &[("authorization", &auth)])
            .await
            .is_none()
    );
    assert!(last_used(&fixture, READ).await.unwrap() >= first);

    session(&fixture, "alice", SESSION, now() + 60_000).await;
    let cookie = format!("kajiba_session={SESSION}");
    assert!(call(&mut app, LIST, &[("cookie", &cookie)]).await.is_none());
    let (seen, expires): (i64, i64) =
        sqlx::query_as("SELECT last_seen_at, expires_at FROM session")
            .fetch_one(&mut *fixture.writer.lock().await)
            .await
            .unwrap();
    assert!(seen >= before);
    // Activity never extends a session (absolute expiry, ADR 0014).
    assert!(expires <= now() + 60_000 && expires > seen);
}

#[tokio::test]
async fn anonymous_routes_stay_anonymous() {
    let fixture = Fixture::new(DEMO).await;
    fixture
        .fill("alice/demo", &[("README.md", "hello\n")], &[])
        .await;
    let mut app = router(fixture.repos.0.clone(), true, Arc::default()).unwrap();
    for path in [
        "/healthz",
        "/alice/demo.git/info/refs?service=git-upload-pack",
        "/raw/alice/demo/main/README.md",
    ] {
        let response = app
            .call(http::Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{path}");
    }
    // Reflection answers without a credential (an empty stream ends OK).
    assert!(
        call(
            &mut app,
            "/grpc.reflection.v1.ServerReflection/ServerReflectionInfo",
            &[]
        )
        .await
        .is_none()
    );
}

#[test]
fn credentials_never_reach_the_logs() {
    const CHILD: &str = "KAJIBA_TEST_AUTH_LOGS";
    if std::env::var_os(CHILD).is_some() {
        kajiba_core::log::init("info").unwrap();
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let fixture = Fixture::new(&format!("{DEMO}{}", tokens())).await;
            session(&fixture, "alice", SESSION, now() + 60_000).await;
            let mut app = router(fixture.repos.0.clone(), false, Arc::default()).unwrap();
            for header in [
                ("authorization", format!("Bearer {READ}")),
                ("authorization", format!("Bearer {PAST}")),
                ("authorization", format!("Bearer {CI}")),
                ("authorization", "Bearer kjb_unknown-secret".to_owned()),
                ("cookie", format!("kajiba_session={SESSION}")),
            ] {
                call(&mut app, LIST, &[(header.0, &header.1)]).await;
            }
        });
        return;
    }
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "transport::auth::tests::credentials_never_reach_the_logs",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .env_remove("RUST_LOG")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let stderr = String::from_utf8(output.stderr).unwrap();
    for secret in [READ, PAST, CI, "kjb_unknown-secret", SESSION] {
        assert!(!stderr.contains(secret), "{secret} in {stderr}");
    }
    let refusals: Vec<serde_json::Value> = stderr
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .filter(|line: &serde_json::Value| line["event"] == "auth-refused")
        .collect();
    assert_eq!(
        refusals
            .iter()
            .map(|line| line["reason"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["CREDENTIAL_EXPIRED", "SCOPE_MISSING", "CREDENTIAL_INVALID"]
    );
    for line in &refusals {
        assert_eq!(line["level"], "warn");
        assert_eq!(line["request_id"].as_str().unwrap().len(), 26, "{line}");
    }
}
