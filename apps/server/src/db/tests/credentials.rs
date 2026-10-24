use super::{
    users::{changes, declared},
    *,
};
use crate::db::{credentials::reconcile_credentials, users::reconcile_users};

const AUTH: &str =
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAINdamAGCsQq31Uv+08lkBzoO4XLz2qYjJa8CGmj3B1Ea";
const SIGNING: &str =
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAID1AF8PoQ4lakrcKp00bfrycmCzPLsSWjMDNVfEq9GYM";

type KeyRow = (
    String,
    String,
    String,
    String,
    String,
    String,
    i64,
    Option<i64>,
);
type TokenRow = (
    String,
    String,
    Vec<u8>,
    String,
    String,
    i64,
    Option<i64>,
    Option<i64>,
    Option<i64>,
);

async fn keys(writer: &mut SqliteConnection) -> Vec<KeyRow> {
    sqlx::query_as("SELECT public_id, usage, key_type, public_key, fingerprint, source, created_at, last_used_at FROM ssh_key ORDER BY public_id")
        .fetch_all(writer).await.unwrap()
}

async fn tokens(writer: &mut SqliteConnection) -> Vec<TokenRow> {
    sqlx::query_as("SELECT public_id, name, token_hash, scopes, source, created_at, expires_at, last_used_at, revoked_at FROM api_token ORDER BY public_id")
        .fetch_all(writer).await.unwrap()
}

fn config() -> std::collections::BTreeMap<String, kajiba_core::config::UserConfig> {
    declared(&format!(
        "[users.alice]\nssh_keys=['{AUTH} user@example.org']\nsigning_keys=['{SIGNING}']\n[users.alice.tokens.ci]\nsha256='{}'\nscopes=['repo:read','ci:trigger']\nexpires='2028-02-29T01:02:03.456789+01:00'",
        "ab".repeat(32)
    ))
}

#[tokio::test]
async fn adds_keys_and_tokens_with_schema_encodings_and_noop_performs_no_write() {
    let dir = TempDir::new();
    let (mut writer, readers) = open(&dir.database()).await.unwrap();
    let config = config();
    reconcile_users(&mut writer, &config).await.unwrap();
    reconcile_credentials(&mut writer, &config).await.unwrap();
    let key_rows = keys(&mut writer).await;
    assert_eq!(key_rows.len(), 2);
    for row in &key_rows {
        assert_eq!(row.0.len(), 26);
        assert_eq!(row.2, "ssh-ed25519");
        assert_eq!(row.5, "config");
        assert!(row.6 > 0);
        assert_eq!(row.7, None);
        // The declared comment is dropped: only the canonical key is stored.
        assert_eq!(row.3, if row.1 == "auth" { AUTH } else { SIGNING });
        let expected = if row.1 == "auth" {
            "SHA256:bbXpuKG6zhzdmnxq256TlqzFBzRl2f6OOg722cYNbU8"
        } else {
            "SHA256:F34nin7tcaYH6WR5LSWSfj6weFBPfBpuyUUoPFP9YjA"
        };
        assert_eq!(row.4, expected);
        assert!(row.4.starts_with("SHA256:"));
    }
    assert_ne!(key_rows[0].0, key_rows[1].0);
    let token_rows = tokens(&mut writer).await;
    assert_eq!(token_rows.len(), 1);
    let token = &token_rows[0];
    assert_eq!(token.0.len(), 26);
    assert_eq!(token.1, "ci");
    assert_eq!(token.2, vec![0xab; 32]);
    assert_eq!(token.3, "repo:read ci:trigger");
    assert_eq!(token.4, "config");
    assert!(token.5 > 0);
    assert_eq!(token.6, Some(1_835_395_323_456));
    assert_eq!((token.7, token.8), (None, None));
    let before = changes(&mut writer).await;
    reconcile_credentials(&mut writer, &config).await.unwrap();
    assert_eq!(changes(&mut writer).await, before);
    assert_eq!(keys(&mut writer).await, key_rows);
    assert_eq!(tokens(&mut writer).await, token_rows);
    readers.close().await;
    writer.close().await.unwrap();
}

#[tokio::test]
async fn changes_scopes_expiry_and_key_comment_without_replacing_identity() {
    let dir = TempDir::new();
    let (mut writer, readers) = open(&dir.database()).await.unwrap();
    let mut config = config();
    reconcile_users(&mut writer, &config).await.unwrap();
    reconcile_credentials(&mut writer, &config).await.unwrap();
    sqlx::query("UPDATE ssh_key SET last_used_at = 42; UPDATE api_token SET last_used_at = 43")
        .execute(&mut writer)
        .await
        .unwrap();
    let expected_keys = keys(&mut writer).await;
    let mut expected_tokens = tokens(&mut writer).await;
    let alice = config.get_mut("alice").unwrap();
    alice.ssh_keys[0] = format!("{AUTH} changed@example.org");
    let token = alice.tokens.get_mut("ci").unwrap();
    token.scopes = vec![
        kajiba_core::config::Scope::RepoWrite,
        kajiba_core::config::Scope::Admin,
    ];
    token.expires = None;
    reconcile_credentials(&mut writer, &config).await.unwrap();
    expected_tokens[0].3 = "repo:write admin".into();
    expected_tokens[0].6 = None;
    assert_eq!(keys(&mut writer).await, expected_keys);
    assert_eq!(tokens(&mut writer).await, expected_tokens);
    let before = changes(&mut writer).await;
    reconcile_credentials(&mut writer, &config).await.unwrap();
    assert_eq!(changes(&mut writer).await, before);
    readers.close().await;
    writer.close().await.unwrap();
}

#[tokio::test]
async fn removes_declared_items_including_absent_users_and_preserves_runtime_rows() {
    let dir = TempDir::new();
    let (mut writer, readers) = open(&dir.database()).await.unwrap();
    let config = config();
    reconcile_users(&mut writer, &config).await.unwrap();
    reconcile_credentials(&mut writer, &config).await.unwrap();
    sqlx::query("UPDATE ssh_key SET source = 'runtime', last_used_at = 42 WHERE usage = 'signing';
        INSERT INTO api_token (public_id, user_id, name, token_hash, scopes, source, created_at, last_used_at, revoked_at)
        SELECT '00000000000000000000000099', id, 'runtime', X'01', 'admin', 'runtime', 11, 12, 13 FROM user")
        .execute(&mut writer).await.unwrap();
    let expected_keys: Vec<_> = keys(&mut writer)
        .await
        .into_iter()
        .filter(|row| row.5 == "runtime")
        .collect();
    let expected_tokens: Vec<_> = tokens(&mut writer)
        .await
        .into_iter()
        .filter(|row| row.4 == "runtime")
        .collect();
    // Keep the user first, then remove it: both paths delete only declared credentials.
    for remaining in [declared("[users.alice]"), declared("")] {
        reconcile_credentials(&mut writer, &remaining)
            .await
            .unwrap();
        assert_eq!(keys(&mut writer).await, expected_keys);
        assert_eq!(tokens(&mut writer).await, expected_tokens);
        let before = changes(&mut writer).await;
        reconcile_credentials(&mut writer, &remaining)
            .await
            .unwrap();
        assert_eq!(changes(&mut writer).await, before);
    }
    // Re-add declared rows, then remove the whole user directly.
    reconcile_credentials(&mut writer, &config)
        .await
        .unwrap_err(); // runtime signing key collision
    sqlx::query("DELETE FROM ssh_key WHERE source = 'runtime'")
        .execute(&mut writer)
        .await
        .unwrap();
    reconcile_credentials(&mut writer, &config).await.unwrap();
    reconcile_credentials(&mut writer, &declared(""))
        .await
        .unwrap();
    assert!(keys(&mut writer).await.is_empty());
    assert_eq!(tokens(&mut writer).await, expected_tokens);
    readers.close().await;
    writer.close().await.unwrap();
}

#[tokio::test]
async fn uniqueness_refusals_roll_back_deletions_and_insertions() {
    let dir = TempDir::new();
    let (mut writer, readers) = open(&dir.database()).await.unwrap();
    let initial = config();
    reconcile_users(&mut writer, &declared("[users.alice]\n[users.bob]"))
        .await
        .unwrap();
    reconcile_credentials(&mut writer, &initial).await.unwrap();
    let original_keys = keys(&mut writer).await;
    let original_tokens = tokens(&mut writer).await;
    // Configuration validation refuses both; the schema must still hold without it.
    use kajiba_core::config::{TokenConfig, UserConfig};
    let key_user = || UserConfig {
        ssh_keys: vec![SIGNING.into()],
        ..Default::default()
    };
    let token_user = |name: &str| UserConfig {
        tokens: [(
            name.to_owned(),
            TokenConfig {
                sha256: "12".repeat(32),
                scopes: Vec::new(),
                expires: None,
            },
        )]
        .into(),
        ..Default::default()
    };
    let bad_configs: [std::collections::BTreeMap<String, UserConfig>; 2] = [
        [
            ("alice".to_owned(), key_user()),
            ("bob".to_owned(), key_user()),
        ]
        .into(),
        [
            ("alice".to_owned(), token_user("one")),
            ("bob".to_owned(), token_user("two")),
        ]
        .into(),
    ];
    for bad in bad_configs {
        let err = reconcile_credentials(&mut writer, &bad).await.unwrap_err();
        assert!(err.contains("UNIQUE constraint failed"), "{err}");
        assert_eq!(keys(&mut writer).await, original_keys);
        assert_eq!(tokens(&mut writer).await, original_tokens);
    }
    readers.close().await;
    writer.close().await.unwrap();
}

#[tokio::test]
async fn permits_shared_fingerprint_across_usages_and_moves_config_keys_between_users() {
    let dir = TempDir::new();
    let (mut writer, readers) = open(&dir.database()).await.unwrap();
    let initial = declared(&format!(
        "[users.alice]\nssh_keys=['{AUTH}']\n[users.bob]\nsigning_keys=['{AUTH}']"
    ));
    reconcile_users(&mut writer, &initial).await.unwrap();
    reconcile_credentials(&mut writer, &initial).await.unwrap();
    let moved = declared(&format!(
        "[users.alice]\nsigning_keys=['{AUTH}']\n[users.bob]\nssh_keys=['{AUTH}']"
    ));
    reconcile_credentials(&mut writer, &moved).await.unwrap();
    let owners: Vec<(String, String)> = sqlx::query_as(
        "SELECT user.name, usage FROM ssh_key JOIN user ON user.id = user_id ORDER BY user.name",
    )
    .fetch_all(&mut writer)
    .await
    .unwrap();
    assert_eq!(
        owners,
        [
            ("alice".into(), "signing".into()),
            ("bob".into(), "auth".into())
        ]
    );
    readers.close().await;
    writer.close().await.unwrap();
}

#[tokio::test]
async fn runtime_collisions_are_refused_without_adopting_or_modifying_rows() {
    let dir = TempDir::new();
    let (mut writer, readers) = open(&dir.database()).await.unwrap();
    let config = config();
    reconcile_users(&mut writer, &config).await.unwrap();
    reconcile_credentials(&mut writer, &config).await.unwrap();
    sqlx::query("UPDATE ssh_key SET source = 'runtime'; UPDATE api_token SET source = 'runtime', revoked_at = 42")
        .execute(&mut writer).await.unwrap();
    let original_keys = keys(&mut writer).await;
    let original_tokens = tokens(&mut writer).await;
    // Exercise each unique boundary independently, including token name and hash.
    for input in [
        format!("[users.alice]\nssh_keys=['{AUTH}']"),
        format!(
            "[users.alice.tokens.ci]\nsha256='{}'\nscopes=['admin']",
            "00".repeat(32)
        ),
        format!(
            "[users.alice.tokens.other]\nsha256='{}'\nscopes=['admin']",
            "ab".repeat(32)
        ),
    ] {
        let err = reconcile_credentials(&mut writer, &declared(&input))
            .await
            .unwrap_err();
        assert!(err.contains("UNIQUE"), "{err}");
        assert_eq!(keys(&mut writer).await, original_keys);
        assert_eq!(tokens(&mut writer).await, original_tokens);
    }
    assert_eq!(keys(&mut writer).await, original_keys);
    assert_eq!(tokens(&mut writer).await, original_tokens);
    readers.close().await;
    writer.close().await.unwrap();
}

#[tokio::test]
async fn rotates_hashes_and_replaces_key_material_then_reruns_without_writes() {
    let dir = TempDir::new();
    let (mut writer, readers) = open(&dir.database()).await.unwrap();
    let mut config = config();
    reconcile_users(&mut writer, &config).await.unwrap();
    reconcile_credentials(&mut writer, &config).await.unwrap();
    let alice = config.get_mut("alice").unwrap();
    alice.ssh_keys = vec![SIGNING.into()];
    alice.signing_keys.clear();
    alice.tokens.get_mut("ci").unwrap().sha256 = "12".repeat(32);
    reconcile_credentials(&mut writer, &config).await.unwrap();
    let rows = keys(&mut writer).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].1, "auth");
    assert_eq!(rows[0].3, SIGNING);
    assert_eq!(tokens(&mut writer).await[0].2, vec![0x12; 32]);
    let before = changes(&mut writer).await;
    reconcile_credentials(&mut writer, &config).await.unwrap();
    assert_eq!(changes(&mut writer).await, before);
    readers.close().await;
    writer.close().await.unwrap();
}

#[tokio::test]
async fn users_and_credentials_reconcile_in_one_transaction() {
    let dir = TempDir::new();
    let (mut writer, readers) = open(&dir.database()).await.unwrap();
    let mut config = config();
    crate::db::reconcile(&mut writer, &config).await.unwrap();
    // A runtime key now collides with alice's declared key, after bob would be added.
    sqlx::query("UPDATE ssh_key SET source = 'runtime' WHERE usage = 'auth'")
        .execute(&mut writer)
        .await
        .unwrap();
    config.insert("bob".into(), Default::default());
    let err = crate::db::reconcile(&mut writer, &config)
        .await
        .unwrap_err();
    assert!(err.contains("UNIQUE"), "{err}");
    let names: Vec<String> = sqlx::query_scalar("SELECT name FROM user ORDER BY name")
        .fetch_all(&mut writer)
        .await
        .unwrap();
    assert_eq!(names, ["alice"]);
    readers.close().await;
    writer.close().await.unwrap();
}
