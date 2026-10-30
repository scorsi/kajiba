use super::*;
use webauthn_rs::prelude::Uuid;

/// The RP ID a browser is told, read from a registration challenge.
fn rp_id(webauthn: &Webauthn) -> String {
    let (challenge, _) = webauthn
        .start_passkey_registration(Uuid::nil(), "alice", "alice", None)
        .unwrap();
    challenge.public_key.rp.id
}

#[test]
fn relying_party_follows_the_public_url() {
    for (url, id, origin) in [
        (
            "https://forge.example.org",
            "forge.example.org",
            "https://forge.example.org/",
        ),
        (
            "https://Forge.Example.org:8443/",
            "forge.example.org",
            "https://forge.example.org:8443/",
        ),
        // Browsers treat http://localhost as a secure context (ADR 0006).
        (
            "http://localhost:5173",
            "localhost",
            "http://localhost:5173/",
        ),
    ] {
        let webauthn = relying_party(url).unwrap();
        assert_eq!(rp_id(&webauthn), id, "{url}");
        let origins: Vec<_> = webauthn
            .get_allowed_origins()
            .iter()
            .map(Url::as_str)
            .collect();
        assert_eq!(origins, [origin], "{url}");
    }
    for url in ["http://127.0.0.1:8080", "http://[::1]:8080"] {
        let err = relying_party(url).err().unwrap();
        assert!(err.starts_with("http.public_url: "), "{url}: {err}");
    }
}

#[test]
fn a_ceremony_finishes_once_within_its_ttl() {
    let start = Instant::now();
    let mut ceremonies = Ceremonies::default();
    let id = ceremonies.begin("state", start).unwrap();
    assert_eq!(id.len(), 64);
    assert_eq!(ceremonies.finish("unknown", start), None);
    let just_in_time = start + CEREMONY_TTL - Duration::from_millis(1);
    assert_eq!(ceremonies.finish(&id, just_in_time), Some("state"));
    // Replay: the state is gone after its first finish.
    assert_eq!(ceremonies.finish(&id, just_in_time), None);

    let id = ceremonies.begin("late", start).unwrap();
    assert_eq!(ceremonies.finish(&id, start + CEREMONY_TTL), None);
}

#[test]
fn outstanding_ceremonies_are_bounded() {
    let start = Instant::now();
    let mut ceremonies = Ceremonies::default();
    let first = ceremonies.begin(0, start).unwrap();
    for n in 1..MAX_CEREMONIES {
        ceremonies.begin(n, start + Duration::from_secs(1)).unwrap();
    }
    let full = ceremonies.begin(MAX_CEREMONIES, start + Duration::from_secs(1));
    assert_eq!(full.unwrap_err().kind(), io::ErrorKind::QuotaExceeded);
    // The refusal evicted nothing in progress.
    assert_eq!(ceremonies.finish(&first, start), Some(0));
    ceremonies.begin(MAX_CEREMONIES, start).unwrap();
    // An expired ceremony no longer counts.
    ceremonies.begin(99, start + CEREMONY_TTL).unwrap();
}

#[test]
fn user_handle_is_the_public_id_bits() {
    assert_eq!(
        user_handle("00000000000000000000000001"),
        Uuid::from_u128(1)
    );
    assert_eq!(
        user_handle("7ZZZZZZZZZZZZZZZZZZZZZZZZZ"),
        Uuid::from_u128(u128::MAX)
    );
    assert_ne!(
        user_handle("01ARZ3NDEKTSV4RRFFQ69G5FAV"),
        user_handle("01ARZ3NDEKTSV4RRFFQ69G5FAW")
    );
}

mod ceremonies {
    use super::super::*;
    use crate::transport::{repos::tests::Fixture, router};
    use axum::{body::Body, http::Request};
    use tonic::codegen::Service;
    use webauthn_authenticator_rs::{WebauthnAuthenticator, softpasskey::SoftPasskey};
    use webauthn_rs::prelude::{CreationChallengeResponse, Passkey, PublicKeyCredential};

    pub(in crate::transport) const ORIGIN: &str = "https://example.org";
    const TTL: Duration = Duration::from_secs(600);

    pub(in crate::transport) async fn fixture() -> (Fixture, Router) {
        let fixture = Fixture::new("").await;
        fixture
            .repos
            .0
            .set_passkeys(Passkeys::new(ORIGIN, Duration::from_secs(3600)).unwrap());
        let app = router(
            fixture.repos.0.clone(),
            false,
            Default::default(),
            "https://example.org",
        )
        .unwrap();
        (fixture, app)
    }

    /// POSTs `body` as JSON: the status, the `Set-Cookie` header and the JSON answer.
    pub(in crate::transport) async fn post(
        app: &mut Router,
        path: &str,
        body: serde_json::Value,
    ) -> (StatusCode, Option<String>, serde_json::Value) {
        let request = Request::post(path)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let response = app.call(request).await.unwrap();
        let status = response.status();
        let cookie = response
            .headers()
            .get(SET_COOKIE)
            .map(|value| value.to_str().unwrap().to_owned());
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json = serde_json::from_slice(&bytes).unwrap_or_default();
        (status, cookie, json)
    }

    pub(in crate::transport) async fn issue(fixture: &Fixture, user: &str, now: i64) -> String {
        crate::db::enrollment::issue(&mut *fixture.writer.lock().await, user, TTL, now)
            .await
            .unwrap()
    }

    pub(in crate::transport) fn epoch_ms() -> i64 {
        now().unwrap()
    }

    /// Starts an enrollment: the ceremony id and the options for the authenticator.
    pub(in crate::transport) async fn start(
        app: &mut Router,
        token: &str,
    ) -> Result<(String, CreationChallengeResponse), (StatusCode, String)> {
        let (status, _, json) = post(
            app,
            "/auth/webauthn/enroll/start",
            serde_json::json!({ "token": token }),
        )
        .await;
        if status != StatusCode::OK {
            return Err((status, json["reason"].as_str().unwrap_or_default().into()));
        }
        Ok((
            json["ceremony"].as_str().unwrap().into(),
            serde_json::from_value(json["options"].clone()).unwrap(),
        ))
    }

    pub(in crate::transport) fn authenticator() -> WebauthnAuthenticator<SoftPasskey> {
        // `true`: claim user verification, which passkey ceremonies require.
        WebauthnAuthenticator::new(SoftPasskey::new(true))
    }

    pub(in crate::transport) async fn finish(
        app: &mut Router,
        ceremony: &str,
        authenticator: &mut WebauthnAuthenticator<SoftPasskey>,
        origin: &str,
        options: CreationChallengeResponse,
    ) -> (StatusCode, Option<String>, serde_json::Value) {
        let credential = authenticator
            .do_registration(Url::parse(origin).unwrap(), options)
            .unwrap();
        post(
            app,
            "/auth/webauthn/enroll/finish",
            serde_json::json!({ "ceremony": ceremony, "credential": credential, "name": "laptop" }),
        )
        .await
    }

    async fn count(fixture: &Fixture, table: &str) -> i64 {
        sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
            .fetch_one(fixture.repos.readers().unwrap())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn enrollment_stores_the_passkey_and_opens_a_session() {
        let (fixture, mut app) = fixture().await;
        let token = issue(&fixture, "alice", epoch_ms()).await;
        let (ceremony, options) = start(&mut app, &token).await.unwrap();
        assert_eq!(
            serde_json::json!(options)["publicKey"]["authenticatorSelection"]["residentKey"],
            "required"
        );
        let user = &options.public_key.user;
        assert_eq!(user.name, "alice");
        let (public_id,): (String,) =
            sqlx::query_as("SELECT public_id FROM user WHERE name = 'alice'")
                .fetch_one(fixture.repos.readers().unwrap())
                .await
                .unwrap();
        assert_eq!(user.id.as_ref(), user_handle(&public_id).as_bytes());

        let mut authenticator = authenticator();
        let (status, cookie, _) =
            finish(&mut app, &ceremony, &mut authenticator, ORIGIN, options).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let cookie = cookie.unwrap();
        assert!(
            cookie.ends_with("; Path=/; Max-Age=3600; HttpOnly; Secure; SameSite=Strict"),
            "{cookie}"
        );
        let value = cookie
            .strip_prefix("kajiba_session=")
            .unwrap()
            .split(';')
            .next()
            .unwrap();
        let session = crate::db::sessions::find(
            fixture.repos.readers().unwrap(),
            &kajiba_core::sha256(value.as_bytes()),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            session.expires_at.unwrap() - session.last_used_at.unwrap(),
            3_600_000
        );
        let (name, passkey): (String, String) =
            sqlx::query_as("SELECT name, passkey FROM webauthn_credential")
                .fetch_one(fixture.repos.readers().unwrap())
                .await
                .unwrap();
        assert_eq!(name, "laptop");
        serde_json::from_str::<webauthn_rs::prelude::Passkey>(&passkey).unwrap();
        // The link is consumed: neither a new start nor a replayed finish works.
        assert_eq!(
            start(&mut app, &token).await.unwrap_err(),
            (StatusCode::FORBIDDEN, "ENROLLMENT_INVALID".into())
        );
        let (_, options) = {
            let token = issue(&fixture, "bob", epoch_ms()).await;
            start(&mut app, &token).await.unwrap()
        };
        let (status, _, json) =
            finish(&mut app, &ceremony, &mut authenticator, ORIGIN, options).await;
        assert_eq!(
            (status, json["reason"].as_str()),
            (StatusCode::FORBIDDEN, Some("CEREMONY_INVALID"))
        );
    }

    #[tokio::test]
    async fn invalid_links_are_refused() {
        let (fixture, mut app) = fixture().await;
        let refused = Err((StatusCode::FORBIDDEN, "ENROLLMENT_INVALID".into()));
        // Expired: issued long enough ago.
        let expired = issue(&fixture, "alice", epoch_ms() - TTL.as_millis() as i64).await;
        assert_eq!(start(&mut app, &expired).await.map(drop), refused);
        // Superseded by a newer link.
        let old = issue(&fixture, "alice", epoch_ms()).await;
        let new = issue(&fixture, "alice", epoch_ms()).await;
        assert_eq!(start(&mut app, &old).await.map(drop), refused);
        assert_eq!(start(&mut app, "unknown").await.map(drop), refused);
        // Disabled before the start, then between start and finish.
        let (ceremony, options) = start(&mut app, &new).await.unwrap();
        sqlx::query("UPDATE user SET disabled_at = 1 WHERE name = 'alice'")
            .execute(&mut *fixture.writer.lock().await)
            .await
            .unwrap();
        assert_eq!(start(&mut app, &new).await.map(drop), refused);
        let (status, cookie, json) =
            finish(&mut app, &ceremony, &mut authenticator(), ORIGIN, options).await;
        assert_eq!(
            (status, json["reason"].as_str()),
            (StatusCode::FORBIDDEN, Some("ENROLLMENT_INVALID"))
        );
        assert_eq!(cookie, None);
        assert_eq!(count(&fixture, "webauthn_credential").await, 0);
        assert_eq!(count(&fixture, "session").await, 0);
    }

    #[tokio::test]
    async fn a_foreign_origin_is_refused() {
        let (fixture, mut app) = fixture().await;
        let token = issue(&fixture, "alice", epoch_ms()).await;
        let (ceremony, options) = start(&mut app, &token).await.unwrap();
        let (status, _, json) = finish(
            &mut app,
            &ceremony,
            &mut authenticator(),
            "https://evil.example.org",
            options,
        )
        .await;
        assert_eq!(
            (status, json["reason"].as_str()),
            (StatusCode::FORBIDDEN, Some("VERIFICATION_FAILED"))
        );
        // The link stays usable for a genuine ceremony.
        assert!(start(&mut app, &token).await.is_ok());
        assert_eq!(count(&fixture, "webauthn_credential").await, 0);
    }

    #[tokio::test]
    async fn a_long_or_control_name_is_refused_before_the_ceremony() {
        let (fixture, mut app) = fixture().await;
        let token = issue(&fixture, "alice", epoch_ms()).await;
        let (ceremony, options) = start(&mut app, &token).await.unwrap();
        let credential = authenticator()
            .do_registration(Url::parse(ORIGIN).unwrap(), options)
            .unwrap();
        for name in ["x".repeat(65), "a\nb".into()] {
            let (status, _, json) = post(
                &mut app,
                "/auth/webauthn/enroll/finish",
                serde_json::json!({ "ceremony": ceremony, "credential": credential, "name": name }),
            )
            .await;
            assert_eq!(
                (status, json["reason"].as_str()),
                (StatusCode::BAD_REQUEST, Some("NAME_INVALID"))
            );
        }
        // Without a name: an empty one is stored.
        let (status, ..) = post(
            &mut app,
            "/auth/webauthn/enroll/finish",
            serde_json::json!({ "ceremony": ceremony, "credential": credential }),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let name: String = sqlx::query_scalar("SELECT name FROM webauthn_credential")
            .fetch_one(fixture.repos.readers().unwrap())
            .await
            .unwrap();
        assert_eq!(name, "");
    }

    #[tokio::test]
    async fn concurrent_finishes_consume_the_link_once() {
        let (fixture, app) = fixture().await;
        let token = issue(&fixture, "alice", epoch_ms()).await;
        let mut started = Vec::new();
        for _ in 0..2 {
            started.push(start(&mut app.clone(), &token).await.unwrap());
        }
        let mut finishes = Vec::new();
        for (ceremony, options) in started {
            let mut app = app.clone();
            finishes.push(tokio::spawn(async move {
                finish(&mut app, &ceremony, &mut authenticator(), ORIGIN, options)
                    .await
                    .0
            }));
        }
        let mut statuses = Vec::new();
        for finish in finishes {
            statuses.push(finish.await.unwrap());
        }
        statuses.sort();
        assert_eq!(statuses, [StatusCode::NO_CONTENT, StatusCode::FORBIDDEN]);
        assert_eq!(count(&fixture, "webauthn_credential").await, 1);
        assert_eq!(count(&fixture, "session").await, 1);
    }

    #[test]
    fn the_enrollment_token_never_reaches_the_logs() {
        const CHILD: &str = "KAJIBA_TEST_ENROLL_LOGS";
        if let Some(path) = std::env::var_os(CHILD) {
            kajiba_core::log::init("info").unwrap();
            tokio::runtime::Runtime::new().unwrap().block_on(async {
                let (fixture, mut app) = fixture().await;
                let token = issue(&fixture, "alice", epoch_ms()).await;
                std::fs::write(&path, &token).unwrap();
                let (ceremony, options) = start(&mut app, &token).await.unwrap();
                finish(&mut app, &ceremony, &mut authenticator(), ORIGIN, options).await;
                // Refused: consumed.
                start(&mut app, &token).await.unwrap_err();
            });
            return;
        }
        let file = std::env::temp_dir().join(format!("kajiba-enroll-logs-{}", std::process::id()));
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "transport::webauthn::tests::ceremonies::the_enrollment_token_never_reaches_the_logs",
                "--nocapture",
            ])
            .env(CHILD, &file)
            .env_remove("RUST_LOG")
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let token = std::fs::read_to_string(&file).unwrap();
        std::fs::remove_file(&file).unwrap();
        assert_eq!(token.len(), 64);
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(!stderr.contains(&token), "{stderr}");
        assert!(stderr.contains("\"ENROLLMENT_INVALID\""), "{stderr}");
    }

    /// Enrolls `user` with a new software authenticator: it, the credential id and the
    /// user handle.
    async fn enrolled(
        fixture: &Fixture,
        app: &mut Router,
        user: &str,
    ) -> (WebauthnAuthenticator<SoftPasskey>, Vec<u8>, Uuid) {
        let token = issue(fixture, user, epoch_ms()).await;
        let (ceremony, options) = start(app, &token).await.unwrap();
        let mut authenticator = authenticator();
        let credential = authenticator
            .do_registration(Url::parse(ORIGIN).unwrap(), options)
            .unwrap();
        let (status, ..) = post(
            app,
            "/auth/webauthn/enroll/finish",
            serde_json::json!({ "ceremony": ceremony, "credential": credential }),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let public_id: String = sqlx::query_scalar("SELECT public_id FROM user WHERE name = ?")
            .bind(user)
            .fetch_one(fixture.repos.readers().unwrap())
            .await
            .unwrap();
        let id = credential.raw_id.as_ref().to_vec();
        (authenticator, id, user_handle(&public_id))
    }

    /// Starts a login and signs its challenge as a browser would with a discoverable
    /// credential: the ceremony id and the assertion.
    async fn assertion(
        app: &mut Router,
        authenticator: &mut WebauthnAuthenticator<SoftPasskey>,
        credential_id: &[u8],
        handle: Uuid,
        origin: &str,
    ) -> (String, PublicKeyCredential) {
        use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
        let (status, _, mut json) =
            post(app, "/auth/webauthn/login/start", serde_json::json!({})).await;
        assert_eq!(status, StatusCode::OK);
        let options = &mut json["options"];
        assert_eq!(
            options["publicKey"]["allowCredentials"],
            serde_json::json!([])
        );
        assert_eq!(options["mediation"], serde_json::Value::Null);
        // The soft authenticator cannot discover: name the credential the user would pick.
        options["publicKey"]["allowCredentials"] = serde_json::json!([{ "type": "public-key", "id": URL_SAFE_NO_PAD.encode(credential_id) }]);
        let mut credential = authenticator
            .do_authentication(
                Url::parse(origin).unwrap(),
                serde_json::from_value(options.take()).unwrap(),
            )
            .unwrap();
        // Unsigned, set by a discoverable authenticator.
        credential.response.user_handle = Some(handle.as_bytes().to_vec().into());
        (json["ceremony"].as_str().unwrap().into(), credential)
    }

    async fn login(
        app: &mut Router,
        ceremony: &str,
        credential: &PublicKeyCredential,
    ) -> (StatusCode, Option<String>, Option<String>) {
        let (status, cookie, json) = post(
            app,
            "/auth/webauthn/login/finish",
            serde_json::json!({ "ceremony": ceremony, "credential": credential }),
        )
        .await;
        (status, cookie, json["reason"].as_str().map(Into::into))
    }

    fn refused(reason: &str) -> (StatusCode, Option<String>, Option<String>) {
        (StatusCode::FORBIDDEN, None, Some(reason.into()))
    }

    #[tokio::test]
    async fn login_opens_a_session_and_records_the_passkey_use() {
        let (fixture, mut app) = fixture().await;
        let (mut authenticator, id, handle) = enrolled(&fixture, &mut app, "alice").await;
        let (ceremony, credential) =
            assertion(&mut app, &mut authenticator, &id, handle, ORIGIN).await;
        let (status, cookie, _) = login(&mut app, &ceremony, &credential).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let cookie = cookie.unwrap();
        let value = cookie
            .strip_prefix("kajiba_session=")
            .unwrap()
            .split(';')
            .next()
            .unwrap();
        let session = crate::db::sessions::find(
            fixture.repos.readers().unwrap(),
            &kajiba_core::sha256(value.as_bytes()),
        )
        .await
        .unwrap()
        .unwrap();
        let alice: i64 = sqlx::query_scalar("SELECT id FROM user WHERE name = 'alice'")
            .fetch_one(fixture.repos.readers().unwrap())
            .await
            .unwrap();
        assert_eq!(session.user_id, alice);
        let (last_used_at, stored): (Option<i64>, String) =
            sqlx::query_as("SELECT last_used_at, passkey FROM webauthn_credential")
                .fetch_one(fixture.repos.readers().unwrap())
                .await
                .unwrap();
        assert!(last_used_at.is_some());
        serde_json::from_str::<Passkey>(&stored).unwrap();
        // Replays: the same ceremony again, the same assertion on a new challenge.
        assert_eq!(
            login(&mut app, &ceremony, &credential).await,
            refused("CEREMONY_INVALID")
        );
        let (status, _, mut json) = post(
            &mut app,
            "/auth/webauthn/login/start",
            serde_json::json!({}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let fresh = json["ceremony"].take();
        assert_eq!(
            login(&mut app, fresh.as_str().unwrap(), &credential).await,
            refused("VERIFICATION_FAILED")
        );
        // A second login works: the stored counter state follows.
        let (ceremony, credential) =
            assertion(&mut app, &mut authenticator, &id, handle, ORIGIN).await;
        assert_eq!(
            login(&mut app, &ceremony, &credential).await.0,
            StatusCode::NO_CONTENT
        );
    }

    #[tokio::test]
    async fn login_refuses_unknown_disabled_and_revoked_passkeys() {
        let (fixture, mut app) = fixture().await;
        let (mut alice, alice_id, alice_handle) = enrolled(&fixture, &mut app, "alice").await;
        let (mut bob, bob_id, bob_handle) = enrolled(&fixture, &mut app, "bob").await;
        // Another user's handle on alice's credential.
        let (ceremony, credential) =
            assertion(&mut app, &mut alice, &alice_id, bob_handle, ORIGIN).await;
        assert_eq!(
            login(&mut app, &ceremony, &credential).await,
            refused("PASSKEY_INVALID")
        );
        // A second admin enroll deletes alice's passkey: the credential is unknown.
        let (ceremony, credential) =
            assertion(&mut app, &mut alice, &alice_id, alice_handle, ORIGIN).await;
        issue(&fixture, "alice", epoch_ms()).await;
        assert_eq!(
            login(&mut app, &ceremony, &credential).await,
            refused("PASSKEY_INVALID")
        );
        // A disabled user.
        let (ceremony, credential) =
            assertion(&mut app, &mut bob, &bob_id, bob_handle, ORIGIN).await;
        sqlx::query("UPDATE user SET disabled_at = 1 WHERE name = 'bob'")
            .execute(&mut *fixture.writer.lock().await)
            .await
            .unwrap();
        assert_eq!(
            login(&mut app, &ceremony, &credential).await,
            refused("PASSKEY_INVALID")
        );
        // No login opened a session; the second link deleted alice's enrollment one.
        assert_eq!(count(&fixture, "session").await, 1);
    }

    #[tokio::test]
    async fn login_from_a_foreign_origin_is_refused() {
        let (fixture, mut app) = fixture().await;
        let (mut authenticator, id, handle) = enrolled(&fixture, &mut app, "alice").await;
        let (ceremony, credential) = assertion(
            &mut app,
            &mut authenticator,
            &id,
            handle,
            "https://evil.example.org",
        )
        .await;
        assert_eq!(
            login(&mut app, &ceremony, &credential).await,
            refused("VERIFICATION_FAILED")
        );
    }

    #[tokio::test]
    async fn an_enrollment_ceremony_cannot_finish_a_login() {
        let (fixture, mut app) = fixture().await;
        let (mut authenticator, id, handle) = enrolled(&fixture, &mut app, "alice").await;
        let token = issue(&fixture, "bob", epoch_ms()).await;
        let (enrollment, _) = start(&mut app, &token).await.unwrap();
        let (_, credential) = assertion(&mut app, &mut authenticator, &id, handle, ORIGIN).await;
        assert_eq!(
            login(&mut app, &enrollment, &credential).await,
            refused("CEREMONY_INVALID")
        );
    }
}
