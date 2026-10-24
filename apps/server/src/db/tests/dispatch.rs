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
