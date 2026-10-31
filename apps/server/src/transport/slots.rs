//! Counted slots per key under a total (ADRs 0119, 0120, 0122): bundle transfers per
//! runner, run watchers per principal, worktree uploads per user.
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

pub struct Slots<K> {
    total: usize,
    held: Mutex<BTreeMap<K, usize>>,
}

/// One slot of `key`, given back when dropped.
pub struct Slot<K: Ord> {
    slots: Arc<Slots<K>>,
    key: K,
}

impl<K: Ord + Clone> Slots<K> {
    pub fn new(total: usize) -> Arc<Self> {
        Arc::new(Self {
            total,
            held: Mutex::default(),
        })
    }

    /// A slot for `key` while it holds fewer than `limit` and all keys fewer than the total.
    pub fn take(self: &Arc<Self>, key: K, limit: usize) -> Option<Slot<K>> {
        let mut held = self.held.lock().unwrap_or_else(|e| e.into_inner());
        let all: usize = held.values().sum();
        let mine = held.get(&key).copied().unwrap_or(0);
        if mine >= limit || all >= self.total {
            return None;
        }
        held.insert(key.clone(), mine + 1);
        Some(Slot {
            slots: self.clone(),
            key,
        })
    }

    /// Slots `key` holds now.
    pub fn held(&self, key: &K) -> usize {
        let held = self.held.lock().unwrap_or_else(|e| e.into_inner());
        held.get(key).copied().unwrap_or(0)
    }
}

impl<K: Ord> Drop for Slot<K> {
    fn drop(&mut self) {
        let mut held = self.slots.held.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(count) = held.get_mut(&self.key) {
            *count -= 1;
            if *count == 0 {
                held.remove(&self.key);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_are_bounded_per_key_and_in_total_until_dropped() {
        let slots = Slots::new(3);
        let a1 = slots.take("a", 2).unwrap();
        let _a2 = slots.take("a", 2).unwrap();
        assert!(slots.take("a", 2).is_none());
        let _b = slots.take("b", 2).unwrap();
        assert!(slots.take("c", 2).is_none(), "total reached");
        assert_eq!(slots.held(&"a"), 2);
        drop(a1);
        assert_eq!(slots.held(&"a"), 1);
        let _c = slots.take("c", 2).unwrap();
        assert!(slots.take("a", 0).is_none());
    }
}
