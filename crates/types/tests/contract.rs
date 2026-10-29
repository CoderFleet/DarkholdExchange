use dx_types::*;
use serde_json::Value;

fn fixture(name: &str) -> Value {
    let p = format!("{}/../../contracts/{}", env!("CARGO_MANIFEST_DIR"), name);
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

#[test]
fn place_limit_matches_fixture() {
    let m = ClientMessage::PlaceLimit(PlaceLimit {
        client_order_id: 1,
        market_id: 1,
        side: Side::Buy,
        price_ticks: 100,
        qty: 1,
        escrow: false,
    });
    assert_eq!(
        serde_json::to_value(&m).unwrap(),
        fixture("place_limit.json")
    );
    let back: ClientMessage = serde_json::from_value(fixture("place_limit.json")).unwrap();
    assert_eq!(back, m);
}

#[test]
fn accepted_matches_fixture() {
    let m = ServerMessage::Accepted {
        seq: 1,
        order_id: 42,
    };
    assert_eq!(serde_json::to_value(&m).unwrap(), fixture("accepted.json"));
}

#[test]
fn rejected_matches_fixture() {
    let m = ServerMessage::Rejected {
        code: ReasonCode::OutsideCollar,
    };
    assert_eq!(serde_json::to_value(&m).unwrap(), fixture("rejected.json"));
}

#[test]
fn balances_matches_fixture() {
    let m = WsServerMessage::Balances {
        user_id: 9,
        balances: vec![Balance {
            asset: Asset::Cash,
            available: 150_000,
            held: 2_000,
        }],
    };
    assert_eq!(serde_json::to_value(&m).unwrap(), fixture("balances.json"));
    let back: WsServerMessage = serde_json::from_value(fixture("balances.json")).unwrap();
    assert_eq!(back, m);
}

/// Private per-user fill push (row 7): only token-checked private streams
/// carry it, so skipping `escrow_id` means a plain spot fill.
#[test]
fn fill_self_matches_fixture() {
    let m = WsServerMessage::FillSelf {
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
    };
    assert_eq!(serde_json::to_value(&m).unwrap(), fixture("fill_self.json"));
    let back: WsServerMessage = serde_json::from_value(fixture("fill_self.json")).unwrap();
    assert_eq!(back, m);
}

#[test]
fn escrow_update_matches_fixture() {
    let m = WsServerMessage::EscrowUpdate {
        record: EscrowRecord {
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
    };
    assert_eq!(
        serde_json::to_value(&m).unwrap(),
        fixture("escrow_update.json")
    );
    let back: WsServerMessage = serde_json::from_value(fixture("escrow_update.json")).unwrap();
    assert_eq!(back, m);
}

#[test]
fn delta_matches_fixture() {
    let m = WsServerMessage::Delta {
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
    };
    assert_eq!(serde_json::to_value(&m).unwrap(), fixture("delta.json"));
    let back: WsServerMessage = serde_json::from_value(fixture("delta.json")).unwrap();
    assert_eq!(back, m);
}

#[test]
fn resync_required_matches_fixture() {
    let m = WsServerMessage::ResyncRequired { market_id: 1 };
    assert_eq!(
        serde_json::to_value(&m).unwrap(),
        fixture("resync_required.json")
    );
    let back: WsServerMessage = serde_json::from_value(fixture("resync_required.json")).unwrap();
    assert_eq!(back, m);
}

#[test]
fn metrics_matches_fixture() {
    let m = WsServerMessage::Metrics {
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
    };
    assert_eq!(serde_json::to_value(&m).unwrap(), fixture("metrics.json"));
    let back: WsServerMessage = serde_json::from_value(fixture("metrics.json")).unwrap();
    assert_eq!(back, m);
}
