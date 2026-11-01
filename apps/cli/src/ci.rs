//! `kajiba ci run|status|watch|logs|list` (0007, 0010, ADRs 0117, 0131): trigger, follow,
//! read and list runs.
use crate::client::{self, Api, Error};
use crate::repo::{pretty, protojson};
use crate::worktree::{self, Local};
use clap::Subcommand;
use kajiba_proto::v1::{
    GetRunRequest, ListRunsRequest, Run, RunState, RunTrigger, TriggerRunRequest, WatchRunRequest,
    run_service_client::RunServiceClient, watch_run_response::Event,
};
use std::{io::Write, process::ExitCode, time::Duration};
use tonic::Code;

#[derive(Subcommand)]
pub enum Command {
    /// Run CI on a commit, or on the working tree with --worktree, and follow its log.
    Run {
        /// `<owner>/<name>` or a public id.
        #[arg(required_unless_present = "worktree")]
        repo: Option<String>,
        /// The full commit id, reachable from a ref of the repository.
        #[arg(required_unless_present = "worktree")]
        sha: Option<String>,
        /// Print the run id and return at once.
        #[arg(short, long)]
        detach: bool,
        /// Run on the tracked files of this clone's working tree and index, sent to the
        /// forge without a push (0097).
        #[arg(long, conflicts_with_all = ["repo", "sha"])]
        worktree: bool,
        /// With --worktree: the repository, instead of the one this clone's remote
        /// addresses on the forge.
        #[arg(
            long = "repo",
            id = "worktree_repo",
            value_name = "OWNER/NAME",
            requires = "worktree"
        )]
        worktree_repo: Option<String>,
        /// With --worktree: build only `checks.<system>.<NAME>` (repeatable) instead of
        /// the whole `nix flake check`.
        #[arg(long = "check", value_name = "NAME", requires = "worktree")]
        checks: Vec<String>,
        /// With --worktree: the Nix system to run on [default: this machine's].
        #[arg(long, requires = "worktree")]
        system: Option<String>,
    },
    /// Print a run's state without waiting: exit 0 succeeded, 1 failed or cancelled,
    /// 75 pending or running.
    Status { run: String },
    /// Follow a run's log until it ends.
    Watch { run: String },
    /// Print a run's stored log.
    Logs { run: String },
    /// List a repository's recent runs, newest first: id, state, trigger, commit, ref and
    /// creation time.
    List {
        /// The repository (`<owner>/<name>` or a public id) instead of the one this
        /// clone's remote addresses on the forge.
        #[arg(long, value_name = "OWNER/NAME")]
        repo: Option<String>,
        /// Only runs in this state: queued, running, succeeded, failed or cancelled
        /// (repeatable, or comma-separated).
        #[arg(long = "state", value_name = "STATE", value_delimiter = ',', value_parser = run_state)]
        states: Vec<RunState>,
        /// At most this many runs.
        #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u32).range(1..))]
        limit: u32,
    },
}

/// `failed` → `RunState::Failed`.
fn run_state(name: &str) -> Result<RunState, String> {
    RunState::from_str_name(&format!("RUN_STATE_{}", name.to_uppercase()))
        .filter(|state| *state != RunState::Unspecified)
        .ok_or_else(|| "expected queued, running, succeeded, failed or cancelled".into())
}

/// `EX_TEMPFAIL`: the run has not ended yet.
const PENDING: u8 = 75;
/// A dropped stream is resumed after this delay, at most `RETRIES` times in a row
/// without new bytes (ADRs 0117, 0130).
const RETRY_DELAY: Duration = Duration::from_secs(1);
const RETRIES: u32 = 30;

/// The log goes to `out`; run ids, pending reasons, attempt markers and failure
/// summaries go to `err`, whose write errors are ignored (ADR 0116).
pub async fn run(
    command: Command,
    api: Api,
    local: &Local,
    json: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<ExitCode, Error> {
    let mut runs = RunServiceClient::new(api);
    match command {
        Command::Run {
            repo,
            sha,
            detach,
            worktree,
            worktree_repo,
            checks,
            system,
        } => {
            if !worktree && (worktree_repo.is_some() || !checks.is_empty() || system.is_some()) {
                return Err(Error("--repo, --check and --system need --worktree".into()));
            }
            let run = match (repo, sha) {
                (Some(repo), Some(sha)) if !worktree => {
                    client::answer(runs.trigger_run(TriggerRunRequest { repo, sha }).await)?
                        .run
                        .ok_or_else(|| Error("the forge answered no run".into()))?
                }
                _ => {
                    let choice = worktree::Choice {
                        repo: worktree_repo,
                        checks,
                        system,
                    };
                    worktree::create(&mut runs, local, choice).await?
                }
            };
            if detach {
                let text = format!("{}\n", run.public_id);
                print(out, &describe(&run, json, text)?)?;
                return Ok(ExitCode::SUCCESS);
            }
            let _ = writeln!(err, "kajiba: run {}", run.public_id);
            follow(&mut runs, &run.public_id, true, out, err).await
        }
        Command::Status { run } => {
            let run = get(&mut runs, run).await?;
            let name = state(&run).as_str_name();
            let text = format!(
                "{}\n",
                name.strip_prefix("RUN_STATE_")
                    .unwrap_or(name)
                    .to_lowercase()
            );
            print(out, &describe(&run, json, text)?)?;
            explain(&run, err);
            Ok(match state(&run) {
                RunState::Succeeded => ExitCode::SUCCESS,
                RunState::Failed | RunState::Cancelled => ExitCode::FAILURE,
                _ => ExitCode::from(PENDING),
            })
        }
        Command::Watch { run } => {
            let run = get(&mut runs, run).await?;
            if state(&run) == RunState::Queued {
                explain(&run, err);
            }
            follow(&mut runs, &run.public_id, true, out, err).await
        }
        Command::Logs { run } => follow(&mut runs, &run, false, out, err).await,
        Command::List {
            repo,
            states,
            limit,
        } => {
            let repo = match repo {
                Some(repo) => repo,
                None => worktree::repository(&local.dir, &local.forge)?,
            };
            let (mut found, mut page_token) = (Vec::new(), String::new());
            loop {
                let page = client::answer(
                    runs.list_runs(ListRunsRequest {
                        repo: repo.clone(),
                        page_size: (limit as usize - found.len()).min(200) as i32,
                        page_token,
                        states: states.iter().map(|&state| state.into()).collect(),
                        ..Default::default()
                    })
                    .await,
                )?;
                found.extend(page.runs);
                if page.next_page_token.is_empty() || found.len() >= limit as usize {
                    break;
                }
                page_token = page.next_page_token;
            }
            found.truncate(limit as usize);
            let text = if json {
                let list = found
                    .iter()
                    .map(|run| protojson("kajiba.v1.Run", run))
                    .collect::<Result<Vec<_>, _>>()?;
                pretty(serde_json::to_string_pretty(&list))?
            } else {
                found.iter().map(row).collect::<Result<String, _>>()?
            };
            print(out, &text)?;
            Ok(ExitCode::SUCCESS)
        }
    }
}

/// `<id>  <state>  <trigger>  <short sha>  <ref or ->  <created, RFC 3339>`.
fn row(run: &Run) -> Result<String, Error> {
    let short = |name: &str, prefix: &str| name.strip_prefix(prefix).unwrap_or(name).to_lowercase();
    let trigger = RunTrigger::try_from(run.trigger).unwrap_or_default();
    let created = match &run.created_at {
        Some(time) => serde_json::to_string(&protojson("google.protobuf.Timestamp", time)?)
            .map_err(|err| Error(format!("protojson: {err}")))?
            .trim_matches('"')
            .to_owned(),
        None => "-".into(),
    };
    Ok(format!(
        "{}  {:<9}  {:<8}  {:.12}  {}  {created}\n",
        run.public_id,
        short(state(run).as_str_name(), "RUN_STATE_"),
        short(trigger.as_str_name(), "RUN_TRIGGER_"),
        run.sha,
        if trigger == RunTrigger::Worktree {
            format!("worktree:{}", run.triggered_by)
        } else {
            match run.r#ref.as_str() {
                "" => "-",
                reference => reference,
            }
            .to_owned()
        },
    ))
}

async fn get(runs: &mut RunServiceClient<Api>, run: String) -> Result<Run, Error> {
    client::answer(runs.get_run(GetRunRequest { run }).await)?
        .run
        .ok_or_else(|| Error("the forge answered no run".into()))
}

fn state(run: &Run) -> RunState {
    RunState::try_from(run.state).unwrap_or_default()
}

/// `text`, or the run as protojson.
fn describe(run: &Run, json: bool, text: String) -> Result<String, Error> {
    if json {
        pretty(serde_json::to_string_pretty(&protojson(
            "kajiba.v1.Run",
            run,
        )?))
    } else {
        Ok(text)
    }
}

fn print(out: &mut dyn Write, text: &str) -> Result<(), Error> {
    out.write_all(text.as_bytes())
        .map_err(|err| Error(format!("stdout: {err}")))
}

/// On stderr: why a run waits, or why it failed, from the run itself (no extra request).
fn explain(run: &Run, err: &mut dyn Write) {
    let id = &run.public_id;
    let _ = match state(run) {
        RunState::Failed => {
            let failure = run.failure.clone().unwrap_or_default();
            let check = match failure.check.as_str() {
                "" => String::new(),
                check => format!(" in check {check}"),
            };
            let mut text = format!("kajiba: run {id} failed{check}: {}\n", failure.reason);
            for line in &failure.last_log_lines {
                text += &format!("{line}\n");
            }
            err.write_all(text.as_bytes())
        }
        RunState::Cancelled => writeln!(err, "kajiba: run {id} cancelled"),
        _ => match run.job.as_ref().map(|job| job.pending_reason.as_str()) {
            Some(reason) if !reason.is_empty() => {
                writeln!(err, "kajiba: run {id} pending: {reason}")
            }
            _ => Ok(()),
        },
    };
}

/// The run's log from the start, resumed from the received offset when the stream
/// drops; with `live`, until the run ends, whose state gives the exit status.
async fn follow(
    runs: &mut RunServiceClient<Api>,
    run: &str,
    live: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<ExitCode, Error> {
    let (mut offset, mut marked, mut retries) = (0, String::new(), 0);
    loop {
        let dropped = match runs
            .watch_run(WatchRunRequest {
                run: run.into(),
                offset,
                follow: live,
            })
            .await
        {
            Ok(response) => {
                let mut stream = response.into_inner();
                loop {
                    match stream.message().await {
                        Ok(Some(message)) => {
                            match message.event {
                                Some(Event::Section(section)) => {
                                    let attempt = section.attempt.unwrap_or_default();
                                    if attempt.public_id != marked {
                                        marked = attempt.public_id;
                                        let host = match attempt.runner_hostname.as_str() {
                                            "" => String::new(),
                                            host => format!(" ({host})"),
                                        };
                                        let _ = writeln!(
                                            err,
                                            "kajiba: attempt {} on {}{host}",
                                            attempt.number, attempt.runner
                                        );
                                    }
                                }
                                Some(Event::Log(log))
                                    if log.offset + log.data.len() as u64 > offset =>
                                {
                                    // Skip what an earlier stream already delivered.
                                    let skip = offset.saturating_sub(log.offset) as usize;
                                    out.write_all(&log.data[skip..])
                                        .and_then(|()| out.flush())
                                        .map_err(|err| Error(format!("stdout: {err}")))?;
                                    offset = log.offset + log.data.len() as u64;
                                    retries = 0;
                                }
                                Some(Event::Finished(_)) if !live => {
                                    return Ok(ExitCode::SUCCESS);
                                }
                                Some(Event::Finished(run)) => {
                                    explain(&run, err);
                                    return Ok(match state(&run) {
                                        RunState::Succeeded => ExitCode::SUCCESS,
                                        _ => ExitCode::FAILURE,
                                    });
                                }
                                _ => {}
                            }
                        }
                        Ok(None) if !live => return Ok(ExitCode::SUCCESS),
                        Ok(None) => break None,
                        Err(status) => break Some(status),
                    }
                }
            }
            Err(status) => Some(status),
        };
        if let Some(status) = dropped.filter(|status| !live || !resumable(status)) {
            return Err(status.into());
        }
        retries += 1;
        if retries > RETRIES {
            return Err(Error(format!(
                "the log stream of run {run} dropped {RETRIES} times in a row"
            )));
        }
        tokio::time::sleep(RETRY_DELAY).await;
    }
}

/// A broken transport or an intermediary's answer, rather than the forge's own: the
/// forge refuses with one of the excluded codes (ADR 0130). An error raised by the
/// client's transport (an h2 reset, a closed connection) always resumes, whatever the
/// code it maps to.
fn resumable(status: &tonic::Status) -> bool {
    std::error::Error::source(status).is_some()
        || !matches!(
            status.code(),
            Code::Ok
                | Code::InvalidArgument
                | Code::NotFound
                | Code::AlreadyExists
                | Code::PermissionDenied
                | Code::ResourceExhausted
                | Code::FailedPrecondition
                | Code::OutOfRange
                | Code::Unimplemented
                | Code::Unauthenticated
        )
}

#[cfg(test)]
mod tests;
