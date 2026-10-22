use dx_types::*;

#[test]
fn server_messages_roundtrip_through_msgpack() {
    let snapshot = BookSnapshot {
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
    };
    let messages = vec![
        WsServerMessage::Hello {
            markets: seed_markets(),
            user_id: Some(1),
        },
        WsServerMessage::Snapshot { snapshot },
        WsServerMessage::Delta {
            delta: BookDelta {
                market_id: 1,
                book_seq: 8,
                changes: vec![LevelChange {
                    side: Side::Buy,
                    price_ticks: 5_000,
                    qty: 3,
                }],
            },
        },
        WsServerMessage::ResyncRequired { market_id: 1 },
        WsServerMessage::Metrics {
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
        WsServerMessage::Pong,
        WsServerMessage::Error {
            code: ReasonCode::OutsideCollar,
        },
    ];
    for message in messages {
        let bytes = rmp_serde::to_vec_named(&message).unwrap();
        let decoded: WsServerMessage = rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(decoded, message);
    }
}

fn golden(name: &str) -> Vec<u8> {
    std::fs::read(format!(
        "{}/../../contracts/msgpack/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

#[test]
fn decodes_golden_server_snapshot_frame() {
    let decoded: WsServerMessage = rmp_serde::from_slice(&golden("snapshot.msgpack")).unwrap();
    assert_eq!(
        decoded,
        WsServerMessage::Snapshot {
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
        }
    );
}

#[test]
fn decodes_golden_subscribe_frame() {
    let decoded: WsClientMessage = rmp_serde::from_slice(&golden("subscribe.msgpack")).unwrap();
    assert_eq!(
        decoded,
        WsClientMessage::Subscribe {
            market_ids: vec![1, 3],
            depth: 20,
        }
    );
}

#[test]
fn decodes_golden_delta_frame() {
    let decoded: WsServerMessage = rmp_serde::from_slice(&golden("delta.msgpack")).unwrap();
    assert_eq!(
        decoded,
        WsServerMessage::Delta {
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
        }
    );
}

#[test]
fn decodes_golden_resync_frame() {
    let decoded: WsServerMessage =
        rmp_serde::from_slice(&golden("resync_required.msgpack")).unwrap();
    assert_eq!(decoded, WsServerMessage::ResyncRequired { market_id: 1 });
}

#[test]
fn decodes_golden_metrics_frame() {
    let decoded: WsServerMessage = rmp_serde::from_slice(&golden("metrics.msgpack")).unwrap();
    assert_eq!(
        decoded,
        WsServerMessage::Metrics {
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
        }
    );
}

#[test]
fn client_messages_roundtrip_through_msgpack() {
    let messages = vec![
        WsClientMessage::Subscribe {
            market_ids: vec![1, 3],
            depth: 20,
        },
        WsClientMessage::Ping,
    ];
    for message in messages {
        let bytes = rmp_serde::to_vec_named(&message).unwrap();
        let decoded: WsClientMessage = rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(decoded, message);
    }
}
