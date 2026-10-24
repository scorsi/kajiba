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
    /// branch and tag on its own empty commit, each the child of the previous one
    /// (distinct ids: a fetch wants each), pushed with the git binary.
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
        let mut parent = git(&work, &["rev-parse", "HEAD"]);
        for name in refs {
            parent = git(
                &work,
                &[
                    "commit-tree",
                    "HEAD^{tree}",
                    "-p",
                    parent.trim(),
                    "-m",
                    name,
                ],
            );
            git(&work, &["update-ref", name, parent.trim()]);
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
    assert_eq!(
        methods,
        [
            "GetRepo",
            "ListRepos",
            "GetTree",
            "GetBlob",
            "ListCommits",
            "GetCommitDiff"
        ]
    );
}

fn reason(status: &Status) -> String {
    status
        .get_error_details()
        .error_info()
        .unwrap()
        .reason
        .clone()
}

async fn tree(
    repos: &Repos,
    repo: &str,
    revision: &str,
    path: &str,
) -> Result<Vec<String>, Status> {
    let response = repos
        .get_tree(Request::new(GetTreeRequest {
            repo: repo.into(),
            revision: revision.into(),
            path: path.into(),
        }))
        .await?
        .into_inner();
    Ok(response
        .entries
        .into_iter()
        .map(|entry| format!("{}:{:?}", entry.name, entry.kind()))
        .collect())
}

async fn blob(repos: &Repos, repo: &str, revision: &str, path: &str) -> Result<Vec<u8>, Status> {
    repos
        .get_blob(Request::new(GetBlobRequest {
            repo: repo.into(),
            revision: revision.into(),
            path: path.into(),
        }))
        .await
        .map(|response| response.into_inner().content)
}

#[tokio::test]
async fn browses_tree_blob_log_and_diff_of_active_and_archived_repositories() {
    let mut fixture = Fixture::new(DEMO).await;
    fixture
        .fill(
            "alice/demo",
            &[("flake.nix", "{ }\n"), ("src/lib.rs", "")],
            &["refs/heads/b1", "refs/heads/b2", "refs/tags/v1"],
        )
        .await;
    let demo = get(
        &fixture.repos,
        get_repo_request::Repo::Name("alice/demo".into()),
    )
    .await
    .unwrap();
    // Archived: browsing goes on, by current name, previous name and public id.
    fixture.apply("").await;
    let repos = &fixture.repos;
    for repo in ["alice/demo", "alice/old", demo.public_id.as_str()] {
        assert_eq!(
            tree(repos, repo, "main", "").await.unwrap(),
            ["flake.nix:Blob", "src:Tree"]
        );
        assert_eq!(
            tree(repos, repo, "v1", "src").await.unwrap(),
            ["lib.rs:Blob"]
        );
        assert_eq!(
            blob(repos, repo, "b2", "flake.nix").await.unwrap(),
            b"{ }\n"
        );
    }

    // v1 -> b2 -> b1 -> main, newest first, two per page.
    let mut token = String::new();
    let mut messages = Vec::new();
    loop {
        let page = repos
            .list_commits(Request::new(ListCommitsRequest {
                repo: "alice/demo".into(),
                revision: "v1".into(),
                page_size: 2,
                page_token: token,
            }))
            .await
            .unwrap()
            .into_inner();
        assert!(page.commits.len() <= 2);
        messages.extend(page.commits.into_iter().map(|c| c.message));
        token = page.next_page_token;
        if token.is_empty() {
            break;
        }
    }
    assert_eq!(
        messages,
        [
            "refs/tags/v1\n",
            "refs/heads/b2\n",
            "refs/heads/b1\n",
            "init\n"
        ]
    );

    let diff = |revision: &str| {
        repos.get_commit_diff(Request::new(GetCommitDiffRequest {
            repo: "alice/demo".into(),
            revision: revision.into(),
        }))
    };
    // The root commit against the empty tree; b1 adds nothing to main.
    let root = diff("main").await.unwrap().into_inner();
    assert!(root.commit.unwrap().parent_ids.is_empty());
    assert_eq!(
        root.changes
            .iter()
            .map(|c| (c.path.as_str(), c.kind(), c.old_id.is_empty()))
            .collect::<Vec<_>>(),
        [
            ("flake.nix", ChangeKind::Added, true),
            ("src/lib.rs", ChangeKind::Added, true)
        ]
    );
    let b1 = diff("b1").await.unwrap().into_inner();
    assert_eq!(b1.commit.unwrap().parent_ids.len(), 1);
    assert!(b1.changes.is_empty());

    for (repo, revision, path, code, expected) in [
        (
            "alice/missing",
            "main",
            "",
            Code::NotFound,
            "REPO_NOT_FOUND",
        ),
        (
            "01ARZ3NDEKTSV4RRFFQ69G5FAV",
            "main",
            "",
            Code::NotFound,
            "REPO_NOT_FOUND",
        ),
        (
            "alice/demo",
            "nope",
            "",
            Code::NotFound,
            "REVISION_NOT_FOUND",
        ),
        ("alice/demo", "", "", Code::NotFound, "REVISION_NOT_FOUND"),
        (
            "alice/demo",
            "main/src",
            "",
            Code::NotFound,
            "REVISION_NOT_FOUND",
        ),
        (
            "alice/demo",
            "main",
            "missing",
            Code::NotFound,
            "PATH_NOT_FOUND",
        ),
        (
            "alice/demo",
            "main",
            "flake.nix",
            Code::NotFound,
            "PATH_NOT_FOUND",
        ),
        (
            "alice/demo",
            "main",
            "../x",
            Code::InvalidArgument,
            "INVALID_FIELD",
        ),
    ] {
        let status = tree(repos, repo, revision, path).await.unwrap_err();
        assert_eq!(
            (status.code(), reason(&status).as_str()),
            (code, expected),
            "{repo} {revision} {path}"
        );
    }
    let status = blob(repos, "alice/demo", "main", "src").await.unwrap_err();
    assert_eq!(reason(&status), "PATH_NOT_FOUND");
    let status = diff("nope").await.unwrap_err();
    assert_eq!(reason(&status), "REVISION_NOT_FOUND");
}

#[tokio::test]
async fn a_blob_above_the_cap_is_resource_exhausted() {
    let fixture = Fixture::new(DEMO).await;
    let at_cap = "a".repeat(MAX_BLOB_BYTES);
    let above = "a".repeat(MAX_BLOB_BYTES + 1);
    fixture
        .fill("alice/demo", &[("at", &at_cap), ("above", &above)], &[])
        .await;
    let repos = &fixture.repos;
    assert_eq!(
        blob(repos, "alice/demo", "main", "at").await.unwrap().len(),
        MAX_BLOB_BYTES
    );
    let status = blob(repos, "alice/demo", "main", "above")
        .await
        .unwrap_err();
    assert_eq!(
        (status.code(), reason(&status).as_str()),
        (Code::ResourceExhausted, "BLOB_TOO_LARGE")
    );
}
