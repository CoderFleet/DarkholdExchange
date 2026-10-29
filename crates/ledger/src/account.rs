//! Accounts, postings, and the account key space.

use serde::{Deserialize, Serialize};

use dx_types::{Asset, Minor, UserId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Owner {
    User(UserId),
    /// Issuance account. Allowed to go negative; never holds funds.
    Mint,
    Fees,
    Escrow,
}

impl Owner {
    pub fn is_user(self) -> bool {
        matches!(self, Owner::User(_))
    }

    pub fn allows_negative(self) -> bool {
        matches!(self, Owner::Mint)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AccountKey {
    pub owner: Owner,
    pub asset: Asset,
}

impl AccountKey {
    pub fn user(user_id: UserId, asset: Asset) -> Self {
        Self {
            owner: Owner::User(user_id),
            asset,
        }
    }

    pub fn system(owner: Owner, asset: Asset) -> Self {
        Self { owner, asset }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Account {
    pub available: Minor,
    pub held: Minor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Bucket {
    Available,
    Held,
}

/// One signed leg of a balanced journal entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Posting {
    pub account: AccountKey,
    pub bucket: Bucket,
    pub amount: Minor,
}

impl Posting {
    pub fn new(account: AccountKey, bucket: Bucket, amount: Minor) -> Self {
        Self {
            account,
            bucket,
            amount,
        }
    }
}
