//! Domain events emitted by the core after a command is sequenced.

use crate::{MarketId, OrderId, Qty, Seq, Side, Ticks, TradeId, UserId};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// A settled fill. `seq` is the global command sequence that produced it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Fill {
    pub trade_id: TradeId,
    pub market_id: MarketId,
    pub maker_order_id: OrderId,
    pub taker_order_id: OrderId,
    pub maker_user_id: UserId,
    pub taker_user_id: UserId,
    pub maker_side: Side,
    pub price_ticks: Ticks,
    pub qty: Qty,
    pub seq: Seq,
    /// Set when the fill was settled into escrow; absent for spot fills.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub escrow_id: Option<u64>,
}
