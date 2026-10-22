//! Public book snapshots and absolute level deltas.

use crate::{BookSeq, MarketId, Qty, Side, Ticks};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct BookLevel {
    pub price_ticks: Ticks,
    pub qty: Qty,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct BookSnapshot {
    pub market_id: MarketId,
    pub book_seq: BookSeq,
    pub bids: Vec<BookLevel>,
    pub asks: Vec<BookLevel>,
    pub last_trade_price: Option<Ticks>,
}

impl BookSnapshot {
    /// Bids are in display order (highest first).
    pub fn best_bid(&self) -> Option<Ticks> {
        self.bids.first().map(|level| level.price_ticks)
    }

    /// Asks are in display order (lowest first).
    pub fn best_ask(&self) -> Option<Ticks> {
        self.asks.first().map(|level| level.price_ticks)
    }
}

/// Absolute quantity at a level. `qty == 0` means the level was removed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LevelChange {
    pub side: Side,
    pub price_ticks: Ticks,
    pub qty: Qty,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct BookDelta {
    pub market_id: MarketId,
    pub book_seq: BookSeq,
    pub changes: Vec<LevelChange>,
}
