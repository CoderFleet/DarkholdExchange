import { describe, it, expect } from 'vitest';
import fc from 'fast-check';
import {
  GENESIS_PREV_HASH,
  bytesToHex,
  hexToBytes,
  hashLogEntry,
  hashAuditEntry,
  hashMerkleLeaf,
  hashMerkleLeafWithAsset,
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

  it('matches golden vector for hashMerkleLeafWithAsset', async () => {
    const salt = hexToBytes(vectors.merkle_leaf_asset.salt_hex);

    const hash = await hashMerkleLeafWithAsset(
      salt,
      BigInt(vectors.merkle_leaf_asset.user_id),
      vectors.merkle_leaf_asset.asset_kind,
      vectors.merkle_leaf_asset.market_id,
      BigInt(vectors.merkle_leaf_asset.balance)
    );

    expect(bytesToHex(hash)).toBe(vectors.merkle_leaf_asset.expected_hash);
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

  // Fast-check property test for Merkle path verification and tamper rejection
  it('property-based: for random balances every valid Merkle proof verifies, and changing balance, salt or sibling makes it fail', async () => {
    await fc.assert(
      fc.asyncProperty(
        fc.uint8Array({ minLength: 16, maxLength: 16 }),
        fc.bigInt({ min: 0n, max: 0xffffffffffffffffn }),
        fc.bigInt({ min: 0n, max: 1_000_000_000_000n }),
        fc.array(
          fc.record({
            siblingHash: fc.uint8Array({ minLength: 32, maxLength: 32 }),
            siblingSum: fc.bigInt({ min: 0n, max: 1_000_000_000_000n }),
            siblingDirection: fc.constantFrom<'Left', 'Right'>('Left', 'Right'),
          }),
          { minLength: 1, maxLength: 6 }
        ),
        async (salt, userId, balance, path) => {
          const leafHash = await hashMerkleLeaf(salt, userId, balance);

          // Calculate correct root
          let currHash = leafHash;
          let currSum = balance;
          for (const step of path) {
            let parent;
            if (step.siblingDirection === 'Left') {
              parent = await hashMerkleNode(
                step.siblingHash,
                step.siblingSum,
                currHash,
                currSum
              );
            } else {
              parent = await hashMerkleNode(
                currHash,
                currSum,
                step.siblingHash,
                step.siblingSum
              );
            }
            currHash = parent.hash;
            currSum = parent.sum;
          }

          // 1. Valid proof must pass
          const valid = await verifyMerklePath(
            leafHash,
            balance,
            path,
            currHash,
            currSum
          );
          expect(valid).toBe(true);

          // 2. Tampered balance must fail
          const tamperedBalance = await verifyMerklePath(
            leafHash,
            balance + 1n,
            path,
            currHash,
            currSum
          );
          expect(tamperedBalance).toBe(false);

          // 3. Tampered salt must produce different leaf hash and fail
          const tamperedSalt = new Uint8Array(salt);
          tamperedSalt[0] ^= 0xff;
          const tamperedLeafHash = await hashMerkleLeaf(tamperedSalt, userId, balance);
          const tamperedSaltResult = await verifyMerklePath(
            tamperedLeafHash,
            balance,
            path,
            currHash,
            currSum
          );
          expect(tamperedSaltResult).toBe(false);

          // 4. Tampered sibling hash/sum must fail
          const tamperedPath: MerklePathStep[] = path.map((s, idx) => {
            if (idx === 0) {
              const badHash = new Uint8Array(s.siblingHash);
              badHash[0] ^= 0xff;
              return { ...s, siblingHash: badHash };
            }
            return s;
          });
          const tamperedSiblingResult = await verifyMerklePath(
            leafHash,
            balance,
            tamperedPath,
            currHash,
            currSum
          );
          expect(tamperedSiblingResult).toBe(false);
        }
      ),
      { numRuns: 20 }
    );
  });

  // Fast-check property test for audit log chain verification and tamper detection
  it('property-based: editing an audit record breaks verification at that row', async () => {
    await fc.assert(
      fc.asyncProperty(
        fc.array(
          fc.record({
            kind: fc.integer({ min: 1, max: 5 }),
            market: fc.integer({ min: 1, max: 100 }),
            qty: fc.integer({ min: 1, max: 1000 }),
            price: fc.bigInt({ min: 0n, max: 1_000_000_000n }),
          }),
          { minLength: 2, maxLength: 8 }
        ),
        async (entries) => {
          let prev = GENESIS_PREV_HASH;
          const records: AuditRecord[] = [];

          for (let i = 0; i < entries.length; i++) {
            const seq = BigInt(i + 1);
            const tsMicros = 1_000_000n + seq;
            const hash = await hashAuditEntry(
              seq,
              tsMicros,
              prev,
              entries[i].kind,
              entries[i].market,
              entries[i].qty,
              entries[i].price
            );
            records.push({
              seq,
              tsMicros,
              prev,
              kind: entries[i].kind,
              market: entries[i].market,
              qty: entries[i].qty,
              price: entries[i].price,
              hash,
            });
            prev = hash;
          }

          // Initial valid chain
          const valid = await verifyAuditChain(records);
          expect(valid).toBe(true);

          // Editing any record breaks chain verification
          const editIdx = Math.floor(records.length / 2);
          const corrupted = records.map((r, idx) =>
            idx === editIdx ? { ...r, price: r.price + 100n } : r
          );
          const corruptValid = await verifyAuditChain(corrupted);
          expect(corruptValid).toBe(false);
        }
      ),
      { numRuns: 15 }
    );
  });
});
