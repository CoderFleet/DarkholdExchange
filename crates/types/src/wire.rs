//! WebSocket wire messages. The wire is MessagePack; JSON fixtures remain the
//! canonical readable contract.

use crate::{Balance, BookSnapshot, Fill, MarketConfig, MarketId, ReasonCode, UserId};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum WsClientMessage {
    /// Select markets and depth. An empty list means all markets.
    Subscribe {
        market_ids: Vec<MarketId>,
        depth: u16,
    },
    Ping,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum WsServerMessage {
    Hello {
        markets: Vec<MarketConfig>,
        user_id: Option<UserId>,
    },
    Snapshot {
        snapshot: BookSnapshot,
    },
    Trade {
        fill: Fill,
    },
    Balances {
        user_id: UserId,
        balances: Vec<Balance>,
    },
    Pong,
    Error {
        code: ReasonCode,
    },
}
