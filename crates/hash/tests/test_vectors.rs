use dkx_hash::{
    AuditRecord, GENESIS_PREV_HASH, MerklePathStep, PathDirection, hash_audit_entry,
    hash_log_entry, hash_merkle_leaf, hash_merkle_leaf_with_asset, hash_merkle_node,
    pair_with_empty, verify_audit_chain, verify_merkle_path,
};
use serde::Deserialize;

#[allow(dead_code)]
#[derive(Deserialize)]
struct TestVectors {
    genesis_prev: String,
    log_entry: LogEntryVector,
    audit_entry: AuditEntryVector,
    merkle_leaf: MerkleLeafVector,
    merkle_leaf_asset: MerkleLeafAssetVector,
    merkle_node: MerkleNodeVector,
}

#[derive(Deserialize)]
struct LogEntryVector {
    seq: u64,
    ts_micros: u64,
    prev: String,
    kind: u8,
    payload_hex: String,
    expected_hash: String,
}

#[derive(Deserialize)]
struct AuditEntryVector {
    seq: u64,
    ts_micros: u64,
    prev: String,
    kind: u8,
    market: u32,
    qty: u32,
    price: i64,
    expected_hash: String,
}

#[derive(Deserialize)]
struct MerkleLeafVector {
    salt_hex: String,
    user_id: u64,
    balance: u64,
    expected_hash: String,
}

#[derive(Deserialize)]
struct MerkleLeafAssetVector {
    salt_hex: String,
    user_id: u64,
    asset_kind: u8,
    market_id: u32,
    balance: u64,
    expected_hash: String,
}

#[derive(Deserialize)]
struct MerkleNodeVector {
    left_hash: String,
    left_sum: u64,
    right_hash: String,
    right_sum: u64,
    expected_sum: u64,
    expected_hash: String,
}

fn decode_32_bytes(hex_str: &str) -> [u8; 32] {
    let bytes = hex::decode(hex_str).expect("invalid hex");
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    arr
}

fn decode_16_bytes(hex_str: &str) -> [u8; 16] {
    let bytes = hex::decode(hex_str).expect("invalid hex");
    let mut arr = [0u8; 16];
    arr.copy_from_slice(&bytes);
    arr
}

#[test]
fn test_genesis_prev_is_32_zeros() {
    assert_eq!(GENESIS_PREV_HASH, [0u8; 32]);
}

#[test]
fn test_hash_log_entry_matches_golden() {
    let json_str = include_str!("../../../contracts/hash_test_vectors.json");
    let vectors: TestVectors = serde_json::from_str(json_str).expect("parse json");

    let prev = decode_32_bytes(&vectors.log_entry.prev);
    let payload = hex::decode(&vectors.log_entry.payload_hex).expect("payload hex");

    let hash = hash_log_entry(
        vectors.log_entry.seq,
        vectors.log_entry.ts_micros,
        &prev,
        vectors.log_entry.kind,
        &payload,
    )
    .expect("hash log entry");

    assert_eq!(hex::encode(hash), vectors.log_entry.expected_hash);
}

#[test]
fn test_hash_audit_entry_matches_golden() {
    let json_str = include_str!("../../../contracts/hash_test_vectors.json");
    let vectors: TestVectors = serde_json::from_str(json_str).expect("parse json");

    let prev = decode_32_bytes(&vectors.audit_entry.prev);

    let hash = hash_audit_entry(
        vectors.audit_entry.seq,
        vectors.audit_entry.ts_micros,
        &prev,
        vectors.audit_entry.kind,
        vectors.audit_entry.market,
        vectors.audit_entry.qty,
        vectors.audit_entry.price,
    );

    assert_eq!(hex::encode(hash), vectors.audit_entry.expected_hash);
}

#[test]
fn test_hash_merkle_leaf_matches_golden() {
    let json_str = include_str!("../../../contracts/hash_test_vectors.json");
    let vectors: TestVectors = serde_json::from_str(json_str).expect("parse json");

    let salt = decode_16_bytes(&vectors.merkle_leaf.salt_hex);

    let hash = hash_merkle_leaf(
        &salt,
        vectors.merkle_leaf.user_id,
        vectors.merkle_leaf.balance,
    );

    assert_eq!(hex::encode(hash), vectors.merkle_leaf.expected_hash);
}

#[test]
fn test_hash_merkle_leaf_with_asset_matches_golden() {
    let json_str = include_str!("../../../contracts/hash_test_vectors.json");
    let vectors: TestVectors = serde_json::from_str(json_str).expect("parse json");

    let salt = decode_16_bytes(&vectors.merkle_leaf_asset.salt_hex);

    let hash = hash_merkle_leaf_with_asset(
        &salt,
        vectors.merkle_leaf_asset.user_id,
        vectors.merkle_leaf_asset.asset_kind,
        vectors.merkle_leaf_asset.market_id,
        vectors.merkle_leaf_asset.balance,
    );

    assert_eq!(hex::encode(hash), vectors.merkle_leaf_asset.expected_hash);
}

#[test]
fn test_hash_merkle_node_matches_golden() {
    let json_str = include_str!("../../../contracts/hash_test_vectors.json");
    let vectors: TestVectors = serde_json::from_str(json_str).expect("parse json");

    let left = decode_32_bytes(&vectors.merkle_node.left_hash);
    let right = decode_32_bytes(&vectors.merkle_node.right_hash);

    let node = hash_merkle_node(
        &left,
        vectors.merkle_node.left_sum,
        &right,
        vectors.merkle_node.right_sum,
    )
    .expect("merkle node");

    assert_eq!(node.sum, vectors.merkle_node.expected_sum);
    assert_eq!(hex::encode(node.hash), vectors.merkle_node.expected_hash);
}

#[test]
fn test_odd_count_pair_with_empty() {
    let node = dkx_hash::MerkleNode {
        hash: [1u8; 32],
        sum: 500,
    };
    let paired = pair_with_empty(&node).expect("pair with empty");
    assert_eq!(paired.sum, 500);

    let manual = hash_merkle_node(&[1u8; 32], 500, &[0u8; 32], 0).expect("manual");
    assert_eq!(paired.hash, manual.hash);
}

#[test]
fn test_verify_merkle_path() {
    let leaf_salt = [7u8; 16];
    let user_id = 99;
    let balance = 12500;
    let leaf_hash = hash_merkle_leaf(&leaf_salt, user_id, balance);

    let sibling_salt = [8u8; 16];
    let sibling_hash = hash_merkle_leaf(&sibling_salt, 100, 7500);
    let sibling_sum = 7500;

    // Right sibling
    let parent = hash_merkle_node(&leaf_hash, balance, &sibling_hash, sibling_sum).expect("parent");

    let path = vec![MerklePathStep {
        sibling_hash,
        sibling_sum,
        sibling_direction: PathDirection::Right,
    }];

    let ok =
        verify_merkle_path(&leaf_hash, balance, &path, &parent.hash, parent.sum).expect("verify");
    assert!(ok);
}

#[test]
fn test_verify_audit_chain() {
    let mut prev = GENESIS_PREV_HASH;
    let mut records = Vec::new();

    for seq in 1..=3 {
        let hash = hash_audit_entry(seq, 1000 + seq, &prev, 1, 100, 1, 5000);
        records.push(AuditRecord {
            seq,
            ts_micros: 1000 + seq,
            prev,
            kind: 1,
            market: 100,
            qty: 1,
            price: 5000,
            hash,
        });
        prev = hash;
    }

    assert!(verify_audit_chain(&records).is_ok());

    // Corrupt one record
    records[1].price = 9999;
    assert!(verify_audit_chain(&records).is_err());
}
