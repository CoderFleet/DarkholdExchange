use std::collections::{HashMap, VecDeque};

use crate::error::{LedgerError, Violation};
use crate::types::{
    Account, Asset, EntryKind, Fill, IdemKey, JournalEntry, JournalId, Leg, MarketId,
    OrderId, Paise, SettleOutcome, UserId,
};

/// 100 basis points = 1.00%
const FEE_BPS: i64 = 100;
const BPS_DENOMINATOR: i64 = 10_000;
const MAX_IDEMPOTENCY_ENTRIES: usize = 50_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reservation {
    Cash { user: UserId, amount: Paise },
    Pass { user: UserId, market: MarketId, qty: i64 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdemResult {
    Journal(JournalId),
    Unit,
    Settle(SettleOutcome),
}

pub struct Ledger {
    balances: HashMap<Account, i64>,
    reservations: HashMap<OrderId, Reservation>,
    journal: Vec<JournalEntry>,
    next_journal_id: JournalId,
    current_ts_micros: u64,
    idem_results: HashMap<IdemKey, IdemResult>,
    idem_order: VecDeque<IdemKey>,
}

impl Default for Ledger {
    fn default() -> Self {
        Self::new()
    }
}

impl Ledger {
    pub fn new() -> Self {
        Self {
            balances: HashMap::new(),
            reservations: HashMap::new(),
            journal: Vec::new(),
            next_journal_id: 1,
            current_ts_micros: 0,
            idem_results: HashMap::new(),
            idem_order: VecDeque::new(),
        }
    }

    pub fn set_current_ts_micros(&mut self, ts: u64) {
        self.current_ts_micros = ts;
    }

    pub fn balance_of(&self, account: Account) -> i64 {
        self.balances.get(&account).copied().unwrap_or(0)
    }

    pub fn liability_of(&self, user: UserId) -> Paise {
        let avail = self.balance_of(Account::CashAvail(user));
        let locked = self.balance_of(Account::CashLocked(user));
        avail.saturating_add(locked)
    }

    fn record_idem(&mut self, key: IdemKey, res: IdemResult) {
        if self.idem_results.contains_key(&key) {
            return;
        }
        if self.idem_order.len() >= MAX_IDEMPOTENCY_ENTRIES {
            if let Some(old) = self.idem_order.pop_front() {
                self.idem_results.remove(&old);
            }
        }
        self.idem_order.push_back(key);
        self.idem_results.insert(key, res);
    }

    fn apply_entry(
        &mut self,
        kind: EntryKind,
        legs: Vec<Leg>,
        key: IdemKey,
    ) -> Result<JournalId, LedgerError> {
        let mut sums: HashMap<Asset, i64> = HashMap::new();
        for leg in &legs {
            let s = sums.entry(leg.asset).or_insert(0);
            *s = s.checked_add(leg.amount).ok_or(LedgerError::ArithmeticOverflow)?;
        }
        for (asset, net) in sums {
            if net != 0 {
                return Err(LedgerError::UnbalancedJournalEntry { asset, net });
            }
        }

        let mut projected: HashMap<Account, i64> = HashMap::new();
        for leg in &legs {
            let current = projected
                .get(&leg.account)
                .copied()
                .unwrap_or_else(|| self.balance_of(leg.account));
            let next = current.checked_add(leg.amount).ok_or(LedgerError::ArithmeticOverflow)?;
            if !leg.account.is_source() && next < 0 {
                return Err(LedgerError::NegativeBalanceNotAllowed {
                    account: leg.account,
                    balance: next,
                });
            }
            projected.insert(leg.account, next);
        }

        for (account, balance) in projected {
            if balance == 0 {
                self.balances.remove(&account);
            } else {
                self.balances.insert(account, balance);
            }
        }

        let id = self.next_journal_id;
        self.next_journal_id = self.next_journal_id.saturating_add(1);

        self.journal.push(JournalEntry {
            id,
            key,
            ts_micros: self.current_ts_micros,
            kind,
            legs,
        });

        Ok(id)
    }

    pub fn topup(&mut self, user: UserId, amount: Paise, key: IdemKey) -> Result<JournalId, LedgerError> {
        if let Some(IdemResult::Journal(id)) = self.idem_results.get(&key) {
            return Ok(*id);
        }
        if amount <= 0 {
            return Err(LedgerError::NonPositiveAmount { amount });
        }

        let legs = vec![
            Leg {
                account: Account::FundingSource,
                asset: Asset::Cash,
                amount: -amount,
            },
            Leg {
                account: Account::CashAvail(user),
                asset: Asset::Cash,
                amount,
            },
        ];

        let id = self.apply_entry(EntryKind::TopUp, legs, key)?;
        self.record_idem(key, IdemResult::Journal(id));
        Ok(id)
    }

    pub fn issue_pass(
        &mut self,
        market: MarketId,
        to: UserId,
        qty: i64,
        key: IdemKey,
    ) -> Result<JournalId, LedgerError> {
        if let Some(IdemResult::Journal(id)) = self.idem_results.get(&key) {
            return Ok(*id);
        }
        if qty <= 0 {
            return Err(LedgerError::NonPositiveAmount { amount: qty });
        }

        let legs = vec![
            Leg {
                account: Account::IssuanceSource(market),
                asset: Asset::Pass(market),
                amount: -qty,
            },
            Leg {
                account: Account::PassAvail(to, market),
                asset: Asset::Pass(market),
                amount: qty,
            },
        ];

        let id = self.apply_entry(EntryKind::IssuePass, legs, key)?;
        self.record_idem(key, IdemResult::Journal(id));
        Ok(id)
    }

    pub fn reserve_cash(
        &mut self,
        user: UserId,
        amount: Paise,
        order: OrderId,
        key: IdemKey,
    ) -> Result<(), LedgerError> {
        if self.idem_results.contains_key(&key) {
            return Ok(());
        }
        if self.reservations.contains_key(&order) {
            return Err(LedgerError::ReservationAlreadyExists { order });
        }
        if amount <= 0 {
            return Err(LedgerError::NonPositiveAmount { amount });
        }

        let avail = self.balance_of(Account::CashAvail(user));
        if avail < amount {
            return Err(LedgerError::InsufficientFunds {
                user,
                requested: amount,
                available: avail,
            });
        }

        let legs = vec![
            Leg {
                account: Account::CashAvail(user),
                asset: Asset::Cash,
                amount: -amount,
            },
            Leg {
                account: Account::CashLocked(user),
                asset: Asset::Cash,
                amount,
            },
        ];

        self.apply_entry(EntryKind::ReserveCash, legs, key)?;
        self.reservations.insert(order, Reservation::Cash { user, amount });
        self.record_idem(key, IdemResult::Unit);
        Ok(())
    }

    pub fn reserve_pass(
        &mut self,
        user: UserId,
        market: MarketId,
        qty: i64,
        order: OrderId,
        key: IdemKey,
    ) -> Result<(), LedgerError> {
        if self.idem_results.contains_key(&key) {
            return Ok(());
        }
        if self.reservations.contains_key(&order) {
            return Err(LedgerError::ReservationAlreadyExists { order });
        }
        if qty <= 0 {
            return Err(LedgerError::NonPositiveAmount { amount: qty });
        }

        let avail = self.balance_of(Account::PassAvail(user, market));
        if avail < qty {
            return Err(LedgerError::InsufficientPasses {
                user,
                market,
                requested: qty,
                available: avail,
            });
        }

        let legs = vec![
            Leg {
                account: Account::PassAvail(user, market),
                asset: Asset::Pass(market),
                amount: -qty,
            },
            Leg {
                account: Account::PassLocked(user, market),
                asset: Asset::Pass(market),
                amount: qty,
            },
        ];

        self.apply_entry(EntryKind::ReservePass, legs, key)?;
        self.reservations.insert(order, Reservation::Pass { user, market, qty });
        self.record_idem(key, IdemResult::Unit);
        Ok(())
    }

    pub fn release(&mut self, order: OrderId, key: IdemKey) -> Result<(), LedgerError> {
        if self.idem_results.contains_key(&key) {
            return Ok(());
        }

        let res = match self.reservations.remove(&order) {
            Some(r) => r,
            None => return Err(LedgerError::ReservationNotFound { order }),
        };

        let legs = match res {
            Reservation::Cash { user, amount } => {
                if amount == 0 {
                    self.record_idem(key, IdemResult::Unit);
                    return Ok(());
                }
                vec![
                    Leg {
                        account: Account::CashLocked(user),
                        asset: Asset::Cash,
                        amount: -amount,
                    },
                    Leg {
                        account: Account::CashAvail(user),
                        asset: Asset::Cash,
                        amount,
                    },
                ]
            }
            Reservation::Pass { user, market, qty } => {
                if qty == 0 {
                    self.record_idem(key, IdemResult::Unit);
                    return Ok(());
                }
                vec![
                    Leg {
                        account: Account::PassLocked(user, market),
                        asset: Asset::Pass(market),
                        amount: -qty,
                    },
                    Leg {
                        account: Account::PassAvail(user, market),
                        asset: Asset::Pass(market),
                        amount: qty,
                    },
                ]
            }
        };

        self.apply_entry(EntryKind::Release, legs, key)?;
        self.record_idem(key, IdemResult::Unit);
        Ok(())
    }

    pub fn post_fill(&mut self, fill: &Fill, key: IdemKey) -> Result<SettleOutcome, LedgerError> {
        if let Some(IdemResult::Settle(outcome)) = self.idem_results.get(&key) {
            return Ok(outcome.clone());
        }

        if fill.escrow {
            return Err(LedgerError::EscrowNotReady);
        }

        if fill.qty <= 0 || fill.price <= 0 {
            return Err(LedgerError::NonPositiveAmount {
                amount: if fill.qty <= 0 { fill.qty } else { fill.price },
            });
        }

        let notional = fill
            .price
            .checked_mul(fill.qty)
            .ok_or(LedgerError::ArithmeticOverflow)?;

        let fee = notional
            .checked_mul(FEE_BPS)
            .ok_or(LedgerError::ArithmeticOverflow)?
            / BPS_DENOMINATOR;
        let seller_proceeds = notional.checked_sub(fee).ok_or(LedgerError::ArithmeticOverflow)?;

        let buyer_res = match self.reservations.get(&fill.buyer_order) {
            Some(Reservation::Cash { user, amount }) => {
                if *user != fill.buyer {
                    return Err(LedgerError::ReservationNotFound { order: fill.buyer_order });
                }
                *amount
            }
            _ => return Err(LedgerError::ReservationNotFound { order: fill.buyer_order }),
        };

        if buyer_res < notional {
            return Err(LedgerError::InsufficientReservation {
                order: fill.buyer_order,
                requested: notional,
                locked: buyer_res,
            });
        }

        let seller_res = match self.reservations.get(&fill.seller_order) {
            Some(Reservation::Pass { user, market, qty }) => {
                if *user != fill.seller || *market != fill.market {
                    return Err(LedgerError::ReservationNotFound { order: fill.seller_order });
                }
                *qty
            }
            _ => return Err(LedgerError::ReservationNotFound { order: fill.seller_order }),
        };

        if seller_res < fill.qty {
            return Err(LedgerError::InsufficientReservation {
                order: fill.seller_order,
                requested: fill.qty,
                locked: seller_res,
            });
        }

        let legs = vec![
            Leg {
                account: Account::CashLocked(fill.buyer),
                asset: Asset::Cash,
                amount: -notional,
            },
            Leg {
                account: Account::CashAvail(fill.seller),
                asset: Asset::Cash,
                amount: seller_proceeds,
            },
            Leg {
                account: Account::PlatformFee,
                asset: Asset::Cash,
                amount: fee,
            },
            Leg {
                account: Account::PassLocked(fill.seller, fill.market),
                asset: Asset::Pass(fill.market),
                amount: -fill.qty,
            },
            Leg {
                account: Account::PassAvail(fill.buyer, fill.market),
                asset: Asset::Pass(fill.market),
                amount: fill.qty,
            },
        ];

        self.apply_entry(EntryKind::Fill, legs, key)?;

        let rem_buyer = buyer_res - notional;
        if rem_buyer == 0 {
            self.reservations.remove(&fill.buyer_order);
        } else {
            self.reservations.insert(
                fill.buyer_order,
                Reservation::Cash {
                    user: fill.buyer,
                    amount: rem_buyer,
                },
            );
        }

        let rem_seller = seller_res - fill.qty;
        if rem_seller == 0 {
            self.reservations.remove(&fill.seller_order);
        } else {
            self.reservations.insert(
                fill.seller_order,
                Reservation::Pass {
                    user: fill.seller,
                    market: fill.market,
                    qty: rem_seller,
                },
            );
        }

        let outcome = SettleOutcome::Settled;
        self.record_idem(key, IdemResult::Settle(outcome.clone()));
        Ok(outcome)
    }

    pub fn check_invariants(&self) -> Vec<Violation> {
        let mut violations = Vec::new();

        let mut asset_totals: HashMap<Asset, i64> = HashMap::new();
        for (account, &bal) in &self.balances {
            let asset = match account {
                Account::CashAvail(_)
                | Account::CashLocked(_)
                | Account::EscrowCash
                | Account::PlatformFee
                | Account::FundingSource => Asset::Cash,
                Account::PassAvail(_, m)
                | Account::PassLocked(_, m)
                | Account::PassHeld(_, m)
                | Account::IssuanceSource(m) => Asset::Pass(*m),
            };
            let entry = asset_totals.entry(asset).or_insert(0);
            *entry = entry.saturating_add(bal);

            if !account.is_source() && bal < 0 {
                violations.push(Violation::NegativeBalance {
                    account: *account,
                    balance: bal,
                });
            }
        }

        for (asset, sum) in asset_totals {
            if sum != 0 {
                violations.push(Violation::AssetSumNonZero { asset, sum });
            }
        }

        let mut expected_cash_locked: HashMap<UserId, i64> = HashMap::new();
        let mut expected_pass_locked: HashMap<(UserId, MarketId), i64> = HashMap::new();

        for res in self.reservations.values() {
            match res {
                Reservation::Cash { user, amount } => {
                    let e = expected_cash_locked.entry(*user).or_insert(0);
                    *e = e.saturating_add(*amount);
                }
                Reservation::Pass { user, market, qty } => {
                    let e = expected_pass_locked.entry((*user, *market)).or_insert(0);
                    *e = e.saturating_add(*qty);
                }
            }
        }

        for (&user, &exp) in &expected_cash_locked {
            let actual = self.balance_of(Account::CashLocked(user));
            if actual != exp {
                violations.push(Violation::ReservationMismatch {
                    account: Account::CashLocked(user),
                    account_locked: actual,
                    sum_reservations: exp,
                });
            }
        }
        for (account, &actual) in &self.balances {
            if let Account::CashLocked(user) = account {
                if !expected_cash_locked.contains_key(user) && actual != 0 {
                    violations.push(Violation::ReservationMismatch {
                        account: *account,
                        account_locked: actual,
                        sum_reservations: 0,
                    });
                }
            }
        }

        for (&(user, market), &exp) in &expected_pass_locked {
            let actual = self.balance_of(Account::PassLocked(user, market));
            if actual != exp {
                violations.push(Violation::ReservationMismatch {
                    account: Account::PassLocked(user, market),
                    account_locked: actual,
                    sum_reservations: exp,
                });
            }
        }
        for (account, &actual) in &self.balances {
            if let Account::PassLocked(user, market) = account {
                if !expected_pass_locked.contains_key(&(*user, *market)) && actual != 0 {
                    violations.push(Violation::ReservationMismatch {
                        account: *account,
                        account_locked: actual,
                        sum_reservations: 0,
                    });
                }
            }
        }

        violations
    }
}
