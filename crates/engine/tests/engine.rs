use dx_engine::Engine;
use dx_types::{
    BookDelta, BookLevel, BookSnapshot, LevelChange, OrderStatus, ReasonCode, Side, seed_markets,
};

const MARKET: u32 = 1;
const FLOOR: u64 = 2_500;
const CAP: u64 = 6_000;

fn engine() -> Engine {
    Engine::new(seed_markets()).unwrap()
}

fn apply_delta(snapshot: &mut BookSnapshot, delta: &BookDelta) {
    assert_eq!(snapshot.market_id, delta.market_id);
    assert_eq!(
        delta.book_seq,
        snapshot.book_seq + 1,
        "delta must be the next book sequence"
    );

    for change in &delta.changes {
        let levels = match change.side {
            Side::Buy => &mut snapshot.bids,
            Side::Sell => &mut snapshot.asks,
        };
        if let Some(level) = levels
            .iter_mut()
            .find(|level| level.price_ticks == change.price_ticks)
        {
            level.qty = change.qty;
        } else if change.qty > 0 {
            levels.push(BookLevel {
                price_ticks: change.price_ticks,
                qty: change.qty,
            });
        }
        levels.retain(|level| level.qty > 0);
        match change.side {
            Side::Buy => levels.sort_unstable_by_key(|level| std::cmp::Reverse(level.price_ticks)),
            Side::Sell => levels.sort_unstable_by_key(|level| level.price_ticks),
        }
    }

    snapshot.book_seq = delta.book_seq;
}

#[test]
fn runtime_market_gets_a_fresh_book_and_rejects_duplicates() {
    let mut engine = engine();
    let mut config = seed_markets().into_iter().next().unwrap();
    config.id = 100;
    config.event_id = 99;
    config.name = "Runtime market".into();

    engine.add_market(config.clone()).unwrap();

    assert_eq!(engine.config(100).unwrap().id, 100);
    let snapshot = engine.snapshot(100, 10).unwrap();
    assert_eq!(snapshot.book_seq, 0);
    assert!(snapshot.bids.is_empty());
    assert!(snapshot.asks.is_empty());
    assert!(engine.add_market(config).is_err());
}

#[test]
fn limit_order_rests_and_updates_best_bid() {
    let mut engine = engine();
    let outcome = engine.place_limit(1, MARKET, Side::Buy, 5_000, 3).unwrap();

    assert_eq!(outcome.status, OrderStatus::Rested);
    assert_eq!(outcome.filled_qty, 0);
    assert_eq!(outcome.remaining_qty, 3);
    assert_eq!(engine.book(MARKET).unwrap().best_bid(), Some(5_000));
    assert_eq!(engine.book(MARKET).unwrap().best_ask(), None);

    let snapshot = engine.snapshot(MARKET, 10).unwrap();
    assert_eq!(
        snapshot.bids,
        vec![BookLevel {
            price_ticks: 5_000,
            qty: 3
        }]
    );
}

#[test]
fn crossing_order_trades_and_remainder_rests() {
    let mut engine = engine();
    engine.place_limit(2, MARKET, Side::Sell, 5_000, 2).unwrap();
    let outcome = engine.place_limit(1, MARKET, Side::Buy, 5_000, 3).unwrap();

    assert_eq!(outcome.status, OrderStatus::PartiallyFilled);
    assert_eq!(outcome.filled_qty, 2);
    assert_eq!(outcome.remaining_qty, 1);
    assert_eq!(outcome.fills.len(), 1);
    assert_eq!(outcome.fills[0].price_ticks, 5_000);
    assert_eq!(outcome.fills[0].qty, 2);
    assert_eq!(outcome.fills[0].maker_side, Side::Sell);

    let snapshot = engine.snapshot(MARKET, 10).unwrap();
    assert_eq!(snapshot.best_bid(), Some(5_000));
    assert_eq!(
        snapshot.bids,
        vec![BookLevel {
            price_ticks: 5_000,
            qty: 1
        }]
    );
    assert!(snapshot.asks.is_empty());
}

#[test]
fn price_time_priority_is_fifo_within_a_level() {
    let mut engine = engine();
    let first = engine.place_limit(2, MARKET, Side::Sell, 5_000, 2).unwrap();
    let second = engine.place_limit(3, MARKET, Side::Sell, 5_000, 2).unwrap();
    let third = engine.place_limit(4, MARKET, Side::Sell, 5_100, 2).unwrap();

    let outcome = engine.place_limit(1, MARKET, Side::Buy, 5_100, 5).unwrap();

    assert_eq!(outcome.fills.len(), 3);
    assert_eq!(outcome.fills[0].maker_order_id, first.order_id);
    assert_eq!(outcome.fills[0].qty, 2);
    assert_eq!(outcome.fills[1].maker_order_id, second.order_id);
    assert_eq!(outcome.fills[1].qty, 2);
    assert_eq!(outcome.fills[2].maker_order_id, third.order_id);
    assert_eq!(outcome.fills[2].qty, 1);

    let snapshot = engine.snapshot(MARKET, 10).unwrap();
    assert!(snapshot.bids.is_empty());
    assert_eq!(
        snapshot.asks,
        vec![BookLevel {
            price_ticks: 5_100,
            qty: 1
        }]
    );
}

#[test]
fn cancel_removes_order_and_is_owner_checked() {
    let mut engine = engine();
    let outcome = engine.place_limit(1, MARKET, Side::Buy, 5_000, 3).unwrap();

    assert_eq!(
        engine.cancel(2, outcome.order_id).unwrap_err(),
        ReasonCode::NotOrderOwner
    );

    let cancelled = engine.cancel(1, outcome.order_id).unwrap();
    assert_eq!(cancelled.cancelled_qty, 3);
    assert_eq!(engine.book(MARKET).unwrap().open_order_count(), 0);
    assert_eq!(
        engine.cancel(1, outcome.order_id).unwrap_err(),
        ReasonCode::OrderNotFound
    );
}

#[test]
fn self_trade_prevention_cancels_the_resting_order() {
    let mut engine = engine();
    let resting = engine.place_limit(1, MARKET, Side::Buy, 5_000, 2).unwrap();

    let outcome = engine.place_limit(1, MARKET, Side::Sell, 5_000, 2).unwrap();

    assert_eq!(outcome.fills.len(), 0);
    assert_eq!(
        outcome
            .cancelled_self_trade_orders
            .iter()
            .map(|order| order.order_id)
            .collect::<Vec<_>>(),
        vec![resting.order_id]
    );
    assert_eq!(outcome.status, OrderStatus::Rested);

    let snapshot = engine.snapshot(MARKET, 10).unwrap();
    assert!(snapshot.bids.is_empty());
    assert_eq!(
        snapshot.asks,
        vec![BookLevel {
            price_ticks: 5_000,
            qty: 2
        }]
    );
}

#[test]
fn collar_and_quantity_are_enforced() {
    let mut engine = engine();

    assert_eq!(
        engine
            .place_limit(1, MARKET, Side::Buy, FLOOR - 1, 1)
            .unwrap_err(),
        ReasonCode::OutsideCollar
    );
    assert_eq!(
        engine
            .place_limit(1, MARKET, Side::Buy, CAP + 1, 1)
            .unwrap_err(),
        ReasonCode::OutsideCollar
    );
    assert_eq!(
        engine
            .place_limit(1, MARKET, Side::Buy, 5_000, 0)
            .unwrap_err(),
        ReasonCode::InvalidQuantity
    );
    assert_eq!(
        engine.place_limit(1, 999, Side::Buy, 5_000, 1).unwrap_err(),
        ReasonCode::UnknownMarket
    );
}

#[test]
fn market_order_sweeps_within_protection_and_does_not_rest() {
    let mut engine = engine();
    engine.place_limit(2, MARKET, Side::Sell, 5_000, 2).unwrap();
    engine.place_limit(2, MARKET, Side::Sell, 5_100, 2).unwrap();
    engine.place_limit(2, MARKET, Side::Sell, 5_200, 2).unwrap();

    let outcome = engine.place_market(1, MARKET, Side::Buy, 5, 5_100).unwrap();

    assert_eq!(outcome.status, OrderStatus::Cancelled);
    assert_eq!(outcome.filled_qty, 4);
    assert_eq!(outcome.remaining_qty, 1);
    assert_eq!(outcome.fills.len(), 2);
    assert_eq!(outcome.fills[0].price_ticks, 5_000);
    assert_eq!(outcome.fills[0].qty, 2);
    assert_eq!(outcome.fills[1].price_ticks, 5_100);
    assert_eq!(outcome.fills[1].qty, 2);

    let snapshot = engine.snapshot(MARKET, 10).unwrap();
    assert!(snapshot.bids.is_empty());
    assert_eq!(
        snapshot.asks,
        vec![BookLevel {
            price_ticks: 5_200,
            qty: 2
        }]
    );
}

#[test]
fn state_hash_is_deterministic_for_the_same_sequence() {
    let mut a = engine();
    let mut b = engine();
    for engine in [&mut a, &mut b] {
        engine.place_limit(1, MARKET, Side::Buy, 5_000, 2).unwrap();
        engine.place_limit(2, MARKET, Side::Sell, 5_100, 3).unwrap();
        engine.place_limit(1, MARKET, Side::Buy, 5_100, 1).unwrap();
    }
    assert_eq!(a.state_hash(), b.state_hash());

    b.place_limit(2, MARKET, Side::Sell, 5_200, 1).unwrap();
    assert_ne!(a.state_hash(), b.state_hash());
}

#[test]
fn emitted_deltas_are_absolute_and_reconstruct_the_next_snapshot() {
    let mut engine = engine();

    let mut before = engine.snapshot(MARKET, 10).unwrap();
    let first = engine.place_limit(1, MARKET, Side::Buy, 5_000, 3).unwrap();
    let first_delta = first.delta.clone().expect("rest changes the book");
    assert_eq!(
        first_delta.changes,
        vec![LevelChange {
            side: Side::Buy,
            price_ticks: 5_000,
            qty: 3,
        }]
    );
    apply_delta(&mut before, &first_delta);
    assert_eq!(before, engine.snapshot(MARKET, 10).unwrap());

    let mut before = engine.snapshot(MARKET, 10).unwrap();
    let second = engine.place_limit(2, MARKET, Side::Buy, 5_000, 2).unwrap();
    let second_delta = second.delta.clone().expect("rest changes the book");
    assert_eq!(second_delta.book_seq, first_delta.book_seq + 1);
    assert_eq!(
        second_delta.changes,
        vec![LevelChange {
            side: Side::Buy,
            price_ticks: 5_000,
            qty: 5,
        }]
    );
    apply_delta(&mut before, &second_delta);
    assert_eq!(before, engine.snapshot(MARKET, 10).unwrap());

    let mut before = engine.snapshot(MARKET, 10).unwrap();
    let cancelled = engine.cancel(1, first.order_id).unwrap();
    assert_eq!(cancelled.delta.book_seq, second_delta.book_seq + 1);
    assert_eq!(
        cancelled.delta.changes,
        vec![LevelChange {
            side: Side::Buy,
            price_ticks: 5_000,
            qty: 2,
        }]
    );
    apply_delta(&mut before, &cancelled.delta);
    assert_eq!(before, engine.snapshot(MARKET, 10).unwrap());
}

#[test]
fn market_order_without_liquidity_does_not_advance_book_seq_or_emit_a_delta() {
    let mut engine = engine();
    let before = engine.snapshot(MARKET, 10).unwrap();

    let outcome = engine
        .place_market(1, MARKET, Side::Buy, 2, 5_000)
        .expect("valid market order");

    assert_eq!(outcome.status, OrderStatus::Cancelled);
    assert_eq!(outcome.filled_qty, 0);
    assert!(outcome.fills.is_empty());
    assert!(outcome.delta.is_none());
    assert_eq!(engine.book(MARKET).unwrap().book_seq(), 0);
    assert_eq!(engine.snapshot(MARKET, 10).unwrap(), before);
}

#[test]
fn self_trade_cancellation_emits_both_level_changes() {
    let mut engine = engine();
    engine.place_limit(1, MARKET, Side::Buy, 5_000, 2).unwrap();

    let mut before = engine.snapshot(MARKET, 10).unwrap();
    let outcome = engine.place_limit(1, MARKET, Side::Sell, 5_000, 2).unwrap();
    let delta = outcome.delta.clone().expect("self-trade changes the book");

    assert!(outcome.fills.is_empty());
    assert_eq!(
        delta.changes,
        vec![
            LevelChange {
                side: Side::Buy,
                price_ticks: 5_000,
                qty: 0,
            },
            LevelChange {
                side: Side::Sell,
                price_ticks: 5_000,
                qty: 2,
            },
        ]
    );
    apply_delta(&mut before, &delta);
    assert_eq!(before, engine.snapshot(MARKET, 10).unwrap());
}

#[test]
fn multi_level_sweep_emits_one_delta_with_absolute_level_changes() {
    let mut engine = engine();
    engine.place_limit(2, MARKET, Side::Sell, 5_000, 2).unwrap();
    engine.place_limit(2, MARKET, Side::Sell, 5_100, 2).unwrap();
    assert_eq!(engine.book(MARKET).unwrap().book_seq(), 2);

    let mut before = engine.snapshot(MARKET, 10).unwrap();
    let outcome = engine.place_market(1, MARKET, Side::Buy, 3, 5_100).unwrap();
    let delta = outcome.delta.clone().expect("sweep changes the book");

    assert_eq!(delta.book_seq, 3);
    assert_eq!(
        delta.changes,
        vec![
            LevelChange {
                side: Side::Sell,
                price_ticks: 5_000,
                qty: 0,
            },
            LevelChange {
                side: Side::Sell,
                price_ticks: 5_100,
                qty: 1,
            },
        ]
    );
    apply_delta(&mut before, &delta);
    before.last_trade_price = outcome.fills.last().map(|fill| fill.price_ticks);
    assert_eq!(before, engine.snapshot(MARKET, 10).unwrap());
    assert_eq!(engine.book(MARKET).unwrap().book_seq(), 3);
}
