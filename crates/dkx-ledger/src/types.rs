use serde::{Deserialize, Serialize};

pub type UserId = u64;
pub type MarketId = u32;
pub type OrderId = u64;
pub type EscrowId = u64;
pub type Paise = i64;
pub type JournalId = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct IdemKey(pub u128);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Asset {
    Cash,
    Pass(MarketId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Account {
    CashAvail(UserId),
    CashLocked(UserId),
    PassAvail(UserId, MarketId),
    PassLocked(UserId, MarketId),
    PassHeld(UserId, MarketId),
    EscrowCash,
    PlatformFee,
    FundingSource,
    IssuanceSource(MarketId),
}

impl Account {
    pub fn is_source(&self) -> bool {
        matches!(self, Account::FundingSource | Account::IssuanceSource(_))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Leg {
    pub account: Account,
    pub asset: Asset,
    pub amount: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntryKind {
    TopUp,
    IssuePass,
    ReserveCash,
    ReservePass,
    Release,
    Fill,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalEntry {
    pub id: JournalId,
    pub key: IdemKey,
    pub ts_micros: u64,
    pub kind: EntryKind,
    pub legs: Vec<Leg>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fill {
    pub market: MarketId,
    pub buyer: UserId,
    pub seller: UserId,
    pub buyer_order: OrderId,
    pub seller_order: OrderId,
    pub price: Paise,
    pub qty: i64,
    pub escrow: bool,
    pub seq: u64,
    pub ts_micros: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SettleOutcome {
    Settled,
    Escrowed(EscrowId),
}
