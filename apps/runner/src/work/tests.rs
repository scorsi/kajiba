use super::*;
use crate::cache::tests::{Setup, setup};
use kajiba_proto::runner::v1::ConnectRequest;
use std::{future::Future, path::PathBuf, time::Duration};

/// Commit `n` of the fixture holds `file` = `n`: 1 succeeds, 2 fails with exit 2, 3 runs
/// until cancelled.
struct Fake;

impl Executor for Fake {
    type Env = PathBuf;

    fn prepare(&self, job: &kajiba_exec::Job) -> Result<(PathBuf, Vec<String>), String> {
        Ok((job.source.to_owned(), vec!["fake".into()]))
    }

    async fn run(
        &self,
        source: &PathBuf,
        _: usize,
        log: mpsc::Sender<Vec<u8>>,
        cancel: impl Future<Output = ()> + Send,
    ) -> Result<Exit, String> {
        let file = std::fs::read_to_string(source.join("file")).unwrap();
        log.send(format!("file={file}\n").into_bytes())
            .await
            .unwrap();
        match file.as_str() {
            "1" => Ok(Exit::Code(0)),
            "2" => Ok(Exit::Code(2)),
            _ => {
                cancel.await;
                Ok(Exit::Cancelled)
            }
        }
    }

    fn cleanup(&self, _: PathBuf) {}
}

struct Running {
    setup: Setup,
    jobs: mpsc::Sender<Job>,
    sent: mpsc::UnboundedReceiver<ConnectRequest>,
    metrics: Arc<Metrics>,
}

async fn start(name: &str, max_jobs: usize) -> Running {
    let setup = setup(name, 3).await;
    let attempts = Attempts::default();
    let (outbound, sent) = mpsc::unbounded_channel();
    attempts.attach(Some(outbound));
    let metrics = Arc::new(Metrics::default());
    let (jobs, assigned) = mpsc::channel(4);
    tokio::spawn(serve(
        Worker {
            attempts,
            cache: setup.cache.clone(),
            executor: Fake,
            metrics: metrics.clone(),
            max_jobs,
        },
        assigned,
    ));
    Running {
        setup,
        jobs,
        sent,
        metrics,
    }
}

impl Running {
    async fn assign(&self, n: usize, index: usize) -> String {
        let assignment = self.setup.assignment(n, index);
        let id = assignment.attempt_id.clone();
        self.jobs.send(Job::Assigned(assignment)).await.unwrap();
        id
    }

    async fn next(&mut self) -> Message {
        tokio::time::timeout(Duration::from_secs(10), self.sent.recv())
            .await
            .unwrap()
            .unwrap()
            .message
            .unwrap()
    }

    fn step(id: &str, state: StepState, exit_code: Option<i32>) -> Message {
        Message::StepStatus(StepStatus {
            attempt_id: id.into(),
            step_index: 0,
            name: "fake".into(),
            state: state.into(),
            exit_code,
        })
    }

    fn log(id: &str, sequence: u64, data: &str) -> Message {
        Message::LogChunk(kajiba_proto::runner::v1::LogChunk {
            attempt_id: id.into(),
            sequence,
            step_index: 0,
            data: data.into(),
        })
    }
}

#[tokio::test]
async fn an_attempt_is_accepted_executed_and_its_result_sent() {
    let mut running = start("work-results", 2).await;
    let id = running.assign(1, 0).await;
    let mut sent = Vec::new();
    for _ in 0..5 {
        sent.push(running.next().await);
    }
    assert_eq!(
        sent,
        [
            Message::Accept(kajiba_proto::runner::v1::Accept {
                attempt_id: id.clone()
            }),
            Running::step(&id, StepState::Running, None),
            Running::log(&id, 1, "file=1\n"),
            Running::step(&id, StepState::Succeeded, Some(0)),
            Message::Result(RunResult {
                attempt_id: id.clone(),
                state: ResultState::Succeeded.into(),
                exit_code: Some(0),
                failing_check: String::new(),
                reason: String::new(),
            }),
        ]
    );
    let id = running.assign(2, 1).await;
    let mut last = running.next().await;
    while !matches!(last, Message::Result(_)) {
        last = running.next().await;
    }
    assert_eq!(
        last,
        Message::Result(RunResult {
            attempt_id: id,
            state: ResultState::Failed.into(),
            exit_code: Some(2),
            failing_check: String::new(),
            reason: "exit status 2".into(),
        })
    );
    let exposition = running.metrics.exposition("0.1.0");
    assert!(exposition.contains("kajiba_runner_jobs_total{result=\"succeeded\"} 1\n"));
    assert!(exposition.contains("kajiba_runner_jobs_total{result=\"failed\"} 1\n"));
}

#[tokio::test]
async fn a_cancelled_attempt_stops_without_a_result_and_frees_its_slot() {
    let mut running = start("work-cancel", 1).await;
    let id = running.assign(1, 2).await;
    while !matches!(running.next().await, Message::LogChunk(_)) {}
    // At max_jobs: a second assignment is left unaccepted.
    running.assign(2, 0).await;
    let scratch = running.setup.dir.join("state/scratch").join(&id);
    assert!(scratch.exists());
    running.jobs.send(Job::Cancelled(id)).await.unwrap();
    for _ in 0..100 {
        if !scratch.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(!scratch.exists());
    assert!(running.sent.try_recv().is_err(), "no result, no acceptance");
    assert!(
        running
            .metrics
            .exposition("0.1.0")
            .contains("kajiba_runner_jobs_total{result=\"cancelled\"} 1\n")
    );
    // The slot is free again.
    let id = running.assign(3, 0).await;
    assert_eq!(
        running.next().await,
        Message::Accept(kajiba_proto::runner::v1::Accept { attempt_id: id })
    );
}

#[tokio::test]
async fn a_source_failure_fails_the_attempt_with_its_reason() {
    let mut running = start("work-source", 1).await;
    let mut assignment = running.setup.assignment(1, 0);
    assignment.sha = "f".repeat(40);
    let id = assignment.attempt_id.clone();
    running.jobs.send(Job::Assigned(assignment)).await.unwrap();
    assert!(matches!(running.next().await, Message::Accept(_)));
    let Message::Result(result) = running.next().await else {
        panic!("result expected");
    };
    assert_eq!(
        (result.state(), result.attempt_id, result.exit_code),
        (ResultState::Failed, id, None)
    );
    assert!(!result.reason.is_empty());
}
