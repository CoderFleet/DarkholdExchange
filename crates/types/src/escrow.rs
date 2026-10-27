//! Escrow records and delivery receipts for markets with `escrow_enabled`.

use crate::{MarketId, Minor, Qty, TradeId, UserId};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// A trade that is waiting for the seller's signed delivery receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum EscrowState {
    PendingDelivery,
    Delivered,
    Refunded,
}

/// Buyer cash and seller passes held until delivery is proven (or the deadline passes).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct EscrowRecord {
    pub trade_id: TradeId,
    pub market_id: MarketId,
    pub buyer_id: UserId,
    pub seller_id: UserId,
    pub qty: Qty,
    pub notional: Minor,
    pub fee: Minor,
    pub fee_payer: UserId,
    pub state: EscrowState,
    pub created_at_ms: u64,
    pub deadline_ms: u64,
    pub delivered_at_ms: Option<u64>,
    /// Public key captured when the fill happened, so later key rotation cannot
    /// change how an existing escrow is released.
    pub seller_public_key: [u8; 32],
}

/// The exact bytes a seller signs for a delivery receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct DeliveryReceipt {
    pub trade_id: TradeId,
    pub market_id: MarketId,
    pub qty: Qty,
    pub buyer_id: UserId,
    pub seller_id: UserId,
    pub delivered_at_ms: u64,
}

impl DeliveryReceipt {
    pub const DOMAIN: &'static [u8] = b"darkhold-escrow-receipt-v1";

    /// Canonical signed message. Every integer is unsigned big-endian so the
    /// Rust core and the browser signer produce identical bytes.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(Self::DOMAIN.len() + 6 * 8);
        out.extend_from_slice(Self::DOMAIN);
        out.extend_from_slice(&self.trade_id.to_be_bytes());
        out.extend_from_slice(&u64::from(self.market_id).to_be_bytes());
        out.extend_from_slice(&self.qty.to_be_bytes());
        out.extend_from_slice(&self.buyer_id.to_be_bytes());
        out.extend_from_slice(&self.seller_id.to_be_bytes());
        out.extend_from_slice(&self.delivered_at_ms.to_be_bytes());
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receipt_bytes_are_stable() {
        let receipt = DeliveryReceipt {
            trade_id: 1,
            market_id: 3,
            qty: 2,
            buyer_id: 10,
            seller_id: 20,
            delivered_at_ms: 30,
        };
        let mut expected = DeliveryReceipt::DOMAIN.to_vec();
        for value in [1u64, 3, 2, 10, 20, 30] {
            expected.extend_from_slice(&value.to_be_bytes());
        }
        assert_eq!(receipt.canonical_bytes(), expected);
    }
}
