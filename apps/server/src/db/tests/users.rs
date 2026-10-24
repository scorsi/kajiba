use super::*;
use crate::db::users::reconcile_users;
use kajiba_core::config::{ServerConfig, UserConfig};
use std::collections::BTreeMap;

pub(super) fn declared(input: &str) -> BTreeMap<String, UserConfig> {
    ServerConfig::parse(&format!(
        "[http]\npublic_url='https://example.org'\n{input}"
    ))
    .unwrap()
    .users
}

type UserRow = (i64, String, String, String, bool, String, i64, Option<i64>);

async fn users(writer: &mut SqliteConnection) -> Vec<UserRow> {
    sqlx::query_as("SELECT id, public_id, name, kind, is_admin, source, created_at, disabled_at FROM user ORDER BY name")
        .fetch_all(writer).await.unwrap()
}

pub(super) async fn changes(writer: &mut SqliteConnection) -> i64 {
    sqlx::query_scalar("SELECT total_changes()")
        .fetch_one(writer)
        .await
        .unwrap()
}

#[tokio::test]
async fn adds_declared_users_with_new_canonical_ulids_and_noop_writes_nothing() {
    let dir = TempDir::new();
    let (mut writer, readers) = open(&dir.database()).await.unwrap();
    let config = declared("[users.Alice]\nis_admin=true\n[users.bob]");
    reconcile_users(&mut writer, &config).await.unwrap();
    let rows = users(&mut writer).await;
    assert_eq!(rows.len(), 2);
    assert_ne!(rows[0].1, rows[1].1);
    for (row, name, admin) in [(&rows[0], "alice", true), (&rows[1], "bob", false)] {
        assert_eq!(row.2, name);
        assert_eq!(row.3, "local");
        assert_eq!(row.4, admin);
        assert_eq!(row.5, "config");
        assert!(row.6 > 0);
        assert_eq!(row.7, None);
        let bytes = row.1.as_bytes();
        assert_eq!(bytes.len(), 26);
        assert!(bytes[0] <= b'7');
        assert!(
            bytes
                .iter()
                .all(|byte| b"0123456789ABCDEFGHJKMNPQRSTVWXYZ".contains(byte))
        );
        let timestamp = bytes[..10].iter().fold(0_i64, |value, byte| {
            value * 32
                + b"0123456789ABCDEFGHJKMNPQRSTVWXYZ"
                    .iter()
                    .position(|b| b == byte)
                    .unwrap() as i64
        });
        assert_eq!(timestamp, row.6);
    }
    let before = changes(&mut writer).await;
    reconcile_users(&mut writer, &config).await.unwrap();
    assert_eq!(changes(&mut writer).await, before);
    assert_eq!(users(&mut writer).await, rows);
    readers.close().await;
    writer.close().await.unwrap();
}

#[tokio::test]
async fn changes_admin_in_both_directions_without_replacing_identity() {
    let dir = TempDir::new();
    let (mut writer, readers) = open(&dir.database()).await.unwrap();
    reconcile_users(&mut writer, &declared("[users.alice]"))
        .await
        .unwrap();
    let mut expected = users(&mut writer).await;
    for admin in [true, false] {
        reconcile_users(
            &mut writer,
            &declared(&format!("[users.alice]\nis_admin={admin}")),
        )
        .await
        .unwrap();
        expected[0].4 = admin;
        assert_eq!(users(&mut writer).await, expected);
    }
    readers.close().await;
    writer.close().await.unwrap();
}

async fn credentials(writer: &mut SqliteConnection) {
    sqlx::raw_sql(
        "INSERT INTO user (id, public_id, name, kind, is_admin, source, created_at)
         VALUES (99, '00000000000000000000000099', 'outside', 'external', 0, 'runtime', 0);
         INSERT INTO session (user_id, token_hash, created_at, expires_at, last_seen_at)
         SELECT id, CAST(printf('%032d', id) AS BLOB), 0, 1000, 0 FROM user;
         INSERT INTO api_token (public_id, user_id, name, token_hash, scopes, source, created_at)
         SELECT public_id, id, 'cli', CAST(printf('%032d', id) AS BLOB), 'repo:read', 'runtime', 0 FROM user;
         INSERT INTO api_token (public_id, user_id, name, token_hash, scopes, source, created_at, revoked_at)
         SELECT '00000000000000000000000098', id, 'declared', zeroblob(32), 'admin', 'config', 0, NULL FROM user WHERE name = 'alice';
         INSERT INTO api_token (public_id, user_id, name, token_hash, scopes, source, created_at, revoked_at)
         SELECT '00000000000000000000000097', id, 'old', X'01', 'repo:read', 'runtime', 0, 42 FROM user WHERE name = 'alice';
         INSERT INTO repo (public_id, owner_id, name, visibility, ci_enabled, default_branch, description, created_at)
         SELECT public_id, id, 'demo', 'public', 0, 'main', 'kept', 0 FROM user WHERE name = 'alice';
         INSERT INTO ssh_key (public_id, user_id, usage, key_type, public_key, fingerprint, source, created_at)
         SELECT public_id, id, 'auth', 'ssh-ed25519', 'opaque', 'SHA256:opaque', 'config', 0 FROM user WHERE name = 'alice';
         INSERT INTO webauthn_credential (public_id, user_id, credential_id, passkey, name, created_at)
         SELECT public_id, id, X'01', '{}', 'passkey', 0 FROM user WHERE name = 'alice';
         INSERT INTO enrollment_token (user_id, token_hash, created_at, expires_at)
         SELECT id, X'01', 0, 1000 FROM user WHERE name = 'alice';",
    ).execute(writer).await.unwrap();
}

#[tokio::test]
async fn removal_revokes_only_its_sessions_and_runtime_pats_and_return_reenables() {
    let dir = TempDir::new();
    let (mut writer, readers) = open(&dir.database()).await.unwrap();
    let initial = declared("[users.alice]\nis_admin=true\n[users.bob]");
    reconcile_users(&mut writer, &initial).await.unwrap();
    credentials(&mut writer).await;
    let original = users(&mut writer).await;
    let remaining = declared("[users.bob]");
    reconcile_users(&mut writer, &remaining).await.unwrap();
    let removed = users(&mut writer).await;
    let mut expected = original.clone();
    expected[0].7 = removed[0].7;
    assert!(expected[0].7.unwrap() > 0);
    assert_eq!(removed, expected);
    let sessions: Vec<String> = sqlx::query_scalar(
        "SELECT user.name FROM session JOIN user ON user.id = session.user_id ORDER BY user.name",
    )
    .fetch_all(&mut writer)
    .await
    .unwrap();
    assert_eq!(sessions, ["bob", "outside"]);
    let tokens: Vec<(String, String, Option<i64>)> = sqlx::query_as("SELECT user.name, api_token.name, revoked_at FROM api_token JOIN user ON user.id = api_token.user_id ORDER BY user.name, api_token.name")
        .fetch_all(&mut writer).await.unwrap();
    assert_eq!(
        tokens,
        vec![
            ("alice".into(), "cli".into(), removed[0].7),
            ("alice".into(), "declared".into(), None),
            ("alice".into(), "old".into(), Some(42)),
            ("bob".into(), "cli".into(), None),
            ("outside".into(), "cli".into(), None),
        ]
    );
    for table in ["repo", "ssh_key", "webauthn_credential", "enrollment_token"] {
        let count: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
            .fetch_one(&mut writer)
            .await
            .unwrap();
        assert_eq!(count, 1, "{table}");
    }
    let before = changes(&mut writer).await;
    reconcile_users(&mut writer, &remaining).await.unwrap();
    assert_eq!(changes(&mut writer).await, before);
    assert_eq!(users(&mut writer).await, removed);
    reconcile_users(&mut writer, &initial).await.unwrap();
    assert_eq!(users(&mut writer).await, original);
    let still_revoked: Option<i64> =
        sqlx::query_scalar("SELECT revoked_at FROM api_token WHERE name = 'cli' AND user_id = ?")
            .bind(original[0].0)
            .fetch_one(&mut writer)
            .await
            .unwrap();
    assert_eq!(still_revoked, removed[0].7);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM session WHERE user_id = ?")
        .bind(original[0].0)
        .fetch_one(&mut writer)
        .await
        .unwrap();
    assert_eq!(count, 0);
    readers.close().await;
    writer.close().await.unwrap();
}

#[tokio::test]
async fn reconciliation_failure_rolls_back_add_change_disable_and_session_revocation() {
    let dir = TempDir::new();
    let (mut writer, readers) = open(&dir.database()).await.unwrap();
    reconcile_users(&mut writer, &declared("[users.alice]\n[users.bob]"))
        .await
        .unwrap();
    credentials(&mut writer).await;
    let original = users(&mut writer).await;
    sqlx::raw_sql("CREATE TRIGGER refuse_revocation BEFORE UPDATE OF revoked_at ON api_token BEGIN SELECT RAISE(ABORT, 'revocation failed'); END;")
        .execute(&mut writer).await.unwrap();
    let err = reconcile_users(
        &mut writer,
        &declared("[users.bob]\nis_admin=true\n[users.charlie]"),
    )
    .await
    .unwrap_err();
    assert!(err.contains("revocation failed"), "{err}");
    assert_eq!(users(&mut writer).await, original);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM session")
        .fetch_one(&mut writer)
        .await
        .unwrap();
    assert_eq!(count, 3);
    let revoked: Vec<Option<i64>> =
        sqlx::query_scalar("SELECT revoked_at FROM api_token ORDER BY id")
            .fetch_all(&mut writer)
            .await
            .unwrap();
    assert_eq!(revoked, [None, None, None, None, Some(42)]);
    readers.close().await;
    writer.close().await.unwrap();
}
