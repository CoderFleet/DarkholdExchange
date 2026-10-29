//! Pure order book: flat price-level array, slab-allocated orders, intrusive FIFO
//! lists, and a bitset for O(1)-ish best-price lookup. No I/O, clock, or randomness.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use dx_types::{
    BookDelta, BookLevel, BookSeq, BookSnapshot, LevelChange, MarketConfig, MarketId, MarketStatus,
    OrderId, OrderStatus, Qty, ReasonCode, Side, Ticks, TradeId, UserId,
};
use sha2::{Digest, Sha256};

/// A FIFO queue of orders at one price.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PriceLevel {
    pub head: Option<u32>,
    pub tail: Option<u32>,
    pub qty: Qty,
}

/// One resting order. `prev`/`next` are slab indices, not pointers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Order {
    pub id: OrderId,
    pub user_id: UserId,
    pub side: Side,
    pub price_ticks: Ticks,
    pub qty: Qty,
    pub remaining: Qty,
    prev: Option<u32>,
    next: Option<u32>,
}

/// A match produced by the pure engine. The core maps this into a settled `Fill`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchFill {
    pub trade_id: TradeId,
    pub maker_order_id: OrderId,
    pub taker_order_id: OrderId,
    pub maker_user_id: UserId,
    pub taker_user_id: UserId,
    pub maker_side: Side,
    pub price_ticks: Ticks,
    pub qty: Qty,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CancelledOrder {
    pub order_id: OrderId,
    pub user_id: UserId,
    pub side: Side,
    pub price_ticks: Ticks,
    pub remaining: Qty,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaceOutcome {
    pub order_id: OrderId,
    pub status: OrderStatus,
    pub filled_qty: Qty,
    pub remaining_qty: Qty,
    pub fills: Vec<MatchFill>,
    /// Resting orders cancelled by self-trade prevention.
    pub cancelled_self_trade_orders: Vec<CancelledOrder>,
    /// Absolute level changes caused by this command. `None` when the book did
    /// not change (for example, an unfilled market order with no liquidity).
    pub delta: Option<BookDelta>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CancelOutcome {
    pub order_id: OrderId,
    pub cancelled_qty: Qty,
    pub delta: BookDelta,
}

/// One side of the book, indexed by `price_ticks - floor`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct BookSide {
    side: Side,
    floor: Ticks,
    levels: Vec<PriceLevel>,
    bits: Vec<u64>,
}

impl BookSide {
    fn new(side: Side, floor: Ticks, cap: Ticks) -> Self {
        let len = (cap - floor + 1) as usize;
        Self {
            side,
            floor,
            levels: (0..len)
                .map(|_| PriceLevel {
                    head: None,
                    tail: None,
                    qty: 0,
                })
                .collect(),
            bits: vec![0; len.div_ceil(64)],
        }
    }

    fn index_of(&self, price_ticks: Ticks) -> Option<usize> {
        let offset = price_ticks.checked_sub(self.floor)?;
        let idx = usize::try_from(offset).ok()?;
        (idx < self.levels.len()).then_some(idx)
    }

    fn price_at(&self, idx: usize) -> Ticks {
        self.floor + idx as Ticks
    }

    fn level_change(&self, idx: usize) -> LevelChange {
        LevelChange {
            side: self.side,
            price_ticks: self.price_at(idx),
            qty: self.levels[idx].qty,
        }
    }

    fn set_bit(&mut self, idx: usize) {
        self.bits[idx / 64] |= 1u64 << (idx % 64);
    }

    fn clear_bit(&mut self, idx: usize) {
        self.bits[idx / 64] &= !(1u64 << (idx % 64));
    }

    fn best_index(&self) -> Option<usize> {
        match self.side {
            Side::Buy => {
                for (word_idx, word) in self.bits.iter().enumerate().rev() {
                    if *word != 0 {
                        let bit = 63 - word.leading_zeros() as usize;
                        return Some(word_idx * 64 + bit);
                    }
                }
            }
            Side::Sell => {
                for (word_idx, word) in self.bits.iter().enumerate() {
                    if *word != 0 {
                        let bit = word.trailing_zeros() as usize;
                        return Some(word_idx * 64 + bit);
                    }
                }
            }
        }
        None
    }

    fn best_price(&self) -> Option<Ticks> {
        self.best_index().map(|idx| self.price_at(idx))
    }

    fn append(&mut self, slot: u32, orders: &mut [Option<Order>]) {
        let (price, remaining) = {
            let order = orders[slot as usize].as_ref().expect("allocated slot");
            (order.price_ticks, order.remaining)
        };
        let idx = self.index_of(price).expect("price inside collar");
        let tail = self.levels[idx].tail;

        if let Some(tail) = tail {
            orders[tail as usize].as_mut().expect("tail slot").next = Some(slot);
        }
        {
            let order = orders[slot as usize].as_mut().expect("allocated slot");
            order.prev = tail;
            order.next = None;
        }

        let level = &mut self.levels[idx];
        if tail.is_none() {
            level.head = Some(slot);
        }
        level.tail = Some(slot);
        level.qty += remaining;
        self.set_bit(idx);
    }

    fn unlink(&mut self, slot: u32, orders: &mut [Option<Order>]) {
        let (price, remaining, prev, next) = {
            let order = orders[slot as usize].as_ref().expect("allocated slot");
            (order.price_ticks, order.remaining, order.prev, order.next)
        };
        let idx = self.index_of(price).expect("price inside collar");

        let level = &mut self.levels[idx];
        if let Some(prev) = prev {
            orders[prev as usize].as_mut().expect("prev slot").next = next;
        } else {
            level.head = next;
        }
        if let Some(next) = next {
            orders[next as usize].as_mut().expect("next slot").prev = prev;
        } else {
            level.tail = prev;
        }
        level.qty = level
            .qty
            .checked_sub(remaining)
            .expect("level quantity never underflows");
        if level.qty == 0 {
            self.clear_bit(idx);
        }

        let order = orders[slot as usize].as_mut().expect("allocated slot");
        order.prev = None;
        order.next = None;
    }

    /// Levels in display order (bids high -> low, asks low -> high).
    fn snapshot(&self, depth: usize) -> Vec<BookLevel> {
        let mut out = Vec::with_capacity(depth);
        let Some(mut idx) = self.best_index() else {
            return out;
        };

        loop {
            let level = &self.levels[idx];
            if level.qty > 0 {
                out.push(BookLevel {
                    price_ticks: self.price_at(idx),
                    qty: level.qty,
                });
                if out.len() >= depth {
                    break;
                }
            }

            match self.side {
                Side::Buy => {
                    if idx == 0 {
                        break;
                    }
                    idx -= 1;
                }
                Side::Sell => {
                    idx += 1;
                    if idx >= self.levels.len() {
                        break;
                    }
                }
            }
        }
        out
    }

    fn hash_into(&self, hasher: &mut Sha256, orders: &[Option<Order>]) {
        hasher.update([match self.side {
            Side::Buy => 0u8,
            Side::Sell => 1u8,
        }]);
        for idx in 0..self.levels.len() {
            let level = &self.levels[idx];
            if level.qty == 0 {
                continue;
            }
            hasher.update(self.price_at(idx).to_le_bytes());
            hasher.update(level.qty.to_le_bytes());

            let mut cursor = level.head;
            while let Some(slot) = cursor {
                let order = orders[slot as usize].as_ref().expect("head slot");
                hasher.update(order.id.to_le_bytes());
                hasher.update(order.user_id.to_le_bytes());
                hasher.update(order.qty.to_le_bytes());
                hasher.update(order.remaining.to_le_bytes());
                cursor = order.next;
            }
        }
    }
}

/// One market's order book.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderBook {
    config: MarketConfig,
    bids: BookSide,
    asks: BookSide,
    orders: Vec<Option<Order>>,
    free: Vec<u32>,
    index: HashMap<OrderId, u32>,
    book_seq: BookSeq,
    last_trade_price: Option<Ticks>,
}

impl OrderBook {
    pub(crate) fn new(config: MarketConfig) -> Self {
        let floor = config.floor_ticks();
        let cap = config.cap_ticks();
        let bids = BookSide::new(Side::Buy, floor, cap);
        let asks = BookSide::new(Side::Sell, floor, cap);
        Self {
            config,
            bids,
            asks,
            orders: Vec::with_capacity(1_024),
            free: Vec::new(),
            index: HashMap::with_capacity(1_024),
            book_seq: 0,
            last_trade_price: None,
        }
    }

    pub fn config(&self) -> &MarketConfig {
        &self.config
    }

    pub fn book_seq(&self) -> BookSeq {
        self.book_seq
    }

    pub fn best_bid(&self) -> Option<Ticks> {
        self.bids.best_price()
    }

    pub fn best_ask(&self) -> Option<Ticks> {
        self.asks.best_price()
    }

    pub fn snapshot(&self, depth: usize) -> BookSnapshot {
        BookSnapshot {
            market_id: self.config.id,
            book_seq: self.book_seq,
            bids: self.bids.snapshot(depth),
            asks: self.asks.snapshot(depth),
            last_trade_price: self.last_trade_price,
        }
    }

    pub fn order(&self, order_id: OrderId) -> Option<&Order> {
        self.index
            .get(&order_id)
            .and_then(|slot| self.orders[*slot as usize].as_ref())
    }

    pub fn has_order(&self, order_id: OrderId) -> bool {
        self.index.contains_key(&order_id)
    }

    pub fn orders(&self) -> impl Iterator<Item = &Order> {
        self.orders.iter().flatten()
    }

    pub fn open_order_count(&self) -> usize {
        self.index.len()
    }

    pub(crate) fn validate_limit(&self, price_ticks: Ticks, qty: Qty) -> Result<(), ReasonCode> {
        if self.config.status != MarketStatus::Open {
            return Err(ReasonCode::MarketClosed);
        }
        if qty == 0 {
            return Err(ReasonCode::InvalidQuantity);
        }
        if !self.config.price_in_collar(price_ticks) {
            return Err(ReasonCode::OutsideCollar);
        }
        Ok(())
    }

    pub(crate) fn validate_market(
        &self,
        protect_price_ticks: Ticks,
        qty: Qty,
    ) -> Result<(), ReasonCode> {
        if self.config.status != MarketStatus::Open {
            return Err(ReasonCode::MarketClosed);
        }
        if qty == 0 {
            return Err(ReasonCode::InvalidQuantity);
        }
        if !self.config.price_in_collar(protect_price_ticks) {
            return Err(ReasonCode::OutsideCollar);
        }
        Ok(())
    }

    pub(crate) fn submit_limit(
        &mut self,
        order_id: OrderId,
        user_id: UserId,
        side: Side,
        price_ticks: Ticks,
        qty: Qty,
        next_trade_id: &mut TradeId,
    ) -> Result<PlaceOutcome, ReasonCode> {
        self.validate_limit(price_ticks, qty)?;
        let market_id = self.config.id;

        let Self {
            bids,
            asks,
            orders,
            free,
            index,
            book_seq,
            last_trade_price,
            config: _,
        } = self;

        let (own, opposite) = match side {
            Side::Buy => (bids, asks),
            Side::Sell => (asks, bids),
        };

        let mut remaining = qty;
        let mut fills = Vec::new();
        let mut cancelled_self_trade_orders = Vec::new();
        let mut changes = Vec::new();

        while remaining > 0 {
            let Some(level_idx) = opposite.best_index() else {
                break;
            };
            let level_price = opposite.price_at(level_idx);
            let crosses = match side {
                Side::Buy => level_price <= price_ticks,
                Side::Sell => level_price >= price_ticks,
            };
            if !crosses {
                break;
            }

            let mut level_touched = false;
            while remaining > 0 {
                let Some(head_slot) = opposite.levels[level_idx].head else {
                    break;
                };
                let (maker_id, maker_user, maker_remaining) = {
                    let maker = orders[head_slot as usize].as_ref().expect("head slot");
                    (maker.id, maker.user_id, maker.remaining)
                };

                // Self-trade prevention: cancel the resting order, keep matching.
                if maker_user == user_id {
                    let removed = remove_order(orders, free, index, opposite, head_slot);
                    cancelled_self_trade_orders.push(CancelledOrder {
                        order_id: removed.id,
                        user_id: removed.user_id,
                        side: removed.side,
                        price_ticks: removed.price_ticks,
                        remaining: removed.remaining,
                    });
                    level_touched = true;
                    continue;
                }

                let fill_qty = remaining.min(maker_remaining);
                let trade_id = *next_trade_id;
                *next_trade_id += 1;
                fills.push(MatchFill {
                    trade_id,
                    maker_order_id: maker_id,
                    taker_order_id: order_id,
                    maker_user_id: maker_user,
                    taker_user_id: user_id,
                    maker_side: opposite.side,
                    price_ticks: level_price,
                    qty: fill_qty,
                });
                remaining -= fill_qty;
                level_touched = true;

                if fill_qty == maker_remaining {
                    // `unlink` subtracts the maker's remaining quantity from the level.
                    remove_order(orders, free, index, opposite, head_slot);
                } else {
                    orders[head_slot as usize]
                        .as_mut()
                        .expect("head slot")
                        .remaining -= fill_qty;
                    opposite.levels[level_idx].qty -= fill_qty;
                }
            }

            if level_touched {
                changes.push(opposite.level_change(level_idx));
            }
        }

        let filled_qty = qty - remaining;
        let mut status = if remaining == 0 {
            OrderStatus::Filled
        } else if filled_qty > 0 {
            OrderStatus::PartiallyFilled
        } else {
            OrderStatus::Rested
        };

        if remaining > 0 {
            let order = Order {
                id: order_id,
                user_id,
                side,
                price_ticks,
                qty,
                remaining,
                prev: None,
                next: None,
            };
            let slot = alloc_slot(orders, free, order);
            index.insert(order_id, slot);
            own.append(slot, orders);
            let own_idx = own.index_of(price_ticks).expect("price inside collar");
            changes.push(own.level_change(own_idx));
        } else {
            status = OrderStatus::Filled;
        }

        if let Some(fill) = fills.last() {
            *last_trade_price = Some(fill.price_ticks);
        }
        let delta = book_delta(market_id, book_seq, changes);

        Ok(PlaceOutcome {
            order_id,
            status,
            filled_qty,
            remaining_qty: remaining,
            fills,
            cancelled_self_trade_orders,
            delta,
        })
    }

    pub(crate) fn submit_market(
        &mut self,
        order_id: OrderId,
        user_id: UserId,
        side: Side,
        qty: Qty,
        protect_price_ticks: Ticks,
        next_trade_id: &mut TradeId,
    ) -> Result<PlaceOutcome, ReasonCode> {
        self.validate_market(protect_price_ticks, qty)?;
        let market_id = self.config.id;

        let Self {
            bids,
            asks,
            orders,
            free,
            index,
            book_seq,
            last_trade_price,
            config: _,
        } = self;

        let opposite = match side {
            Side::Buy => asks,
            Side::Sell => bids,
        };

        let mut remaining = qty;
        let mut fills = Vec::new();
        let mut cancelled_self_trade_orders = Vec::new();
        let mut changes = Vec::new();

        while remaining > 0 {
            let Some(level_idx) = opposite.best_index() else {
                break;
            };
            let level_price = opposite.price_at(level_idx);
            let crosses = match side {
                Side::Buy => level_price <= protect_price_ticks,
                Side::Sell => level_price >= protect_price_ticks,
            };
            if !crosses {
                break;
            }

            let mut level_touched = false;
            while remaining > 0 {
                let Some(head_slot) = opposite.levels[level_idx].head else {
                    break;
                };
                let (maker_id, maker_user, maker_remaining) = {
                    let maker = orders[head_slot as usize].as_ref().expect("head slot");
                    (maker.id, maker.user_id, maker.remaining)
                };

                if maker_user == user_id {
                    let removed = remove_order(orders, free, index, opposite, head_slot);
                    cancelled_self_trade_orders.push(CancelledOrder {
                        order_id: removed.id,
                        user_id: removed.user_id,
                        side: removed.side,
                        price_ticks: removed.price_ticks,
                        remaining: removed.remaining,
                    });
                    level_touched = true;
                    continue;
                }

                let fill_qty = remaining.min(maker_remaining);
                let trade_id = *next_trade_id;
                *next_trade_id += 1;
                fills.push(MatchFill {
                    trade_id,
                    maker_order_id: maker_id,
                    taker_order_id: order_id,
                    maker_user_id: maker_user,
                    taker_user_id: user_id,
                    maker_side: opposite.side,
                    price_ticks: level_price,
                    qty: fill_qty,
                });
                remaining -= fill_qty;
                level_touched = true;

                if fill_qty == maker_remaining {
                    remove_order(orders, free, index, opposite, head_slot);
                } else {
                    orders[head_slot as usize]
                        .as_mut()
                        .expect("head slot")
                        .remaining -= fill_qty;
                    opposite.levels[level_idx].qty -= fill_qty;
                }
            }

            if level_touched {
                changes.push(opposite.level_change(level_idx));
            }
        }

        if let Some(fill) = fills.last() {
            *last_trade_price = Some(fill.price_ticks);
        }
        let delta = book_delta(market_id, book_seq, changes);

        let filled_qty = qty - remaining;
        Ok(PlaceOutcome {
            order_id,
            status: if remaining == 0 {
                OrderStatus::Filled
            } else {
                OrderStatus::Cancelled
            },
            filled_qty,
            remaining_qty: remaining,
            fills,
            cancelled_self_trade_orders,
            delta,
        })
    }

    pub(crate) fn cancel(
        &mut self,
        user_id: UserId,
        order_id: OrderId,
    ) -> Result<CancelOutcome, ReasonCode> {
        let Some(slot) = self.index.get(&order_id).copied() else {
            return Err(ReasonCode::OrderNotFound);
        };
        let (owner, side, price_ticks, remaining) = {
            let order = self.orders[slot as usize]
                .as_ref()
                .ok_or(ReasonCode::OrderNotFound)?;
            (
                order.user_id,
                order.side,
                order.price_ticks,
                order.remaining,
            )
        };
        if owner != user_id {
            return Err(ReasonCode::NotOrderOwner);
        }

        let book_side = match side {
            Side::Buy => &mut self.bids,
            Side::Sell => &mut self.asks,
        };
        book_side.unlink(slot, &mut self.orders);
        let level_idx = book_side
            .index_of(price_ticks)
            .expect("cancelled order price inside collar");
        let change = book_side.level_change(level_idx);
        self.index.remove(&order_id);
        self.orders[slot as usize] = None;
        self.free.push(slot);
        self.book_seq += 1;
        let delta = BookDelta {
            market_id: self.config.id,
            book_seq: self.book_seq,
            changes: vec![change],
        };

        Ok(CancelOutcome {
            order_id,
            cancelled_qty: remaining,
            delta,
        })
    }

    pub(crate) fn hash_into(&self, hasher: &mut Sha256) {
        hasher.update(self.config.id.to_le_bytes());
        hasher.update(self.config.face_value_minor.to_le_bytes());
        hasher.update(self.config.tick_size_minor.to_le_bytes());
        hasher.update(self.config.collar_floor_bps.to_le_bytes());
        hasher.update(self.config.collar_cap_bps.to_le_bytes());
        hasher.update(self.config.holding_limit.to_le_bytes());
        hasher.update(self.config.taker_fee_bps.to_le_bytes());
        hasher.update([match self.config.status {
            MarketStatus::Open => 0u8,
            MarketStatus::Closed => 1u8,
            MarketStatus::Auction => 2u8,
        }]);
        hasher.update(self.book_seq.to_le_bytes());
        hasher.update(self.last_trade_price.unwrap_or(0).to_le_bytes());
        self.bids.hash_into(hasher, &self.orders);
        self.asks.hash_into(hasher, &self.orders);
    }
}

fn book_delta(
    market_id: MarketId,
    book_seq: &mut BookSeq,
    changes: Vec<LevelChange>,
) -> Option<BookDelta> {
    if changes.is_empty() {
        return None;
    }
    *book_seq += 1;
    Some(BookDelta {
        market_id,
        book_seq: *book_seq,
        changes,
    })
}

fn alloc_slot(orders: &mut Vec<Option<Order>>, free: &mut Vec<u32>, order: Order) -> u32 {
    if let Some(slot) = free.pop() {
        orders[slot as usize] = Some(order);
        slot
    } else {
        let slot = orders.len() as u32;
        orders.push(Some(order));
        slot
    }
}

fn remove_order(
    orders: &mut [Option<Order>],
    free: &mut Vec<u32>,
    index: &mut HashMap<OrderId, u32>,
    side: &mut BookSide,
    slot: u32,
) -> Order {
    side.unlink(slot, orders);
    let order = orders[slot as usize].take().expect("allocated slot");
    index.remove(&order.id);
    free.push(slot);
    order
}
