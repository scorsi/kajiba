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
const REPOS: &str = "[repos.ci]\nowner='alice'\ndescription=''\nci='push'\n\
     permissions.bob='write'\n[repos.off]\nowner='alice'\ndescription=''\n\
     [repos.zeta]\nowner='bob'\ndescription=''\nci='push'\n";
const ALICE: i64 = 1;
const BOB: i64 = 2;

async fn fixture(extra: &str) -> (Fixture, Runs) {
    let fixture = Fixture::new(&format!("{REPOS}{extra}")).await;
    // One declared runner: x86_64-linux, nix, tagged `kvm` (offline: declared is enough).
    let runner = kajiba_core::config::ServerConfig::parse(
        "[http]\npublic_url='https://example.org'\n[runner_listener]\nlisten='127.0.0.1:8443'\n\
         trust_domain='example.org'\nsvid_file='/s'\nkey_file='/k'\nbundle_file='/b'\n\
         [runners.build-1]\nspiffe_id='spiffe://example.org/build-1/kajiba-runner'\n\
         capabilities={ arch='x86_64', os='linux', tags=['kvm'] }\n",
    )
    .unwrap()
    .runners;
    fixture.repos.0.set_runners(Declared {
        runners: tokio::sync::watch::Sender::new(Arc::new(runner)),
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
        (
            "CancelRun",
            &read,
            CancelRunRequest::default().encode_to_vec(),
        ),
        (
            "RerunRun",
            &read,
            RerunRunRequest::default().encode_to_vec(),
        ),
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
        base_sha: String::new(),
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
        fixture("[repos.empty]\nowner='alice'\ndescription=''\nci='push'\n").await;
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

/// ADR 0133: CI mode `manual` admits manual and worktree runs; `off` refuses both above.
#[tokio::test]
async fn manual_ci_admits_triggered_and_worktree_runs() {
    let (fixture, runs) =
        fixture("[repos.hand]\nowner='alice'\ndescription=''\nci='manual'\n").await;
    fixture
        .fill("alice/hand", &[("flake.nix", "{}\n")], &[])
        .await;
    let tip = head(&fixture.bare("alice/hand").await);
    let run = trigger(&runs, ALICE, "alice/hand", &tip).await.unwrap();
    assert_eq!(run.trigger(), RunTrigger::Manual);
    base(&runs, ALICE, spec("alice/hand")).await.unwrap();
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
        (
            BOB,
            with("alice/ci", |s| s.base_sha = "bad".into()),
            invalid,
        ),
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
                ..Default::default()
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

    // Filtered by state, still newest first.
    sqlx::query("UPDATE run SET state = 'failed' WHERE public_id = ?")
        .bind(&created[1])
        .execute(&mut *fixture.writer.lock().await)
        .await
        .unwrap();
    let only = |states: Vec<RunState>| {
        runs.list_runs(as_user(
            ALICE,
            ListRunsRequest {
                repo: "alice/ci".into(),
                states: states.into_iter().map(i32::from).collect(),
                ..Default::default()
            },
        ))
    };
    let failed = only(vec![RunState::Failed]).await.unwrap().into_inner();
    assert_eq!(ids(&failed.runs), [created[1].clone()]);
    let both = only(vec![RunState::Queued, RunState::Failed])
        .await
        .unwrap()
        .into_inner();
    assert_eq!(ids(&both.runs).len(), 3);
    assert!(
        only(vec![RunState::Succeeded])
            .await
            .unwrap()
            .into_inner()
            .runs
            .is_empty()
    );
    let status = only(vec![RunState::Unspecified]).await.unwrap_err();
    assert_eq!(
        refusal(status),
        (Code::InvalidArgument, "INVALID_FIELD".into())
    );

    // Filtered by exact ref, with states and pagination; an unknown ref is an empty page.
    sqlx::query("UPDATE run SET ref = 'refs/heads/main' WHERE public_id IN (?, ?)")
        .bind(&created[0])
        .bind(&created[1])
        .execute(&mut *fixture.writer.lock().await)
        .await
        .unwrap();
    let on = |r#ref: &str, states: Vec<RunState>, page_size: i32, page_token: String| {
        runs.list_runs(as_user(
            ALICE,
            ListRunsRequest {
                repo: "alice/ci".into(),
                r#ref: r#ref.into(),
                states: states.into_iter().map(i32::from).collect(),
                page_size,
                page_token,
            },
        ))
    };
    let main = |states, size, token| on("refs/heads/main", states, size, token);
    let first = main(vec![], 1, String::new()).await.unwrap().into_inner();
    assert_eq!(ids(&first.runs), [created[1].clone()]);
    let second = main(vec![], 1, first.next_page_token)
        .await
        .unwrap()
        .into_inner();
    assert_eq!(ids(&second.runs), [created[0].clone()]);
    assert!(second.next_page_token.is_empty());
    let failed = main(vec![RunState::Failed], 0, String::new())
        .await
        .unwrap()
        .into_inner();
    assert_eq!(ids(&failed.runs), [created[1].clone()]);
    for unknown in ["refs/heads/nope", "main"] {
        let page = on(unknown, vec![], 0, String::new())
            .await
            .unwrap()
            .into_inner();
        assert!(page.runs.is_empty() && page.next_page_token.is_empty());
    }
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
    .map(|mut response| {
        use tokio_stream::StreamExt;
        let permit = response
            .extensions_mut()
            .remove::<super::super::stream::WatchPermit>();
        Box::pin(response.into_inner().map(move |event| {
            let _ = &permit;
            event
        })) as Events
    })
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
            watch_run_response::Event::Keepalive(_) => {}
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
async fn stopping_ends_followers_with_unavailable() {
    use tokio_stream::StreamExt;
    let (fixture, runs) = fixture("").await;
    let tip = head(&fixture.bare("alice/ci").await);
    let run = trigger(&runs, ALICE, "alice/ci", &tip)
        .await
        .unwrap()
        .public_id;
    let mut follower = watch(&runs, ALICE, &run, 0, true).await.unwrap();
    runs.0.stop.send_replace(true);
    let ended = tokio::time::timeout(Duration::from_secs(5), follower.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(ended.code(), tonic::Code::Unavailable);
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
async fn a_quiet_follower_gets_keepalives_between_bytes() {
    let (fixture, runs) = fixture("").await;
    let tip = head(&fixture.bare("alice/ci").await);
    let run = trigger(&runs, ALICE, "alice/ci", &tip)
        .await
        .unwrap()
        .public_id;
    let progress = Progress::new(&fixture, &runs, &run).await;
    let attempt = progress.attempt(1).await;
    progress.log(&attempt, "start\n").await;
    let (watcher, mut events) = tokio::sync::mpsc::channel(4);
    let quiet = Duration::from_millis(100);
    let task = {
        let (runs, run) = (runs.clone(), run.clone());
        let signal = runs.0.ci_progress.subscribe();
        tokio::spawn(async move { runs.watch(&run, 0, true, signal, &watcher, quiet).await })
    };
    let mut next = async || {
        let started = tokio::time::Instant::now();
        let event = tokio::time::timeout(Duration::from_secs(5), events.recv())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .event
            .unwrap();
        (event, started.elapsed())
    };
    assert!(matches!(
        next().await.0,
        watch_run_response::Event::Section(_)
    ));
    assert!(matches!(next().await.0, watch_run_response::Event::Log(_)));
    for _ in 0..2 {
        let (event, waited) = next().await;
        assert!(matches!(event, watch_run_response::Event::Keepalive(_)));
        assert!(
            waited >= quiet / 2,
            "not before the run was quiet: {waited:?}"
        );
    }
    progress.log(&attempt, "more\n").await;
    let watch_run_response::Event::Log(log) = next().await.0 else {
        panic!("bytes after keepalives");
    };
    assert_eq!((log.offset, log.data.as_slice()), (6, &b"more\n"[..]));
    drop(events);
    task.await.unwrap().unwrap();
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
        base_sha: tip.clone(),
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
    assert_eq!(run.base_sha, tip);
    let reread = runs
        .get_run(as_user(
            BOB,
            GetRunRequest {
                run: run.public_id.clone(),
            },
        ))
        .await
        .unwrap()
        .into_inner()
        .run
        .unwrap();
    assert_eq!(reread.base_sha, tip);
    assert_eq!(reread.checks, run.checks);
    assert_eq!(reread.triggered_by, "bob");
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
async fn terminal_watches_hold_slots_through_encoded_delivery() {
    use tonic::codegen::Service;
    let (fixture, runs) = fixture(&tokens()).await;
    let tip = head(&fixture.bare("alice/ci").await);
    let run = trigger(&runs, ALICE, "alice/ci", &tip)
        .await
        .unwrap()
        .public_id;
    sqlx::query("UPDATE run SET state = 'cancelled' WHERE public_id = ?")
        .bind(&run)
        .execute(&mut *fixture.writer.lock().await)
        .await
        .unwrap();
    let mut app = super::super::router(
        runs.0.clone(),
        false,
        Default::default(),
        "https://example.org",
    )
    .unwrap();
    let message = WatchRunRequest {
        run: run.clone(),
        offset: 0,
        follow: true,
    }
    .encode_to_vec();
    let mut encoded = vec![0];
    encoded.extend((message.len() as u32).to_be_bytes());
    encoded.extend(message);
    let mut bodies = Vec::new();
    for _ in 0..WATCHERS_PER_PRINCIPAL {
        let response = app
            .call(
                axum::http::Request::post("/kajiba.v1.RunService/WatchRun")
                    .header("content-type", "application/grpc")
                    .header("authorization", format!("Bearer {READ}"))
                    .body(axum::body::Body::from(encoded.clone()))
                    .unwrap(),
            )
            .await
            .unwrap();
        bodies.push(response.into_body());
    }
    // Let the producers finish; unread terminal messages must still occupy slots.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(runs.0.watchers.held(&ALICE), WATCHERS_PER_PRINCIPAL);
    let status = watch(&runs, ALICE, &run, 0, true).await.err().unwrap();
    assert_eq!(
        (status.code(), reason(&status).as_str()),
        (Code::ResourceExhausted, "WATCH_STREAMS_LIMIT")
    );
    drop(bodies.pop());
    tokio::time::timeout(Duration::from_secs(5), async {
        while runs.0.watchers.held(&ALICE) == WATCHERS_PER_PRINCIPAL {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    for body in bodies {
        let bytes = axum::body::to_bytes(body, 1 << 20).await.unwrap();
        let message = WatchRunResponse::decode(&bytes[5..]).unwrap();
        assert!(matches!(
            message.event,
            Some(watch_run_response::Event::Finished(_))
        ));
    }
    tokio::time::timeout(Duration::from_secs(5), async {
        while runs.0.watchers.held(&ALICE) != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
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

async fn cancel_run(runs: &Runs, user: i64, id: &str) -> Result<Run, Status> {
    runs.cancel_run(as_user(user, CancelRunRequest { run: id.into() }))
        .await
        .map(|r| r.into_inner().run.unwrap())
}

async fn rerun_run(runs: &Runs, user: i64, id: &str) -> Result<Run, Status> {
    runs.rerun_run(as_user(user, RerunRunRequest { run: id.into() }))
        .await
        .map(|r| r.into_inner().run.unwrap())
}

#[tokio::test]
async fn cancel_queued_running_and_finished_records_each_result_once() {
    let (fixture, runs) = fixture("").await;
    let tip = head(&fixture.bare("alice/ci").await);
    for running in [false, true] {
        let run = trigger(&runs, ALICE, "alice/ci", &tip).await.unwrap();
        let mut cancellations = runs.0.ci_cancellations.subscribe();
        let attempt = if running {
            let progress = Progress::new(&fixture, &runs, &run.public_id).await;
            let attempt = progress.attempt(1).await;
            let mut writer = fixture.writer.lock().await;
            sqlx::query("UPDATE job SET state = 'running' WHERE id = ?")
                .bind(progress.job)
                .execute(&mut *writer)
                .await
                .unwrap();
            sqlx::query("UPDATE run SET state = 'running' WHERE public_id = ?")
                .bind(&run.public_id)
                .execute(&mut *writer)
                .await
                .unwrap();
            Some(attempt)
        } else {
            None
        };
        let cancelled = cancel_run(&runs, BOB, &run.public_id).await.unwrap();
        assert_eq!(cancelled.state(), RunState::Cancelled);
        assert_eq!(cancelled.job.as_ref().unwrap().state(), JobState::Cancelled);
        assert!(cancelled.finished_at.is_some());
        if let Some(attempt) = attempt {
            assert_eq!(cancellations.recv().await.unwrap(), (1, attempt.clone()));
            assert_eq!(
                cancelled
                    .job
                    .as_ref()
                    .unwrap()
                    .current_attempt
                    .as_ref()
                    .unwrap()
                    .state(),
                AttemptState::Cancelled
            );
            let mut writer = fixture.writer.lock().await;
            for _ in 0..2 {
                assert_eq!(
                    crate::db::attempts::finish(
                        &mut writer,
                        (1, "build-1"),
                        &attempt,
                        &crate::db::attempts::Finish {
                            state: crate::db::runs::AttemptState::Succeeded,
                            exit_code: Some(0),
                            check: "",
                            reason: ""
                        },
                        "late",
                        1
                    )
                    .await
                    .unwrap(),
                    crate::db::attempts::Outcome::Done(vec![])
                );
            }
        } else {
            assert!(cancellations.try_recv().is_err());
        }
        assert_eq!(
            cancel_run(&runs, BOB, &run.public_id).await.unwrap(),
            cancelled
        );
        let events: Vec<(String, i64)> = sqlx::query_as(
            "SELECT kind, count(*) FROM event WHERE json_extract(payload, '$.run') = ? GROUP BY kind ORDER BY kind"
        ).bind(&run.public_id).fetch_all(runs.readers().unwrap()).await.unwrap();
        let mut expected = vec![
            ("run-cancelled".into(), 1),
            ("run-created".into(), 1),
            ("run-finished".into(), 1),
        ];
        if running {
            expected.insert(0, ("attempt-cancelled".into(), 1));
        }
        assert_eq!(events, expected);
    }
    for state in ["succeeded", "failed", "cancelled"] {
        let run = trigger(&runs, ALICE, "alice/ci", &tip).await.unwrap();
        sqlx::query("UPDATE run SET state = ?, finished_at = 123 WHERE public_id = ?")
            .bind(state)
            .bind(&run.public_id)
            .execute(&mut *fixture.writer.lock().await)
            .await
            .unwrap();
        let done = cancel_run(&runs, BOB, &run.public_id).await.unwrap();
        assert_eq!(done.finished_at, time(Some(123)));
        assert_eq!(
            done.state,
            match state {
                "succeeded" => RunState::Succeeded,
                "failed" => RunState::Failed,
                _ => RunState::Cancelled,
            } as i32
        );
    }
}

#[tokio::test]
async fn run_actions_follow_trigger_authorization_order() {
    let (fixture, runs) = fixture("").await;
    let tip = head(&fixture.bare("alice/ci").await);
    let run = trigger(&runs, ALICE, "alice/ci", &tip).await.unwrap();
    for (sql, user, expected) in [
        (
            "UPDATE repo SET archived_at = 1 WHERE id = 1",
            BOB,
            (Code::FailedPrecondition, "INVALID_STATE"),
        ),
        (
            "UPDATE repo SET archived_at = NULL, ci_mode = 'off' WHERE id = 1",
            BOB,
            (Code::FailedPrecondition, "CI_DISABLED"),
        ),
        (
            "UPDATE repo SET owner_id = 2 WHERE id = 1",
            ALICE,
            (Code::PermissionDenied, "ROLE_MISSING"),
        ),
        ("SELECT 1", 999, (Code::NotFound, "REPO_NOT_FOUND")),
    ] {
        sqlx::query(sql)
            .execute(&mut *fixture.writer.lock().await)
            .await
            .unwrap();
        for result in [
            cancel_run(&runs, user, &run.public_id).await,
            rerun_run(&runs, user, &run.public_id).await,
        ] {
            assert_eq!(
                refusal(result.unwrap_err()),
                (expected.0, expected.1.into())
            );
        }
    }
    assert_eq!(
        refusal(cancel_run(&runs, BOB, "absent").await.unwrap_err()),
        (Code::NotFound, "RUN_NOT_FOUND".into())
    );
}

#[tokio::test]
async fn rerun_clones_source_and_requirements_and_keeps_the_original() {
    let (fixture, runs) = fixture("").await;
    let tip = head(&fixture.bare("alice/ci").await);
    let original = trigger(&runs, ALICE, "alice/ci", &tip).await.unwrap();
    sqlx::query("UPDATE run SET trigger = 'push', ref = 'refs/heads/main' WHERE public_id = ?")
        .bind(&original.public_id)
        .execute(&mut *fixture.writer.lock().await)
        .await
        .unwrap();
    let new = rerun_run(&runs, BOB, &original.public_id).await.unwrap();
    assert_ne!(new.public_id, original.public_id);
    assert_eq!(new.rerun_of, original.public_id);
    assert_eq!(new.sha, tip);
    assert_eq!(new.r#ref, "refs/heads/main");
    assert_eq!(new.trigger(), RunTrigger::Push);
    assert_eq!(new.triggered_by, "bob");
    assert_eq!(new.state(), RunState::Queued);
    assert_eq!(new.job.unwrap().state(), JobState::Pending);
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT state FROM run WHERE public_id = ?")
            .bind(&original.public_id)
            .fetch_one(runs.readers().unwrap())
            .await
            .unwrap(),
        "queued"
    );
    let bundles = runs.0.state_dir.join("ci/bundles");
    std::fs::create_dir_all(&bundles).unwrap();
    sqlx::query("UPDATE run SET trigger = 'worktree', checks = '[\"unit\"]', base_sha = 'base' WHERE public_id = ?")
        .bind(&original.public_id).execute(&mut *fixture.writer.lock().await).await.unwrap();
    sqlx::query("UPDATE job SET requirements = '{\"backend\":\"nix\",\"arch\":\"x86_64\",\"os\":\"linux\",\"tags\":[\"kvm\"]}' WHERE public_id = ?")
        .bind(original.job.unwrap().public_id).execute(&mut *fixture.writer.lock().await).await.unwrap();
    assert_eq!(
        refusal(
            rerun_run(&runs, BOB, &original.public_id)
                .await
                .unwrap_err()
        ),
        (Code::FailedPrecondition, "BUNDLE_MISSING".into())
    );
    std::fs::write(
        bundles.join(format!("{}.bundle", original.public_id)),
        b"retained bundle",
    )
    .unwrap();
    let new = rerun_run(&runs, BOB, &original.public_id).await.unwrap();
    assert_eq!(new.checks, ["unit"]);
    assert_eq!(new.base_sha, "base");
    assert_eq!(
        std::fs::read(bundles.join(format!("{}.bundle", new.public_id))).unwrap(),
        b"retained bundle"
    );
    let requirements: Vec<String> = sqlx::query_scalar("SELECT requirements FROM job WHERE run_id IN (SELECT id FROM run WHERE trigger = 'worktree')")
        .fetch_all(runs.readers().unwrap()).await.unwrap();
    assert_eq!(requirements.len(), 2);
    assert_eq!(requirements[0], requirements[1]);
}

#[tokio::test]
async fn worktree_rerun_counts_concurrent_uploads_against_the_callers_limit() {
    let (fixture, runs) = fixture("").await;
    let tip = head(&fixture.bare("alice/ci").await);
    let run = trigger(&runs, ALICE, "alice/ci", &tip).await.unwrap();
    sqlx::query("UPDATE run SET trigger = 'worktree' WHERE public_id = ?")
        .bind(&run.public_id)
        .execute(&mut *fixture.writer.lock().await)
        .await
        .unwrap();
    let bundles = runs.0.state_dir.join("ci/bundles");
    std::fs::create_dir_all(&bundles).unwrap();
    std::fs::write(
        bundles.join(format!("{}.bundle", run.public_id)),
        b"retained",
    )
    .unwrap();
    let _uploads: Vec<_> = (0..WORKTREE_RUNS)
        .map(|_| runs.0.uploads.take(BOB, WORKTREE_RUNS).unwrap())
        .collect();
    assert_eq!(
        refusal(rerun_run(&runs, BOB, &run.public_id).await.unwrap_err()),
        (Code::ResourceExhausted, "WORKTREE_RUNS_LIMIT".into())
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM run")
        .fetch_one(runs.readers().unwrap())
        .await
        .unwrap();
    assert_eq!(count, 1);
    assert_eq!(std::fs::read_dir(bundles).unwrap().count(), 1);
}
