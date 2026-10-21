//! Pure matching engine: no I/O, no async, no clock, no randomness.
//!
//! The book uses a flat price-level array bounded by the market collar, slab-allocated
//! orders, intrusive FIFO links, and a bitset for best-price lookup.

mod book;
mod engine;

pub use book::{CancelOutcome, MatchFill, Order, OrderBook, PlaceOutcome};
pub use engine::Engine;
