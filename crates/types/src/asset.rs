//! Assets and balances shared by the ledger, gateway, and UI.

use crate::{MarketId, Minor};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Cash or a fungible pass for one market.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export)]
pub enum Asset {
    Cash,
    Pass { market_id: MarketId },
}

/// A user's balance for one asset. `held` is reserved by open orders/escrow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Balance {
    pub asset: Asset,
    pub available: Minor,
    pub held: Minor,
}
