use super::*;
use crate::db::events::{Event, EventBus, Handler, append_event, events_after};
use std::{future::Future, pin::pin, task::Context};

struct TestHandler<const MIRROR: bool> {
    fail: bool,
}

impl<const MIRROR: bool> Handler for TestHandler<MIRROR> {
    const NAME: &'static str = if MIRROR { "mirror" } else { "ci" };

    async fn apply(
        &mut self,
        transaction: &mut SqliteConnection,
        event: &Event,
    ) -> Result<(), sqlx::Error> {
        sqlx::query("INSERT INTO test_effect (handler, event_id) VALUES (?, ?) ON CONFLICT(handler, event_id) DO NOTHING")
            .bind(Self::NAME)
            .bind(event.id)
            .execute(transaction)
            .await?;
        if self.fail {
            return Err(sqlx::Error::Protocol("failure after effect".into()));
        }
        Ok(())
    }
}

async fn setup(writer: &mut SqliteConnection) {
    sqlx::query("CREATE TABLE test_effect (seq INTEGER PRIMARY KEY, handler TEXT NOT NULL, event_id INTEGER NOT NULL, UNIQUE(handler, event_id))")
        .execute(writer).await.unwrap();
}

async fn append(transaction: &mut SqliteConnection, number: i64) -> i64 {
    append_event(transaction, &super::events::event(&format!("{number:026}")))
        .await
        .unwrap()
}

async fn effects(readers: &SqlitePool, handler: &str) -> Vec<i64> {
    sqlx::query_scalar("SELECT event_id FROM test_effect WHERE handler = ? ORDER BY seq")
        .bind(handler)
        .fetch_all(readers)
        .await
        .unwrap()
}

async fn checkpoint(readers: &SqlitePool, handler: &str) -> Option<i64> {
    sqlx::query_scalar("SELECT last_event_id FROM handler_checkpoint WHERE handler = ?")
        .bind(handler)
        .fetch_optional(readers)
        .await
        .unwrap()
}

#[tokio::test]
async fn rollback_never_dispatches_and_commit_wakes_after_visibility() {
    let dir = TempDir::new();
    let (mut writer, readers) = open(&dir.database()).await.unwrap();
    setup(&mut writer).await;
    let bus = EventBus::default();
    let mut handler = TestHandler::<false> { fail: false };
    let mut wake = pin!(bus.notified());
    let mut context = Context::from_waker(std::task::Waker::noop());
    assert!(wake.as_mut().poll(&mut context).is_pending());

    let mut transaction = writer.begin().await.unwrap();
    append(&mut transaction, 1).await;
    assert!(events_after(&readers, 0).await.unwrap().is_empty());
    assert!(wake.as_mut().poll(&mut context).is_pending());
    transaction.rollback().await.unwrap();
    assert!(!bus.dispatch_next(&mut writer, &mut handler).await.unwrap());
    assert!(effects(&readers, "ci").await.is_empty());
    assert_eq!(checkpoint(&readers, "ci").await, None);
    assert!(wake.as_mut().poll(&mut context).is_pending());

    let mut transaction = writer.begin().await.unwrap();
    let id = append(&mut transaction, 2).await;
    bus.commit(transaction).await.unwrap();
    assert!(wake.as_mut().poll(&mut context).is_ready());
    assert_eq!(events_after(&readers, 0).await.unwrap()[0].id, id);
    // A commit before a waiter starts also retains its wake-up permit.
    let transaction = writer.begin().await.unwrap();
    bus.commit(transaction).await.unwrap();
    let mut wake = pin!(bus.notified());
    assert!(wake.as_mut().poll(&mut context).is_ready());
    assert!(bus.dispatch_next(&mut writer, &mut handler).await.unwrap());
    assert_eq!(effects(&readers, "ci").await, [id]);
    assert_eq!(checkpoint(&readers, "ci").await, Some(id));
    readers.close().await;
    writer.close().await.unwrap();
}

#[tokio::test]
async fn failed_commit_does_not_wake_or_publish_event() {
    let dir = TempDir::new();
    let (mut writer, readers) = open(&dir.database()).await.unwrap();
    sqlx::query("CREATE TABLE test_parent (id INTEGER PRIMARY KEY); CREATE TABLE test_child (parent_id INTEGER REFERENCES test_parent(id) DEFERRABLE INITIALLY DEFERRED)")
        .execute(&mut writer).await.unwrap();
    let bus = EventBus::default();
    let mut wake = pin!(bus.notified());
    let mut context = Context::from_waker(std::task::Waker::noop());
    assert!(wake.as_mut().poll(&mut context).is_pending());
    let mut transaction = writer.begin().await.unwrap();
    append(&mut transaction, 1).await;
    sqlx::query("INSERT INTO test_child VALUES (999)")
        .execute(&mut *transaction)
        .await
        .unwrap();
    assert!(bus.commit(transaction).await.is_err());
    // Flush the rollback queued by the failed transaction's drop.
    sqlx::query("SELECT 1").execute(&mut writer).await.unwrap();
    assert!(wake.as_mut().poll(&mut context).is_pending());
    assert!(events_after(&readers, 0).await.unwrap().is_empty());
    readers.close().await;
    writer.close().await.unwrap();
}

#[tokio::test]
async fn effect_and_checkpoint_roll_back_on_handler_or_checkpoint_failure() {
    let dir = TempDir::new();
    let (mut writer, readers) = open(&dir.database()).await.unwrap();
    setup(&mut writer).await;
    let bus = EventBus::default();
    let mut handler = TestHandler::<false> { fail: true };
    let first = append(&mut writer, 1).await;
    assert!(bus.dispatch_next(&mut writer, &mut handler).await.is_err());
    sqlx::query("SELECT 1").execute(&mut writer).await.unwrap();
    assert!(effects(&readers, "ci").await.is_empty());
    assert_eq!(checkpoint(&readers, "ci").await, None);
    handler.fail = false;
    assert!(bus.dispatch_next(&mut writer, &mut handler).await.unwrap());
    let second = append(&mut writer, 2).await;
    handler.fail = true;
    let error = bus
        .dispatch_next(&mut writer, &mut handler)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("failure after effect"));
    sqlx::query("SELECT 1").execute(&mut writer).await.unwrap();
    assert_eq!(effects(&readers, "ci").await, [first]);
    assert_eq!(checkpoint(&readers, "ci").await, Some(first));

    handler.fail = false;
    // Test-only trigger injects a DB failure after apply succeeds.
    sqlx::query("CREATE TRIGGER fail_checkpoint BEFORE UPDATE ON handler_checkpoint BEGIN SELECT RAISE(ABORT, 'checkpoint failure'); END")
        .execute(&mut writer).await.unwrap();
    let error = bus
        .dispatch_next(&mut writer, &mut handler)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("checkpoint failure"));
    sqlx::query("DROP TRIGGER fail_checkpoint")
        .execute(&mut writer)
        .await
        .unwrap();
    assert_eq!(effects(&readers, "ci").await, [first]);
    assert_eq!(checkpoint(&readers, "ci").await, Some(first));
    assert!(bus.dispatch_next(&mut writer, &mut handler).await.unwrap());
    assert_eq!(effects(&readers, "ci").await, [first, second]);
    assert_eq!(checkpoint(&readers, "ci").await, Some(second));
    readers.close().await;
    writer.close().await.unwrap();
}

#[tokio::test]
async fn replay_from_old_checkpoint_does_not_duplicate_effects() {
    let dir = TempDir::new();
    let (mut writer, readers) = open(&dir.database()).await.unwrap();
    setup(&mut writer).await;
    let bus = EventBus::default();
    let mut handler = TestHandler::<false> { fail: false };
    let first = append(&mut writer, 2).await;
    let second = append(&mut writer, 1).await;
    while bus.dispatch_next(&mut writer, &mut handler).await.unwrap() {}
    sqlx::query("UPDATE handler_checkpoint SET last_event_id = 0 WHERE handler = 'ci'")
        .execute(&mut writer)
        .await
        .unwrap();
    while bus.dispatch_next(&mut writer, &mut handler).await.unwrap() {}
    assert_eq!(effects(&readers, "ci").await, [first, second]);
    assert_eq!(checkpoint(&readers, "ci").await, Some(second));
    readers.close().await;
    writer.close().await.unwrap();
}

#[tokio::test]
async fn handlers_progress_independently_in_event_id_order() {
    let dir = TempDir::new();
    let (mut writer, readers) = open(&dir.database()).await.unwrap();
    setup(&mut writer).await;
    let bus = EventBus::default();
    let first = append(&mut writer, 3).await;
    let second = append(&mut writer, 2).await;
    let third = append(&mut writer, 1).await;
    let mut ci = TestHandler::<false> { fail: true };
    let mut mirror = TestHandler::<true> { fail: false };
    assert!(bus.dispatch_next(&mut writer, &mut ci).await.is_err());
    while bus.dispatch_next(&mut writer, &mut mirror).await.unwrap() {}
    assert!(effects(&readers, "ci").await.is_empty());
    assert_eq!(checkpoint(&readers, "ci").await, None);
    assert_eq!(effects(&readers, "mirror").await, [first, second, third]);
    assert_eq!(checkpoint(&readers, "mirror").await, Some(third));
    ci.fail = false;
    assert!(bus.dispatch_next(&mut writer, &mut ci).await.unwrap());
    assert_eq!(checkpoint(&readers, "ci").await, Some(first));
    assert_eq!(checkpoint(&readers, "mirror").await, Some(third));
    while bus.dispatch_next(&mut writer, &mut ci).await.unwrap() {}
    assert_eq!(effects(&readers, "ci").await, [first, second, third]);
    assert_eq!(effects(&readers, "mirror").await, [first, second, third]);
    readers.close().await;
    writer.close().await.unwrap();
}

struct FakeClock {
    now: std::cell::Cell<std::time::Instant>,
    sleeps: std::cell::RefCell<Vec<std::time::Duration>>,
}

impl FakeClock {
    fn new() -> Self {
        Self {
            now: std::cell::Cell::new(std::time::Instant::now()),
            sleeps: Default::default(),
        }
    }
}

impl crate::db::events::Clock for FakeClock {
    fn now(&self) -> std::time::Instant {
        self.now.get()
    }
    async fn sleep(&self, duration: std::time::Duration) {
        self.sleeps.borrow_mut().push(duration);
        self.now.set(self.now.get() + duration);
    }
}

struct FailingHandler {
    remaining: usize,
    readers: SqlitePool,
    previous: Option<i64>,
    check_mirror: bool,
}

impl Handler for FailingHandler {
    const NAME: &'static str = "ci";
    async fn apply(
        &mut self,
        transaction: &mut SqliteConnection,
        event: &Event,
    ) -> Result<(), sqlx::Error> {
        // Every attempt must start with the previous transaction's effect absent.
        assert_eq!(checkpoint(&self.readers, "ci").await, self.previous);
        assert_eq!(
            effects(&self.readers, "ci").await,
            self.previous.into_iter().collect::<Vec<_>>()
        );
        if self.check_mirror && self.remaining == 1 {
            assert_eq!(checkpoint(&self.readers, "mirror").await, Some(event.id));
        }
        TestHandler::<false> { fail: false }
            .apply(transaction, event)
            .await?;
        if self.remaining > 0 {
            self.remaining -= 1;
            return Err(sqlx::Error::Protocol("injected failure".into()));
        }
        Ok(())
    }
}

#[tokio::test]
async fn retries_twice_with_rollback_counts_and_failure_logs() {
    use crate::db::events::Dispatcher;
    const CHILD: &str = "KAJIBA_RETRY_LOG_TEST";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "db::tests::dispatch::retries_twice_with_rollback_counts_and_failure_logs",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env_remove("RUST_LOG")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let lines: Vec<serde_json::Value> = String::from_utf8(output.stderr)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        for line in lines {
            assert_eq!(line["event"], "handler-failed");
            assert_eq!(line["level"], "error");
            assert_eq!(line["handler"], "ci");
            assert_eq!(line["event_id"], 2);
            assert!(line["err"].as_str().unwrap().contains("injected failure"));
        }
        return;
    }
    // Isolate the real stderr logger from parallel tests and tracing's callsite cache.
    kajiba_core::log::init("error").unwrap();
    let dir = TempDir::new();
    let (mut writer, readers) = open(&dir.database()).await.unwrap();
    setup(&mut writer).await;
    let bus = EventBus::default();
    let first = append(&mut writer, 1).await;
    bus.dispatch_next(&mut writer, &mut TestHandler::<false> { fail: false })
        .await
        .unwrap();
    let second = append(&mut writer, 2).await;
    let mut dispatcher = Dispatcher::default();
    dispatcher.register(FailingHandler {
        remaining: 2,
        readers: readers.clone(),
        previous: Some(first),
        check_mirror: true,
    });
    dispatcher.register(TestHandler::<true> { fail: false });
    let clock = FakeClock::new();
    let writer = tokio::sync::Mutex::new(writer);
    dispatcher.drain(&bus, &writer, &clock).await;
    let writer = writer.into_inner();
    assert_eq!(effects(&readers, "ci").await, [first, second]);
    assert_eq!(effects(&readers, "mirror").await, [first, second]);
    assert_eq!(checkpoint(&readers, "ci").await, Some(second));
    assert_eq!(bus.failure_count("ci"), 2);
    assert_eq!(bus.failure_count("mirror"), 0);
    assert_eq!(
        *clock.sleeps.borrow(),
        [
            std::time::Duration::from_millis(100),
            std::time::Duration::from_millis(200)
        ]
    );
    readers.close().await;
    writer.close().await.unwrap();
}

#[tokio::test]
async fn restart_dispatches_committed_event_without_notification_and_replay_is_unique() {
    use crate::db::events::Dispatcher;
    let dir = TempDir::new();
    let (mut writer, readers) = open(&dir.database()).await.unwrap();
    setup(&mut writer).await;
    let bus = EventBus::default();
    let first = append(&mut writer, 1).await;
    bus.dispatch_next(&mut writer, &mut TestHandler::<false> { fail: false })
        .await
        .unwrap();
    let mut transaction = writer.begin().await.unwrap();
    let second = append(&mut transaction, 2).await;
    bus.commit(transaction).await.unwrap();
    // Simulate death before dispatch: discard the bus, reopen the same durable DB.
    drop(bus);
    readers.close().await;
    writer.close().await.unwrap();
    let (writer, readers) = open(&dir.database()).await.unwrap();
    let bus = EventBus::default();
    let mut dispatcher = Dispatcher::default();
    dispatcher.register(TestHandler::<false> { fail: false });
    let clock = FakeClock::new();
    let writer = tokio::sync::Mutex::new(writer);
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::select! {
            () = dispatcher.run(&bus, &writer) => panic!("dispatcher stopped"),
            () = async {
                while checkpoint(&readers, "ci").await != Some(second) {
                    tokio::task::yield_now().await;
                }
            } => {},
        }
    })
    .await
    .expect("startup must replay without a wake-up");
    assert_eq!(effects(&readers, "ci").await, [first, second]);
    assert_eq!(checkpoint(&readers, "ci").await, Some(second));
    sqlx::query("UPDATE handler_checkpoint SET last_event_id = ? WHERE handler = 'ci'")
        .bind(first)
        .execute(&mut *writer.lock().await)
        .await
        .unwrap();
    dispatcher.drain(&bus, &writer, &clock).await;
    let writer = writer.into_inner();
    assert_eq!(effects(&readers, "ci").await, [first, second]);
    assert_eq!(checkpoint(&readers, "ci").await, Some(second));
    assert_eq!(bus.failure_count("ci"), 0);
    assert!(clock.sleeps.borrow().is_empty());
    readers.close().await;
    writer.close().await.unwrap();
}

#[tokio::test]
async fn retry_backoff_is_capped() {
    use crate::db::events::Dispatcher;
    let dir = TempDir::new();
    let (mut writer, readers) = open(&dir.database()).await.unwrap();
    setup(&mut writer).await;
    let bus = EventBus::default();
    let id = append(&mut writer, 1).await;
    let mut dispatcher = Dispatcher::default();
    dispatcher.register(FailingHandler {
        remaining: 12,
        readers: readers.clone(),
        previous: None,
        check_mirror: false,
    });
    let clock = FakeClock::new();
    let writer = tokio::sync::Mutex::new(writer);
    dispatcher.drain(&bus, &writer, &clock).await;
    let writer = writer.into_inner();
    let milliseconds: Vec<_> = clock
        .sleeps
        .borrow()
        .iter()
        .map(|duration| duration.as_millis())
        .collect();
    assert_eq!(
        milliseconds,
        [
            100, 200, 400, 800, 1600, 3200, 6400, 12800, 25600, 30000, 30000, 30000
        ]
    );
    assert_eq!(effects(&readers, "ci").await, [id]);
    assert_eq!(bus.failure_count("ci"), 12);
    readers.close().await;
    writer.close().await.unwrap();
}

#[tokio::test]
async fn metrics_count_each_failed_attempt_once_and_lag_tracks_checkpoints() {
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode},
    };
    use std::sync::Arc;
    use tonic::codegen::Service;
    let dir = TempDir::new();
    let (mut writer, readers) = open(&dir.database()).await.unwrap();
    setup(&mut writer).await;
    let bus = Arc::new(EventBus::default());
    let first = append(&mut writer, 1).await;
    let second = append(&mut writer, 2).await;
    // Stored names cannot create metric labels.
    sqlx::query("INSERT INTO handler_checkpoint (handler, last_event_id, updated_at) VALUES ('private', 0, 0)")
        .execute(&mut writer).await.unwrap();
    assert_eq!(crate::metrics::handler_lag(&readers).await.unwrap(), [2; 4]);
    let mut app = crate::metrics::router(Arc::default(), readers.clone(), bus.clone());
    let scrape =
        |app: &mut axum::Router| app.call(Request::get("/metrics").body(Body::empty()).unwrap());
    let response = scrape(&mut app).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["content-type"],
        crate::metrics::CONTENT_TYPE
    );
    let output = String::from_utf8(
        to_bytes(response.into_body(), 65536)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(output.contains("kajiba_handler_failures_total{handler=\"ci\"} 0\n"));
    let mut handler = TestHandler::<false> { fail: true };
    assert!(bus.dispatch_next(&mut writer, &mut handler).await.is_err());
    assert_eq!(bus.failure_count("ci"), 1);
    assert_eq!(crate::metrics::handler_lag(&readers).await.unwrap(), [2; 4]);
    let response = scrape(&mut app).await.unwrap();
    let output = String::from_utf8(
        to_bytes(response.into_body(), 65536)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(output.contains("kajiba_handler_failures_total{handler=\"ci\"} 1\n"));
    assert!(!output.contains("private"));
    assert_eq!(
        output
            .lines()
            .filter(|line| line.starts_with("kajiba_handler_failures_total{"))
            .count(),
        4
    );
    handler.fail = false;
    assert!(bus.dispatch_next(&mut writer, &mut handler).await.unwrap());
    assert_eq!(checkpoint(&readers, "ci").await, Some(first));
    assert_eq!(
        crate::metrics::handler_lag(&readers).await.unwrap(),
        [1, 2, 2, 2]
    );
    assert!(bus.dispatch_next(&mut writer, &mut handler).await.unwrap());
    assert_eq!(checkpoint(&readers, "ci").await, Some(second));
    assert!(!bus.dispatch_next(&mut writer, &mut handler).await.unwrap());
    assert_eq!(
        crate::metrics::handler_lag(&readers).await.unwrap(),
        [0, 2, 2, 2]
    );
    assert_eq!(bus.failure_count("ci"), 1);
    assert_eq!(
        app.call(Request::get("/other").body(Body::empty()).unwrap())
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        app.call(Request::post("/metrics").body(Body::empty()).unwrap())
            .await
            .unwrap()
            .status(),
        StatusCode::METHOD_NOT_ALLOWED
    );
    readers.close().await;
    assert_eq!(
        scrape(&mut app).await.unwrap().status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    writer.close().await.unwrap();
}

#[tokio::test]
async fn request_transactions_share_the_writer_with_a_running_dispatcher() {
    use crate::db::events::Dispatcher;
    let dir = TempDir::new();
    let (mut writer, readers) = open(&dir.database()).await.unwrap();
    setup(&mut writer).await;
    let writer = tokio::sync::Mutex::new(writer);
    let bus = EventBus::default();
    let mut dispatcher = Dispatcher::default();
    dispatcher.register(TestHandler::<false> { fail: false });
    let id = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::select! {
            () = dispatcher.run(&bus, &writer) => panic!("dispatcher stopped"),
            id = async {
                // A request path opens its own caller transaction while the dispatcher runs.
                let mut connection = writer.lock().await;
                let mut transaction = connection.begin().await.unwrap();
                let id = append(&mut transaction, 1).await;
                bus.commit(transaction).await.unwrap();
                drop(connection);
                while checkpoint(&readers, "ci").await != Some(id) {
                    tokio::task::yield_now().await;
                }
                id
            } => id,
        }
    })
    .await
    .expect("request transaction must not wait for the dispatcher");
    assert_eq!(effects(&readers, "ci").await, [id]);
    readers.close().await;
    writer.into_inner().close().await.unwrap();
}

/// Writes its effect, then never finishes: the child dies inside the dispatch transaction.
struct HangingHandler;

impl Handler for HangingHandler {
    const NAME: &'static str = "ci";

    async fn apply(
        &mut self,
        transaction: &mut SqliteConnection,
        event: &Event,
    ) -> Result<(), sqlx::Error> {
        TestHandler::<false> { fail: false }
            .apply(transaction, event)
            .await?;
        println!("DISPATCHING={}", event.id);
        std::future::pending().await
    }
}

#[tokio::test]
async fn sigkill_after_commit_replays_exactly_one_effect_on_restart() {
    use crate::db::events::Dispatcher;
    use std::io::BufRead;
    const CHILD: &str = "KAJIBA_CRASH_TEST_DATABASE";
    if let Some(path) = std::env::var_os(CHILD) {
        let (writer, _readers) = open(std::path::Path::new(&path)).await.unwrap();
        let writer = tokio::sync::Mutex::new(writer);
        let bus = EventBus::default();
        let mut dispatcher = Dispatcher::default();
        dispatcher.register(HangingHandler);
        tokio::join!(dispatcher.run(&bus, &writer), async {
            let mut connection = writer.lock().await;
            let mut transaction = connection.begin().await.unwrap();
            let id = append(&mut transaction, 1).await;
            bus.commit(transaction).await.unwrap();
            println!("COMMITTED={id}");
        });
        unreachable!("the parent kills this process");
    }
    let dir = TempDir::new();
    let (mut writer, readers) = open(&dir.database()).await.unwrap();
    setup(&mut writer).await;
    readers.close().await;
    writer.close().await.unwrap();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "db::tests::dispatch::sigkill_after_commit_replays_exactly_one_effect_on_restart",
            "--nocapture",
        ])
        .env(CHILD, dir.database())
        .env_remove("RUST_LOG")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let mut committed = None;
    let mut dispatching = false;
    for line in std::io::BufReader::new(child.stdout.take().unwrap()).lines() {
        let line = line.unwrap();
        if let Some(id) = line.strip_prefix("COMMITTED=") {
            committed = Some(id.parse::<i64>().unwrap());
        }
        if line.starts_with("DISPATCHING=") {
            dispatching = true;
            break;
        }
    }
    // Child::kill sends SIGKILL: no destructor, rollback or flush runs in the child.
    child.kill().unwrap();
    child.wait().unwrap();
    let id = committed.expect("child committed an event");
    assert!(dispatching, "child reached the handler");

    let (writer, readers) = open(&dir.database()).await.unwrap();
    assert!(effects(&readers, "ci").await.is_empty());
    assert_eq!(checkpoint(&readers, "ci").await, None);
    let writer = tokio::sync::Mutex::new(writer);
    let bus = EventBus::default();
    let mut dispatcher = Dispatcher::default();
    dispatcher.register(TestHandler::<false> { fail: false });
    dispatcher.drain(&bus, &writer, &FakeClock::new()).await;
    assert_eq!(effects(&readers, "ci").await, [id]);
    assert_eq!(checkpoint(&readers, "ci").await, Some(id));
    readers.close().await;
    writer.into_inner().close().await.unwrap();
}
