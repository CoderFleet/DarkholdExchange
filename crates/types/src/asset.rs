//! Assets and balances shared by the ledger, gateway, and UI.

use crate::{MarketId, Minor};
use serde::{Deserialize, Serialize};

/// Cash or a fungible pass for one market.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Asset {
    Cash,
    Pass { market_id: MarketId },
}

/// A user's balance for one asset. `held` is reserved by open orders/escrow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Balance {
    pub asset: Asset,
    pub available: Minor,
    pub held: Minor,
}
