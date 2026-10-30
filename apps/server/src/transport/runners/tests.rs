use super::*;
use crate::transport::{PUBLIC, auth::tests::reason, router, serve_tcp, tests::State};
use kajiba_core::config::ServerConfig;
use kajiba_proto::runner::v1::{
    Capabilities as Announced, runner_service_client::RunnerServiceClient,
};
use tokio::sync::watch;
use tonic::transport::Channel;

const SECRET: &str = "runner-secret";
const VERSION: &str = env!("CARGO_PKG_VERSION");

struct Forge {
    state: State,
    registry: Arc<Registry>,
    client: RunnerServiceClient<Channel>,
    server: tokio::task::JoinHandle<std::io::Result<()>>,
    _stop: watch::Sender<bool>,
}

impl Forge {
    /// `build-1` (aarch64-linux, nix and process, tag kvm, 2 jobs) is declared; `gone`
    /// was declared once and is disabled.
    async fn start() -> Self {
        let state = State::new();
        state.start().await;
        let dir = &state.health.state_dir;
        let token = dir.join("runner-token");
        std::fs::write(&token, format!("{SECRET}\n")).unwrap();
        let runner = |name: &str| {
            format!(
                "[runners.{name}]\ntoken_file = {:?}\n[runners.{name}.capabilities]\narch = 'aarch64'\nos = 'linux'\nbackends = ['nix', 'process']\ntags = ['kvm']\nmax_jobs = 2\n",
                token.display()
            )
        };
        let config = |runners: &str| {
            ServerConfig::parse(&format!(
                "[http]\npublic_url = 'https://example.org'\n{runners}"
            ))
            .unwrap()
        };
        let before = config(&format!("{}{}", runner("build-1"), runner("gone")));
        let config = config(&runner("build-1"));
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
async fn a_stream_must_open_with_a_hello_and_bundles_wait_for_their_task() {
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
    assert_eq!(status.code(), tonic::Code::Unimplemented);
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
