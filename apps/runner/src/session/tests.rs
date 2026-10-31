use super::*;
use kajiba_proto::runner::v1::{
    CancelAttempt, ConnectResponse, ContinueAttempt, FetchRunBundleRequest, FetchRunBundleResponse,
    LogAck, ResultAck, ResultState,
    runner_service_server::{RunnerService, RunnerServiceServer},
};
use std::pin::Pin;
use tokio::sync::oneshot;
use tokio_stream::{Stream, wrappers::ReceiverStream};
use tonic::{Response, Streaming};

type Responses = mpsc::Receiver<Result<ConnectResponse, Status>>;

/// A connection as the fake server sees it; the test admits it by handing back the
/// stream of responses, or refuses it.
struct Offered {
    authorization: Option<String>,
    hello: Hello,
    requests: Streaming<ConnectRequest>,
    verdict: oneshot::Sender<Result<Responses, Status>>,
}

struct Fake(mpsc::Sender<Offered>);

#[tonic::async_trait]
impl RunnerService for Fake {
    type ConnectStream = ReceiverStream<Result<ConnectResponse, Status>>;
    type FetchRunBundleStream =
        Pin<Box<dyn Stream<Item = Result<FetchRunBundleResponse, Status>> + Send>>;

    async fn connect(
        &self,
        request: Request<Streaming<ConnectRequest>>,
    ) -> Result<Response<Self::ConnectStream>, Status> {
        let authorization = request
            .metadata()
            .get("authorization")
            .map(|value| value.to_str().unwrap().to_owned());
        let mut requests = request.into_inner();
        let Some(ConnectRequest {
            message: Some(Message::Hello(hello)),
        }) = requests.message().await?
        else {
            return Err(Status::invalid_argument("hello first"));
        };
        let (verdict, decided) = oneshot::channel();
        let _ = self
            .0
            .send(Offered {
                authorization,
                hello,
                requests,
                verdict,
            })
            .await;
        let responses = decided.await.map_err(|_| Status::internal("test gone"))??;
        Ok(Response::new(ReceiverStream::new(responses)))
    }

    async fn fetch_run_bundle(
        &self,
        _: Request<FetchRunBundleRequest>,
    ) -> Result<Response<Self::FetchRunBundleStream>, Status> {
        Err(Status::unimplemented("not faked"))
    }
}

/// A fake server on a loopback port and the connections it is offered.
async fn fake() -> (Endpoint, mpsc::Receiver<Offered>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (offers, offered) = mpsc::channel(1);
    let incoming =
        tonic::transport::server::TcpIncoming::from_listener(listener, true, None).unwrap();
    tokio::spawn(
        tonic::transport::Server::builder()
            .add_service(RunnerServiceServer::new(Fake(offers)))
            .serve_with_incoming(incoming),
    );
    let endpoint = Endpoint::from_shared(format!("http://{addr}")).unwrap();
    (endpoint, offered)
}

fn identity() -> Identity {
    Identity {
        name: "build-1".into(),
        hostname: "vm-host.example.org".into(),
        capabilities: Capabilities {
            arch: "x86_64".into(),
            os: "linux".into(),
            backends: vec!["nix".into()],
            tags: vec!["kvm".into()],
            max_jobs: 2,
        },
        secret: "s3cret".into(),
    }
}

fn response(message: connect_response::Message) -> Result<ConnectResponse, Status> {
    Ok(ConnectResponse {
        message: Some(message),
    })
}

async fn next(requests: &mut Streaming<ConnectRequest>) -> Message {
    tokio::time::timeout(Duration::from_secs(5), requests.message())
        .await
        .unwrap()
        .unwrap()
        .unwrap()
        .message
        .unwrap()
}

fn chunk(sequence: u64, data: &str) -> Message {
    Message::LogChunk(LogChunk {
        attempt_id: "A1".into(),
        sequence,
        step_index: 0,
        data: data.into(),
    })
}

#[test]
fn backoff_doubles_from_one_second_up_to_thirty() {
    let mut delays = Vec::new();
    let mut previous = None;
    for _ in 0..7 {
        let delay = next_backoff(previous);
        delays.push(delay.as_secs());
        previous = Some(delay);
    }
    assert_eq!(delays, [1, 2, 4, 8, 16, 30, 30]);
}

#[tokio::test]
async fn logs_and_results_are_kept_until_acknowledged_and_resent_after_a_continue() {
    let (attempts, metrics) = (Attempts::default(), Metrics::default());
    attempts.accept("A1");
    for data in ["one", "two", "three"] {
        attempts.log("A1", 0, data.into()).await;
    }
    assert_eq!(attempts.reconcile(), ["A1"]);
    let (outbound, mut sent) = mpsc::unbounded_channel();
    attempts.attach(Some(outbound));
    let mut drain = || {
        std::iter::from_fn(|| sent.try_recv().ok())
            .map(|request| request.message.unwrap())
            .collect::<Vec<_>>()
    };
    // A plain acknowledgment only drops what it covers.
    let ack = |sequence| {
        connect_response::Message::LogAck(LogAck {
            attempt_id: "A1".into(),
            sequence,
        })
    };
    assert_eq!(attempts.receive(ack(1)), None);
    assert!(drain().is_empty());
    let resumed = || {
        connect_response::Message::ContinueAttempt(ContinueAttempt {
            attempt_id: "A1".into(),
        })
    };
    attempts.receive(resumed());
    attempts.receive(ack(1));
    assert_eq!(drain(), [chunk(2, "two"), chunk(3, "three")]);
    let result = RunResult {
        attempt_id: "A1".into(),
        state: ResultState::Failed.into(),
        exit_code: Some(1),
        failing_check: String::new(),
        reason: "exit status 1".into(),
    };
    attempts.finish(result.clone(), &metrics);
    assert_eq!(drain(), [Message::Result(result.clone())]);
    attempts.receive(resumed());
    attempts.receive(ack(3));
    assert_eq!(drain(), [Message::Result(result)]);
    attempts.receive(connect_response::Message::ResultAck(ResultAck {
        attempt_id: "A1".into(),
    }));
    assert!(attempts.ids().is_empty());
    assert!(
        metrics
            .exposition("0")
            .contains("kajiba_runner_jobs_total{result=\"failed\"} 1")
    );
}

async fn offer(offered: &mut mpsc::Receiver<Offered>) -> Offered {
    tokio::time::timeout(Duration::from_secs(5), offered.recv())
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn completion_during_reconnection_waits_for_log_replay() {
    for finish_before_continue in [true, false] {
        let (attempts, metrics) = (Attempts::default(), Metrics::default());
        attempts.accept("A1");
        attempts.log("A1", 0, b"one".to_vec()).await;
        attempts.log("A1", 0, b"two".to_vec()).await;
        assert_eq!(attempts.reconcile(), ["A1"]);
        let (outbound, mut sent) = mpsc::unbounded_channel();
        attempts.attach(Some(outbound));
        attempts.accept("A2");
        assert_eq!(
            sent.try_recv().unwrap().message,
            Some(Message::Accept(Accept {
                attempt_id: "A2".into(),
            }))
        );
        attempts.send(Message::Heartbeat(Heartbeat {}));
        assert_eq!(
            sent.try_recv().unwrap().message,
            Some(Message::Heartbeat(Heartbeat {}))
        );
        assert_eq!(
            attempts.receive(connect_response::Message::CancelAttempt(CancelAttempt {
                attempt_id: "A2".into(),
            })),
            Some(Job::Cancelled("A2".into()))
        );
        let result = RunResult {
            attempt_id: "A1".into(),
            state: ResultState::Succeeded.into(),
            exit_code: Some(0),
            ..Default::default()
        };
        if finish_before_continue {
            attempts.log("A1", 0, b"three".to_vec()).await;
            attempts.finish(result.clone(), &metrics);
        }
        assert!(sent.try_recv().is_err());
        attempts.receive(connect_response::Message::ContinueAttempt(
            ContinueAttempt {
                attempt_id: "A1".into(),
            },
        ));
        if !finish_before_continue {
            attempts.log("A1", 0, b"three".to_vec()).await;
            attempts.finish(result.clone(), &metrics);
        }
        assert!(sent.try_recv().is_err());
        attempts.receive(connect_response::Message::LogAck(LogAck {
            attempt_id: "A1".into(),
            sequence: 1,
        }));
        let messages: Vec<_> = std::iter::from_fn(|| sent.try_recv().ok())
            .map(|request| request.message.unwrap())
            .collect();
        assert_eq!(
            messages,
            [chunk(2, "two"), chunk(3, "three"), Message::Result(result)]
        );
        attempts.receive(connect_response::Message::ResultAck(ResultAck {
            attempt_id: "A1".into(),
        }));
        assert!(attempts.ids().is_empty());
    }
}

#[tokio::test]
async fn reconnection_resumes_in_flight_attempts_with_backoff() {
    let (endpoint, mut offered) = fake().await;
    let attempts = Attempts::default();
    attempts.accept("A1");
    for data in ["one", "two", "three"] {
        attempts.log("A1", 0, data.into()).await;
    }
    let (jobs, mut handed) = mpsc::channel(4);
    let (paused, mut pauses) = mpsc::unbounded_channel();
    let runner = tokio::spawn(run(
        endpoint,
        identity(),
        attempts.clone(),
        jobs,
        move |delay| {
            let _ = paused.send(delay.as_secs());
            async {}
        },
    ));

    // The hello carries the identity and the in-flight attempt; logs resume after the
    // acknowledged sequence.
    let mut first = offer(&mut offered).await;
    assert_eq!(first.authorization.as_deref(), Some("Bearer s3cret"));
    assert_eq!(
        first.hello,
        Hello {
            name: "build-1".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            capabilities: Some(identity().capabilities),
            hostname: "vm-host.example.org".into(),
            in_flight_attempt_ids: vec!["A1".into()],
        }
    );
    let (server, responses) = mpsc::channel(8);
    first.verdict.send(Ok(responses)).unwrap();
    server
        .send(response(connect_response::Message::ContinueAttempt(
            ContinueAttempt {
                attempt_id: "A1".into(),
            },
        )))
        .await
        .unwrap();
    server
        .send(response(connect_response::Message::LogAck(LogAck {
            attempt_id: "A1".into(),
            sequence: 1,
        })))
        .await
        .unwrap();
    assert_eq!(next(&mut first.requests).await, chunk(2, "two"));
    assert_eq!(next(&mut first.requests).await, chunk(3, "three"));
    let assignment = Assignment {
        attempt_id: "A2".into(),
        repo_public_id: "00000000000000000000000001".into(),
        sha: "1111111111111111111111111111111111111111".into(),
        system: "x86_64-linux".into(),
        checks: Vec::new(),
    };
    server
        .send(response(connect_response::Message::Assignment(
            assignment.clone(),
        )))
        .await
        .unwrap();
    assert_eq!(handed.recv().await, Some(Job::Assigned(assignment)));
    drop((server, first.requests));

    // Refusals back off; an admitted connection starts the backoff over.
    for _ in 0..2 {
        let refused = offer(&mut offered).await;
        assert_eq!(refused.hello.in_flight_attempt_ids, ["A1"]);
        refused
            .verdict
            .send(Err(Status::unauthenticated("wrong secret")))
            .unwrap();
    }
    let again = offer(&mut offered).await;
    let (server, responses) = mpsc::channel(8);
    again.verdict.send(Ok(responses)).unwrap();
    server
        .send(response(connect_response::Message::CancelAttempt(
            CancelAttempt {
                attempt_id: "A1".into(),
            },
        )))
        .await
        .unwrap();
    assert_eq!(handed.recv().await, Some(Job::Cancelled("A1".into())));
    drop((server, again.requests));
    let last = offer(&mut offered).await;
    assert!(last.hello.in_flight_attempt_ids.is_empty());
    runner.abort();
    let pauses: Vec<u64> = std::iter::from_fn(|| pauses.try_recv().ok()).collect();
    assert_eq!(pauses, [1, 2, 4, 1]);
}

#[tokio::test]
async fn full_log_window_backpressures_until_acknowledged_or_cancelled() {
    let attempts = Attempts::default();
    attempts.accept("A1");
    // A disconnected build cannot retain more than the window.
    for _ in 0..LOG_WINDOW {
        attempts.log("A1", 0, vec![0; kajiba_exec::CHUNK]).await;
    }
    let blocked = attempts.log("A1", 0, b"next".to_vec());
    tokio::pin!(blocked);
    assert!(
        tokio::time::timeout(Duration::from_millis(10), &mut blocked)
            .await
            .is_err()
    );
    let (outbound, mut sent) = mpsc::unbounded_channel();
    attempts.attach(Some(outbound));
    // Control traffic remains independent of a full log window.
    attempts.send(Message::Heartbeat(Heartbeat {}));
    assert_eq!(
        sent.try_recv().unwrap().message,
        Some(Message::Heartbeat(Heartbeat {}))
    );
    // Another attempt can still make progress while A1 is backpressured.
    attempts.accept("A2");
    sent.try_recv().unwrap();
    tokio::time::timeout(
        Duration::from_secs(1),
        attempts.log("A2", 0, b"other".to_vec()),
    )
    .await
    .unwrap();
    assert!(matches!(
        sent.try_recv().unwrap().message,
        Some(Message::LogChunk(_))
    ));
    attempts.receive(connect_response::Message::CancelAttempt(CancelAttempt {
        attempt_id: "A2".into(),
    }));
    attempts.receive(connect_response::Message::LogAck(LogAck {
        attempt_id: "A1".into(),
        sequence: 1,
    }));
    tokio::time::timeout(Duration::from_secs(1), &mut blocked)
        .await
        .unwrap();
    assert_eq!(
        sent.try_recv().unwrap().message,
        Some(chunk(LOG_WINDOW as u64 + 1, "next"))
    );
    assert_eq!(
        attempts.0.lock().unwrap().attempts["A1"]
            .unacknowledged
            .len(),
        LOG_WINDOW
    );

    // Cancellation must release the forwarding task even without an acknowledgment.
    let blocked = attempts.log("A1", 0, b"cancelled".to_vec());
    tokio::pin!(blocked);
    assert!(
        tokio::time::timeout(Duration::from_millis(10), &mut blocked)
            .await
            .is_err()
    );
    attempts.receive(connect_response::Message::CancelAttempt(CancelAttempt {
        attempt_id: "A1".into(),
    }));
    tokio::time::timeout(Duration::from_secs(1), &mut blocked)
        .await
        .unwrap();
    assert!(sent.try_recv().is_err());
    assert!(attempts.ids().is_empty());
}
