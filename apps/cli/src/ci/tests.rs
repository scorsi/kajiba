use super::*;
use crate::client::{
    connect,
    tests::{Fake, TOKEN, refusal},
};
use kajiba_proto::v1::{Attempt, FailureSummary, Job, RunTrigger};
use tonic::Status;

const ID: &str = "01JBRUN0000000000000000000";
const SHA: &str = "1111111111111111111111111111111111111111";
const LOG: &[u8] = b"building\nchecks passed\n";

fn run_in(state: RunState) -> Run {
    Run {
        public_id: ID.into(),
        repo_public_id: "01JBDEMO000000000000000000".into(),
        sha: SHA.into(),
        state: state.into(),
        ..Default::default()
    }
}

fn failed(check: &str) -> Run {
    Run {
        failure: Some(FailureSummary {
            check: check.into(),
            reason: "exit status 1".into(),
            last_log_lines: vec!["error: build failed".into(), "1 check failed".into()],
        }),
        ..run_in(RunState::Failed)
    }
}

/// A fake whose run is queued, then ends as `finished` after `LOG`.
async fn fake(finished: Run) -> (Fake, String) {
    let fake = Fake::default();
    {
        let mut state = fake.0.lock().unwrap();
        state.run = Some(run_in(RunState::Queued));
        state.attempt = Attempt {
            public_id: "01JBATTEMPT000000000000000".into(),
            number: 1,
            runner: "build-1".into(),
            runner_hostname: "host-1".into(),
            ..Default::default()
        };
        state.log = LOG.into();
        state.finished = Some(finished);
    }
    let url = fake.serve().await;
    (fake, url)
}

/// No clone: commands on a pushed commit never read it.
fn nowhere() -> Local {
    Local {
        forge: "https://forge.example.org".into(),
        nix: "/nonexistent/nix".into(),
        dir: "/nonexistent".into(),
    }
}

/// Exit code, stdout, stderr.
async fn output(url: &str, command: Command, json: bool) -> (ExitCode, String, String) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = match run(
        command,
        connect(url, TOKEN, true).unwrap(),
        &nowhere(),
        json,
        &mut out,
        &mut err,
    )
    .await
    {
        Ok(code) => code,
        Err(error) => {
            err.extend(format!("kajiba: {error}\n").bytes());
            ExitCode::FAILURE
        }
    };
    (
        code,
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    )
}

fn trigger(detach: bool) -> Command {
    Command::Run {
        repo: Some("alice/demo".into()),
        sha: Some(SHA.into()),
        detach,
        worktree: false,
        worktree_repo: None,
        checks: Vec::new(),
        system: None,
    }
}

const MARK: &str = "kajiba: attempt 1 on build-1 (host-1)\n";

#[tokio::test]
async fn run_follows_to_success() {
    let (fake, url) = fake(run_in(RunState::Succeeded)).await;
    let (code, out, err) = output(&url, trigger(false), false).await;
    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(out.as_bytes(), LOG);
    assert_eq!(err, format!("kajiba: run {ID}\n{MARK}"));
    assert_eq!(fake.0.lock().unwrap().watches, [(0, true)]);
}

#[tokio::test]
async fn run_follows_to_failure_and_prints_the_summary() {
    for (check, header) in [
        ("", "failed: exit status 1"),
        ("fmt", "failed in check fmt: exit status 1"),
    ] {
        let (_, url) = fake(failed(check)).await;
        let (code, out, err) = output(&url, trigger(false), false).await;
        assert_eq!(code, ExitCode::FAILURE);
        assert_eq!(out.as_bytes(), LOG);
        assert_eq!(
            err,
            format!(
                "kajiba: run {ID}\n{MARK}kajiba: run {ID} {header}\nerror: build failed\n1 check failed\n"
            )
        );
    }
    let (_, url) = fake(run_in(RunState::Cancelled)).await;
    let (code, _, err) = output(&url, trigger(false), false).await;
    assert_eq!(code, ExitCode::FAILURE);
    assert!(
        err.ends_with(&format!("kajiba: run {ID} cancelled\n")),
        "{err}"
    );
}

#[tokio::test]
async fn detach_prints_the_run_id_and_returns() {
    let (fake, url) = fake(run_in(RunState::Succeeded)).await;
    assert_eq!(
        output(&url, trigger(true), false).await,
        (ExitCode::SUCCESS, format!("{ID}\n"), String::new())
    );
    let (code, out, _) = output(&url, trigger(true), true).await;
    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(
        out,
        format!(
            "{{\n  \"publicId\": \"{ID}\",\n  \"repoPublicId\": \"01JBDEMO000000000000000000\",\n  \"sha\": \"{SHA}\",\n  \"state\": \"RUN_STATE_QUEUED\"\n}}\n"
        )
    );
    assert!(fake.0.lock().unwrap().watches.is_empty());
}

#[tokio::test]
async fn status_does_not_wait_and_exits_by_state() {
    let (fake, url) = fake(run_in(RunState::Succeeded)).await;
    let status = || Command::Status { run: ID.into() };
    let pending = Run {
        job: Some(Job {
            pending_reason: "no-connected-runner".into(),
            ..Default::default()
        }),
        ..run_in(RunState::Queued)
    };
    for (run, code, out, err) in [
        (
            pending,
            ExitCode::from(75),
            "queued\n",
            format!("kajiba: run {ID} pending: no-connected-runner\n"),
        ),
        (
            run_in(RunState::Running),
            ExitCode::from(75),
            "running\n",
            String::new(),
        ),
        (
            run_in(RunState::Succeeded),
            ExitCode::SUCCESS,
            "succeeded\n",
            String::new(),
        ),
        (
            failed(""),
            ExitCode::FAILURE,
            "failed\n",
            format!(
                "kajiba: run {ID} failed: exit status 1\nerror: build failed\n1 check failed\n"
            ),
        ),
        (
            run_in(RunState::Cancelled),
            ExitCode::FAILURE,
            "cancelled\n",
            format!("kajiba: run {ID} cancelled\n"),
        ),
    ] {
        fake.0.lock().unwrap().run = Some(run);
        assert_eq!(output(&url, status(), false).await, (code, out.into(), err));
    }
    let (_, json, _) = output(&url, status(), true).await;
    assert!(
        json.contains("\"state\": \"RUN_STATE_CANCELLED\""),
        "{json}"
    );
    assert!(fake.0.lock().unwrap().watches.is_empty());
}

#[tokio::test]
async fn watch_prints_the_pending_reason_then_follows() {
    let (fake, url) = fake(failed("")).await;
    fake.0.lock().unwrap().run = Some(Run {
        job: Some(Job {
            pending_reason: "runners-busy".into(),
            ..Default::default()
        }),
        ..run_in(RunState::Queued)
    });
    let (code, out, err) = output(&url, Command::Watch { run: ID.into() }, false).await;
    assert_eq!(code, ExitCode::FAILURE);
    assert_eq!(out.as_bytes(), LOG);
    assert!(
        err.starts_with(&format!("kajiba: run {ID} pending: runners-busy\n{MARK}")),
        "{err}"
    );
}

#[tokio::test]
async fn watching_a_terminal_run_prints_its_outcome_once() {
    for (run, summary) in [
        (
            failed("fmt"),
            format!(
                "kajiba: run {ID} failed in check fmt: exit status 1\nerror: build failed\n1 check failed\n"
            ),
        ),
        (
            run_in(RunState::Cancelled),
            format!("kajiba: run {ID} cancelled\n"),
        ),
    ] {
        let (fake, url) = fake(run.clone()).await;
        fake.0.lock().unwrap().run = Some(run);
        let (code, out, err) = output(&url, Command::Watch { run: ID.into() }, false).await;
        assert_eq!(code, ExitCode::FAILURE);
        assert_eq!(out.as_bytes(), LOG);
        assert_eq!(err, format!("{MARK}{summary}"));
        assert_eq!(fake.0.lock().unwrap().watches, [(0, true)]);
    }
}

#[tokio::test]
async fn an_interrupted_stream_resumes_from_the_received_offset() {
    let (fake, url) = fake(run_in(RunState::Succeeded)).await;
    fake.0.lock().unwrap().drops = vec![(10, Some(Status::unavailable("dropped")))];
    let (code, out, err) = output(&url, Command::Watch { run: ID.into() }, false).await;
    assert_eq!(code, ExitCode::SUCCESS);
    // Neither lost nor duplicated, and the attempt marked once.
    assert_eq!(out.as_bytes(), LOG);
    assert_eq!(err, MARK);
    assert_eq!(fake.0.lock().unwrap().watches, [(0, true), (10, true)]);
}

#[tokio::test]
async fn every_transport_break_resumes_and_forge_refusals_end() {
    let (fake, url) = fake(run_in(RunState::Succeeded)).await;
    // A broken connection, then what proxies and h2 resets surface as.
    fake.0.lock().unwrap().drops = vec![
        (4, None),
        (
            8,
            Some(Status::internal(
                "h2 protocol error: error reading a body from connection",
            )),
        ),
        (12, Some(Status::cancelled("upstream reset"))),
        (16, Some(Status::unknown("stream error"))),
        (20, Some(Status::deadline_exceeded("timeout"))),
    ];
    for command in [trigger(false), Command::Watch { run: ID.into() }] {
        let (code, out, _) = output(&url, command, false).await;
        assert_eq!(code, ExitCode::SUCCESS);
        assert_eq!(out.as_bytes(), LOG, "nothing lost nor duplicated");
    }
    let watches = std::mem::take(&mut fake.0.lock().unwrap().watches);
    assert_eq!(
        watches,
        [0, 4, 8, 12, 16, 20, 0].map(|offset| (offset, true)),
        "the run resumed at each break, the watch went through"
    );
    for (code, reason) in [
        (Code::NotFound, "RUN_NOT_FOUND"),
        (Code::PermissionDenied, "ROLE_MISSING"),
        (Code::Unauthenticated, "CREDENTIAL_EXPIRED"),
        (Code::InvalidArgument, "INVALID_FIELD"),
        (Code::ResourceExhausted, "WATCH_STREAMS_LIMIT"),
    ] {
        fake.0.lock().unwrap().drops = vec![(4, Some(refusal(code, reason, "refused")))];
        let (exit, out, err) = output(&url, trigger(false), false).await;
        assert_eq!(exit, ExitCode::FAILURE);
        assert_eq!(out, "buil");
        assert!(
            err.ends_with(&format!(
                "({reason}, request 01JBREQUEST0000000000000000)\n"
            )),
            "{err}"
        );
    }
    assert_eq!(fake.0.lock().unwrap().watches.len(), 5, "never resumed");
}

#[tokio::test]
async fn logs_prints_the_stored_log_without_following() {
    let (fake, url) = fake(failed("")).await;
    assert_eq!(
        output(&url, Command::Logs { run: ID.into() }, false).await,
        (
            ExitCode::SUCCESS,
            String::from_utf8(LOG.into()).unwrap(),
            MARK.into()
        )
    );
    assert_eq!(fake.0.lock().unwrap().watches, [(0, false)]);
}

#[tokio::test]
async fn refusals_name_their_reason() {
    let (fake, url) = fake(run_in(RunState::Succeeded)).await;
    for (reason, message) in [
        ("ROLE_MISSING", "triggering a run needs the write role"),
        (
            "REVISION_NOT_FOUND",
            "no commit by this id is reachable from a ref",
        ),
    ] {
        fake.0.lock().unwrap().refusal = Some(refusal(Code::PermissionDenied, reason, message));
        assert_eq!(
            output(&url, trigger(false), false).await,
            (
                ExitCode::FAILURE,
                String::new(),
                format!("kajiba: {message} ({reason}, request 01JBREQUEST0000000000000000)\n")
            )
        );
    }
    fake.0.lock().unwrap().refusal = None;
    let (code, _, err) = output(
        &url,
        Command::Status {
            run: "01JBOTHER".into(),
        },
        false,
    )
    .await;
    assert_eq!(code, ExitCode::FAILURE);
    assert!(err.contains("(RUN_NOT_FOUND, "), "{err}");
}

#[tokio::test]
async fn logs_into_a_closed_stdout_exits_quietly() {
    const CHILD: &str = "KAJIBA_CLOSED_LOGS_TEST";
    if std::env::var_os(CHILD).is_some() {
        let mut ends = [0; 2];
        // SAFETY: plain descriptor calls on descriptors this process owns.
        unsafe {
            assert_eq!(libc::pipe(ends.as_mut_ptr()), 0);
            libc::close(ends[0]);
            libc::dup2(ends[1], 1);
            libc::close(ends[1]);
        }
        let (_fake, url) = fake(run_in(RunState::Succeeded)).await;
        let result = run(
            Command::Logs { run: ID.into() },
            connect(&url, TOKEN, true).unwrap(),
            &nowhere(),
            false,
            &mut crate::Stdout,
            &mut std::io::sink(),
        )
        .await;
        panic!("still running after writing to a closed stdout: {result:?}");
    }
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "ci::tests::logs_into_a_closed_stdout_exits_quietly",
        ])
        .env(CHILD, "1")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr
            .lines()
            .all(|line| line.starts_with("kajiba: warning: development mode")),
        "{stderr}"
    );
}

#[tokio::test]
async fn list_prints_recent_runs_filtered_and_bounded() {
    let (fake, url) = fake(run_in(RunState::Succeeded)).await;
    let run = |id: u8, state: RunState, trigger: RunTrigger, reference: &str| Run {
        public_id: format!("01JBRUN00000000000000000{id:02}"),
        r#ref: reference.into(),
        trigger: trigger.into(),
        created_at: Some(kajiba_proto::google::protobuf::Timestamp {
            seconds: 1_800_000_000 + i64::from(id),
            nanos: 0,
        }),
        ..run_in(state)
    };
    fake.0.lock().unwrap().runs = vec![
        run(3, RunState::Running, RunTrigger::Push, "refs/heads/main"),
        run(2, RunState::Failed, RunTrigger::Manual, ""),
        run(1, RunState::Succeeded, RunTrigger::Push, "refs/tags/v1"),
    ];
    let list = |states: Vec<RunState>, limit| Command::List {
        repo: Some("alice/demo".into()),
        states,
        limit,
    };
    let (code, out, _) = output(&url, list(vec![], 20), false).await;
    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(
        out,
        "01JBRUN0000000000000000003  running    push      111111111111  refs/heads/main  2027-01-15T08:00:03Z\n\
         01JBRUN0000000000000000002  failed     manual    111111111111  -  2027-01-15T08:00:02Z\n\
         01JBRUN0000000000000000001  succeeded  push      111111111111  refs/tags/v1  2027-01-15T08:00:01Z\n"
    );
    // The limit bounds the page asked for.
    let (_, out, _) = output(&url, list(vec![], 2), false).await;
    assert_eq!(out.lines().count(), 2);
    let (_, out, _) = output(
        &url,
        list(vec![RunState::Failed, RunState::Succeeded], 20),
        true,
    )
    .await;
    let json: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(json[0]["publicId"], "01JBRUN0000000000000000002");
    assert_eq!(json[1]["state"], "RUN_STATE_SUCCEEDED");
    assert_eq!(json.as_array().unwrap().len(), 2);
    let listed = std::mem::take(&mut fake.0.lock().unwrap().listed);
    let asked = listed
        .iter()
        .map(|request| {
            (
                request.repo.as_str(),
                request.page_size,
                request.states.len(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        asked,
        [
            ("alice/demo", 20, 0),
            ("alice/demo", 2, 0),
            ("alice/demo", 20, 2)
        ]
    );
}

#[test]
fn list_parses_states_repeated_or_comma_separated() {
    use clap::Parser;
    #[derive(Parser)]
    struct Cli {
        #[command(subcommand)]
        command: Command,
    }
    let Command::List {
        states,
        limit,
        repo,
    } = Cli::try_parse_from([
        "kajiba",
        "list",
        "--state",
        "failed,running",
        "--state",
        "queued",
    ])
    .unwrap()
    .command
    else {
        panic!("list");
    };
    assert_eq!(
        (states, limit, repo),
        (
            vec![RunState::Failed, RunState::Running, RunState::Queued],
            20,
            None
        )
    );
    assert!(Cli::try_parse_from(["kajiba", "list", "--state", "done"]).is_err());
    assert!(Cli::try_parse_from(["kajiba", "list", "--limit", "0"]).is_err());
}
