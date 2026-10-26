use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::{SystemTime, UNIX_EPOCH};

use crossbeam_queue::ArrayQueue;

use crate::error::WalError;
use crate::record::LogRecord;
use crate::writer::WalWriter;

/// Synchronous append handle for services that need durability before replying.
///
/// The background group-commit [`WalWriter`] remains available for high-throughput
/// producers. This wrapper uses the same record format and hash chain, but commits
/// each batch before returning, so the caller can acknowledge the write safely.
pub struct Wal {
    writer: WalWriter,
    last_seq: u64,
    last_hash: [u8; 32],
}

impl Wal {
    pub fn open(
        base_dir: impl AsRef<Path>,
        start_seq: u64,
        start_hash: [u8; 32],
    ) -> Result<Self, WalError> {
        let ring = Arc::new(ArrayQueue::new(1));
        let durable_seq = Arc::new(AtomicU64::new(start_seq));
        let writer = WalWriter::new(base_dir, ring, durable_seq, start_seq, start_hash)?;
        Ok(Self {
            writer,
            last_seq: start_seq,
            last_hash: start_hash,
        })
    }

    /// Append one record and fsync it before returning.
    pub fn append(&mut self, kind: u8, payload: Vec<u8>) -> Result<LogRecord, WalError> {
        let mut records = self.append_batch(&[(kind, payload)])?;
        Ok(records.remove(0))
    }

    /// Append a batch as one fsync. Records are chained in order.
    pub fn append_batch(&mut self, entries: &[(u8, Vec<u8>)]) -> Result<Vec<LogRecord>, WalError> {
        if entries.is_empty() {
            return Ok(Vec::new());
        }

        let mut records = Vec::with_capacity(entries.len());
        let mut seq = self.last_seq;
        let mut prev_hash = self.last_hash;

        for (kind, payload) in entries {
            seq = seq.saturating_add(1);
            let record = LogRecord::new(seq, now_micros(), prev_hash, *kind, payload.clone())?;
            prev_hash = record.hash;
            records.push(record);
        }

        self.writer.commit_batch(&records)?;
        self.last_seq = seq;
        self.last_hash = prev_hash;
        Ok(records)
    }

    pub fn last_seq(&self) -> u64 {
        self.last_seq
    }

    pub fn last_hash(&self) -> [u8; 32] {
        self.last_hash
    }
}

fn now_micros() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_micros()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}
