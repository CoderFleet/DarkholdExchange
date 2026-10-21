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
