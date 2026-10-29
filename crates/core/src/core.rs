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
    AcceptDutchAuction, AskRecord, Asset, AuctionAccept, AuctionStatus, Balance, BatchAuction,
    BatchAuctionFill, BatchAuctionPhase, BidRecord, BookDelta, BookSnapshot, Cancel, ClientOrderId,
    Command, CommandResponse, CommitAsk, CommitBid, CreateBatchAuction, CreateDutchAuction,
    DeliveryReceipt, DutchAuction, EscrowRecord, EscrowState, Fill, MarketConfig, MarketId,
    MarketStatus, Minor, OrderStatus, PlaceLimit, PlaceMarket, Qty, ReasonCode, RevealAsk,
    RevealBid, Seq, SettleBatchAuction, Side, Ticks, TradeId, UserId,
};
use ed25519_dalek::{Signature, VerifyingKey};
use sha2::{Digest, Sha256};

pub mod snapshot;

/// How long a buyer's cash and a seller's passes stay in escrow before a refund
/// can be requested. Kept short for the demo; production would use a market-level
/// policy.
pub const ESCROW_TIMEOUT_MS: u64 = 5 * 60 * 1000;

/// Upper bound on a Dutch-auction window. Keeps the published countdown sane and
/// stops a typo from reserving inventory for years.
pub const MAX_DUTCH_AUCTION_MS: u64 = 7 * 24 * 60 * 60 * 1000;

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
    /// Dutch auctions by id. Seller passes stay reserved until a sale or expiry.
    auctions: HashMap<u64, DutchAuction>,
    next_auction_id: u64,
    /// Accepted auction slices, keyed by `(buyer, client_order_id)`, so a retried
    /// accept returns the original settlement instead of buying twice.
    auction_accepts: HashMap<(UserId, ClientOrderId), AuctionAccept>,
    /// Auction ids keyed by `(seller, client_order_id)`, so a retried create does
    /// not reserve the seller's passes twice.
    auction_creates: HashMap<(UserId, ClientOrderId), u64>,
    /// Batch auctions by id.
    batch_auctions: HashMap<u64, BatchAuction>,
    next_batch_auction_id: u64,
    /// Bids submitted for batch auctions: (auction_id, bidder_id, client_order_id) -> BidRecord.
    batch_bids: HashMap<(u64, UserId, ClientOrderId), BidRecord>,
    /// Asks submitted for batch auctions: (auction_id, seller_id, client_order_id) -> AskRecord.
    batch_asks: HashMap<(u64, UserId, ClientOrderId), AskRecord>,
    /// Commit idempotency: (bidder, client_order_id) -> (auction_id, commitment)
    batch_commits: HashMap<(UserId, ClientOrderId), u64>,
    /// Ask-commit idempotency: (seller, client_order_id) -> auction_id
    batch_ask_commits: HashMap<(UserId, ClientOrderId), u64>,
    /// Reveal idempotency: (bidder, client_order_id) -> auction_id
    batch_reveals: HashMap<(UserId, ClientOrderId), u64>,
    /// Ask-reveal idempotency: (seller, client_order_id) -> auction_id
    batch_ask_reveals: HashMap<(UserId, ClientOrderId), u64>,
    /// Batch auction creates idempotency: (creator, client_order_id) -> auction_id
    batch_creates: HashMap<(UserId, ClientOrderId), u64>,
    /// Batch settlements: (auction_id) -> Vec<BatchAuctionFill>
    batch_settlements: HashMap<u64, Vec<BatchAuctionFill>>,
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

/// A batch settlement plan: each winning bid with the ask fills assigned to
/// it, in deterministic descending-bid / ascending-ask order.
type BatchAllocations = Vec<(BidRecord, Vec<((UserId, ClientOrderId), Qty)>)>;

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
            auctions: HashMap::new(),
            next_auction_id: 1,
            auction_accepts: HashMap::new(),
            auction_creates: HashMap::new(),
            batch_auctions: HashMap::new(),
            next_batch_auction_id: 1,
            batch_bids: HashMap::new(),
            batch_asks: HashMap::new(),
            batch_commits: HashMap::new(),
            batch_ask_commits: HashMap::new(),
            batch_reveals: HashMap::new(),
            batch_ask_reveals: HashMap::new(),
            batch_creates: HashMap::new(),
            batch_settlements: HashMap::new(),
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
            match self.settle_fills(seq, user_id, place.side, &market, &outcome, place.escrow) {
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
            match self.settle_fills(seq, user_id, place.side, &market, &outcome, place.escrow) {
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
        escrow_requested: bool,
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
            // Escrow only applies when the market supports it AND the taker requested it
            let use_escrow = escrow_requested && market.escrow_enabled;
            let settlement = TradeSettlement {
                trade_id: fill.trade_id,
                market_id: market.id,
                buyer_id,
                seller_id,
                qty: fill.qty,
                notional,
                fee,
                fee_payer: taker_user,
                escrow: use_escrow,
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
                escrow_id: if use_escrow {
                    Some(fill.trade_id)
                } else {
                    None
                },
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
        let code = ledger_reason(error, asset);
        if code == ReasonCode::InternalError {
            self.poisoned = true;
        }
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

    /// All escrow records currently tracked by the core.
    pub fn all_escrow_records(&self) -> Vec<EscrowRecord> {
        let mut records: Vec<_> = self.escrow.values().cloned().collect();
        records.sort_by_key(|record| record.trade_id);
        records
    }

    /// Journal entries with id greater than `after_id` (ascending). Convenience
    /// accessor for the PostgreSQL projection feed (row 21).
    pub fn journals_since(&self, after_id: u64) -> Vec<dx_ledger::JournalEntry> {
        self.ledger.journals_since(after_id)
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

    /// Admin resolution of a disputed escrow. Either refunds buyer or releases to seller.
    pub fn admin_settle_escrow(
        &mut self,
        trade_id: TradeId,
        refund_to_buyer: bool,
    ) -> Result<EscrowRecord, ReasonCode> {
        let mut record = self
            .escrow
            .get(&trade_id)
            .cloned()
            .ok_or(ReasonCode::OrderNotFound)?;
        if record.state != EscrowState::PendingDelivery {
            return Err(ReasonCode::EscrowNotReady);
        }

        if refund_to_buyer {
            if self
                .ledger
                .refund_escrow(escrow_settlement(&record))
                .is_err()
            {
                self.poisoned = true;
                return Err(ReasonCode::InternalError);
            }
            record.state = EscrowState::Refunded;
        } else {
            if self
                .ledger
                .release_escrow(escrow_settlement(&record))
                .is_err()
            {
                self.poisoned = true;
                return Err(ReasonCode::InternalError);
            }
            record.state = EscrowState::Delivered;
            if record.delivered_at_ms.is_none() {
                record.delivered_at_ms = Some(self.current_time_ms);
            }
        }

        self.escrow.insert(trade_id, record.clone());
        self.bump_state_version();
        Ok(record)
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

    /// Create a seller-backed Dutch auction. The seller's passes are reserved now
    /// and released only when the auction sells out or expires.
    pub fn create_dutch_auction(
        &mut self,
        seller_id: UserId,
        request: CreateDutchAuction,
        now_ms: u64,
    ) -> Result<DutchAuction, ReasonCode> {
        let create_key = (seller_id, request.client_order_id);
        if let Some(auction_id) = self.auction_creates.get(&create_key).copied()
            && let Some(existing) = self.auctions.get(&auction_id)
        {
            return Ok(existing.clone());
        }

        if request.qty == 0 {
            return Err(ReasonCode::InvalidQuantity);
        }
        if request.duration_ms == 0 || request.duration_ms > MAX_DUTCH_AUCTION_MS {
            return Err(ReasonCode::InvalidRequest);
        }
        let Some(market) = self.markets.get(&request.market_id).cloned() else {
            return Err(ReasonCode::UnknownMarket);
        };
        if market.status != MarketStatus::Open {
            return Err(ReasonCode::MarketClosed);
        }
        if request.start_price_ticks <= request.floor_price_ticks {
            return Err(ReasonCode::InvalidPrice);
        }
        if !market.price_in_collar(request.start_price_ticks)
            || !market.price_in_collar(request.floor_price_ticks)
        {
            return Err(ReasonCode::OutsideCollar);
        }

        let Ok(qty) = i64::try_from(request.qty) else {
            return Err(ReasonCode::InvalidQuantity);
        };
        if let Err(error) = self.ledger.reserve(
            seller_id,
            Asset::Pass {
                market_id: market.id,
            },
            qty,
        ) {
            return Err(ledger_reason(
                &error,
                Asset::Pass {
                    market_id: market.id,
                },
            ));
        }

        let auction = DutchAuction {
            id: self.next_auction_id,
            market_id: market.id,
            seller_id,
            qty_total: request.qty,
            qty_remaining: request.qty,
            start_price_ticks: request.start_price_ticks,
            floor_price_ticks: request.floor_price_ticks,
            start_ms: now_ms,
            end_ms: now_ms.saturating_add(request.duration_ms),
            status: AuctionStatus::Active,
        };
        self.next_auction_id = self.next_auction_id.saturating_add(1);
        self.auctions.insert(auction.id, auction.clone());
        self.auction_creates.insert(create_key, auction.id);
        self.bump_state_version();
        Ok(auction)
    }

    /// Take a slice of an active auction at the current decayed price. The price
    /// is recomputed here from `now_ms`; the buyer's `max_price_ticks` is the only
    /// price they authorise.
    pub fn accept_dutch_auction(
        &mut self,
        buyer_id: UserId,
        request: AcceptDutchAuction,
        now_ms: u64,
    ) -> Result<AuctionAccept, ReasonCode> {
        let accept_key = (buyer_id, request.client_order_id);
        if let Some(cached) = self.auction_accepts.get(&accept_key) {
            return Ok(cached.clone());
        }

        let Some(mut auction) = self.auctions.get(&request.auction_id).cloned() else {
            return Err(ReasonCode::OrderNotFound);
        };
        if auction.status != AuctionStatus::Active || auction.qty_remaining == 0 {
            return Err(ReasonCode::AuctionClosed);
        }
        if now_ms >= auction.end_ms {
            return Err(ReasonCode::AuctionClosed);
        }
        if request.qty == 0 || request.qty > auction.qty_remaining {
            return Err(ReasonCode::InvalidQuantity);
        }
        let price_ticks = auction.price_at(now_ms);
        if price_ticks > request.max_price_ticks {
            return Err(ReasonCode::StaleAuctionPrice);
        }
        if buyer_id == auction.seller_id {
            return Err(ReasonCode::SelfTrade);
        }
        let Some(market) = self.markets.get(&auction.market_id).cloned() else {
            self.poisoned = true;
            return Err(ReasonCode::InternalError);
        };
        if !self.within_holding_limit(
            buyer_id,
            market.id,
            Side::Buy,
            request.qty,
            market.holding_limit,
        ) {
            return Err(ReasonCode::HoldingLimitExceeded);
        }

        let Some(notional) = market.notional_minor(price_ticks, request.qty) else {
            return Err(ReasonCode::InvalidQuantity);
        };
        let fee = market.taker_fee_minor(notional);
        let Some(cash) = notional.checked_add(fee) else {
            self.poisoned = true;
            return Err(ReasonCode::InternalError);
        };
        if let Err(error) = self.ledger.reserve(buyer_id, Asset::Cash, cash) {
            return Err(ledger_reason(&error, Asset::Cash));
        }

        let trade_id = self.engine.alloc_trade_id();
        let settlement = TradeSettlement {
            trade_id,
            market_id: market.id,
            buyer_id,
            seller_id: auction.seller_id,
            qty: request.qty,
            notional,
            fee,
            fee_payer: buyer_id,
            escrow: false,
        };
        if self.ledger.settle_trade(settlement).is_err() {
            self.poisoned = true;
            return Err(ReasonCode::InternalError);
        }

        auction.qty_remaining -= request.qty;
        if auction.qty_remaining == 0 {
            auction.status = AuctionStatus::Sold;
        }
        self.auctions.insert(auction.id, auction);
        let accepted = AuctionAccept {
            auction_id: request.auction_id,
            trade_id,
            market_id: market.id,
            buyer_id,
            seller_id: settlement.seller_id,
            qty: request.qty,
            price_ticks,
            notional,
            fee,
        };
        self.auction_accepts.insert(accept_key, accepted.clone());
        self.bump_state_version();
        Ok(accepted)
    }

    /// Close an auction after its deadline and return unsold passes to the seller.
    /// Safe to retry once the auction is already expired.
    pub fn expire_dutch_auction(
        &mut self,
        actor_user_id: UserId,
        auction_id: u64,
        now_ms: u64,
    ) -> Result<DutchAuction, ReasonCode> {
        let Some(mut auction) = self.auctions.get(&auction_id).cloned() else {
            return Err(ReasonCode::OrderNotFound);
        };
        if auction.status == AuctionStatus::Expired {
            return Ok(auction);
        }
        if auction.status != AuctionStatus::Active || now_ms < auction.end_ms {
            return Err(ReasonCode::AuctionNotActive);
        }
        if actor_user_id != auction.seller_id {
            return Err(ReasonCode::NotOrderOwner);
        }
        if auction.qty_remaining > 0 {
            self.release_pass(auction.seller_id, auction.market_id, auction.qty_remaining)?;
        }
        auction.status = AuctionStatus::Expired;
        self.auctions.insert(auction_id, auction.clone());
        self.bump_state_version();
        Ok(auction)
    }

    pub fn dutch_auction(&self, auction_id: u64) -> Option<&DutchAuction> {
        self.auctions.get(&auction_id)
    }

    /// All auctions, oldest first, optionally filtered by market.
    pub fn dutch_auctions(&self, market_id: Option<MarketId>) -> Vec<DutchAuction> {
        let mut auctions: Vec<_> = self
            .auctions
            .values()
            .filter(|auction| market_id.is_none_or(|id| auction.market_id == id))
            .cloned()
            .collect();
        auctions.sort_by_key(|auction| auction.id);
        auctions
    }

    // -----------------------------------------------------------------------
    // Batch Auction (Commit-Reveal)
    // -----------------------------------------------------------------------

    /// Schedule a pooled batch window for one market. No inventory is reserved
    /// here — sellers enter sealed asks during the commit phase, and their
    /// passes are reserved when they reveal.
    pub fn create_batch_auction(
        &mut self,
        created_by: UserId,
        request: CreateBatchAuction,
        now_ms: u64,
    ) -> Result<BatchAuction, ReasonCode> {
        let create_key = (created_by, request.client_order_id);
        if let Some(auction_id) = self.batch_creates.get(&create_key).copied()
            && let Some(existing) = self.batch_auctions.get(&auction_id)
        {
            return Ok(existing.clone());
        }

        if request.commit_duration_ms == 0 || request.reveal_duration_ms == 0 {
            return Err(ReasonCode::InvalidRequest);
        }
        let Some(market) = self.markets.get(&request.market_id).cloned() else {
            return Err(ReasonCode::UnknownMarket);
        };
        if market.status != MarketStatus::Open {
            return Err(ReasonCode::MarketClosed);
        }
        if !market.price_in_collar(request.floor_price_ticks) {
            return Err(ReasonCode::OutsideCollar);
        }

        let commit_end_ms = now_ms.saturating_add(request.commit_duration_ms);
        let reveal_end_ms = commit_end_ms.saturating_add(request.reveal_duration_ms);
        let auction = BatchAuction {
            id: self.next_batch_auction_id,
            market_id: market.id,
            created_by,
            floor_price_ticks: request.floor_price_ticks,
            commit_end_ms,
            reveal_end_ms,
            clearing_price_ticks: None,
            phase: BatchAuctionPhase::Commit,
            qty_total: None,
            qty_remaining: None,
        };
        self.next_batch_auction_id = self.next_batch_auction_id.saturating_add(1);
        self.batch_auctions.insert(auction.id, auction.clone());
        self.batch_creates.insert(create_key, auction.id);
        self.bump_state_version();
        Ok(auction)
    }

    /// Submit a bid hash commitment during the commit phase.
    pub fn commit_batch_bid(
        &mut self,
        bidder_id: UserId,
        request: CommitBid,
        now_ms: u64,
    ) -> Result<BidRecord, ReasonCode> {
        let commit_key = (bidder_id, request.client_order_id);
        if let Some(existing_auction_id) = self.batch_commits.get(&commit_key).copied()
            && let Some(bid) =
                self.batch_bids
                    .get(&(existing_auction_id, bidder_id, request.client_order_id))
        {
            return Ok(bid.clone());
        }

        let Some(auction) = self.batch_auctions.get(&request.auction_id) else {
            return Err(ReasonCode::OrderNotFound);
        };
        if auction.phase != BatchAuctionPhase::Commit || now_ms >= auction.commit_end_ms {
            return Err(ReasonCode::AuctionPhaseMismatch);
        }

        let record = BidRecord {
            bidder_id,
            client_order_id: request.client_order_id,
            commitment: request.commitment,
            price_ticks: None,
            qty: None,
            filled_qty: 0,
            valid: true,
        };

        self.batch_bids.insert(
            (auction.id, bidder_id, request.client_order_id),
            record.clone(),
        );
        self.batch_commits.insert(commit_key, auction.id);
        self.bump_state_version();
        Ok(record)
    }

    /// Submit an ask hash commitment during the commit phase (seller side).
    pub fn commit_batch_ask(
        &mut self,
        seller_id: UserId,
        request: CommitAsk,
        now_ms: u64,
    ) -> Result<AskRecord, ReasonCode> {
        let commit_key = (seller_id, request.client_order_id);
        if let Some(existing_auction_id) = self.batch_ask_commits.get(&commit_key).copied()
            && let Some(ask) =
                self.batch_asks
                    .get(&(existing_auction_id, seller_id, request.client_order_id))
        {
            return Ok(ask.clone());
        }

        let Some(auction) = self.batch_auctions.get(&request.auction_id) else {
            return Err(ReasonCode::OrderNotFound);
        };
        if auction.phase != BatchAuctionPhase::Commit || now_ms >= auction.commit_end_ms {
            return Err(ReasonCode::AuctionPhaseMismatch);
        }

        let record = AskRecord {
            seller_id,
            client_order_id: request.client_order_id,
            commitment: request.commitment,
            price_ticks: None,
            qty: None,
            filled_qty: 0,
            valid: true,
        };

        self.batch_asks.insert(
            (auction.id, seller_id, request.client_order_id),
            record.clone(),
        );
        self.batch_ask_commits.insert(commit_key, auction.id);
        self.bump_state_version();
        Ok(record)
    }

    /// Reveal the price, qty, and salt during the reveal phase.
    pub fn reveal_batch_bid(
        &mut self,
        bidder_id: UserId,
        request: RevealBid,
        now_ms: u64,
    ) -> Result<BidRecord, ReasonCode> {
        let reveal_key = (bidder_id, request.client_order_id);
        if let Some(existing_auction_id) = self.batch_reveals.get(&reveal_key).copied()
            && let Some(bid) =
                self.batch_bids
                    .get(&(existing_auction_id, bidder_id, request.client_order_id))
        {
            return Ok(bid.clone());
        }

        let Some(auction) = self.batch_auctions.get_mut(&request.auction_id) else {
            return Err(ReasonCode::OrderNotFound);
        };
        // Update phase to Reveal if past commit_end_ms
        if now_ms >= auction.commit_end_ms && now_ms < auction.reveal_end_ms {
            auction.phase = BatchAuctionPhase::Reveal;
        }
        if auction.phase != BatchAuctionPhase::Reveal {
            return Err(ReasonCode::AuctionPhaseMismatch);
        }

        let Some(bid) = self
            .batch_bids
            .get_mut(&(auction.id, bidder_id, request.client_order_id))
        else {
            return Err(ReasonCode::OrderNotFound);
        };
        if bid.client_order_id != request.client_order_id {
            return Err(ReasonCode::InvalidRequest);
        }

        // Verify commitment: sha256(price_ticks_le8 || qty_le8 || salt32)
        let preimage =
            CommitBid::commitment_preimage(request.price_ticks, request.qty, &request.salt);
        let mut hasher = Sha256::new();
        hasher.update(preimage);
        let expected_hash: [u8; 32] = hasher.finalize().into();

        if expected_hash != bid.commitment {
            bid.valid = false;
            return Err(ReasonCode::InvalidCommitment);
        }

        // Verify collar and floor price
        let Some(market) = self.markets.get(&auction.market_id) else {
            return Err(ReasonCode::InternalError);
        };
        bid.valid = market.price_in_collar(request.price_ticks)
            && request.price_ticks >= auction.floor_price_ticks;

        bid.price_ticks = Some(request.price_ticks);
        bid.qty = Some(request.qty);

        // Reserve cash for valid bid: notional + taker fee
        if bid.valid && request.qty > 0 {
            if let Some(notional) = market.notional_minor(request.price_ticks, request.qty) {
                let fee = market.taker_fee_minor(notional);
                if let Some(cash) = notional.checked_add(fee) {
                    if self.ledger.reserve(bidder_id, Asset::Cash, cash).is_err() {
                        bid.valid = false;
                    }
                } else {
                    bid.valid = false;
                }
            } else {
                bid.valid = false;
            }
        }

        let result = bid.clone();
        self.batch_reveals.insert(reveal_key, auction.id);
        self.bump_state_version();
        Ok(result)
    }

    /// Reveal an ask pre-image during the reveal phase (seller side).
    ///
    /// A matching, priced ask reserves the seller's passes right away, so a
    /// fair settlement never needs passes the seller does not have. A short
    /// inventory silently marks the ask invalid — the same treatment an
    /// unaffordable cash reservation gets on the bid side.
    pub fn reveal_batch_ask(
        &mut self,
        seller_id: UserId,
        request: RevealAsk,
        now_ms: u64,
    ) -> Result<AskRecord, ReasonCode> {
        let reveal_key = (seller_id, request.client_order_id);
        if let Some(existing_auction_id) = self.batch_ask_reveals.get(&reveal_key).copied()
            && let Some(ask) =
                self.batch_asks
                    .get(&(existing_auction_id, seller_id, request.client_order_id))
        {
            return Ok(ask.clone());
        }

        let Some(auction) = self.batch_auctions.get_mut(&request.auction_id) else {
            return Err(ReasonCode::OrderNotFound);
        };
        // Update phase to Reveal if past commit_end_ms
        if now_ms >= auction.commit_end_ms && now_ms < auction.reveal_end_ms {
            auction.phase = BatchAuctionPhase::Reveal;
        }
        if auction.phase != BatchAuctionPhase::Reveal {
            return Err(ReasonCode::AuctionPhaseMismatch);
        }

        let Some(ask) = self
            .batch_asks
            .get_mut(&(auction.id, seller_id, request.client_order_id))
        else {
            return Err(ReasonCode::OrderNotFound);
        };
        if ask.client_order_id != request.client_order_id {
            return Err(ReasonCode::InvalidRequest);
        }

        // Verify commitment: sha256(price_ticks_le8 || qty_le8 || salt32)
        let preimage =
            CommitBid::commitment_preimage(request.price_ticks, request.qty, &request.salt);
        let mut hasher = Sha256::new();
        hasher.update(preimage);
        let expected_hash: [u8; 32] = hasher.finalize().into();

        if expected_hash != ask.commitment {
            ask.valid = false;
            return Err(ReasonCode::InvalidCommitment);
        }

        // Verify collar and floor price
        let Some(market) = self.markets.get(&auction.market_id) else {
            return Err(ReasonCode::InternalError);
        };
        ask.valid = market.price_in_collar(request.price_ticks)
            && request.price_ticks >= auction.floor_price_ticks;

        ask.price_ticks = Some(request.price_ticks);
        ask.qty = Some(request.qty);

        // Reserve the seller's passes for valid asks
        if ask.valid && request.qty > 0 {
            let Ok(qty) = i64::try_from(request.qty) else {
                ask.valid = false;
                return Ok(ask.clone());
            };
            if self
                .ledger
                .reserve(
                    seller_id,
                    Asset::Pass {
                        market_id: auction.market_id,
                    },
                    qty,
                )
                .is_err()
            {
                ask.valid = false;
            }
        } else {
            ask.valid = false;
        }

        let result = ask.clone();
        self.batch_ask_reveals.insert(reveal_key, auction.id);
        self.bump_state_version();
        Ok(result)
    }

    /// Settle the batch auction at a single uniform price after reveal_end_ms.
    ///
    /// The clearing price maximises executable volume (highest such price);
    /// winning bids are filled from the cheapest asks in deterministic order.
    /// Bids pay `uniform_price * qty` plus the taker fee, sellers receive
    /// `uniform_price * qty`, and every reservation is unwound exactly:
    /// winners get their over-reservation released, losers the whole amount,
    /// sellers the unsold part of their reserved passes.
    pub fn settle_batch_auction(
        &mut self,
        _caller_id: UserId,
        request: SettleBatchAuction,
        now_ms: u64,
    ) -> Result<Vec<BatchAuctionFill>, ReasonCode> {
        if let Some(existing_fills) = self.batch_settlements.get(&request.auction_id) {
            return Ok(existing_fills.clone());
        }

        let Some(auction) = self.batch_auctions.get_mut(&request.auction_id) else {
            return Err(ReasonCode::OrderNotFound);
        };
        if now_ms < auction.reveal_end_ms {
            return Err(ReasonCode::AuctionPhaseMismatch);
        }
        if auction.phase == BatchAuctionPhase::Settled
            || auction.phase == BatchAuctionPhase::Expired
        {
            return Ok(self
                .batch_settlements
                .get(&request.auction_id)
                .cloned()
                .unwrap_or_default());
        }

        auction.phase = BatchAuctionPhase::Clearing;
        let market_id = auction.market_id;
        let Some(market) = self.markets.get(&market_id).cloned() else {
            return Err(ReasonCode::InternalError);
        };

        // Gather valid revealed orders on both sides of this window.
        let auction_id = auction.id;
        let mut valid_bids: Vec<_> = self
            .batch_bids
            .iter()
            .filter(|((id, _, _), b)| {
                *id == auction_id
                    && b.valid
                    && b.price_ticks.is_some()
                    && b.qty.is_some()
                    && b.qty.unwrap() > 0
            })
            .map(|(_, b)| b.clone())
            .collect();
        let mut valid_asks: Vec<_> = self
            .batch_asks
            .iter()
            .filter(|((id, _, _), a)| {
                *id == auction_id
                    && a.valid
                    && a.price_ticks.is_some()
                    && a.qty.is_some()
                    && a.qty.unwrap() > 0
            })
            .map(|(_, a)| a.clone())
            .collect();

        // Sort bids by price descending; asks by price ascending; ties broken
        // by client_order_id ascending (deterministic on both sides).
        valid_bids.sort_by(|a, b| {
            b.price_ticks
                .unwrap()
                .cmp(&a.price_ticks.unwrap())
                .then_with(|| a.client_order_id.cmp(&b.client_order_id))
        });
        valid_asks.sort_by(|a, b| {
            a.price_ticks
                .unwrap()
                .cmp(&b.price_ticks.unwrap())
                .then_with(|| a.client_order_id.cmp(&b.client_order_id))
        });

        let total_revealed_supply: Qty = valid_asks.iter().map(|a| a.qty.unwrap()).sum();

        // No counterpart on at least one side: expire the window.
        if valid_bids.is_empty() || valid_asks.is_empty() {
            self.expire_batch_auction(auction_id, &market, &valid_bids, &valid_asks);
            self.batch_settlements.insert(auction_id, Vec::new());
            self.bump_state_version();
            return Ok(Vec::new());
        }

        // Determine the uniform clearing price by testing each candidate
        // price (the reveal prices of both sides, descending) with a dry
        // pairing run: winning bids (price >= candidate) fill from the
        // qualifying asks (price <= candidate, ascending, cheapest first), a
        // user's bid never fills their own ask. The winning candidate is the
        // one whose dry run maximises *executable* volume, with the higher
        // price preferred on ties (seller-favourable). Testing with the real
        // pairing rules keeps the selection consistent with what is later
        // settled, so a price whose only trades would be self-trades never
        // wins.
        let mut candidates: Vec<_> = valid_bids
            .iter()
            .map(|b| b.price_ticks.unwrap())
            .chain(valid_asks.iter().map(|a| a.price_ticks.unwrap()))
            .collect();
        candidates.sort_unstable();
        candidates.dedup();
        candidates.reverse();

        let mut best_volume: Qty = 0;
        let mut best_price: Option<Ticks> = None;
        let mut best_allocations: BatchAllocations = Vec::new();
        let mut best_ask_remaining: HashMap<(UserId, ClientOrderId), Qty> = HashMap::new();
        for &candidate in &candidates {
            // Dry pairing at `candidate`.
            let mut ask_remaining: HashMap<(UserId, ClientOrderId), Qty> = HashMap::new();
            for ask in &valid_asks {
                if ask.price_ticks.unwrap() <= candidate {
                    ask_remaining.insert((ask.seller_id, ask.client_order_id), ask.qty.unwrap());
                }
            }
            let mut allocations: BatchAllocations = Vec::new();
            let mut volume: Qty = 0;
            for bid in &valid_bids {
                if bid.price_ticks.unwrap() < candidate {
                    continue;
                }
                let mut pairs: Vec<((UserId, ClientOrderId), Qty)> = Vec::new();
                let mut bid_left = bid.qty.unwrap();
                for ask in &valid_asks {
                    if bid_left == 0 {
                        break;
                    }
                    if ask.price_ticks.unwrap() > candidate {
                        break;
                    }
                    let ask_left =
                        match ask_remaining.get_mut(&(ask.seller_id, ask.client_order_id)) {
                            Some(left) if *left > 0 => *left,
                            _ => continue,
                        };
                    if ask.seller_id == bid.bidder_id {
                        continue;
                    }
                    let qty = bid_left.min(ask_left);
                    let key = (ask.seller_id, ask.client_order_id);
                    pairs.push((key, qty));
                    *ask_remaining.get_mut(&key).unwrap() -= qty;
                    bid_left -= qty;
                    volume += qty;
                }
                allocations.push((bid.clone(), pairs));
            }
            if volume > best_volume {
                best_volume = volume;
                best_price = Some(candidate);
                best_allocations = allocations;
                best_ask_remaining = ask_remaining;
            }
        }

        // No candidate produces an executable trade: expire the window.
        if best_price.is_none() {
            self.expire_batch_auction(auction_id, &market, &valid_bids, &valid_asks);
            self.batch_settlements.insert(auction_id, Vec::new());
            self.bump_state_version();
            return Ok(Vec::new());
        }
        let uniform_price = best_price.unwrap();
        let allocations = best_allocations;
        let ask_remaining = best_ask_remaining;

        // Execute the fills.
        let mut filled_by_bid: HashMap<(UserId, ClientOrderId), Qty> = HashMap::new();
        let mut fills: Vec<BatchAuctionFill> = Vec::new();
        for (bid, pairs) in &allocations {
            if pairs.is_empty() {
                continue;
            }
            let mut filled: Qty = 0;
            for ((seller_id, ask_order_id), qty) in pairs {
                let Some(notional) = market.notional_minor(uniform_price, *qty) else {
                    continue;
                };
                let fee = market.taker_fee_minor(notional);
                let trade_id = self.engine.alloc_trade_id();
                let settlement = TradeSettlement {
                    trade_id,
                    market_id,
                    buyer_id: bid.bidder_id,
                    seller_id: *seller_id,
                    qty: *qty,
                    notional,
                    fee,
                    fee_payer: bid.bidder_id,
                    escrow: false,
                };
                if self.ledger.settle_trade(settlement).is_err() {
                    continue;
                }
                fills.push(BatchAuctionFill {
                    auction_id,
                    trade_id,
                    market_id,
                    buyer_id: bid.bidder_id,
                    seller_id: *seller_id,
                    qty: *qty,
                    price_ticks: uniform_price,
                    notional,
                    fee,
                });
                filled += qty;
                if let Some(stored_ask) =
                    self.batch_asks
                        .get_mut(&(auction_id, *seller_id, *ask_order_id))
                {
                    stored_ask.filled_qty += qty;
                }
            }
            filled_by_bid.insert((bid.bidder_id, bid.client_order_id), filled);
            if filled > 0
                && let Some(stored_bid) =
                    self.batch_bids
                        .get_mut(&(auction_id, bid.bidder_id, bid.client_order_id))
            {
                stored_bid.filled_qty = filled;
            }
        }

        // Return whatever the pool did not buy to each seller.
        let mut unsold_total: Qty = 0;
        for ask in &valid_asks {
            let remaining = if ask.price_ticks.unwrap() > uniform_price {
                // Never qualified for clearing: fully unsold.
                ask.qty.unwrap()
            } else {
                ask_remaining
                    .get(&(ask.seller_id, ask.client_order_id))
                    .copied()
                    .unwrap_or(ask.qty.unwrap())
            };
            if remaining > 0 {
                let _ = self.release_pass(ask.seller_id, market_id, remaining);
            }
            unsold_total = unsold_total.saturating_add(remaining);
        }

        // Unwind every bid reservation: winners release the part of their
        // reservation the pool did not consume (they reserved at their bid
        // price for the whole bid, but the trade settled the filled part at
        // the uniform price); losers release the whole reservation.
        for bid in &valid_bids {
            let Some(reserved) = market.notional_minor(bid.price_ticks.unwrap(), bid.qty.unwrap())
            else {
                continue;
            };
            let reserved_cash = reserved
                .checked_add(market.taker_fee_minor(reserved))
                .unwrap_or(reserved);
            let filled = filled_by_bid
                .get(&(bid.bidder_id, bid.client_order_id))
                .copied()
                .unwrap_or(0);
            if filled == 0 {
                let _ = self
                    .ledger
                    .release(bid.bidder_id, Asset::Cash, reserved_cash);
            } else if let Some(consumed) = market.notional_minor(uniform_price, filled) {
                let consumed_cash = consumed
                    .checked_add(market.taker_fee_minor(consumed))
                    .unwrap_or(consumed);
                let excess = reserved_cash - consumed_cash;
                if excess > 0 {
                    let _ = self.ledger.release(bid.bidder_id, Asset::Cash, excess);
                }
            }
        }

        if let Some(auc) = self.batch_auctions.get_mut(&auction_id) {
            auc.qty_total = Some(total_revealed_supply);
            auc.qty_remaining = Some(unsold_total);
            auc.clearing_price_ticks = Some(uniform_price);
            auc.phase = BatchAuctionPhase::Settled;
        }

        self.batch_settlements.insert(auction_id, fills.clone());
        self.bump_state_version();
        Ok(fills)
    }

    /// Release every reservation of an unmatched or uncrossing window and mark
    /// it expired. Bidders get their cash back, sellers their passes.
    fn expire_batch_auction(
        &mut self,
        auction_id: u64,
        market: &MarketConfig,
        bids: &[BidRecord],
        asks: &[AskRecord],
    ) {
        if let Some(auc) = self.batch_auctions.get_mut(&auction_id) {
            auc.phase = BatchAuctionPhase::Expired;
            let total_revealed_supply: Qty = asks.iter().map(|a| a.qty.unwrap_or(0)).sum();
            auc.qty_total = Some(total_revealed_supply);
            auc.qty_remaining = Some(total_revealed_supply);
        }
        for bid in bids {
            if let (Some(price), Some(qty)) = (bid.price_ticks, bid.qty)
                && let Some(notional) = market.notional_minor(price, qty)
            {
                let cash = notional.saturating_add(market.taker_fee_minor(notional));
                let _ = self.ledger.release(bid.bidder_id, Asset::Cash, cash);
            }
        }
        for ask in asks {
            if let Some(qty) = ask.qty {
                let _ = self.release_pass(ask.seller_id, market.id, qty);
            }
        }
    }

    pub fn batch_auction(&self, auction_id: u64) -> Option<&BatchAuction> {
        self.batch_auctions.get(&auction_id)
    }

    pub fn batch_auctions(&self, market_id: Option<MarketId>) -> Vec<BatchAuction> {
        let mut list: Vec<_> = self
            .batch_auctions
            .values()
            .filter(|a| market_id.is_none_or(|id| a.market_id == id))
            .cloned()
            .collect();
        list.sort_by_key(|a| a.id);
        list
    }

    pub fn batch_bids(&self, auction_id: u64) -> Vec<BidRecord> {
        self.batch_bids
            .iter()
            .filter(|((id, _, _), _)| *id == auction_id)
            .map(|(_, b)| b.clone())
            .collect()
    }

    pub fn batch_asks(&self, auction_id: u64) -> Vec<AskRecord> {
        self.batch_asks
            .iter()
            .filter(|((id, _, _), _)| *id == auction_id)
            .map(|(_, a)| a.clone())
            .collect()
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

        let mut auctions: Vec<_> = self.auctions.values().collect();
        auctions.sort_by_key(|auction| auction.id);
        for auction in auctions {
            hasher.update(auction.id.to_le_bytes());
            hasher.update(auction.market_id.to_le_bytes());
            hasher.update(auction.seller_id.to_le_bytes());
            hasher.update(auction.qty_total.to_le_bytes());
            hasher.update(auction.qty_remaining.to_le_bytes());
            hasher.update(auction.start_price_ticks.to_le_bytes());
            hasher.update(auction.floor_price_ticks.to_le_bytes());
            hasher.update(auction.start_ms.to_le_bytes());
            hasher.update(auction.end_ms.to_le_bytes());
            hasher.update([match auction.status {
                AuctionStatus::Active => 0,
                AuctionStatus::Sold => 1,
                AuctionStatus::Expired => 2,
            }]);
        }
        hasher.update(self.next_auction_id.to_le_bytes());

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

        for auction in self.auctions.values() {
            match auction.status {
                AuctionStatus::Active => {
                    let entry = expected_pass
                        .entry((auction.seller_id, auction.market_id))
                        .or_default();
                    *entry = entry
                        .checked_add(auction.qty_remaining)
                        .ok_or_else(|| "expected auction pass overflow".to_string())?;
                }
                AuctionStatus::Sold if auction.qty_remaining != 0 => {
                    return Err(format!(
                        "sold auction {} still has {} passes",
                        auction.id, auction.qty_remaining
                    ));
                }
                _ => {}
            }
        }

        // In-flight batch-auction reservations: a valid revealed bid holds
        // worst-case cash (notional plus taker fee) and a valid revealed ask
        // holds its passes. Settled and expired windows already released or
        // consumed every reservation, so only live phases contribute.
        for (key, bid) in &self.batch_bids {
            let (Some(price), Some(qty)) = (bid.price_ticks, bid.qty) else {
                continue;
            };
            if !bid.valid || qty == 0 {
                continue;
            }
            let Some(auction) = self.batch_auctions.get(&key.0) else {
                return Err(format!(
                    "batch bid for user {} references missing auction {}",
                    bid.bidder_id, key.0
                ));
            };
            if matches!(
                auction.phase,
                BatchAuctionPhase::Settled | BatchAuctionPhase::Expired
            ) {
                continue;
            }
            let Some(market) = self.markets.get(&auction.market_id) else {
                return Err(format!("missing market {}", auction.market_id));
            };
            let Some(notional) = market.notional_minor(price, qty) else {
                return Err("batch bid notional overflow".to_string());
            };
            let fee = market.taker_fee_minor(notional);
            let entry = expected_cash.entry(bid.bidder_id).or_default();
            *entry = entry
                .checked_add(notional)
                .and_then(|value| value.checked_add(fee))
                .ok_or_else(|| "expected batch cash overflow".to_string())?;
        }
        for (key, ask) in &self.batch_asks {
            let (Some(_price), Some(qty)) = (ask.price_ticks, ask.qty) else {
                continue;
            };
            if !ask.valid || qty == 0 {
                continue;
            }
            let Some(auction) = self.batch_auctions.get(&key.0) else {
                return Err(format!(
                    "batch ask for user {} references missing auction {}",
                    ask.seller_id, key.0
                ));
            };
            if matches!(
                auction.phase,
                BatchAuctionPhase::Settled | BatchAuctionPhase::Expired
            ) {
                continue;
            }
            let entry = expected_pass
                .entry((ask.seller_id, auction.market_id))
                .or_default();
            *entry = entry
                .checked_add(qty)
                .ok_or_else(|| "expected batch pass overflow".to_string())?;
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
        escrow: true,
    }
}

fn ledger_reason(error: &LedgerError, asset: Asset) -> ReasonCode {
    match error {
        LedgerError::InsufficientFunds { .. } | LedgerError::InsufficientHeld { .. } => {
            if asset == Asset::Cash {
                ReasonCode::InsufficientFunds
            } else {
                ReasonCode::InsufficientInventory
            }
        }
        LedgerError::NonPositiveAmount => ReasonCode::InvalidQuantity,
        _ => ReasonCode::InternalError,
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
