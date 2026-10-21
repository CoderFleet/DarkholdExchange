//! Stable identifiers and numeric units used across the workspace.

/// A verified identity. Per-identity holding limits attach to this.
pub type UserId = u64;

/// An event tier or relic series. Each market has one order book.
pub type MarketId = u32;

/// Global, monotonically increasing order id assigned by the core.
pub type OrderId = u64;

/// Global trade id assigned when a fill is settled.
pub type TradeId = u64;

/// Global command sequence assigned by the single-writer core.
pub type Seq = u64;

/// Per-market book sequence assigned on every book change.
pub type BookSeq = u64;

/// Client-supplied idempotency key for a command.
pub type ClientOrderId = u64;

/// Money in integer minor units (paise). Never use floats for money.
pub type Minor = i64;

/// Price expressed in ticks. One tick is `MarketConfig::tick_size_minor`.
pub type Ticks = u64;

/// Fungible quantity within a market.
pub type Qty = u64;
