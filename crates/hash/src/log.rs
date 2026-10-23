use crate::error::HashError;
use sha2::{Digest, Sha256};

pub const LOG_PREFIX: &[u8] = b"DKX-LOG-v1";

/// Compute Log hash:
/// SHA256( 'DKX-LOG-v1' | seq u64 | ts_micros u64 | prev 32B | kind u8 | payload_len u32 | payload )
/// All integers big-endian.
pub fn hash_log_entry(
    seq: u64,
    ts_micros: u64,
    prev: &[u8; 32],
    kind: u8,
    payload: &[u8],
) -> Result<[u8; 32], HashError> {
    let payload_len: u32 = match u32::try_from(payload.len()) {
        Ok(len) => len,
        Err(_) => return Err(HashError::PayloadTooLarge { size: payload.len() }),
    };

    let mut hasher = Sha256::new();
    hasher.update(LOG_PREFIX);
    hasher.update(&seq.to_be_bytes());
    hasher.update(&ts_micros.to_be_bytes());
    hasher.update(prev);
    hasher.update(&[kind]);
    hasher.update(&payload_len.to_be_bytes());
    hasher.update(payload);

    let output = hasher.finalize();
    let mut hash = [0u8; 32];
    hash.copy_from_slice(&output);
    Ok(hash)
}
