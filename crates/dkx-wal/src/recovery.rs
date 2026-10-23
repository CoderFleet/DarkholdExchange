use std::fs::{self, File};
use std::io::BufReader;
use std::path::{Path, PathBuf};

use dkx_hash::hash_log_entry;
use crate::error::WalError;
use crate::record::LogRecord;
use crate::snapshot::SnapshotData;

#[derive(Debug)]
pub struct RecoveredState {
    pub last_seq: u64,
    pub last_hash: [u8; 32],
    pub snapshot: Option<SnapshotData>,
    pub replayed_records: Vec<LogRecord>,
}

pub fn recover(dir: impl AsRef<Path>) -> Result<RecoveredState, WalError> {
    let dir = dir.as_ref();
    let snap_dir = dir.join("data/snap");
    let log_dir = dir.join("data/log");

    fs::create_dir_all(&snap_dir)?;
    fs::create_dir_all(&log_dir)?;

    // 1. Discover and load the newest valid snapshot
    let mut snapshots = Vec::new();
    if let Ok(entries) = fs::read_dir(&snap_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("snap") {
                snapshots.push(path);
            }
        }
    }
    snapshots.sort();

    let mut latest_valid_snap: Option<SnapshotData> = None;
    while let Some(snap_path) = snapshots.pop() {
        if let Ok(bytes) = fs::read(&snap_path) {
            if let Ok(snap) = rmp_serde::from_slice::<SnapshotData>(&bytes) {
                if snap.verify().is_ok() {
                    latest_valid_snap = Some(snap);
                    break;
                }
            }
        }
    }

    let (mut start_seq, mut current_hash) = match &latest_valid_snap {
        Some(s) => (s.last_seq, s.last_hash),
        None => (0, dkx_hash::GENESIS_PREV_HASH),
    };

    // 2. Scan and read all log segment files in sequence
    let mut log_files = Vec::new();
    if let Ok(entries) = fs::read_dir(&log_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("log") {
                log_files.push(path);
            }
        }
    }
    log_files.sort();

    let mut replayed_records = Vec::new();

    for log_path in log_files {
        let file = File::open(&log_path)?;
        let mut reader = BufReader::new(file);

        while let Some(record) = LogRecord::decode_length_prefixed(&mut reader)? {
            // Verify chain continuity
            if record.seq <= start_seq {
                // Record is already included in the snapshot checkpoint
                continue;
            }

            if record.prev_hash != current_hash {
                return Err(WalError::ChainBroken {
                    seq: record.seq,
                    expected: hex_fmt(&current_hash),
                    actual: hex_fmt(&record.prev_hash),
                });
            }

            // Verify record hash integrity
            let computed = hash_log_entry(
                record.seq,
                record.ts_micros,
                &record.prev_hash,
                record.kind,
                &record.payload,
            )?;

            if computed != record.hash {
                return Err(WalError::RecordHashMismatch { seq: record.seq });
            }

            current_hash = record.hash;
            start_seq = record.seq;
            replayed_records.push(record);
        }
    }

    Ok(RecoveredState {
        last_seq: start_seq,
        last_hash: current_hash,
        snapshot: latest_valid_snap,
        replayed_records,
    })
}

fn hex_fmt(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(s, "{:02x}", b);
    }
    s
}
