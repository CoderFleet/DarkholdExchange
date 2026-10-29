use dkx_escrow::{DisputeRecord, EscrowRecord};
use dx_ledger::money::{Fill, JournalEntry, Paise};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TradeExecuted {
    pub id: u64,
    pub fill: Fill,
    pub fee: Paise,
    pub escrow_id: Option<u64>,
    /// Stream sequence from the publisher (the projection watermark), not the
    /// engine's command sequence which lives in `fill.seq`.
    pub seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PassBalance {
    pub market_id: u32,
    pub available: i64,
    pub held: i64,
}

/// Absolute per-user balance snapshot. Real ledger reservations are not
/// journal posts (`Ledger::reserve` mutates the account without a journal
/// key), so incremental leg sums cannot rebuild `held`; the writer therefore
/// re-states the authoritative numbers for every user a job touched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BalanceSnapshot {
    pub cash_available: i64,
    pub cash_held: i64,
    pub passes: Vec<PassBalance>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProjectionEvent {
    LedgerPosted {
        seq: u64,
        entry: JournalEntry,
    },
    TradeExecuted(TradeExecuted),
    EscrowUpserted {
        seq: u64,
        escrow: EscrowRecord,
    },
    DisputeOpened {
        seq: u64,
        dispute: DisputeRecord,
    },
    DisputeResolved {
        seq: u64,
        escrow_id: u64,
        ruling: String,
        ruled_by: u64,
        ruled_at: u64,
        note: String,
    },
    BalancesSet {
        seq: u64,
        user_id: u64,
        snapshot: BalanceSnapshot,
    },
}

impl ProjectionEvent {
    pub fn seq(&self) -> u64 {
        match self {
            ProjectionEvent::LedgerPosted { seq, .. } => *seq,
            ProjectionEvent::TradeExecuted(t) => t.seq,
            ProjectionEvent::EscrowUpserted { seq, .. } => *seq,
            ProjectionEvent::DisputeOpened { seq, .. } => *seq,
            ProjectionEvent::DisputeResolved { seq, .. } => *seq,
            ProjectionEvent::BalancesSet { seq, .. } => *seq,
        }
    }
}
