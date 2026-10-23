use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crossbeam_queue::ArrayQueue;

use crate::error::WalError;
use crate::record::LogRecord;
use crate::snapshot::SnapshotData;

pub const GROUP_COMMIT_MAX_RECORDS: usize = 256;
pub const GROUP_COMMIT_MAX_MILLIS: u64 = 2;
pub const MAX_SEGMENT_SIZE_BYTES: u64 = 64 * 1024 * 1024; // 64 MB
pub const SNAPSHOT_EVERY_RECORDS: u64 = 10_000;
pub const SNAPSHOT_EVERY_SECS: u64 = 60;

pub struct WalWriter {
    base_dir: PathBuf,
    durable_seq: Arc<AtomicU64>,
    ring: Arc<ArrayQueue<LogRecord>>,
    last_seq: u64,
    last_hash: [u8; 32],
    current_segment: Option<File>,
    current_segment_index: u64,
    current_segment_size: u64,
    records_since_snapshot: u64,
    last_snapshot_time: Instant,
}

impl WalWriter {
    pub fn new(
        base_dir: impl AsRef<Path>,
        ring: Arc<ArrayQueue<LogRecord>>,
        durable_seq: Arc<AtomicU64>,
        start_seq: u64,
        start_hash: [u8; 32],
    ) -> Result<Self, WalError> {
        let base_dir = base_dir.as_ref().to_path_buf();
        fs::create_dir_all(base_dir.join("data/log"))?;
        fs::create_dir_all(base_dir.join("data/snap"))?;

        Ok(Self {
            base_dir,
            durable_seq,
            ring,
            last_seq: start_seq,
            last_hash: start_hash,
            current_segment: None,
            current_segment_index: 0,
            current_segment_size: 0,
            records_since_snapshot: 0,
            last_snapshot_time: Instant::now(),
        })
    }

    pub fn run_loop_step(
        &mut self,
        snapshot_generator: Option<&dyn Fn(u64, [u8; 32]) -> (Vec<u8>, Vec<u8>, Vec<u8>)>,
    ) -> Result<bool, WalError> {
        let start = Instant::now();
        let mut batch = Vec::with_capacity(GROUP_COMMIT_MAX_RECORDS);

        while batch.len() < GROUP_COMMIT_MAX_RECORDS
            && start.elapsed() < Duration::from_millis(GROUP_COMMIT_MAX_MILLIS)
        {
            if let Some(record) = self.ring.pop() {
                batch.push(record);
            } else if !batch.is_empty() {
                // Have some records, can proceed or wait small timeout
                break;
            } else {
                std::thread::yield_now();
                if start.elapsed() >= Duration::from_millis(GROUP_COMMIT_MAX_MILLIS) {
                    break;
                }
            }
        }

        if batch.is_empty() {
            return Ok(false);
        }

        self.commit_batch(&batch)?;

        if let Some(generator) = snapshot_generator {
            if self.records_since_snapshot >= SNAPSHOT_EVERY_RECORDS
                || self.last_snapshot_time.elapsed() >= Duration::from_secs(SNAPSHOT_EVERY_SECS)
            {
                self.take_snapshot(generator)?;
            }
        }

        Ok(true)
    }

    pub fn commit_batch(&mut self, batch: &[LogRecord]) -> Result<(), WalError> {
        let mut encoded_batch = Vec::new();
        let mut max_seq = self.last_seq;

        for record in batch {
            let encoded = record.encode_length_prefixed();
            encoded_batch.extend_from_slice(&encoded);
            max_seq = record.seq;
            self.last_hash = record.hash;
        }

        if self.current_segment.is_none()
            || (self.current_segment_size + encoded_batch.len() as u64) > MAX_SEGMENT_SIZE_BYTES
        {
            self.roll_segment()?;
        }

        if let Some(ref mut file) = self.current_segment {
            file.write_all(&encoded_batch)?;
            file.sync_data()?;
            self.current_segment_size += encoded_batch.len() as u64;
        }

        self.last_seq = max_seq;
        self.durable_seq.store(max_seq, Ordering::Release);
        self.records_since_snapshot += batch.len() as u64;

        Ok(())
    }

    fn roll_segment(&mut self) -> Result<(), WalError> {
        if let Some(ref mut file) = self.current_segment {
            file.sync_all()?;
        }

        self.current_segment_index += 1;
        let filename = format!("{:020}.log", self.last_seq + 1);
        let path = self.base_dir.join("data/log").join(filename);

        let file = OpenOptions::new().create(true).append(true).open(&path)?;

        self.current_segment = Some(file);
        self.current_segment_size = 0;
        Ok(())
    }

    pub fn take_snapshot(
        &mut self,
        generator: &dyn Fn(u64, [u8; 32]) -> (Vec<u8>, Vec<u8>, Vec<u8>),
    ) -> Result<(), WalError> {
        let (ledger_bytes, escrow_bytes, engine_bytes) = generator(self.last_seq, self.last_hash);
        let snap = SnapshotData::new(
            self.last_seq,
            self.last_hash,
            ledger_bytes,
            escrow_bytes,
            engine_bytes,
        );

        let filename = format!("{:020}.snap", self.last_seq);
        let path = self.base_dir.join("data/snap").join(filename);
        let bytes = rmp_serde::to_vec_named(&snap)?;

        let mut file = File::create(&path)?;
        file.write_all(&bytes)?;
        file.sync_all()?;

        self.records_since_snapshot = 0;
        self.last_snapshot_time = Instant::now();
        Ok(())
    }
}
