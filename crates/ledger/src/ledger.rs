//! Double-entry ledger with reservations.
//!
//! Every settlement is one balanced journal entry. Reservations move funds between
//! `available` and `held` inside an account; they are tied to open orders by the core.

use std::collections::HashMap;

use dx_types::{Asset, Balance, ClientOrderId, MarketId, Minor, Qty, TradeId, UserId};

use crate::{
    Account, AccountKey, Bucket, InvariantError, JournalEntry, JournalKey, LedgerError, Owner,
    Posting,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TradeSettlement {
    pub trade_id: TradeId,
    pub market_id: MarketId,
    pub buyer_id: UserId,
    pub seller_id: UserId,
    pub qty: Qty,
    pub notional: Minor,
    /// Platform fee, paid by exactly one side.
    pub fee: Minor,
    pub fee_payer: UserId,
    /// When true, funds enter escrow rather than settling immediately.
    pub escrow: bool,
}

pub struct Ledger {
    accounts: HashMap<AccountKey, Account>,
    journals: Vec<JournalEntry>,
    key_index: HashMap<JournalKey, u64>,
    next_journal_id: u64,
}

impl Default for Ledger {
    fn default() -> Self {
        Self::new()
    }
}

impl Ledger {
    pub fn new() -> Self {
        Self {
            accounts: HashMap::new(),
            journals: Vec::new(),
            key_index: HashMap::new(),
            next_journal_id: 1,
        }
    }

    /// Post one balanced journal entry. Duplicate `key` is an idempotent success.
    pub fn post(&mut self, key: JournalKey, postings: Vec<Posting>) -> Result<u64, LedgerError> {
        if let Some(id) = self.key_index.get(&key) {
            return Ok(*id);
        }

        let postings: Vec<Posting> = postings
            .into_iter()
            .filter(|posting| posting.amount != 0)
            .collect();
        if postings.is_empty() {
            return Err(LedgerError::EmptyJournal);
        }

        let mut totals: HashMap<Asset, i128> = HashMap::new();
        for posting in &postings {
            *totals.entry(posting.account.asset).or_default() += posting.amount as i128;
        }
        for (asset, sum) in totals {
            if sum != 0 {
                return Err(LedgerError::UnbalancedJournal { asset, sum });
            }
        }

        // Validate the complete result before mutating any account.
        let mut deltas: HashMap<AccountKey, (i128, i128)> = HashMap::new();
        for posting in &postings {
            let (available, held) = deltas.entry(posting.account).or_default();
            match posting.bucket {
                Bucket::Available => *available += posting.amount as i128,
                Bucket::Held => *held += posting.amount as i128,
            }
        }

        for (key, (available_delta, held_delta)) in &deltas {
            let account = self.accounts.get(key).copied().unwrap_or_default();
            let available = account.available as i128 + available_delta;
            let held = account.held as i128 + held_delta;

            if held < 0 {
                return Err(LedgerError::InsufficientHeld {
                    account: *key,
                    needed: (-held) as Minor,
                    held: account.held,
                });
            }
            if key.owner == Owner::Mint && held != 0 {
                return Err(LedgerError::InvalidAccount(*key));
            }
            if available < 0 && !key.owner.allows_negative() {
                return Err(if key.owner.is_user() {
                    LedgerError::InsufficientFunds {
                        account: *key,
                        needed: (-available) as Minor,
                        available: account.available,
                    }
                } else {
                    LedgerError::NegativeBalance(*key)
                });
            }
            if available < i64::MIN as i128
                || available > i64::MAX as i128
                || held > i64::MAX as i128
            {
                return Err(LedgerError::Overflow);
            }
        }

        for (key, (available_delta, held_delta)) in deltas {
            let account = self.accounts.entry(key).or_default();
            account.available = (account.available as i128 + available_delta) as Minor;
            account.held = (account.held as i128 + held_delta) as Minor;
        }

        let id = self.next_journal_id;
        self.next_journal_id += 1;
        self.key_index.insert(key, id);
        self.journals.push(JournalEntry { id, key, postings });
        Ok(id)
    }

    /// Simulated top-up. The mint account goes negative; the user gets spendable cash.
    pub fn deposit_cash(
        &mut self,
        user_id: UserId,
        amount: Minor,
        client_request_id: ClientOrderId,
    ) -> Result<u64, LedgerError> {
        if amount <= 0 {
            return Err(LedgerError::NonPositiveAmount);
        }
        let key = JournalKey::Deposit {
            user_id,
            client_request_id,
        };
        let postings = vec![
            Posting::new(
                AccountKey::system(Owner::Mint, Asset::Cash),
                Bucket::Available,
                -amount,
            ),
            Posting::new(
                AccountKey::user(user_id, Asset::Cash),
                Bucket::Available,
                amount,
            ),
        ];
        self.post(key, postings)
    }

    /// Issue passes into a user's account. Total pass supply stays constant.
    pub fn issue_passes(
        &mut self,
        user_id: UserId,
        market_id: MarketId,
        qty: Qty,
        client_request_id: ClientOrderId,
    ) -> Result<u64, LedgerError> {
        let qty = i64::try_from(qty).map_err(|_| LedgerError::Overflow)?;
        if qty <= 0 {
            return Err(LedgerError::NonPositiveAmount);
        }
        let asset = Asset::Pass { market_id };
        let key = JournalKey::Issue {
            user_id,
            market_id,
            client_request_id,
        };
        let postings = vec![
            Posting::new(
                AccountKey::system(Owner::Mint, asset),
                Bucket::Available,
                -qty,
            ),
            Posting::new(AccountKey::user(user_id, asset), Bucket::Available, qty),
        ];
        self.post(key, postings)
    }

    pub fn reserve(
        &mut self,
        user_id: UserId,
        asset: Asset,
        amount: Minor,
    ) -> Result<(), LedgerError> {
        if amount <= 0 {
            return Err(LedgerError::NonPositiveAmount);
        }
        let key = AccountKey::user(user_id, asset);
        let account = self.accounts.entry(key).or_default();
        if account.available < amount {
            return Err(LedgerError::InsufficientFunds {
                account: key,
                needed: amount,
                available: account.available,
            });
        }
        let held = account
            .held
            .checked_add(amount)
            .ok_or(LedgerError::Overflow)?;
        account.available -= amount;
        account.held = held;
        Ok(())
    }

    pub fn release(
        &mut self,
        user_id: UserId,
        asset: Asset,
        amount: Minor,
    ) -> Result<(), LedgerError> {
        if amount < 0 {
            return Err(LedgerError::NonPositiveAmount);
        }
        if amount == 0 {
            return Ok(());
        }
        let key = AccountKey::user(user_id, asset);
        let account = self.accounts.entry(key).or_default();
        if account.held < amount {
            return Err(LedgerError::InsufficientHeld {
                account: key,
                needed: amount,
                held: account.held,
            });
        }
        let available = account
            .available
            .checked_add(amount)
            .ok_or(LedgerError::Overflow)?;
        account.held -= amount;
        account.available = available;
        Ok(())
    }

    /// Settle one fill atomically. The buyer's cash and seller's passes must already
    /// be reserved; this entry moves them to the counterparties and pays the fee.
    /// Returns the journal id. If `settlement.escrow` is true, funds are routed
    /// to the escrow account instead of settling immediately.
    pub fn settle_trade(&mut self, settlement: TradeSettlement) -> Result<u64, LedgerError> {
        let (buyer_cash, seller_cash, qty) = Self::trade_amounts(&settlement)?;

        let cash = Asset::Cash;
        let pass = Asset::Pass {
            market_id: settlement.market_id,
        };

        if settlement.escrow {
            // Escrow path: buyer cash → escrow account; seller passes → buyer held
            let postings = vec![
                Posting::new(
                    AccountKey::user(settlement.buyer_id, cash),
                    Bucket::Held,
                    -buyer_cash,
                ),
                Posting::new(
                    AccountKey::system(Owner::Escrow, cash),
                    Bucket::Available,
                    buyer_cash,
                ),
                Posting::new(
                    AccountKey::user(settlement.seller_id, pass),
                    Bucket::Held,
                    -qty,
                ),
                // Pass goes to buyer as held (not tradable until escrow resolves)
                Posting::new(
                    AccountKey::user(settlement.buyer_id, pass),
                    Bucket::Held,
                    qty,
                ),
            ];
            self.post(JournalKey::Trade(settlement.trade_id), postings)
        } else {
            // Spot path: immediate settlement
            let mut postings = Vec::with_capacity(5);
            postings.push(Posting::new(
                AccountKey::user(settlement.buyer_id, cash),
                Bucket::Held,
                -buyer_cash,
            ));
            postings.push(Posting::new(
                AccountKey::user(settlement.seller_id, cash),
                Bucket::Available,
                seller_cash,
            ));
            if settlement.fee > 0 {
                postings.push(Posting::new(
                    AccountKey::system(Owner::Fees, cash),
                    Bucket::Available,
                    settlement.fee,
                ));
            }
            postings.push(Posting::new(
                AccountKey::user(settlement.seller_id, pass),
                Bucket::Held,
                -qty,
            ));
            postings.push(Posting::new(
                AccountKey::user(settlement.buyer_id, pass),
                Bucket::Available,
                qty,
            ));
            self.post(JournalKey::Trade(settlement.trade_id), postings)
        }
    }

    /// Move a fill's cash and passes into the shared escrow accounts. The buyer's cash
    /// and seller's passes must already be reserved. No fee is paid until release.
    pub fn hold_escrow(&mut self, settlement: TradeSettlement) -> Result<u64, LedgerError> {
        let (buyer_cash, _seller_cash, qty) = Self::trade_amounts(&settlement)?;
        let cash = Asset::Cash;
        let pass = Asset::Pass {
            market_id: settlement.market_id,
        };
        let postings = vec![
            Posting::new(
                AccountKey::user(settlement.buyer_id, cash),
                Bucket::Held,
                -buyer_cash,
            ),
            Posting::new(
                AccountKey::system(Owner::Escrow, cash),
                Bucket::Available,
                buyer_cash,
            ),
            Posting::new(
                AccountKey::user(settlement.seller_id, pass),
                Bucket::Held,
                -qty,
            ),
            Posting::new(
                AccountKey::system(Owner::Escrow, pass),
                Bucket::Available,
                qty,
            ),
        ];
        self.post(JournalKey::Trade(settlement.trade_id), postings)
    }

    /// Release an escrow hold after a valid delivery receipt: pay the seller, transfer
    /// the pass to the buyer, and pay the platform fee.
    pub fn release_escrow(&mut self, settlement: TradeSettlement) -> Result<u64, LedgerError> {
        let (buyer_cash, seller_cash, qty) = Self::trade_amounts(&settlement)?;
        let cash = Asset::Cash;
        let pass = Asset::Pass {
            market_id: settlement.market_id,
        };
        let mut postings = Vec::with_capacity(5);
        postings.push(Posting::new(
            AccountKey::system(Owner::Escrow, cash),
            Bucket::Available,
            -buyer_cash,
        ));
        postings.push(Posting::new(
            AccountKey::user(settlement.seller_id, cash),
            Bucket::Available,
            seller_cash,
        ));
        if settlement.fee > 0 {
            postings.push(Posting::new(
                AccountKey::system(Owner::Fees, cash),
                Bucket::Available,
                settlement.fee,
            ));
        }
        postings.push(Posting::new(
            AccountKey::system(Owner::Escrow, pass),
            Bucket::Available,
            -qty,
        ));
        postings.push(Posting::new(
            AccountKey::user(settlement.buyer_id, pass),
            Bucket::Available,
            qty,
        ));
        self.post(JournalKey::EscrowRelease(settlement.trade_id), postings)
    }

    /// Refund an escrow hold after its deadline. The buyer gets the cash back and
    /// the seller gets the passes back.
    pub fn refund_escrow(&mut self, settlement: TradeSettlement) -> Result<u64, LedgerError> {
        let (buyer_cash, _seller_cash, qty) = Self::trade_amounts(&settlement)?;
        let cash = Asset::Cash;
        let pass = Asset::Pass {
            market_id: settlement.market_id,
        };
        let postings = vec![
            Posting::new(
                AccountKey::system(Owner::Escrow, cash),
                Bucket::Available,
                -buyer_cash,
            ),
            Posting::new(
                AccountKey::user(settlement.buyer_id, cash),
                Bucket::Available,
                buyer_cash,
            ),
            Posting::new(
                AccountKey::system(Owner::Escrow, pass),
                Bucket::Available,
                -qty,
            ),
            Posting::new(
                AccountKey::user(settlement.seller_id, pass),
                Bucket::Available,
                qty,
            ),
        ];
        self.post(JournalKey::EscrowRefund(settlement.trade_id), postings)
    }

    fn trade_amounts(settlement: &TradeSettlement) -> Result<(Minor, Minor, i64), LedgerError> {
        if settlement.buyer_id == settlement.seller_id {
            return Err(LedgerError::SelfTrade);
        }
        if settlement.qty == 0 || settlement.notional <= 0 || settlement.fee < 0 {
            return Err(LedgerError::NonPositiveAmount);
        }

        let (buyer_fee, seller_fee) = if settlement.fee_payer == settlement.buyer_id {
            (settlement.fee, 0)
        } else if settlement.fee_payer == settlement.seller_id {
            (0, settlement.fee)
        } else {
            return Err(LedgerError::FeePayerNotParty);
        };

        let buyer_cash = settlement
            .notional
            .checked_add(buyer_fee)
            .ok_or(LedgerError::Overflow)?;
        let seller_cash = settlement
            .notional
            .checked_sub(seller_fee)
            .ok_or(LedgerError::FeeExceedsNotional)?;
        if seller_cash < 0 {
            return Err(LedgerError::FeeExceedsNotional);
        }
        let qty = i64::try_from(settlement.qty).map_err(|_| LedgerError::Overflow)?;
        Ok((buyer_cash, seller_cash, qty))
    }

    pub fn balance(&self, user_id: UserId, asset: Asset) -> Balance {
        let account = self
            .accounts
            .get(&AccountKey::user(user_id, asset))
            .copied()
            .unwrap_or_default();
        Balance {
            asset,
            available: account.available,
            held: account.held,
        }
    }

    pub fn system_balance(&self, owner: Owner, asset: Asset) -> Minor {
        let account = self
            .accounts
            .get(&AccountKey::system(owner, asset))
            .copied()
            .unwrap_or_default();
        account.available.saturating_add(account.held)
    }

    pub fn balances(&self, user_id: UserId) -> Vec<Balance> {
        let mut balances: Vec<_> = self
            .accounts
            .iter()
            .filter(|(key, _)| key.owner == Owner::User(user_id))
            .map(|(key, account)| Balance {
                asset: key.asset,
                available: account.available,
                held: account.held,
            })
            .collect();
        balances.sort_by_key(|balance| match balance.asset {
            Asset::Cash => (0u8, 0u32),
            Asset::Pass { market_id } => (1u8, market_id),
        });
        balances
    }

    pub fn total_user_held(&self, asset: Asset) -> Minor {
        self.accounts
            .iter()
            .filter(|(key, _)| key.owner.is_user() && key.asset == asset)
            .fold(0, |total, (_, account)| total.saturating_add(account.held))
    }

    pub fn accounts(&self) -> impl Iterator<Item = (&AccountKey, &Account)> {
        self.accounts.iter()
    }

    /// Deterministic account ordering for state hashing and snapshots.
    pub fn sorted_accounts(&self) -> Vec<(AccountKey, Account)> {
        let mut accounts: Vec<_> = self
            .accounts
            .iter()
            .map(|(key, value)| (*key, *value))
            .collect();
        accounts.sort_by_key(|(key, _)| *key);
        accounts
    }

    pub fn journal_count(&self) -> usize {
        self.journals.len()
    }

    pub fn journal(&self, key: JournalKey) -> Option<&JournalEntry> {
        self.key_index
            .get(&key)
            .and_then(|id| self.journals.get((*id - 1) as usize))
    }

    pub fn check_invariants(&self) -> Result<(), InvariantError> {
        for (key, account) in &self.accounts {
            if (key.owner.is_user() || matches!(key.owner, Owner::Fees | Owner::Escrow))
                && (account.available < 0 || account.held < 0)
            {
                return Err(InvariantError::NegativeBalance(*key));
            }
            if key.owner == Owner::Mint && account.held != 0 {
                return Err(InvariantError::MintHeld(*key));
            }
        }

        let mut totals: HashMap<Asset, i128> = HashMap::new();
        for (key, account) in &self.accounts {
            *totals.entry(key.asset).or_default() +=
                account.available as i128 + account.held as i128;
        }
        for (asset, sum) in totals {
            if sum != 0 {
                return Err(InvariantError::AssetNotConserved { asset, sum });
            }
        }

        for entry in &self.journals {
            let mut sums: HashMap<Asset, i128> = HashMap::new();
            for posting in &entry.postings {
                *sums.entry(posting.account.asset).or_default() += posting.amount as i128;
            }
            if sums.values().any(|sum| *sum != 0) {
                return Err(InvariantError::UnbalancedJournal {
                    journal_id: entry.id,
                });
            }
        }
        Ok(())
    }
}
