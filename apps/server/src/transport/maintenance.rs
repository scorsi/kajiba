//! Shared admission for operator maintenance (0016, 0053).
#![allow(clippy::result_large_err)]
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::{OwnedRwLockReadGuard, RwLock};
use tonic::Status;

#[derive(Clone, Default)]
pub(crate) struct Admission {
    closed: Arc<AtomicBool>,
    active: Arc<RwLock<()>>,
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

    // The dump command will own this guard throughout drain and copying.
    #[allow(dead_code)]
    pub(crate) fn close(&self) -> Result<Closed, Status> {
        self.closed
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .map_err(|_| Status::unavailable("maintenance already in progress"))?;
        Ok(Closed(self.clone()))
    }
}

pub(crate) struct Closed(Admission);

impl Drop for Closed {
    fn drop(&mut self) {
        self.0.closed.store(false, Ordering::SeqCst);
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
