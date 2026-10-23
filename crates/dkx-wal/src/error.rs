use thiserror::Error;

#[derive(Debug, Error)]
pub enum WalError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    Serialization(#[from] rmp_serde::encode::Error),

    #[error("Deserialization error: {0}")]
    Deserialization(#[from] rmp_serde::decode::Error),

    #[error("Hash error: {0}")]
    Hash(#[from] dkx_hash::HashError),

    #[error("WAL hash chain broken at seq {seq}: expected prev {expected}, found {actual}")]
    ChainBroken {
        seq: u64,
        expected: String,
        actual: String,
    },

    #[error("Record hash mismatch at seq {seq}")]
    RecordHashMismatch { seq: u64 },

    #[error("Snapshot corrupted: state hash mismatch")]
    SnapshotCorrupted,

    #[error("Invalid sequence: expected {expected}, got {got}")]
    SequenceGap { expected: u64, got: u64 },
}
