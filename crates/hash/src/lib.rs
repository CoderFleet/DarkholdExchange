//! Hash and cryptographic helpers for Darkhold Exchange.
//!
//! Provides deterministic SHA-256 hashes for the command log, the audit log,
//! and the Merkle-sum solvency tree. All numeric fields are formatted big-endian.

pub mod audit;
pub mod error;
pub mod log;
pub mod merkle;

pub use audit::*;
pub use error::*;
pub use log::*;
pub use merkle::*;

/// Genesis previous hash: 32 zero bytes.
pub const GENESIS_PREV_HASH: [u8; 32] = [0u8; 32];
