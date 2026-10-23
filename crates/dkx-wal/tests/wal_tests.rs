use std::fs::{self, OpenOptions};
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use crossbeam_queue::ArrayQueue;
use dkx_wal::{GENESIS_PREV_HASH, LogRecord, WalWriter, recover};
use tempfile::tempdir;

#[test]
fn test_wal_write_and_recovery_continuous_chain() {
    let dir = tempdir().expect("tempdir");
    let queue = Arc::new(ArrayQueue::new(1024));
    let durable = Arc::new(AtomicU64::new(0));

    let mut writer = WalWriter::new(
        dir.path(),
        queue.clone(),
        durable.clone(),
        0,
        GENESIS_PREV_HASH,
    )
    .expect("writer");

    let mut prev = GENESIS_PREV_HASH;
    let mut batch = Vec::new();

    for seq in 1..=10 {
        let payload = rmp_serde::to_vec_named(&format!("command-{}", seq)).expect("payload");
        let rec = LogRecord::new(seq, 1000 + seq, prev, 1, payload).expect("record");
        prev = rec.hash;
        batch.push(rec);
    }

    writer.commit_batch(&batch).expect("commit");
    assert_eq!(durable.load(std::sync::atomic::Ordering::Acquire), 10);

    let recovered = recover(dir.path()).expect("recover");
    assert_eq!(recovered.last_seq, 10);
    assert_eq!(recovered.last_hash, prev);
    assert_eq!(recovered.replayed_records.len(), 10);
}

#[test]
fn test_editing_record_breaks_chain_at_that_record() {
    let dir = tempdir().expect("tempdir");
    let queue = Arc::new(ArrayQueue::new(1024));
    let durable = Arc::new(AtomicU64::new(0));

    let mut writer =
        WalWriter::new(dir.path(), queue, durable, 0, GENESIS_PREV_HASH).expect("writer");

    let mut prev = GENESIS_PREV_HASH;
    let mut batch = Vec::new();

    for seq in 1..=5 {
        let payload = rmp_serde::to_vec_named(&format!("command-{}", seq)).expect("payload");
        let rec = LogRecord::new(seq, 1000 + seq, prev, 1, payload).expect("record");
        prev = rec.hash;
        batch.push(rec);
    }
    writer.commit_batch(&batch).expect("commit");

    // Tamper with the log file
    let log_file = dir.path().join("data/log/00000000000000000001.log");
    let mut data = fs::read(&log_file).expect("read");
    // Flip a byte in the payload of an early record
    data[50] ^= 0xFF;
    fs::write(&log_file, data).expect("write");

    let res = recover(dir.path());
    assert!(res.is_err());
}

#[test]
fn test_torn_final_record_is_dropped() {
    let dir = tempdir().expect("tempdir");
    let queue = Arc::new(ArrayQueue::new(1024));
    let durable = Arc::new(AtomicU64::new(0));

    let mut writer =
        WalWriter::new(dir.path(), queue, durable, 0, GENESIS_PREV_HASH).expect("writer");

    let mut prev = GENESIS_PREV_HASH;
    let mut batch = Vec::new();

    for seq in 1..=5 {
        let payload = rmp_serde::to_vec_named(&format!("command-{}", seq)).expect("payload");
        let rec = LogRecord::new(seq, 1000 + seq, prev, 1, payload).expect("record");
        prev = rec.hash;
        batch.push(rec);
    }
    writer.commit_batch(&batch).expect("commit");

    // Append a partially written torn record
    let log_file = dir.path().join("data/log/00000000000000000001.log");
    let mut file = OpenOptions::new()
        .append(true)
        .open(&log_file)
        .expect("open");
    // Write just 10 bytes of a new record header (cut off mid-record)
    file.write_all(&[0, 0, 0, 100, 0, 0, 0, 6, 0, 0])
        .expect("write torn");
    file.sync_all().expect("sync");

    let recovered = recover(dir.path()).expect("recover");
    assert_eq!(recovered.last_seq, 5);
    assert_eq!(recovered.replayed_records.len(), 5);
}

#[test]
fn test_snapshot_midway_recovers_to_same_final_state() {
    let dir = tempdir().expect("tempdir");
    let queue = Arc::new(ArrayQueue::new(1024));
    let durable = Arc::new(AtomicU64::new(0));

    let mut writer =
        WalWriter::new(dir.path(), queue, durable, 0, GENESIS_PREV_HASH).expect("writer");

    let mut prev = GENESIS_PREV_HASH;
    let mut batch1 = Vec::new();

    for seq in 1..=5 {
        let payload = rmp_serde::to_vec_named(&format!("command-{}", seq)).expect("payload");
        let rec = LogRecord::new(seq, 1000 + seq, prev, 1, payload).expect("record");
        prev = rec.hash;
        batch1.push(rec);
    }
    writer.commit_batch(&batch1).expect("commit");

    // Take snapshot at seq 5
    writer
        .take_snapshot(&|_seq, _hash| (vec![1, 2, 3], vec![], vec![4, 5, 6]))
        .expect("snapshot");

    let mut batch2 = Vec::new();
    for seq in 6..=10 {
        let payload = rmp_serde::to_vec_named(&format!("command-{}", seq)).expect("payload");
        let rec = LogRecord::new(seq, 1000 + seq, prev, 1, payload).expect("record");
        prev = rec.hash;
        batch2.push(rec);
    }
    writer.commit_batch(&batch2).expect("commit");

    let recovered = recover(dir.path()).expect("recover");
    assert_eq!(recovered.last_seq, 10);
    assert_eq!(recovered.last_hash, prev);
    assert!(recovered.snapshot.is_some());
    // Only records 6..=10 need replay because 1..=5 were snapshotted
    assert_eq!(recovered.replayed_records.len(), 5);
    assert_eq!(recovered.replayed_records[0].seq, 6);
    assert_eq!(recovered.replayed_records[4].seq, 10);
}
