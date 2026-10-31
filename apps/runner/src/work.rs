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
}

/// Consume the connection's jobs until the channel closes.
pub async fn serve<E: Executor>(worker: Worker<E>, mut jobs: mpsc::Receiver<Job>) {
    let worker = Arc::new(worker);
    let running: Arc<Mutex<BTreeMap<String, oneshot::Sender<()>>>> = Arc::default();
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
                if let Some(cancel) = running.lock().unwrap().remove(&id) {
                    let _ = cancel.send(());
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
    let (state, exit_code, reason) = match execute(worker, assignment, cancelled).await {
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
            failing_check: String::new(),
            reason,
        },
        &worker.metrics,
    );
}

/// Sources, then each step until one does not succeed; the scratch goes when this returns.
async fn execute<E: Executor>(
    worker: &Worker<E>,
    assignment: &Assignment,
    mut cancelled: oneshot::Receiver<()>,
) -> Result<Exit, String> {
    let id = assignment.attempt_id.as_str();
    let checkout = tokio::select! {
        checkout = worker.cache.prepare(assignment) => checkout?,
        _ = &mut cancelled => return Ok(Exit::Cancelled),
    };
    let (env, steps) = worker.executor.prepare(&kajiba_exec::Job {
        source: &checkout.dir,
        sha: &assignment.sha,
        system: &assignment.system,
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
        let (log, mut output) = mpsc::channel(4);
        let forward = async {
            while let Some(data) = output.recv().await {
                worker.attempts.log(id, index as u32, data);
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
    worker.executor.cleanup(env);
    exit
}

#[cfg(test)]
mod tests;
