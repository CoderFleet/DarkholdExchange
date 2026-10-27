use crate::money::types::{Account, Asset, Paise};
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq, Clone)]
pub enum LedgerError {
    #[error("insufficient funds for user {user}: requested {requested}, available {available}")]
    InsufficientFunds {
        user: u64,
        requested: Paise,
        available: Paise,
    },

    #[error(
        "insufficient passes for user {user} in market {market}: requested {requested}, available {available}"
    )]
    InsufficientPasses {
        user: u64,
        market: u32,
        requested: i64,
        available: i64,
    },

    #[error("reservation not found for order {order}")]
    ReservationNotFound { order: u64 },

    #[error("reservation already exists for order {order}")]
    ReservationAlreadyExists { order: u64 },

    #[error("insufficient reservation for order {order}: requested {requested}, locked {locked}")]
    InsufficientReservation {
        order: u64,
        requested: i64,
        locked: i64,
    },

    #[error("escrow mode is not ready until Stage 6")]
    EscrowNotReady,

    #[error("escrow not found: {id}")]
    EscrowNotFound { id: u64 },

    #[error("invalid escrow state transition: escrow {id} in state {current}")]
    InvalidEscrowState { id: u64, current: String },

    #[error("unauthorized action on escrow {id}")]
    UnauthorizedEscrowAction { id: u64 },

    #[error("amount must be positive: {amount}")]
    NonPositiveAmount { amount: i64 },

    #[error("unbalanced journal entry: asset {asset:?} does not sum to zero (net {net})")]
    UnbalancedJournalEntry { asset: Asset, net: i64 },

    #[error(
        "account balance would become negative for non-source account: {account:?} (balance {balance})"
    )]
    NegativeBalanceNotAllowed { account: Account, balance: i64 },

    #[error("arithmetic overflow occurred during ledger operation")]
    ArithmeticOverflow,
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum Violation {
    AssetSumNonZero {
        asset: Asset,
        sum: i64,
    },
    NegativeBalance {
        account: Account,
        balance: i64,
    },
    ReservationMismatch {
        account: Account,
        account_locked: i64,
        sum_reservations: i64,
    },
    EscrowCashMismatch {
        account_balance: i64,
        open_escrows_total: i64,
    },
    PassHeldMismatch {
        account: Account,
        account_held: i64,
        open_escrows_qty: i64,
    },
}
