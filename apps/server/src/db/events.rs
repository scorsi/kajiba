use sqlx::{Connection, FromRow, Sqlite, SqliteConnection, SqlitePool, Transaction};
use tokio::sync::Notify;

/// One wake-up consumer drives all handlers against the single writer.
#[derive(Default)]
pub struct EventBus {
    wake: Notify,
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
        let mut transaction = writer.begin().await?;
        let event: Option<Event> = sqlx::query_as("SELECT id, public_id, kind, version, occurred_at, principal_kind, principal_id, repo_id, correlation_id, payload FROM event WHERE id > COALESCE((SELECT last_event_id FROM handler_checkpoint WHERE handler = ?), 0) ORDER BY id LIMIT 1")
            .bind(H::NAME)
            .fetch_optional(&mut *transaction)
            .await?;
        let Some(event) = event else {
            transaction.rollback().await?;
            return Ok(false);
        };
        handler.apply(&mut transaction, &event).await?;
        sqlx::query("INSERT INTO handler_checkpoint (handler, last_event_id, updated_at) VALUES (?, ?, CAST(strftime('%s', 'now') AS INTEGER) * 1000) ON CONFLICT(handler) DO UPDATE SET last_event_id = excluded.last_event_id, updated_at = excluded.updated_at")
            .bind(H::NAME)
            .bind(event.id)
            .execute(&mut *transaction)
            .await?;
        self.commit(transaction).await?;
        Ok(true)
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
