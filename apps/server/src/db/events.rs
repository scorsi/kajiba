use sqlx::{Connection, FromRow, Sqlite, SqliteConnection, SqlitePool, Transaction};
use std::{
    collections::BTreeMap,
    future::Future,
    pin::Pin,
    time::{Duration, Instant},
};
use tokio::sync::{Mutex, Notify};

/// One wake-up consumer drives all handlers against the single shared writer.
#[derive(Default)]
pub struct EventBus {
    wake: Notify,
    failures: std::sync::Mutex<BTreeMap<&'static str, u64>>,
}

impl EventBus {
    /// Use for transactions that append events, including handler transactions.
    pub async fn commit(&self, transaction: Transaction<'_, Sqlite>) -> Result<(), sqlx::Error> {
        transaction.commit().await?;
        self.wake.notify_one();
        Ok(())
    }

    /// Wake-ups coalesce; the consumer must drain every handler before waiting again.
    pub async fn notified(&self) {
        self.wake.notified().await;
    }

    /// Process at most one event, so a failed handler need not block other handlers.
    pub async fn dispatch_next<H: Handler>(
        &self,
        writer: &mut SqliteConnection,
        handler: &mut H,
    ) -> Result<bool, sqlx::Error> {
        self.dispatch(writer, handler).await
    }

    /// In-process total for task 22's per-handler metric; resets on restart.
    pub fn failure_count(&self, handler: &str) -> u64 {
        self.failures
            .lock()
            .unwrap()
            .get(handler)
            .copied()
            .unwrap_or(0)
    }

    async fn dispatch(
        &self,
        writer: &mut SqliteConnection,
        handler: &mut dyn RegisteredHandler,
    ) -> Result<bool, sqlx::Error> {
        let name = handler.name();
        let mut transaction = writer.begin().await?;
        let event: Option<Event> = sqlx::query_as("SELECT id, public_id, kind, version, occurred_at, principal_kind, principal_id, repo_id, correlation_id, payload FROM event WHERE id > COALESCE((SELECT last_event_id FROM handler_checkpoint WHERE handler = ?), 0) ORDER BY id LIMIT 1")
            .bind(name)
            .fetch_optional(&mut *transaction)
            .await?;
        let Some(event) = event else {
            transaction.rollback().await?;
            return Ok(false);
        };
        let result = async {
            handler.apply(&mut transaction, &event).await?;
            sqlx::query("INSERT INTO handler_checkpoint (handler, last_event_id, updated_at) VALUES (?, ?, CAST(strftime('%s', 'now') AS INTEGER) * 1000) ON CONFLICT(handler) DO UPDATE SET last_event_id = excluded.last_event_id, updated_at = excluded.updated_at")
                .bind(name)
                .bind(event.id)
                .execute(&mut *transaction)
                .await?;
            self.commit(transaction).await
        }.await;
        if let Err(err) = result {
            {
                let mut failures = self.failures.lock().unwrap();
                let count = failures.entry(name).or_default();
                *count = count.saturating_add(1);
            }
            tracing::error!(event = "handler-failed", handler = name, event_id = event.id, err = %err);
            // Flush sqlx's queued rollback before another handler or retry runs.
            sqlx::query("SELECT 1").execute(writer).await?;
            return Err(err);
        }
        Ok(true)
    }
}

// ADR 0017 specifies capped backoff but no duration: 100 ms, doubling to 30 s.
const RETRY_INITIAL: Duration = Duration::from_millis(100);
const RETRY_CAP: Duration = Duration::from_secs(30);

type ApplyFuture<'a> = Pin<Box<dyn Future<Output = Result<(), sqlx::Error>> + 'a>>;

trait RegisteredHandler {
    fn name(&self) -> &'static str;
    fn apply<'a>(
        &'a mut self,
        transaction: &'a mut SqliteConnection,
        event: &'a Event,
    ) -> ApplyFuture<'a>;
}

impl<H: Handler> RegisteredHandler for H {
    fn name(&self) -> &'static str {
        H::NAME
    }
    fn apply<'a>(
        &'a mut self,
        transaction: &'a mut SqliteConnection,
        event: &'a Event,
    ) -> ApplyFuture<'a> {
        Box::pin(Handler::apply(self, transaction, event))
    }
}

struct HandlerState {
    handler: Box<dyn RegisteredHandler>,
    retry_at: Option<Instant>,
    backoff: Duration,
}

#[derive(Default)]
pub struct Dispatcher {
    handlers: Vec<HandlerState>,
}

pub trait Clock {
    fn now(&self) -> Instant;
    async fn sleep(&self, duration: Duration);
}

struct SystemClock;
impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
    async fn sleep(&self, duration: Duration) {
        tokio::time::sleep(duration).await;
    }
}

impl Dispatcher {
    pub fn register<H: Handler + 'static>(&mut self, handler: H) {
        assert!(
            !self
                .handlers
                .iter()
                .any(|state| state.handler.name() == H::NAME),
            "duplicate handler name"
        );
        self.handlers.push(HandlerState {
            handler: Box::new(handler),
            retry_at: None,
            backoff: RETRY_INITIAL,
        });
    }

    /// Drain immediately on startup, before waiting for any commit notification.
    pub async fn run(&mut self, bus: &EventBus, writer: &Mutex<SqliteConnection>) {
        loop {
            self.drain(bus, writer, &SystemClock).await;
            bus.notified().await;
        }
    }

    /// Retry deadlines belong to individual handlers; a stuck handler cannot block others.
    pub async fn drain(
        &mut self,
        bus: &EventBus,
        writer: &Mutex<SqliteConnection>,
        clock: &impl Clock,
    ) {
        loop {
            let mut progressed = false;
            for state in &mut self.handlers {
                if state
                    .retry_at
                    .is_some_and(|deadline| deadline > clock.now())
                {
                    continue;
                }
                // Hold the writer for one event only, so request transactions interleave.
                let result = bus
                    .dispatch(&mut *writer.lock().await, state.handler.as_mut())
                    .await;
                match result {
                    Ok(handled) => {
                        state.retry_at = None;
                        state.backoff = RETRY_INITIAL;
                        progressed |= handled;
                    }
                    Err(_) => {
                        state.retry_at = Some(clock.now() + state.backoff);
                        state.backoff = (state.backoff * 2).min(RETRY_CAP);
                    }
                }
            }
            if progressed {
                continue;
            }
            let Some(deadline) = self
                .handlers
                .iter()
                .filter_map(|state| state.retry_at)
                .min()
            else {
                return;
            };
            // Commits may make work available to healthy handlers during a retry wait.
            tokio::select! {
                () = clock.sleep(deadline.saturating_duration_since(clock.now())) => {},
                () = bus.notified() => {},
            }
        }
    }
}

pub trait Handler {
    /// Persistent checkpoint identity; never rename a deployed handler.
    const NAME: &'static str;

    /// Write only through this connection. Effects must have a unique event-derived
    /// key and tolerate conflicts on replay; external effects must enqueue DB jobs.
    async fn apply(
        &mut self,
        transaction: &mut SqliteConnection,
        event: &Event,
    ) -> Result<(), sqlx::Error>;
}

#[derive(Debug, PartialEq, Eq, FromRow)]
pub struct NewEvent {
    pub public_id: String,
    pub kind: String,
    pub version: i64,
    pub occurred_at: i64,
    pub principal_kind: String,
    pub principal_id: Option<i64>,
    pub repo_id: Option<i64>,
    pub correlation_id: String,
    pub payload: String,
}

#[derive(Debug, PartialEq, Eq, FromRow)]
pub struct Event {
    pub id: i64,
    #[sqlx(flatten)]
    pub data: NewEvent,
}

/// Pass the caller's transaction connection, then commit through `EventBus::commit`
/// so the reported change and event become visible before the dispatcher wakes.
pub async fn append_event(
    writer: &mut SqliteConnection,
    event: &NewEvent,
) -> Result<i64, sqlx::Error> {
    let result = sqlx::query("INSERT INTO event (public_id, kind, version, occurred_at, principal_kind, principal_id, repo_id, correlation_id, payload) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)")
        .bind(&event.public_id)
        .bind(&event.kind)
        .bind(event.version)
        .bind(event.occurred_at)
        .bind(&event.principal_kind)
        .bind(event.principal_id)
        .bind(event.repo_id)
        .bind(&event.correlation_id)
        .bind(&event.payload)
        .execute(writer)
        .await?;
    Ok(result.last_insert_rowid())
}

pub async fn events_after(readers: &SqlitePool, id: i64) -> Result<Vec<Event>, sqlx::Error> {
    sqlx::query_as("SELECT id, public_id, kind, version, occurred_at, principal_kind, principal_id, repo_id, correlation_id, payload FROM event WHERE id > ? ORDER BY id")
        .bind(id)
        .fetch_all(readers)
        .await
}
