import { describe, it, expect } from 'vitest';
import {
  GENESIS_PREV_HASH,
  bytesToHex,
  hexToBytes,
  hashLogEntry,
  hashAuditEntry,
  hashMerkleLeaf,
  hashMerkleNode,
  pairWithEmpty,
  verifyMerklePath,
  verifyAuditChain,
  type AuditRecord,
  type MerklePathStep,
} from './hash';
import vectors from '../../../contracts/hash_test_vectors.json';

describe('hash helper (Web / TypeScript)', () => {
  it('has genesis prev hash with 32 zero bytes', () => {
    expect(bytesToHex(GENESIS_PREV_HASH)).toBe(vectors.genesis_prev);
  });

  it('matches golden vector for hashLogEntry', async () => {
    const prev = hexToBytes(vectors.log_entry.prev);
    const payload = hexToBytes(vectors.log_entry.payload_hex);

    const hash = await hashLogEntry(
      BigInt(vectors.log_entry.seq),
      BigInt(vectors.log_entry.ts_micros),
      prev,
      vectors.log_entry.kind,
      payload
    );

    expect(bytesToHex(hash)).toBe(vectors.log_entry.expected_hash);
  });

  it('matches golden vector for hashAuditEntry', async () => {
    const prev = hexToBytes(vectors.audit_entry.prev);

    const hash = await hashAuditEntry(
      BigInt(vectors.audit_entry.seq),
      BigInt(vectors.audit_entry.ts_micros),
      prev,
      vectors.audit_entry.kind,
      vectors.audit_entry.market,
      vectors.audit_entry.qty,
      BigInt(vectors.audit_entry.price)
    );

    expect(bytesToHex(hash)).toBe(vectors.audit_entry.expected_hash);
  });

  it('matches golden vector for hashMerkleLeaf', async () => {
    const salt = hexToBytes(vectors.merkle_leaf.salt_hex);

    const hash = await hashMerkleLeaf(
      salt,
      BigInt(vectors.merkle_leaf.user_id),
      BigInt(vectors.merkle_leaf.balance)
    );

    expect(bytesToHex(hash)).toBe(vectors.merkle_leaf.expected_hash);
  });

  it('matches golden vector for hashMerkleNode', async () => {
    const leftHash = hexToBytes(vectors.merkle_node.left_hash);
    const rightHash = hexToBytes(vectors.merkle_node.right_hash);

    const node = await hashMerkleNode(
      leftHash,
      BigInt(vectors.merkle_node.left_sum),
      rightHash,
      BigInt(vectors.merkle_node.right_sum)
    );

    expect(node.sum).toBe(BigInt(vectors.merkle_node.expected_sum));
    expect(bytesToHex(node.hash)).toBe(vectors.merkle_node.expected_hash);
  });

  it('pairs odd-count node with empty node', async () => {
    const dummyHash = new Uint8Array(32).fill(1);
    const node = { hash: dummyHash, sum: 500n };

    const paired = await pairWithEmpty(node);
    expect(paired.sum).toBe(500n);

    const manual = await hashMerkleNode(dummyHash, 500n, new Uint8Array(32), 0n);
    expect(bytesToHex(paired.hash)).toBe(bytesToHex(manual.hash));
  });

  it('verifies a valid Merkle inclusion path', async () => {
    const leafSalt = new Uint8Array(16).fill(7);
    const userId = 99n;
    const balance = 12500n;
    const leafHash = await hashMerkleLeaf(leafSalt, userId, balance);

    const siblingSalt = new Uint8Array(16).fill(8);
    const siblingHash = await hashMerkleLeaf(siblingSalt, 100n, 7500n);
    const siblingSum = 7500n;

    const parent = await hashMerkleNode(leafHash, balance, siblingHash, siblingSum);

    const path: MerklePathStep[] = [
      {
        siblingHash,
        siblingSum,
        siblingDirection: 'Right',
      },
    ];

    const valid = await verifyMerklePath(
      leafHash,
      balance,
      path,
      parent.hash,
      parent.sum
    );

    expect(valid).toBe(true);

    // Tampered balance should fail
    const tampered = await verifyMerklePath(
      leafHash,
      balance + 1n,
      path,
      parent.hash,
      parent.sum
    );
    expect(tampered).toBe(false);
  });

  it('verifies a valid audit chain and flags tampered entries', async () => {
    let prev = GENESIS_PREV_HASH;
    const records: AuditRecord[] = [];

    for (let seq = 1n; seq <= 3n; seq++) {
      const hash = await hashAuditEntry(
        seq,
        1000n + seq,
        prev,
        1,
        100,
        1,
        5000n
      );
      records.push({
        seq,
        tsMicros: 1000n + seq,
        prev,
        kind: 1,
        market: 100,
        qty: 1,
        price: 5000n,
        hash,
      });
      prev = hash;
    }

    const isValid = await verifyAuditChain(records);
    expect(isValid).toBe(true);

    // Corrupt one record
    records[1].price = 9999n;
    const isCorruptValid = await verifyAuditChain(records);
    expect(isCorruptValid).toBe(false);
  });
});
