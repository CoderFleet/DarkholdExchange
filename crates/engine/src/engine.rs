//! Multi-market engine. The engine is pure: it owns books and order/trade ids, and
//! never touches the ledger, network, clock, or disk.

use std::collections::HashMap;

use dx_types::{MarketConfig, MarketId, OrderId, Qty, ReasonCode, Side, Ticks, TradeId, UserId};
use sha2::{Digest, Sha256};

use crate::book::{CancelOutcome, OrderBook, PlaceOutcome};

pub struct Engine {
    books: HashMap<MarketId, OrderBook>,
    next_order_id: OrderId,
    next_trade_id: TradeId,
}

impl Engine {
    pub fn new(configs: Vec<MarketConfig>) -> Result<Self, String> {
        let mut books = HashMap::with_capacity(configs.len());
        for config in configs {
            config.validate()?;
            if books.insert(config.id, OrderBook::new(config)).is_some() {
                return Err("duplicate market id".into());
            }
        }
        Ok(Self {
            books,
            next_order_id: 1,
            next_trade_id: 1,
        })
    }

    pub fn place_limit(
        &mut self,
        user_id: UserId,
        market_id: MarketId,
        side: Side,
        price_ticks: Ticks,
        qty: Qty,
    ) -> Result<PlaceOutcome, ReasonCode> {
        let Self {
            books,
            next_order_id,
            next_trade_id,
        } = self;
        let book = books.get_mut(&market_id).ok_or(ReasonCode::UnknownMarket)?;
        book.validate_limit(price_ticks, qty)?;

        let order_id = *next_order_id;
        *next_order_id += 1;
        book.submit_limit(order_id, user_id, side, price_ticks, qty, next_trade_id)
    }

    pub fn place_market(
        &mut self,
        user_id: UserId,
        market_id: MarketId,
        side: Side,
        qty: Qty,
        protect_price_ticks: Ticks,
    ) -> Result<PlaceOutcome, ReasonCode> {
        let Self {
            books,
            next_order_id,
            next_trade_id,
        } = self;
        let book = books.get_mut(&market_id).ok_or(ReasonCode::UnknownMarket)?;
        book.validate_market(protect_price_ticks, qty)?;

        let order_id = *next_order_id;
        *next_order_id += 1;
        book.submit_market(
            order_id,
            user_id,
            side,
            qty,
            protect_price_ticks,
            next_trade_id,
        )
    }

    pub fn cancel(
        &mut self,
        user_id: UserId,
        order_id: OrderId,
    ) -> Result<CancelOutcome, ReasonCode> {
        for book in self.books.values_mut() {
            if book.has_order(order_id) {
                return book.cancel(user_id, order_id);
            }
        }
        Err(ReasonCode::OrderNotFound)
    }

    pub fn snapshot(&self, market_id: MarketId, depth: usize) -> Option<dx_types::BookSnapshot> {
        self.books.get(&market_id).map(|book| book.snapshot(depth))
    }

    pub fn book(&self, market_id: MarketId) -> Option<&OrderBook> {
        self.books.get(&market_id)
    }

    pub fn config(&self, market_id: MarketId) -> Option<&MarketConfig> {
        self.books.get(&market_id).map(OrderBook::config)
    }

    pub fn market_ids(&self) -> Vec<MarketId> {
        let mut ids: Vec<_> = self.books.keys().copied().collect();
        ids.sort_unstable();
        ids
    }

    /// Deterministic hash of all books. Used by replay tests and crash recovery.
    pub fn state_hash(&self) -> [u8; 32] {
        let mut hasher = Sha256::new();
        let ids = self.market_ids();
        hasher.update((ids.len() as u64).to_le_bytes());
        hasher.update(self.next_order_id.to_le_bytes());
        hasher.update(self.next_trade_id.to_le_bytes());
        for id in ids {
            self.books[&id].hash_into(&mut hasher);
        }
        hasher.finalize().into()
    }
}
