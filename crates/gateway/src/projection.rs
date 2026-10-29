//! Row 21: PostgreSQL projection wiring. The writer thread converts engine
//! state changes into `dkx_projection::ProjectionEvent`s on an unbounded
//! channel; a tokio worker applies them to Postgres (`ProjectionWorker`).
//!
//! Natural keys keep re-delivery safe: journals dedupe on their entry id
//! (`ledger_entries.idem_key`), trades on the trade id, escrows upsert by
//! current state. So the catch-up flush that re-reads all journals after a
//! restart never double-counts money.
//!
//! The gateway's HTTP routes keep serving from the in-memory core; these
//! tables are the persistent replica.

use std::collections::HashMap;

use dkx_escrow::{EscrowRecord as DkxEscrowRecord, EscrowState as DkxEscrowState};
use dkx_projection::{ProjectionEvent, ProjectionWorker, TradeExecuted};
use dx_ledger::money::{self, Account as MoneyAccount, Asset as MoneyAsset, EntryKind};
use dx_ledger::{Bucket, JournalEntry as CoreJournalEntry, JournalKey, Owner};
use dx_types::{
    EscrowRecord as CoreEscrowRecord, EscrowState as CoreEscrowState, Fill as CoreFill, Side,
};
use tokio::sync::mpsc::UnboundedSender;

/// Mapping of a real ledger account/posting into the projection crate's
/// money-shaped account. Bucket `Held` maps to cash-locked and pass-held.
fn money_account(account: dx_ledger::AccountKey, bucket: Bucket) -> MoneyAccount {
    match (account.owner, bucket, account.asset) {
        (Owner::User(user), Bucket::Available, dx_types::Asset::Cash) => {
            MoneyAccount::CashAvail(user)
        }
        (Owner::User(user), _, dx_types::Asset::Cash) => MoneyAccount::CashLocked(user),
        (Owner::User(user), Bucket::Available, dx_types::Asset::Pass { market_id }) => {
            MoneyAccount::PassAvail(user, market_id)
        }
        (Owner::User(user), _, dx_types::Asset::Pass { market_id }) => {
            MoneyAccount::PassHeld(user, market_id)
        }
        (Owner::Escrow, _, _) => MoneyAccount::EscrowCash,
        (Owner::Fees, _, _) => MoneyAccount::PlatformFee,
        (Owner::Mint, _, dx_types::Asset::Pass { market_id }) => {
            MoneyAccount::IssuanceSource(market_id)
        }
        (Owner::Mint, _, dx_types::Asset::Cash) => MoneyAccount::FundingSource,
    }
}

/// Real journal entry -> projection entry. The idem key is the journal id
/// itself (unique and stable across WAL replays), the kind comes from the
/// journal key, and postings keep their signed amounts.
pub(crate) fn to_projection_entry(entry: &CoreJournalEntry) -> money::JournalEntry {
    money::JournalEntry {
        id: entry.id,
        key: money::IdemKey(entry.id as u128),
        ts_micros: 0, // journals carry no timestamp; the WAL/audit log keeps the
        kind: match entry.key {
            JournalKey::Trade(_) => EntryKind::Fill,
            JournalKey::Deposit { .. } => EntryKind::TopUp,
            JournalKey::Issue { .. } => EntryKind::IssuePass,
            JournalKey::EscrowRelease(_) => EntryKind::EscrowRelease,
            JournalKey::EscrowRefund(_) => EntryKind::EscrowRefund,
        },
        legs: entry
            .postings
            .iter()
            .map(|posting| money::Leg {
                account: money_account(posting.account, posting.bucket),
                asset: match posting.account.asset {
                    dx_types::Asset::Cash => MoneyAsset::Cash,
                    dx_types::Asset::Pass { market_id } => MoneyAsset::Pass(market_id),
                },
                amount: posting.amount,
            })
            .collect(),
    }
}

/// Core fill -> projection fill. Buyer/seller are resolved from the maker
/// side; the price stays in ticks (the projection is a replica of the tape,
/// the audit log records paise amounts where precision matters).
pub(crate) fn to_projection_fill(fill: &CoreFill) -> money::Fill {
    let (buyer, buyer_order, seller, seller_order) = if fill.maker_side == Side::Buy {
        (
            fill.maker_user_id,
            fill.maker_order_id,
            fill.taker_user_id,
            fill.taker_order_id,
        )
    } else {
        (
            fill.taker_user_id,
            fill.taker_order_id,
            fill.maker_user_id,
            fill.maker_order_id,
        )
    };
    money::Fill {
        market: fill.market_id,
        buyer,
        seller,
        buyer_order,
        seller_order,
        price: fill.price_ticks as i64,
        qty: fill.qty as i64,
        escrow: fill.escrow_id.is_some(),
        seq: fill.seq,
        ts_micros: 0, // core fills have no timestamp either
    }
}

/// Core escrow record -> dkx-escrow record. `id` collides with the trade id by
/// design: the gateway keys escrows by trade.
fn to_projection_escrow(record: &CoreEscrowRecord, updated_seq: u64) -> DkxEscrowRecord {
    DkxEscrowRecord {
        id: record.trade_id,
        trade_id: record.trade_id,
        market_id: record.market_id,
        buyer_id: record.buyer_id,
        seller_id: record.seller_id,
        qty: record.qty as u32,
        price: record.notional,
        fee: record.fee,
        state: match record.state {
            CoreEscrowState::PendingDelivery => DkxEscrowState::FundsLocked,
            CoreEscrowState::Delivered => DkxEscrowState::Delivered,
            CoreEscrowState::Refunded => DkxEscrowState::Refunded,
        },
        delivered_at: record.delivered_at_ms.unwrap_or(0),
        deadline: record.deadline_ms,
        receipt_bytes: Vec::new(),
        receipt_sig: Vec::new(),
        updated_seq,
    }
}

/// Event numbering for the projection stream. `next_seq` continues from the
/// watermark the worker last persisted, so restarts never go backwards.
pub(crate) struct ProjectionFeed {
    sender: UnboundedSender<ProjectionEvent>,
    next_seq: u64,
    journals_emitted: u64,
    escrows_seen: HashMap<u64, CoreEscrowRecord>,
    /// Users whose money a processed job touched; re-stated as absolute
    /// balance snapshots on the next flush.
    touch_users: std::collections::HashSet<u64>,
}

impl ProjectionFeed {
    pub(crate) fn new(sender: UnboundedSender<ProjectionEvent>, last_seq: u64) -> Self {
        Self {
            sender,
            next_seq: last_seq,
            journals_emitted: 0,
            escrows_seen: HashMap::new(),
            touch_users: std::collections::HashSet::new(),
        }
    }

    fn assign(&mut self) -> u64 {
        self.next_seq += 1;
        self.next_seq
    }

    /// One executed trade (called with each fresh fill inside the command
    /// batch, where the fills are at hand).
    pub(crate) fn emit_trade(&mut self, fill: &CoreFill) {
        let _seq = self.assign();
        let event = ProjectionEvent::TradeExecuted(TradeExecuted {
            id: fill.trade_id,
            fill: to_projection_fill(fill),
            fee: 0, // fees are booked as their own ledger legs
            escrow_id: fill.escrow_id,
            seq: self.assign(),
        });
        let _ = self.sender.send(event);
        self.touch_users.insert(fill.maker_user_id);
        self.touch_users.insert(fill.taker_user_id);
    }

    /// The absolute per-user balance snapshot event.
    fn emit_balances(&mut self, core: &dx_core::Core, user_id: u64) {
        let mut snapshot = dkx_projection::BalanceSnapshot {
            cash_available: 0,
            cash_held: 0,
            passes: Vec::new(),
        };
        for balance in core.balances(user_id) {
            match balance.asset {
                dx_types::Asset::Cash => {
                    snapshot.cash_available = balance.available;
                    snapshot.cash_held = balance.held;
                }
                dx_types::Asset::Pass { market_id } => {
                    snapshot.passes.push(dkx_projection::PassBalance {
                        market_id,
                        available: balance.available,
                        held: balance.held,
                    });
                }
            }
        }
        let seq = self.assign();
        let _ = self.sender.send(ProjectionEvent::BalancesSet {
            seq,
            user_id,
            snapshot,
        });
    }

    /// Catch-up flush: journals and current escrow records the projection may
    /// not have seen yet. Called after each writer job (and before the first
    /// one), so a restart replays everything the last incarnation missed.
    pub(crate) fn flush(&mut self, core: &dx_core::Core) {
        for entry in core.journals_since(self.journals_emitted) {
            self.journals_emitted = entry.id;
            for posting in &entry.postings {
                if let Owner::User(user_id) = posting.account.owner {
                    self.touch_users.insert(user_id);
                }
            }
            let seq = self.assign();
            let event = ProjectionEvent::LedgerPosted {
                seq,
                entry: to_projection_entry(&entry),
            };
            let _ = self.sender.send(event);
        }
        for record in core.all_escrow_records() {
            if self.escrows_seen.get(&record.trade_id) == Some(&record) {
                continue;
            }
            let seq = self.assign();
            let escrow = to_projection_escrow(&record, seq);
            self.escrows_seen.insert(record.trade_id, record.clone());
            let _ = self
                .sender
                .send(ProjectionEvent::EscrowUpserted { seq, escrow });
            self.touch_users.insert(record.buyer_id);
            self.touch_users.insert(record.seller_id);
        }
        // Absolute balance re-states last, after the journals that moved money.
        let touched: Vec<u64> = self.touch_users.drain().collect();
        for user_id in touched {
            self.emit_balances(core, user_id);
        }
    }
}

/// Connects, bootstraps the schema and spawns the apply worker. Returns the
/// channel end the writer feeds plus the last persisted projection seq. None
/// disables the projection (no `DATABASE_URL`, or `DX_PROJECTION=0`).
pub(crate) async fn spawn_projection_worker() -> Option<(UnboundedSender<ProjectionEvent>, u64)> {
    use tokio::sync::mpsc;

    if std::env::var("DX_PROJECTION")
        .map(|value| value == "0")
        .unwrap_or(false)
    {
        return None;
    }
    let Ok(url) = std::env::var("DATABASE_URL") else {
        return None;
    };
    let worker = match ProjectionWorker::connect(&url).await {
        Ok(worker) => worker,
        Err(error) => {
            eprintln!("projection: PostgreSQL unreachable ({error}), staying memory only");
            return None;
        }
    };
    if let Err(error) = worker.bootstrap().await {
        eprintln!("projection: schema bootstrap failed ({error}), staying memory only");
        return None;
    }
    let last_seq = worker.get_last_seq().await.unwrap_or(0);
    let (tx, mut rx) = mpsc::unbounded_channel::<ProjectionEvent>();
    tokio::spawn(async move {
        let mut buffer: Vec<ProjectionEvent> = Vec::new();
        loop {
            while let Ok(event) = rx.try_recv() {
                buffer.push(event);
            }
            if buffer.is_empty() {
                // Sleep until something arrives (None means the writer is
                // gone and the channel is closed).
                match rx.recv().await {
                    Some(event) => buffer.push(event),
                    None => break,
                }
                continue;
            }
            if let Err(error) = worker.apply_batch(&buffer).await {
                eprintln!("projection: apply failed ({error}), retrying in 1s");
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                continue;
            }
            buffer.clear();
        }
    });
    Some((tx, last_seq))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn money_account_maps_owner_asset_and_bucket() {
        assert_eq!(
            money_account(
                dx_ledger::AccountKey::user(7, dx_types::Asset::Cash),
                Bucket::Available
            ),
            MoneyAccount::CashAvail(7)
        );
        assert_eq!(
            money_account(
                dx_ledger::AccountKey::user(7, dx_types::Asset::Cash),
                Bucket::Held
            ),
            MoneyAccount::CashLocked(7)
        );
        assert_eq!(
            money_account(
                dx_ledger::AccountKey::user(7, dx_types::Asset::Pass { market_id: 3 }),
                Bucket::Held
            ),
            MoneyAccount::PassHeld(7, 3)
        );
        assert_eq!(
            money_account(
                dx_ledger::AccountKey::system(Owner::Fees, dx_types::Asset::Cash),
                Bucket::Available
            ),
            MoneyAccount::PlatformFee
        );
    }

    #[test]
    fn projection_journal_keeps_amounts_and_unique_idem_key() {
        let entry = CoreJournalEntry {
            id: 4,
            key: JournalKey::Deposit {
                user_id: 7,
                client_request_id: 900,
            },
            postings: vec![dx_ledger::Posting::new(
                dx_ledger::AccountKey::user(7, dx_types::Asset::Cash),
                Bucket::Available,
                500,
            )],
        };
        let projected = to_projection_entry(&entry);
        assert_eq!(projected.key.0, 4); // idem key = journal id
        assert_eq!(projected.kind, EntryKind::TopUp);
        assert_eq!(projected.legs.len(), 1);
        assert_eq!(projected.legs[0].account, MoneyAccount::CashAvail(7));
        assert_eq!(projected.legs[0].amount, 500);
    }

    #[test]
    fn fill_projection_resolves_buyer_and_seller_from_maker_side() {
        let fill_buyer = CoreFill {
            trade_id: 5,
            market_id: 3,
            maker_order_id: 11,
            taker_order_id: 22,
            maker_user_id: 1,
            taker_user_id: 2,
            maker_side: Side::Sell,
            price_ticks: 5200,
            qty: 2,
            seq: 12,
            escrow_id: None,
        };
        let projected = to_projection_fill(&fill_buyer);
        assert_eq!(projected.buyer, 2); // maker sold, so taker bought
        assert_eq!(projected.seller, 1);
        assert_eq!(projected.price, 5200);
        assert_eq!(projected.seq, 12);

        let maker_bought = CoreFill {
            maker_side: Side::Buy,
            ..fill_buyer
        };
        let projected = to_projection_fill(&maker_bought);
        assert_eq!(projected.buyer, 1);
        assert_eq!(projected.seller, 2);
    }

    #[test]
    fn escrow_projection_maps_states_by_trade_id() {
        let record = CoreEscrowRecord {
            trade_id: 9,
            market_id: 3,
            buyer_id: 1,
            seller_id: 2,
            qty: 3,
            notional: 15_600,
            fee: 16,
            fee_payer: 1,
            state: CoreEscrowState::PendingDelivery,
            created_at_ms: 1000,
            deadline_ms: 9000,
            delivered_at_ms: None,
            seller_public_key: [0u8; 32],
        };
        let projected = to_projection_escrow(&record, 77);
        assert_eq!(projected.id, 9);
        assert_eq!(projected.price, 15_600);
        assert_eq!(projected.state, DkxEscrowState::FundsLocked);
        assert_eq!(projected.updated_seq, 77);

        let refunded = CoreEscrowRecord {
            state: CoreEscrowState::Refunded,
            ..record
        };
        assert_eq!(
            to_projection_escrow(&refunded, 78).state,
            DkxEscrowState::Refunded
        );
    }

    #[test]
    fn flush_is_idempotent_and_tracks_journal_and_escrow_changes() {
        let mut core = dx_core::Core::new(dx_types::seed_markets()).unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ProjectionEvent>();
        let mut feed = ProjectionFeed::new(tx, 0);

        core.deposit_cash(1, 5000, 1).unwrap();
        feed.flush(&core);
        // One top-up journal entry plus an absolute balance re-state.
        let first: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert_eq!(first.len(), 2);
        let ProjectionEvent::LedgerPosted { seq, entry } = &first[0] else {
            panic!("expected ledger event");
        };
        assert_eq!(*seq, 1);
        assert_eq!(entry.kind, EntryKind::TopUp);
        assert!(
            entry
                .legs
                .iter()
                .any(|leg| leg.amount == 5_000
                    && matches!(leg.account, MoneyAccount::CashAvail(1))),
            "expected a +5000 CashAvail(1) leg, got {:?}",
            entry.legs
        );
        let ProjectionEvent::BalancesSet {
            seq,
            user_id,
            snapshot,
        } = &first[1]
        else {
            panic!("expected balances snapshot");
        };
        assert_eq!(*seq, 2);
        assert_eq!(*user_id, 1);
        assert_eq!(snapshot.cash_available, 5_000);
        assert!(snapshot.passes.is_empty());

        // Nothing changed: no journal, no escrow, no balance event.
        feed.flush(&core);
        assert!(rx.try_recv().is_err());

        // New journal: gets the next seq without depending on command seqs.
        core.deposit_cash(1, 2500, 2).unwrap();
        feed.flush(&core);
        let ProjectionEvent::LedgerPosted { seq, .. } = rx.try_recv().unwrap() else {
            panic!("expected ledger event");
        };
        assert_eq!(seq, 3);
        let ProjectionEvent::BalancesSet { snapshot, .. } = rx.try_recv().unwrap() else {
            panic!("expected balances snapshot");
        };
        assert_eq!(snapshot.cash_available, 7_500);
    }
}
