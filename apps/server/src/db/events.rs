use sqlx::{FromRow, SqliteConnection, SqlitePool};

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
