//! REST response bodies shared by the gateway and the web client.

use crate::{BookSnapshot, MarketConfig, MarketId, OrderId, Qty, ReasonCode, Side, Ticks};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct MarketView {
    pub config: MarketConfig,
    pub snapshot: BookSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct OpenOrderView {
    pub market_id: MarketId,
    pub order_id: OrderId,
    pub side: Side,
    pub price_ticks: Ticks,
    pub qty: Qty,
    pub remaining: Qty,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct JournalResponse {
    pub journal_id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ErrorBody {
    pub reason: ReasonCode,
}
