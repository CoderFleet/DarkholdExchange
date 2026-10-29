//! Single-writer core: matching engine + ledger + reservations + idempotency.

mod core;

pub use core::snapshot::{IdempotencyEntry, RestSnapshot, RestoredState, SnapshotCommandKind};
pub use core::{CommandOutcome, Core, ESCROW_TIMEOUT_MS};
