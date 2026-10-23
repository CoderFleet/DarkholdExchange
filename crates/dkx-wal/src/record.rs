use std::io::{Read, Write};
use dkx_hash::hash_log_entry;
use crate::error::WalError;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogRecord {
    pub seq: u64,
    pub ts_micros: u64,
    pub prev_hash: [u8; 32],
    pub kind: u8,
    pub payload: Vec<u8>,
    pub hash: [u8; 32],
}

impl LogRecord {
    pub fn new(
        seq: u64,
        ts_micros: u64,
        prev_hash: [u8; 32],
        kind: u8,
        payload: Vec<u8>,
    ) -> Result<Self, WalError> {
        let hash = hash_log_entry(seq, ts_micros, &prev_hash, kind, &payload)?;
        Ok(Self {
            seq,
            ts_micros,
            prev_hash,
            kind,
            payload,
            hash,
        })
    }

    /// Binary format:
    /// total_len u32 (length of the rest of the record)
    /// seq u64
    /// ts_micros u64
    /// prev_hash 32B
    /// kind u8
    /// payload_len u32
    /// payload [u8]
    /// hash 32B
    pub fn encode_length_prefixed(&self) -> Vec<u8> {
        let record_body_len = 8 + 8 + 32 + 1 + 4 + self.payload.len() + 32;
        let mut buf = Vec::with_capacity(4 + record_body_len);

        buf.extend_from_slice(&(record_body_len as u32).to_be_bytes());
        buf.extend_from_slice(&self.seq.to_be_bytes());
        buf.extend_from_slice(&self.ts_micros.to_be_bytes());
        buf.extend_from_slice(&self.prev_hash);
        buf.push(self.kind);
        buf.extend_from_slice(&(self.payload.len() as u32).to_be_bytes());
        buf.extend_from_slice(&self.payload);
        buf.extend_from_slice(&self.hash);

        buf
    }

    /// Attempts to decode one record from a reader. Returns Ok(None) if EOF is reached.
    /// If fewer bytes than expected exist (torn write), returns Ok(None) to drop cleanly.
    pub fn decode_length_prefixed<R: Read>(reader: &mut R) -> Result<Option<Self>, WalError> {
        let mut len_buf = [0u8; 4];
        match reader.read_exact(&mut len_buf) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(e) => return Err(WalError::Io(e)),
        }

        let body_len = u32::from_be_bytes(len_buf) as usize;
        if body_len < (8 + 8 + 32 + 1 + 4 + 32) {
            return Ok(None);
        }

        let mut body = vec![0u8; body_len];
        match reader.read_exact(&mut body) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(e) => return Err(WalError::Io(e)),
        }

        let mut offset = 0;
        let seq = u64::from_be_bytes(body[offset..offset + 8].try_into().map_err(|_| WalError::SequenceGap { expected: 0, got: 0 })?);
        offset += 8;

        let ts_micros = u64::from_be_bytes(body[offset..offset + 8].try_into().map_err(|_| WalError::SequenceGap { expected: 0, got: 0 })?);
        offset += 8;

        let mut prev_hash = [0u8; 32];
        prev_hash.copy_from_slice(&body[offset..offset + 32]);
        offset += 32;

        let kind = body[offset];
        offset += 1;

        let payload_len = u32::from_be_bytes(body[offset..offset + 4].try_into().map_err(|_| WalError::SequenceGap { expected: 0, got: 0 })?) as usize;
        offset += 4;

        if offset + payload_len + 32 != body_len {
            return Ok(None); // corrupted or torn
        }

        let payload = body[offset..offset + payload_len].to_vec();
        offset += payload_len;

        let mut hash = [0u8; 32];
        hash.copy_from_slice(&body[offset..offset + 32]);

        Ok(Some(Self {
            seq,
            ts_micros,
            prev_hash,
            kind,
            payload,
            hash,
        }))
    }
}
