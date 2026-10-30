use super::*;
use crate::transport::{
    auth::tests::{READ, call, reason, tokens},
    repos::tests::{DEMO, Fixture},
    router,
};
use std::sync::Arc;
use tonic::Code;

/// A request from `user` (row id) as the auth layer passes it.
fn from<T>(user_id: i64, session: bool, message: T) -> Request<T> {
    let mut request = Request::new(message);
    request.extensions_mut().insert(Principal {
        user_id,
        scopes: vec![Scope::Admin],
        session: session.then(|| kajiba_core::sha256(user_id.to_string().as_bytes())),
    });
    request
}

async fn fixture() -> Fixture {
    let fixture = Fixture::new(&format!("{DEMO}{}", tokens())).await;
    for user in [1_i64, 2] {
        sqlx::query(
            "INSERT INTO session (user_id, token_hash, created_at, expires_at, last_seen_at)
             VALUES (?, ?, 0, ?, 0)",
        )
        .bind(user)
        .bind(kajiba_core::sha256(user.to_string().as_bytes()).as_slice())
        .bind(i64::MAX)
        .execute(&mut *fixture.writer.lock().await)
        .await
        .unwrap();
    }
    fixture
}

fn create(name: &str, scopes: &[Scope]) -> CreateTokenRequest {
    CreateTokenRequest {
        name: name.into(),
        scopes: scopes.iter().map(|scope| i32::from(*scope)).collect(),
        expires_at: None,
    }
}

async fn list(service: &Tokens, user: i64) -> Vec<Token> {
    service
        .list_tokens(from(user, true, ListTokensRequest::default()))
        .await
        .unwrap()
        .into_inner()
        .tokens
}

const LIST: &str = "/kajiba.v1.RepoService/ListRepos";

#[test]
fn expiry_timestamp_respects_protobuf_bounds() {
    for (seconds, nanos, expected) in [
        (-62_135_596_800, 0, Some(-62_135_596_800_000)),
        (253_402_300_799, 999_999_999, Some(253_402_300_799_999)),
        (-62_135_596_801, 999_999_999, None),
        (253_402_300_800, 0, None),
        (0, -1, None),
        (0, 1_000_000_000, None),
    ] {
        assert_eq!(millis(&Timestamp { seconds, nanos }), expected);
    }
}

#[tokio::test]
async fn created_token_is_shown_once_stored_hashed_and_authenticates() {
    let fixture = fixture().await;
    let service = Tokens(fixture.repos.0.clone());
    let mut app = router(
        fixture.repos.0.clone(),
        false,
        Arc::default(),
        "https://example.org",
    )
    .unwrap();
    let created = service
        .create_token(from(
            1,
            true,
            create("Laptop", &[Scope::RepoRead, Scope::RepoRead]),
        ))
        .await
        .unwrap()
        .into_inner();
    let secret = created.secret;
    let encoded = secret.strip_prefix("kjb_").unwrap();
    assert_eq!(URL_SAFE_NO_PAD.decode(encoded).unwrap().len(), 32);
    let token = created.token.unwrap();
    assert_eq!(token.name, "laptop");
    assert_eq!(token.scopes, [i32::from(Scope::RepoRead)]);
    assert_eq!(token.source, i32::from(TokenSource::Runtime));
    assert!(token.expires_at.is_none() && token.revoked_at.is_none());
    let stored: (Vec<u8>, String) =
        sqlx::query_as("SELECT token_hash, scopes FROM api_token WHERE public_id = ?")
            .bind(&token.public_id)
            .fetch_one(&mut *fixture.writer.lock().await)
            .await
            .unwrap();
    assert_eq!(
        stored,
        (
            kajiba_core::sha256(secret.as_bytes()).to_vec(),
            "repo:read".into()
        )
    );
    let bearer = format!("Bearer {secret}");
    assert!(
        call(&mut app, LIST, &[("authorization", &bearer)])
            .await
            .is_none()
    );

    // The listing carries metadata only: declared and runtime tokens, by name.
    let listed = list(&service, 1).await;
    assert_eq!(
        listed
            .iter()
            .map(|t| (t.name.as_str(), t.source))
            .collect::<Vec<_>>(),
        [
            ("laptop", i32::from(TokenSource::Runtime)),
            ("read", i32::from(TokenSource::Config))
        ]
    );
    // The call above recorded its use.
    assert!(listed[0].last_used_at.is_some());
    assert_eq!(
        Token {
            last_used_at: None,
            ..listed[0].clone()
        },
        token
    );

    let refused = |name: &str, scopes: &[i32], expires_at: Option<Timestamp>| {
        let request = CreateTokenRequest {
            name: name.into(),
            scopes: scopes.to_vec(),
            expires_at,
        };
        let service = &service;
        async move {
            service
                .create_token(from(1, true, request))
                .await
                .unwrap_err()
        }
    };
    let read = [i32::from(Scope::RepoRead)];
    for name in ["laptop", "LAPTOP", "read"] {
        let status = refused(name, &read, None).await;
        assert_eq!(
            (status.code(), reason(&status)),
            (Code::AlreadyExists, "NAME_TAKEN".into())
        );
    }
    for (name, scopes, expires_at) in [
        ("bad name", &read[..], None),
        ("x", &[][..], None),
        ("x", &[0][..], None),
        ("x", &[99][..], None),
        (
            "x",
            &read[..],
            Some(Timestamp {
                seconds: 1,
                nanos: 0,
            }),
        ),
        (
            "x",
            &read[..],
            Some(Timestamp {
                seconds: 253_402_300_800,
                nanos: 0,
            }),
        ),
        (
            "x",
            &read[..],
            Some(Timestamp {
                seconds: i64::MAX,
                nanos: 0,
            }),
        ),
        (
            "x",
            &read[..],
            Some(Timestamp {
                seconds: 4_000_000_000,
                nanos: -1,
            }),
        ),
    ] {
        let status = refused(name, scopes, expires_at).await;
        assert_eq!(
            (status.code(), reason(&status)),
            (Code::InvalidArgument, "INVALID_FIELD".into()),
            "{name} {scopes:?}"
        );
    }
    let expiring = service
        .create_token(from(
            1,
            true,
            CreateTokenRequest {
                expires_at: Some(Timestamp {
                    seconds: 4_000_000_000,
                    nanos: 5_000_000,
                }),
                ..create("expiring", &[Scope::Admin, Scope::CiTrigger])
            },
        ))
        .await
        .unwrap()
        .into_inner()
        .token
        .unwrap();
    assert_eq!(
        expiring.expires_at,
        Some(Timestamp {
            seconds: 4_000_000_000,
            nanos: 5_000_000
        })
    );
    assert_eq!(
        expiring.scopes,
        [i32::from(Scope::CiTrigger), i32::from(Scope::Admin)]
    );
}

#[tokio::test]
async fn only_a_session_manages_tokens() {
    let fixture = fixture().await;
    let service = Tokens(fixture.repos.0.clone());
    let status = service
        .list_tokens(from(1, false, ListTokensRequest::default()))
        .await
        .unwrap_err();
    assert_eq!(
        (status.code(), reason(&status)),
        (Code::PermissionDenied, "SESSION_REQUIRED".into())
    );

    // Through the auth layer: PATs are refused regardless of scope, a session passes.
    let admin_pat = service
        .create_token(from(1, true, create("admin", &[Scope::Admin])))
        .await
        .unwrap()
        .into_inner()
        .secret;
    let user = 1;
    let cookie = {
        let mut writer = fixture.writer.lock().await;
        crate::db::sessions::create(
            &mut writer,
            user,
            std::time::Duration::from_secs(60),
            now().unwrap(),
        )
        .await
        .unwrap()
    };
    let mut app = router(
        fixture.repos.0.clone(),
        false,
        Arc::default(),
        "https://example.org",
    )
    .unwrap();
    for path in ["CreateToken", "ListTokens", "RevokeToken"] {
        let path = format!("/kajiba.v1.TokenService/{path}");
        for pat in [admin_pat.as_str(), READ] {
            let status = call(
                &mut app,
                &path,
                &[("authorization", &format!("Bearer {pat}"))],
            )
            .await
            .unwrap();
            assert_eq!(
                (status.code(), reason(&status)),
                (Code::PermissionDenied, "SESSION_REQUIRED".into()),
                "{path}"
            );
        }
    }
    let cookie = format!("kajiba_session={cookie}");
    assert!(
        call(
            &mut app,
            "/kajiba.v1.TokenService/ListTokens",
            &[("cookie", &cookie)]
        )
        .await
        .is_none()
    );
}

#[tokio::test]
async fn revocation_refuses_the_token_and_spares_declared_ones() {
    let fixture = fixture().await;
    let service = Tokens(fixture.repos.0.clone());
    let mut app = router(
        fixture.repos.0.clone(),
        false,
        Arc::default(),
        "https://example.org",
    )
    .unwrap();
    let created = service
        .create_token(from(1, true, create("laptop", &[Scope::RepoRead])))
        .await
        .unwrap()
        .into_inner();
    let bearer = format!("Bearer {}", created.secret);
    let id = created.token.unwrap().public_id;
    let revoke = |user: i64, public_id: String| {
        let service = &service;
        async move {
            service
                .revoke_token(from(user, true, RevokeTokenRequest { public_id }))
                .await
        }
    };

    // Another user's token does not exist for the caller.
    let status = revoke(2, id.clone()).await.unwrap_err();
    assert_eq!(
        (status.code(), reason(&status)),
        (Code::NotFound, "TOKEN_NOT_FOUND".into())
    );
    assert!(
        call(&mut app, LIST, &[("authorization", &bearer)])
            .await
            .is_none()
    );

    let revoked = revoke(1, id.clone())
        .await
        .unwrap()
        .into_inner()
        .token
        .unwrap();
    assert!(revoked.revoked_at.is_some());
    let status = call(&mut app, LIST, &[("authorization", &bearer)])
        .await
        .unwrap();
    assert_eq!(reason(&status), "CREDENTIAL_INVALID");
    // Again: nothing changes, the first revocation time stays.
    let again = revoke(1, id).await.unwrap().into_inner().token.unwrap();
    assert_eq!(again, revoked);

    let declared = list(&service, 1)
        .await
        .into_iter()
        .find(|token| token.name == "read")
        .unwrap();
    let status = revoke(1, declared.public_id.clone()).await.unwrap_err();
    assert_eq!(
        (status.code(), reason(&status)),
        (Code::FailedPrecondition, "DECLARED_IN_CONFIG".into())
    );
    assert_eq!(
        list(&service, 1)
            .await
            .into_iter()
            .find(|t| t.name == "read")
            .unwrap(),
        declared
    );
    let read = format!("Bearer {READ}");
    assert!(
        call(&mut app, LIST, &[("authorization", &read)])
            .await
            .is_none()
    );

    // `admin enroll` revokes runtime tokens and keeps declared ones (ADR 0014).
    let fresh = service
        .create_token(from(1, true, create("fresh", &[Scope::RepoRead])))
        .await
        .unwrap()
        .into_inner()
        .secret;
    crate::db::enrollment::issue(
        &mut *fixture.writer.lock().await,
        "alice",
        std::time::Duration::from_secs(60),
        now().unwrap(),
    )
    .await
    .unwrap();
    let status = call(
        &mut app,
        LIST,
        &[("authorization", &format!("Bearer {fresh}"))],
    )
    .await
    .unwrap();
    assert_eq!(reason(&status), "CREDENTIAL_INVALID");
    assert!(
        call(&mut app, LIST, &[("authorization", &read)])
            .await
            .is_none()
    );
}

#[tokio::test]
async fn listing_pages_by_name_over_the_callers_tokens_only() {
    let fixture = fixture().await;
    let service = Tokens(fixture.repos.0.clone());
    for name in ["c", "a", "b"] {
        service
            .create_token(from(1, true, create(name, &[Scope::RepoRead])))
            .await
            .unwrap();
    }
    let mut names = Vec::new();
    let mut page_token = String::new();
    loop {
        let page = service
            .list_tokens(from(
                1,
                true,
                ListTokensRequest {
                    page_size: 2,
                    page_token,
                },
            ))
            .await
            .unwrap()
            .into_inner();
        assert!(page.tokens.len() <= 2);
        names.extend(page.tokens.into_iter().map(|token| token.name));
        page_token = page.next_page_token;
        if page_token.is_empty() {
            break;
        }
    }
    assert_eq!(names, ["a", "b", "c", "read"]);
    // bob sees his two declared tokens, never alice's.
    let bob: Vec<_> = list(&service, 2)
        .await
        .into_iter()
        .map(|t| t.name)
        .collect();
    assert_eq!(bob, ["ci", "past"]);
    let status = service
        .list_tokens(from(
            1,
            true,
            ListTokensRequest {
                page_size: 0,
                page_token: cursor::encode("v1.ListRepos", "a", 1),
            },
        ))
        .await
        .unwrap_err();
    assert_eq!(status.code(), Code::InvalidArgument);
}

#[tokio::test]
async fn creation_rechecks_the_session_after_authentication() {
    for (change, expected) in [
        ("recovery", "CREDENTIAL_INVALID"),
        ("expiry", "CREDENTIAL_EXPIRED"),
        ("disabled", "CREDENTIAL_INVALID"),
    ] {
        let fixture = fixture().await;
        let service = Tokens(fixture.repos.0.clone());
        // Authentication has already admitted this request; its handler waits for the writer.
        let request = from(1, true, create("late", &[Scope::RepoRead]));
        let mut writer = fixture.writer.lock().await;
        let creation = service.create_token(request);
        tokio::pin!(creation);
        assert!(
            std::future::poll_fn(|cx| {
                std::task::Poll::Ready(
                    std::future::Future::poll(creation.as_mut(), cx).is_pending(),
                )
            })
            .await
        );
        match change {
            "recovery" => {
                crate::db::enrollment::issue(
                    &mut writer,
                    "alice",
                    std::time::Duration::from_secs(60),
                    now().unwrap(),
                )
                .await
                .unwrap();
                // Reusing the deleted session's integer ID must not revive the request.
                sqlx::query(
                    "INSERT INTO session (id, user_id, token_hash, created_at, expires_at, last_seen_at)
                     VALUES (1, 1, ?, 0, ?, 0)",
                )
                .bind(kajiba_core::sha256(b"replacement").as_slice())
                .bind(i64::MAX)
                .execute(&mut *writer)
                .await
                .unwrap();
            }
            "expiry" => {
                sqlx::query("UPDATE session SET expires_at = ? WHERE user_id = 1")
                    .bind(now().unwrap())
                    .execute(&mut *writer)
                    .await
                    .unwrap();
            }
            "disabled" => {
                sqlx::query("UPDATE user SET disabled_at = 1 WHERE id = 1")
                    .execute(&mut *writer)
                    .await
                    .unwrap();
            }
            _ => unreachable!(),
        }
        drop(writer);
        let status = creation.await.unwrap_err();
        assert_eq!(
            (status.code(), reason(&status)),
            (Code::Unauthenticated, expected.into())
        );
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM api_token WHERE source = 'runtime'")
                .fetch_one(&mut *fixture.writer.lock().await)
                .await
                .unwrap();
        assert_eq!(count, 0, "{change}");
    }
}
