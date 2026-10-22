//! Single-writer orchestration: commands, matching, settlement, reservations, and
//! idempotency. Everything here is synchronous and deterministic.
//!
//! Reservation model:
//! - A buy reserves the worst-case cash at its limit/protection price plus a fee
//!   buffer and one minor unit per unit of quantity to cover per-fill fee rounding.
//! - Once matching finishes, the taker's excess is released. A resting buy keeps
//!   exactly `notional(remaining)` (it will be a fee-free maker); a resting sell
//!   keeps exactly `remaining` passes.
//! - A maker fill consumes exactly its remaining collateral. A self-trade
//!   cancellation releases the cancelled order's collateral explicitly.

use std::collections::HashMap;

use dx_engine::{Engine, PlaceOutcome};
use dx_ledger::{Ledger, LedgerError, Owner, TradeSettlement};
use dx_types::{
    Asset, Balance, BookSnapshot, Cancel, ClientOrderId, Command, CommandResponse, Fill,
    MarketConfig, MarketId, MarketStatus, Minor, OrderStatus, PlaceLimit, PlaceMarket, Qty,
    ReasonCode, Seq, Side, Ticks, UserId,
};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommandKind {
    PlaceLimit,
    PlaceMarket,
    Cancel,
}

struct CachedCommand {
    kind: CommandKind,
    response: CommandResponse,
}

pub struct Core {
    engine: Engine,
    ledger: Ledger,
    markets: HashMap<MarketId, MarketConfig>,
    next_seq: Seq,
    idempotency: HashMap<(UserId, ClientOrderId), CachedCommand>,
    /// Sum of open buy quantities per identity/market, for the holding limit.
    open_buy_qty: HashMap<(UserId, MarketId), Qty>,
    poisoned: bool,
}

impl Core {
    pub fn new(markets: Vec<MarketConfig>) -> Result<Self, String> {
        let engine = Engine::new(markets.clone())?;
        let markets = markets
            .into_iter()
            .map(|market| (market.id, market))
            .collect();
        Ok(Self {
            engine,
            ledger: Ledger::new(),
            markets,
            next_seq: 1,
            idempotency: HashMap::new(),
            open_buy_qty: HashMap::new(),
            poisoned: false,
        })
    }

    /// Process one authenticated command. The caller supplies identity; the wire
    /// command never carries a user id.
    pub fn process(&mut self, user_id: UserId, command: Command) -> CommandResponse {
        let (kind, client_order_id) = command_identity(&command);
        let cache_key = (user_id, client_order_id);

        if let Some(cached) = self.idempotency.get(&cache_key) {
            if cached.kind == kind {
                return cached.response.clone();
            }
            let seq = self.alloc_seq();
            return CommandResponse::rejected(seq, ReasonCode::DuplicateClientOrderId);
        }

        if self.poisoned {
            let seq = self.alloc_seq();
            return CommandResponse::rejected(seq, ReasonCode::InternalError);
        }

        let seq = self.alloc_seq();
        let response = match command {
            Command::PlaceLimit(place) => self.place_limit(seq, user_id, place),
            Command::PlaceMarket(place) => self.place_market(seq, user_id, place),
            Command::Cancel(cancel) => self.cancel(seq, user_id, cancel),
        };
        self.idempotency.insert(
            cache_key,
            CachedCommand {
                kind,
                response: response.clone(),
            },
        );
        response
    }

    fn alloc_seq(&mut self) -> Seq {
        let seq = self.next_seq;
        self.next_seq = self.next_seq.saturating_add(1);
        seq
    }

    fn place_limit(&mut self, seq: Seq, user_id: UserId, place: PlaceLimit) -> CommandResponse {
        let Some(market) = self.markets.get(&place.market_id).cloned() else {
            return CommandResponse::rejected(seq, ReasonCode::UnknownMarket);
        };
        if market.status != MarketStatus::Open {
            return CommandResponse::rejected(seq, ReasonCode::MarketClosed);
        }
        if place.qty == 0 {
            return CommandResponse::rejected(seq, ReasonCode::InvalidQuantity);
        }
        if !market.price_in_collar(place.price_ticks) {
            return CommandResponse::rejected(seq, ReasonCode::OutsideCollar);
        }

        let pass_asset = Asset::Pass {
            market_id: market.id,
        };
        let mut reserved_cash = 0;
        let mut reserved_pass = 0;

        match place.side {
            Side::Buy => {
                if !self.within_holding_limit(user_id, market.id, place.qty, market.holding_limit) {
                    return CommandResponse::rejected(seq, ReasonCode::HoldingLimitExceeded);
                }
                let Some(cash) = worst_case_cash(&market, place.price_ticks, place.qty) else {
                    return CommandResponse::rejected(seq, ReasonCode::InvalidQuantity);
                };
                if let Err(error) = self.ledger.reserve(user_id, Asset::Cash, cash) {
                    return self.reject_ledger(seq, &error, Asset::Cash);
                }
                reserved_cash = cash;
                if let Err(code) = self.add_open_buy(user_id, market.id, place.qty) {
                    let _ = self.ledger.release(user_id, Asset::Cash, cash);
                    return CommandResponse::rejected(seq, code);
                }
            }
            Side::Sell => {
                let Ok(qty) = i64::try_from(place.qty) else {
                    return CommandResponse::rejected(seq, ReasonCode::InvalidQuantity);
                };
                if let Err(error) = self.ledger.reserve(user_id, pass_asset, qty) {
                    return self.reject_ledger(seq, &error, pass_asset);
                }
                reserved_pass = qty;
            }
        }

        let outcome = match self.engine.place_limit(
            user_id,
            market.id,
            place.side,
            place.price_ticks,
            place.qty,
        ) {
            Ok(outcome) => outcome,
            Err(code) => {
                self.rollback_reservation(user_id, market.id, place.side, reserved_cash, place.qty);
                return CommandResponse::rejected(seq, code);
            }
        };

        let (fills, taker_cash_spent) =
            match self.settle_fills(seq, user_id, place.side, &market, &outcome) {
                Ok(result) => result,
                Err(code) => return CommandResponse::rejected(seq, code),
            };
        if let Err(code) = self.release_cancelled_self_trades(&market, &outcome) {
            return CommandResponse::rejected(seq, code);
        }

        if place.side == Side::Buy {
            let canonical = if outcome.remaining_qty == 0 {
                Some(0)
            } else {
                market.notional_minor(place.price_ticks, outcome.remaining_qty)
            };
            let Some(canonical) = canonical else {
                self.poisoned = true;
                return CommandResponse::rejected(seq, ReasonCode::InternalError);
            };
            let Some(excess) = reserved_cash
                .checked_sub(taker_cash_spent)
                .and_then(|value| value.checked_sub(canonical))
            else {
                self.poisoned = true;
                return CommandResponse::rejected(seq, ReasonCode::InternalError);
            };
            if let Err(code) = self.release_cash(user_id, excess) {
                return CommandResponse::rejected(seq, code);
            }
        } else {
            let Ok(filled) = i64::try_from(outcome.filled_qty) else {
                self.poisoned = true;
                return CommandResponse::rejected(seq, ReasonCode::InternalError);
            };
            let Ok(canonical) = i64::try_from(outcome.remaining_qty) else {
                self.poisoned = true;
                return CommandResponse::rejected(seq, ReasonCode::InternalError);
            };
            let Some(excess) = reserved_pass
                .checked_sub(filled)
                .and_then(|value| value.checked_sub(canonical))
            else {
                self.poisoned = true;
                return CommandResponse::rejected(seq, ReasonCode::InternalError);
            };
            // A resting limit sell keeps exactly its remaining passes.
            if excess != 0 {
                self.poisoned = true;
                return CommandResponse::rejected(seq, ReasonCode::InternalError);
            }
        }

        CommandResponse {
            seq,
            status: outcome.status,
            order_id: Some(outcome.order_id),
            filled_qty: outcome.filled_qty,
            remaining_qty: outcome.remaining_qty,
            fills,
            reason: None,
        }
    }

    fn place_market(&mut self, seq: Seq, user_id: UserId, place: PlaceMarket) -> CommandResponse {
        let Some(market) = self.markets.get(&place.market_id).cloned() else {
            return CommandResponse::rejected(seq, ReasonCode::UnknownMarket);
        };
        if market.status != MarketStatus::Open {
            return CommandResponse::rejected(seq, ReasonCode::MarketClosed);
        }
        if place.qty == 0 {
            return CommandResponse::rejected(seq, ReasonCode::InvalidQuantity);
        }
        if !market.price_in_collar(place.protect_price_ticks) {
            return CommandResponse::rejected(seq, ReasonCode::OutsideCollar);
        }

        let pass_asset = Asset::Pass {
            market_id: market.id,
        };
        let mut reserved_cash = 0;
        let mut reserved_pass = 0;

        match place.side {
            Side::Buy => {
                if !self.within_holding_limit(user_id, market.id, place.qty, market.holding_limit) {
                    return CommandResponse::rejected(seq, ReasonCode::HoldingLimitExceeded);
                }
                let Some(cash) = worst_case_cash(&market, place.protect_price_ticks, place.qty)
                else {
                    return CommandResponse::rejected(seq, ReasonCode::InvalidQuantity);
                };
                if let Err(error) = self.ledger.reserve(user_id, Asset::Cash, cash) {
                    return self.reject_ledger(seq, &error, Asset::Cash);
                }
                reserved_cash = cash;
                if let Err(code) = self.add_open_buy(user_id, market.id, place.qty) {
                    let _ = self.ledger.release(user_id, Asset::Cash, cash);
                    return CommandResponse::rejected(seq, code);
                }
            }
            Side::Sell => {
                let Ok(qty) = i64::try_from(place.qty) else {
                    return CommandResponse::rejected(seq, ReasonCode::InvalidQuantity);
                };
                if let Err(error) = self.ledger.reserve(user_id, pass_asset, qty) {
                    return self.reject_ledger(seq, &error, pass_asset);
                }
                reserved_pass = qty;
            }
        }

        let outcome = match self.engine.place_market(
            user_id,
            market.id,
            place.side,
            place.qty,
            place.protect_price_ticks,
        ) {
            Ok(outcome) => outcome,
            Err(code) => {
                self.rollback_reservation(user_id, market.id, place.side, reserved_cash, place.qty);
                return CommandResponse::rejected(seq, code);
            }
        };

        let (fills, taker_cash_spent) =
            match self.settle_fills(seq, user_id, place.side, &market, &outcome) {
                Ok(result) => result,
                Err(code) => return CommandResponse::rejected(seq, code),
            };
        if let Err(code) = self.release_cancelled_self_trades(&market, &outcome) {
            return CommandResponse::rejected(seq, code);
        }

        match place.side {
            Side::Buy => {
                let Some(excess) = reserved_cash.checked_sub(taker_cash_spent) else {
                    self.poisoned = true;
                    return CommandResponse::rejected(seq, ReasonCode::InternalError);
                };
                if let Err(code) = self.release_cash(user_id, excess) {
                    return CommandResponse::rejected(seq, code);
                }
                // A market order never rests: release the unfilled holding-limit slot.
                if let Err(code) = self.sub_open_buy(user_id, market.id, outcome.remaining_qty) {
                    return CommandResponse::rejected(seq, code);
                }
            }
            Side::Sell => {
                let Ok(filled) = i64::try_from(outcome.filled_qty) else {
                    self.poisoned = true;
                    return CommandResponse::rejected(seq, ReasonCode::InternalError);
                };
                let Some(unfilled) = reserved_pass.checked_sub(filled) else {
                    self.poisoned = true;
                    return CommandResponse::rejected(seq, ReasonCode::InternalError);
                };
                if unfilled < 0 {
                    self.poisoned = true;
                    return CommandResponse::rejected(seq, ReasonCode::InternalError);
                }
                if unfilled > 0
                    && let Err(code) = self.release_pass(user_id, market.id, unfilled as Qty)
                {
                    return CommandResponse::rejected(seq, code);
                }
            }
        }

        CommandResponse {
            seq,
            status: outcome.status,
            order_id: Some(outcome.order_id),
            filled_qty: outcome.filled_qty,
            remaining_qty: outcome.remaining_qty,
            fills,
            reason: None,
        }
    }

    fn cancel(&mut self, seq: Seq, user_id: UserId, cancel: Cancel) -> CommandResponse {
        let mut found = None;
        for market_id in self.engine.market_ids() {
            if let Some(order) = self
                .engine
                .book(market_id)
                .and_then(|book| book.order(cancel.order_id))
            {
                found = Some((
                    market_id,
                    order.user_id,
                    order.side,
                    order.price_ticks,
                    order.remaining,
                ));
                break;
            }
        }
        let Some((market_id, owner, side, price_ticks, remaining)) = found else {
            return CommandResponse::rejected(seq, ReasonCode::OrderNotFound);
        };
        if owner != user_id {
            return CommandResponse::rejected(seq, ReasonCode::NotOrderOwner);
        }
        let Some(market) = self.markets.get(&market_id).cloned() else {
            self.poisoned = true;
            return CommandResponse::rejected(seq, ReasonCode::InternalError);
        };

        let outcome = match self.engine.cancel(user_id, cancel.order_id) {
            Ok(outcome) => outcome,
            Err(code) => return CommandResponse::rejected(seq, code),
        };
        if outcome.cancelled_qty != remaining {
            self.poisoned = true;
            return CommandResponse::rejected(seq, ReasonCode::InternalError);
        }

        match side {
            Side::Buy => {
                let Some(cash) = market.notional_minor(price_ticks, outcome.cancelled_qty) else {
                    self.poisoned = true;
                    return CommandResponse::rejected(seq, ReasonCode::InternalError);
                };
                if let Err(code) = self.release_cash(user_id, cash) {
                    return CommandResponse::rejected(seq, code);
                }
                if let Err(code) = self.sub_open_buy(user_id, market_id, outcome.cancelled_qty) {
                    return CommandResponse::rejected(seq, code);
                }
            }
            Side::Sell => {
                if let Err(code) = self.release_pass(user_id, market_id, outcome.cancelled_qty) {
                    return CommandResponse::rejected(seq, code);
                }
            }
        }

        CommandResponse {
            seq,
            status: OrderStatus::Cancelled,
            order_id: Some(cancel.order_id),
            filled_qty: 0,
            remaining_qty: outcome.cancelled_qty,
            fills: Vec::new(),
            reason: None,
        }
    }

    fn settle_fills(
        &mut self,
        seq: Seq,
        taker_user: UserId,
        taker_side: Side,
        market: &MarketConfig,
        outcome: &PlaceOutcome,
    ) -> Result<(Vec<Fill>, Minor), ReasonCode> {
        let mut fills = Vec::with_capacity(outcome.fills.len());
        let mut taker_cash_spent: Minor = 0;

        for fill in &outcome.fills {
            let (buyer_id, seller_id) = match taker_side {
                Side::Buy => (taker_user, fill.maker_user_id),
                Side::Sell => (fill.maker_user_id, taker_user),
            };
            let Some(notional) = market.notional_minor(fill.price_ticks, fill.qty) else {
                self.poisoned = true;
                return Err(ReasonCode::InternalError);
            };
            let fee = market.taker_fee_minor(notional);
            let settlement = TradeSettlement {
                trade_id: fill.trade_id,
                market_id: market.id,
                buyer_id,
                seller_id,
                qty: fill.qty,
                notional,
                fee,
                fee_payer: taker_user,
            };
            if self.ledger.settle_trade(settlement).is_err() {
                self.poisoned = true;
                return Err(ReasonCode::InternalError);
            }

            if taker_side == Side::Buy {
                let Some(spent) = notional.checked_add(fee) else {
                    self.poisoned = true;
                    return Err(ReasonCode::InternalError);
                };
                let Some(total) = taker_cash_spent.checked_add(spent) else {
                    self.poisoned = true;
                    return Err(ReasonCode::InternalError);
                };
                taker_cash_spent = total;
            }

            self.sub_open_buy(buyer_id, market.id, fill.qty)?;

            fills.push(Fill {
                trade_id: fill.trade_id,
                market_id: market.id,
                maker_order_id: fill.maker_order_id,
                taker_order_id: fill.taker_order_id,
                maker_user_id: fill.maker_user_id,
                taker_user_id: fill.taker_user_id,
                maker_side: fill.maker_side,
                price_ticks: fill.price_ticks,
                qty: fill.qty,
                seq,
            });
        }

        Ok((fills, taker_cash_spent))
    }

    fn release_cancelled_self_trades(
        &mut self,
        market: &MarketConfig,
        outcome: &PlaceOutcome,
    ) -> Result<(), ReasonCode> {
        for order in &outcome.cancelled_self_trade_orders {
            match order.side {
                Side::Buy => {
                    let Some(cash) = market.notional_minor(order.price_ticks, order.remaining)
                    else {
                        self.poisoned = true;
                        return Err(ReasonCode::InternalError);
                    };
                    self.release_cash(order.user_id, cash)?;
                    self.sub_open_buy(order.user_id, market.id, order.remaining)?;
                }
                Side::Sell => {
                    self.release_pass(order.user_id, market.id, order.remaining)?;
                }
            }
        }
        Ok(())
    }

    fn rollback_reservation(
        &mut self,
        user_id: UserId,
        market_id: MarketId,
        side: Side,
        reserved_cash: Minor,
        qty: Qty,
    ) {
        match side {
            Side::Buy => {
                let _ = self.release_cash(user_id, reserved_cash);
                let _ = self.sub_open_buy(user_id, market_id, qty);
            }
            Side::Sell => {
                let _ = self.release_pass(user_id, market_id, qty);
            }
        }
    }

    fn within_holding_limit(
        &self,
        user_id: UserId,
        market_id: MarketId,
        qty: Qty,
        limit: u32,
    ) -> bool {
        let pass = self.ledger.balance(user_id, Asset::Pass { market_id });
        let available = u64::try_from(pass.available).unwrap_or(0);
        let held = u64::try_from(pass.held).unwrap_or(0);
        let open = self
            .open_buy_qty
            .get(&(user_id, market_id))
            .copied()
            .unwrap_or(0);
        available
            .saturating_add(held)
            .saturating_add(open)
            .saturating_add(qty)
            <= limit as u64
    }

    fn add_open_buy(
        &mut self,
        user_id: UserId,
        market_id: MarketId,
        qty: Qty,
    ) -> Result<(), ReasonCode> {
        if qty == 0 {
            return Ok(());
        }
        let key = (user_id, market_id);
        let current = self.open_buy_qty.get(&key).copied().unwrap_or(0);
        let Some(next) = current.checked_add(qty) else {
            self.poisoned = true;
            return Err(ReasonCode::InternalError);
        };
        self.open_buy_qty.insert(key, next);
        Ok(())
    }

    fn sub_open_buy(
        &mut self,
        user_id: UserId,
        market_id: MarketId,
        qty: Qty,
    ) -> Result<(), ReasonCode> {
        if qty == 0 {
            return Ok(());
        }
        let key = (user_id, market_id);
        let Some(current) = self.open_buy_qty.get(&key).copied() else {
            self.poisoned = true;
            return Err(ReasonCode::InternalError);
        };
        let Some(next) = current.checked_sub(qty) else {
            self.poisoned = true;
            return Err(ReasonCode::InternalError);
        };
        if next == 0 {
            self.open_buy_qty.remove(&key);
        } else {
            self.open_buy_qty.insert(key, next);
        }
        Ok(())
    }

    fn release_cash(&mut self, user_id: UserId, amount: Minor) -> Result<(), ReasonCode> {
        if amount < 0 {
            self.poisoned = true;
            return Err(ReasonCode::InternalError);
        }
        if amount == 0 {
            return Ok(());
        }
        if self.ledger.release(user_id, Asset::Cash, amount).is_err() {
            self.poisoned = true;
            return Err(ReasonCode::InternalError);
        }
        Ok(())
    }

    fn release_pass(
        &mut self,
        user_id: UserId,
        market_id: MarketId,
        amount: Qty,
    ) -> Result<(), ReasonCode> {
        if amount == 0 {
            return Ok(());
        }
        let Ok(amount) = i64::try_from(amount) else {
            self.poisoned = true;
            return Err(ReasonCode::InternalError);
        };
        if self
            .ledger
            .release(user_id, Asset::Pass { market_id }, amount)
            .is_err()
        {
            self.poisoned = true;
            return Err(ReasonCode::InternalError);
        }
        Ok(())
    }

    fn reject_ledger(&mut self, seq: Seq, error: &LedgerError, asset: Asset) -> CommandResponse {
        let code = match error {
            LedgerError::InsufficientFunds { .. } | LedgerError::InsufficientHeld { .. } => {
                if asset == Asset::Cash {
                    ReasonCode::InsufficientFunds
                } else {
                    ReasonCode::InsufficientInventory
                }
            }
            LedgerError::NonPositiveAmount => ReasonCode::InvalidQuantity,
            _ => {
                self.poisoned = true;
                ReasonCode::InternalError
            }
        };
        CommandResponse::rejected(seq, code)
    }

    /// Dev/simulator top-up. Idempotent per `(user_id, client_request_id)`.
    pub fn deposit_cash(
        &mut self,
        user_id: UserId,
        amount: Minor,
        client_request_id: ClientOrderId,
    ) -> Result<u64, ReasonCode> {
        self.ledger
            .deposit_cash(user_id, amount, client_request_id)
            .map_err(|error| match error {
                LedgerError::NonPositiveAmount => ReasonCode::InvalidQuantity,
                _ => ReasonCode::InternalError,
            })
    }

    /// Dev/simulator pass issuance. Idempotent per user/market/request.
    pub fn issue_passes(
        &mut self,
        user_id: UserId,
        market_id: MarketId,
        qty: Qty,
        client_request_id: ClientOrderId,
    ) -> Result<u64, ReasonCode> {
        if !self.markets.contains_key(&market_id) {
            return Err(ReasonCode::UnknownMarket);
        }
        self.ledger
            .issue_passes(user_id, market_id, qty, client_request_id)
            .map_err(|error| match error {
                LedgerError::NonPositiveAmount => ReasonCode::InvalidQuantity,
                _ => ReasonCode::InternalError,
            })
    }

    pub fn balance(&self, user_id: UserId, asset: Asset) -> Balance {
        self.ledger.balance(user_id, asset)
    }

    pub fn balances(&self, user_id: UserId) -> Vec<Balance> {
        self.ledger.balances(user_id)
    }

    pub fn snapshot(&self, market_id: MarketId, depth: usize) -> Option<BookSnapshot> {
        self.engine.snapshot(market_id, depth)
    }

    pub fn config(&self, market_id: MarketId) -> Option<&MarketConfig> {
        self.markets.get(&market_id)
    }

    pub fn open_buy_qty(&self, user_id: UserId, market_id: MarketId) -> Qty {
        self.open_buy_qty
            .get(&(user_id, market_id))
            .copied()
            .unwrap_or(0)
    }

    pub fn is_poisoned(&self) -> bool {
        self.poisoned
    }

    /// Last sequence assigned by the core. Zero means no command has been processed.
    pub fn last_seq(&self) -> Seq {
        self.next_seq.saturating_sub(1)
    }

    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    pub fn ledger(&self) -> &Ledger {
        &self.ledger
    }

    /// Hash of matching state plus account balances. The idempotency cache is not
    /// included; recovery rebuilds it from the command log.
    pub fn state_hash(&self) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(self.engine.state_hash());

        for (key, account) in self.ledger.sorted_accounts() {
            hash_owner(&mut hasher, key.owner);
            hash_asset(&mut hasher, key.asset);
            hasher.update(account.available.to_le_bytes());
            hasher.update(account.held.to_le_bytes());
        }

        let mut open_buys: Vec<_> = self.open_buy_qty.iter().collect();
        open_buys.sort_by_key(|((user_id, market_id), _)| (*user_id, *market_id));
        for ((user_id, market_id), qty) in open_buys {
            hasher.update(user_id.to_le_bytes());
            hasher.update(market_id.to_le_bytes());
            hasher.update(qty.to_le_bytes());
        }
        hasher.update(self.next_seq.to_le_bytes());
        hasher.update([self.poisoned as u8]);
        hasher.finalize().into()
    }

    /// Strong consistency check for tests and the solvency worker. Verifies that
    /// ledger `held` exactly matches the canonical collateral of every open order.
    pub fn check_invariants(&self) -> Result<(), String> {
        self.ledger
            .check_invariants()
            .map_err(|error| format!("ledger invariant: {error:?}"))?;

        let mut expected_cash: HashMap<UserId, Minor> = HashMap::new();
        let mut expected_pass: HashMap<(UserId, MarketId), Qty> = HashMap::new();
        let mut expected_open_buy: HashMap<(UserId, MarketId), Qty> = HashMap::new();

        for market_id in self.engine.market_ids() {
            let market = self
                .markets
                .get(&market_id)
                .ok_or_else(|| format!("missing market {market_id}"))?;
            let book = self
                .engine
                .book(market_id)
                .ok_or_else(|| format!("missing book {market_id}"))?;
            for order in book.orders() {
                match order.side {
                    Side::Buy => {
                        let cash = market
                            .notional_minor(order.price_ticks, order.remaining)
                            .ok_or_else(|| "open buy notional overflow".to_string())?;
                        let entry = expected_cash.entry(order.user_id).or_default();
                        *entry = entry
                            .checked_add(cash)
                            .ok_or_else(|| "expected cash overflow".to_string())?;
                        let entry = expected_open_buy
                            .entry((order.user_id, market_id))
                            .or_default();
                        *entry = entry
                            .checked_add(order.remaining)
                            .ok_or_else(|| "expected open buy overflow".to_string())?;
                    }
                    Side::Sell => {
                        let entry = expected_pass.entry((order.user_id, market_id)).or_default();
                        *entry = entry
                            .checked_add(order.remaining)
                            .ok_or_else(|| "expected pass overflow".to_string())?;
                    }
                }
            }
        }

        for (key, account) in self.ledger.accounts() {
            let Owner::User(user_id) = key.owner else {
                continue;
            };
            let expected = match key.asset {
                Asset::Cash => expected_cash.get(&user_id).copied().unwrap_or(0),
                Asset::Pass { market_id } => i64::try_from(
                    expected_pass
                        .get(&(user_id, market_id))
                        .copied()
                        .unwrap_or(0),
                )
                .map_err(|_| "expected pass held overflow".to_string())?,
            };
            if account.held != expected {
                return Err(format!(
                    "held mismatch for user {user_id} {:?}: ledger {} expected {expected}",
                    key.asset, account.held
                ));
            }
        }

        for (user_id, expected) in expected_cash {
            let held = self.ledger.balance(user_id, Asset::Cash).held;
            if held != expected {
                return Err(format!(
                    "cash held mismatch for user {user_id}: ledger {held} expected {expected}"
                ));
            }
        }
        for ((user_id, market_id), expected) in expected_pass {
            let expected =
                i64::try_from(expected).map_err(|_| "expected pass held overflow".to_string())?;
            let held = self.ledger.balance(user_id, Asset::Pass { market_id }).held;
            if held != expected {
                return Err(format!(
                    "pass held mismatch for user {user_id} market {market_id}: ledger {held} expected {expected}"
                ));
            }
        }

        let mut open_buy_keys: Vec<_> = self.open_buy_qty.keys().copied().collect();
        open_buy_keys.extend(expected_open_buy.keys().copied());
        open_buy_keys.sort_unstable();
        open_buy_keys.dedup();
        for key in open_buy_keys {
            let actual = self.open_buy_qty.get(&key).copied().unwrap_or(0);
            let expected = expected_open_buy.get(&key).copied().unwrap_or(0);
            if actual != expected {
                return Err(format!(
                    "open buy mismatch for user {} market {}: actual {actual} expected {expected}",
                    key.0, key.1
                ));
            }
        }

        Ok(())
    }
}

fn worst_case_cash(market: &MarketConfig, price_ticks: Ticks, qty: Qty) -> Option<Minor> {
    let notional = market.notional_minor(price_ticks, qty)?;
    let fee = market.taker_fee_minor(notional);
    let rounding_buffer = i64::try_from(qty).ok()?;
    notional.checked_add(fee)?.checked_add(rounding_buffer)
}

fn command_identity(command: &Command) -> (CommandKind, ClientOrderId) {
    match command {
        Command::PlaceLimit(place) => (CommandKind::PlaceLimit, place.client_order_id),
        Command::PlaceMarket(place) => (CommandKind::PlaceMarket, place.client_order_id),
        Command::Cancel(cancel) => (CommandKind::Cancel, cancel.client_order_id),
    }
}

fn hash_owner(hasher: &mut Sha256, owner: Owner) {
    match owner {
        Owner::User(user_id) => {
            hasher.update([0]);
            hasher.update(user_id.to_le_bytes());
        }
        Owner::Mint => hasher.update([1]),
        Owner::Fees => hasher.update([2]),
        Owner::Escrow => hasher.update([3]),
    }
}

fn hash_asset(hasher: &mut Sha256, asset: Asset) {
    match asset {
        Asset::Cash => hasher.update([0]),
        Asset::Pass { market_id } => {
            hasher.update([1]);
            hasher.update(market_id.to_le_bytes());
        }
    }
}
