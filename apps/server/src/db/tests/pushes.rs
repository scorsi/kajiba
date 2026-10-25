use super::repos::{State, git};
use crate::db::{
    events::EventBus,
    pushes::{Gate, RefUpdate, Refused, ZERO},
};
use kajiba_gitstore::Gix;

fn update(name: &str, old: &str, new: &str) -> RefUpdate {
    RefUpdate {
        name: format!("refs/heads/{name}"),
        old: old.into(),
        new: new.into(),
    }
}

/// A declared repository with `main` and `gone` on c1, and a second commit c2.
async fn setup() -> (State, std::path::PathBuf, String, String) {
    let mut state = State::new().await;
    state
        .apply("[repos.demo]\nowner='alice'\ndescription=''\n")
        .await
        .unwrap();
    let (public_id, _, _) = state.repo("demo").await;
    let path = state.path(&public_id);
    let tree = git(&path, &["mktree"], "");
    let c1 = git(&path, &["commit-tree", &tree, "-m", "one"], "");
    let c2 = git(&path, &["commit-tree", &tree, "-p", &c1, "-m", "two"], "");
    git(&path, &["update-ref", "refs/heads/main", &c1], "");
    git(&path, &["update-ref", "refs/heads/gone", &c1], "");
    (state, path, c1, c2)
}

async fn events(state: &mut State) -> Vec<String> {
    state
        .strings("SELECT principal_kind || ' ' || ifnull(principal_id, '-') || ' ' || repo_id || ' ' || correlation_id || ' ' || payload FROM event WHERE kind = 'push' ORDER BY id")
        .await
}

fn payload(name: &str, old: &str, new: &str) -> String {
    format!(r#"{{"new":"{new}","old":"{old}","ref":"refs/heads/{name}"}}"#)
}

#[tokio::test]
async fn applied_refs_become_ordered_events_and_admission_waits_for_the_row() {
    let (mut state, path, c1, c2) = setup().await;
    let alice: i64 = sqlx::query_scalar("SELECT id FROM user WHERE name = 'alice'")
        .fetch_one(&mut state.writer)
        .await
        .unwrap();
    let gate = Gate::default();
    let bus = EventBus::default();
    let updates = [
        update("main", &c1, &c2),
        update("same", &c1, &c1),
        update("gone", &c1, ZERO),
        update("skipped", ZERO, &c2),
        update("new", ZERO, &c1),
    ];
    gate.admit(
        &mut state.writer,
        1,
        Some(alice),
        "request-1",
        &updates,
        None,
    )
    .await
    .unwrap();
    // Partial success: `skipped` is never written, `same` is ignored.
    git(&path, &["update-ref", "refs/heads/main", &c2], "");
    git(&path, &["update-ref", "-d", "refs/heads/gone"], "");
    git(&path, &["update-ref", "refs/heads/new", &c1], "");
    let refused = gate
        .admit(
            &mut state.writer,
            1,
            Some(alice),
            "request-2",
            &updates[..1],
            None,
        )
        .await;
    assert!(matches!(refused, Err(Refused::InProgress)), "{refused:?}");
    // Not reconciled while the process runs.
    gate.reconcile(&mut state.writer, &bus, &state.dir.0, &Gix)
        .await
        .unwrap();
    assert!(events(&mut state).await.is_empty());

    gate.exited(1);
    gate.reconcile(&mut state.writer, &bus, &state.dir.0, &Gix)
        .await
        .unwrap();
    assert_eq!(
        events(&mut state).await,
        [
            format!("user {alice} 1 request-1 {}", payload("main", &c1, &c2)),
            format!("user {alice} 1 request-1 {}", payload("gone", &c1, ZERO)),
            format!("user {alice} 1 request-1 {}", payload("new", ZERO, &c1)),
        ]
    );
    assert!(
        state
            .strings("SELECT correlation_id FROM pending_push")
            .await
            .is_empty()
    );

    // Nothing applied: no event, the row is dropped and admission resumes.
    gate.admit(
        &mut state.writer,
        1,
        None,
        "request-3",
        &[update("main", &c2, &c1)],
        None,
    )
    .await
    .unwrap();
    gate.exited(1);
    gate.reconcile(&mut state.writer, &bus, &state.dir.0, &Gix)
        .await
        .unwrap();
    assert_eq!(events(&mut state).await.len(), 3);
    gate.admit(&mut state.writer, 1, None, "request-4", &[], None)
        .await
        .unwrap();
    state.close().await;
}

#[tokio::test]
async fn crash_after_ref_update_yields_one_event_after_restart() {
    let (mut state, path, c1, c2) = setup().await;
    let bus = EventBus::default();
    Gate::default()
        .admit(
            &mut state.writer,
            1,
            None,
            "import",
            &[update("main", &c1, &c2)],
            None,
        )
        .await
        .unwrap();
    git(&path, &["update-ref", "refs/heads/main", &c2], "");
    // The process and the server die before `exited`: a new instance reopens the
    // database and recovers the row.
    state.reopen().await;
    let gate = Gate::default();
    for _ in 0..2 {
        gate.reconcile(&mut state.writer, &bus, &state.dir.0, &Gix)
            .await
            .unwrap();
        assert_eq!(
            events(&mut state).await,
            [format!("system - 1 import {}", payload("main", &c1, &c2))]
        );
    }
    state.close().await;
}

#[tokio::test]
async fn unexpected_value_retains_the_row_and_blocks_admission() {
    let (mut state, path, c1, c2) = setup().await;
    let bus = EventBus::default();
    let gate = Gate::default();
    gate.admit(
        &mut state.writer,
        1,
        None,
        "request-1",
        &[update("gone", &c1, ZERO), update("main", &c1, &c2)],
        None,
    )
    .await
    .unwrap();
    git(&path, &["update-ref", "-d", "refs/heads/gone"], "");
    // Neither the old nor the new value: evidence of an out-of-band write.
    git(&path, &["update-ref", "-d", "refs/heads/main"], "");
    gate.exited(1);
    for _ in 0..2 {
        gate.reconcile(&mut state.writer, &bus, &state.dir.0, &Gix)
            .await
            .unwrap();
    }
    assert!(events(&mut state).await.is_empty());
    assert_eq!(
        state
            .strings("SELECT correlation_id FROM pending_push")
            .await,
        ["request-1"]
    );
    let refused = gate
        .admit(&mut state.writer, 1, None, "request-2", &[], None)
        .await;
    assert!(matches!(refused, Err(Refused::Retained)), "{refused:?}");
    state.close().await;
}

#[tokio::test]
async fn a_recovery_pass_converts_rows_and_the_next_one_retries_after_an_error() {
    let (mut state, path, c1, c2) = setup().await;
    state
        .apply("[repos.demo]\nowner='alice'\ndescription=''\n[repos.other]\nowner='alice'\ndescription=''\n")
        .await
        .unwrap();
    let other: i64 = sqlx::query_scalar("SELECT id FROM repo WHERE config_key = 'other'")
        .fetch_one(&mut state.writer)
        .await
        .unwrap();
    let dead = Gate::default();
    dead.admit(&mut state.writer, other, None, "empty", &[], None)
        .await
        .unwrap();
    dead.admit(
        &mut state.writer,
        1,
        None,
        "import",
        &[update("main", &c1, &c2)],
        None,
    )
    .await
    .unwrap();
    git(&path, &["update-ref", "refs/heads/main", &c2], "");
    // The instance died without `exited`. The new one's timer recovers the rows in
    // id order: `demo` is unreadable at first, so the first pass converts `empty`,
    // fails on `import` without panicking, and the next pass finishes the job.
    let hidden = path.with_extension("hidden");
    std::fs::rename(&path, &hidden).unwrap();
    let (gate, bus) = (Gate::default(), EventBus::default());
    gate.recover_once(&mut state.writer, &bus, &state.dir.0, &Gix)
        .await;
    assert_eq!(
        state
            .strings("SELECT correlation_id FROM pending_push")
            .await,
        ["import"]
    );
    std::fs::rename(&hidden, &path).unwrap();
    gate.recover_once(&mut state.writer, &bus, &state.dir.0, &Gix)
        .await;
    assert!(
        state
            .strings("SELECT correlation_id FROM pending_push")
            .await
            .is_empty()
    );
    assert_eq!(
        events(&mut state).await,
        [format!("system - 1 import {}", payload("main", &c1, &c2))]
    );
    state.close().await;
}

/// A clock whose every sleep reports its duration and waits for the test's wake-up.
struct Ticks {
    asleep: tokio::sync::mpsc::UnboundedSender<std::time::Duration>,
    wake: tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<()>>,
}

impl crate::db::events::Clock for Ticks {
    fn now(&self) -> std::time::Instant {
        std::time::Instant::now()
    }
    async fn sleep(&self, duration: std::time::Duration) {
        self.asleep.send(duration).unwrap();
        self.wake.lock().await.recv().await.unwrap();
    }
}

#[tokio::test]
async fn the_recovery_timer_waits_its_interval_then_retries_each_tick() {
    let (mut state, path, c1, c2) = setup().await;
    let dead = Gate::default();
    dead.admit(
        &mut state.writer,
        1,
        None,
        "import",
        &[update("main", &c1, &c2)],
        None,
    )
    .await
    .unwrap();
    git(&path, &["update-ref", "refs/heads/main", &c2], "");
    let hidden = path.with_extension("hidden");
    std::fs::rename(&path, &hidden).unwrap();

    let (asleep, mut slept) = tokio::sync::mpsc::unbounded_channel();
    let (wake, woken) = tokio::sync::mpsc::unbounded_channel();
    let clock = Ticks {
        asleep,
        wake: woken.into(),
    };
    let writer = tokio::sync::Mutex::new(std::mem::replace(
        &mut state.writer,
        sqlx::Connection::connect("sqlite::memory:").await.unwrap(),
    ));
    let (gate, bus, dir) = (Gate::default(), EventBus::default(), state.dir.0.clone());
    let pending = async || -> Vec<String> {
        sqlx::query_scalar("SELECT correlation_id FROM pending_push")
            .fetch_all(&mut *writer.lock().await)
            .await
            .unwrap()
    };
    {
        let recovery = gate.recover_periodically(&clock, &writer, &bus, &dir, &Gix);
        tokio::pin!(recovery);
        // Runs the loop until its next sleep, i.e. until the current pass is over.
        let mut tick = async || {
            tokio::select! {
                () = &mut recovery => unreachable!("the loop never ends"),
                duration = slept.recv() => assert_eq!(duration, Some(std::time::Duration::from_secs(60))),
            }
        };
        tick().await;
        assert_eq!(pending().await, ["import"], "no pass before the interval");
        wake.send(()).unwrap();
        tick().await;
        assert_eq!(pending().await, ["import"], "a failed pass keeps the row");
        std::fs::rename(&hidden, &path).unwrap();
        wake.send(()).unwrap();
        tick().await;
        assert!(pending().await.is_empty(), "the next tick retries");
    }
    state.writer = writer.into_inner();
    assert_eq!(
        events(&mut state).await,
        [format!("system - 1 import {}", payload("main", &c1, &c2))]
    );
    state.close().await;
}
