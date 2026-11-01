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
        post_as(app, path, None, body).await
    }

    /// `post` with the session cookie `cookie`, if any.
    async fn post_as(
        app: &mut Router,
        path: &str,
        cookie: Option<&str>,
        body: serde_json::Value,
    ) -> (StatusCode, Option<String>, serde_json::Value) {
        let mut request = Request::post(path).header("content-type", "application/json");
        if let Some(cookie) = cookie {
            request = request.header("cookie", format!("kajiba_session={cookie}"));
        }
        let request = request.body(Body::from(body.to_string())).unwrap();
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
    async fn a_link_expiring_while_finish_waits_for_the_writer_is_refused() {
        let (fixture, mut app) = fixture().await;
        let token = issue(&fixture, "alice", epoch_ms()).await;
        let (ceremony, options) = start(&mut app, &token).await.unwrap();
        let mut authenticator = authenticator();
        let mut writer = fixture.writer.lock().await;
        let finish = finish(&mut app, &ceremony, &mut authenticator, ORIGIN, options);
        tokio::pin!(finish);
        // Poll through attestation verification until finish blocks on the held writer.
        tokio::select! {
            biased;
            _ = &mut finish => panic!("finish must wait for the writer"),
            _ = std::future::ready(()) => {}
        }
        let expires_at = epoch_ms() + 20;
        sqlx::query("UPDATE enrollment_token SET expires_at = ?")
            .bind(expires_at)
            .execute(&mut *writer)
            .await
            .unwrap();
        while epoch_ms() < expires_at {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        drop(writer);

        let (status, cookie, json) = finish.await;
        assert_eq!(
            (status, json["reason"].as_str()),
            (StatusCode::FORBIDDEN, Some("ENROLLMENT_INVALID"))
        );
        assert_eq!(cookie, None);
        assert_eq!(count(&fixture, "webauthn_credential").await, 0);
        assert_eq!(count(&fixture, "session").await, 0);
        let consumed_at: Option<i64> =
            sqlx::query_scalar("SELECT consumed_at FROM enrollment_token")
                .fetch_one(fixture.repos.readers().unwrap())
                .await
                .unwrap();
        assert_eq!(consumed_at, None);
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

    /// A raw POST: the status, `Retry-After` and the JSON reason, if any.
    async fn raw(
        app: &mut Router,
        path: &str,
        body: Vec<u8>,
    ) -> (StatusCode, Option<String>, Option<String>) {
        let request = Request::post(path)
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap();
        let response = app.call(request).await.unwrap();
        let status = response.status();
        let retry = response
            .headers()
            .get(axum::http::header::RETRY_AFTER)
            .map(|value| value.to_str().unwrap().to_owned());
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or_default();
        (status, retry, json["reason"].as_str().map(str::to_owned))
    }

    /// `{"token":"…"}` padded to exactly `size` bytes.
    fn enroll_body(size: usize) -> Vec<u8> {
        let body = format!("{{\"token\":\"{}\"}}", "a".repeat(size - 12));
        assert_eq!(body.len(), size);
        body.into_bytes()
    }

    #[tokio::test]
    async fn ceremony_bodies_are_capped_at_64_kib() {
        let (_fixture, mut app) = fixture().await;
        for path in [
            "/auth/webauthn/enroll/start",
            "/auth/webauthn/enroll/finish",
            "/auth/webauthn/login/finish",
            "/auth/webauthn/step-up/finish",
            "/auth/webauthn/register/finish",
        ] {
            let (status, _, _) = raw(&mut app, path, enroll_body(MAX_BODY + 1)).await;
            assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{path}");
        }
        // Exactly at the cap: the handler answers.
        assert_eq!(
            raw(
                &mut app,
                "/auth/webauthn/enroll/start",
                enroll_body(MAX_BODY)
            )
            .await,
            (
                StatusCode::FORBIDDEN,
                None,
                Some("ENROLLMENT_INVALID".into())
            )
        );
    }

    #[tokio::test]
    async fn ceremony_requests_share_a_budget_taken_before_the_body() {
        let (fixture, mut app) = fixture().await;
        let budget = fixture.repos.0.webauthn_requests.clone();
        assert_eq!(budget.available_permits(), MAX_REQUESTS);
        let held = budget
            .clone()
            .try_acquire_many_owned(MAX_REQUESTS as u32)
            .unwrap();
        let busy = (
            StatusCode::SERVICE_UNAVAILABLE,
            Some("1".into()),
            Some("BUSY".into()),
        );
        for path in [
            "/auth/webauthn/enroll/start",
            "/auth/webauthn/enroll/finish",
            "/auth/webauthn/login/start",
            "/auth/webauthn/login/finish",
            "/auth/webauthn/step-up/start",
            "/auth/webauthn/step-up/finish",
            "/auth/webauthn/register/start",
            "/auth/webauthn/register/finish",
        ] {
            // An oversized body is not read: refused as busy, not as too large.
            assert_eq!(
                raw(&mut app, path, enroll_body(MAX_BODY + 1)).await,
                busy,
                "{path}"
            );
        }
        drop(held);
        assert_eq!(
            raw(&mut app, "/auth/webauthn/enroll/start", enroll_body(64)).await,
            (
                StatusCode::FORBIDDEN,
                None,
                Some("ENROLLMENT_INVALID".into())
            )
        );
        // Every permit is back once the answers are sent.
        assert_eq!(budget.available_permits(), MAX_REQUESTS);
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
    async fn recovery_reusing_a_row_id_cannot_record_a_revoked_login() {
        for replacement_user in ["alice", "bob"] {
            let (fixture, mut app) = fixture().await;
            let (_, credential_id, _) = enrolled(&fixture, &mut app, "alice").await;
            // Capture the state an in-flight login read before recovery.
            let mut writer = fixture.writer.lock().await;
            let (public_id, user, _, stored) =
                crate::db::passkeys::find(&mut writer, &credential_id)
                    .await
                    .unwrap()
                    .unwrap();
            let row_id: i64 = sqlx::query_scalar("SELECT id FROM webauthn_credential")
                .fetch_one(&mut *writer)
                .await
                .unwrap();
            drop(writer);
            issue(&fixture, "alice", epoch_ms()).await;
            enrolled(&fixture, &mut app, replacement_user).await;
            let mut writer = fixture.writer.lock().await;
            let (replacement_id, replacement): (i64, String) =
                sqlx::query_as("SELECT id, passkey FROM webauthn_credential")
                    .fetch_one(&mut *writer)
                    .await
                    .unwrap();
            assert_eq!(replacement_id, row_id, "SQLite reused the revoked row ID");
            assert!(
                !crate::db::passkeys::used(&mut writer, &public_id, user, &stored, epoch_ms())
                    .await
                    .unwrap()
            );
            let (current, last_used_at): (String, Option<i64>) =
                sqlx::query_as("SELECT passkey, last_used_at FROM webauthn_credential")
                    .fetch_one(&mut *writer)
                    .await
                    .unwrap();
            assert_eq!(current, replacement);
            assert_eq!(last_used_at, None);
        }
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

    fn cookie_value(set_cookie: &str) -> String {
        let value = set_cookie.strip_prefix("kajiba_session=").unwrap();
        value.split(';').next().unwrap().into()
    }

    /// Signs in with `authenticator`'s credential: the session cookie value.
    async fn signed_in(
        app: &mut Router,
        authenticator: &mut WebauthnAuthenticator<SoftPasskey>,
        id: &[u8],
        handle: Uuid,
    ) -> String {
        let (ceremony, credential) = assertion(app, authenticator, id, handle, ORIGIN).await;
        let (status, cookie, _) = login(app, &ceremony, &credential).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        cookie_value(&cookie.unwrap())
    }

    /// Starts a step-up for session `cookie` and signs it from `origin` with credential `id`
    /// of `authenticator`, whether the server allowed it or not: the ceremony and assertion.
    async fn step_up_assertion(
        app: &mut Router,
        cookie: &str,
        authenticator: &mut WebauthnAuthenticator<SoftPasskey>,
        id: &[u8],
        origin: &str,
    ) -> (String, PublicKeyCredential) {
        use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
        let (status, _, mut json) = post_as(
            app,
            "/auth/webauthn/step-up/start",
            Some(cookie),
            serde_json::json!({}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{json}");
        let options = &mut json["options"];
        options["publicKey"]["allowCredentials"] =
            serde_json::json!([{ "type": "public-key", "id": URL_SAFE_NO_PAD.encode(id) }]);
        let credential = authenticator
            .do_authentication(
                Url::parse(origin).unwrap(),
                serde_json::from_value(options.take()).unwrap(),
            )
            .unwrap();
        (json["ceremony"].as_str().unwrap().into(), credential)
    }

    /// Finishes a step-up: the status, any `Set-Cookie` and the refusal reason.
    async fn finish_step_up(
        app: &mut Router,
        cookie: &str,
        ceremony: &str,
        credential: &PublicKeyCredential,
    ) -> (StatusCode, Option<String>, Option<String>) {
        let (status, set_cookie, json) = post_as(
            app,
            "/auth/webauthn/step-up/finish",
            Some(cookie),
            serde_json::json!({ "ceremony": ceremony, "credential": credential }),
        )
        .await;
        (status, set_cookie, json["reason"].as_str().map(Into::into))
    }

    async fn step_up(
        app: &mut Router,
        cookie: &str,
        authenticator: &mut WebauthnAuthenticator<SoftPasskey>,
        id: &[u8],
    ) {
        let (ceremony, credential) =
            step_up_assertion(app, cookie, authenticator, id, ORIGIN).await;
        assert_eq!(
            finish_step_up(app, cookie, &ceremony, &credential).await,
            (StatusCode::NO_CONTENT, None, None)
        );
    }

    async fn register_start(
        app: &mut Router,
        cookie: Option<&str>,
    ) -> Result<(String, CreationChallengeResponse), (StatusCode, String)> {
        let (status, _, json) = post_as(
            app,
            "/auth/webauthn/register/start",
            cookie,
            serde_json::json!({}),
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

    /// Registers a new authenticator for session `cookie`: the status, any `Set-Cookie`,
    /// the refusal reason, and the authenticator with its credential id.
    async fn register(
        app: &mut Router,
        cookie: &str,
        ceremony: &str,
        options: CreationChallengeResponse,
    ) -> (
        (StatusCode, Option<String>, Option<String>),
        WebauthnAuthenticator<SoftPasskey>,
        Vec<u8>,
    ) {
        let mut authenticator = authenticator();
        let credential = authenticator
            .do_registration(Url::parse(ORIGIN).unwrap(), options)
            .unwrap();
        let (status, set_cookie, json) = post_as(
            app,
            "/auth/webauthn/register/finish",
            Some(cookie),
            serde_json::json!({ "ceremony": ceremony, "credential": credential, "name": "backup" }),
        )
        .await;
        let id = credential.raw_id.as_ref().to_vec();
        (
            (status, set_cookie, json["reason"].as_str().map(Into::into)),
            authenticator,
            id,
        )
    }

    fn step_up_required() -> Result<(), (StatusCode, String)> {
        Err((StatusCode::FORBIDDEN, "STEP_UP_REQUIRED".into()))
    }

    async fn passkeys_of(fixture: &Fixture, user: &str) -> Vec<(String, String)> {
        sqlx::query_as(
            "SELECT webauthn_credential.public_id, webauthn_credential.name FROM webauthn_credential
             JOIN user ON user.id = user_id WHERE user.name = ? ORDER BY webauthn_credential.id",
        )
        .bind(user)
        .fetch_all(fixture.repos.readers().unwrap())
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn registration_options_match_enrollment() {
        let (fixture, mut app) = fixture().await;
        let token = issue(&fixture, "alice", epoch_ms()).await;
        let (status, _, enrollment) = post(
            &mut app,
            "/auth/webauthn/enroll/start",
            serde_json::json!({ "token": token }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let mut first = authenticator();
        let credential = first
            .do_registration(
                Url::parse(ORIGIN).unwrap(),
                serde_json::from_value(enrollment["options"].clone()).unwrap(),
            )
            .unwrap();
        let (status, cookie, _) = post(
            &mut app,
            "/auth/webauthn/enroll/finish",
            serde_json::json!({ "ceremony": enrollment["ceremony"], "credential": credential }),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let cookie = cookie_value(&cookie.unwrap());
        step_up(&mut app, &cookie, &mut first, credential.raw_id.as_ref()).await;
        let (status, _, registration) = post_as(
            &mut app,
            "/auth/webauthn/register/start",
            Some(&cookie),
            serde_json::json!({}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let enrollment = &enrollment["options"]["publicKey"];
        let registration = &registration["options"]["publicKey"];
        for field in [
            "authenticatorSelection",
            "attestation",
            "hints",
            "extensions",
            "timeout",
            "pubKeyCredParams",
            "rp",
            "user",
        ] {
            assert_eq!(registration.get(field), enrollment.get(field), "{field}");
        }
        assert_eq!(
            enrollment["authenticatorSelection"]["residentKey"],
            "required"
        );
        assert_ne!(registration["challenge"], enrollment["challenge"]);
        assert!(
            enrollment["excludeCredentials"]
                .as_array()
                .is_none_or(Vec::is_empty)
        );
        let excluded = registration["excludeCredentials"].as_array().unwrap();
        assert_eq!(excluded.len(), 1);
        assert_eq!(excluded[0]["id"], serde_json::json!(credential.raw_id));
    }

    #[tokio::test]
    async fn a_step_up_lets_the_session_add_a_passkey() {
        let (fixture, mut app) = fixture().await;
        let (mut first, first_id, handle) = enrolled(&fixture, &mut app, "alice").await;
        let cookie = signed_in(&mut app, &mut first, &first_id, handle).await;
        let session_invalid = Err((StatusCode::UNAUTHORIZED, "SESSION_INVALID".into()));
        assert_eq!(
            register_start(&mut app, None).await.map(drop),
            session_invalid
        );
        let (status, _, json) = post(
            &mut app,
            "/auth/webauthn/step-up/start",
            serde_json::json!({}),
        )
        .await;
        assert_eq!(
            (status, json["reason"].as_str()),
            (StatusCode::UNAUTHORIZED, Some("SESSION_INVALID"))
        );
        // An open session is not enough.
        assert_eq!(
            register_start(&mut app, Some(&cookie)).await.map(drop),
            step_up_required()
        );

        step_up(&mut app, &cookie, &mut first, &first_id).await;
        let (ceremony, options) = register_start(&mut app, Some(&cookie)).await.unwrap();
        let json = serde_json::json!(options);
        assert_eq!(
            json["publicKey"]["authenticatorSelection"]["residentKey"],
            "required"
        );
        assert_eq!(
            options.public_key.user.id.as_ref(),
            handle.as_bytes(),
            "the user handle of enrollment"
        );
        let excluded: Vec<Vec<u8>> = options
            .public_key
            .exclude_credentials
            .clone()
            .unwrap_or_default()
            .iter()
            .map(|credential| credential.id.as_ref().to_vec())
            .collect();
        assert_eq!(excluded, std::slice::from_ref(&first_id));
        let (answer, mut second, second_id) = register(&mut app, &cookie, &ceremony, options).await;
        // No new session: the caller's stays.
        assert_eq!(answer, (StatusCode::NO_CONTENT, None, None));
        let names: Vec<_> = passkeys_of(&fixture, "alice")
            .await
            .into_iter()
            .map(|(_, name)| name)
            .collect();
        assert_eq!(names, ["", "backup"]);
        assert_eq!(count(&fixture, "session").await, 2);
        // The new passkey signs in; the ceremony cannot be finished twice.
        signed_in(&mut app, &mut second, &second_id, handle).await;
        let (_, options) = register_start(&mut app, Some(&cookie)).await.unwrap();
        let (answer, ..) = register(&mut app, &cookie, &ceremony, options).await;
        assert_eq!(answer, refused("CEREMONY_INVALID"));
    }

    #[tokio::test]
    async fn a_step_up_refuses_replays_other_sessions_and_other_users() {
        let (fixture, mut app) = fixture().await;
        let (mut alice, alice_id, alice_handle) = enrolled(&fixture, &mut app, "alice").await;
        let (mut bob, bob_id, bob_handle) = enrolled(&fixture, &mut app, "bob").await;
        let cookie = signed_in(&mut app, &mut alice, &alice_id, alice_handle).await;
        let other = signed_in(&mut app, &mut alice, &alice_id, alice_handle).await;
        let bob_cookie = signed_in(&mut app, &mut bob, &bob_id, bob_handle).await;

        // Another session of the same user cannot finish it, and that consumed it.
        let (ceremony, credential) =
            step_up_assertion(&mut app, &cookie, &mut alice, &alice_id, ORIGIN).await;
        assert_eq!(
            finish_step_up(&mut app, &other, &ceremony, &credential).await,
            refused("CEREMONY_INVALID")
        );
        assert_eq!(
            finish_step_up(&mut app, &cookie, &ceremony, &credential).await,
            refused("CEREMONY_INVALID")
        );
        // The same assertion on a new challenge.
        let (ceremony, _) =
            step_up_assertion(&mut app, &cookie, &mut alice, &alice_id, ORIGIN).await;
        assert_eq!(
            finish_step_up(&mut app, &cookie, &ceremony, &credential).await,
            refused("VERIFICATION_FAILED")
        );
        // A login ceremony is not a step-up one.
        let (login_ceremony, _) =
            assertion(&mut app, &mut alice, &alice_id, alice_handle, ORIGIN).await;
        let (_, credential) =
            step_up_assertion(&mut app, &cookie, &mut alice, &alice_id, ORIGIN).await;
        assert_eq!(
            finish_step_up(&mut app, &cookie, &login_ceremony, &credential).await,
            refused("CEREMONY_INVALID")
        );
        // Bob's session signed by alice's passkey.
        let (ceremony, credential) =
            step_up_assertion(&mut app, &bob_cookie, &mut alice, &alice_id, ORIGIN).await;
        assert_eq!(
            finish_step_up(&mut app, &bob_cookie, &ceremony, &credential).await,
            refused("PASSKEY_INVALID")
        );
        // Another origin.
        let (ceremony, credential) = step_up_assertion(
            &mut app,
            &cookie,
            &mut alice,
            &alice_id,
            "https://evil.example.org",
        )
        .await;
        assert_eq!(
            finish_step_up(&mut app, &cookie, &ceremony, &credential).await,
            refused("VERIFICATION_FAILED")
        );
        for cookie in [&cookie, &other, &bob_cookie] {
            assert_eq!(
                register_start(&mut app, Some(cookie)).await.map(drop),
                step_up_required()
            );
        }
        // A step-up counts for its own session only.
        step_up(&mut app, &cookie, &mut alice, &alice_id).await;
        assert!(register_start(&mut app, Some(&cookie)).await.is_ok());
        assert_eq!(
            register_start(&mut app, Some(&other)).await.map(drop),
            step_up_required()
        );
    }

    async fn age_step_ups(fixture: &Fixture, millis: i64) {
        sqlx::query("UPDATE session SET step_up_at = step_up_at - ?")
            .bind(millis)
            .execute(&mut *fixture.writer.lock().await)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn a_step_up_expires_after_five_minutes() {
        let (fixture, mut app) = fixture().await;
        let (mut alice, alice_id, handle) = enrolled(&fixture, &mut app, "alice").await;
        let cookie = signed_in(&mut app, &mut alice, &alice_id, handle).await;
        step_up(&mut app, &cookie, &mut alice, &alice_id).await;
        let ttl = STEP_UP_TTL.as_millis() as i64;
        age_step_ups(&fixture, ttl - 10_000).await;
        let (ceremony, options) = register_start(&mut app, Some(&cookie)).await.unwrap();
        // Expired between start and finish: nothing stored.
        age_step_ups(&fixture, 10_000).await;
        let (answer, ..) = register(&mut app, &cookie, &ceremony, options).await;
        assert_eq!(answer, refused("STEP_UP_REQUIRED"));
        assert_eq!(passkeys_of(&fixture, "alice").await.len(), 1);
        assert_eq!(
            register_start(&mut app, Some(&cookie)).await.map(drop),
            step_up_required()
        );
        // A new step-up opens a new window.
        step_up(&mut app, &cookie, &mut alice, &alice_id).await;
        assert!(register_start(&mut app, Some(&cookie)).await.is_ok());
    }

    /// `alice`'s session `cookie` as the auth layer passes it to the passkey service.
    async fn as_session<T>(fixture: &Fixture, cookie: &str, message: T) -> tonic::Request<T> {
        let user: i64 = sqlx::query_scalar("SELECT id FROM user WHERE name = 'alice'")
            .fetch_one(fixture.repos.readers().unwrap())
            .await
            .unwrap();
        let mut request = tonic::Request::new(message);
        request
            .extensions_mut()
            .insert(crate::transport::auth::Principal {
                user_id: user,
                scopes: crate::transport::auth::scopes(None),
                session: Some(kajiba_core::sha256(cookie.as_bytes())),
            });
        request
    }

    async fn delete(fixture: &Fixture, cookie: &str, public_id: &str) -> Result<(), String> {
        use kajiba_proto::v1::{DeletePasskeyRequest, passkey_service_server::PasskeyService};
        use tonic_types::StatusExt;
        let service = crate::transport::passkeys::Passkeys(fixture.repos.0.clone());
        let request = as_session(
            fixture,
            cookie,
            DeletePasskeyRequest {
                public_id: public_id.into(),
            },
        )
        .await;
        service
            .delete_passkey(request)
            .await
            .map(drop)
            .map_err(|status| {
                format!(
                    "{:?} {}",
                    status.code(),
                    status.get_details_error_info().unwrap().reason
                )
            })
    }

    #[tokio::test]
    async fn removing_a_passkey_needs_a_step_up_and_keeps_the_last_one() {
        use kajiba_proto::v1::{ListPasskeysRequest, passkey_service_server::PasskeyService};
        let (fixture, mut app) = fixture().await;
        let (mut first, first_id, handle) = enrolled(&fixture, &mut app, "alice").await;
        let (_, _, _) = enrolled(&fixture, &mut app, "bob").await;
        let cookie = signed_in(&mut app, &mut first, &first_id, handle).await;
        step_up(&mut app, &cookie, &mut first, &first_id).await;
        let (ceremony, options) = register_start(&mut app, Some(&cookie)).await.unwrap();
        let (_, mut second, second_id) = register(&mut app, &cookie, &ceremony, options).await;
        let alice = passkeys_of(&fixture, "alice").await;
        let bob = passkeys_of(&fixture, "bob").await;

        // Listed oldest first, one page at a time; bob's are not alice's.
        let service = crate::transport::passkeys::Passkeys(fixture.repos.0.clone());
        let mut listed = Vec::new();
        let mut page_token = String::new();
        loop {
            let request = as_session(
                &fixture,
                &cookie,
                ListPasskeysRequest {
                    page_size: 1,
                    page_token,
                },
            )
            .await;
            let page = service.list_passkeys(request).await.unwrap().into_inner();
            assert_eq!(page.passkeys.len(), 1);
            listed.extend(page.passkeys.into_iter().map(|p| (p.public_id, p.name)));
            page_token = page.next_page_token;
            if page_token.is_empty() {
                break;
            }
        }
        assert_eq!(listed, alice);

        assert_eq!(
            delete(&fixture, &cookie, &bob[0].0).await,
            Err("NotFound PASSKEY_NOT_FOUND".into())
        );
        age_step_ups(&fixture, STEP_UP_TTL.as_millis() as i64).await;
        assert_eq!(
            delete(&fixture, &cookie, &alice[0].0).await,
            Err("PermissionDenied STEP_UP_REQUIRED".into())
        );
        step_up(&mut app, &cookie, &mut second, &second_id).await;
        delete(&fixture, &cookie, &alice[0].0).await.unwrap();
        // The removed passkey no longer signs in; the session it opened stays.
        let (ceremony, credential) =
            assertion(&mut app, &mut first, &first_id, handle, ORIGIN).await;
        assert_eq!(
            login(&mut app, &ceremony, &credential).await,
            refused("PASSKEY_INVALID")
        );
        assert!(register_start(&mut app, Some(&cookie)).await.is_ok());
        assert_eq!(
            delete(&fixture, &cookie, &alice[1].0).await,
            Err("FailedPrecondition LAST_PASSKEY".into())
        );
        assert_eq!(passkeys_of(&fixture, "alice").await, alice[1..]);
        signed_in(&mut app, &mut second, &second_id, handle).await;

        // Recovery still deletes every passkey of the user, and its sessions.
        let (ceremony, options) = register_start(&mut app, Some(&cookie)).await.unwrap();
        register(&mut app, &cookie, &ceremony, options).await;
        assert_eq!(passkeys_of(&fixture, "alice").await.len(), 2);
        issue(&fixture, "alice", epoch_ms()).await;
        assert_eq!(passkeys_of(&fixture, "alice").await, []);
        assert_eq!(passkeys_of(&fixture, "bob").await, bob);
        assert_eq!(
            delete(&fixture, &cookie, &alice[1].0).await,
            Err("Unauthenticated CREDENTIAL_INVALID".into())
        );
    }
}
