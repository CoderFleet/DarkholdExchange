//! Commands accepted by the core. `user_id` is intentionally absent: the gateway
//! derives it from the authenticated session and never trusts a body field.

use crate::{ClientOrderId, MarketId, OrderId, Qty, Side, Ticks};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct PlaceLimit {
    pub client_order_id: ClientOrderId,
    pub market_id: MarketId,
    pub side: Side,
    pub price_ticks: Ticks,
    pub qty: Qty,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct PlaceMarket {
    pub client_order_id: ClientOrderId,
    pub market_id: MarketId,
    pub side: Side,
    pub qty: Qty,
    /// Buy: maximum price. Sell: minimum price.
    pub protect_price_ticks: Ticks,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct Cancel {
    /// Idempotency key for this cancel request.
    pub client_order_id: ClientOrderId,
    pub order_id: OrderId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "type")]
#[ts(export)]
pub enum Command {
    PlaceLimit(PlaceLimit),
    PlaceMarket(PlaceMarket),
    Cancel(Cancel),
}
