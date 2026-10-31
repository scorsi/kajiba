use crate::transport::{
    auth::tests::{READ, call, reason, session, tokens},
    repos::tests::{DEMO, Fixture},
    router,
};
use tonic::Code;

#[tokio::test]
async fn only_a_session_manages_passkeys() {
    const ADMIN: &str = "kjb_runtime-admin";
    let fixture = Fixture::new(&format!("{DEMO}{}", tokens())).await;
    sqlx::query(
        "INSERT INTO api_token (public_id, user_id, name, token_hash, scopes, source, created_at)
         SELECT '01ARZ3NDEKTSV4RRFFQ69G5FAV', id, 'admin', ?, 'admin', 'runtime', 0
         FROM user WHERE name = 'alice'",
    )
    .bind(kajiba_core::sha256(ADMIN.as_bytes()).as_slice())
    .execute(&mut *fixture.writer.lock().await)
    .await
    .unwrap();
    session(&fixture, "alice", "alice-session", i64::MAX).await;
    let mut app = router(
        fixture.repos.0.clone(),
        false,
        Default::default(),
        "https://example.org",
    )
    .unwrap();
    for method in ["ListPasskeys", "DeletePasskey"] {
        let path = format!("/kajiba.v1.PasskeyService/{method}");
        // Whatever its scopes, a PAT never lists or removes passkeys (ADR 0143).
        for pat in [ADMIN, READ] {
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
    let cookie = [("cookie", "kajiba_session=alice-session")];
    assert!(
        call(&mut app, "/kajiba.v1.PasskeyService/ListPasskeys", &cookie)
            .await
            .is_none()
    );
    // A session that never stepped up removes nothing.
    let status = call(&mut app, "/kajiba.v1.PasskeyService/DeletePasskey", &cookie)
        .await
        .unwrap();
    assert_eq!(
        (status.code(), reason(&status)),
        (Code::PermissionDenied, "STEP_UP_REQUIRED".into())
    );
}
