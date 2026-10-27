//! Single-writer core: matching engine + ledger + reservations + idempotency.

mod core;

pub use core::{CommandOutcome, Core, ESCROW_TIMEOUT_MS};
