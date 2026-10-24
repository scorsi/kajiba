use super::*;
use kajiba_git::Gix;
use std::{
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};
use tonic::Code;
use tonic_types::StatusExt;

/// A started state directory whose repositories are reconciled from `repos.<key>` tables.
pub(in crate::transport) struct Fixture {
    pub(in crate::transport) dir: PathBuf,
    _lock: crate::state::StateLock,
    writer: SqliteConnection,
    pub(in crate::transport) repos: Repos,
}

impl Fixture {
    pub(in crate::transport) async fn new(repos: &str) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "kajiba-repos-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let lock = crate::state::prepare(&dir).unwrap();
        let (writer, readers) = crate::db::open(&dir.join("db/kajiba.sqlite"))
            .await
            .unwrap();
        let health = Health::new(dir.clone());
        health.started(readers);
        let mut fixture = Self {
            dir,
            _lock: lock,
            writer,
            repos: Repos(health),
        };
        fixture.apply(repos).await;
        fixture
    }

    pub(in crate::transport) async fn apply(&mut self, repos: &str) {
        let mut config = kajiba_core::config::ServerConfig::parse(&format!(
            "[http]\npublic_url='https://example.org'\n[users.alice]\n[users.bob]\n\
             [mirror.credentials.github]\ntoken_file='/run/secrets/github'\n{repos}"
        ))
        .unwrap();
        config.state_dir = self.dir.clone();
        crate::db::reconcile_declared(&mut self.writer, &config, &Gix)
            .await
            .unwrap();
    }
}

impl Fixture {
    /// Fills a declared repository: `files` committed on `main`, then each extra
    /// branch and tag on that commit, pushed with the git binary.
    pub(in crate::transport) async fn fill(
        &self,
        repo: &str,
        files: &[(&str, &str)],
        refs: &[&str],
    ) {
        let (owner, name) = repo.split_once('/').unwrap();
        let found = crate::db::repos::find(self.repos.readers().unwrap(), owner, name)
            .await
            .unwrap()
            .unwrap();
        let bare = self.dir.join(format!("repos/{}.git", found.public_id));
        let work = self.dir.join("tmp/work");
        git(
            &self.dir,
            &["init", "-q", "-b", "main", work.to_str().unwrap()],
        );
        for (path, content) in files {
            let file = work.join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, content).unwrap();
        }
        git(&work, &["add", "-A"]);
        git(&work, &["commit", "-q", "-m", "init"]);
        for name in refs {
            git(&work, &["update-ref", name, "HEAD"]);
        }
        git(
            &work,
            &[
                "push",
                "-q",
                bare.to_str().unwrap(),
                "refs/heads/*:refs/heads/*",
                "refs/tags/*:refs/tags/*",
            ],
        );
        std::fs::remove_dir_all(work).unwrap();
    }
}

/// The git binary without user configuration and with fixed identity and dates.
pub(in crate::transport) fn git(dir: &std::path::Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "a")
        .env("GIT_AUTHOR_EMAIL", "a@example.org")
        .env("GIT_COMMITTER_NAME", "a")
        .env("GIT_COMMITTER_EMAIL", "a@example.org")
        .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

pub(in crate::transport) const DEMO: &str = "[repos.demo]\nowner='alice'\ndescription='Demo'\n\
     previous_names=['alice/old']\npermissions.bob='write'\n\
     mirrors=[{remote_url='https://example.org/demo.git', credential='github'}]\n";
const TOOLS: &str =
    "[repos.tools]\nowner='alice'\ndescription='Tools'\nprevious_names=['bob/tools']\n";
const ZETA: &str = "[repos.zeta]\nowner='bob'\ndescription=''\nci_enabled=true\n";

async fn get(repos: &Repos, repo: get_repo_request::Repo) -> Result<Repo, Status> {
    repos
        .get_repo(Request::new(GetRepoRequest { repo: Some(repo) }))
        .await
        .map(|response| response.into_inner().repo.unwrap())
}

async fn list(repos: &Repos, page_size: i32, page_token: String) -> ListReposResponse {
    repos
        .list_repos(Request::new(ListReposRequest {
            page_size,
            page_token,
        }))
        .await
        .unwrap()
        .into_inner()
}

fn names(repos: &[Repo]) -> Vec<String> {
    repos
        .iter()
        .map(|repo| format!("{}/{}", repo.owner, repo.name))
        .collect()
}

#[tokio::test]
async fn get_returns_declared_fields_by_id_name_and_redirect_archived_included() {
    let mut fixture = Fixture::new(&format!("{DEMO}{TOOLS}{ZETA}")).await;
    let repos = &fixture.repos;
    let demo = get(repos, get_repo_request::Repo::Name("alice/demo".into()))
        .await
        .unwrap();
    assert_eq!(
        demo,
        Repo {
            public_id: demo.public_id.clone(),
            owner: "alice".into(),
            name: "demo".into(),
            description: "Demo".into(),
            visibility: Visibility::Public.into(),
            default_branch: "main".into(),
            ci_enabled: false,
            previous_names: vec!["alice/old".into()],
            permissions: vec![Permission {
                user: "bob".into(),
                role: Role::Write.into(),
            }],
            mirrors: vec![Mirror {
                remote_url: "https://example.org/demo.git".into(),
                credential: "github".into(),
                enabled: true,
            }],
            archived: false,
        }
    );
    for repo in [
        get_repo_request::Repo::PublicId(demo.public_id.clone()),
        get_repo_request::Repo::Name("alice/old".into()),
        get_repo_request::Repo::Name("Alice/Demo".into()),
    ] {
        assert_eq!(get(repos, repo).await.unwrap(), demo);
    }

    // `tools` leaves the configuration: archived, found by id, name and redirect.
    let tools = get(repos, get_repo_request::Repo::Name("alice/tools".into()))
        .await
        .unwrap();
    fixture.apply(&format!("{DEMO}{ZETA}")).await;
    let repos = &fixture.repos;
    for repo in [
        get_repo_request::Repo::PublicId(tools.public_id.clone()),
        get_repo_request::Repo::Name("alice/tools".into()),
        get_repo_request::Repo::Name("bob/tools".into()),
    ] {
        let archived = get(repos, repo).await.unwrap();
        assert!(archived.archived);
        assert_eq!(archived.public_id, tools.public_id);
        assert_eq!(archived.previous_names, ["bob/tools"]);
    }

    for repo in [
        get_repo_request::Repo::Name("alice/missing".into()),
        get_repo_request::Repo::Name("alice".into()),
        get_repo_request::Repo::Name("alice/../demo".into()),
        get_repo_request::Repo::PublicId("01ARZ3NDEKTSV4RRFFQ69G5FAV".into()),
    ] {
        let status = get(repos, repo).await.unwrap_err();
        assert_eq!(status.code(), Code::NotFound);
        assert_eq!(
            status.get_error_details().error_info().unwrap().reason,
            "REPO_NOT_FOUND"
        );
    }
    let status = repos
        .get_repo(Request::new(GetRepoRequest { repo: None }))
        .await
        .unwrap_err();
    assert_eq!(status.code(), Code::InvalidArgument);
}

#[tokio::test]
async fn get_and_list_redact_unsafe_persisted_mirror_urls_including_archived_repos() {
    let mut fixture = Fixture::new(DEMO).await;
    for remote in [
        "https://user:PRIVATE-TOKEN@example.org/demo.git",
        "ssh://PRIVATE-TOKEN@example.org/demo.git",
        "https://example.org/demo.git?token=PRIVATE-TOKEN",
    ] {
        sqlx::query("UPDATE mirror SET remote_url = ?")
            .bind(remote)
            .execute(&mut fixture.writer)
            .await
            .unwrap();
        for repo in [
            get(
                &fixture.repos,
                get_repo_request::Repo::Name("alice/demo".into()),
            )
            .await
            .unwrap(),
            list(&fixture.repos, 0, String::new()).await.repos.remove(0),
        ] {
            assert_eq!(repo.mirrors.len(), 1);
            assert!(repo.mirrors[0].remote_url.is_empty());
            assert_eq!(repo.mirrors[0].credential, "github");
            assert_eq!(repo.permissions.len(), 1);
        }
    }
    fixture.apply("").await;
    for repo in [
        get(
            &fixture.repos,
            get_repo_request::Repo::Name("alice/old".into()),
        )
        .await
        .unwrap(),
        list(&fixture.repos, 0, String::new()).await.repos.remove(0),
    ] {
        assert!(repo.archived);
        assert_eq!(repo.mirrors.len(), 1);
        assert!(repo.mirrors[0].remote_url.is_empty());
    }
}

#[tokio::test]
async fn list_pages_by_name_stay_stable_under_inserts_and_mark_archived() {
    let mut fixture = Fixture::new(&format!("{DEMO}{TOOLS}{ZETA}")).await;
    fixture.apply(&format!("{DEMO}{ZETA}")).await;
    let all = list(&fixture.repos, 0, String::new()).await;
    assert_eq!(names(&all.repos), ["alice/demo", "alice/tools", "bob/zeta"]);
    assert!(all.next_page_token.is_empty());
    assert_eq!(
        all.repos.iter().map(|r| r.archived).collect::<Vec<_>>(),
        [false, true, false]
    );
    assert!(all.repos[2].ci_enabled);

    let first = list(&fixture.repos, 1, String::new()).await;
    assert_eq!(names(&first.repos), ["alice/demo"]);
    // A repository sorting before the cursor appears meanwhile: the next pages neither
    // repeat nor skip a row.
    fixture
        .apply(&format!(
            "{DEMO}{ZETA}[repos.aaa]\nowner='alice'\ndescription=''\n"
        ))
        .await;
    let mut seen = first.repos;
    let mut token = first.next_page_token;
    while !token.is_empty() {
        let page = list(&fixture.repos, 1, token).await;
        assert!(page.repos.len() <= 1);
        seen.extend(page.repos);
        token = page.next_page_token;
    }
    assert_eq!(names(&seen), ["alice/demo", "alice/tools", "bob/zeta"]);

    for (size, token) in [(1, "garbage".to_owned()), (-1, String::new())] {
        let status = fixture
            .repos
            .list_repos(Request::new(ListReposRequest {
                page_size: size,
                page_token: token,
            }))
            .await
            .unwrap_err();
        assert_eq!(status.code(), Code::InvalidArgument);
    }
    // A cursor of another list method is foreign.
    let status = fixture
        .repos
        .list_repos(Request::new(ListReposRequest {
            page_size: 1,
            page_token: cursor::encode("admin.ListRepos", "alice/demo", 1),
        }))
        .await
        .unwrap_err();
    assert_eq!(status.code(), Code::InvalidArgument);
}

#[test]
fn the_service_has_no_mutating_method() {
    let proto = include_str!("../../../../../proto/kajiba/v1/repo.proto");
    let methods: Vec<&str> = proto
        .lines()
        .filter_map(|line| line.trim().strip_prefix("rpc "))
        .map(|rpc| rpc.split('(').next().unwrap())
        .collect();
    assert_eq!(methods, ["GetRepo", "ListRepos"]);
}
