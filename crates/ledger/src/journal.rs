//! Journal keys and entries. Keys are fixed-size and allocation-free.

use serde::{Deserialize, Serialize};

use dx_types::{ClientOrderId, MarketId, TradeId, UserId};

use crate::Posting;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum JournalKey {
    Trade(TradeId),
    Deposit {
        user_id: UserId,
        client_request_id: ClientOrderId,
    },
    Issue {
        user_id: UserId,
        market_id: MarketId,
        client_request_id: ClientOrderId,
    },
    EscrowRelease(TradeId),
    EscrowRefund(TradeId),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalEntry {
    pub id: u64,
    pub key: JournalKey,
    pub postings: Vec<Posting>,
}
