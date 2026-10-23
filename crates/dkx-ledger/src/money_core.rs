use crate::error::LedgerError;
use crate::ledger::Ledger;
use crate::types::{Fill, IdemKey, MarketId, OrderId, Paise, SettleOutcome, UserId};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    TopUp {
        user: UserId,
        amount: Paise,
        key: IdemKey,
    },
    IssuePass {
        market: MarketId,
        to: UserId,
        qty: i64,
        key: IdemKey,
    },
    ReserveCash {
        user: UserId,
        amount: Paise,
        order: OrderId,
        key: IdemKey,
    },
    ReservePass {
        user: UserId,
        market: MarketId,
        qty: i64,
        order: OrderId,
        key: IdemKey,
    },
    Release {
        order: OrderId,
        key: IdemKey,
    },
    PostFill {
        fill: Fill,
        key: IdemKey,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    TopUpApplied { user: UserId, amount: Paise },
    PassIssued { market: MarketId, to: UserId, qty: i64 },
    CashReserved { user: UserId, amount: Paise, order: OrderId },
    PassReserved { user: UserId, market: MarketId, qty: i64, order: OrderId },
    Released { order: OrderId },
    FillSettled { outcome: SettleOutcome },
}

pub struct MoneyCore {
    pub ledger: Ledger,
}

impl Default for MoneyCore {
    fn default() -> Self {
        Self::new()
    }
}

impl MoneyCore {
    pub fn new() -> Self {
        Self {
            ledger: Ledger::new(),
        }
    }

    pub fn handle(
        &mut self,
        cmd: &Command,
        _seq: u64,
        ts_micros: u64,
    ) -> Result<Vec<Event>, LedgerError> {
        self.ledger.set_current_ts_micros(ts_micros);
        match cmd {
            Command::TopUp { user, amount, key } => {
                self.ledger.topup(*user, *amount, *key)?;
                Ok(vec![Event::TopUpApplied {
                    user: *user,
                    amount: *amount,
                }])
            }
            Command::IssuePass {
                market,
                to,
                qty,
                key,
            } => {
                self.ledger.issue_pass(*market, *to, *qty, *key)?;
                Ok(vec![Event::PassIssued {
                    market: *market,
                    to: *to,
                    qty: *qty,
                }])
            }
            Command::ReserveCash {
                user,
                amount,
                order,
                key,
            } => {
                self.ledger.reserve_cash(*user, *amount, *order, *key)?;
                Ok(vec![Event::CashReserved {
                    user: *user,
                    amount: *amount,
                    order: *order,
                }])
            }
            Command::ReservePass {
                user,
                market,
                qty,
                order,
                key,
            } => {
                self.ledger.reserve_pass(*user, *market, *qty, *order, *key)?;
                Ok(vec![Event::PassReserved {
                    user: *user,
                    market: *market,
                    qty: *qty,
                    order: *order,
                }])
            }
            Command::Release { order, key } => {
                self.ledger.release(*order, *key)?;
                Ok(vec![Event::Released { order: *order }])
            }
            Command::PostFill { fill, key } => {
                let outcome = self.ledger.post_fill(fill, *key)?;
                Ok(vec![Event::FillSettled { outcome }])
            }
        }
    }
}
