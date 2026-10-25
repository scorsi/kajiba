use crate::{Error, Limit, Result};
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

// Per-operation work bounds, independent of result counts (ADR 0059).
const MAX_OBJECTS: usize = 25_000;
const MAX_ENTRIES: usize = 100_000;
const MAX_BYTES: u64 = 64 << 20;
pub(super) const MAX_FRONTIER: usize = 1024;
const MAX_DEPTH: usize = 128;
const MAX_PATH_BYTES: usize = 4096;
const MAX_TIME: Duration = Duration::from_secs(10);

pub(super) struct Traversal<'a> {
    cancelled: Option<&'a AtomicBool>,
    deadline: Instant,
    objects: usize,
    entries: usize,
    bytes: u64,
}

impl<'a> Traversal<'a> {
    pub(super) fn new(cancelled: Option<&'a AtomicBool>) -> Self {
        Self {
            cancelled,
            deadline: Instant::now() + MAX_TIME,
            objects: 0,
            entries: 0,
            bytes: 0,
        }
    }

    pub(super) fn check(&self) -> Result<()> {
        if self
            .cancelled
            .is_some_and(|flag| flag.load(Ordering::Relaxed))
            || Instant::now() >= self.deadline
        {
            return Err(Error::TooLarge(Limit::Traversal));
        }
        Ok(())
    }

    pub(super) fn object(&mut self, bytes: u64) -> Result<()> {
        self.check()?;
        if self.objects == MAX_OBJECTS || bytes > MAX_BYTES - self.bytes {
            return Err(Error::TooLarge(Limit::Traversal));
        }
        self.objects += 1;
        self.bytes += bytes;
        Ok(())
    }

    pub(super) fn entry(&mut self) -> Result<()> {
        self.check()?;
        if self.entries == MAX_ENTRIES {
            return Err(Error::TooLarge(Limit::Traversal));
        }
        self.entries += 1;
        Ok(())
    }

    pub(super) fn path(&self, path: &[u8]) -> Result<()> {
        self.check()?;
        if path.len() > MAX_PATH_BYTES
            || (!path.is_empty() && path.iter().filter(|&&b| b == b'/').count() >= MAX_DEPTH)
        {
            return Err(Error::TooLarge(Limit::Traversal));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
