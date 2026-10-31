use super::*;
use crate::transport::{
    auth::tests::{CI, READ, reason, send, tokens},
    repos::tests::{Fixture, git},
    runners::Declared,
};
use kajiba_proto::v1::Scope;
use prost::Message;
use std::sync::Arc;
use tonic::Code;

/// `alice` owns `ci` (CI on, `bob` writes) and `off` (CI off); `bob` owns `zeta`.
const REPOS: &str = "[repos.ci]\nowner='alice'\ndescription=''\nci_enabled=true\n\
     permissions.bob='write'\n[repos.off]\nowner='alice'\ndescription=''\n\
     [repos.zeta]\nowner='bob'\ndescription=''\nci_enabled=true\n";
const ALICE: i64 = 1;
const BOB: i64 = 2;

async fn fixture(extra: &str) -> (Fixture, Runs) {
    let fixture = Fixture::new(&format!("{REPOS}{extra}")).await;
    fixture.repos.0.set_runners(Declared {
        runners: Default::default(),
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
