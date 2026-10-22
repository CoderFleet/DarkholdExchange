//! REST response bodies shared by the gateway and the web client.

use crate::{BookSnapshot, MarketConfig, MarketId, OrderId, Qty, ReasonCode, Seq, Side, Ticks};
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

/// Point-in-time gateway and engine health. Synthetic `p99` fields stay zero
/// until the sequencer and latency histogram land; counters are live.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct MetricsSnapshot {
    /// Last command sequence processed by the core. Zero before the first command.
    pub seq: Seq,
    pub queue_depth: u32,
    pub commands_total: u64,
    pub rejected_total: u64,
    /// Microseconds. Zero while the bounded command queue is not wired.
    pub queue_wait_p99_us: u64,
    /// Microseconds. Zero until the latency histogram is wired.
    pub core_p99_us: u64,
    pub ws_clients: u32,
    pub slow_consumers: u64,
}
