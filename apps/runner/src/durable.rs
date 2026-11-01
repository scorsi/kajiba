//! Restart-safe attempt records and log chunk boundaries (0156).

use kajiba_core::names::is_public_id;
use kajiba_proto::runner::v1::Assignment;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

pub type Shared = Arc<Mutex<State>>;

#[derive(Clone, Serialize, Deserialize)]
pub struct State {
    pub attempt: String,
    pub unit: String,
    pub log: PathBuf,
    pub source: PathBuf,
    pub sha: String,
    pub system: String,
    pub checks: Vec<String>,
    pub step: String,
    pub acked_sequence: u64,
    pub acked_offset: u64,
    /// End offsets of chunks durably indexed before transmission, at most one window.
    pub pending: Vec<u64>,
    #[serde(default)]
    pub pending_times: Vec<i64>,
}

pub fn atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let temporary = path.with_extension("tmp");
    let mut file = fs::File::create(&temporary).map_err(|e| e.to_string())?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .and_then(|_| fs::rename(&temporary, path))
        .and_then(|_| fs::File::open(path.parent().unwrap())?.sync_all())
        .map_err(|e| format!("attempt state: {e}"))
}

impl State {
    pub fn new(root: &Path, assignment: &Assignment, source: PathBuf) -> Result<Self, String> {
        if !is_public_id(&assignment.attempt_id) {
            return Err("invalid attempt id".into());
        }
        let dir = root.join("jobs").join(&assignment.attempt_id);
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let state = Self {
            attempt: assignment.attempt_id.clone(),
            unit: format!("kajiba-job-{}.service", assignment.attempt_id),
            log: dir.join("output.log"),
            source,
            sha: assignment.sha.clone(),
            system: assignment.system.clone(),
            checks: assignment.checks.clone(),
            step: if assignment.checks.is_empty() {
                "flake-check"
            } else {
                "build-checks"
            }
            .into(),
            acked_sequence: 0,
            acked_offset: 0,
            pending: Vec::new(),
            pending_times: Vec::new(),
        };
        fs::File::create(&state.log).map_err(|e| e.to_string())?;
        fs::File::create(state.dir().join("output.times")).map_err(|e| e.to_string())?;
        state.save()?;
        Ok(state)
    }

    pub fn dir(&self) -> &Path {
        self.log.parent().unwrap()
    }
    pub fn save(&self) -> Result<(), String> {
        atomic(
            &self.dir().join("state.json"),
            &serde_json::to_vec(self).map_err(|e| e.to_string())?,
        )
    }
    pub fn offset(&self) -> u64 {
        self.pending.last().copied().unwrap_or(self.acked_offset)
    }
    pub fn sequence(&self) -> u64 {
        self.acked_sequence + self.pending.len() as u64
    }
    pub fn index(&mut self, size: usize, produced_at_ms: i64) -> Result<(), String> {
        self.pending.push(self.offset() + size as u64);
        self.pending_times.push(produced_at_ms);
        self.save()
    }
    pub fn acknowledge(&mut self, sequence: u64) -> Result<(), String> {
        if sequence <= self.acked_sequence {
            return Ok(());
        }
        let count = (sequence - self.acked_sequence) as usize;
        if count > self.pending.len() {
            return Err("log acknowledgment beyond persisted chunks".into());
        }
        self.acked_offset = self.pending[count - 1];
        self.pending.drain(..count);
        self.pending_times.drain(..count);
        self.acked_sequence = sequence;
        self.save()
    }
    pub fn chunks(&self) -> Result<Vec<Vec<u8>>, String> {
        let mut file = fs::File::open(&self.log).map_err(|e| e.to_string())?;
        file.seek(SeekFrom::Start(self.acked_offset))
            .map_err(|e| e.to_string())?;
        let mut start = self.acked_offset;
        self.pending
            .iter()
            .map(|&end| {
                if end <= start || end - start > kajiba_exec::CHUNK as u64 {
                    return Err("invalid persisted chunk boundary".into());
                }
                let mut data = vec![0; (end - start) as usize];
                file.read_exact(&mut data).map_err(|e| e.to_string())?;
                start = end;
                Ok(data)
            })
            .collect()
    }
    pub fn load(root: &Path, id: &str) -> Result<Self, String> {
        if !is_public_id(id) {
            return Err("invalid attempt id".into());
        }
        let dir = root.join("jobs").join(id);
        let mut state: Self =
            serde_json::from_slice(&fs::read(dir.join("state.json")).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        if state.pending_times.is_empty() {
            state.pending_times.resize(state.pending.len(), 0);
        }
        if state.pending_times.len() != state.pending.len()
            || state.attempt != id
            || state.unit != format!("kajiba-job-{id}.service")
            || state.log != dir.join("output.log")
            || state.source != root.join("scratch").join(id)
            || state.pending.len() > 16
        {
            return Err("invalid persisted attempt paths or window".into());
        }
        if state
            .acked_sequence
            .checked_add(state.pending.len() as u64)
            .is_none()
            || fs::metadata(&state.log).map_err(|e| e.to_string())?.len() < state.offset()
        {
            return Err("invalid persisted log offset".into());
        }
        state.chunks()?;
        Ok(state)
    }
    pub fn remove(&self) {
        // Hide the record atomically before recursive cleanup, which a restart can interrupt.
        let removed = self.dir().with_extension("done");
        if fs::rename(self.dir(), &removed).is_ok() {
            let _ = fs::remove_dir_all(&self.source);
            let _ = fs::remove_dir_all(removed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn adoption_resumes_at_durable_boundaries_including_an_ack_lost_before_restart() {
        let root = std::env::temp_dir().join(format!("kajiba-durable-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let id = "01K00000000000000000000000";
        let assignment = Assignment {
            attempt_id: id.into(),
            ..Default::default()
        };
        let mut state = State::new(&root, &assignment, root.join("scratch").join(id)).unwrap();
        fs::write(&state.log, b"first\nsecond\nthird\n").unwrap();
        state.index(6, 1234).unwrap();
        state.index(7, 1234).unwrap();
        state.acknowledge(1).unwrap();
        state.index(6, 1234).unwrap();
        let mut adopted = State::load(&root, id).unwrap();
        assert_eq!(
            adopted.chunks().unwrap(),
            [b"second\n".to_vec(), b"third\n".to_vec()]
        );
        assert_eq!(
            (adopted.acked_offset, adopted.offset(), adopted.sequence()),
            (6, 19, 3)
        );
        // The server accepted chunk 2 just before the runner died, before its ack was saved.
        adopted.acknowledge(2).unwrap();
        assert_eq!(
            State::load(&root, id).unwrap().chunks().unwrap(),
            [b"third\n".to_vec()]
        );
        assert!(adopted.acknowledge(4).is_err());
        // Pre-timestamp records deserialize without changing their byte boundaries.
        let mut legacy = serde_json::to_value(&adopted).unwrap();
        legacy.as_object_mut().unwrap().remove("pending_times");
        atomic(
            &adopted.dir().join("state.json"),
            &serde_json::to_vec(&legacy).unwrap(),
        )
        .unwrap();
        assert_eq!(State::load(&root, id).unwrap().pending_times, [0]);

        adopted.source = root.join("outside");
        adopted.save().unwrap();
        assert!(State::load(&root, id).is_err());
        assert!(State::load(&root, "../escape").is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
