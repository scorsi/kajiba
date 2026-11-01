use super::*;
use crate::transport::{
    PUBLIC, auth::tests::reason, router, runner_router, serve_runners, serve_tcp, tests::State,
};
use kajiba_core::config::ServerConfig;
use kajiba_proto::runner::v1::{
    Capabilities as Announced, runner_service_client::RunnerServiceClient,
};
use tokio::sync::watch;
use tonic::transport::Channel;

const BUILD_1: &str = "spiffe://example.org/build-1/kajiba-runner";
const BUILD_2: &str = "spiffe://example.org/build-2/kajiba-runner";
/// The ID `gone` held while it was declared.
const GONE: &str = "spiffe://example.org/gone/kajiba-runner";
/// An ID the CA issued to a host that is no declared runner.
const UNDECLARED: &str = "spiffe://example.org/other/kajiba-runner";
/// The test client names its verified ID in this header, which [`verified`] turns into the
/// [`Peer`] extension the TLS listener sets (0124).
const PEER: &str = "x-test-spiffe-id";
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
    /// another ID) are declared; `gone` was declared once and is disabled.
    async fn state() -> State {
        let state = State::new();
        state.start().await;
        let runner = |name: &str| {
            format!(
                "[runners.{name}]\nspiffe_id = 'spiffe://example.org/{name}/kajiba-runner'\n[runners.{name}.capabilities]\narch = 'aarch64'\nos = 'linux'\nbackends = ['nix', 'process']\ntags = ['kvm']\nmax_jobs = 2\n",
            )
        };
        let config = |runners: &str| {
            ServerConfig::parse(&format!(
                "[http]\npublic_url = 'https://example.org'\n[runner_listener]\nlisten = '127.0.0.1:8443'\ntrust_domain = 'example.org'\nsvid_file = '/svid.pem'\nkey_file = '/key.pem'\nbundle_file = '/bundle.pem'\n{runners}"
            ))
            .unwrap()
        };
        let before = config(&format!("{}{}", runner("build-1"), runner("gone")));
        let config = config(&format!("{}{}", runner("build-1"), runner("build-2")));
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
            runners: watch::Sender::new(Arc::new(config.runners)),
            bus: Arc::default(),
            wake: Arc::default(),
        });
        state
    }

    async fn start() -> Self {
        let state = Self::state().await;
        let registry = Arc::new(Registry::default());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = runner_router(state.health.clone(), registry.clone())
            .layer(axum::middleware::from_fn(verified));
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
        peer: Option<&str>,
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
        if let Some(peer) = peer {
            request.metadata_mut().insert(PEER, peer.parse().unwrap());
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
            INSERT INTO repo (id, public_id, owner_id, name, visibility, ci_mode, default_branch, description, created_at, config_key) VALUES (1, '00000000000000000000000001', 1, 'demo', 'public', 'push', 'main', '', 0, 'demo')").await;
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

fn peer(id: &str) -> Peer {
    Peer {
        id: id.into(),
        not_after: std::time::UNIX_EPOCH + Duration::from_secs(4_102_444_800),
    }
}

/// Sets the [`Peer`] extension named by the [`PEER`] header, as the TLS listener does.
async fn verified(
    mut request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    if let Some(id) = request.headers().get(PEER).and_then(|id| id.to_str().ok()) {
        let peer = peer(id);
        request.extensions_mut().insert(peer);
    }
    next.run(request).await
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
async fn a_stalled_runner_does_not_block_matching_or_lease_expiry() {
    let state = Forge::state().await;
    let runners = Runners::new(state.health.clone(), Arc::default());
    let (stalled, _unread) = mpsc::channel(1);
    stalled.try_send(Ok(cancel("unknown"))).unwrap();
    let (stalled_id, generation, mut ended) = runners
        .admit(Some(&peer(BUILD_1)), &hello("build-1"), "test", stalled)
        .await
        .unwrap();
    let (healthy, mut received) = mpsc::channel(4);
    runners
        .admit(Some(&peer(BUILD_2)), &hello("build-2"), "test", healthy)
        .await
        .unwrap();
    let now = crate::db::runs::now();
    {
        let mut writer = state.health.writer.get().unwrap().lock().await;
        sqlx::raw_sql("INSERT INTO user (id, public_id, name, kind, is_admin, source, created_at) VALUES (1, '00000000000000000000000001', 'alice', 'local', 0, 'config', 0);
            INSERT INTO repo (id, public_id, owner_id, name, visibility, ci_mode, default_branch, description, created_at, config_key) VALUES (1, '00000000000000000000000001', 1, 'demo', 'public', 'push', 'main', '', 0, 'demo')")
            .execute(&mut *writer).await.unwrap();
        let mut tx = writer.begin().await.unwrap();
        for _ in 0..2 {
            crate::db::runs::create_run(
                &mut tx,
                &crate::db::runs::NewRun {
                    repo_id: 1,
                    sha: SHA,
                    reference: Some("refs/heads/main"),
                    trigger: "push",
                    event_id: None,
                    triggered_by: Some(1),
                    correlation_id: "test",
                    now,
                    worktree: None,
                },
            )
            .await
            .unwrap();
        }
        tx.commit().await.unwrap();
    }
    tokio::time::timeout(Duration::from_secs(1), runners.pass(now))
        .await
        .expect("a full runner queue blocked the scheduler");
    assert_eq!(ended.try_recv(), Err(oneshot::error::TryRecvError::Closed));
    assert!(!runners.live.lock().unwrap().1.contains_key("build-1"));
    assert!(matches!(
        received.try_recv().unwrap().unwrap().message,
        Some(connect_response::Message::Assignment(_))
    ));
    {
        let mut writer = state.health.writer.get().unwrap().lock().await;
        let connected: bool = sqlx::query_scalar("SELECT connected FROM runner WHERE id = ?")
            .bind(stalled_id)
            .fetch_one(&mut *writer)
            .await
            .unwrap();
        assert!(!connected);
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM event WHERE kind = 'runner-disconnected'"
            )
            .fetch_one(&mut *writer)
            .await
            .unwrap(),
            1
        );
    }
    // A late cleanup from the ended session is harmless.
    runners
        .disconnected(stalled_id, "build-1", generation, "test")
        .await;
    {
        let mut writer = state.health.writer.get().unwrap().lock().await;
        sqlx::query("UPDATE runner SET last_contact_at = ? WHERE name = 'build-2'")
            .bind(now + attempts::LEASE_MS + 1)
            .execute(&mut *writer)
            .await
            .unwrap();
    }
    tokio::time::timeout(
        Duration::from_secs(1),
        runners.pass(now + attempts::LEASE_MS + 1),
    )
    .await
    .expect("lease expiry stopped after disconnecting the stalled runner");
    assert!(matches!(
        received.try_recv().unwrap().unwrap().message,
        Some(connect_response::Message::Assignment(_))
    ));
    let mut writer = state.health.writer.get().unwrap().lock().await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM job_attempt WHERE state = 'lost'")
            .fetch_one(&mut *writer)
            .await
            .unwrap(),
        2
    );
}

#[tokio::test]
async fn a_frozen_runner_does_not_consume_replays_with_a_healthy_alternative() {
    let state = Forge::state().await;
    let runners = Runners::new(state.health.clone(), Arc::default());
    let (frozen, mut frozen_received) = mpsc::channel(4);
    let (frozen_id, _, _frozen_session) = runners
        .admit(Some(&peer(BUILD_1)), &hello("build-1"), "test", frozen)
        .await
        .unwrap();
    let (healthy, mut healthy_received) = mpsc::channel(4);
    let (healthy_id, _, _healthy_session) = runners
        .admit(Some(&peer(BUILD_2)), &hello("build-2"), "test", healthy)
        .await
        .unwrap();
    let now = crate::db::runs::now();
    {
        let mut writer = state.health.writer.get().unwrap().lock().await;
        sqlx::raw_sql("INSERT INTO user (id, public_id, name, kind, is_admin, source, created_at) VALUES (1, '00000000000000000000000001', 'alice', 'local', 0, 'config', 0);
            INSERT INTO repo (id, public_id, owner_id, name, visibility, ci_mode, default_branch, description, created_at, config_key) VALUES (1, '00000000000000000000000001', 1, 'demo', 'public', 'push', 'main', '', 0, 'demo')")
            .execute(&mut *writer).await.unwrap();
        let mut tx = writer.begin().await.unwrap();
        crate::db::runs::create_run(
            &mut tx,
            &crate::db::runs::NewRun {
                repo_id: 1,
                sha: SHA,
                reference: Some("refs/heads/main"),
                trigger: "push",
                event_id: None,
                triggered_by: Some(1),
                correlation_id: "test",
                now,
                worktree: None,
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }
    runners.pass(now).await;
    assert!(matches!(
        frozen_received.try_recv().unwrap().unwrap().message,
        Some(connect_response::Message::Assignment(_))
    ));
    // Both sockets stay open, but only build-2 keeps heartbeating.
    let replay_at = now + attempts::LEASE_MS + 1;
    {
        let mut writer = state.health.writer.get().unwrap().lock().await;
        attempts::heartbeat(&mut writer, healthy_id, replay_at)
            .await
            .unwrap();
    }
    runners.pass(replay_at).await;
    assert!(matches!(
        healthy_received.try_recv().unwrap().unwrap().message,
        Some(connect_response::Message::Assignment(_))
    ));
    assert!(frozen_received.try_recv().is_err());
    let mut writer = state.health.writer.get().unwrap().lock().await;
    let attempts: Vec<(i64, i64, String)> =
        sqlx::query_as("SELECT number, runner_id, state FROM job_attempt ORDER BY number")
            .fetch_all(&mut *writer)
            .await
            .unwrap();
    assert_eq!(
        attempts,
        [
            (1, frozen_id, "lost".into()),
            (2, healthy_id, "running".into())
        ]
    );
}

#[tokio::test]
async fn a_stream_ends_when_its_peer_svid_expires() {
    let state = Forge::state().await;
    let runners = Runners::new(state.health.clone(), Arc::default());
    let in_an_hour = std::time::SystemTime::now() + Duration::from_secs(3600);
    let expiring = Peer {
        id: BUILD_1.into(),
        not_after: in_an_hour,
    };
    // A full queue: the cut must not wait for the runner to read.
    let (outbound, _unread) = mpsc::channel(1);
    outbound.try_send(Ok(cancel("unknown"))).unwrap();
    let (_, _, mut ended) = runners
        .admit(Some(&expiring), &hello("build-1"), "test", outbound)
        .await
        .unwrap();
    let (outbound, _received) = mpsc::channel(1);
    let (_, _, mut unaffected) = runners
        .admit(Some(&peer(BUILD_2)), &hello("build-2"), "test", outbound)
        .await
        .unwrap();
    let at = |time: std::time::SystemTime| {
        time.duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64
    };
    runners.pass(at(in_an_hour) - 1).await;
    assert_eq!(ended.try_recv(), Err(oneshot::error::TryRecvError::Empty));

    tokio::time::timeout(Duration::from_secs(1), runners.pass(at(in_an_hour)))
        .await
        .expect("the cut blocked on an unread response queue");
    assert_eq!(ended.try_recv(), Err(oneshot::error::TryRecvError::Closed));
    assert_eq!(
        unaffected.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    );
    assert!(!runners.live.lock().unwrap().1.contains_key("build-1"));
    let mut writer = state.health.writer.get().unwrap().lock().await;
    assert!(
        !sqlx::query_scalar::<_, bool>("SELECT connected FROM runner WHERE name = 'build-1'")
            .fetch_one(&mut *writer)
            .await
            .unwrap()
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM event WHERE kind = 'runner-disconnected'"
        )
        .fetch_one(&mut *writer)
        .await
        .unwrap(),
        1
    );
    drop(writer);

    // A request on a connection whose SVID expired since its handshake is no runner's.
    let expired = Peer {
        id: BUILD_1.into(),
        not_after: std::time::SystemTime::now() - Duration::from_secs(1),
    };
    let (outbound, _received) = mpsc::channel(1);
    let status = runners
        .admit(Some(&expired), &hello("build-1"), "test", outbound)
        .await
        .unwrap_err();
    assert_eq!(status.code(), tonic::Code::Unauthenticated);
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
            Some(BUILD_1),
            hello("nobody"),
            tonic::Code::Unauthenticated,
            "CREDENTIAL_INVALID",
            "identity",
        ),
        (
            Some(BUILD_2),
            hello("build-1"),
            tonic::Code::Unauthenticated,
            "CREDENTIAL_INVALID",
            "identity",
        ),
        (
            Some(GONE),
            hello("gone"),
            tonic::Code::Unauthenticated,
            "CREDENTIAL_INVALID",
            "disabled",
        ),
        (
            Some(UNDECLARED),
            hello("build-1"),
            tonic::Code::Unauthenticated,
            "CREDENTIAL_INVALID",
            "identity",
        ),
        (
            None,
            hello("build-1"),
            tonic::Code::Unauthenticated,
            "CREDENTIAL_INVALID",
            "identity",
        ),
        (
            Some(BUILD_1),
            Hello {
                version: other_minor(),
                ..hello("build-1")
            },
            tonic::Code::FailedPrecondition,
            "VERSION_INCOMPATIBLE",
            "version",
        ),
        (
            Some(BUILD_1),
            capabilities(|c| c.arch = "x86_64".into()),
            tonic::Code::InvalidArgument,
            "INVALID_FIELD",
            "capabilities",
        ),
        (
            Some(BUILD_1),
            capabilities(|c| c.os = "darwin".into()),
            tonic::Code::InvalidArgument,
            "INVALID_FIELD",
            "capabilities",
        ),
        (
            Some(BUILD_1),
            capabilities(|c| c.backends.push("microvm".into())),
            tonic::Code::InvalidArgument,
            "INVALID_FIELD",
            "capabilities",
        ),
        (
            Some(BUILD_1),
            capabilities(|c| c.tags.push("gpu".into())),
            tonic::Code::InvalidArgument,
            "INVALID_FIELD",
            "capabilities",
        ),
        (
            Some(BUILD_1),
            capabilities(|c| c.max_jobs = 3),
            tonic::Code::InvalidArgument,
            "INVALID_FIELD",
            "capabilities",
        ),
        (
            Some(BUILD_1),
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
    for (peer, hello, code, expected, why) in cases {
        let version = hello.version.clone();
        let status = forge.connect(peer, hello).await.unwrap_err();
        assert_eq!(
            (status.code(), reason(&status).as_str()),
            (code, expected),
            "{why}"
        );
        if why == "version" {
            assert!(status.message().contains(&version) && status.message().contains(VERSION));
        }
        // Only a caller holding the runner's certificate may grow the journal (0108).
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
        ("identity", 4),
        ("disabled", 1),
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
            Some(BUILD_1),
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

    let (second, _second_stream) = forge
        .connect(Some(BUILD_1), hello("build-1"))
        .await
        .unwrap();
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

/// A runner's connector for the TLS listener's tests: TLS to `addr` whatever the URI,
/// ALPN `h2`, the runner's SVID from the fixtures.
#[derive(Clone)]
struct Tls(
    Arc<tokio_rustls::rustls::ClientConfig>,
    std::net::SocketAddr,
);

impl tonic::codegen::Service<tonic::codegen::http::Uri> for Tls {
    type Response = hyper_util::rt::TokioIo<tokio_rustls::client::TlsStream<tokio::net::TcpStream>>;
    type Error = std::io::Error;
    type Future =
        Pin<Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(
        &mut self,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, _: tonic::codegen::http::Uri) -> Self::Future {
        let Tls(config, addr) = self.clone();
        Box::pin(async move {
            let tcp = tokio::net::TcpStream::connect(addr).await?;
            let name = "runners.example.org".try_into().unwrap();
            let tls = tokio_rustls::TlsConnector::from(config)
                .connect(name, tcp)
                .await?;
            Ok(hyper_util::rt::TokioIo::new(tls))
        })
    }
}

fn svid(leaf: &str) -> Arc<kajiba_core::svid::Source> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/kajiba-core/tests/fixtures/svid");
    kajiba_core::svid::Source::load(kajiba_core::svid::Files {
        trust_domain: "example.org".into(),
        svid_file: dir.join(format!("{leaf}.pem")),
        key_file: dir.join(format!("{leaf}.key")),
        bundle_file: dir.join("ca.pem"),
    })
    .unwrap()
}

#[tokio::test]
async fn the_tls_listener_serves_runners_by_certificate_and_the_public_port_does_not() {
    let state = Forge::state().await;
    let registry = Arc::new(Registry::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (_stop, stopped) = watch::channel(false);
    let server = tokio::spawn(serve_runners(
        listener,
        svid("forge"),
        runner_router(state.health.clone(), registry.clone()),
        stopped,
    ));
    let client = |leaf: &str| {
        let config = svid(leaf).client_config("spiffe://example.org/forge/kajiba-server");
        let channel = tonic::transport::Endpoint::from_static("http://runners.example.org")
            .connect_with_connector_lazy(Tls(Arc::new(config), addr));
        RunnerServiceClient::new(channel)
    };
    let open = |client: RunnerServiceClient<Channel>, hello: Hello| async move {
        let (sender, receiver) = mpsc::channel(4);
        sender
            .send(ConnectRequest {
                message: Some(Message::Hello(hello)),
            })
            .await
            .unwrap();
        let stream = client.clone().connect(ReceiverStream::new(receiver)).await;
        (sender, stream)
    };

    // The certificate names build-1: its hello is admitted, another runner's name is not.
    let (_sender, admitted) = open(client("runner"), hello("build-1")).await;
    admitted.unwrap();
    let status = open(client("runner"), hello("build-2"))
        .await
        .1
        .unwrap_err();
    assert_eq!(
        (status.code(), reason(&status).as_str()),
        (tonic::Code::Unauthenticated, "CREDENTIAL_INVALID")
    );
    assert!(
        registry
            .exposition("test", [0; 4], [0; 4])
            .contains("kajiba_runner_refusals_total{reason=\"identity\"} 1")
    );
    // Another CA's certificate never reaches the service: the handshake fails.
    let status = open(client("foreign"), hello("build-1"))
        .await
        .1
        .unwrap_err();
    assert!(
        format!("{status:?}").contains("received fatal alert"),
        "{status:?}"
    );
    // A valid SVID of the trust domain whose ID is no declared runner's.
    let status = open(client("forge"), hello("build-1")).await.1.unwrap_err();
    assert_eq!(status.code(), tonic::Code::Unauthenticated);
    assert!(
        registry
            .exposition("test", [0; 4], [0; 4])
            .contains("kajiba_runner_refusals_total{reason=\"identity\"} 2")
    );
    server.abort();

    // kajiba.runner.v1 is not served on the public port (0124).
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(state.health.clone(), false, registry, "https://example.org").unwrap();
    let (_stop, stopped) = watch::channel(false);
    let server = tokio::spawn(serve_tcp(listener, app, PUBLIC, stopped));
    let channel = tonic::transport::Endpoint::from_shared(format!("http://{addr}"))
        .unwrap()
        .connect_lazy();
    let mut request = Request::new(ReceiverStream::new(mpsc::channel(1).1));
    request
        .metadata_mut()
        .insert(PEER, BUILD_1.parse().unwrap());
    let status = RunnerServiceClient::new(channel)
        .connect(request)
        .await
        .unwrap_err();
    assert_eq!(status.code(), tonic::Code::Unimplemented);
    server.abort();
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
        .insert(PEER, BUILD_1.parse().unwrap());
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
            (Some("warn"), Some("identity"), Some(""))
        );
        assert_eq!(refused[0]["spiffe_id"], UNDECLARED);
        assert!(
            !stderr.contains("not-a-runner"),
            "an unverified name is never logged"
        );
        assert_eq!(
            (
                refused[1]["reason"].as_str(),
                refused[1]["runner"].as_str(),
                refused[1]["spiffe_id"].as_str()
            ),
            (Some("identity"), Some("build-2"), Some(BUILD_2))
        );
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
        .connect(Some(UNDECLARED), hello("not-a-runner"))
        .await
        .unwrap_err();
    forge
        .connect(Some(BUILD_2), hello("build-1"))
        .await
        .unwrap_err();
    let (sender, stream) = forge
        .connect(Some(BUILD_1), hello("build-1"))
        .await
        .unwrap();
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
    let (sender, mut stream) = forge
        .connect(Some(BUILD_1), hello("build-1"))
        .await
        .unwrap();
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
    let (sender, mut stream) = forge
        .connect(Some(BUILD_1), hello("build-1"))
        .await
        .unwrap();
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
            produced_at_ms: 1234,
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
    let (sender, mut stream) = forge
        .connect(Some(BUILD_1), hello("build-1"))
        .await
        .unwrap();
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
            produced_at_ms: 1234,
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
            Some(BUILD_1),
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

#[tokio::test]
async fn a_restarted_runner_process_loses_its_unlisted_attempt_and_the_job_is_replayed() {
    use kajiba_proto::runner::v1::{Accept, Heartbeat};
    let forge = Forge::start().await;
    forge.run().await;
    let (sender, mut stream) = forge
        .connect(Some(BUILD_1), hello("build-1"))
        .await
        .unwrap();
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
    forge
        .until("SELECT count(*) FROM job WHERE state = 'running'", 1)
        .await;
    // A fresh process holds nothing: its hello lists no attempt.
    let (sender, mut stream) = forge
        .connect(Some(BUILD_1), hello("build-1"))
        .await
        .unwrap();
    let connect_response::Message::Assignment(replay) = next(&mut stream).await else {
        panic!("expected the replay's assignment");
    };
    assert_ne!(replay.attempt_id, attempt);
    let lease = "SELECT lease_expires_at FROM job_attempt WHERE number = 1";
    let before: Vec<(i64,)> = forge.sql(lease).await;
    send(&sender, Message::Heartbeat(Heartbeat {})).await;
    // Messages are handled in order: once the replay is accepted, the heartbeat was too.
    send(
        &sender,
        Message::Accept(Accept {
            attempt_id: replay.attempt_id.clone(),
        }),
    )
    .await;
    forge
        .until("SELECT count(*) FROM job WHERE state = 'running'", 1)
        .await;
    assert_eq!(forge.sql::<(i64,)>(lease).await, before);
    let attempts: Vec<(i64, String)> = forge
        .sql("SELECT number, state FROM job_attempt ORDER BY number")
        .await;
    assert_eq!(attempts, [(1, "lost".into()), (2, "running".into())]);
    let events: Vec<(String, String)> = forge
        .sql("SELECT kind, correlation_id FROM event WHERE kind IN ('attempt-lost', 'attempt-replayed') ORDER BY id")
        .await;
    assert_eq!(
        events,
        [
            ("attempt-lost".into(), attempt.clone()),
            ("attempt-replayed".into(), attempt)
        ]
    );
}

impl Forge {
    async fn bundle(
        &self,
        peer: Option<&str>,
        attempt: &str,
        have: &[&str],
    ) -> Result<String, Status> {
        let mut request = Request::new(FetchRunBundleRequest {
            attempt_id: attempt.into(),
            have: have.iter().map(|id| id.to_string()).collect(),
        });
        if let Some(peer) = peer {
            request.metadata_mut().insert(PEER, peer.parse().unwrap());
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
        INSERT INTO repo (id, public_id, owner_id, name, visibility, ci_mode, default_branch, description, created_at, config_key) VALUES (1, '00000000000000000000000001', 1, 'demo', 'public', 'push', 'main', '', 0, 'demo')").await;
    forge.run_at(&second).await;
    let (_sender, mut stream) = forge
        .connect(Some(BUILD_1), hello("build-1"))
        .await
        .unwrap();
    let connect_response::Message::Assignment(assignment) = next(&mut stream).await else {
        panic!("expected an assignment");
    };
    let attempt = assignment.attempt_id.as_str();

    // An empty cache gets the full history; a cache holding the parent only the new
    // commit; unknown and malformed ids are ignored.
    let full = forge.bundle(Some(BUILD_1), attempt, &[]).await.unwrap();
    assert!(
        full.starts_with(&format!("# v2 git bundle\n{second} refs/kajiba/run\n\n")),
        "{full}"
    );
    let incremental = forge
        .bundle(
            Some(BUILD_1),
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

    // Another runner's ID, no ID and an unknown attempt are not this runner.
    for (peer, attempt) in [
        (Some(BUILD_2), attempt),
        (None, attempt),
        (Some(BUILD_1), "7ZZZZZZZZZZZZZZZZZZZZZZZZZ"),
    ] {
        let status = forge.bundle(peer, attempt, &[]).await.unwrap_err();
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
    let status = forge.bundle(Some(BUILD_1), attempt, &[]).await.unwrap_err();
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
        .bundle(Some(BUILD_1), attempt, &[&second])
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
    let status = forge.bundle(Some(BUILD_1), attempt, &[]).await.unwrap_err();
    assert_eq!(status.code(), tonic::Code::FailedPrecondition);
    forge.exec(&format!("INSERT INTO job_attempt (public_id, job_id, number, runner_id, state, lease_expires_at, started_at)
        SELECT '7YYYYYYYYYYYYYYYYYYYYYYYYY', job_id, number + 1, runner_id, 'running', 9999999999999, 1 FROM job_attempt WHERE public_id = '{attempt}'")).await;
    forge
        .exec(&format!(
            "UPDATE job_attempt SET lease_expires_at = 9999999999999 WHERE public_id = '{attempt}'"
        ))
        .await;
    let status = forge.bundle(Some(BUILD_1), attempt, &[]).await.unwrap_err();
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
            .insert(PEER, BUILD_1.parse().unwrap());
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
        INSERT INTO repo (id, public_id, owner_id, name, visibility, ci_mode, default_branch, description, created_at, config_key) VALUES (1, '00000000000000000000000001', 1, 'demo', 'public', 'push', 'main', '', 0, 'demo')").await;
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
                    base_sha: "",
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
    let (_sender, mut stream) = forge
        .connect(Some(BUILD_1), hello("build-1"))
        .await
        .unwrap();
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

    // Another runner's ID is refused, as for any run.
    let status = forge
        .bundle(Some(BUILD_2), &attempt, &[])
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

#[tokio::test]
async fn runner_requests_waiting_for_admission_and_their_messages_are_bounded() {
    let forge = Forge::start().await;
    // Streams that never send their hello hold every admission place.
    let mut waiting = Vec::new();
    let mut senders = Vec::new();
    for _ in 0..MAX_PENDING {
        let (sender, receiver) = mpsc::channel::<ConnectRequest>(1);
        senders.push(sender);
        let mut client = forge.client.clone();
        waiting.push(tokio::spawn(async move {
            client
                .connect(Request::new(ReceiverStream::new(receiver)))
                .await
        }));
    }
    // Time for all of them to reach the server: one more is refused at once.
    tokio::time::sleep(Duration::from_secs(1)).await;
    let (_sender, receiver) = mpsc::channel::<ConnectRequest>(1);
    let status = tokio::time::timeout(
        Duration::from_secs(5),
        forge
            .client
            .clone()
            .connect(Request::new(ReceiverStream::new(receiver))),
    )
    .await
    .expect("refused before the hello timeout")
    .unwrap_err();
    assert_eq!(
        (status.code(), reason(&status).as_str()),
        (tonic::Code::ResourceExhausted, "RUNNER_ADMISSIONS_LIMIT")
    );
    // Places come back once the waiting requests are gone.
    for task in waiting {
        task.abort();
    }
    drop(senders);
    let mut admitted = false;
    for _ in 0..100 {
        if forge.connect(Some(BUILD_1), hello("build-1")).await.is_ok() {
            admitted = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(admitted);

    // A message above 128 KiB is refused before it reaches the handler.
    let mut large = hello("build-2");
    large.in_flight_attempt_ids = vec!["7".repeat(1024); 130];
    let status = forge.connect(Some(BUILD_2), large).await.unwrap_err();
    assert_eq!(status.code(), tonic::Code::OutOfRange, "{status:?}");
    let mut fits = hello("build-2");
    fits.in_flight_attempt_ids = vec!["7".repeat(1024); 120];
    assert!(forge.connect(Some(BUILD_2), fits).await.is_ok());
}

/// Incompressible bytes, so a bundle holding them stays as large.
fn noise(length: usize) -> Vec<u8> {
    let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
    (0..length)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect()
}

impl Forge {
    /// A bundle transfer whose first message was received, then left unread.
    async fn open_transfer(
        &self,
        attempt: &str,
    ) -> Result<Streaming<FetchRunBundleResponse>, Status> {
        let mut request = Request::new(FetchRunBundleRequest {
            attempt_id: attempt.into(),
            have: Vec::new(),
        });
        request
            .metadata_mut()
            .insert(PEER, BUILD_1.parse().unwrap());
        let mut stream = self
            .client
            .clone()
            .fetch_run_bundle(request)
            .await?
            .into_inner();
        stream.message().await?;
        Ok(stream)
    }
}

#[tokio::test]
async fn bundle_transfers_are_bounded_per_runner_and_end_with_their_stream_or_lease() {
    let forge = Forge::start().await;
    let git_dir = forge.state.health.git_state_dir.clone();
    let repo = git_dir.join("repos/00000000000000000000000001.git");
    git(&git_dir, &["init", "-q", "--bare", repo.to_str().unwrap()]);
    // Far more than the HTTP/2 window and the channels: an unread transfer stays open.
    let work = git_dir.join("work");
    git(&git_dir, &["init", "-q", work.to_str().unwrap()]);
    std::fs::write(work.join("big"), noise(8 << 20)).unwrap();
    git(&work, &["add", "big"]);
    git(&work, &["commit", "-q", "-m", "big"]);
    git(
        &work,
        &["push", "-q", repo.to_str().unwrap(), "HEAD:refs/heads/main"],
    );
    let sha = git(&work, &["rev-parse", "HEAD"]);
    forge.exec("INSERT INTO user (id, public_id, name, kind, is_admin, source, created_at) VALUES (1, '00000000000000000000000001', 'alice', 'local', 0, 'config', 0);
        INSERT INTO repo (id, public_id, owner_id, name, visibility, ci_mode, default_branch, description, created_at, config_key) VALUES (1, '00000000000000000000000001', 1, 'demo', 'public', 'push', 'main', '', 0, 'demo')").await;
    forge.run_at(&sha).await;
    let (_sender, mut stream) = forge
        .connect(Some(BUILD_1), hello("build-1"))
        .await
        .unwrap();
    let connect_response::Message::Assignment(assignment) = next(&mut stream).await else {
        panic!("expected an assignment");
    };
    let attempt = assignment.attempt_id.as_str();

    // `build-1` declares max_jobs 2: a third transfer at once is refused.
    let first = forge.open_transfer(attempt).await.unwrap();
    let second = forge.open_transfer(attempt).await.unwrap();
    let status = forge.open_transfer(attempt).await.unwrap_err();
    assert_eq!(
        (status.code(), reason(&status).as_str()),
        (tonic::Code::ResourceExhausted, "BUNDLE_TRANSFERS_LIMIT")
    );
    // An ended stream gives its place back once its git child is killed and reaped.
    drop(first);
    let mut third = None;
    for _ in 0..250 {
        if let Ok(stream) = forge.open_transfer(attempt).await {
            third = Some(stream);
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let third = third.expect("the ended transfer's place is free again");

    // A lost lease ends the transfers in progress with a refusal.
    forge
        .exec(&format!(
            "UPDATE job_attempt SET lease_expires_at = 1 WHERE public_id = '{attempt}'"
        ))
        .await;
    // Unread meanwhile: the cut comes from the lease check, not from delivery.
    tokio::time::sleep(TRANSFER_LEASE_CHECK * 3).await;
    for mut transfer in [second, third] {
        let status = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                match transfer.message().await {
                    Ok(Some(_)) => {}
                    Ok(None) => panic!("a transfer without a lease completed"),
                    Err(status) => return status,
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(
            (status.code(), reason(&status).as_str()),
            (tonic::Code::FailedPrecondition, "INVALID_STATE")
        );
    }
}

#[tokio::test]
async fn stopping_ends_runner_streams_with_unavailable() {
    let forge = Forge::start().await;
    let (_sender, mut stream) = forge
        .connect(Some(BUILD_1), hello("build-1"))
        .await
        .unwrap();
    forge.state.health.stop.send_replace(true);
    let ended = tokio::time::timeout(Duration::from_secs(5), stream.message())
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(ended.code(), tonic::Code::Unavailable);
}

#[tokio::test]
async fn a_reload_cuts_only_the_streams_whose_declaration_went_or_changed_identity() {
    let forge = Forge::start().await;
    let streams = [BUILD_1, BUILD_2].map(|id| (id, id.split('/').nth(3).unwrap()));
    let mut open = Vec::new();
    for (id, name) in streams {
        open.push(forge.connect(Some(id), hello(name)).await.unwrap());
    }
    let mut runners = forge
        .state
        .health
        .runners
        .get()
        .unwrap()
        .current()
        .as_ref()
        .clone();
    // build-1 gets another ID; build-2 only other capabilities.
    runners.get_mut("build-1").unwrap().spiffe_id = UNDECLARED.into();
    runners.get_mut("build-2").unwrap().capabilities.max_jobs = 1;
    forge.state.health.reloaded(runners.clone());
    let ended = tokio::time::timeout(Duration::from_secs(5), open[0].1.message())
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(ended.code(), tonic::Code::Unavailable);
    assert!(
        tokio::time::timeout(Duration::from_millis(300), open[1].1.message())
            .await
            .is_err(),
        "build-2's stream stays open"
    );
    // A removed declaration is cut too.
    runners.remove("build-2");
    forge.state.health.reloaded(runners);
    let ended = tokio::time::timeout(Duration::from_secs(5), open[1].1.message())
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(ended.code(), tonic::Code::Unavailable);
}

#[tokio::test]
async fn cancelling_a_silent_running_job_pushes_cancel_and_ignores_late_results() {
    use kajiba_proto::v1::{CancelRunRequest, Scope, run_service_server::RunService};
    let forge = Forge::start().await;
    let run = forge.run().await;
    let (sender, mut stream) = forge
        .connect(Some(BUILD_1), hello("build-1"))
        .await
        .unwrap();
    let connect_response::Message::Assignment(assignment) = next(&mut stream).await else {
        panic!("expected assignment");
    };
    sender
        .send(ConnectRequest {
            message: Some(Message::Accept(kajiba_proto::runner::v1::Accept {
                attempt_id: assignment.attempt_id.clone(),
            })),
        })
        .await
        .unwrap();
    forge
        .until("SELECT count(*) FROM run WHERE state = 'running'", 1)
        .await;
    let request = || {
        let mut request = Request::new(CancelRunRequest { run: run.clone() });
        request
            .extensions_mut()
            .insert(super::super::auth::Principal {
                user_id: 1,
                scopes: vec![Scope::CiTrigger],
                session: None,
            });
        request
    };
    let runs = super::super::runs::Runs(forge.state.health.clone());
    let cancelled = runs.cancel_run(request()).await.unwrap().into_inner();
    assert_eq!(
        next(&mut stream).await,
        connect_response::Message::CancelAttempt(CancelAttempt {
            attempt_id: assignment.attempt_id.clone(),
        })
    );
    sender
        .send(ConnectRequest {
            message: Some(Message::Result(kajiba_proto::runner::v1::Result {
                attempt_id: assignment.attempt_id.clone(),
                state: ResultState::Succeeded.into(),
                exit_code: Some(0),
                ..Default::default()
            })),
        })
        .await
        .unwrap();
    assert_eq!(
        next(&mut stream).await,
        connect_response::Message::ResultAck(ResultAck {
            attempt_id: assignment.attempt_id,
        })
    );
    assert_eq!(
        runs.cancel_run(request()).await.unwrap().into_inner(),
        cancelled
    );
    forge
        .until("SELECT count(*) FROM event WHERE kind = 'run-finished'", 1)
        .await;
    assert_eq!(
        forge.sql::<(String,)>("SELECT state FROM run").await,
        [("cancelled".into(),)]
    );
    assert!(forge.metric("kajiba_runs_total{result=\"cancelled\"} 1"));
}

#[tokio::test]
async fn maintenance_defers_runner_messages_disconnect_and_reconnect() {
    let state = Forge::state().await;
    let runners = Runners::new(state.health.clone(), Arc::default());
    let (out, _received) = mpsc::channel(4);
    let (id, generation, _ended) = runners
        .admit(Some(&peer(BUILD_1)), &hello("build-1"), "test", out)
        .await
        .unwrap();
    let closed = state.health.maintenance.close().unwrap();
    let heartbeat = runners.handle(
        (id, "build-1"),
        "test",
        Message::Heartbeat(Default::default()),
    );
    let disconnect = runners.disconnected(id, "build-1", generation, "test");
    let (out, _received) = mpsc::channel(4);
    let identity = peer(BUILD_2);
    let greeting = hello("build-2");
    let reconnect = runners.admit(Some(&identity), &greeting, "test", out);
    tokio::pin!(heartbeat, disconnect, reconnect);
    for _ in 0..2 {
        assert!(
            tokio::time::timeout(Duration::from_millis(10), &mut heartbeat)
                .await
                .is_err()
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(10), &mut disconnect)
                .await
                .is_err()
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(10), &mut reconnect)
                .await
                .is_err()
        );
    }
    // Scheduler expiry/assignment is a no-op even with a far-future clock.
    runners
        .pass(crate::db::runs::now() + attempts::LEASE_MS * 10)
        .await;
    let connected: i64 = sqlx::query_scalar("SELECT connected FROM runner WHERE id = ?")
        .bind(id)
        .fetch_one(&mut *state.health.writer.get().unwrap().lock().await)
        .await
        .unwrap();
    assert_eq!(connected, 1);
    drop(closed);
    assert!(heartbeat.await.unwrap().is_empty());
    disconnect.await;
    reconnect.await.unwrap();
    assert!(!runners.live.lock().unwrap().1.contains_key("build-1"));
    assert!(runners.live.lock().unwrap().1.contains_key("build-2"));
}

#[tokio::test]
async fn maintenance_extends_live_leases_once_without_reviving_expired_attempts() {
    let state = Forge::state().await;
    let writer = state.health.writer.get().unwrap();
    {
        let mut writer = writer.lock().await;
        sqlx::raw_sql("INSERT INTO user (id, public_id, name, kind, is_admin, source, created_at) VALUES (1, '00000000000000000000000001', 'alice', 'local', 0, 'config', 0);
            INSERT INTO repo (id, public_id, owner_id, name, visibility, ci_mode, default_branch, description, created_at, config_key) VALUES (1, '00000000000000000000000001', 1, 'demo', 'public', 'push', 'main', '', 0, 'demo');
            INSERT INTO run (id, public_id, repo_id, sha, trigger, state, created_at) VALUES (1, '00000000000000000000000001', 1, 'a', 'manual', 'running', 0);
            INSERT INTO job (id, public_id, run_id, name, requirements, replayable, max_attempts, state, created_at) VALUES (1, '00000000000000000000000001', 1, 'flake-check', '{}', 1, 3, 'running', 0);
            INSERT INTO job_attempt (id, public_id, job_id, number, runner_id, state, lease_expires_at, started_at) VALUES
            (1, '00000000000000000000000001', 1, 1, 1, 'running', 0, 0),
            (2, '00000000000000000000000002', 1, 2, 1, 'failed', 0, 0),
            (3, '00000000000000000000000003', 1, 3, 1, 'running', 0, 0)").execute(&mut *writer).await.unwrap();
        sqlx::query("UPDATE job_attempt SET lease_expires_at = ? WHERE id = 3")
            .bind(crate::db::runs::now() + 60000)
            .execute(&mut *writer)
            .await
            .unwrap();
    }
    let before: Vec<i64> =
        sqlx::query_scalar("SELECT lease_expires_at FROM job_attempt ORDER BY id")
            .fetch_all(&mut *writer.lock().await)
            .await
            .unwrap();
    let closed = state.health.maintenance.close().unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;
    drop(closed);
    state
        .health
        .maintenance
        .resume_leases(&mut *writer.lock().await)
        .await
        .unwrap();
    let after: Vec<i64> =
        sqlx::query_scalar("SELECT lease_expires_at FROM job_attempt ORDER BY id")
            .fetch_all(&mut *writer.lock().await)
            .await
            .unwrap();
    assert_eq!(&after[..2], &[0, 0]);
    assert!(after[2] >= before[2] + 30);
    state
        .health
        .maintenance
        .resume_leases(&mut *writer.lock().await)
        .await
        .unwrap();
    let again: Vec<i64> =
        sqlx::query_scalar("SELECT lease_expires_at FROM job_attempt ORDER BY id")
            .fetch_all(&mut *writer.lock().await)
            .await
            .unwrap();
    assert_eq!(again, after);
}

#[tokio::test]
async fn a_reconnected_runner_cancels_an_attempt_invalidated_by_restore() {
    use kajiba_proto::runner::v1::LogChunk;
    let state = Forge::state().await;
    let runners = Runners::new(state.health.clone(), Arc::default());
    let (out, mut received) = mpsc::channel(4);
    let (id, generation, _ended) = runners
        .admit(Some(&peer(BUILD_1)), &hello("build-1"), "test", out)
        .await
        .unwrap();
    let now = crate::db::runs::now();
    {
        let mut writer = state.health.writer.get().unwrap().lock().await;
        sqlx::raw_sql("INSERT INTO user (id, public_id, name, kind, is_admin, source, created_at) VALUES (1, '00000000000000000000000001', 'alice', 'local', 0, 'config', 0);
            INSERT INTO repo (id, public_id, owner_id, name, visibility, ci_mode, default_branch, description, created_at, config_key) VALUES (1, '00000000000000000000000001', 1, 'demo', 'public', 'push', 'main', '', 0, 'demo')")
            .execute(&mut *writer).await.unwrap();
        let mut tx = writer.begin().await.unwrap();
        crate::db::runs::create_run(
            &mut tx,
            &crate::db::runs::NewRun {
                repo_id: 1,
                sha: SHA,
                reference: Some("refs/heads/main"),
                trigger: "push",
                event_id: None,
                triggered_by: Some(1),
                correlation_id: "test",
                now,
                worktree: None,
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }
    runners.pass(now).await;
    let Some(connect_response::Message::Assignment(assignment)) =
        received.recv().await.unwrap().unwrap().message
    else {
        panic!("expected an assignment");
    };
    let attempt = assignment.attempt_id;
    let answers = runners
        .handle(
            (id, "build-1"),
            "test",
            Message::LogChunk(LogChunk {
                attempt_id: attempt.clone(),
                sequence: 1,
                data: b"already acknowledged".to_vec(),
                ..Default::default()
            }),
        )
        .await
        .unwrap();
    assert_eq!(
        answers,
        [response(connect_response::Message::LogAck(LogAck {
            attempt_id: attempt.clone(),
            sequence: 1
        }))]
    );
    runners
        .disconnected(id, "build-1", generation, "test")
        .await;
    {
        let mut writer = state.health.writer.get().unwrap().lock().await;
        sqlx::query("DELETE FROM log_chunk")
            .execute(&mut *writer)
            .await
            .unwrap();
        std::fs::write(state.health.state_dir.join("db/restore-pending"), b"").unwrap();
        crate::restore::reconcile(&mut writer, &state.health.state_dir, now)
            .await
            .unwrap();
    }
    let greeting = Hello {
        in_flight_attempt_ids: vec![attempt.clone()],
        ..hello("build-1")
    };
    let (out, _received) = mpsc::channel(4);
    runners
        .admit(Some(&peer(BUILD_1)), &greeting, "test", out)
        .await
        .unwrap();
    assert_eq!(
        runners
            .resume(id, &greeting.in_flight_attempt_ids)
            .await
            .unwrap(),
        [cancel(&attempt)]
    );
}
