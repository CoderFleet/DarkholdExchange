use std::fs::{self, OpenOptions};
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use crossbeam_queue::ArrayQueue;
use dkx_wal::{GENESIS_PREV_HASH, LogRecord, Wal, WalWriter, recover};
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
fn test_torn_first_record_does_not_hide_later_records() {
    let dir = tempdir().expect("tempdir");
    let log_dir = dir.path().join("data/log");
    fs::create_dir_all(&log_dir).expect("log dir");
    let log_file = log_dir.join("00000000000000000001.log");
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_file)
        .expect("open");
    file.write_all(&[0, 0, 0, 100, 0, 0, 0, 6, 0, 0])
        .expect("write torn");
    file.sync_all().expect("sync");
    drop(file);

    let recovered = recover(dir.path()).expect("recover torn file");
    assert_eq!(recovered.last_seq, 0);
    assert_eq!(recovered.replayed_records.len(), 0);

    let mut wal = Wal::open(dir.path(), recovered.last_seq, recovered.last_hash).expect("wal");
    wal.append(1, b"after-torn".to_vec()).expect("append");
    drop(wal);

    let recovered = recover(dir.path()).expect("recover new segment");
    assert_eq!(recovered.last_seq, 1);
    assert_eq!(recovered.replayed_records.len(), 1);
    assert_eq!(recovered.replayed_records[0].payload, b"after-torn");
}

#[test]
fn test_sync_wal_appends_a_batch_and_recovers() {
    let dir = tempdir().expect("tempdir");
    let mut wal = Wal::open(dir.path(), 0, GENESIS_PREV_HASH).expect("wal");

    let records = wal
        .append_batch(&[(1, b"first".to_vec()), (2, b"second".to_vec())])
        .expect("append");
    assert_eq!(records.len(), 2);
    assert_eq!(wal.last_seq(), 2);
    assert_eq!(wal.last_hash(), records[1].hash);
    drop(wal);

    let recovered = recover(dir.path()).expect("recover");
    assert_eq!(recovered.last_seq, 2);
    assert_eq!(recovered.last_hash, records[1].hash);
    assert_eq!(recovered.replayed_records[0].payload, b"first");
    assert_eq!(recovered.replayed_records[1].payload, b"second");
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
