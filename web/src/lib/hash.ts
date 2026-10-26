/**
 * Cryptographic and hashing helpers for Darkhold Exchange.
 * Implements identical hashing and big-endian encoding to the Rust dkx-hash crate.
 */

export const GENESIS_PREV_HASH = new Uint8Array(32);
const LOG_PREFIX = new TextEncoder().encode('DKX-LOG-v1');
const AUDIT_PREFIX = new TextEncoder().encode('DKX-AUD-v1');
const MERKLE_LEAF_PREFIX = 0x00;
const MERKLE_NODE_PREFIX = 0x01;

export const EMPTY_NODE_HASH = new Uint8Array(32);
export const EMPTY_NODE_SUM = 0n;

export type PathDirection = 'Left' | 'Right';

export interface MerklePathStep {
  siblingHash: Uint8Array;
  siblingSum: bigint;
  siblingDirection: PathDirection;
}

export interface AuditRecord {
  seq: bigint;
  tsMicros: bigint;
  prev: Uint8Array;
  kind: number;
  market: number;
  qty: number;
  price: bigint;
  hash: Uint8Array;
}

export interface MerkleNode {
  hash: Uint8Array;
  sum: bigint;
}

/** Helper to convert Uint8Array to hex string */
export function bytesToHex(bytes: Uint8Array): string {
  return Array.from(bytes)
    .map((b) => b.toString(16).padStart(2, '0'))
    .join('');
}

/** Helper to convert hex string to Uint8Array */
export function hexToBytes(hex: string): Uint8Array {
  const cleanHex = hex.startsWith('0x') ? hex.slice(2) : hex;
  if (cleanHex.length % 2 !== 0) {
    throw new Error(`Invalid hex string length: ${cleanHex.length}`);
  }
  const bytes = new Uint8Array(cleanHex.length / 2);
  for (let i = 0; i < cleanHex.length; i += 2) {
    bytes[i / 2] = parseInt(cleanHex.substring(i, i + 2), 16);
  }
  return bytes;
}

/** Helper to compare two byte arrays */
export function bytesEqual(a: Uint8Array, b: Uint8Array): boolean {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) {
    if (a[i] !== b[i]) return false;
  }
  return true;
}

/** Computes SHA-256 using Web Crypto API */
async function sha256(data: Uint8Array): Promise<Uint8Array> {
  const hashBuffer = await crypto.subtle.digest('SHA-256', data as unknown as BufferSource);
  return new Uint8Array(hashBuffer);
}

/**
 * Log hash = SHA256( 'DKX-LOG-v1' | seq u64 | ts_micros u64 | prev 32B | kind u8 | payload_len u32 | payload )
 * All integers big-endian.
 */
export async function hashLogEntry(
  seq: bigint,
  tsMicros: bigint,
  prev: Uint8Array,
  kind: number,
  payload: Uint8Array
): Promise<Uint8Array> {
  if (prev.length !== 32) {
    throw new Error(`Expected prev hash to be 32 bytes, got ${prev.length}`);
  }
  const totalLen = LOG_PREFIX.length + 8 + 8 + 32 + 1 + 4 + payload.length;
  const buffer = new Uint8Array(totalLen);
  const view = new DataView(buffer.buffer, buffer.byteOffset, buffer.byteLength);

  let offset = 0;
  buffer.set(LOG_PREFIX, offset);
  offset += LOG_PREFIX.length;

  view.setBigUint64(offset, seq, false);
  offset += 8;

  view.setBigUint64(offset, tsMicros, false);
  offset += 8;

  buffer.set(prev, offset);
  offset += 32;

  view.setUint8(offset, kind);
  offset += 1;

  view.setUint32(offset, payload.length, false);
  offset += 4;

  buffer.set(payload, offset);

  return sha256(buffer);
}

/**
 * Audit hash = SHA256( 'DKX-AUD-v1' | seq u64 | ts_micros u64 | prev 32B | kind u8 | market u32 | qty u32 | price i64 )
 * All integers big-endian.
 */
export async function hashAuditEntry(
  seq: bigint,
  tsMicros: bigint,
  prev: Uint8Array,
  kind: number,
  market: number,
  qty: number,
  price: bigint
): Promise<Uint8Array> {
  if (prev.length !== 32) {
    throw new Error(`Expected prev hash to be 32 bytes, got ${prev.length}`);
  }
  const totalLen = AUDIT_PREFIX.length + 8 + 8 + 32 + 1 + 4 + 4 + 8;
  const buffer = new Uint8Array(totalLen);
  const view = new DataView(buffer.buffer, buffer.byteOffset, buffer.byteLength);

  let offset = 0;
  buffer.set(AUDIT_PREFIX, offset);
  offset += AUDIT_PREFIX.length;

  view.setBigUint64(offset, seq, false);
  offset += 8;

  view.setBigUint64(offset, tsMicros, false);
  offset += 8;

  buffer.set(prev, offset);
  offset += 32;

  view.setUint8(offset, kind);
  offset += 1;

  view.setUint32(offset, market, false);
  offset += 4;

  view.setUint32(offset, qty, false);
  offset += 4;

  view.setBigInt64(offset, price, false);

  return sha256(buffer);
}

/**
 * Merkle leaf = SHA256( 0x00 | salt 16B | user_id u64 | balance u64 )
 * All integers big-endian.
 */
export async function hashMerkleLeaf(
  salt: Uint8Array,
  userId: bigint,
  balance: bigint
): Promise<Uint8Array> {
  if (salt.length !== 16) {
    throw new Error(`Expected salt to be 16 bytes, got ${salt.length}`);
  }
  const buffer = new Uint8Array(1 + 16 + 8 + 8);
  const view = new DataView(buffer.buffer, buffer.byteOffset, buffer.byteLength);

  let offset = 0;
  view.setUint8(offset, MERKLE_LEAF_PREFIX);
  offset += 1;

  buffer.set(salt, offset);
  offset += 16;

  view.setBigUint64(offset, userId, false);
  offset += 8;

  view.setBigUint64(offset, balance, false);

  return sha256(buffer);
}

/**
 * Asset-aware Merkle leaf = SHA256( 0x00 | salt 16B | user_id u64 |
 * asset_kind u8 | market_id u32 | balance u64 )
 * All integers big-endian. Cash uses asset_kind 0 and market_id 0.
 */
export async function hashMerkleLeafWithAsset(
  salt: Uint8Array,
  userId: bigint,
  assetKind: number,
  marketId: number,
  balance: bigint
): Promise<Uint8Array> {
  if (salt.length !== 16) {
    throw new Error(`Expected salt to be 16 bytes, got ${salt.length}`);
  }
  const buffer = new Uint8Array(1 + 16 + 8 + 1 + 4 + 8);
  const view = new DataView(buffer.buffer, buffer.byteOffset, buffer.byteLength);

  let offset = 0;
  view.setUint8(offset, MERKLE_LEAF_PREFIX);
  offset += 1;

  buffer.set(salt, offset);
  offset += 16;

  view.setBigUint64(offset, userId, false);
  offset += 8;

  view.setUint8(offset, assetKind);
  offset += 1;

  view.setUint32(offset, marketId, false);
  offset += 4;

  view.setBigUint64(offset, balance, false);

  return sha256(buffer);
}

/**
 * Node = SHA256( 0x01 | left_hash 32B | left_sum u64 | right_hash 32B | right_sum u64 )
 * node sum = left_sum + right_sum (check overflow).
 * All integers big-endian.
 */
export async function hashMerkleNode(
  leftHash: Uint8Array,
  leftSum: bigint,
  rightHash: Uint8Array,
  rightSum: bigint
): Promise<MerkleNode> {
  if (leftHash.length !== 32 || rightHash.length !== 32) {
    throw new Error('Left and right hashes must be 32 bytes each');
  }

  const MAX_U64 = 0xffffffffffffffffn;
  const sum = leftSum + rightSum;
  if (sum > MAX_U64) {
    throw new Error('Arithmetic overflow computing Merkle sum');
  }

  const buffer = new Uint8Array(1 + 32 + 8 + 32 + 8);
  const view = new DataView(buffer.buffer, buffer.byteOffset, buffer.byteLength);

  let offset = 0;
  view.setUint8(offset, MERKLE_NODE_PREFIX);
  offset += 1;

  buffer.set(leftHash, offset);
  offset += 32;

  view.setBigUint64(offset, leftSum, false);
  offset += 8;

  buffer.set(rightHash, offset);
  offset += 32;

  view.setBigUint64(offset, rightSum, false);

  const hash = await sha256(buffer);
  return { hash, sum };
}

/**
 * Odd count: pair the last node with the empty node (zero hash, sum 0).
 */
export async function pairWithEmpty(node: MerkleNode): Promise<MerkleNode> {
  return hashMerkleNode(node.hash, node.sum, EMPTY_NODE_HASH, EMPTY_NODE_SUM);
}

/**
 * Verifies a Merkle inclusion path against an expected root hash and total balance.
 */
export async function verifyMerklePath(
  leafHash: Uint8Array,
  leafBalance: bigint,
  path: MerklePathStep[],
  expectedRootHash: Uint8Array,
  expectedTotal: bigint
): Promise<boolean> {
  let currentHash = leafHash;
  let currentSum = leafBalance;

  for (const step of path) {
    let parent: MerkleNode;
    if (step.siblingDirection === 'Left') {
      parent = await hashMerkleNode(
        step.siblingHash,
        step.siblingSum,
        currentHash,
        currentSum
      );
    } else {
      parent = await hashMerkleNode(
        currentHash,
        currentSum,
        step.siblingHash,
        step.siblingSum
      );
    }
    currentHash = parent.hash;
    currentSum = parent.sum;
  }

  return (
    bytesEqual(currentHash, expectedRootHash) &&
    currentSum === expectedTotal
  );
}

/**
 * Verifies an audit log chain for continuity and correct hashes.
 */
export async function verifyAuditChain(records: AuditRecord[]): Promise<boolean> {
  if (records.length === 0) return true;

  for (let i = 0; i < records.length; i++) {
    const record = records[i];

    if (i > 0) {
      const prevRecord = records[i - 1];
      if (!bytesEqual(record.prev, prevRecord.hash)) {
        return false;
      }
    }

    const computed = await hashAuditEntry(
      record.seq,
      record.tsMicros,
      record.prev,
      record.kind,
      record.market,
      record.qty,
      record.price
    );

    if (!bytesEqual(computed, record.hash)) {
      return false;
    }
  }

  return true;
}
