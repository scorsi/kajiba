use super::*;
use std::{
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};
use tonic::Code;
use tonic_types::StatusExt;

/// A started state directory whose repositories are reconciled from `repos.<key>` tables.
pub(in crate::transport) struct Fixture {
    daemon: crate::transport::git_side::fake::Fake,
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
        let lock = crate::state::prepare(&dir.join("server")).unwrap();
        let (writer, readers) = crate::db::open(&dir.join("server/db/kajiba.sqlite"))
            .await
            .unwrap();
        let health = Health::new(dir.join("server"), dir.clone(), None);
        health.started(readers);
        let daemon = crate::transport::git_side::fake::Fake::start(&dir).await;
        let mut fixture = Self {
            daemon,
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
        config.state_dir = self.dir.join("server");
        config.git.state_dir = self.dir.clone();
        crate::db::reconcile_declared(&mut self.writer, &config, &mut self.daemon.client)
            .await
            .unwrap();
    }
}

impl Fixture {
    /// The git directory of a declared `<owner>/<name>`.
    pub(in crate::transport) async fn bare(&self, repo: &str) -> PathBuf {
        let (owner, name) = repo.split_once('/').unwrap();
        let found = crate::db::repos::find(self.repos.readers().unwrap(), owner, name)
            .await
            .unwrap()
            .unwrap();
        self.dir.join(format!("repos/{}.git", found.public_id))
    }

    /// Fills a declared repository: `files` committed on `main`, then each extra
    /// branch and tag on its own empty commit, each the child of the previous one
    /// (distinct ids: a fetch wants each), pushed with the git binary.
    pub(in crate::transport) async fn fill(
        &self,
        repo: &str,
        files: &[(&str, &str)],
        refs: &[&str],
    ) {
        let bare = self.bare(repo).await;
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

async fn commits(
    repos: &Repos,
    revision: &str,
    page_size: i32,
    page_token: String,
) -> Result<ListCommitsResponse, Status> {
    repos
        .list_commits(Request::new(ListCommitsRequest {
            repo: "alice/demo".into(),
            revision: revision.into(),
            page_size,
            page_token,
        }))
        .await
        .map(Response::into_inner)
}

async fn diff(repos: &Repos, revision: &str) -> Result<GetCommitDiffResponse, Status> {
    repos
        .get_commit_diff(Request::new(GetCommitDiffRequest {
            repo: "alice/demo".into(),
            revision: revision.into(),
        }))
        .await
        .map(Response::into_inner)
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
        let page = commits(repos, "v1", 2, token).await.unwrap();
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

    // The root commit against the empty tree; b1 adds nothing to main.
    let root = diff(repos, "main").await.unwrap();
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
    let b1 = diff(repos, "b1").await.unwrap();
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
    let status = diff(repos, "nope").await.unwrap_err();
    assert_eq!(reason(&status), "REVISION_NOT_FOUND");
}

#[tokio::test]
async fn forged_commit_cursors_are_bounded_and_history_ends_without_a_token() {
    let fixture = Fixture::new(DEMO).await;
    fixture
        .fill(
            "alice/demo",
            &[("flake.nix", "{ }\n")],
            &["refs/heads/b1", "refs/heads/b2", "refs/tags/v1"],
        )
        .await;
    let repos = &fixture.repos;
    let tip = commits(repos, "v1", 1, String::new())
        .await
        .unwrap()
        .commits
        .remove(0)
        .id;
    let page = |skip: usize| cursor::encode("v1.ListCommits", &tip, skip as i64);
    // The deepest issued cursor; beyond it the cursor was forged.
    let last = commits(repos, "", 2, page(MAX_COMMIT_SKIP)).await.unwrap();
    assert!(last.commits.is_empty() && last.next_page_token.is_empty());
    let status = commits(repos, "", 1, page(MAX_COMMIT_SKIP + 1))
        .await
        .unwrap_err();
    assert_eq!(
        (status.code(), reason(&status).as_str()),
        (Code::ResourceExhausted, "HISTORY_TOO_DEEP")
    );
    // Past the two remaining commits: no token even though the page is full.
    let rest = commits(repos, "", 2, page(2)).await.unwrap();
    assert_eq!(rest.commits.len(), 2);
    assert!(rest.next_page_token.is_empty());
}

#[tokio::test]
async fn browsing_shares_the_public_read_budget() {
    let fixture = Fixture::new(DEMO).await;
    fixture
        .fill("alice/demo", &[("flake.nix", "{ }\n")], &[])
        .await;
    let repos = &fixture.repos;
    let permits: Vec<_> = (0..super::super::PUBLIC_READS)
        .map(|_| repos.read_permit().unwrap())
        .collect();
    for status in [
        tree(repos, "alice/demo", "main", "").await.unwrap_err(),
        blob(repos, "alice/demo", "main", "flake.nix")
            .await
            .unwrap_err(),
        commits(repos, "main", 1, String::new()).await.unwrap_err(),
        diff(repos, "main").await.unwrap_err(),
    ] {
        assert_eq!(
            (status.code(), reason(&status).as_str()),
            (Code::Unavailable, "READS_BUSY")
        );
    }
    drop(permits);
    assert_eq!(
        tree(repos, "alice/demo", "main", "").await.unwrap(),
        ["flake.nix:Blob"]
    );
    assert_eq!(
        repos.0.public_reads.available_permits(),
        super::super::PUBLIC_READS
    );
}

#[tokio::test]
async fn browsing_responses_keep_their_permit_until_the_body_is_delivered() {
    use prost::Message;
    use tonic::codegen::Service;
    let fixture = Fixture::new(DEMO).await;
    fixture
        .fill("alice/demo", &[("flake.nix", "{ }\n")], &[])
        .await;
    let message = GetBlobRequest {
        repo: "alice/demo".into(),
        revision: "main".into(),
        path: "flake.nix".into(),
    }
    .encode_to_vec();
    let mut body = vec![0];
    body.extend((message.len() as u32).to_be_bytes());
    body.extend(message);
    let response = super::super::router(fixture.repos.0.clone(), false, Default::default())
        .unwrap()
        .call(
            axum::http::Request::post("/kajiba.v1.RepoService/GetBlob")
                .header("content-type", "application/grpc")
                .body(axum::body::Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    // Built but unread: the permit is still taken (ADR 0050).
    let budget = &fixture.repos.0.public_reads;
    assert_eq!(budget.available_permits(), super::super::PUBLIC_READS - 1);
    let encoded = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    let decoded = GetBlobResponse::decode(&encoded[5..]).unwrap();
    assert_eq!(decoded.content, b"{ }\n");
    // Delivered: every permit comes back once the sending task has released its own.
    let mut held = Vec::new();
    for _ in 0..super::super::PUBLIC_READS {
        held.push(
            tokio::time::timeout(Duration::from_secs(5), budget.clone().acquire_owned())
                .await
                .unwrap()
                .unwrap(),
        );
    }
}

#[tokio::test]
async fn oversized_trees_and_diffs_are_resource_exhausted() {
    let fixture = Fixture::new(DEMO).await;
    fixture
        .fill("alice/demo", &[("flake.nix", "{ }\n")], &[])
        .await;
    let bare = fixture.bare("alice/demo").await;
    let blob = git(&bare, &["rev-parse", "refs/heads/main:flake.nix"]);
    // Root commits of `count` files sharing one blob, on `refs/heads/<branch>`.
    for (branch, count) in [("at", MAX_TREE_ENTRIES), ("above", MAX_TREE_ENTRIES + 1)] {
        let mut mktree = std::process::Command::new("git")
            .current_dir(&bare)
            .args(["mktree"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        use std::io::Write;
        let mut stdin = mktree.stdin.take().unwrap();
        for i in 0..count {
            writeln!(stdin, "100644 blob {}\tf{i}", blob.trim()).unwrap();
        }
        drop(stdin);
        let tree = String::from_utf8(mktree.wait_with_output().unwrap().stdout).unwrap();
        let commit = git(&bare, &["commit-tree", tree.trim(), "-m", branch]);
        git(
            &bare,
            &["update-ref", &format!("refs/heads/{branch}"), commit.trim()],
        );
    }
    let repos = &fixture.repos;
    assert_eq!(
        tree(repos, "alice/demo", "at", "").await.unwrap().len(),
        MAX_TREE_ENTRIES
    );
    assert_eq!(
        diff(repos, "at").await.unwrap().changes.len(),
        MAX_DIFF_CHANGES
    );
    let status = tree(repos, "alice/demo", "above", "").await.unwrap_err();
    assert_eq!(
        (status.code(), reason(&status).as_str()),
        (Code::ResourceExhausted, "TREE_TOO_LARGE")
    );
    let status = diff(repos, "above").await.unwrap_err();
    assert_eq!(
        (status.code(), reason(&status).as_str()),
        (Code::ResourceExhausted, "DIFF_TOO_LARGE")
    );
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
    // Its id is no revision and no cursor tip: refused on the object header.
    let bare = fixture.bare("alice/demo").await;
    let blob_id = git(&bare, &["rev-parse", "main:above"]).trim().to_owned();
    for status in [
        tree(repos, "alice/demo", &blob_id, "").await.unwrap_err(),
        blob(repos, "alice/demo", &blob_id, "at").await.unwrap_err(),
        commits(repos, &blob_id, 1, String::new())
            .await
            .unwrap_err(),
        commits(repos, "", 1, cursor::encode("v1.ListCommits", &blob_id, 0))
            .await
            .unwrap_err(),
        diff(repos, &blob_id).await.unwrap_err(),
    ] {
        assert_eq!(
            (status.code(), reason(&status).as_str()),
            (Code::NotFound, "REVISION_NOT_FOUND")
        );
    }
}

#[tokio::test]
async fn commit_pages_end_at_their_byte_budget_and_oversized_commits_are_refused() {
    let fixture = Fixture::new(DEMO).await;
    fixture
        .fill("alice/demo", &[("flake.nix", "{ }\n")], &[])
        .await;
    let bare = fixture.bare("alice/demo").await;
    let tree_id = git(&bare, &["rev-parse", "main^{tree}"]).trim().to_owned();
    let message = |bytes: usize| {
        let path = bare.join("message");
        std::fs::write(&path, "m".repeat(bytes)).unwrap();
        path.to_str().unwrap().to_owned()
    };
    // Three commits of half a page each: a page of three ends after two.
    let mut parent = git(&bare, &["rev-parse", "main"]).trim().to_owned();
    for _ in 0..3 {
        let path = message(MAX_COMMIT_PAGE_BYTES / 2 + 1);
        parent = git(
            &bare,
            &["commit-tree", &tree_id, "-p", &parent, "-F", &path],
        )
        .trim()
        .to_owned();
    }
    git(&bare, &["update-ref", "refs/heads/pages", &parent]);
    let repos = &fixture.repos;
    let page = commits(repos, "pages", 3, String::new()).await.unwrap();
    assert_eq!(page.commits.len(), 2);
    assert!(!page.next_page_token.is_empty());
    // The third big commit, then main's small one.
    let rest = commits(repos, "", 3, page.next_page_token).await.unwrap();
    assert_eq!(rest.commits.len(), 2);
    assert!(rest.next_page_token.is_empty());

    let path = message(kajiba_gitstore::MAX_COMMIT_OBJECT_BYTES as usize + 1);
    let huge = git(&bare, &["commit-tree", &tree_id, "-F", &path])
        .trim()
        .to_owned();
    git(&bare, &["update-ref", "refs/heads/huge", &huge]);
    let child = git(
        &bare,
        &["commit-tree", &tree_id, "-p", &huge, "-m", "child"],
    )
    .trim()
    .to_owned();
    git(&bare, &["update-ref", "refs/heads/child", &child]);
    // The oversized ancestor is refused beyond the page and at a cursor boundary too
    // (ADR 0051), the page's look-ahead reaching it in both cases.
    for status in [
        tree(repos, "alice/demo", "huge", "").await.unwrap_err(),
        commits(repos, "huge", 1, String::new()).await.unwrap_err(),
        commits(repos, "child", 1, String::new()).await.unwrap_err(),
        commits(repos, "", 1, cursor::encode("v1.ListCommits", &child, 1))
            .await
            .unwrap_err(),
        diff(repos, "huge").await.unwrap_err(),
    ] {
        assert_eq!(
            (status.code(), reason(&status).as_str()),
            (Code::ResourceExhausted, "COMMIT_TOO_LARGE")
        );
    }
}

#[tokio::test]
async fn commit_pages_count_author_fields_and_end_with_an_empty_page() {
    let fixture = Fixture::new(DEMO).await;
    fixture
        .fill("alice/demo", &[("flake.nix", "{ }\n")], &[])
        .await;
    let bare = fixture.bare("alice/demo").await;
    let tree_id = git(&bare, &["rev-parse", "main^{tree}"]).trim().to_owned();
    // Two commits with tiny messages and a 600 KiB author name, written directly
    // (no command-line bound): the second takes the page past its budget.
    let raw_commit = |parent: Option<&str>| {
        let parent = parent.map(|p| format!("parent {p}\n")).unwrap_or_default();
        let author = "A".repeat(600 << 10);
        let content = format!(
            "tree {tree_id}\n{parent}author {author} <a@example.org> 1 +0000\ncommitter a <a@example.org> 1 +0000\n\nm\n"
        );
        let path = bare.join("raw-commit");
        std::fs::write(&path, content).unwrap();
        git(
            &bare,
            &["hash-object", "-t", "commit", "-w", path.to_str().unwrap()],
        )
        .trim()
        .to_owned()
    };
    let root = raw_commit(None);
    let child = raw_commit(Some(&root));
    git(&bare, &["update-ref", "refs/heads/authors", &child]);
    let repos = &fixture.repos;
    let page = commits(repos, "authors", 3, String::new()).await.unwrap();
    let ids: Vec<&str> = page.commits.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids, [child.as_str(), root.as_str()]);
    assert!(!page.next_page_token.is_empty());
    // Nothing skipped or repeated: the continuation is the end of history.
    let rest = commits(repos, "", 3, page.next_page_token).await.unwrap();
    assert!(rest.commits.is_empty() && rest.next_page_token.is_empty());
}
