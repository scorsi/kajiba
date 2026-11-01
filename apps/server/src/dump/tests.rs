use super::*;
use std::os::unix::fs::{PermissionsExt, symlink};

mod restore;
mod retention;

const LIVE: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
const ARCHIVED: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAW";

// APFS refuses non-UTF-8 names; keep the byte-preservation fixture on Linux.
fn quarantine_name() -> OsString {
    if cfg!(target_os = "linux") {
        OsString::from_vec(b"legacy-\xff.git".to_vec())
    } else {
        "legacy.git".into()
    }
}

struct Fixture {
    root: PathBuf,
    server: PathBuf,
    git: PathBuf,
    dumps: PathBuf,
    writer: SqliteConnection,
    readers: sqlx::SqlitePool,
}
impl Fixture {
    async fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("kajiba-dump-{}", kajiba_core::new_id().unwrap()));
        let server = root.join("server");
        let git = root.join("git");
        let dumps = root.join("dumps");
        for dir in [
            "db",
            "tmp/orphans",
            "ci/bundles",
            "ci/logs",
            "ci/artifacts",
            "cache",
        ] {
            fs::create_dir_all(server.join(dir)).unwrap();
        }
        fs::create_dir_all(git.join("repos")).unwrap();
        fs::create_dir_all(git.join("tmp/orphans")).unwrap();
        fs::create_dir(&dumps).unwrap();
        let (mut writer, readers) = crate::db::open(&server.join("db/kajiba.sqlite"))
            .await
            .unwrap();
        sqlx::query("PRAGMA wal_autocheckpoint=0")
            .execute(&mut writer)
            .await
            .unwrap();
        sqlx::query("INSERT INTO user (public_id, name, kind, is_admin, source, created_at) VALUES (?, 'alice', 'local', 1, 'config', 0)")
            .bind(kajiba_core::new_id().unwrap()).execute(&mut writer).await.unwrap();
        for (id, name, archived) in [(LIVE, "live", None), (ARCHIVED, "old", Some(1_i64))] {
            sqlx::query("INSERT INTO repo (public_id, config_key, owner_id, name, description, visibility, default_branch, ci_mode, archived_at, created_at) VALUES (?, ?, 1, ?, '', 'public', 'main', 'off', ?, 0)")
                .bind(id).bind(name).bind(name).bind(archived).execute(&mut writer).await.unwrap();
            let repo = git.join(format!("repos/{id}.git"));
            run(Command::new("git")
                .args(["init", "--bare", "--initial-branch=main"])
                .arg(&repo));
        }
        let work = root.join("work");
        run(Command::new("git")
            .args(["init", "--initial-branch=main"])
            .arg(&work));
        fs::write(work.join("README"), "durable history\n").unwrap();
        run(Command::new("git")
            .arg("-C")
            .arg(&work)
            .args(["add", "README"]));
        run(Command::new("git").arg("-C").arg(&work).args([
            "-c",
            "user.name=Alice",
            "-c",
            "user.email=alice@example.org",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-m",
            "initial",
        ]));
        run(Command::new("git").arg("-C").arg(&work).args(["tag", "v1"]));
        for id in [LIVE, ARCHIVED] {
            run(Command::new("git")
                .arg("-C")
                .arg(&work)
                .arg("push")
                .arg(git.join(format!("repos/{id}.git")))
                .args(["main", "--tags"]));
        }
        for dir in ["ci/bundles", "ci/logs", "ci/artifacts", "cache", "tmp"] {
            fs::write(server.join(dir).join("excluded"), "ephemeral").unwrap();
        }
        fs::write(server.join("tmp/orphans/preserved"), "server quarantine").unwrap();
        fs::write(
            git.join("tmp/orphans/01ARZ3NDEKTSV4RRFFQ69G5FAX.git.2"),
            "reservation",
        )
        .unwrap();
        fs::write(
            git.join("tmp/orphans/01ARZ3NDEKTSV4RRFFQ69G5FAX.git.lock"),
            "reserved",
        )
        .unwrap();
        let raw = quarantine_name();
        fs::create_dir(git.join("tmp/orphans").join(raw)).unwrap();
        let repo = git.join(format!("repos/{LIVE}.git"));
        fs::create_dir_all(repo.join("refs/heads/objects")).unwrap();
        fs::copy(
            repo.join("refs/heads/main"),
            repo.join("refs/heads/objects/incoming-real"),
        )
        .unwrap();
        fs::write(repo.join("packed-refs.lock"), "lock").unwrap();
        fs::create_dir(repo.join("objects/incoming-stale")).unwrap();
        fs::write(repo.join("objects/incoming-stale/scratch"), "scratch").unwrap();
        Self {
            root,
            server,
            git,
            dumps,
            writer,
            readers,
        }
    }
    async fn dump(&mut self) -> Result<PathBuf> {
        create_payload(&mut self.writer, &self.server, &self.git, &self.dumps).await
    }
    async fn close(self) {
        self.readers.close().await;
        self.writer.close().await.unwrap();
        fs::remove_dir_all(self.root).unwrap();
    }
    fn no_dump(&self) {
        assert_eq!(fs::read_dir(&self.dumps).unwrap().count(), 0);
    }
}
fn run(cmd: &mut Command) {
    let output = cmd
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{cmd:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
async fn committed_wal_refs_head_archives_quarantines_and_exclusions() {
    let mut f = Fixture::new().await;
    assert!(
        fs::metadata(f.server.join("db/kajiba.sqlite-wal"))
            .unwrap()
            .len()
            > 32
    );
    let original_mode = fs::metadata(f.git.join(format!("repos/{LIVE}.git/HEAD")))
        .unwrap()
        .permissions()
        .mode();
    let dump = f.dump().await.unwrap();
    let manifest = verify(&dump).await.unwrap();
    assert_eq!(manifest.repositories, [LIVE, ARCHIVED]);
    assert_eq!(
        manifest.schema,
        crate::db::MIGRATOR.iter().last().unwrap().version
    );
    assert_eq!(manifest.version, env!("CARGO_PKG_VERSION"));
    assert!(manifest.created_at_ms > 0);
    assert_eq!(
        fs::metadata(&dump).unwrap().permissions().mode() & 0o777,
        0o700
    );
    for id in [LIVE, ARCHIVED] {
        for name in ["HEAD", "refs/heads/main", "refs/tags/v1"] {
            let relative = format!("repos/{id}.git/{name}");
            assert_eq!(
                fs::read(f.git.join(&relative)).unwrap(),
                fs::read(dump.join("git").join(&relative)).unwrap()
            );
        }
    }
    assert_eq!(
        fs::read(dump.join("git/tmp/orphans/01ARZ3NDEKTSV4RRFFQ69G5FAX.git.2")).unwrap(),
        b"reservation"
    );
    assert!(
        dump.join("git/tmp/orphans")
            .join(quarantine_name())
            .is_dir()
    );
    assert!(dump.join("server/tmp/orphans/preserved").is_file());
    assert!(
        dump.join("git/tmp/orphans/01ARZ3NDEKTSV4RRFFQ69G5FAX.git.lock")
            .is_file()
    );
    assert_eq!(
        fs::read(dump.join(format!(
            "git/repos/{LIVE}.git/refs/heads/objects/incoming-real"
        )))
        .unwrap(),
        fs::read(f.git.join(format!("repos/{LIVE}.git/refs/heads/main"))).unwrap()
    );
    assert!(!dump.join("server/ci").exists());
    assert!(!dump.join("server/cache").exists());
    assert!(!dump.join("server/tmp/excluded").exists());
    assert!(
        !dump
            .join(format!("git/repos/{LIVE}.git/packed-refs.lock"))
            .exists()
    );
    assert!(
        !dump
            .join(format!("git/repos/{LIVE}.git/objects/incoming-stale"))
            .exists()
    );
    assert_eq!(
        fs::metadata(f.git.join(format!("repos/{LIVE}.git/HEAD")))
            .unwrap()
            .permissions()
            .mode(),
        original_mode
    );
    let mut db = readonly(&dump.join("server/db/kajiba.sqlite"))
        .await
        .unwrap();
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM repo WHERE archived_at IS NOT NULL")
        .fetch_one(&mut db)
        .await
        .unwrap();
    assert_eq!(count, 1);
    db.close().await.unwrap();
    assert_eq!(fs::read_dir(dump.join("server/db")).unwrap().count(), 1);
    f.close().await;
}

#[tokio::test]
async fn altered_missing_and_extra_content_are_refused() {
    let mut f = Fixture::new().await;
    let dump = f.dump().await.unwrap();
    let head = dump.join(format!("git/repos/{LIVE}.git/HEAD"));
    let original = fs::read(&head).unwrap();
    fs::write(&head, "ref: refs/heads/missing\n").unwrap();
    assert!(verify(&dump).await.is_err());
    fs::write(&head, original).unwrap();
    fs::write(dump.join("unexpected"), "extra").unwrap();
    assert!(verify(&dump).await.is_err());
    fs::remove_file(dump.join("unexpected")).unwrap();
    fs::remove_dir_all(dump.join(format!("git/repos/{ARCHIVED}.git"))).unwrap();
    assert!(verify(&dump).await.is_err());
    f.close().await;
}

#[tokio::test]
async fn missing_or_corrupt_source_and_copy_failure_publish_nothing() {
    let mut f = Fixture::new().await;
    let path = f.git.join(format!("repos/{LIVE}.git"));
    fs::rename(&path, f.root.join("saved")).unwrap();
    assert!(f.dump().await.is_err());
    f.no_dump();
    fs::rename(f.root.join("saved"), &path).unwrap();
    // A symlink cannot be copied as ordinary durable content, even if readable.
    symlink("HEAD", path.join("copy-failure")).unwrap();
    assert!(f.dump().await.is_err());
    f.no_dump();
    fs::remove_file(path.join("copy-failure")).unwrap();
    fs::write(
        path.join("refs/heads/main"),
        format!("{}\n", "a".repeat(40)),
    )
    .unwrap();
    assert!(f.dump().await.is_err());
    f.no_dump();
    f.close().await;
}

#[tokio::test]
async fn checks_objects_not_just_manifest_hashes() {
    let mut f = Fixture::new().await;
    let dump = f.dump().await.unwrap();
    let objects = dump.join(format!("git/repos/{LIVE}.git/objects"));
    for entry in fs::read_dir(&objects).unwrap() {
        let entry = entry.unwrap();
        if entry.file_name().as_bytes().len() == 2 {
            fs::remove_dir_all(entry.path()).unwrap();
        }
    }
    let mut manifest: Manifest =
        serde_json::from_slice(&fs::read(dump.join("manifest.json")).unwrap()).unwrap();
    manifest.entries.clear();
    inventory(&dump, Path::new(""), &mut manifest.entries, false).unwrap();
    fs::write(
        dump.join("manifest.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    assert!(
        verify(&dump)
            .await
            .unwrap_err()
            .contains("invalid dump repository")
    );
    f.close().await;
}

#[test]
fn paths_reject_escape_and_keep_unix_bytes() {
    for path in ["../escape", "/absolute", "", "a/../b", "./a", "a\0b"] {
        assert!(decode_path(&encoded(Path::new(path))).is_err(), "{path}");
    }
    let raw = PathBuf::from(OsString::from_vec(b"git/tmp/orphans/\xff.git.1".to_vec()));
    assert_eq!(decode_path(&encoded(&raw)).unwrap(), raw);
}

#[tokio::test]
async fn sqlite_corruption_is_refused_even_with_matching_file_hash() {
    let mut f = Fixture::new().await;
    let dump = f.dump().await.unwrap();
    let database = dump.join("server/db/kajiba.sqlite");
    File::options()
        .write(true)
        .open(&database)
        .unwrap()
        .set_len(100)
        .unwrap();
    let mut manifest: Manifest =
        serde_json::from_slice(&fs::read(dump.join("manifest.json")).unwrap()).unwrap();
    manifest.entries.insert(
        encoded(Path::new("server/db/kajiba.sqlite")),
        hash(&database).unwrap(),
    );
    fs::write(
        dump.join("manifest.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    assert!(verify(&dump).await.is_err());
    assert_eq!(fs::metadata(database).unwrap().len(), 100);
    f.close().await;
}

#[test]
fn copy_io_failure_removes_only_its_staging_tree() {
    let root = std::env::temp_dir().join(format!(
        "kajiba-dump-copy-{}",
        kajiba_core::new_id().unwrap()
    ));
    fs::create_dir(&root).unwrap();
    fs::write(root.join("source"), "payload").unwrap();
    fs::write(root.join("previous-good-dump"), "keep").unwrap();
    let stage = root.join(".partial");
    fs::create_dir(&stage).unwrap();
    {
        let staging = Staging(stage.clone());
        // An existing directory at the file destination makes fs::copy fail.
        fs::create_dir(stage.join("file")).unwrap();
        assert!(
            copy_tree(
                &root.join("source"),
                &staging.0,
                Path::new("file"),
                &mut BTreeMap::new()
            )
            .is_err()
        );
    }
    assert!(!stage.exists());
    assert_eq!(fs::read(root.join("previous-good-dump")).unwrap(), b"keep");
    fs::remove_dir_all(root).unwrap();
}
