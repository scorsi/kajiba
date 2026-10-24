use super::users::declared;
use super::*;
use crate::db::enrollment::{Error, issue};

const TTL: Duration = Duration::from_secs(15 * 60);

async fn setup(writer: &mut SqliteConnection) {
    let config = declared(&format!(
        "[users.alice]\n[users.alice.tokens.ci]\nsha256='{}'\nscopes=['repo:read']\n[users.bob]",
        "a".repeat(64)
    ));
    reconcile(writer, &config).await.unwrap();
    // Canonical ULIDs, zero-padded with distinct suffixes.
    for (user, passkey, pat) in [("alice", "0W", "0T"), ("bob", "1W", "1T")] {
        sqlx::query(
            "INSERT INTO webauthn_credential (public_id, user_id, credential_id, passkey, name, created_at)
             SELECT ?, id, CAST(name AS BLOB), '{}', 'passkey', 0 FROM user WHERE name = ?",
        )
        .bind(format!("{:0>26}", passkey))
        .bind(user)
        .execute(&mut *writer)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO api_token (public_id, user_id, name, token_hash, scopes, source, created_at)
             SELECT ?, id, 'cli', CAST(name AS BLOB), 'repo:read', 'runtime', 0 FROM user WHERE name = ?",
        )
        .bind(format!("{:0>26}", pat))
        .bind(user)
        .execute(&mut *writer)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO session (user_id, token_hash, created_at, expires_at, last_seen_at)
             SELECT id, CAST(name AS BLOB), 0, 1000, 0 FROM user WHERE name = ?",
        )
        .bind(user)
        .execute(&mut *writer)
        .await
        .unwrap();
    }
}

async fn count(writer: &mut SqliteConnection, table: &str, user: &str) -> i64 {
    sqlx::query_scalar(&format!(
        "SELECT count(*) FROM {table} WHERE user_id = (SELECT id FROM user WHERE name = ?)"
    ))
    .bind(user)
    .fetch_one(writer)
    .await
    .unwrap()
}

async fn tokens(writer: &mut SqliteConnection, user: &str) -> Vec<(String, Option<i64>)> {
    sqlx::query_as(
        "SELECT source, revoked_at FROM api_token
         WHERE user_id = (SELECT id FROM user WHERE name = ?) ORDER BY source",
    )
    .bind(user)
    .fetch_all(writer)
    .await
    .unwrap()
}

async fn links(writer: &mut SqliteConnection) -> Vec<(String, Vec<u8>, i64, i64, Option<i64>)> {
    sqlx::query_as(
        "SELECT user.name, token_hash, enrollment_token.created_at, expires_at, consumed_at
         FROM enrollment_token JOIN user ON user.id = user_id",
    )
    .fetch_all(writer)
    .await
    .unwrap()
}

#[tokio::test]
async fn revokes_passkeys_sessions_and_runtime_pats_only_and_records_ttl() {
    let dir = TempDir::new();
    let (mut writer, _readers) = open(&dir.database()).await.unwrap();
    setup(&mut writer).await;
    let token = issue(&mut writer, "Alice", TTL, 1_000).await.unwrap();
    assert_eq!(token.len(), 64);
    assert!(
        token
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    );
    assert_eq!(count(&mut writer, "webauthn_credential", "alice").await, 0);
    assert_eq!(count(&mut writer, "session", "alice").await, 0);
    assert_eq!(
        tokens(&mut writer, "alice").await,
        [("config".into(), None), ("runtime".into(), Some(1_000))]
    );
    // Another user's credentials are untouched.
    assert_eq!(count(&mut writer, "webauthn_credential", "bob").await, 1);
    assert_eq!(count(&mut writer, "session", "bob").await, 1);
    assert_eq!(tokens(&mut writer, "bob").await, [("runtime".into(), None)]);
    assert_eq!(
        links(&mut writer).await,
        [(
            "alice".into(),
            kajiba_core::sha256(token.as_bytes()).to_vec(),
            1_000,
            1_000 + 15 * 60 * 1000,
            None
        )]
    );
}

#[tokio::test]
async fn second_link_replaces_the_first_and_keeps_earlier_revocation_time() {
    let dir = TempDir::new();
    let (mut writer, _readers) = open(&dir.database()).await.unwrap();
    setup(&mut writer).await;
    let first = issue(&mut writer, "alice", TTL, 1_000).await.unwrap();
    let second = issue(&mut writer, "alice", Duration::from_secs(60), 5_000)
        .await
        .unwrap();
    assert_ne!(first, second);
    let rows = links(&mut writer).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].1, kajiba_core::sha256(second.as_bytes()));
    assert_eq!((rows[0].2, rows[0].3), (5_000, 65_000));
    assert_eq!(
        tokens(&mut writer, "alice").await[1],
        ("runtime".into(), Some(1_000))
    );
}

#[tokio::test]
async fn unknown_or_disabled_user_is_refused_without_writes() {
    let dir = TempDir::new();
    let (mut writer, _readers) = open(&dir.database()).await.unwrap();
    setup(&mut writer).await;
    issue(&mut writer, "bob", TTL, 500).await.unwrap();
    // Removing bob from the configuration disables him and deletes his sessions, passkeys and
    // pending link, and revokes his runtime PATs (ADR 0025).
    reconcile(&mut writer, &declared("[users.alice]"))
        .await
        .unwrap();
    assert!(links(&mut writer).await.is_empty());
    let before = super::users::changes(&mut writer).await;
    assert!(matches!(
        issue(&mut writer, "carol", TTL, 1_000).await,
        Err(Error::UnknownUser)
    ));
    assert!(matches!(
        issue(&mut writer, "bob", TTL, 1_000).await,
        Err(Error::Disabled)
    ));
    assert_eq!(super::users::changes(&mut writer).await, before);
    assert_eq!(count(&mut writer, "webauthn_credential", "bob").await, 0);
    assert!(links(&mut writer).await.is_empty());
    // Declared again: no passkey came back, a fresh link is the only way in.
    reconcile(&mut writer, &declared("[users.alice]\n[users.bob]"))
        .await
        .unwrap();
    assert_eq!(count(&mut writer, "webauthn_credential", "bob").await, 0);
    assert!(links(&mut writer).await.is_empty());
    issue(&mut writer, "bob", TTL, 2_000).await.unwrap();
    assert_eq!(links(&mut writer).await.len(), 1);
}

#[tokio::test]
async fn unrepresentable_expiry_is_refused_before_any_write() {
    let dir = TempDir::new();
    let (mut writer, _readers) = open(&dir.database()).await.unwrap();
    setup(&mut writer).await;
    let before = super::users::changes(&mut writer).await;
    for (ttl, now) in [(Duration::MAX, 1_000), (Duration::from_secs(60), i64::MAX)] {
        assert!(matches!(
            issue(&mut writer, "alice", ttl, now).await,
            Err(Error::Expiry)
        ));
    }
    assert_eq!(super::users::changes(&mut writer).await, before);
    assert_eq!(count(&mut writer, "webauthn_credential", "alice").await, 1);
    assert!(links(&mut writer).await.is_empty());
}
