//! WebSocket wire messages. The wire is MessagePack; JSON fixtures remain the
//! canonical readable contract.

use crate::{
    Balance, BookDelta, BookSnapshot, Fill, MarketConfig, MarketId, MetricsSnapshot, ReasonCode,
    UserId,
};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "type")]
#[ts(export)]
pub enum WsClientMessage {
    /// Select markets and depth. An empty list means all markets.
    Subscribe {
        market_ids: Vec<MarketId>,
        depth: u16,
    },
    Ping,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "type")]
#[ts(export)]
pub enum WsServerMessage {
    Hello {
        markets: Vec<MarketConfig>,
        user_id: Option<UserId>,
    },
    Snapshot {
        snapshot: BookSnapshot,
    },
    /// Absolute level changes. Apply only when `delta.book_seq` is the next
    /// sequence for that market.
    Delta {
        delta: BookDelta,
    },
    Trade {
        fill: Fill,
    },
    /// The client fell behind. Discard the local book and wait for a snapshot.
    ResyncRequired {
        market_id: MarketId,
    },
    Metrics {
        metrics: MetricsSnapshot,
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
