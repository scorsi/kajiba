//! The job side (0008, 0009): accepts assignments within `max_jobs`, prepares their
//! sources, runs the backend's steps and sends the result.

use crate::{
    cache::Cache,
    metrics::Metrics,
    session::{Attempts, Job},
};
use kajiba_exec::{Executor, Exit};
use kajiba_proto::runner::v1::{
    Assignment, Result as RunResult, ResultState, StepState, StepStatus, connect_request::Message,
};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use tokio::sync::{mpsc, oneshot};

pub struct Worker<E> {
    pub attempts: Attempts,
    pub cache: Arc<Cache>,
    pub executor: E,
    pub metrics: Arc<Metrics>,
    pub max_jobs: usize,
    pub state_dir: Option<std::path::PathBuf>,
}

/// Consume the connection's jobs until the channel closes.
pub async fn serve<E: Executor>(
    worker: Worker<E>,
    mut jobs: mpsc::Receiver<Job>,
    recovered: Vec<crate::durable::Shared>,
) {
    let worker = Arc::new(worker);
    let running: Arc<Mutex<BTreeMap<String, oneshot::Sender<()>>>> = Arc::default();
    for state in recovered {
        let id = state.lock().unwrap().attempt.clone();
        let (cancel, cancelled) = oneshot::channel();
        running.lock().unwrap().insert(id.clone(), cancel);
        let (worker, running) = (worker.clone(), running.clone());
        tokio::spawn(async move {
            let outcome = crate::unit::follow(state, &worker.attempts, cancelled).await;
            complete(&worker, &id, outcome);
            running.lock().unwrap().remove(&id);
        });
    }
    while let Some(job) = jobs.recv().await {
        match job {
            Job::Assigned(assignment) => {
                let id = assignment.attempt_id.clone();
                let (cancel, cancelled) = oneshot::channel();
                {
                    let mut running = running.lock().unwrap();
                    if running.contains_key(&id) {
                        continue;
                    }
                    if running.len() >= worker.max_jobs {
                        // Left unaccepted: its lease expires and the job is replayed (0008).
                        tracing::warn!(
                            event = "assignment-refused",
                            attempt = id,
                            reason = "max-jobs"
                        );
                        continue;
                    }
                    running.insert(id.clone(), cancel);
                }
                let (worker, running) = (worker.clone(), running.clone());
                tokio::spawn(async move {
                    attempt(&worker, &assignment, cancelled).await;
                    running.lock().unwrap().remove(&id);
                });
            }
            Job::Cancelled(id) => {
                let cancel = running.lock().unwrap().remove(&id);
                if let Some(cancel) = cancel {
                    let _ = cancel.send(());
                } else if let Some(root) = &worker.state_dir {
                    // A completed monitor may have exited before a stale attempt is cancelled.
                    if let Ok(state) = crate::durable::State::load(root, &id) {
                        if crate::unit::control("stop", &id).await.is_ok() {
                            state.remove();
                        }
                    }
                }
            }
        }
    }
}

async fn attempt<E: Executor>(
    worker: &Worker<E>,
    assignment: &Assignment,
    cancelled: oneshot::Receiver<()>,
) {
    let id = assignment.attempt_id.as_str();
    tracing::info!(
        event = "attempt-started",
        attempt = id,
        sha = assignment.sha
    );
    worker.attempts.accept(id);
    complete(worker, id, execute(worker, assignment, cancelled).await);
}

fn complete<E: Executor>(
    worker: &Worker<E>,
    id: &str,
    outcome: Result<(Exit, Option<String>), String>,
) {
    let (exit, failing_check) = match outcome {
        Ok((exit, check)) => (Ok(exit), check.unwrap_or_default()),
        Err(err) => (Err(err), String::new()),
    };
    let (state, exit_code, reason) = match exit {
        Ok(Exit::Code(0)) => (ResultState::Succeeded, Some(0), String::new()),
        Ok(Exit::Code(code)) => (
            ResultState::Failed,
            Some(code),
            format!("exit status {code}"),
        ),
        Ok(Exit::Signal(signal)) => (
            ResultState::Failed,
            None,
            format!("killed by signal {signal}"),
        ),
        Ok(Exit::Cancelled) => {
            // The server cancelled it and needs no result (0008).
            worker.metrics.job_finished(ResultState::Cancelled);
            tracing::info!(
                event = "attempt-finished",
                attempt = id,
                state = "cancelled"
            );
            return;
        }
        Err(err) => (ResultState::Failed, None, err),
    };
    tracing::info!(
        event = "attempt-finished",
        attempt = id,
        state = state.as_str_name(),
        reason
    );
    worker.attempts.finish(
        RunResult {
            attempt_id: id.into(),
            state: state.into(),
            exit_code,
            // ADR 0112: known only structurally, never parsed from the output.
            failing_check,
            reason,
        },
        &worker.metrics,
    );
}

/// Sources, then each step until one does not succeed, and the failing check when the
/// backend knows it; the scratch goes when this returns.
async fn execute<E: Executor>(
    worker: &Worker<E>,
    assignment: &Assignment,
    mut cancelled: oneshot::Receiver<()>,
) -> Result<(Exit, Option<String>), String> {
    let id = assignment.attempt_id.as_str();
    let mut checkout = tokio::select! {
        checkout = worker.cache.prepare(assignment) => checkout?,
        _ = &mut cancelled => return Ok((Exit::Cancelled, None)),
    };
    if let Some(root) = &worker.state_dir {
        let state = crate::durable::State::new(root, assignment, checkout.dir.clone())?;
        let state = Arc::new(Mutex::new(state));
        if !worker.attempts.bind(id, state.clone()) {
            state.lock().unwrap().remove();
            return Ok((Exit::Cancelled, None));
        }
        // The unit owns the checkout until cancellation or ResultAck, even if this task drops.
        checkout.preserved = true;
        if let Err(err) = crate::unit::control("start", id).await {
            // The broker may have started the unit before the connection failed; recover it.
            tracing::error!(event = "job-start-failed", err);
            std::process::exit(1);
        }
        return crate::unit::follow(state, &worker.attempts, cancelled).await;
    }
    let (env, steps) = worker.executor.prepare(&kajiba_exec::Job {
        source: &checkout.dir,
        sha: &assignment.sha,
        system: &assignment.system,
        checks: &assignment.checks,
    })?;
    let status = |index: usize, name: &str, state: StepState, exit_code| {
        worker.attempts.send(Message::StepStatus(StepStatus {
            attempt_id: id.into(),
            step_index: index as u32,
            name: name.into(),
            state: state.into(),
            exit_code,
        }));
    };
    let mut exit = Ok(Exit::Code(0));
    for (index, name) in steps.iter().enumerate() {
        status(index, name, StepState::Running, None);
        let (log, mut output) = mpsc::channel::<kajiba_exec::Log>(4);
        let forward = async {
            while let Some(data) = output.recv().await {
                worker
                    .attempts
                    .log(id, index as u32, data.data, data.produced_at_ms)
                    .await;
            }
        };
        let cancel = async {
            let _ = (&mut cancelled).await;
        };
        (exit, ()) = tokio::join!(worker.executor.run(&env, index, log, cancel), forward);
        match &exit {
            Ok(Exit::Code(0)) => status(index, name, StepState::Succeeded, Some(0)),
            Ok(Exit::Code(code)) => status(index, name, StepState::Failed, Some(*code)),
            Ok(Exit::Signal(_)) | Err(_) => status(index, name, StepState::Failed, None),
            Ok(Exit::Cancelled) => {}
        }
        if exit != Ok(Exit::Code(0)) {
            break;
        }
    }
    let check = worker.executor.failing_check(&env);
    worker.executor.cleanup(env);
    exit.map(|exit| (exit, check))
}

#[cfg(test)]
mod tests;
