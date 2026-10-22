//! Regenerate the golden MessagePack frames used by the web contract tests.
//!
//! Run with `cargo run -p dx-types --example generate_msgpack` or `just fixtures`.
//! The JSON fixtures remain the canonical, reviewable schema.

use std::fs;
use std::path::Path;

use dx_types::{
    BookDelta, BookLevel, BookSnapshot, LevelChange, MetricsSnapshot, Side, WsClientMessage,
    WsServerMessage,
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
}

fn write<T: serde::Serialize>(dir: &Path, name: &str, message: &T) {
    let bytes = rmp_serde::to_vec_named(message).expect("encode fixture");
    fs::write(dir.join(name), bytes).expect("write fixture");
}
