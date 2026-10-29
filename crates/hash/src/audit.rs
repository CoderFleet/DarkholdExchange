use crate::error::HashError;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const AUDIT_PREFIX: &[u8] = b"DKX-AUD-v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditRecord {
    pub seq: u64,
    pub ts_micros: u64,
    pub prev: [u8; 32],
    pub kind: u8,
    pub market: u32,
    pub qty: u32,
    pub price: i64,
    pub hash: [u8; 32],
}

/// Compute Audit hash:
/// SHA256( 'DKX-AUD-v1' | seq u64 | ts_micros u64 | prev 32B | kind u8 | market u32 | qty u32 | price i64 )
/// All integers big-endian.
pub fn hash_audit_entry(
    seq: u64,
    ts_micros: u64,
    prev: &[u8; 32],
    kind: u8,
    market: u32,
    qty: u32,
    price: i64,
) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(AUDIT_PREFIX);
    hasher.update(seq.to_be_bytes());
    hasher.update(ts_micros.to_be_bytes());
    hasher.update(prev);
    hasher.update([kind]);
    hasher.update(market.to_be_bytes());
    hasher.update(qty.to_be_bytes());
    hasher.update(price.to_be_bytes());

    let output = hasher.finalize();
    let mut hash = [0u8; 32];
    hash.copy_from_slice(&output);
    hash
}

/// Verifies that an audit chain is continuous and every hash is valid.
pub fn verify_audit_chain(records: &[AuditRecord]) -> Result<(), HashError> {
    if records.is_empty() {
        return Ok(());
    }

    for (i, record) in records.iter().enumerate() {
        if i > 0 {
            let prev_record = &records[i - 1];
            if record.prev != prev_record.hash {
                return Err(HashError::AuditChainBroken {
                    seq: record.seq,
                    expected: hex_encode(&prev_record.hash),
                    actual: hex_encode(&record.prev),
                });
            }
        }

        let computed = hash_audit_entry(
            record.seq,
            record.ts_micros,
            &record.prev,
            record.kind,
            record.market,
            record.qty,
            record.price,
        );

        if computed != record.hash {
            return Err(HashError::AuditHashMismatch { seq: record.seq });
        }
    }

    Ok(())
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(s, "{:02x}", b);
    }
    s
}
