use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq, Clone)]
pub enum HashError {
    #[error("arithmetic overflow computing Merkle sum")]
    SumOverflow,

    #[error("invalid hash length: expected 32 bytes, got {got}")]
    InvalidHashLength { got: usize },

    #[error("invalid salt length: expected 16 bytes, got {got}")]
    InvalidSaltLength { got: usize },

    #[error("payload exceeds maximum allowable size: {size}")]
    PayloadTooLarge { size: usize },

    #[error("audit chain broken at seq {seq}: expected prev {expected}, found {actual}")]
    AuditChainBroken {
        seq: u64,
        expected: String,
        actual: String,
    },

    #[error("audit entry hash mismatch at seq {seq}")]
    AuditHashMismatch { seq: u64 },
}
