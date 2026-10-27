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
    Asset, Balance, BookDelta, BookSnapshot, Cancel, ClientOrderId, Command, CommandResponse,
    DeliveryReceipt, EscrowRecord, EscrowState, Fill, MarketConfig, MarketId, MarketStatus, Minor,
    OrderStatus, PlaceLimit, PlaceMarket, Qty, ReasonCode, Seq, Side, Ticks, TradeId, UserId,
};
use ed25519_dalek::{Signature, VerifyingKey};
use sha2::{Digest, Sha256};

/// How long a buyer's cash and a seller's passes stay in escrow before a refund
/// can be requested. Kept short for the demo; production would use a market-level
/// policy.
pub const ESCROW_TIMEOUT_MS: u64 = 5 * 60 * 1000;

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
    /// Monotonic version for state fingerprints and solvency snapshots.
    ///
    /// Advances on every committed command and every new ledger journal. Unlike
    /// `next_seq`, this includes deposits and issuance, which bypass command
    /// sequencing.
    state_version: u64,
    idempotency: HashMap<(UserId, ClientOrderId), CachedCommand>,
    /// Sum of open buy quantities per identity/market, for the holding limit.
    open_buy_qty: HashMap<(UserId, MarketId), Qty>,
    /// Trades waiting for a signed delivery receipt, or already closed.
    escrow: HashMap<TradeId, EscrowRecord>,
    /// Seller signing keys. Registered explicitly so escrow release is replayable.
    public_keys: HashMap<UserId, [u8; 32]>,
    /// Wall-clock time supplied by the gateway for the current command. Not hashed;
    /// replay supplies the same value from the WAL operation.
    current_time_ms: u64,
    poisoned: bool,
}

/// The response to a command plus the absolute book changes it produced.
///
/// Successful trades keep their fills in [`CommandOutcome::response`]. Cached
/// idempotent responses, rejected commands, and calls on a poisoned core carry
/// no deltas because they do not change the book.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutcome {
    pub response: CommandResponse,
    pub deltas: Vec<BookDelta>,
    /// True when this response came from the idempotency cache. Publishers must
    /// not treat the cached response's fills as new trades.
    pub cached: bool,
}

impl CommandOutcome {
    fn without_deltas(response: CommandResponse) -> Self {
        Self {
            response,
            deltas: Vec::new(),
            cached: false,
        }
    }

    fn cached(response: CommandResponse) -> Self {
        Self {
            response,
            deltas: Vec::new(),
            cached: true,
        }
    }

    /// Fills produced by the command. They remain part of the response contract;
    /// this is a convenience accessor for event publishers.
    pub fn fills(&self) -> &[Fill] {
        &self.response.fills
    }
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
            state_version: 0,
            idempotency: HashMap::new(),
            open_buy_qty: HashMap::new(),
            escrow: HashMap::new(),
            public_keys: HashMap::new(),
            current_time_ms: 0,
            poisoned: false,
        })
    }

    /// Create one market's engine book and make it visible to command validation.
    ///
    /// The admin publisher calls this through the single-writer queue, so the new
    /// market is inserted at the same point in the commit order as every other
    /// state change. This is a state change, so it consumes a core sequence.
    pub fn add_market(&mut self, config: MarketConfig) -> Result<(), String> {
        if self.poisoned {
            return Err("core is poisoned".into());
        }
        config.validate()?;
        if self.markets.contains_key(&config.id) {
            return Err("duplicate market id".into());
        }

        self.engine.add_market(config.clone())?;
        self.markets.insert(config.id, config);
        self.alloc_seq();
        Ok(())
    }

    /// Process one authenticated command. The caller supplies identity; the wire
    /// command never carries a user id.
    ///
    /// This response-only wrapper is kept for callers that do not publish market
    /// data. Event publishers should use [`Core::process_with_deltas`].
    pub fn process(&mut self, user_id: UserId, command: Command) -> CommandResponse {
        self.process_with_deltas(user_id, command).response
    }

    /// Process one command and return the client response together with the
    /// absolute book changes caused by it.
    ///
    /// Delta sequencing is per market. A delta is emitted only when at least
    /// one level actually changes; cached idempotent responses, rejects, and
    /// poisoned-core responses emit nothing.
    pub fn process_with_deltas(&mut self, user_id: UserId, command: Command) -> CommandOutcome {
        let (kind, client_order_id) = command_identity(&command);
        let cache_key = (user_id, client_order_id);

        if let Some(cached) = self.idempotency.get(&cache_key) {
            if cached.kind == kind {
                return CommandOutcome::cached(cached.response.clone());
            }
            let seq = self.alloc_seq();
            return CommandOutcome::without_deltas(CommandResponse::rejected(
                seq,
                ReasonCode::DuplicateClientOrderId,
            ));
        }

        if self.poisoned {
            let seq = self.alloc_seq();
            return CommandOutcome::without_deltas(CommandResponse::rejected(
                seq,
                ReasonCode::InternalError,
            ));
        }

        let seq = self.alloc_seq();
        let mut deltas = Vec::new();
        let response = match command {
            Command::PlaceLimit(place) => self.place_limit(seq, user_id, place, &mut deltas),
            Command::PlaceMarket(place) => self.place_market(seq, user_id, place, &mut deltas),
            Command::Cancel(cancel) => self.cancel(seq, user_id, cancel, &mut deltas),
        };
        self.idempotency.insert(
            cache_key,
            CachedCommand {
                kind,
                response: response.clone(),
            },
        );
        CommandOutcome {
            response,
            deltas,
            cached: false,
        }
    }

    fn alloc_seq(&mut self) -> Seq {
        let seq = self.next_seq;
        self.next_seq = self.next_seq.saturating_add(1);
        self.bump_state_version();
        seq
    }

    fn bump_state_version(&mut self) {
        self.state_version = self.state_version.saturating_add(1);
    }

    fn place_limit(
        &mut self,
        seq: Seq,
        user_id: UserId,
        place: PlaceLimit,
        deltas: &mut Vec<BookDelta>,
    ) -> CommandResponse {
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
        if market.escrow_enabled
            && place.side == Side::Sell
            && !self.public_keys.contains_key(&user_id)
        {
            return CommandResponse::rejected(seq, ReasonCode::EscrowNotReady);
        }

        let pass_asset = Asset::Pass {
            market_id: market.id,
        };
        let mut reserved_cash = 0;
        let mut reserved_pass = 0;

        match place.side {
            Side::Buy => {
                if !self.within_holding_limit(
                    user_id,
                    market.id,
                    place.side,
                    place.qty,
                    market.holding_limit,
                ) {
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

        if let Some(delta) = outcome.delta {
            deltas.push(delta);
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

    fn place_market(
        &mut self,
        seq: Seq,
        user_id: UserId,
        place: PlaceMarket,
        deltas: &mut Vec<BookDelta>,
    ) -> CommandResponse {
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
        if market.escrow_enabled
            && place.side == Side::Sell
            && !self.public_keys.contains_key(&user_id)
        {
            return CommandResponse::rejected(seq, ReasonCode::EscrowNotReady);
        }

        let pass_asset = Asset::Pass {
            market_id: market.id,
        };
        let mut reserved_cash = 0;
        let mut reserved_pass = 0;

        match place.side {
            Side::Buy => {
                if !self.within_holding_limit(
                    user_id,
                    market.id,
                    place.side,
                    place.qty,
                    market.holding_limit,
                ) {
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

        if let Some(delta) = outcome.delta {
            deltas.push(delta);
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

    fn cancel(
        &mut self,
        seq: Seq,
        user_id: UserId,
        cancel: Cancel,
        deltas: &mut Vec<BookDelta>,
    ) -> CommandResponse {
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

        deltas.push(outcome.delta);
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
            if market.escrow_enabled {
                let Some(seller_public_key) = self.public_keys.get(&seller_id).copied() else {
                    self.poisoned = true;
                    return Err(ReasonCode::EscrowNotReady);
                };
                if self.ledger.hold_escrow(settlement).is_err() {
                    self.poisoned = true;
                    return Err(ReasonCode::InternalError);
                }
                self.escrow.insert(
                    fill.trade_id,
                    EscrowRecord {
                        trade_id: fill.trade_id,
                        market_id: market.id,
                        buyer_id,
                        seller_id,
                        qty: fill.qty,
                        notional,
                        fee,
                        fee_payer: taker_user,
                        state: EscrowState::PendingDelivery,
                        created_at_ms: self.current_time_ms,
                        deadline_ms: self.current_time_ms.saturating_add(ESCROW_TIMEOUT_MS),
                        delivered_at_ms: None,
                        seller_public_key,
                    },
                );
            } else if self.ledger.settle_trade(settlement).is_err() {
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
        side: Side,
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
        // Passes bought through escrow are in the system escrow account, not the
        // buyer's wallet yet, but they still consume the identity's holding limit.
        let escrowed: u64 = self
            .escrow
            .values()
            .filter(|record| {
                record.market_id == market_id
                    && record.buyer_id == user_id
                    && record.state == EscrowState::PendingDelivery
            })
            .map(|record| record.qty)
            .sum();
        let projected = available
            .saturating_add(held)
            .saturating_add(open)
            .saturating_add(escrowed);
        // A sell moves an already-owned pass into an order; it does not add a new
        // pass to the identity's exposure. Only buys consume new holding capacity.
        let projected = match side {
            Side::Buy => projected.saturating_add(qty),
            Side::Sell => projected,
        };
        projected <= limit as u64
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
        let journals = self.ledger.journal_count();
        let journal_id = self
            .ledger
            .deposit_cash(user_id, amount, client_request_id)
            .map_err(|error| match error {
                LedgerError::NonPositiveAmount => ReasonCode::InvalidQuantity,
                _ => ReasonCode::InternalError,
            })?;
        if self.ledger.journal_count() != journals {
            self.bump_state_version();
        }
        Ok(journal_id)
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
        let journals = self.ledger.journal_count();
        let journal_id = self
            .ledger
            .issue_passes(user_id, market_id, qty, client_request_id)
            .map_err(|error| match error {
                LedgerError::NonPositiveAmount => ReasonCode::InvalidQuantity,
                _ => ReasonCode::InternalError,
            })?;
        if self.ledger.journal_count() != journals {
            self.bump_state_version();
        }
        Ok(journal_id)
    }

    /// Register the seller key used to verify delivery receipts. A key rotation
    /// only affects future fills; each escrow record keeps the key captured at fill.
    pub fn register_public_key(
        &mut self,
        user_id: UserId,
        public_key: [u8; 32],
    ) -> Result<(), ReasonCode> {
        if public_key == [0u8; 32] || VerifyingKey::from_bytes(&public_key).is_err() {
            return Err(ReasonCode::InvalidReceipt);
        }
        if self.public_keys.get(&user_id) == Some(&public_key) {
            return Ok(());
        }
        self.public_keys.insert(user_id, public_key);
        self.bump_state_version();
        Ok(())
    }

    pub fn public_key(&self, user_id: UserId) -> Option<[u8; 32]> {
        self.public_keys.get(&user_id).copied()
    }

    pub fn escrow_record(&self, trade_id: TradeId) -> Option<&EscrowRecord> {
        self.escrow.get(&trade_id)
    }

    /// Escrow records where `user_id` is the buyer or seller, oldest first.
    pub fn escrow_records(&self, user_id: UserId) -> Vec<EscrowRecord> {
        let mut records: Vec<_> = self
            .escrow
            .values()
            .filter(|record| record.buyer_id == user_id || record.seller_id == user_id)
            .cloned()
            .collect();
        records.sort_by_key(|record| record.trade_id);
        records
    }

    /// Release escrow after verifying the seller's signed delivery receipt.
    pub fn release_escrow(
        &mut self,
        actor_user_id: UserId,
        trade_id: TradeId,
        delivered_at_ms: u64,
        signature: &[u8],
        now_ms: u64,
    ) -> Result<EscrowRecord, ReasonCode> {
        let mut record = self
            .escrow
            .get(&trade_id)
            .cloned()
            .ok_or(ReasonCode::OrderNotFound)?;
        if record.seller_id != actor_user_id {
            return Err(ReasonCode::NotOrderOwner);
        }
        if record.state != EscrowState::PendingDelivery {
            return Err(ReasonCode::EscrowNotReady);
        }
        if delivered_at_ms < record.created_at_ms
            || delivered_at_ms > record.deadline_ms
            || now_ms > record.deadline_ms
        {
            return Err(ReasonCode::ReceiptExpired);
        }

        let receipt = DeliveryReceipt {
            trade_id: record.trade_id,
            market_id: record.market_id,
            qty: record.qty,
            buyer_id: record.buyer_id,
            seller_id: record.seller_id,
            delivered_at_ms,
        };
        let verifying_key = VerifyingKey::from_bytes(&record.seller_public_key)
            .map_err(|_| ReasonCode::InvalidReceipt)?;
        let signature = Signature::from_slice(signature).map_err(|_| ReasonCode::InvalidReceipt)?;
        verifying_key
            .verify_strict(&receipt.canonical_bytes(), &signature)
            .map_err(|_| ReasonCode::InvalidReceipt)?;

        if self
            .ledger
            .release_escrow(escrow_settlement(&record))
            .is_err()
        {
            self.poisoned = true;
            return Err(ReasonCode::InternalError);
        }
        record.state = EscrowState::Delivered;
        record.delivered_at_ms = Some(delivered_at_ms);
        self.escrow.insert(trade_id, record.clone());
        self.bump_state_version();
        Ok(record)
    }

    /// Refund an escrow after its deadline. Either the buyer or seller may trigger it.
    pub fn refund_escrow(
        &mut self,
        actor_user_id: UserId,
        trade_id: TradeId,
        now_ms: u64,
    ) -> Result<EscrowRecord, ReasonCode> {
        let mut record = self
            .escrow
            .get(&trade_id)
            .cloned()
            .ok_or(ReasonCode::OrderNotFound)?;
        if actor_user_id != record.buyer_id && actor_user_id != record.seller_id {
            return Err(ReasonCode::NotOrderOwner);
        }
        if record.state != EscrowState::PendingDelivery {
            return Err(ReasonCode::EscrowNotReady);
        }
        if now_ms < record.deadline_ms {
            return Err(ReasonCode::EscrowNotReady);
        }
        if self
            .ledger
            .refund_escrow(escrow_settlement(&record))
            .is_err()
        {
            self.poisoned = true;
            return Err(ReasonCode::InternalError);
        }
        record.state = EscrowState::Refunded;
        self.escrow.insert(trade_id, record.clone());
        self.bump_state_version();
        Ok(record)
    }

    pub fn balance(&self, user_id: UserId, asset: Asset) -> Balance {
        self.ledger.balance(user_id, asset)
    }

    pub fn balances(&self, user_id: UserId) -> Vec<Balance> {
        self.ledger.balances(user_id)
    }

    /// Per-user total liabilities by asset (`available + held`) for solvency
    /// snapshots. System accounts and `Mint` are excluded: the published root
    /// covers user claims, not the backing accounts.
    pub fn solvency_balances(&self) -> Result<Vec<(UserId, Asset, u64)>, String> {
        let mut balances = Vec::new();
        for (key, account) in self.ledger.accounts() {
            let Owner::User(user_id) = key.owner else {
                continue;
            };
            let total = account
                .available
                .checked_add(account.held)
                .ok_or_else(|| format!("balance overflow for user {user_id}"))?;
            let total =
                u64::try_from(total).map_err(|_| format!("negative balance for user {user_id}"))?;
            balances.push((user_id, key.asset, total));
        }
        balances.sort_by_key(|(user_id, asset, _)| (*user_id, *asset));
        Ok(balances)
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

    /// Monotonic version covering commands and journal-posting operations.
    ///
    /// This is the identifier used by solvency snapshots. It advances on deposits
    /// and issuance even though those operations do not consume `last_seq`.
    pub fn state_version(&self) -> u64 {
        self.state_version
    }

    /// Set the gateway timestamp used for new escrow deadlines. The WAL stores the
    /// same value with each command, so replay produces the same deadlines.
    pub fn set_current_time_ms(&mut self, now_ms: u64) {
        self.current_time_ms = now_ms;
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

        let mut escrow: Vec<_> = self.escrow.values().collect();
        escrow.sort_by_key(|record| record.trade_id);
        for record in escrow {
            hasher.update(record.trade_id.to_le_bytes());
            hasher.update(record.market_id.to_le_bytes());
            hasher.update(record.buyer_id.to_le_bytes());
            hasher.update(record.seller_id.to_le_bytes());
            hasher.update(record.qty.to_le_bytes());
            hasher.update(record.notional.to_le_bytes());
            hasher.update(record.fee.to_le_bytes());
            hasher.update(record.fee_payer.to_le_bytes());
            hasher.update([match record.state {
                EscrowState::PendingDelivery => 0,
                EscrowState::Delivered => 1,
                EscrowState::Refunded => 2,
            }]);
            hasher.update(record.created_at_ms.to_le_bytes());
            hasher.update(record.deadline_ms.to_le_bytes());
            match record.delivered_at_ms {
                Some(ts) => {
                    hasher.update([1]);
                    hasher.update(ts.to_le_bytes());
                }
                None => hasher.update([0]),
            }
            hasher.update(record.seller_public_key);
        }

        let mut public_keys: Vec<_> = self.public_keys.iter().collect();
        public_keys.sort_by_key(|(user_id, _)| **user_id);
        for (user_id, key) in public_keys {
            hasher.update(user_id.to_le_bytes());
            hasher.update(key);
        }
        hasher.update(self.next_seq.to_le_bytes());
        hasher.update(self.state_version.to_le_bytes());
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

        let mut expected_escrow_cash: Minor = 0;
        let mut expected_escrow_pass: HashMap<MarketId, i64> = HashMap::new();
        for record in self
            .escrow
            .values()
            .filter(|record| record.state == EscrowState::PendingDelivery)
        {
            let buyer_fee = if record.fee_payer == record.buyer_id {
                record.fee
            } else {
                0
            };
            expected_escrow_cash = expected_escrow_cash
                .checked_add(record.notional)
                .and_then(|value| value.checked_add(buyer_fee))
                .ok_or_else(|| "expected escrow cash overflow".to_string())?;
            let qty = i64::try_from(record.qty)
                .map_err(|_| "escrow pass quantity overflow".to_string())?;
            let entry = expected_escrow_pass.entry(record.market_id).or_default();
            *entry = entry
                .checked_add(qty)
                .ok_or_else(|| "expected escrow pass overflow".to_string())?;
        }
        let escrow_cash = self.ledger.system_balance(Owner::Escrow, Asset::Cash);
        if escrow_cash != expected_escrow_cash {
            return Err(format!(
                "escrow cash mismatch: ledger {escrow_cash} expected {expected_escrow_cash}"
            ));
        }
        for (key, account) in self.ledger.accounts() {
            if key.owner != Owner::Escrow {
                continue;
            }
            let Asset::Pass { market_id } = key.asset else {
                // Cash is checked above.
                continue;
            };
            let actual = account.available.saturating_add(account.held);
            let expected = expected_escrow_pass.remove(&market_id).unwrap_or(0);
            if actual != expected {
                return Err(format!(
                    "escrow pass mismatch for market {market_id}: ledger {actual} expected {expected}"
                ));
            }
        }
        for (market_id, expected) in expected_escrow_pass {
            if expected != 0 {
                return Err(format!(
                    "escrow pass mismatch for market {market_id}: ledger 0 expected {expected}"
                ));
            }
        }

        Ok(())
    }
}

fn escrow_settlement(record: &EscrowRecord) -> TradeSettlement {
    TradeSettlement {
        trade_id: record.trade_id,
        market_id: record.market_id,
        buyer_id: record.buyer_id,
        seller_id: record.seller_id,
        qty: record.qty,
        notional: record.notional,
        fee: record.fee,
        fee_payer: record.fee_payer,
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
