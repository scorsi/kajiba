//! Shared admission for operator maintenance (0016, 0053).
#![allow(clippy::result_large_err)]
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::{Mutex, OwnedRwLockReadGuard, OwnedRwLockWriteGuard, RwLock, watch};
use tonic::Status;

#[derive(Clone)]
pub(crate) struct Admission {
    closed: Arc<AtomicBool>,
    active: Arc<RwLock<()>>,
    completion: Arc<RwLock<()>>,
    changed: Arc<watch::Sender<()>>,
    pub(crate) dispatcher: Arc<Mutex<crate::db::events::Dispatcher>>,
    pauses: Arc<std::sync::Mutex<Vec<(i64, i64)>>>,
}

impl Default for Admission {
    fn default() -> Self {
        Self {
            closed: Arc::default(),
            active: Arc::default(),
            completion: Arc::default(),
            changed: Arc::new(watch::channel(()).0),
            dispatcher: Arc::default(),
            pauses: Arc::default(),
        }
    }
}

impl Admission {
    pub(crate) fn admit(&self) -> Result<OwnedRwLockReadGuard<()>, Status> {
        let permit = self.active.clone().try_read_owned().ok();
        if self.closed.load(Ordering::SeqCst) || permit.is_none() {
            return Err(super::local::refused(
                tonic::Code::Unavailable,
                "MAINTENANCE",
                "maintenance in progress; retry later".into(),
            ));
        }
        Ok(permit.unwrap())
    }

    /// Background and runner writes wait without acknowledging unpersisted messages.
    pub(crate) async fn background(&self) -> OwnedRwLockReadGuard<()> {
        let mut changed = self.changed.subscribe();
        loop {
            if !self.closed.load(Ordering::SeqCst) {
                let permit = self.active.clone().read_owned().await;
                if !self.closed.load(Ordering::SeqCst) {
                    return permit;
                }
            }
            let _ = changed.changed().await;
        }
    }

    /// Git completion and reconnect recovery remain available until sessions drain.
    pub(crate) async fn completion(&self) -> OwnedRwLockReadGuard<()> {
        self.completion.clone().read_owned().await
    }

    /// Called under the writer lock before runner lease checks after maintenance.
    pub(crate) async fn resume_leases(
        &self,
        writer: &mut sqlx::SqliteConnection,
    ) -> Result<(), sqlx::Error> {
        use sqlx::Connection;
        let pauses = self.pauses.lock().unwrap().clone();
        if pauses.is_empty() {
            return Ok(());
        }
        let mut tx = writer.begin().await?;
        for (start, end) in &pauses {
            sqlx::query("UPDATE job_attempt SET lease_expires_at = lease_expires_at + ? WHERE state = 'running' AND lease_expires_at >= ?")
                .bind(end - start).bind(start).execute(&mut *tx).await?;
            sqlx::query("UPDATE runner SET last_contact_at = last_contact_at + ? WHERE last_contact_at >= ?")
                .bind(end - start).bind(start - crate::db::attempts::LEASE_MS).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        self.pauses.lock().unwrap().drain(..pauses.len());
        Ok(())
    }

    pub(crate) async fn dispatch(
        &self,
        bus: &crate::db::events::EventBus,
        writer: &crate::db::Writer,
    ) {
        loop {
            let (progressed, delay) = {
                let _permit = self.background().await;
                self.dispatcher
                    .lock()
                    .await
                    .tick(bus, writer, &crate::db::events::SystemClock)
                    .await
            };
            if progressed {
                continue;
            }
            tokio::select! {
                () = bus.notified() => {},
                () = tokio::time::sleep(delay) => {},
            }
        }
    }

    pub(crate) fn close(&self) -> Result<Closed, Status> {
        self.closed
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .map_err(|_| {
                super::local::refused(
                    tonic::Code::Unavailable,
                    "MAINTENANCE",
                    "maintenance already in progress; retry later".into(),
                )
            })?;
        Ok(Closed {
            admission: self.clone(),
            started: crate::db::runs::now(),
        })
    }
}

pub(crate) struct Closed {
    admission: Admission,
    started: i64,
}

/// Owns closed admission and both write classes until copying ends or is canceled.
pub(crate) struct Quiescent {
    _active: OwnedRwLockWriteGuard<()>,
    _completion: OwnedRwLockWriteGuard<()>,
    _closed: Closed,
}

impl Closed {
    pub(crate) async fn drain(
        self,
        admin: &super::local::Admin,
        git: &mut super::git_side::Repositories,
    ) -> Result<Quiescent, String> {
        let active = self.admission.active.clone().write_owned().await;
        loop {
            let completion = self.admission.completion.clone().write_owned().await;
            // Check even while local sessions remain: a dead daemon cannot report their exits.
            let mut client = git.client().await.map_err(|e| e.to_string())?.clone();
            let mut request = tonic::Request::new(kajiba_proto::git::v1::ListSessionsRequest {});
            request.set_timeout(std::time::Duration::from_secs(5));
            let snapshot = client
                .list_sessions(request)
                .await
                .map_err(|e| e.to_string())?
                .into_inner();
            if admin.gate.generation().as_ref() != Some(&snapshot.generation) {
                return Err("git session snapshot changed during maintenance".into());
            }
            if admin.gate.quiescent() {
                if snapshot
                    .sessions
                    .iter()
                    .any(|s| s.kind != kajiba_proto::git::v1::SessionKind::UploadPack as i32)
                {
                    return Err("git session snapshot changed during maintenance".into());
                }
                let mut writer = admin.writer.lock().await;
                let repos = admin.state_dir.join("repos");
                let store = kajiba_gitstore::Gix::shared(&repos);
                admin
                    .gate
                    .reconcile(&mut writer, &admin.bus, &admin.state_dir, &store)
                    .await?;
                let pending: i64 = sqlx::query_scalar("SELECT count(*) FROM pending_push")
                    .fetch_one(&mut *writer)
                    .await
                    .map_err(|e| e.to_string())?;
                if pending != 0 {
                    return Err("pending push reconciliation incomplete".into());
                }
                self.admission
                    .dispatcher
                    .lock()
                    .await
                    .flush(&admin.bus, &mut writer)
                    .await?;
                return Ok(Quiescent {
                    _active: active,
                    _completion: completion,
                    _closed: self,
                });
            }
            drop(completion);
            // SessionExited and reconnect must be able to acquire both locks here.
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }
}

impl Drop for Closed {
    fn drop(&mut self) {
        self.admission
            .pauses
            .lock()
            .unwrap()
            .push((self.started, crate::db::runs::now()));
        self.admission.closed.store(false, Ordering::SeqCst);
        self.admission.changed.send_replace(());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closing_refuses_new_admission_and_drop_reopens() {
        let admission = Admission::default();
        let admitted = admission.admit().unwrap();
        let closed = admission.close().unwrap();
        assert_eq!(
            admission.admit().unwrap_err().code(),
            tonic::Code::Unavailable
        );
        assert!(admission.close().is_err());
        assert!(admission.active.try_write().is_err());
        drop(admitted);
        assert!(admission.active.try_write().is_ok());
        drop(closed);
        assert!(admission.admit().is_ok());
    }
}

#[cfg(test)]
mod drain_tests {
    use crate::transport::{git_side::fake::Fake, local, tests::State};
    use std::time::Duration;

    #[tokio::test]
    async fn admitted_handlers_hold_drain_without_holding_the_writer() {
        let state = State::new();
        let admin = local::tests::admin(&state.health.state_dir, "").await;
        let mut git = Fake::start(&admin.state_dir).await;
        let permit = admin.maintenance.admit().unwrap();
        let closed = admin.maintenance.close().unwrap();
        let drain = closed.drain(&admin, &mut git.client);
        tokio::pin!(drain);
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut drain)
                .await
                .is_err()
        );
        // Waiting for a handler never holds the database writer.
        assert!(admin.writer.try_lock().is_ok());
        let queued = admin.maintenance.background();
        tokio::pin!(queued);
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut queued)
                .await
                .is_err()
        );
        drop(permit);
    }

    #[tokio::test]
    async fn quiescence_holds_both_write_classes_and_drop_reopens() {
        let state = State::new();
        let admin = local::tests::admin(&state.health.state_dir, "").await;
        let mut git = Fake::start(&admin.state_dir).await;
        admin
            .gate
            .rebuild(
                &mut *admin.writer.lock().await,
                "00000000000000000000000001",
                vec![],
            )
            .await
            .unwrap();
        let guard = admin
            .maintenance
            .close()
            .unwrap()
            .drain(&admin, &mut git.client)
            .await
            .unwrap();
        assert!(admin.maintenance.admit().is_err());
        assert!(admin.maintenance.active.try_write().is_err());
        assert!(admin.maintenance.completion.try_write().is_err());
        let queued = admin.maintenance.background();
        tokio::pin!(queued);
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut queued)
                .await
                .is_err()
        );
        drop(guard);
        let _resumed = tokio::time::timeout(Duration::from_secs(1), queued)
            .await
            .unwrap();
        assert!(admin.maintenance.completion.try_write().is_ok());
    }

    #[tokio::test]
    async fn stale_writing_session_does_not_hide_unavailable_or_changed_daemon() {
        use kajiba_proto::git::v1::{Session, SessionKind};

        for unavailable in [true, false] {
            let state = State::new();
            let admin = local::tests::admin(&state.health.state_dir, "").await;
            let generation = if unavailable {
                "00000000000000000000000001"
            } else {
                "00000000000000000000000002"
            };
            admin
                .gate
                .rebuild(&mut *admin.writer.lock().await, generation, vec![])
                .await
                .unwrap();
            admin.gate.opened(
                1,
                Session {
                    session_id: kajiba_core::new_id().unwrap(),
                    generation: generation.into(),
                    kind: SessionKind::ReceivePack as i32,
                    ..Default::default()
                },
            );
            let mut git = if unavailable {
                crate::transport::git_side::Repositories::new(&admin.git_socket)
            } else {
                Fake::start(&admin.state_dir).await.client
            };
            assert!(!admin.gate.quiescent());
            let result = tokio::time::timeout(
                Duration::from_secs(1),
                admin.maintenance.close().unwrap().drain(&admin, &mut git),
            )
            .await
            .expect("stale sessions must not prevent daemon failure from aborting maintenance");
            assert!(result.is_err());
            assert!(admin.maintenance.admit().is_ok());
            assert!(admin.maintenance.completion.try_write().is_ok());
            let _resumed =
                tokio::time::timeout(Duration::from_secs(1), admin.maintenance.background())
                    .await
                    .unwrap();
        }
    }

    #[tokio::test]
    async fn canceled_drain_and_unavailable_daemon_release_admission() {
        let state = State::new();
        let admin = local::tests::admin(&state.health.state_dir, "").await;
        let mut git = Fake::start(&admin.state_dir).await;
        let permit = admin.maintenance.admit().unwrap();
        assert!(
            tokio::time::timeout(
                Duration::from_millis(20),
                admin
                    .maintenance
                    .close()
                    .unwrap()
                    .drain(&admin, &mut git.client)
            )
            .await
            .is_err()
        );
        drop(permit);
        let _resumed = tokio::time::timeout(Duration::from_secs(1), admin.maintenance.background())
            .await
            .unwrap();
        drop(_resumed);
        let mut missing = crate::transport::git_side::Repositories::new(&admin.git_socket);
        assert!(
            admin
                .maintenance
                .close()
                .unwrap()
                .drain(&admin, &mut missing)
                .await
                .is_err()
        );
        assert!(admin.maintenance.admit().is_ok());
    }
}
