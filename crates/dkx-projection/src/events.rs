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
    LedgerPosted { seq: u64, entry: JournalEntry },
    TradeExecuted(TradeExecuted),
}

impl ProjectionEvent {
    pub fn seq(&self) -> u64 {
        match self {
            ProjectionEvent::LedgerPosted { seq, .. } => *seq,
            ProjectionEvent::TradeExecuted(t) => t.fill.seq,
        }
    }
}
