//! Stable rejection codes. The hot path never allocates a human-readable string;
//! the client maps these codes to copy.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ReasonCode {
    OutsideCollar,
    InsufficientFunds,
    InsufficientInventory,
    HoldingLimitExceeded,
    SelfTrade,
    UnknownMarket,
    MarketClosed,
    DuplicateClientOrderId,
    InvalidQuantity,
    InvalidPrice,
    PriceNotTickAligned,
    InvalidRequest,
    OrderNotFound,
    NotOrderOwner,
    CancelNotAllowed,
    RateLimited,
    QueueFull,
    AuctionNotActive,
    AuctionClosed,
    StaleAuctionPrice,
    EscrowNotReady,
    InvalidReceipt,
    ReceiptExpired,
    InternalError,
}
