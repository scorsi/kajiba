use super::*;
use crate::restore::validate;

// Include the manifest, modes, directories and link targets: every refusal is read-only.
fn contents(root: &Path) -> BTreeMap<PathBuf, (u32, Vec<u8>)> {
    fn walk(root: &Path, relative: &Path, result: &mut BTreeMap<PathBuf, (u32, Vec<u8>)>) {
        let path = if relative.as_os_str().is_empty() {
            root.to_path_buf()
        } else {
            root.join(relative)
        };
        let metadata = fs::symlink_metadata(&path).unwrap();
        let bytes = if metadata.is_symlink() {
            fs::read_link(&path)
                .unwrap()
                .as_os_str()
                .as_bytes()
                .to_vec()
        } else if metadata.is_file() {
            fs::read(&path).unwrap()
        } else {
            Vec::new()
        };
        result.insert(relative.into(), (metadata.permissions().mode(), bytes));
        if metadata.is_dir() {
            for child in fs::read_dir(path).unwrap() {
                walk(root, &relative.join(child.unwrap().file_name()), result);
            }
        }
    }
    let mut result = BTreeMap::new();
    walk(root, Path::new(""), &mut result);
    result
}

fn manifest(root: &Path) -> Manifest {
    serde_json::from_slice(&fs::read(root.join("manifest.json")).unwrap()).unwrap()
}
fn save(root: &Path, manifest: &Manifest) {
    fs::write(
        root.join("manifest.json"),
        serde_json::to_vec(manifest).unwrap(),
    )
    .unwrap();
}
fn rehash(root: &Path) {
    let mut manifest = manifest(root);
    manifest.entries.clear();
    inventory(root, Path::new(""), &mut manifest.entries, false).unwrap();
    save(root, &manifest);
}
async fn refused_unchanged(root: &Path, reason: &str) {
    let before = contents(root);
    let error = validate(root).await.unwrap_err();
    assert!(error.contains(reason), "{error}: expected {reason}");
    assert_eq!(contents(root), before);
}

#[tokio::test]
async fn valid_payload_preserves_archives_and_quarantine_reservations() {
    let mut f = Fixture::new().await;
    let dump = f.dump().await.unwrap();
    let before = contents(&dump);
    let validated = validate(&dump).await.unwrap();
    assert_eq!(validated.repositories, [LIVE, ARCHIVED]);
    for path in [
        "git/tmp/orphans/01ARZ3NDEKTSV4RRFFQ69G5FAX.git.2",
        "git/tmp/orphans/01ARZ3NDEKTSV4RRFFQ69G5FAX.git.lock",
        "server/tmp/orphans/preserved",
    ] {
        assert!(validated.entries.contains_key(&encoded(Path::new(path))));
    }
    assert_eq!(contents(&dump), before);
    f.close().await;
}

#[tokio::test]
async fn unsupported_format_and_schema_are_refused_without_migration() {
    let mut f = Fixture::new().await;
    let dump = f.dump().await.unwrap();
    let mut m = manifest(&dump);
    m.format += 1;
    save(&dump, &m);
    refused_unchanged(&dump, "unsupported dump format").await;
    m.format = 1;
    save(&dump, &m);
    let database = dump.join("server/db/kajiba.sqlite");
    let mut conn = SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&database))
        .await
        .unwrap();
    sqlx::query("INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) VALUES (999999, 'future', 1, X'00', 0)")
        .execute(&mut conn).await.unwrap();
    conn.close().await.unwrap();
    rehash(&dump);
    refused_unchanged(&dump, "unsupported or incomplete dump schema").await;
    f.close().await;
}

#[tokio::test]
async fn truncated_database_and_foreign_key_violations_are_refused() {
    for truncate in [true, false] {
        let mut f = Fixture::new().await;
        let dump = f.dump().await.unwrap();
        let database = dump.join("server/db/kajiba.sqlite");
        if truncate {
            File::options()
                .write(true)
                .open(&database)
                .unwrap()
                .set_len(100)
                .unwrap();
        } else {
            let mut conn = SqliteConnection::connect_with(
                &SqliteConnectOptions::new()
                    .filename(&database)
                    .foreign_keys(false),
            )
            .await
            .unwrap();
            sqlx::query("UPDATE repo SET owner_id = 999999")
                .execute(&mut conn)
                .await
                .unwrap();
            conn.close().await.unwrap();
        }
        rehash(&dump);
        refused_unchanged(&dump, if truncate { "database" } else { "foreign keys" }).await;
        f.close().await;
    }
}

#[tokio::test]
async fn missing_repository_and_altered_content_are_refused() {
    let mut f = Fixture::new().await;
    let dump = f.dump().await.unwrap();
    let head = dump.join(format!("git/repos/{LIVE}.git/HEAD"));
    let original = fs::read(&head).unwrap();
    fs::write(&head, "altered").unwrap();
    refused_unchanged(&dump, "inventory/content").await;
    fs::write(head, original).unwrap();
    fs::remove_dir_all(dump.join(format!("git/repos/{ARCHIVED}.git"))).unwrap();
    rehash(&dump);
    refused_unchanged(&dump, "repository inventory").await;
    f.close().await;
}

#[tokio::test]
async fn traversal_and_unexpected_durable_roots_are_refused() {
    let mut f = Fixture::new().await;
    let dump = f.dump().await.unwrap();
    for path in [
        "../escape",
        "/absolute",
        "server/../escape",
        "server/./db",
        "server//db",
        "server/db/",
    ] {
        let mut m = manifest(&dump);
        m.entries.insert(encoded(Path::new(path)), Entry::Directory);
        save(&dump, &m);
        refused_unchanged(&dump, "unsafe dump path").await;
        m.entries.remove(&encoded(Path::new(path)));
        save(&dump, &m);
    }
    fs::create_dir_all(dump.join("server/ci/bundles")).unwrap();
    fs::write(dump.join("server/ci/bundles/extra"), "excluded").unwrap();
    rehash(&dump);
    refused_unchanged(&dump, "unexpected dump payload path").await;
    f.close().await;
}

#[tokio::test]
async fn symlink_escapes_in_manifest_database_and_quarantine_are_refused() {
    let mut f = Fixture::new().await;
    let dump = f.dump().await.unwrap();
    for relative in ["manifest.json", "server/db", "git/tmp/orphans"] {
        let path = dump.join(relative);
        let saved = f.root.join("saved");
        fs::rename(&path, &saved).unwrap();
        symlink(&saved, &path).unwrap();
        let outside = contents(&saved);
        refused_unchanged(
            &dump,
            if relative == "manifest.json" {
                "regular file"
            } else {
                "unsafe dump entry"
            },
        )
        .await;
        assert_eq!(contents(&saved), outside);
        fs::remove_file(&path).unwrap();
        fs::rename(&saved, path).unwrap();
    }
    f.close().await;
}
