//! Market configuration and integer-only collar arithmetic.

use crate::MarketId;
use crate::{Minor, Qty, Ticks};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MarketStatus {
    Open,
    Closed,
    Auction,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarketConfig {
    pub id: MarketId,
    pub event_id: u32,
    pub name: String,
    /// Face value in minor units. Must be divisible by `tick_size_minor`.
    pub face_value_minor: Minor,
    /// One price tick in minor units.
    pub tick_size_minor: Minor,
    /// Collar floor as basis points of face value (5000 = 50%).
    pub collar_floor_bps: u16,
    /// Collar cap as basis points of face value (12000 = 120%).
    pub collar_cap_bps: u16,
    /// Maximum passes held + open buys per identity.
    pub holding_limit: u32,
    /// Taker fee in basis points (100 = 1%).
    pub taker_fee_bps: u16,
    pub escrow_enabled: bool,
    pub status: MarketStatus,
}

impl MarketConfig {
    pub fn validate(&self) -> Result<(), String> {
        if self.face_value_minor <= 0 {
            return Err("face_value_minor must be positive".into());
        }
        if self.tick_size_minor <= 0 {
            return Err("tick_size_minor must be positive".into());
        }
        if self.face_value_minor % self.tick_size_minor != 0 {
            return Err("face_value_minor must be divisible by tick_size_minor".into());
        }
        if self.collar_floor_bps == 0 {
            return Err("collar_floor_bps must be positive".into());
        }
        if self.collar_cap_bps < self.collar_floor_bps {
            return Err("collar_cap_bps must be >= collar_floor_bps".into());
        }
        if self.holding_limit == 0 {
            return Err("holding_limit must be positive".into());
        }
        let floor = self.floor_ticks();
        let cap = self.cap_ticks();
        if floor == 0 {
            return Err("collar floor must be at least one tick".into());
        }
        if cap <= floor {
            return Err("collar cap must be greater than the floor".into());
        }
        Ok(())
    }

    /// Face value expressed in ticks.
    pub fn face_ticks(&self) -> Ticks {
        (self.face_value_minor as u128 / self.tick_size_minor as u128) as Ticks
    }

    /// Lowest allowed price in ticks, rounded up.
    pub fn floor_ticks(&self) -> Ticks {
        let face = self.face_ticks() as u128;
        (face * self.collar_floor_bps as u128).div_ceil(10_000) as Ticks
    }

    /// Highest allowed price in ticks, rounded down.
    pub fn cap_ticks(&self) -> Ticks {
        let face = self.face_ticks() as u128;
        ((face * self.collar_cap_bps as u128) / 10_000) as Ticks
    }

    pub fn price_in_collar(&self, price_ticks: Ticks) -> bool {
        price_ticks >= self.floor_ticks() && price_ticks <= self.cap_ticks()
    }

    /// Notional in minor units, or `None` on overflow.
    pub fn notional_minor(&self, price_ticks: Ticks, qty: Qty) -> Option<Minor> {
        let unit = (price_ticks as u128).checked_mul(self.tick_size_minor as u128)?;
        let total = unit.checked_mul(qty as u128)?;
        i64::try_from(total).ok()
    }

    /// Taker fee in minor units, rounded up. Fees are always non-negative.
    pub fn taker_fee_minor(&self, notional: Minor) -> Minor {
        if notional <= 0 {
            return 0;
        }
        let fee = (notional as u128 * self.taker_fee_bps as u128).div_ceil(10_000);
        fee as Minor
    }
}

/// Demo markets used by the simulator, tests, and the first UI build.
pub fn seed_markets() -> Vec<MarketConfig> {
    vec![
        MarketConfig {
            id: 1,
            event_id: 1,
            name: "Day 1 General".into(),
            face_value_minor: 500_000, // Rs 5,000
            tick_size_minor: 100,      // Rs 1 per tick
            collar_floor_bps: 5_000,
            collar_cap_bps: 12_000,
            holding_limit: 4,
            taker_fee_bps: 100,
            escrow_enabled: false,
            status: MarketStatus::Open,
        },
        MarketConfig {
            id: 2,
            event_id: 1,
            name: "Day 1 VIP".into(),
            face_value_minor: 1_000_000, // Rs 10,000
            tick_size_minor: 100,
            collar_floor_bps: 5_000,
            collar_cap_bps: 12_000,
            holding_limit: 2,
            taker_fee_bps: 100,
            escrow_enabled: false,
            status: MarketStatus::Open,
        },
        MarketConfig {
            id: 3,
            event_id: 2,
            name: "Relic: Obsidian Sigil".into(),
            face_value_minor: 250_000, // Rs 2,500
            tick_size_minor: 50,       // Rs 0.50 per tick
            collar_floor_bps: 8_000,
            collar_cap_bps: 15_000,
            holding_limit: 5,
            taker_fee_bps: 150,
            escrow_enabled: false,
            status: MarketStatus::Open,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collar_math_is_integer_and_rounded_inward() {
        let market = &seed_markets()[0];
        assert_eq!(market.face_ticks(), 5_000);
        assert_eq!(market.floor_ticks(), 2_500);
        assert_eq!(market.cap_ticks(), 6_000);
        assert!(market.price_in_collar(2_500));
        assert!(market.price_in_collar(6_000));
        assert!(!market.price_in_collar(2_499));
        assert!(!market.price_in_collar(6_001));
    }

    #[test]
    fn notional_and_fee_use_minor_units() {
        let market = &seed_markets()[0];
        assert_eq!(market.notional_minor(5_000, 2), Some(1_000_000));
        assert_eq!(market.taker_fee_minor(1_000_000), 10_000);
        assert_eq!(market.taker_fee_minor(1), 1);
    }

    #[test]
    fn seed_markets_validate() {
        for market in seed_markets() {
            market.validate().unwrap();
        }
    }
}
