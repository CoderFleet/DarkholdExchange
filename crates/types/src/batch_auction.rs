//! Batch auction (commit-reveal) types.
//!
//! A batch auction is a pooled, admin-scheduled window for one market. Both
//! sides enter sealed orders and the whole window clears at one price:
//!
//! 1. **Commit** – buyers submit a SHA-256 hash commitment of their bid
//!    (price, qty, salt); sellers do the same for their ask. The values stay
//!    secret until the reveal phase.
//!
//! 2. **Reveal** – after `commit_end_ms` parties broadcast the pre-image
//!    (price, qty, salt). The core verifies the commitment. Revealed bids
//!    reserve cash; revealed asks reserve passes — nobody can win without the
//!    assets already locked up.
//!
//! 3. **Settlement** – once `reveal_end_ms` passes, anyone may call
//!    [`SettleBatchAuction`]. The core finds the price that maximises traded
//!    volume (the highest such price), then fills winning bids (price
//!    descending, ties by `client_order_id` ascending) from the revealing asks
//!    (price ascending, ties by `client_order_id` ascending) in that order. A
//!    counterpart pair of the same user is skipped; every fill happens at the
//!    one uniform clearing price, and unsold or unfilled reservations are
//!    released.

use crate::{ClientOrderId, MarketId, Qty, Ticks, TradeId, UserId};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

// ---------------------------------------------------------------------------
// Phase
// ---------------------------------------------------------------------------

/// Current lifecycle phase of a [`BatchAuction`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum BatchAuctionPhase {
    /// Commit window is open; bidders may submit hash commitments.
    Commit,
    /// Commit window is closed; bidders may reveal their pre-images.
    Reveal,
    /// Reveal window is closed; settlement is in progress (transient).
    Clearing,
    /// Settlement is complete; `clearing_price_ticks` is populated.
    Settled,
    /// The auction ended without clearing (e.g., no valid reveals).
    Expired,
}

// ---------------------------------------------------------------------------
// Auction state
// ---------------------------------------------------------------------------

/// Full state of one batch auction, stored in Core and returned by queries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct BatchAuction {
    /// Globally unique auction identifier assigned by the core.
    pub id: u64,
    /// The market whose passes this window pools.
    pub market_id: MarketId,
    /// The admin/issuer that scheduled the window (audit trail only; they have
    /// no supply of their own here).
    pub created_by: UserId,
    /// Bids and asks below this price are invalid; collar-enforced at creation.
    pub floor_price_ticks: Ticks,
    /// Unix-epoch millisecond at which the commit window closes.
    pub commit_end_ms: u64,
    /// Unix-epoch millisecond at which the reveal window closes.
    pub reveal_end_ms: u64,
    /// Uniform clearing price set after settlement; `None` until then.
    pub clearing_price_ticks: Option<Ticks>,
    /// Current lifecycle phase.
    pub phase: BatchAuctionPhase,
    /// Total quantity revealed by valid asks at settlement; `None` before settlement.
    pub qty_total: Option<Qty>,
    /// Unsold quantity released back to the sellers after settlement; `None`
    /// before settlement.
    pub qty_remaining: Option<Qty>,
}

// ---------------------------------------------------------------------------
// Bidder state (Core-internal)
// ---------------------------------------------------------------------------

/// Per-bidder record maintained by Core across all three phases.
///
/// This is an internal type; it is not exposed on the public API wire but is
/// serialised for persistence and snapshots.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct BidRecord {
    /// Identity of the bidder.
    pub bidder_id: UserId,
    /// Idempotency key supplied with the original [`CommitBid`].
    pub client_order_id: ClientOrderId,
    /// The 32-byte commitment hash submitted during the commit phase.
    pub commitment: [u8; 32],
    /// Price revealed during the reveal phase; `None` before reveal.
    pub price_ticks: Option<Ticks>,
    /// Quantity revealed during the reveal phase; `None` before reveal.
    pub qty: Option<Qty>,
    /// Quantity allocated to this bidder after settlement.
    pub filled_qty: Qty,
    /// `false` when the revealed pre-image did not match `commitment`, or
    /// when the bid falls below `floor_price_ticks` or outside the collar.
    pub valid: bool,
}

/// Per-seller record mirroring [`BidRecord`] for the ask side of the window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AskRecord {
    /// Identity of the seller.
    pub seller_id: UserId,
    /// Idempotency key supplied with the original [`CommitAsk`].
    pub client_order_id: ClientOrderId,
    /// The 32-byte commitment hash submitted during the commit phase.
    pub commitment: [u8; 32],
    /// Minimum price revealed during the reveal phase; `None` before reveal.
    pub price_ticks: Option<Ticks>,
    /// Quantity revealed during the reveal phase; `None` before reveal.
    pub qty: Option<Qty>,
    /// Quantity sold to the pool by this ask after settlement.
    pub filled_qty: Qty,
    /// `false` when the revealed pre-image did not match `commitment`, or
    /// when the ask falls outside the collar or below `floor_price_ticks`,
    /// or when the seller's reserved passes fell short at reveal.
    pub valid: bool,
}

// ---------------------------------------------------------------------------
// Commands (client → server)
// ---------------------------------------------------------------------------

/// **[Commit phase]** Submit a hash commitment.
///
/// The commitment is `SHA-256(price_ticks_le8 || qty_le8 || salt32)`.
/// Use [`CommitBid::verify_commitment`] to compute it locally before sending.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct CommitBid {
    /// Idempotency key; re-sending the same key is a no-op.
    pub client_order_id: ClientOrderId,
    /// Target auction.
    pub auction_id: u64,
    /// 32-byte SHA-256 commitment.
    pub commitment: [u8; 32],
}

impl CommitBid {
    /// Canonical pre-image layout for the commitment hash:
    ///
    /// `SHA-256(price_ticks_le8 || qty_le8 || salt32)` — 48 bytes total.
    ///
    /// Compute this in the browser (or via Core) before submitting.
    /// Core re-derives the same bytes during reveal verification.
    pub fn commitment_preimage(price_ticks: Ticks, qty: Qty, salt: &[u8; 32]) -> [u8; 48] {
        let mut buf = [0u8; 48];
        buf[..8].copy_from_slice(&price_ticks.to_le_bytes());
        buf[8..16].copy_from_slice(&qty.to_le_bytes());
        buf[16..].copy_from_slice(salt);
        buf
    }
}

/// **[Reveal phase]** Reveal the pre-image of a previously submitted commitment.
///
/// Core verifies that `SHA-256(price_ticks_le || qty_le || salt) == commitment`
/// stored for `(auction_id, client_order_id)`.  Mismatches mark the bid
/// invalid without error (silent discard preserves front-running resistance).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct RevealBid {
    /// Must match the `client_order_id` used in the original [`CommitBid`].
    pub client_order_id: ClientOrderId,
    /// Target auction.
    pub auction_id: u64,
    /// Claimed price in ticks.
    pub price_ticks: Ticks,
    /// Claimed quantity.
    pub qty: Qty,
    /// The same 32-byte salt used when computing the commitment.
    pub salt: [u8; 32],
}

/// **[Commit phase]** Submit a hash commitment for an ask (seller side).
///
/// Same layout and semantics as [`CommitBid`], for the seller side of the
/// pooled window: the commitment hides the seller's minimum price and
/// quantity until the reveal phase.
///
/// The commitment is `SHA-256(price_ticks_le8 || qty_le8 || salt32)`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct CommitAsk {
    /// Idempotency key; re-sending the same key is a no-op.
    pub client_order_id: ClientOrderId,
    /// Target auction.
    pub auction_id: u64,
    /// 32-byte SHA-256 commitment.
    pub commitment: [u8; 32],
}

/// **[Reveal phase]** Reveal the pre-image of a previously submitted [`CommitAsk`].
///
/// On a matching pre-image the seller's passes are reserved for the revealed
/// quantity; a short inventory silently marks the ask invalid (same rule as
/// an unaffordable bid).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct RevealAsk {
    /// Must match the `client_order_id` used in the original [`CommitAsk`].
    pub client_order_id: ClientOrderId,
    /// Target auction.
    pub auction_id: u64,
    /// Claimed minimum price in ticks.
    pub price_ticks: Ticks,
    /// Claimed quantity.
    pub qty: Qty,
    /// The same 32-byte salt used when computing the commitment.
    pub salt: [u8; 32],
}

/// **[Setup]** Schedule a pooled batch window for one market (admin or issuer).
///
/// No supply exists at creation: sellers enter sealed asks during the commit
/// phase, and the window clears whoever shows up. The commit window opens at
/// the sequencing timestamp and closes after `commit_duration_ms`. The reveal
/// window follows immediately and closes after `reveal_duration_ms`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct CreateBatchAuction {
    /// Idempotency key for this create request.
    pub client_order_id: ClientOrderId,
    /// Market whose passes this window pools.
    pub market_id: MarketId,
    /// Bids and asks below this price are invalid; collar-enforced at creation.
    pub floor_price_ticks: Ticks,
    /// Length of the commit phase in milliseconds.
    pub commit_duration_ms: u64,
    /// Length of the reveal phase in milliseconds (starts when commit ends).
    pub reveal_duration_ms: u64,
}

/// **[Settlement]** Trigger clearing after the reveal window closes.
///
/// Anyone may submit this command once `now >= reveal_end_ms`.  The core
/// performs the uniform-price clearing atomically and transitions the auction
/// to [`BatchAuctionPhase::Settled`] (or [`BatchAuctionPhase::Expired`] if
/// there were no valid reveals).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct SettleBatchAuction {
    /// Idempotency key for this settlement request.
    pub client_order_id: ClientOrderId,
    /// The auction to settle.
    pub auction_id: u64,
}

// ---------------------------------------------------------------------------
// Settlement output
// ---------------------------------------------------------------------------

/// Fill record emitted for each filled (bid, ask) pair after settlement.
///
/// All fills in one settlement happen at the same `price_ticks`
/// (uniform-price rule); the clearing price maximises traded volume.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct BatchAuctionFill {
    /// The auction that produced this fill.
    pub auction_id: u64,
    /// Globally unique trade identifier.
    pub trade_id: TradeId,
    /// Market in which the fill is recorded.
    pub market_id: MarketId,
    /// The winning bidder.
    pub buyer_id: UserId,
    /// The seller whose ask was filled.
    pub seller_id: UserId,
    /// Quantity this pair traded (may be less than either side wanted at the margin).
    pub qty: Qty,
    /// Clearing price paid; identical for all fills in the same settlement.
    pub price_ticks: Ticks,
    /// `qty * price_ticks * tick_size_minor`, in minor units.
    pub notional: i64,
    /// Platform fee in minor units (negative = deducted from buyer).
    pub fee: i64,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preimage_layout_is_stable() {
        let price_ticks: Ticks = 1_500;
        let qty: Qty = 10;
        let salt: [u8; 32] = core::array::from_fn(|i| i as u8);

        let preimage = CommitBid::commitment_preimage(price_ticks, qty, &salt);
        assert_eq!(preimage.len(), 48);
        assert_eq!(&preimage[..8], &price_ticks.to_le_bytes());
        assert_eq!(&preimage[8..16], &qty.to_le_bytes());
        assert_eq!(&preimage[16..], &salt);
    }
}
