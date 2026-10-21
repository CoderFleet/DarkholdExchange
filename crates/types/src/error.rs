//! Stable rejection codes. The hot path never allocates a human-readable string;
//! the client maps these codes to copy.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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
