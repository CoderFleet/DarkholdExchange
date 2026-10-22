//! The synchronous result of one sequenced command.

use crate::{Fill, OrderId, OrderStatus, Qty, ReasonCode, Seq};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CommandResponse {
    pub seq: Seq,
    pub status: OrderStatus,
    pub order_id: Option<OrderId>,
    pub filled_qty: Qty,
    pub remaining_qty: Qty,
    pub fills: Vec<Fill>,
    /// Present only when `status == Rejected`.
    pub reason: Option<ReasonCode>,
}

impl CommandResponse {
    pub fn rejected(seq: Seq, reason: ReasonCode) -> Self {
        Self {
            seq,
            status: OrderStatus::Rejected,
            order_id: None,
            filled_qty: 0,
            remaining_qty: 0,
            fills: Vec::new(),
            reason: Some(reason),
        }
    }
}
