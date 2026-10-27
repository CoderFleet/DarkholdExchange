//! Dutch-auction state and the integer price curve.
//!
//! A Dutch auction liquidates a fixed quantity of one market's passes at a price
//! that decays linearly from `start_price_ticks` to `floor_price_ticks` between
//! `start_ms` and `end_ms`. The price is a pure function of time, so the client can
//! draw the countdown locally and the core recomputes the same value when the
//! accept is sequenced.

use crate::{ClientOrderId, MarketId, Qty, Ticks, TradeId, UserId};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum AuctionStatus {
    Active,
    Sold,
    Expired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct DutchAuction {
    pub id: u64,
    pub market_id: MarketId,
    pub seller_id: UserId,
    pub qty_total: Qty,
    pub qty_remaining: Qty,
    pub start_price_ticks: Ticks,
    pub floor_price_ticks: Ticks,
    pub start_ms: u64,
    pub end_ms: u64,
    pub status: AuctionStatus,
}

impl DutchAuction {
    /// Price in ticks at `now_ms`, rounded down, clamped to the published range.
    ///
    /// Before the start the auction is at its start price; at or after the end it
    /// is at its floor. The curve never leaves the collar because creation
    /// validates both endpoints.
    pub fn price_at(&self, now_ms: u64) -> Ticks {
        if now_ms >= self.end_ms {
            return self.floor_price_ticks;
        }
        if now_ms <= self.start_ms {
            return self.start_price_ticks;
        }
        let duration = self.end_ms.saturating_sub(self.start_ms) as u128;
        if duration == 0 {
            return self.floor_price_ticks;
        }
        let elapsed = (now_ms - self.start_ms) as u128;
        let span = self
            .start_price_ticks
            .saturating_sub(self.floor_price_ticks) as u128;
        let drop = span.saturating_mul(elapsed) / duration;
        (self.start_price_ticks as u128)
            .saturating_sub(drop)
            .min(u128::from(u64::MAX)) as Ticks
    }
}

/// Create a seller-backed liquidation auction. The seller's passes are reserved
/// until the auction is accepted or expired.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct CreateDutchAuction {
    /// Idempotency key for this create request.
    pub client_order_id: ClientOrderId,
    pub market_id: MarketId,
    pub qty: Qty,
    pub start_price_ticks: Ticks,
    pub floor_price_ticks: Ticks,
    pub duration_ms: u64,
}

/// Take `qty` from an auction at the current price, provided it is at most
/// `max_price_ticks`. Compare-and-set on the remaining quantity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct AcceptDutchAuction {
    /// Idempotency key for this accept request.
    pub client_order_id: ClientOrderId,
    pub auction_id: u64,
    pub qty: Qty,
    pub max_price_ticks: Ticks,
}

/// Result of one accepted auction slice, after the ledger has settled it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AuctionAccept {
    pub auction_id: u64,
    pub trade_id: TradeId,
    pub market_id: MarketId,
    pub buyer_id: UserId,
    pub seller_id: UserId,
    pub qty: Qty,
    pub price_ticks: Ticks,
    pub notional: i64,
    pub fee: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn auction() -> DutchAuction {
        DutchAuction {
            id: 1,
            market_id: 3,
            seller_id: 2,
            qty_total: 10,
            qty_remaining: 10,
            start_price_ticks: 5_000,
            floor_price_ticks: 2_500,
            start_ms: 1_000,
            end_ms: 11_000,
            status: AuctionStatus::Active,
        }
    }

    #[test]
    fn price_decays_linearly_and_clamps_outside_the_window() {
        let auction = auction();
        assert_eq!(auction.price_at(0), 5_000);
        assert_eq!(auction.price_at(1_000), 5_000);
        assert_eq!(auction.price_at(6_000), 3_750);
        assert_eq!(auction.price_at(10_999), 2_501);
        assert_eq!(auction.price_at(11_000), 2_500);
        assert_eq!(auction.price_at(20_000), 2_500);
    }

    #[test]
    fn price_never_leaves_the_published_range() {
        let auction = auction();
        let mut previous = auction.start_price_ticks;
        for now in auction.start_ms..=auction.end_ms {
            let price = auction.price_at(now);
            assert!(price <= previous, "price rose at {now}");
            assert!(price >= auction.floor_price_ticks);
            assert!(price <= auction.start_price_ticks);
            previous = price;
        }
    }

    #[test]
    fn zero_duration_falls_straight_to_the_floor() {
        let mut auction = auction();
        auction.end_ms = auction.start_ms;
        assert_eq!(
            auction.price_at(auction.start_ms),
            auction.floor_price_ticks
        );
    }
}
