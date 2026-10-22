//! Shared wire contract and domain types.
//!
//! Message shapes are pinned by `../../contracts/*.json`, which both the Rust tests
//! and the web tests check against. Keep this crate free of I/O and business logic.

mod api;
mod asset;
mod book;
mod command;
mod error;
mod event;
mod ids;
mod market;
mod order;
mod result;
mod wire;

pub use api::*;
pub use asset::*;
pub use book::*;
pub use command::*;
pub use error::*;
pub use event::*;
pub use ids::*;
pub use market::*;
pub use order::*;
pub use result::*;
pub use wire::*;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Client -> server command. `Command` is the canonical name; `ClientMessage` is
/// kept as an alias for the existing contract tests.
pub type ClientMessage = Command;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "type")]
#[ts(export)]
pub enum ServerMessage {
    Accepted { seq: Seq, order_id: OrderId },
    Rejected { code: ReasonCode },
}
