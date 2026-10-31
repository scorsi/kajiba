use super::*;
use crate::transport::{PUBLIC, auth::tests::reason, router, serve_tcp, tests::State};
use kajiba_core::config::ServerConfig;
use kajiba_proto::runner::v1::{
    Capabilities as Announced, runner_service_client::RunnerServiceClient,
};
use tokio::sync::watch;
use tonic::transport::Channel;

const SECRET: &str = "runner-secret";
const OTHER_SECRET: &str = "other-runner-secret";
const VERSION: &str = env!("CARGO_PKG_VERSION");
const SHA: &str = "1111111111111111111111111111111111111111";

struct Forge {
    state: State,
    registry: Arc<Registry>,
    client: RunnerServiceClient<Channel>,
    server: tokio::task::JoinHandle<std::io::Result<()>>,
    _stop: watch::Sender<bool>,
}

impl Forge {
    /// `build-1` (aarch64-linux, nix and process, tag kvm, 2 jobs) and `build-2` (same,
    /// another secret) are declared; `gone` was declared once and is disabled.
    async fn start() -> Self {
        let state = State::new();
        state.start().await;
        let dir = &state.health.state_dir;
        let token = dir.join("runner-token");
        std::fs::write(&token, format!("{SECRET}\n")).unwrap();
        let other = dir.join("other-token");
        std::fs::write(&other, OTHER_SECRET).unwrap();
        let runner_with = |name: &str, token: &std::path::Path| {
            format!(
                "[runners.{name}]\ntoken_file = {:?}\n[runners.{name}.capabilities]\narch = 'aarch64'\nos = 'linux'\nbackends = ['nix', 'process']\ntags = ['kvm']\nmax_jobs = 2\n",
                token.display()
            )
        };
        let runner = |name: &str| runner_with(name, &token);
        let config = |runners: &str| {
            ServerConfig::parse(&format!(
                "[http]\npublic_url = 'https://example.org'\n{runners}"
            ))
            .unwrap()
        };
        let before = config(&format!("{}{}", runner("build-1"), runner("gone")));
        let config = config(&format!(
            "{}{}",
            runner("build-1"),
            runner_with("build-2", &other)
        ));
        {
            let mut writer = state.health.writer.get().unwrap().lock().await;
            crate::db::runners::reconcile_runners(&mut writer, &before.runners, 1)
                .await
                .unwrap();
            crate::db::runners::reconcile_runners(&mut writer, &config.runners, 2)
                .await
                .unwrap();
        }
        state.health.set_runners(Declared {
            runners: config.runners,
            bus: Arc::default(),
            wake: Arc::default(),
        });
        let registry = Arc::new(Registry::default());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = router(
            state.health.clone(),
            false,
            registry.clone(),
            "https://example.org",
        )
        .unwrap();
        let (stop, stopped) = watch::channel(false);
        let server = tokio::spawn(serve_tcp(listener, app, PUBLIC, stopped));
        let channel = tonic::transport::Endpoint::from_shared(format!("http://{addr}"))
            .unwrap()
            .connect_timeout(Duration::from_secs(5))
            .connect()
            .await
            .unwrap();
        Self {
            state,
            registry,
            client: RunnerServiceClient::new(channel),
            server,
            _stop: stop,
        }
    }

    async fn connect(
        &self,
        secret: Option<&str>,
        hello: Hello,
    ) -> Result<(mpsc::Sender<ConnectRequest>, Streaming<ConnectResponse>), Status> {
        let (sender, receiver) = mpsc::channel(4);
        sender
            .send(ConnectRequest {
                message: Some(Message::Hello(hello)),
            })
            .await
            .unwrap();
        let mut request = Request::new(ReceiverStream::new(receiver));
        if let Some(secret) = secret {
            request
                .metadata_mut()
                .insert("authorization", format!("Bearer {secret}").parse().unwrap());
        }
        let response = self.client.clone().connect(request).await?;
        Ok((sender, response.into_inner()))
    }

    async fn sql<T: for<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow> + Send + Unpin>(
        &self,
        query: &str,
    ) -> Vec<T> {
        let mut writer = self.state.health.writer.get().unwrap().lock().await;
        sqlx::query_as(query).fetch_all(&mut *writer).await.unwrap()
    }

    async fn events(&self) -> Vec<(String, String, Option<i64>, serde_json::Value)> {
        self.sql::<(String, String, Option<i64>, String)>(
            "SELECT kind, principal_kind, principal_id, payload FROM event ORDER BY id",
        )
        .await
        .into_iter()
        .map(|(kind, principal, id, payload)| {
            (kind, principal, id, serde_json::from_str(&payload).unwrap())
        })
        .collect()
    }

    fn metric(&self, line: &str) -> bool {
        self.registry
            .exposition("test", [0; 4], [0; 4])
            .lines()
            .any(|l| l == line)
    }

    async fn exec(&self, query: &str) {
        let mut writer = self.state.health.writer.get().unwrap().lock().await;
        sqlx::raw_sql(query).execute(&mut *writer).await.unwrap();
    }

    /// User `alice` and repository `demo` (public id `…01`) with one pushed run.
    async fn run(&self) -> String {
        self.exec("INSERT INTO user (id, public_id, name, kind, is_admin, source, created_at) VALUES (1, '00000000000000000000000001', 'alice', 'local', 0, 'config', 0);
            INSERT INTO repo (id, public_id, owner_id, name, visibility, ci_enabled, default_branch, description, created_at, config_key) VALUES (1, '00000000000000000000000001', 1, 'demo', 'public', 1, 'main', '', 0, 'demo')").await;
        self.run_at(SHA).await
    }

    /// Another pushed run of `demo`.
    async fn run_at(&self, sha: &str) -> String {
        let mut writer = self.state.health.writer.get().unwrap().lock().await;
        let mut tx = writer.begin().await.unwrap();
        let run = crate::db::runs::create_run(
            &mut tx,
            &crate::db::runs::NewRun {
                repo_id: 1,
                sha,
                reference: Some("refs/heads/main"),
                trigger: "push",
                event_id: None,
                triggered_by: Some(1),
                correlation_id: "push",
                now: 1,
                worktree: None,
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        run
    }

    async fn until_metric(&self, line: &str) {
        for _ in 0..300 {
            if self.metric(line) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("{line} never exposed");
    }

    /// Wait for the server side of a stream to settle (disconnection runs in a task).
    async fn until(&self, query: &str, expected: i64) {
        for _ in 0..200 {
            if self.sql::<(i64,)>(query).await[0].0 == expected {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("{query} never returned {expected}");
    }
}

impl Drop for Forge {
    fn drop(&mut self) {
        self.server.abort();
    }
}

type RunnerRow = (String, Option<String>, bool, Option<i64>, Option<i64>);

fn hello(name: &str) -> Hello {
    Hello {
        name: name.into(),
        version: VERSION.into(),
        capabilities: Some(Announced {
            arch: "aarch64".into(),
            os: "linux".into(),
            backends: vec!["nix".into()],
            tags: vec!["kvm".into()],
            max_jobs: 1,
        }),
        hostname: "builder.example.org".into(),
        in_flight_attempt_ids: Vec::new(),
    }
}

fn other_minor() -> String {
    let (major, minor, _) = kajiba_core::version::semver(VERSION).unwrap();
    format!("{major}.{}.0", minor + 1)
}

#[tokio::test]
async fn each_refusal_has_its_code_reason_metric_and_journal_entry() {
    let forge = Forge::start().await;
    let capabilities = |change: fn(&mut Announced)| {
        let mut hello = hello("build-1");
        change(hello.capabilities.as_mut().unwrap());
        hello
    };
    let cases: Vec<(Option<&str>, Hello, tonic::Code, &str, &str)> = vec![
        (
            Some(SECRET),
            hello("nobody"),
            tonic::Code::Unauthenticated,
            "CREDENTIAL_INVALID",
            "unknown",
        ),
        (
            Some(SECRET),
            hello("gone"),
            tonic::Code::Unauthenticated,
            "CREDENTIAL_INVALID",
            "disabled",
        ),
        (
            Some("wrong"),
            hello("build-1"),
            tonic::Code::Unauthenticated,
            "CREDENTIAL_INVALID",
            "secret",
        ),
        (
            None,
            hello("build-1"),
            tonic::Code::Unauthenticated,
            "CREDENTIAL_INVALID",
            "secret",
        ),
        (
            Some(SECRET),
            Hello {
                version: other_minor(),
                ..hello("build-1")
            },
            tonic::Code::FailedPrecondition,
            "VERSION_INCOMPATIBLE",
            "version",
        ),
        (
            Some(SECRET),
            capabilities(|c| c.arch = "x86_64".into()),
            tonic::Code::InvalidArgument,
            "INVALID_FIELD",
            "capabilities",
        ),
        (
            Some(SECRET),
            capabilities(|c| c.os = "darwin".into()),
            tonic::Code::InvalidArgument,
            "INVALID_FIELD",
            "capabilities",
        ),
        (
            Some(SECRET),
            capabilities(|c| c.backends.push("microvm".into())),
            tonic::Code::InvalidArgument,
            "INVALID_FIELD",
            "capabilities",
        ),
        (
            Some(SECRET),
            capabilities(|c| c.tags.push("gpu".into())),
            tonic::Code::InvalidArgument,
            "INVALID_FIELD",
            "capabilities",
        ),
        (
            Some(SECRET),
            capabilities(|c| c.max_jobs = 3),
            tonic::Code::InvalidArgument,
            "INVALID_FIELD",
            "capabilities",
        ),
        (
            Some(SECRET),
            Hello {
                capabilities: None,
                ..hello("build-1")
            },
            tonic::Code::InvalidArgument,
            "INVALID_FIELD",
            "capabilities",
        ),
    ];
    let mut journaled = Vec::new();
    for (secret, hello, code, expected, why) in cases {
        let version = hello.version.clone();
        let status = forge.connect(secret, hello).await.unwrap_err();
        assert_eq!(
            (status.code(), reason(&status).as_str()),
            (code, expected),
            "{why}"
        );
        if why == "version" {
            assert!(status.message().contains(&version) && status.message().contains(VERSION));
        }
        // Only a caller holding the secret may grow the journal (0108).
        if matches!(why, "version" | "capabilities") {
            journaled.push(why);
        }
    }
    let events = forge.events().await;
    assert_eq!(
        events
            .iter()
            .map(|(kind, principal, _, payload)| {
                assert_eq!(
                    (kind.as_str(), principal.as_str()),
                    ("runner-refused", "runner")
                );
                assert_eq!(payload["runner"], "build-1");
                payload["reason"].as_str().unwrap().to_owned()
            })
            .collect::<Vec<_>>(),
        journaled
    );
    for (reason, count) in [
        ("unknown", 1),
        ("disabled", 1),
        ("secret", 2),
        ("version", 1),
        ("capabilities", 6),
    ] {
        assert!(
            forge.metric(&format!(
                "kajiba_runner_refusals_total{{reason=\"{reason}\"}} {count}"
            )),
            "{reason}"
        );
    }
    assert!(forge.metric("kajiba_runners_connected 0"));
    let connected: Vec<(i64,)> = forge
        .sql("SELECT count(*) FROM runner WHERE connected = 1")
        .await;
    assert_eq!(connected[0].0, 0);
}

#[tokio::test]
async fn admission_records_the_runner_and_a_newer_connection_replaces_the_older() {
    let forge = Forge::start().await;
    // Another patch of the same minor is compatible (0023).
    let (major, minor, patch) = kajiba_core::version::semver(VERSION).unwrap();
    let patched = format!("{major}.{minor}.{}", patch + 1);
    let (_first, mut first_stream) = forge
        .connect(
            Some(SECRET),
            Hello {
                version: patched.clone(),
                ..hello("build-1")
            },
        )
        .await
        .unwrap();
    let row: Vec<RunnerRow> = forge
        .sql("SELECT version, hostname, connected, last_contact_at, disabled_at FROM runner WHERE name = 'build-1'")
        .await;
    assert_eq!(
        (row[0].0.as_str(), row[0].1.as_deref(), row[0].2),
        (patched.as_str(), Some("builder.example.org"), true)
    );
    assert!(row[0].3.is_some() && row[0].4.is_none());
    assert!(forge.metric("kajiba_runners_connected 1"));

    let (second, _second_stream) = forge.connect(Some(SECRET), hello("build-1")).await.unwrap();
    let ended = tokio::time::timeout(Duration::from_secs(5), first_stream.message())
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(ended.code(), tonic::Code::Aborted);
    assert!(forge.metric("kajiba_runners_connected 1"));
    let version: Vec<(String, bool)> = forge
        .sql("SELECT version, connected FROM runner WHERE name = 'build-1'")
        .await;
    assert_eq!(version[0], (VERSION.to_owned(), true));

    drop(second);
    forge
        .until(
            "SELECT count(*) FROM event WHERE kind = 'runner-disconnected'",
            1,
        )
        .await;
    forge
        .until("SELECT connected FROM runner WHERE name = 'build-1'", 0)
        .await;
    assert!(forge.metric("kajiba_runners_connected 0"));
    let id: Vec<(i64,)> = forge
        .sql("SELECT id FROM runner WHERE name = 'build-1'")
        .await;
    let events = forge.events().await;
    let kinds: Vec<&str> = events.iter().map(|event| event.0.as_str()).collect();
    assert_eq!(
        kinds,
        [
            "runner-connected",
            "runner-connected",
            "runner-disconnected"
        ]
    );
    for (_, principal, principal_id, _) in &events {
        assert_eq!(
            (principal.as_str(), *principal_id),
            ("runner", Some(id[0].0))
        );
    }
    assert_eq!(
        events[0].3,
        serde_json::json!({ "runner": "build-1", "version": patched, "hostname": "builder.example.org" })
    );
    assert_eq!(events[2].3, serde_json::json!({ "runner": "build-1" }));
}

#[tokio::test]
async fn a_stream_must_open_with_a_hello() {
    let forge = Forge::start().await;
    let (sender, receiver) = mpsc::channel(1);
    sender
        .send(ConnectRequest {
            message: Some(Message::Heartbeat(Default::default())),
        })
        .await
        .unwrap();
    let mut request = Request::new(ReceiverStream::new(receiver));
    request
        .metadata_mut()
        .insert("authorization", format!("Bearer {SECRET}").parse().unwrap());
    let status = forge.client.clone().connect(request).await.unwrap_err();
    assert_eq!(status.code(), tonic::Code::InvalidArgument);
    let status = forge
        .client
        .clone()
        .fetch_run_bundle(FetchRunBundleRequest::default())
        .await
        .unwrap_err();
    assert_eq!(status.code(), tonic::Code::Unauthenticated);
}

#[tokio::test]
async fn connection_logs_name_the_runner_and_the_request_method() {
    const CHILD: &str = "KAJIBA_TEST_RUNNER_LOGS";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "transport::runners::tests::connection_logs_name_the_runner_and_the_request_method",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env_remove("RUST_LOG")
            .output()
            .unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(output.status.success(), "{stderr}");
        let lines: Vec<serde_json::Value> = stderr
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect();
        let find = |event: &str| {
            lines
                .iter()
                .filter(|line| line["event"] == event)
                .cloned()
                .collect::<Vec<_>>()
        };
        let refused = find("runner-refused");
        assert_eq!(refused.len(), 2, "{stderr}");
        assert_eq!(
            (
                refused[0]["level"].as_str(),
                refused[0]["reason"].as_str(),
                refused[0]["runner"].as_str()
            ),
            (Some("warn"), Some("unknown"), Some(""))
        );
        assert!(
            !stderr.contains("not-a-runner"),
            "an unknown name is never logged"
        );
        assert_eq!(
            (refused[1]["reason"].as_str(), refused[1]["runner"].as_str()),
            (Some("secret"), Some("build-1"))
        );
        assert!(!stderr.contains("wrong-secret") && !stderr.contains(SECRET));
        let connected = find("runner-connected");
        assert_eq!(connected.len(), 1, "{stderr}");
        assert_eq!(connected[0]["runner"], "build-1");
        assert_eq!(connected[0]["version"], VERSION);
        assert_eq!(connected[0]["hostname"], "builder.example.org");
        assert_eq!(find("runner-disconnected").len(), 1, "{stderr}");
        let requests = find("request");
        assert_eq!(requests.len(), 3, "{stderr}");
        for request in requests {
            assert_eq!(request["method"], "/kajiba.runner.v1.RunnerService/Connect");
        }
        return;
    }
    kajiba_core::log::init("info").unwrap();
    let forge = Forge::start().await;
    forge
        .connect(Some(SECRET), hello("not-a-runner"))
        .await
        .unwrap_err();
    forge
        .connect(Some("wrong-secret"), hello("build-1"))
        .await
        .unwrap_err();
    let (sender, stream) = forge.connect(Some(SECRET), hello("build-1")).await.unwrap();
    drop((sender, stream));
    forge
        .until(
            "SELECT count(*) FROM event WHERE kind = 'runner-disconnected'",
            1,
        )
        .await;
}

async fn next(stream: &mut Streaming<ConnectResponse>) -> connect_response::Message {
    tokio::time::timeout(Duration::from_secs(5), stream.message())
        .await
        .unwrap()
        .unwrap()
        .unwrap()
        .message
        .unwrap()
}

#[tokio::test]
async fn a_pending_job_is_assigned_to_an_admitted_runner_and_accepted() {
    let forge = Forge::start().await;
    let run = forge.run().await;
    forge.until_metric("kajiba_jobs_pending 1").await;
    forge
        .until(
            "SELECT count(*) FROM job WHERE pending_reason = 'no-connected-runner'",
            1,
        )
        .await;
    let (sender, mut stream) = forge.connect(Some(SECRET), hello("build-1")).await.unwrap();
    let connect_response::Message::Assignment(assignment) = next(&mut stream).await else {
        panic!("expected an assignment");
    };
    assert_eq!(
        (
            assignment.repo_public_id.as_str(),
            assignment.sha.as_str(),
            assignment.system.as_str()
        ),
        ("00000000000000000000000001", SHA, "aarch64-linux")
    );
    forge.until_metric("kajiba_jobs_pending 0").await;
    let send = |message| {
        let sender = sender.clone();
        async move {
            sender
                .send(ConnectRequest {
                    message: Some(message),
                })
                .await
                .unwrap()
        }
    };
    send(Message::Accept(kajiba_proto::runner::v1::Accept {
        attempt_id: "7ZZZZZZZZZZZZZZZZZZZZZZZZZ".into(),
    }))
    .await;
    assert_eq!(
        next(&mut stream).await,
        connect_response::Message::CancelAttempt(CancelAttempt {
            attempt_id: "7ZZZZZZZZZZZZZZZZZZZZZZZZZ".into()
        })
    );
    send(Message::Accept(kajiba_proto::runner::v1::Accept {
        attempt_id: assignment.attempt_id.clone(),
    }))
    .await;
    forge
        .until(
            "SELECT count(*) FROM job JOIN run ON run.id = job.run_id WHERE job.state = 'running' AND run.state = 'running' AND job.pending_reason IS NULL",
            1,
        )
        .await;
    let events = forge.events().await;
    let started: Vec<_> = events
        .iter()
        .filter(|event| event.0 == "run-started")
        .collect();
    assert_eq!(started.len(), 1);
    assert_eq!(
        started[0].3,
        serde_json::json!({ "run": run, "attempt": assignment.attempt_id, "runner": "build-1" })
    );
}

async fn send(sender: &mpsc::Sender<ConnectRequest>, message: Message) {
    sender
        .send(ConnectRequest {
            message: Some(message),
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn logs_and_results_are_acknowledged_and_counted() {
    use kajiba_proto::runner::v1::{Accept, LogChunk, Result as RunResult};
    let forge = Forge::start().await;
    let run = forge.run().await;
    let (sender, mut stream) = forge.connect(Some(SECRET), hello("build-1")).await.unwrap();
    let connect_response::Message::Assignment(assignment) = next(&mut stream).await else {
        panic!("expected an assignment");
    };
    let attempt = assignment.attempt_id;
    send(
        &sender,
        Message::Accept(Accept {
            attempt_id: attempt.clone(),
        }),
    )
    .await;
    let chunk = |sequence: u64, data: &[u8]| {
        Message::LogChunk(LogChunk {
            attempt_id: attempt.clone(),
            sequence,
            step_index: 0,
            data: data.to_vec(),
        })
    };
    let ack = |sequence| {
        connect_response::Message::LogAck(LogAck {
            attempt_id: attempt.clone(),
            sequence,
        })
    };
    for (sequence, expected) in [(1, 1), (1, 1), (3, 1), (2, 2)] {
        send(&sender, chunk(sequence, b"line\n")).await;
        assert_eq!(next(&mut stream).await, ack(expected));
    }
    let log = forge
        .state
        .health
        .state_dir
        .join(format!("ci/logs/{attempt}.log"));
    assert_eq!(std::fs::read(&log).unwrap(), b"line\nline\n");
    let result = |attempt_id: &str| {
        Message::Result(RunResult {
            attempt_id: attempt_id.into(),
            state: ResultState::Failed.into(),
            exit_code: Some(1),
            failing_check: String::new(),
            reason: "exit status 1".into(),
        })
    };
    let result_ack = connect_response::Message::ResultAck(ResultAck {
        attempt_id: attempt.clone(),
    });
    send(&sender, result(&attempt)).await;
    assert_eq!(next(&mut stream).await, result_ack);
    send(&sender, result(&attempt)).await;
    assert_eq!(next(&mut stream).await, result_ack);
    assert!(forge.metric("kajiba_runs_total{result=\"failed\"} 1"));
    let state: Vec<(String, Option<String>)> =
        forge.sql("SELECT state, failure_reason FROM run").await;
    assert_eq!(state, [("failed".into(), Some("exit status 1".into()))]);
    assert_eq!(
        forge
            .events()
            .await
            .iter()
            .filter(|event| event.0 == "run-finished")
            .map(|event| event.3.clone())
            .collect::<Vec<_>>(),
        [
            serde_json::json!({ "run": run, "state": "failed", "check": null, "reason": "exit status 1" })
        ]
    );
    // A message for an attempt that is not current is answered with cancel (0008).
    send(&sender, result("7ZZZZZZZZZZZZZZZZZZZZZZZZZ")).await;
    assert_eq!(
        next(&mut stream).await,
        connect_response::Message::CancelAttempt(CancelAttempt {
            attempt_id: "7ZZZZZZZZZZZZZZZZZZZZZZZZZ".into()
        })
    );
    // A chunk above 64 KiB is refused and ends the stream.
    send(&sender, chunk(3, &vec![b'x'; 64 * 1024 + 1])).await;
    let status = tokio::time::timeout(Duration::from_secs(5), stream.message())
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(
        (status.code(), reason(&status).as_str()),
        (tonic::Code::InvalidArgument, "INVALID_FIELD")
    );
    assert_eq!(std::fs::read(&log).unwrap(), b"line\nline\n");
}

#[tokio::test]
async fn a_reconnected_runner_continues_its_attempt_and_resends_unacknowledged_logs() {
    use kajiba_proto::runner::v1::{Accept, ContinueAttempt, LogChunk};
    let forge = Forge::start().await;
    forge.run().await;
    let (sender, mut stream) = forge.connect(Some(SECRET), hello("build-1")).await.unwrap();
    let connect_response::Message::Assignment(assignment) = next(&mut stream).await else {
        panic!("expected an assignment");
    };
    let attempt = assignment.attempt_id;
    send(
        &sender,
        Message::Accept(Accept {
            attempt_id: attempt.clone(),
        }),
    )
    .await;
    let chunk = |sequence: u64| {
        Message::LogChunk(LogChunk {
            attempt_id: attempt.clone(),
            sequence,
            step_index: 0,
            data: format!("{sequence}\n").into_bytes(),
        })
    };
    let ack = |sequence| {
        connect_response::Message::LogAck(LogAck {
            attempt_id: attempt.clone(),
            sequence,
        })
    };
    for sequence in [1, 2] {
        send(&sender, chunk(sequence)).await;
        assert_eq!(next(&mut stream).await, ack(sequence));
    }
    drop((sender, stream));
    forge
        .until("SELECT connected FROM runner WHERE name = 'build-1'", 0)
        .await;
    let unknown = "7ZZZZZZZZZZZZZZZZZZZZZZZZZ".to_owned();
    let (sender, mut stream) = forge
        .connect(
            Some(SECRET),
            Hello {
                in_flight_attempt_ids: vec![attempt.clone(), unknown.clone()],
                ..hello("build-1")
            },
        )
        .await
        .unwrap();
    assert_eq!(
        next(&mut stream).await,
        connect_response::Message::ContinueAttempt(ContinueAttempt {
            attempt_id: attempt.clone()
        })
    );
    assert_eq!(next(&mut stream).await, ack(2));
    assert_eq!(
        next(&mut stream).await,
        connect_response::Message::CancelAttempt(CancelAttempt {
            attempt_id: unknown
        })
    );
    // The runner resends from its last acknowledgment it saw, here one chunk early.
    for (sequence, acknowledged) in [(2, 2), (3, 3)] {
        send(&sender, chunk(sequence)).await;
        assert_eq!(next(&mut stream).await, ack(acknowledged));
    }
    let log = forge
        .state
        .health
        .state_dir
        .join(format!("ci/logs/{attempt}.log"));
    assert_eq!(std::fs::read(&log).unwrap(), b"1\n2\n3\n");
    let attempts: Vec<(i64, String)> = forge.sql("SELECT number, state FROM job_attempt").await;
    assert_eq!(attempts, [(1, "running".into())]);
}

impl Forge {
    async fn bundle(
        &self,
        secret: Option<&str>,
        attempt: &str,
        have: &[&str],
    ) -> Result<String, Status> {
        let mut request = Request::new(FetchRunBundleRequest {
            attempt_id: attempt.into(),
            have: have.iter().map(|id| id.to_string()).collect(),
        });
        if let Some(secret) = secret {
            request
                .metadata_mut()
                .insert("authorization", format!("Bearer {secret}").parse().unwrap());
        }
        let mut stream = self
            .client
            .clone()
            .fetch_run_bundle(request)
            .await?
            .into_inner();
        let mut bundle = Vec::new();
        while let Some(message) = stream.message().await? {
            bundle.extend(message.data);
        }
        // The pack is binary: the header is what the tests read.
        Ok(String::from_utf8_lossy(&bundle).into_owned())
    }
}

/// `git` with an isolated configuration in `dir`; its trimmed output.
fn git(dir: &std::path::Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Alice")
        .env("GIT_AUTHOR_EMAIL", "alice@example.org")
        .env("GIT_COMMITTER_NAME", "Alice")
        .env("GIT_COMMITTER_EMAIL", "alice@example.org")
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

#[tokio::test]
async fn bundles_serve_only_the_current_leased_attempt_of_the_calling_runner() {
    let forge = Forge::start().await;
    // `demo`: `first` then `second` on main, `contrib` only under `refs/contrib/*`.
    let repo = forge
        .state
        .health
        .git_state_dir
        .join("repos/00000000000000000000000001.git");
    git(
        &forge.state.health.git_state_dir,
        &["init", "-q", "--bare", repo.to_str().unwrap()],
    );
    let commit = |message: &str| {
        git(
            &repo,
            &[
                "commit-tree",
                "-m",
                message,
                "4b825dc642cb6eb9a060e54bf8d69288fbee4904",
            ],
        )
    };
    let first = commit("first");
    let second = git(
        &repo,
        &[
            "commit-tree",
            "-p",
            &first,
            "-m",
            "second",
            "4b825dc642cb6eb9a060e54bf8d69288fbee4904",
        ],
    );
    let contrib = git(
        &repo,
        &[
            "commit-tree",
            "-p",
            &second,
            "-m",
            "contrib",
            "4b825dc642cb6eb9a060e54bf8d69288fbee4904",
        ],
    );
    git(&repo, &["update-ref", "refs/heads/main", &second]);
    git(&repo, &["update-ref", "refs/contrib/alice/x", &contrib]);

    forge.exec("INSERT INTO user (id, public_id, name, kind, is_admin, source, created_at) VALUES (1, '00000000000000000000000001', 'alice', 'local', 0, 'config', 0);
        INSERT INTO repo (id, public_id, owner_id, name, visibility, ci_enabled, default_branch, description, created_at, config_key) VALUES (1, '00000000000000000000000001', 1, 'demo', 'public', 1, 'main', '', 0, 'demo')").await;
    forge.run_at(&second).await;
    let (_sender, mut stream) = forge.connect(Some(SECRET), hello("build-1")).await.unwrap();
    let connect_response::Message::Assignment(assignment) = next(&mut stream).await else {
        panic!("expected an assignment");
    };
    let attempt = assignment.attempt_id.as_str();

    // An empty cache gets the full history; a cache holding the parent only the new
    // commit; unknown and malformed ids are ignored.
    let full = forge.bundle(Some(SECRET), attempt, &[]).await.unwrap();
    assert!(
        full.starts_with(&format!("# v2 git bundle\n{second} refs/kajiba/run\n\n")),
        "{full}"
    );
    let incremental = forge
        .bundle(
            Some(SECRET),
            attempt,
            &[&first, "zz", "ffffffffffffffffffffffffffffffffffffffff"],
        )
        .await
        .unwrap();
    assert!(
        incremental.starts_with(&format!(
            "# v2 git bundle\n-{first}\n{second} refs/kajiba/run\n\n"
        )),
        "{incremental}"
    );
    assert!(incremental.len() < full.len());

    // Another runner's secret, no secret and an unknown attempt are not this runner.
    for (secret, attempt) in [
        (Some(OTHER_SECRET), attempt),
        (None, attempt),
        (Some(SECRET), "7ZZZZZZZZZZZZZZZZZZZZZZZZZ"),
    ] {
        let status = forge.bundle(secret, attempt, &[]).await.unwrap_err();
        assert_eq!(
            (status.code(), reason(&status).as_str()),
            (tonic::Code::Unauthenticated, "CREDENTIAL_INVALID")
        );
    }

    // A lost attempt is refused; its capacity goes to a manual run of a contribution.
    forge
        .exec(&format!(
            "UPDATE job_attempt SET state = 'lost' WHERE public_id = '{attempt}'"
        ))
        .await;
    let status = forge.bundle(Some(SECRET), attempt, &[]).await.unwrap_err();
    assert_eq!(
        (status.code(), reason(&status).as_str()),
        (tonic::Code::FailedPrecondition, "INVALID_STATE")
    );
    forge.run_at(&contrib).await;
    let connect_response::Message::Assignment(assignment) = next(&mut stream).await else {
        panic!("expected an assignment");
    };
    let attempt = assignment.attempt_id.as_str();
    let served = forge
        .bundle(Some(SECRET), attempt, &[&second])
        .await
        .unwrap();
    assert!(
        served.starts_with(&format!(
            "# v2 git bundle\n-{second}\n{contrib} refs/kajiba/run\n\n"
        )),
        "{served}"
    );

    // A stale attempt (a newer one exists for its job) and an expired lease are refused.
    forge
        .exec(&format!(
            "UPDATE job_attempt SET lease_expires_at = 1 WHERE public_id = '{attempt}'"
        ))
        .await;
    let status = forge.bundle(Some(SECRET), attempt, &[]).await.unwrap_err();
    assert_eq!(status.code(), tonic::Code::FailedPrecondition);
    forge.exec(&format!("INSERT INTO job_attempt (public_id, job_id, number, runner_id, state, lease_expires_at, started_at)
        SELECT '7YYYYYYYYYYYYYYYYYYYYYYYYY', job_id, number + 1, runner_id, 'running', 9999999999999, 1 FROM job_attempt WHERE public_id = '{attempt}'")).await;
    forge
        .exec(&format!(
            "UPDATE job_attempt SET lease_expires_at = 9999999999999 WHERE public_id = '{attempt}'"
        ))
        .await;
    let status = forge.bundle(Some(SECRET), attempt, &[]).await.unwrap_err();
    assert_eq!(status.code(), tonic::Code::FailedPrecondition);
}

impl Forge {
    /// The bundles served to `attempt`, by index, in order.
    async fn bundles(&self, attempt: &str, have: &[&str]) -> Result<Vec<(u32, Vec<u8>)>, Status> {
        let mut request = Request::new(FetchRunBundleRequest {
            attempt_id: attempt.into(),
            have: have.iter().map(|id| id.to_string()).collect(),
        });
        request
            .metadata_mut()
            .insert("authorization", format!("Bearer {SECRET}").parse().unwrap());
        let mut stream = self
            .client
            .clone()
            .fetch_run_bundle(request)
            .await?
            .into_inner();
        let mut bundles: Vec<(u32, Vec<u8>)> = Vec::new();
        while let Some(message) = stream.message().await? {
            match bundles.last_mut() {
                Some((bundle, data)) if *bundle == message.bundle => data.extend(message.data),
                _ => bundles.push((message.bundle, message.data)),
            }
        }
        Ok(bundles)
    }
}

/// The header of a bundle, up to its pack.
fn header(bundle: &[u8]) -> String {
    let end = bundle.windows(5).position(|w| w == b"\nPACK").unwrap() + 1;
    String::from_utf8(bundle[..end].to_vec()).unwrap()
}

#[tokio::test]
async fn a_worktree_attempt_gets_the_base_it_lacks_then_its_stored_bundle() {
    let forge = Forge::start().await;
    let git_dir = forge.state.health.git_state_dir.clone();
    let repo = git_dir.join("repos/00000000000000000000000001.git");
    git(&git_dir, &["init", "-q", "--bare", repo.to_str().unwrap()]);
    let empty_tree = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
    let first = git(&repo, &["commit-tree", "-m", "first", empty_tree]);
    let second = git(
        &repo,
        &["commit-tree", "-p", &first, "-m", "second", empty_tree],
    );
    git(&repo, &["update-ref", "refs/heads/main", &second]);
    // The client's commit exists only in its clone and in the stored bundle.
    let clone = git_dir.join("clone");
    git(
        &git_dir,
        &[
            "clone",
            "-q",
            repo.to_str().unwrap(),
            clone.to_str().unwrap(),
        ],
    );
    let captured = git(
        &clone,
        &["commit-tree", "-p", &second, "-m", "wip", empty_tree],
    );
    git(&clone, &["update-ref", "refs/kajiba/worktree/x", &captured]);
    let stored = git_dir.join("stored.bundle");
    git(
        &clone,
        &[
            "bundle",
            "create",
            "-q",
            stored.to_str().unwrap(),
            "refs/kajiba/worktree/x",
            &format!("^{second}"),
        ],
    );

    forge.exec("INSERT INTO user (id, public_id, name, kind, is_admin, source, created_at) VALUES (1, '00000000000000000000000001', 'alice', 'local', 0, 'config', 0);
        INSERT INTO repo (id, public_id, owner_id, name, visibility, ci_enabled, default_branch, description, created_at, config_key) VALUES (1, '00000000000000000000000001', 1, 'demo', 'public', 1, 'main', '', 0, 'demo')").await;
    let run = {
        let mut writer = forge.state.health.writer.get().unwrap().lock().await;
        let mut tx = writer.begin().await.unwrap();
        let run = crate::db::runs::create_run(
            &mut tx,
            &crate::db::runs::NewRun {
                repo_id: 1,
                sha: &captured,
                reference: None,
                trigger: "worktree",
                event_id: None,
                triggered_by: Some(1),
                correlation_id: "worktree",
                now: 1,
                worktree: Some(crate::db::runs::Worktree {
                    checks: &["a".into(), "b".into()],
                    arch: "aarch64",
                    os: "linux",
                    features: &["kvm".into()],
                }),
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        run
    };
    std::fs::copy(
        &stored,
        forge
            .state
            .health
            .state_dir
            .join(format!("ci/bundles/{run}.bundle")),
    )
    .unwrap();
    let (_sender, mut stream) = forge.connect(Some(SECRET), hello("build-1")).await.unwrap();
    let connect_response::Message::Assignment(assignment) = next(&mut stream).await else {
        panic!("expected an assignment");
    };
    assert_eq!(assignment.sha, captured);
    assert_eq!(assignment.checks, ["a", "b"]);
    assert_eq!(assignment.system, "aarch64-linux");
    let attempt = assignment.attempt_id.clone();

    // An empty cache: the base, then the stored bundle under the run ref; both apply.
    let bundles = forge.bundles(&attempt, &[]).await.unwrap();
    assert_eq!(bundles.iter().map(|(n, _)| *n).collect::<Vec<_>>(), [0, 1]);
    assert_eq!(
        header(&bundles[0].1),
        format!("# v2 git bundle\n{second} refs/kajiba/run\n\n")
    );
    assert_eq!(
        header(&bundles[1].1),
        format!("# v2 git bundle\n-{second}\n{captured} refs/kajiba/run\n\n")
    );
    let cache = git_dir.join("cache.git");
    git(&git_dir, &["init", "-q", "--bare", cache.to_str().unwrap()]);
    for (n, (_, data)) in bundles.iter().enumerate() {
        let path = git_dir.join(format!("part-{n}.bundle"));
        std::fs::write(&path, data).unwrap();
        git(
            &cache,
            &["fetch", "-q", path.to_str().unwrap(), "refs/kajiba/run"],
        );
    }
    git(
        &cache,
        &["cat-file", "-e", &format!("{captured}^{{commit}}")],
    );
    // Holding the parent of the base: only the base's own commit, then the stored bundle.
    let bundles = forge.bundles(&attempt, &[&first]).await.unwrap();
    assert_eq!(
        header(&bundles[0].1),
        format!("# v2 git bundle\n-{first}\n{second} refs/kajiba/run\n\n")
    );
    // Holding the base: the stored bundle alone.
    let bundles = forge.bundles(&attempt, &[&second]).await.unwrap();
    assert_eq!(bundles.iter().map(|(n, _)| *n).collect::<Vec<_>>(), [1]);
    // The captured commit never entered the repository.
    assert!(
        !std::process::Command::new("git")
            .args(["-C", repo.to_str().unwrap(), "cat-file", "-e", &captured])
            .status()
            .unwrap()
            .success()
    );

    // Another runner's secret is refused, as for any run.
    let status = forge
        .bundle(Some(OTHER_SECRET), &attempt, &[])
        .await
        .unwrap_err();
    assert_eq!(status.code(), tonic::Code::Unauthenticated);
    // A replayed attempt fetches again; the stale one is refused.
    forge
        .exec(&format!(
            "UPDATE job_attempt SET state = 'lost', lease_expires_at = 1 WHERE public_id = '{attempt}';
             UPDATE job SET state = 'pending'"
        ))
        .await;
    let connect_response::Message::Assignment(replayed) = next(&mut stream).await else {
        panic!("expected an assignment");
    };
    assert_ne!(replayed.attempt_id, attempt);
    assert_eq!(
        forge
            .bundles(&replayed.attempt_id, &[&second])
            .await
            .unwrap()
            .len(),
        1
    );
    let status = forge.bundles(&attempt, &[]).await.unwrap_err();
    assert_eq!(
        (status.code(), reason(&status).as_str()),
        (tonic::Code::FailedPrecondition, "INVALID_STATE")
    );
}
