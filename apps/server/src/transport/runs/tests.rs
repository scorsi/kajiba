use super::*;
use crate::transport::{
    auth::tests::{CI, READ, reason, send, tokens},
    repos::tests::{Fixture, git},
    runners::Declared,
};
use kajiba_proto::v1::{Scope, create_worktree_run_request::Part};
use prost::Message;
use std::{sync::Arc, time::Duration};
use tonic::Code;

/// `alice` owns `ci` (CI on, `bob` writes) and `off` (CI off); `bob` owns `zeta`.
const REPOS: &str = "[repos.ci]\nowner='alice'\ndescription=''\nci_enabled=true\n\
     permissions.bob='write'\n[repos.off]\nowner='alice'\ndescription=''\n\
     [repos.zeta]\nowner='bob'\ndescription=''\nci_enabled=true\n";
const ALICE: i64 = 1;
const BOB: i64 = 2;

async fn fixture(extra: &str) -> (Fixture, Runs) {
    let fixture = Fixture::new(&format!("{REPOS}{extra}")).await;
    // One declared runner: x86_64-linux, nix, tagged `kvm` (offline: declared is enough).
    let runner = kajiba_core::config::ServerConfig::parse(
        "[http]\npublic_url='https://example.org'\n[runners.build-1]\ntoken_file='/run/secrets/b'\n\
         capabilities={ arch='x86_64', os='linux', tags=['kvm'] }\n",
    )
    .unwrap()
    .runners;
    fixture.repos.0.set_runners(Declared {
        runners: runner,
        bus: Arc::default(),
        wake: Arc::default(),
    });
    for repo in ["alice/ci", "alice/off", "bob/zeta"] {
        fixture.fill(repo, &[("flake.nix", "{}\n")], &[]).await;
    }
    let runs = Runs(fixture.repos.0.clone());
    (fixture, runs)
}

fn as_user<T>(user_id: i64, message: T) -> Request<T> {
    let mut request = Request::new(message);
    request
        .extensions_mut()
        .insert(crate::transport::auth::Principal {
            user_id,
            scopes: vec![Scope::RepoRead, Scope::CiTrigger],
            session: None,
        });
    request
}

async fn trigger(runs: &Runs, user: i64, repo: &str, sha: &str) -> Result<Run, Status> {
    runs.trigger_run(as_user(
        user,
        TriggerRunRequest {
            repo: repo.into(),
            sha: sha.into(),
        },
    ))
    .await
    .map(|response| response.into_inner().run.unwrap())
}

fn head(bare: &Path) -> String {
    git(bare, &["rev-parse", "refs/heads/main"])
        .trim()
        .to_owned()
}

/// A commit only `refs/contrib/bob/x` points to, and one no ref reaches.
fn contrib(bare: &Path) -> (String, String) {
    let tip = head(bare);
    let new = |message: &str| {
        git(
            bare,
            &[
                "commit-tree",
                &format!("{tip}^{{tree}}"),
                "-p",
                &tip,
                "-m",
                message,
            ],
        )
        .trim()
        .to_owned()
    };
    let (contributed, dangling) = (new("contrib"), new("dangling"));
    git(bare, &["update-ref", "refs/contrib/bob/x", &contributed]);
    (contributed, dangling)
}

fn refusal(status: Status) -> (Code, String) {
    (status.code(), reason(&status))
}

#[tokio::test]
async fn a_writer_triggers_a_manual_run_on_a_contribution_tip() {
    let (fixture, runs) = fixture("").await;
    let (contributed, _) = contrib(&fixture.bare("alice/ci").await);
    let run = trigger(&runs, BOB, "alice/ci", &contributed).await.unwrap();
    assert_eq!(
        (
            run.trigger(),
            run.state(),
            run.sha.as_str(),
            run.r#ref.as_str(),
            run.triggered_by.as_str()
        ),
        (
            RunTrigger::Manual,
            RunState::Queued,
            contributed.as_str(),
            "",
            "bob"
        )
    );
    let job = run.job.unwrap();
    assert_eq!(
        (job.name.as_str(), job.state()),
        ("flake-check", JobState::Pending)
    );
    assert!(job.current_attempt.is_none());
    let events: Vec<(String, Option<i64>)> =
        sqlx::query_as("SELECT kind, principal_id FROM event WHERE kind = 'run-created'")
            .fetch_all(runs.readers().unwrap())
            .await
            .unwrap();
    assert_eq!(events, [("run-created".to_owned(), Some(BOB))]);
}

#[tokio::test]
async fn trigger_refusals_carry_their_code_and_reason() {
    let (mut fixture, runs) = fixture("").await;
    let bare = fixture.bare("alice/ci").await;
    let (_, dangling) = contrib(&bare);
    let tip = head(&bare);
    let cases = [
        (
            ALICE,
            "bob/zeta",
            head(&fixture.bare("bob/zeta").await),
            (Code::PermissionDenied, "ROLE_MISSING"),
        ),
        (
            ALICE,
            "alice/off",
            head(&fixture.bare("alice/off").await),
            (Code::FailedPrecondition, "CI_DISABLED"),
        ),
        (
            BOB,
            "alice/ci",
            dangling,
            (Code::NotFound, "REVISION_NOT_FOUND"),
        ),
        (
            BOB,
            "alice/ci",
            "f".repeat(40),
            (Code::NotFound, "REVISION_NOT_FOUND"),
        ),
        (
            BOB,
            "alice/ci",
            "main".into(),
            (Code::NotFound, "REVISION_NOT_FOUND"),
        ),
        (
            BOB,
            "alice/nope",
            tip.clone(),
            (Code::NotFound, "REPO_NOT_FOUND"),
        ),
    ];
    for (user, repo, sha, expected) in cases {
        let status = trigger(&runs, user, repo, &sha).await.unwrap_err();
        assert_eq!(
            refusal(status),
            (expected.0, expected.1.into()),
            "{repo} {sha}"
        );
    }
    // Removed from the configuration: archived, read-only.
    fixture
        .apply("[repos.off]\nowner='alice'\ndescription=''\n")
        .await;
    let status = trigger(&runs, ALICE, "alice/ci", &tip).await.unwrap_err();
    assert_eq!(
        refusal(status),
        (Code::FailedPrecondition, "INVALID_STATE".into())
    );
    let created: i64 = sqlx::query_scalar("SELECT count(*) FROM run")
        .fetch_one(runs.readers().unwrap())
        .await
        .unwrap();
    assert_eq!(created, 0);
}

#[tokio::test]
async fn methods_require_their_scope() {
    let (fixture, _) = fixture(&tokens()).await;
    let mut app = super::super::router(
        fixture.repos.0.clone(),
        false,
        Default::default(),
        "https://example.org",
    )
    .unwrap();
    let tip = head(&fixture.bare("alice/ci").await);
    let (read, ci) = (format!("Bearer {READ}"), format!("Bearer {CI}"));
    let base = GetWorktreeBaseRequest {
        spec: Some(spec("alice/ci")),
    }
    .encode_to_vec();
    let trigger = TriggerRunRequest {
        repo: "alice/ci".into(),
        sha: tip,
    }
    .encode_to_vec();
    let list = ListRunsRequest {
        repo: "alice/ci".into(),
        ..Default::default()
    }
    .encode_to_vec();
    let path = |method: &str| format!("/kajiba.v1.RunService/{method}");
    for (method, token, message) in [
        ("TriggerRun", &read, trigger.clone()),
        ("GetWorktreeBase", &read, base.clone()),
        (
            "CreateWorktreeRun",
            &read,
            CreateWorktreeRunRequest {
                part: Some(Part::Spec(spec("alice/ci"))),
            }
            .encode_to_vec(),
        ),
        ("ListRuns", &ci, list.clone()),
        ("GetRun", &ci, GetRunRequest::default().encode_to_vec()),
    ] {
        let status = send(
            &mut app,
            &path(method),
            &[("authorization", token)],
            message,
        )
        .await
        .unwrap();
        assert_eq!(
            refusal(status),
            (Code::PermissionDenied, "SCOPE_MISSING".into()),
            "{method}"
        );
    }
    // bob's ci:trigger token and write role pass; alice's read token lists.
    assert!(
        send(
            &mut app,
            &path("GetWorktreeBase"),
            &[("authorization", &ci)],
            base
        )
        .await
        .is_none()
    );
    assert!(
        send(
            &mut app,
            &path("TriggerRun"),
            &[("authorization", &ci)],
            trigger
        )
        .await
        .is_none()
    );
    assert!(
        send(
            &mut app,
            &path("ListRuns"),
            &[("authorization", &read)],
            list
        )
        .await
        .is_none()
    );
}

fn spec(repo: &str) -> WorktreeRunSpec {
    WorktreeRunSpec {
        repo: repo.into(),
        system: "x86_64-linux".into(),
        checks: vec![],
        features: vec![],
    }
}

async fn base(runs: &Runs, user: i64, spec: WorktreeRunSpec) -> Result<Vec<String>, Status> {
    runs.get_worktree_base(as_user(user, GetWorktreeBaseRequest { spec: Some(spec) }))
        .await
        .map(|response| response.into_inner().ids)
}

#[tokio::test]
async fn worktree_base_lists_branch_and_tag_tips_only() {
    let (fixture, runs) =
        fixture("[repos.empty]\nowner='alice'\ndescription=''\nci_enabled=true\n").await;
    fixture
        .fill(
            "bob/zeta",
            &[("flake.nix", "{}\n")],
            &["refs/heads/dev", "refs/tags/v1"],
        )
        .await;
    let bare = fixture.bare("bob/zeta").await;
    git(
        &bare,
        &["tag", "-a", "-m", "annotated", "v2", "refs/heads/main"],
    );
    git(
        &bare,
        &["update-ref", "refs/contrib/alice/x", "refs/heads/dev"],
    );
    git(
        &bare,
        &["update-ref", "refs/kajiba/other", "refs/heads/dev~1"],
    );
    let mut expected: Vec<String> = [
        "refs/heads/main",
        "refs/heads/dev",
        "refs/tags/v1",
        "refs/tags/v2",
    ]
    .iter()
    .map(|name| git(&bare, &["rev-parse", name]).trim().to_owned())
    .collect();
    expected.sort();
    expected.dedup();
    assert_eq!(base(&runs, BOB, spec("bob/zeta")).await.unwrap(), expected);
    assert_eq!(
        base(&runs, ALICE, spec("alice/empty")).await.unwrap(),
        Vec::<String>::new()
    );
}

#[tokio::test]
async fn worktree_admission_refuses_in_order() {
    let (mut fixture, runs) = fixture("").await;
    let with = |repo: &str, edit: fn(&mut WorktreeRunSpec)| {
        let mut spec = spec(repo);
        edit(&mut spec);
        spec
    };
    let invalid = (Code::InvalidArgument, "INVALID_FIELD");
    let no_runner = (Code::FailedPrecondition, "NO_MATCHING_RUNNER");
    let cases = [
        (BOB, spec("alice/nope"), (Code::NotFound, "REPO_NOT_FOUND")),
        (
            ALICE,
            spec("bob/zeta"),
            (Code::PermissionDenied, "ROLE_MISSING"),
        ),
        // The role comes before CI and the names.
        (
            ALICE,
            with("bob/zeta", |s| s.system = "bad".into()),
            (Code::PermissionDenied, "ROLE_MISSING"),
        ),
        (
            ALICE,
            spec("alice/off"),
            (Code::FailedPrecondition, "CI_DISABLED"),
        ),
        (
            BOB,
            with("alice/ci", |s| s.checks = vec!["a".into(), "a b".into()]),
            invalid,
        ),
        (
            BOB,
            with("alice/ci", |s| s.checks = vec![String::new()]),
            invalid,
        ),
        (
            BOB,
            with("alice/ci", |s| s.system = "x86_64".into()),
            invalid,
        ),
        (
            BOB,
            with("alice/ci", |s| s.system = "X86_64-linux".into()),
            invalid,
        ),
        (
            BOB,
            with("alice/ci", |s| s.system = "-linux".into()),
            invalid,
        ),
        (
            BOB,
            with("alice/ci", |s| s.features = vec!["KVM".into()]),
            invalid,
        ),
        // Names before matching: a bad name for an absent system is still INVALID_FIELD.
        (
            BOB,
            with("alice/ci", |s| {
                s.system = "aarch64-linux".into();
                s.checks = vec!["a.b".into()]
            }),
            invalid,
        ),
        (
            BOB,
            with("alice/ci", |s| s.system = "aarch64-linux".into()),
            no_runner,
        ),
        (
            BOB,
            with("alice/ci", |s| s.system = "x86_64-darwin".into()),
            no_runner,
        ),
        (
            BOB,
            with("alice/ci", |s| {
                s.features = vec!["kvm".into(), "big-parallel".into()]
            }),
            no_runner,
        ),
    ];
    for (user, spec, expected) in cases {
        let status = base(&runs, user, spec.clone()).await.unwrap_err();
        assert_eq!(refusal(status), (expected.0, expected.1.into()), "{spec:?}");
    }
    let accepted = with("alice/ci", |s| {
        s.checks = vec!["vm-test_1".into()];
        s.features = vec!["kvm".into()];
    });
    assert_eq!(base(&runs, BOB, accepted.clone()).await.unwrap().len(), 1);

    // Sixteen active worktree runs of bob's, uploads included, are the limit; terminal
    // ones and others' don't count.
    let tip = head(&fixture.bare("alice/ci").await);
    let insert = async |index: usize, user: i64, trigger: &str, state: &str| {
        sqlx::query("INSERT INTO run (public_id, repo_id, sha, trigger, triggered_by, state, created_at) VALUES (?, 1, ?, ?, ?, ?, 0)")
            .bind(format!("{index:026}"))
            .bind(&tip)
            .bind(trigger)
            .bind(user)
            .bind(state)
            .execute(&mut *fixture.writer.lock().await)
            .await
            .unwrap();
    };
    insert(1, BOB, "worktree", "succeeded").await;
    insert(2, BOB, "manual", "queued").await;
    insert(3, ALICE, "worktree", "queued").await;
    for index in 0..WORKTREE_RUNS - 1 {
        let state = if index % 2 == 0 { "queued" } else { "running" };
        insert(10 + index, BOB, "worktree", state).await;
    }
    assert!(base(&runs, BOB, accepted.clone()).await.is_ok());
    let uploading = runs.0.uploads.take(BOB, 1).unwrap();
    let status = base(&runs, BOB, accepted.clone()).await.unwrap_err();
    assert_eq!(
        refusal(status),
        (Code::ResourceExhausted, "WORKTREE_RUNS_LIMIT".into())
    );
    drop(uploading);
    assert!(base(&runs, BOB, accepted.clone()).await.is_ok());
    insert(99, BOB, "worktree", "queued").await;
    let status = base(&runs, BOB, accepted.clone()).await.unwrap_err();
    assert_eq!(
        refusal(status),
        (Code::ResourceExhausted, "WORKTREE_RUNS_LIMIT".into())
    );
    // Archived: told before the role (ADR 0118), for a writer as for the owner.
    fixture
        .apply("[repos.off]\nowner='alice'\ndescription=''\n")
        .await;
    for user in [ALICE, BOB] {
        let status = base(&runs, user, spec("alice/ci")).await.unwrap_err();
        assert_eq!(
            refusal(status),
            (Code::FailedPrecondition, "INVALID_STATE".into())
        );
    }
}

const ATTEMPT: &str = "01J0000000000000000000000A";

/// Fail `run` the way a runner would: an attempt on a runner, an indexed log of 25
/// lines plus unindexed bytes past the index end, and the run's reason.
async fn fail(fixture: &Fixture, run: &str) {
    let mut writer = fixture.writer.lock().await;
    sqlx::query(
        "INSERT INTO runner (public_id, name, capabilities, version, connected, hostname)
         VALUES ('01J00000000000000000000001', 'build-1', '{}', '0.1.0', 1, 'vm-host.example.org')",
    )
    .execute(&mut *writer)
    .await
    .unwrap();
    let job: i64 = sqlx::query_scalar(
        "SELECT job.id FROM job JOIN run ON run.id = run_id WHERE run.public_id = ?",
    )
    .bind(run)
    .fetch_one(&mut *writer)
    .await
    .unwrap();
    let log: String = (1..=25).map(|n| format!("line {n}\n")).collect();
    std::fs::write(
        fixture.dir.join(format!("server/ci/logs/{ATTEMPT}.log")),
        format!("{log}overwritten later\n"),
    )
    .unwrap();
    let attempt = sqlx::query(
        "INSERT INTO job_attempt (public_id, job_id, number, runner_id, state, lease_expires_at,
           exit_code, started_at, finished_at)
         VALUES (?, ?, 1, 1, 'failed', 0, 1, 1000, 2500)",
    )
    .bind(ATTEMPT)
    .bind(job)
    .execute(&mut *writer)
    .await
    .unwrap()
    .last_insert_rowid();
    let half = log.len() / 2;
    for (seq, offset, length) in [(1, 0, half), (2, half, log.len() - half)] {
        sqlx::query(
            "INSERT INTO log_chunk (attempt_id, seq, step_idx, offset, length) VALUES (?, ?, 0, ?, ?)",
        )
        .bind(attempt)
        .bind(seq)
        .bind(offset as i64)
        .bind(length as i64)
        .execute(&mut *writer)
        .await
        .unwrap();
    }
    sqlx::query(
        "UPDATE run SET state = 'failed', failure_reason = 'exit status 1' WHERE public_id = ?",
    )
    .bind(run)
    .execute(&mut *writer)
    .await
    .unwrap();
    sqlx::query("UPDATE job SET state = 'failed' WHERE id = ?")
        .bind(job)
        .execute(&mut *writer)
        .await
        .unwrap();
}

#[tokio::test]
async fn a_failed_run_carries_its_attempt_and_summary() {
    let (fixture, runs) = fixture("").await;
    let tip = head(&fixture.bare("alice/ci").await);
    let run = trigger(&runs, ALICE, "alice/ci", &tip).await.unwrap();
    fail(&fixture, &run.public_id).await;
    // Any reader sees it; nobody sees an unknown run.
    let run = runs
        .get_run(as_user(BOB, GetRunRequest { run: run.public_id }))
        .await
        .unwrap()
        .into_inner()
        .run
        .unwrap();
    assert_eq!(run.state(), RunState::Failed);
    let attempt = run.job.unwrap().current_attempt.unwrap();
    assert_eq!(
        attempt,
        Attempt {
            public_id: ATTEMPT.into(),
            number: 1,
            state: AttemptState::Failed.into(),
            runner: "build-1".into(),
            runner_hostname: "vm-host.example.org".into(),
            exit_code: Some(1),
            started_at: time(Some(1000)),
            finished_at: time(Some(2500)),
        }
    );
    assert_eq!(
        run.failure.unwrap(),
        FailureSummary {
            check: String::new(),
            reason: "exit status 1".into(),
            last_log_lines: (6..=25).map(|n| format!("line {n}")).collect(),
        }
    );
    let status = runs
        .get_run(as_user(
            BOB,
            GetRunRequest {
                run: ATTEMPT.into(),
            },
        ))
        .await
        .unwrap_err();
    assert_eq!(refusal(status), (Code::NotFound, "RUN_NOT_FOUND".into()));
}

#[test]
fn last_lines_drop_a_line_cut_by_the_byte_bound() {
    let dir = std::env::temp_dir().join(format!("kajiba-runs-lines-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("log");
    let long = "x".repeat(SUMMARY_BYTES as usize);
    std::fs::write(&path, format!("{long}\nlast\n")).unwrap();
    let end = std::fs::metadata(&path).unwrap().len();
    assert_eq!(last_lines(&path, end), ["last"]);
    assert!(last_lines(&dir.join("deleted"), 10).is_empty());
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn runs_are_listed_newest_first_by_cursor() {
    let (fixture, runs) = fixture("").await;
    let tip = head(&fixture.bare("alice/ci").await);
    let mut created = Vec::new();
    for _ in 0..3 {
        created.push(
            trigger(&runs, ALICE, "alice/ci", &tip)
                .await
                .unwrap()
                .public_id,
        );
    }
    let zeta = head(&fixture.bare("bob/zeta").await);
    trigger(&runs, BOB, "bob/zeta", &zeta).await.unwrap();
    let list = |repo: &str, page_token: String| {
        runs.list_runs(as_user(
            ALICE,
            ListRunsRequest {
                repo: repo.into(),
                page_size: 2,
                page_token,
            },
        ))
    };
    let ids = |runs: &[Run]| {
        runs.iter()
            .map(|run| run.public_id.clone())
            .collect::<Vec<_>>()
    };
    let first = list("alice/ci", String::new()).await.unwrap().into_inner();
    assert_eq!(ids(&first.runs), [created[2].clone(), created[1].clone()]);
    let second = list("alice/ci", first.next_page_token.clone())
        .await
        .unwrap()
        .into_inner();
    assert_eq!(ids(&second.runs), [created[0].clone()]);
    assert!(second.next_page_token.is_empty());
    // A cursor belongs to its repository.
    let status = list("bob/zeta", first.next_page_token).await.unwrap_err();
    assert_eq!(
        refusal(status),
        (Code::InvalidArgument, "INVALID_FIELD".into())
    );
    let status = list("alice/nope", String::new()).await.unwrap_err();
    assert_eq!(refusal(status), (Code::NotFound, "REPO_NOT_FOUND".into()));
}

#[tokio::test]
async fn get_run_answers_a_grpc_web_unary_call() {
    use axum::body::Body;
    use tonic::codegen::Service;
    let (fixture, runs) = fixture(&tokens()).await;
    let tip = head(&fixture.bare("alice/ci").await);
    let id = trigger(&runs, ALICE, "alice/ci", &tip)
        .await
        .unwrap()
        .public_id;
    let mut app = super::super::router(
        fixture.repos.0.clone(),
        false,
        Default::default(),
        "https://example.org",
    )
    .unwrap();
    let message = GetRunRequest { run: id.clone() }.encode_to_vec();
    let mut body = vec![0];
    body.extend_from_slice(&(message.len() as u32).to_be_bytes());
    body.extend_from_slice(&message);
    let response = app
        .call(
            axum::http::Request::post("/kajiba.v1.RunService/GetRun")
                .header("content-type", "application/grpc-web+proto")
                .header("origin", "https://example.org")
                .header("authorization", format!("Bearer {READ}"))
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response.headers()["content-type"],
        "application/grpc-web+proto"
    );
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    let length = u32::from_be_bytes(bytes[1..5].try_into().unwrap()) as usize;
    let run = GetRunResponse::decode(&bytes[5..5 + length])
        .unwrap()
        .run
        .unwrap();
    assert_eq!((run.state(), run.public_id), (RunState::Queued, id));
}

/// A runner's progress on a triggered run, written as the runner handler would.
struct Progress<'a> {
    fixture: &'a Fixture,
    runs: &'a Runs,
    job: i64,
}

impl<'a> Progress<'a> {
    async fn new(fixture: &'a Fixture, runs: &'a Runs, run: &str) -> Self {
        let mut writer = fixture.writer.lock().await;
        sqlx::query(
            "INSERT OR IGNORE INTO runner (public_id, name, capabilities, version, connected)
             VALUES ('01J00000000000000000000001', 'build-1', '{}', '0.1.0', 1)",
        )
        .execute(&mut *writer)
        .await
        .unwrap();
        let job = sqlx::query_scalar(
            "SELECT job.id FROM job JOIN run ON run.id = run_id WHERE run.public_id = ?",
        )
        .bind(run)
        .fetch_one(&mut *writer)
        .await
        .unwrap();
        Self { fixture, runs, job }
    }

    /// Attempt `number`, running, with public id `01J…<number>`.
    async fn attempt(&self, number: i64) -> String {
        let public_id = format!("01J{number:0>23}");
        sqlx::query("UPDATE job_attempt SET state = 'lost' WHERE job_id = ? AND state = 'running'")
            .bind(self.job)
            .execute(&mut *self.fixture.writer.lock().await)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO job_attempt (public_id, job_id, number, runner_id, state, lease_expires_at, started_at)
             VALUES (?, ?, ?, 1, 'running', 0, 0)",
        )
        .bind(&public_id)
        .bind(self.job)
        .bind(number)
        .execute(&mut *self.fixture.writer.lock().await)
        .await
        .unwrap();
        public_id
    }

    /// Append `data` to the attempt's log and index it, then signal watchers.
    async fn log(&self, attempt: &str, data: &str) {
        let path = self.runs.log(attempt);
        let offset = std::fs::metadata(&path).map_or(0, |meta| meta.len());
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .unwrap();
        std::io::Write::write_all(&mut file, data.as_bytes()).unwrap();
        sqlx::query(
            "INSERT INTO log_chunk (attempt_id, seq, step_idx, offset, length)
             SELECT id, (SELECT count(*) + 1 FROM log_chunk WHERE attempt_id = job_attempt.id), 0, ?, ?
             FROM job_attempt WHERE public_id = ?",
        )
        .bind(offset as i64)
        .bind(data.len() as i64)
        .bind(attempt)
        .execute(&mut *self.fixture.writer.lock().await)
        .await
        .unwrap();
        self.runs.0.ci_progress.send_replace(());
    }

    async fn fail(&self, run: &str) {
        let mut writer = self.fixture.writer.lock().await;
        for sql in [
            "UPDATE job_attempt SET state = 'failed', exit_code = 1 WHERE job_id = ?1 AND state = 'running'",
            "UPDATE job SET state = 'failed' WHERE id = ?1",
        ] {
            sqlx::query(sql)
                .bind(self.job)
                .execute(&mut *writer)
                .await
                .unwrap();
        }
        sqlx::query(
            "UPDATE run SET state = 'failed', failure_reason = 'exit status 1' WHERE public_id = ?",
        )
        .bind(run)
        .execute(&mut *writer)
        .await
        .unwrap();
        drop(writer);
        self.runs.0.ci_progress.send_replace(());
    }
}

type Events = <Runs as RunService>::WatchRunStream;

async fn watch(
    runs: &Runs,
    user: i64,
    run: &str,
    offset: u64,
    follow: bool,
) -> Result<Events, Status> {
    runs.watch_run(as_user(
        user,
        WatchRunRequest {
            run: run.into(),
            offset,
            follow,
        },
    ))
    .await
    .map(Response::into_inner)
}

/// The next event, `None` once the stream ended.
async fn next(events: &mut Events) -> Option<watch_run_response::Event> {
    use tokio_stream::StreamExt;
    tokio::time::timeout(Duration::from_secs(5), events.next())
        .await
        .expect("an event or the end")
        .map(|event| event.unwrap().event.unwrap())
}

/// Events until `bytes` of log arrived: the sections' attempt ids, then the bytes, each
/// starting where the previous one ended.
async fn read(events: &mut Events, from: u64, bytes: usize) -> (Vec<(String, u64)>, String) {
    let (mut sections, mut data) = (Vec::new(), String::new());
    while data.len() < bytes {
        match next(events).await.unwrap() {
            watch_run_response::Event::Section(section) => {
                sections.push((section.attempt.unwrap().public_id, section.offset))
            }
            watch_run_response::Event::Log(log) => {
                assert_eq!(log.offset, from + data.len() as u64, "no gap, no duplicate");
                data.push_str(std::str::from_utf8(&log.data).unwrap());
            }
            watch_run_response::Event::Finished(_) => panic!("finished early"),
        }
    }
    (sections, data)
}

#[tokio::test]
async fn followers_get_stored_then_live_bytes_then_the_terminal_run() {
    let (fixture, runs) = fixture("").await;
    let tip = head(&fixture.bare("alice/ci").await);
    let run = trigger(&runs, ALICE, "alice/ci", &tip)
        .await
        .unwrap()
        .public_id;
    let progress = Progress::new(&fixture, &runs, &run).await;
    let attempt = progress.attempt(1).await;
    progress.log(&attempt, "stored ").await;
    let mut first = watch(&runs, BOB, &run, 0, true).await.unwrap();
    let mut second = watch(&runs, ALICE, &run, 0, true).await.unwrap();
    for events in [&mut first, &mut second] {
        assert_eq!(
            read(events, 0, 7).await,
            (vec![(attempt.clone(), 0)], "stored ".into())
        );
    }
    progress.log(&attempt, "live\n").await;
    progress.log(&attempt, "error: boom\n").await;
    for events in [&mut first, &mut second] {
        assert_eq!(
            read(events, 7, 17).await,
            (vec![], "live\nerror: boom\n".into())
        );
    }
    progress.fail(&run).await;
    for events in [&mut first, &mut second] {
        let Some(watch_run_response::Event::Finished(finished)) = next(events).await else {
            panic!("the terminal run");
        };
        assert_eq!(finished.state(), RunState::Failed);
        assert_eq!(
            finished.failure.unwrap().last_log_lines,
            ["stored live", "error: boom"]
        );
        assert!(next(events).await.is_none());
    }
}

#[tokio::test]
async fn a_watcher_cannot_follow_a_reused_run_id_into_another_repository() {
    use tokio_stream::StreamExt;
    let (fixture, runs) = fixture("").await;
    let tip = head(&fixture.bare("alice/ci").await);
    let run = trigger(&runs, ALICE, "alice/ci", &tip)
        .await
        .unwrap()
        .public_id;
    let progress = Progress::new(&fixture, &runs, &run).await;
    let attempt = progress.attempt(1).await;
    progress.log(&attempt, "public\n").await;
    let mut events = watch(&runs, ALICE, &run, 0, true).await.unwrap();
    assert_eq!(read(&mut events, 0, 7).await.1, "public\n");

    let id: i64 = sqlx::query_scalar("DELETE FROM run WHERE public_id = ? RETURNING id")
        .bind(&run)
        .fetch_one(&mut *fixture.writer.lock().await)
        .await
        .unwrap();
    let tip = head(&fixture.bare("bob/zeta").await);
    let replacement = trigger(&runs, BOB, "bob/zeta", &tip)
        .await
        .unwrap()
        .public_id;
    let reused: i64 = sqlx::query_scalar("SELECT id FROM run WHERE public_id = ?")
        .bind(&replacement)
        .fetch_one(runs.readers().unwrap())
        .await
        .unwrap();
    assert_eq!(reused, id, "SQLite reuses the deleted integer key");
    let progress = Progress::new(&fixture, &runs, &replacement).await;
    let attempt = progress.attempt(2).await;
    progress.log(&attempt, "replacement build output\n").await;

    let status = tokio::time::timeout(Duration::from_secs(5), events.next())
        .await
        .expect("the deleted run ends the watch")
        .unwrap()
        .expect_err("no event from the replacement run");
    assert_eq!(refusal(status), (Code::NotFound, "RUN_NOT_FOUND".into()));
    assert!(next(&mut events).await.is_none());
}

#[tokio::test]
async fn a_one_shot_read_of_a_running_run_ends_after_the_stored_bytes() {
    let (fixture, runs) = fixture("").await;
    let tip = head(&fixture.bare("alice/ci").await);
    let run = trigger(&runs, ALICE, "alice/ci", &tip)
        .await
        .unwrap()
        .public_id;
    // A queued run without attempt: nothing yet.
    let mut events = watch(&runs, ALICE, &run, 0, false).await.unwrap();
    assert!(next(&mut events).await.is_none());
    let progress = Progress::new(&fixture, &runs, &run).await;
    let attempt = progress.attempt(1).await;
    progress.log(&attempt, "partial").await;
    let mut events = watch(&runs, ALICE, &run, 3, false).await.unwrap();
    assert_eq!(
        read(&mut events, 3, 4).await,
        (vec![(attempt, 0)], "tial".into())
    );
    assert!(next(&mut events).await.is_none());
}

#[tokio::test]
async fn a_replayed_attempt_opens_a_new_section() {
    let (fixture, runs) = fixture("").await;
    let tip = head(&fixture.bare("alice/ci").await);
    let run = trigger(&runs, ALICE, "alice/ci", &tip)
        .await
        .unwrap()
        .public_id;
    let progress = Progress::new(&fixture, &runs, &run).await;
    let lost = progress.attempt(1).await;
    progress.log(&lost, "one\n").await;
    let replayed = progress.attempt(2).await;
    progress.log(&replayed, "two\n").await;
    let mut events = watch(&runs, ALICE, &run, 0, false).await.unwrap();
    assert_eq!(
        read(&mut events, 0, 8).await,
        (vec![(lost, 0), (replayed.clone(), 4)], "one\ntwo\n".into())
    );
    // Resuming past the lost attempt starts at the replayed one's section.
    let mut events = watch(&runs, ALICE, &run, 4, false).await.unwrap();
    assert_eq!(
        read(&mut events, 4, 4).await,
        (vec![(replayed, 4)], "two\n".into())
    );
    assert!(next(&mut events).await.is_none());
}

#[tokio::test]
async fn retention_preserves_offsets_during_a_replayed_run() {
    let (fixture, runs) = fixture("").await;
    let tip = head(&fixture.bare("alice/ci").await);
    let run = trigger(&runs, ALICE, "alice/ci", &tip)
        .await
        .unwrap()
        .public_id;
    let progress = Progress::new(&fixture, &runs, &run).await;
    let lost = progress.attempt(1).await;
    progress.log(&lost, "one\n").await;
    let replayed = progress.attempt(2).await;
    progress.log(&replayed, "two\n").await;
    let mut follower = watch(&runs, ALICE, &run, 0, true).await.unwrap();
    assert_eq!(read(&mut follower, 0, 8).await.1, "one\ntwo\n");
    sqlx::query("UPDATE job_attempt SET finished_at = 0 WHERE public_id = ?")
        .bind(&lost)
        .execute(&mut *fixture.writer.lock().await)
        .await
        .unwrap();
    for expected in [1, 0] {
        assert_eq!(
            crate::db::retention::purge_logs(
                &mut *fixture.writer.lock().await,
                &runs.0.state_dir.join("ci/logs"),
                Duration::from_secs(1),
                2_000,
            )
            .await
            .unwrap(),
            expected
        );
    }
    assert!(!runs.log(&lost).exists());
    // Fresh and reconnecting watchers skip deleted bytes but retain the replay's position.
    for (offset, data) in [(0, "two\n"), (2, "two\n"), (4, "two\n"), (6, "o\n")] {
        let mut events = watch(&runs, ALICE, &run, offset, false).await.unwrap();
        let mut sections = Vec::new();
        if offset < 4 {
            sections.push((lost.clone(), 0));
        }
        sections.push((replayed.clone(), 4));
        assert_eq!(
            read(&mut events, offset.max(4), data.len()).await,
            (sections, data.into())
        );
        assert!(next(&mut events).await.is_none());
    }
    progress.log(&replayed, "three\n").await;
    assert_eq!(read(&mut follower, 8, 6).await, (vec![], "three\n".into()));
    progress.fail(&run).await;
    assert!(matches!(
        next(&mut follower).await,
        Some(watch_run_response::Event::Finished(_))
    ));
    assert!(next(&mut follower).await.is_none());
}

#[tokio::test]
async fn watching_needs_a_readable_run() {
    let (fixture, runs) = fixture("").await;
    let tip = head(&fixture.bare("alice/ci").await);
    let run = trigger(&runs, ALICE, "alice/ci", &tip)
        .await
        .unwrap()
        .public_id;
    // An unknown or disabled user reads nothing.
    let status = watch(&runs, 99, &run, 0, false).await.err().unwrap();
    assert_eq!(refusal(status), (Code::NotFound, "REPO_NOT_FOUND".into()));
    let status = watch(&runs, ALICE, ATTEMPT, 0, false).await.err().unwrap();
    assert_eq!(refusal(status), (Code::NotFound, "RUN_NOT_FOUND".into()));
}

/// The spec (when given) then `bundle` in chunks the service's 16 KiB message bound admits.
async fn create(
    runs: &Runs,
    user: i64,
    spec: Option<WorktreeRunSpec>,
    bundle: &[u8],
) -> Result<Run, Status> {
    let parts = spec.map(Part::Spec).into_iter().chain(
        bundle
            .chunks(16_000)
            .map(|chunk| Part::Chunk(chunk.to_vec())),
    );
    upload(runs, user, parts).await
}

async fn upload(
    runs: &Runs,
    user: i64,
    parts: impl IntoIterator<Item = Part>,
) -> Result<Run, Status> {
    use tonic::codec::Codec;
    let mut body = Vec::new();
    for part in parts {
        let message = CreateWorktreeRunRequest { part: Some(part) }.encode_to_vec();
        body.push(0);
        body.extend_from_slice(&(message.len() as u32).to_be_bytes());
        body.extend_from_slice(&message);
    }
    let decoder =
        tonic::codec::ProstCodec::<CreateWorktreeRunResponse, CreateWorktreeRunRequest>::default()
            .decoder();
    let parts = tonic::Streaming::new_request(decoder, axum::body::Body::from(body), None, None);
    runs.create_worktree_run(as_user(user, parts))
        .await
        .map(|response| response.into_inner().run.unwrap())
}

/// A clone of `bare` at `main`, for bundles.
fn clone(fixture: &Fixture, bare: &Path) -> std::path::PathBuf {
    let work = fixture.dir.join("tmp/clone");
    git(
        &fixture.dir,
        &[
            "clone",
            "-q",
            bare.to_str().unwrap(),
            work.to_str().unwrap(),
        ],
    );
    work
}

/// Commit a change in `work` and return the new head.
fn commit(work: &Path, file: &str) -> String {
    std::fs::write(work.join(file), file).unwrap();
    git(work, &["add", "-A"]);
    git(work, &["commit", "-q", "-m", file]);
    git(work, &["rev-parse", "HEAD"]).trim().to_owned()
}

fn bundle(work: &Path, args: &[&str]) -> Vec<u8> {
    let path = work.join("out.bundle");
    let path = path.to_str().unwrap();
    git(
        work,
        &[&["bundle", "create", "-q", path][..], args].concat(),
    );
    std::fs::read(path).unwrap()
}

fn listing(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

#[tokio::test]
async fn a_worktree_bundle_becomes_a_run_outside_the_repository() {
    let (fixture, runs) = fixture("").await;
    let bare = fixture.bare("alice/ci").await;
    let tip = head(&bare);
    let work = clone(&fixture, &bare);
    let captured = commit(&work, "change");
    git(&work, &["branch", "wip"]);
    let bytes = bundle(&work, &["wip", "--not", &tip]);
    let selected = WorktreeRunSpec {
        checks: vec!["vm".into(), "unit".into()],
        features: vec!["kvm".into()],
        ..spec("alice/ci")
    };
    let run = create(&runs, BOB, Some(selected), &bytes).await.unwrap();
    assert_eq!(
        (
            run.trigger(),
            run.state(),
            run.sha.as_str(),
            run.r#ref.as_str(),
            run.triggered_by.as_str()
        ),
        (
            RunTrigger::Worktree,
            RunState::Queued,
            captured.as_str(),
            "",
            "bob"
        )
    );
    assert_eq!(run.checks, ["vm", "unit"]);
    let state = fixture.dir.join("server");
    assert_eq!(
        std::fs::read(state.join(format!("ci/bundles/{}.bundle", run.public_id))).unwrap(),
        bytes
    );
    assert!(listing(&state.join("tmp")).is_empty());
    let row: (String, Option<String>, Option<i64>, Option<String>, String) = sqlx::query_as(
        "SELECT trigger, ref, event_id, checks, job.requirements FROM run JOIN job ON job.run_id = run.id",
    )
    .fetch_one(runs.readers().unwrap())
    .await
    .unwrap();
    assert_eq!(
        (row.0.as_str(), row.1, row.2, row.3.as_deref()),
        ("worktree", None, None, Some(r#"["vm","unit"]"#))
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&row.4).unwrap(),
        serde_json::json!({ "backend": "nix", "arch": "x86_64", "os": "linux", "tags": ["kvm"] })
    );
    let events: Vec<(String, Option<i64>, String)> =
        sqlx::query_as("SELECT kind, principal_id, payload FROM event WHERE kind = 'run-created'")
            .fetch_all(runs.readers().unwrap())
            .await
            .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].1, Some(BOB));
    assert!(
        events[0].2.contains(r#""trigger":"worktree""#),
        "{}",
        events[0].2
    );

    // The captured commit never enters the repository: nothing to serve or browse.
    let missing = std::process::Command::new("git")
        .args(["-C", bare.to_str().unwrap(), "cat-file", "-e", &captured])
        .status()
        .unwrap();
    assert!(!missing.success());
    let status = trigger(&runs, BOB, "alice/ci", &captured)
        .await
        .unwrap_err();
    assert_eq!(
        refusal(status),
        (Code::NotFound, "REVISION_NOT_FOUND".into())
    );
    use kajiba_proto::v1::{GetTreeRequest, repo_service_server::RepoService};
    let status = fixture
        .repos
        .get_tree(as_user(
            BOB,
            GetTreeRequest {
                repo: "alice/ci".into(),
                revision: captured.clone(),
                path: String::new(),
            },
        ))
        .await
        .unwrap_err();
    assert_eq!(
        refusal(status),
        (Code::NotFound, "REVISION_NOT_FOUND".into())
    );
    // A whole-history bundle (no prerequisite) is accepted too.
    let whole = bundle(&work, &["wip"]);
    assert!(
        create(&runs, BOB, Some(spec("alice/ci")), &whole)
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn worktree_bundles_are_refused_with_nothing_left_on_disk() {
    let (fixture, runs) = fixture("").await;
    let bare = fixture.bare("alice/ci").await;
    let tip = head(&bare);
    let work = clone(&fixture, &bare);
    let contributed = commit(&work, "contributed");
    git(
        &work,
        &[
            "push",
            "-q",
            bare.to_str().unwrap(),
            "HEAD:refs/contrib/bob/x",
        ],
    );
    commit(&work, "on-contrib");
    git(&work, &["branch", "wip"]);
    git(&work, &["branch", "wip2"]);
    let mut not_a_bundle = b"# v2 git bundle\n".to_vec();
    not_a_bundle.extend_from_slice(&[0; 64]);
    let invalid = (Code::InvalidArgument, "BUNDLE_INVALID");
    let missing = (Code::FailedPrecondition, "BUNDLE_PREREQUISITE_MISSING");
    let mut no_pack = bundle(&work, &["wip", "--not", &tip]);
    let header = no_pack.windows(2).position(|w| w == b"\n\n").unwrap();
    no_pack.truncate(header + 2);
    let cases = [
        (b"not a bundle".to_vec(), invalid),
        (not_a_bundle, invalid),
        (no_pack, invalid),
        (Vec::new(), invalid),
        (bundle(&work, &["wip", "wip2", "--not", &tip]), invalid),
        // Its prerequisite is only on refs/contrib/*.
        (bundle(&work, &["wip", "--not", &contributed]), missing),
    ];
    for (bytes, expected) in cases {
        let status = create(&runs, BOB, Some(spec("alice/ci")), &bytes)
            .await
            .unwrap_err();
        assert_eq!(refusal(status), (expected.0, expected.1.into()));
    }
    // A prerequisite the forge never saw.
    let unseen = commit(&work, "unseen");
    commit(&work, "after-unseen");
    git(&work, &["branch", "-f", "wip"]);
    let status = create(
        &runs,
        BOB,
        Some(spec("alice/ci")),
        &bundle(&work, &["wip", "--not", &unseen]),
    )
    .await
    .unwrap_err();
    assert_eq!(refusal(status), (missing.0, missing.1.into()));

    // One byte over 64 MiB is refused while streaming.
    let status = create(
        &runs,
        BOB,
        Some(spec("alice/ci")),
        &vec![0; MAX_BUNDLE as usize + 1],
    )
    .await
    .unwrap_err();
    assert_eq!(
        refusal(status),
        (Code::ResourceExhausted, "BUNDLE_TOO_LARGE".into())
    );
    // Out of order: no spec first, or a second spec.
    let field = (Code::InvalidArgument, "INVALID_FIELD".into());
    assert_eq!(
        refusal(create(&runs, BOB, None, b"x").await.unwrap_err()),
        field
    );
    let twice = [
        Part::Spec(spec("alice/ci")),
        Part::Chunk(b"x".to_vec()),
        Part::Spec(spec("alice/ci")),
    ];
    assert_eq!(refusal(upload(&runs, BOB, twice).await.unwrap_err()), field);
    // Admission comes first: a refused spec stores nothing either.
    let status = create(&runs, ALICE, Some(spec("bob/zeta")), b"anything")
        .await
        .unwrap_err();
    assert_eq!(
        refusal(status),
        (Code::PermissionDenied, "ROLE_MISSING".into())
    );

    let state = fixture.dir.join("server");
    assert!(listing(&state.join("tmp")).is_empty());
    assert!(listing(&state.join("ci/bundles")).is_empty());
    let runs_count: i64 = sqlx::query_scalar("SELECT count(*) FROM run")
        .fetch_one(runs.readers().unwrap())
        .await
        .unwrap();
    assert_eq!(runs_count, 0);
}

#[tokio::test]
async fn run_watches_are_bounded_per_caller_and_in_all() {
    let (fixture, runs) = fixture("").await;
    let tip = head(&fixture.bare("alice/ci").await);
    let run = trigger(&runs, ALICE, "alice/ci", &tip)
        .await
        .unwrap()
        .public_id;
    let mut open = Vec::new();
    for _ in 0..WATCHERS_PER_PRINCIPAL {
        open.push(watch(&runs, BOB, &run, 0, true).await.unwrap());
    }
    let status = watch(&runs, BOB, &run, 0, true).await.err().unwrap();
    assert_eq!(
        (status.code(), reason(&status).as_str()),
        (Code::ResourceExhausted, "WATCH_STREAMS_LIMIT")
    );
    // Another caller has its own places; a closed watch gives its place back.
    let alice = watch(&runs, ALICE, &run, 0, true).await.unwrap();
    drop(open.pop());
    let mut reopened = None;
    for _ in 0..100 {
        if let Ok(events) = watch(&runs, BOB, &run, 0, true).await {
            reopened = Some(events);
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(reopened.is_some());
    // The server-wide bound counts every caller.
    drop((open, alice, reopened));
    let slots = &runs.0.watchers;
    let mut held = Vec::new();
    for _ in 0..100 {
        held.clear();
        held.extend((0..MAX_WATCHERS as i64).map_while(|user| slots.take(user + 100, 1)));
        if held.len() == MAX_WATCHERS {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(held.len(), MAX_WATCHERS);
    let status = watch(&runs, ALICE, &run, 0, true).await.err().unwrap();
    assert_eq!(status.code(), Code::ResourceExhausted);
}

#[tokio::test]
async fn an_upload_silent_past_its_idle_time_ends_with_deadline_exceeded() {
    use tonic::codec::Codec;
    let (_fixture, runs) = fixture("").await;
    let (_sender, receiver) =
        tokio::sync::mpsc::channel::<Result<axum::body::Bytes, std::io::Error>>(1);
    let body = axum::body::Body::new(crate::transport::Idle::new(
        axum::body::Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(receiver)),
        Duration::from_millis(50),
    ));
    let decoder =
        tonic::codec::ProstCodec::<CreateWorktreeRunResponse, CreateWorktreeRunRequest>::default()
            .decoder();
    let parts = tonic::Streaming::new_request(decoder, body, None, None);
    let status = runs
        .create_worktree_run(as_user(BOB, parts))
        .await
        .err()
        .unwrap();
    assert_eq!(status.code(), Code::DeadlineExceeded, "{status:?}");
}
