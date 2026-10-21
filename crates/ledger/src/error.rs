//! Ledger errors and invariant violations.

use dx_types::{Asset, Minor};

use crate::AccountKey;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LedgerError {
    NonPositiveAmount,
    EmptyJournal,
    InsufficientFunds {
        account: AccountKey,
        needed: Minor,
        available: Minor,
    },
    InsufficientHeld {
        account: AccountKey,
        needed: Minor,
        held: Minor,
    },
    NegativeBalance(AccountKey),
    UnbalancedJournal {
        asset: Asset,
        sum: i128,
    },
    FeePayerNotParty,
    FeeExceedsNotional,
    SelfTrade,
    Overflow,
    InvalidAccount(AccountKey),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InvariantError {
    NegativeBalance(AccountKey),
    MintHeld(AccountKey),
    AssetNotConserved { asset: Asset, sum: i128 },
    UnbalancedJournal { journal_id: u64 },
}
