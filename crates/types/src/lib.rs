//! Shared wire contract. The JSON shapes are pinned by ../../contracts/*.json,
//! which both the Rust tests and the web tests check against.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Side {
    Buy,
    Sell,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaceLimit {
    pub user_id: u64,
    pub market_id: u32,
    pub side: Side,
    pub price_ticks: u64,
    pub qty: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ClientMessage {
    PlaceLimit(PlaceLimit),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ServerMessage {
    Accepted { seq: u64, order_id: u64 },
    Rejected { reason: String },
}
