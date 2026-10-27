use dkx_escrow::{DisputeRecord, EscrowRecord};
use dx_ledger::money::{Fill, JournalEntry, Paise};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TradeExecuted {
    pub id: u64,
    pub fill: Fill,
    pub fee: Paise,
    pub escrow_id: Option<u64>,
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
}

impl ProjectionEvent {
    pub fn seq(&self) -> u64 {
        match self {
            ProjectionEvent::LedgerPosted { seq, .. } => *seq,
            ProjectionEvent::TradeExecuted(t) => t.fill.seq,
            ProjectionEvent::EscrowUpserted { seq, .. } => *seq,
            ProjectionEvent::DisputeOpened { seq, .. } => *seq,
            ProjectionEvent::DisputeResolved { seq, .. } => *seq,
        }
    }
}
