use crate::error::WalError;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotData {
    pub last_seq: u64,
    pub last_hash: [u8; 32],
    pub ledger_bytes: Vec<u8>,
    pub escrow_bytes: Vec<u8>,
    pub engine_bytes: Vec<u8>,
    pub state_hash: [u8; 32],
}

impl SnapshotData {
    pub fn new(
        last_seq: u64,
        last_hash: [u8; 32],
        ledger_bytes: Vec<u8>,
        escrow_bytes: Vec<u8>,
        engine_bytes: Vec<u8>,
    ) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(last_seq.to_be_bytes());
        hasher.update(last_hash);
        hasher.update((ledger_bytes.len() as u32).to_be_bytes());
        hasher.update(&ledger_bytes);
        hasher.update((escrow_bytes.len() as u32).to_be_bytes());
        hasher.update(&escrow_bytes);
        hasher.update((engine_bytes.len() as u32).to_be_bytes());
        hasher.update(&engine_bytes);

        let output = hasher.finalize();
        let mut state_hash = [0u8; 32];
        state_hash.copy_from_slice(&output);

        Self {
            last_seq,
            last_hash,
            ledger_bytes,
            escrow_bytes,
            engine_bytes,
            state_hash,
        }
    }

    pub fn verify(&self) -> Result<(), WalError> {
        let expected = Self::new(
            self.last_seq,
            self.last_hash,
            self.ledger_bytes.clone(),
            self.escrow_bytes.clone(),
            self.engine_bytes.clone(),
        );

        if self.state_hash == expected.state_hash {
            Ok(())
        } else {
            Err(WalError::SnapshotCorrupted)
        }
    }
}
