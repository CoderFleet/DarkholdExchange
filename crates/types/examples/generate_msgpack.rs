//! Regenerate the golden MessagePack frames used by the web contract tests.
//!
//! Run with `cargo run -p dx-types --example generate_msgpack` or `just fixtures`.
//! The JSON fixtures remain the canonical, reviewable schema.

use std::fs;
use std::path::Path;

use dx_types::{
    Asset, Balance, BookDelta, BookLevel, BookSnapshot, EscrowRecord, EscrowState, Fill,
    LevelChange, MetricsSnapshot, Side, WsClientMessage, WsServerMessage,
};

fn main() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contracts/msgpack");
    fs::create_dir_all(&dir).expect("create msgpack fixture directory");

    write(
        &dir,
        "snapshot.msgpack",
        &WsServerMessage::Snapshot {
            snapshot: BookSnapshot {
                market_id: 1,
                book_seq: 7,
                bids: vec![BookLevel {
                    price_ticks: 5_000,
                    qty: 2,
                }],
                asks: vec![BookLevel {
                    price_ticks: 5_100,
                    qty: 1,
                }],
                last_trade_price: Some(5_050),
            },
        },
    );
    write(
        &dir,
        "subscribe.msgpack",
        &WsClientMessage::Subscribe {
            market_ids: vec![1, 3],
            depth: 20,
        },
    );
    write(
        &dir,
        "delta.msgpack",
        &WsServerMessage::Delta {
            delta: BookDelta {
                market_id: 1,
                book_seq: 11,
                changes: vec![
                    LevelChange {
                        side: Side::Buy,
                        price_ticks: 5_000,
                        qty: 3,
                    },
                    LevelChange {
                        side: Side::Sell,
                        price_ticks: 5_100,
                        qty: 0,
                    },
                ],
            },
        },
    );
    write(
        &dir,
        "resync_required.msgpack",
        &WsServerMessage::ResyncRequired { market_id: 1 },
    );
    write(
        &dir,
        "metrics.msgpack",
        &WsServerMessage::Metrics {
            metrics: MetricsSnapshot {
                seq: 42,
                queue_depth: 4,
                commands_total: 120_000,
                rejected_total: 120,
                queue_wait_p99_us: 850,
                core_p99_us: 210,
                ws_clients: 32,
                slow_consumers: 1,
            },
        },
    );
    write(
        &dir,
        "balances.msgpack",
        &WsServerMessage::Balances {
            user_id: 9,
            balances: vec![Balance {
                asset: Asset::Cash,
                available: 150_000,
                held: 2_000,
            }],
        },
    );
    write(
        &dir,
        "fill_self.msgpack",
        &WsServerMessage::FillSelf {
            fill: Fill {
                trade_id: 31,
                market_id: 1,
                maker_order_id: 11,
                taker_order_id: 12,
                maker_user_id: 2,
                taker_user_id: 9,
                maker_side: Side::Sell,
                price_ticks: 5_000,
                qty: 2,
                seq: 77,
                escrow_id: None,
            },
        },
    );
    write(
        &dir,
        "escrow_update.msgpack",
        &WsServerMessage::EscrowUpdate {
            record: crate::EscrowRecord {
                trade_id: 31,
                market_id: 1,
                buyer_id: 9,
                seller_id: 2,
                qty: 2,
                notional: 100_000,
                fee: 1_500,
                fee_payer: 9,
                state: EscrowState::PendingDelivery,
                created_at_ms: 1_000,
                deadline_ms: 301_000,
                delivered_at_ms: None,
                seller_public_key: [0x1a; 32],
            },
        },
    );
}

fn write<T: serde::Serialize>(dir: &Path, name: &str, message: &T) {
    let bytes = rmp_serde::to_vec_named(message).expect("encode fixture");
    fs::write(dir.join(name), bytes).expect("write fixture");
}
