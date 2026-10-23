use dx_core::{CommandOutcome, Core};
use dx_types::{
    Asset, Balance, BookDelta, BookLevel, BookSnapshot, Cancel, Command, LevelChange, OrderStatus,
    PlaceLimit, PlaceMarket, Qty, ReasonCode, Side, Ticks, seed_markets,
};

const MARKET: u32 = 1;
const PRICE: Ticks = 5_000;

fn core() -> Core {
    Core::new(seed_markets()).unwrap()
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

fn reconstruct_without_tape(before: &BookSnapshot, deltas: &[BookDelta], after: &BookSnapshot) {
    let mut reconstructed = before.clone();
    for delta in deltas {
        apply_delta(&mut reconstructed, delta);
    }
    assert_eq!(&reconstructed, after);
}

fn reconstruct_with_tape(outcome: &CommandOutcome, before: &BookSnapshot, after: &BookSnapshot) {
    let mut reconstructed = before.clone();
    for delta in &outcome.deltas {
        apply_delta(&mut reconstructed, delta);
    }
    if let Some(fill) = outcome.response.fills.last() {
        reconstructed.last_trade_price = Some(fill.price_ticks);
    }
    assert_eq!(&reconstructed, after);
}

fn limit(client_order_id: u64, side: Side, price_ticks: Ticks, qty: Qty) -> Command {
    Command::PlaceLimit(PlaceLimit {
        client_order_id,
        market_id: MARKET,
        side,
        price_ticks,
        qty,
    })
}

fn market_order(client_order_id: u64, side: Side, qty: Qty, protect: Ticks) -> Command {
    Command::PlaceMarket(PlaceMarket {
        client_order_id,
        market_id: MARKET,
        side,
        qty,
        protect_price_ticks: protect,
    })
}

fn cancel(client_order_id: u64, order_id: u64) -> Command {
    Command::Cancel(Cancel {
        client_order_id,
        order_id,
    })
}

fn cash(available: i64, held: i64) -> Balance {
    Balance {
        asset: Asset::Cash,
        available,
        held,
    }
}

fn pass(market_id: u32, available: i64, held: i64) -> Balance {
    Balance {
        asset: Asset::Pass { market_id },
        available,
        held,
    }
}

fn assert_rejected(response: &dx_types::CommandResponse, reason: ReasonCode) {
    assert_eq!(response.status, OrderStatus::Rejected, "{response:?}");
    assert_eq!(response.reason, Some(reason), "{response:?}");
}

#[test]
fn buy_taker_pays_fee_and_receives_passes() {
    let mut core = core();
    core.deposit_cash(1, 2_000_000, 1).unwrap();
    core.issue_passes(2, MARKET, 2, 1).unwrap();

    let sell = core.process(2, limit(1, Side::Sell, PRICE, 2));
    assert_eq!(sell.status, OrderStatus::Rested);

    let buy = core.process(1, limit(1, Side::Buy, PRICE, 2));
    assert_eq!(buy.status, OrderStatus::Filled);
    assert_eq!(buy.filled_qty, 2);
    assert_eq!(buy.remaining_qty, 0);
    assert_eq!(buy.fills.len(), 1);
    assert_eq!(buy.fills[0].price_ticks, PRICE);
    assert_eq!(buy.fills[0].qty, 2);
    assert_eq!(buy.fills[0].maker_user_id, 2);
    assert_eq!(buy.fills[0].taker_user_id, 1);

    // 1,000,000 notional + 10,000 taker fee.
    assert_eq!(core.balance(1, Asset::Cash), cash(990_000, 0));
    assert_eq!(core.balance(2, Asset::Cash), cash(1_000_000, 0));
    assert_eq!(
        core.balance(1, Asset::Pass { market_id: MARKET }),
        pass(MARKET, 2, 0)
    );
    assert_eq!(
        core.balance(2, Asset::Pass { market_id: MARKET }),
        pass(MARKET, 0, 0)
    );
    core.check_invariants().unwrap();
}

#[test]
fn seller_taker_pays_fee_from_proceeds() {
    let mut core = core();
    core.deposit_cash(1, 2_000_000, 1).unwrap();
    core.issue_passes(2, MARKET, 1, 1).unwrap();

    let bid = core.process(1, limit(1, Side::Buy, PRICE, 1));
    assert_eq!(bid.status, OrderStatus::Rested);
    assert_eq!(core.balance(1, Asset::Cash), cash(1_500_000, 500_000));

    let sell = core.process(2, limit(2, Side::Sell, PRICE, 1));
    assert_eq!(sell.status, OrderStatus::Filled);
    assert_eq!(sell.fills.len(), 1);

    assert_eq!(core.balance(2, Asset::Cash), cash(495_000, 0));
    assert_eq!(core.balance(1, Asset::Cash), cash(1_500_000, 0));
    assert_eq!(
        core.balance(1, Asset::Pass { market_id: MARKET }),
        pass(MARKET, 1, 0)
    );
    core.check_invariants().unwrap();
}

#[test]
fn cancel_releases_canonical_collateral() {
    let mut core = core();
    core.deposit_cash(1, 1_000_000, 1).unwrap();

    let bid = core.process(1, limit(1, Side::Buy, PRICE, 1));
    assert_eq!(bid.status, OrderStatus::Rested);
    assert_eq!(core.balance(1, Asset::Cash), cash(500_000, 500_000));
    assert_eq!(core.open_buy_qty(1, MARKET), 1);

    let cancelled = core.process(1, cancel(2, bid.order_id.unwrap()));
    assert_eq!(cancelled.status, OrderStatus::Cancelled);
    assert_eq!(cancelled.remaining_qty, 1);
    assert_eq!(core.balance(1, Asset::Cash), cash(1_000_000, 0));
    assert_eq!(core.open_buy_qty(1, MARKET), 0);
    core.check_invariants().unwrap();
}

#[test]
fn holding_limit_counts_passes_and_open_buys() {
    let mut core = core();
    core.deposit_cash(1, 10_000_000, 1).unwrap();
    let mut order_ids = Vec::new();

    for index in 0..4 {
        let response = core.process(1, limit(10 + index, Side::Buy, 4_000, 1));
        assert_eq!(response.status, OrderStatus::Rested, "{response:?}");
        order_ids.push(response.order_id.unwrap());
    }
    assert_eq!(core.open_buy_qty(1, MARKET), 4);

    let rejected = core.process(1, limit(20, Side::Buy, 4_000, 1));
    assert_rejected(&rejected, ReasonCode::HoldingLimitExceeded);

    let cancelled = core.process(1, cancel(21, order_ids[0]));
    assert_eq!(cancelled.status, OrderStatus::Cancelled);
    assert_eq!(core.open_buy_qty(1, MARKET), 3);

    let accepted = core.process(1, limit(22, Side::Buy, 4_000, 1));
    assert_eq!(accepted.status, OrderStatus::Rested);
    assert_eq!(core.open_buy_qty(1, MARKET), 4);
    core.check_invariants().unwrap();
}

#[test]
fn holding_limit_allows_selling_all_held_passes() {
    let mut core = core();
    core.issue_passes(1, MARKET, 4, 1).unwrap();

    for index in 0..4 {
        let response = core.process(1, limit(10 + index, Side::Sell, PRICE, 1));
        assert_eq!(response.status, OrderStatus::Rested, "{response:?}");
    }
    assert_eq!(
        core.balance(1, Asset::Pass { market_id: MARKET }),
        Balance {
            asset: Asset::Pass { market_id: MARKET },
            available: 0,
            held: 4,
        }
    );

    let rejected = core.process(1, limit(20, Side::Sell, PRICE, 1));
    assert_rejected(&rejected, ReasonCode::InsufficientInventory);
    core.check_invariants().unwrap();
}

#[test]
fn insufficient_funds_is_rejected_without_mutating() {
    let mut core = core();
    core.deposit_cash(1, 100, 1).unwrap();

    let rejected = core.process(1, limit(1, Side::Buy, PRICE, 1));
    assert_rejected(&rejected, ReasonCode::InsufficientFunds);
    assert_eq!(core.balance(1, Asset::Cash), cash(100, 0));
    assert_eq!(core.engine().book(MARKET).unwrap().open_order_count(), 0);
    core.check_invariants().unwrap();
}

#[test]
fn insufficient_inventory_is_rejected_without_mutating() {
    let mut core = core();

    let rejected = core.process(2, limit(1, Side::Sell, PRICE, 1));
    assert_rejected(&rejected, ReasonCode::InsufficientInventory);
    assert_eq!(core.engine().book(MARKET).unwrap().open_order_count(), 0);
    core.check_invariants().unwrap();
}

#[test]
fn place_is_idempotent_per_user_and_client_order_id() {
    let mut core = core();
    core.deposit_cash(1, 1_000_000, 1).unwrap();

    let command = limit(1, Side::Buy, PRICE, 1);
    let first = core.process(1, command.clone());
    let second = core.process(1, command);
    assert_eq!(first, second);
    assert_eq!(core.engine().book(MARKET).unwrap().open_order_count(), 1);
    assert_eq!(core.balance(1, Asset::Cash), cash(500_000, 500_000));

    // Reusing the id for a different command kind is a conflict, not a cancel.
    let conflicting = core.process(1, cancel(1, first.order_id.unwrap()));
    assert_rejected(&conflicting, ReasonCode::DuplicateClientOrderId);
    assert_eq!(core.engine().book(MARKET).unwrap().open_order_count(), 1);
    core.check_invariants().unwrap();
}

#[test]
fn cancel_is_idempotent() {
    let mut core = core();
    core.deposit_cash(1, 1_000_000, 1).unwrap();
    let bid = core.process(1, limit(1, Side::Buy, PRICE, 1));
    let cancel_command = cancel(2, bid.order_id.unwrap());

    let first = core.process(1, cancel_command.clone());
    let second = core.process(1, cancel_command);
    assert_eq!(first, second);
    assert_eq!(first.status, OrderStatus::Cancelled);
    assert_eq!(core.balance(1, Asset::Cash), cash(1_000_000, 0));
    core.check_invariants().unwrap();
}

#[test]
fn collar_quantity_and_market_are_validated() {
    let mut core = core();
    core.deposit_cash(1, 1_000_000, 1).unwrap();

    let below = core.process(1, limit(1, Side::Buy, 2_499, 1));
    assert_rejected(&below, ReasonCode::OutsideCollar);
    let above = core.process(1, limit(2, Side::Buy, 6_001, 1));
    assert_rejected(&above, ReasonCode::OutsideCollar);
    let zero = core.process(1, limit(3, Side::Buy, PRICE, 0));
    assert_rejected(&zero, ReasonCode::InvalidQuantity);

    let unknown = core.process(
        1,
        Command::PlaceLimit(PlaceLimit {
            client_order_id: 4,
            market_id: 999,
            side: Side::Buy,
            price_ticks: PRICE,
            qty: 1,
        }),
    );
    assert_rejected(&unknown, ReasonCode::UnknownMarket);
    assert_eq!(core.balance(1, Asset::Cash), cash(1_000_000, 0));
    core.check_invariants().unwrap();
}

#[test]
fn market_buy_sweeps_and_releases_the_unfilled_reservation() {
    let mut core = core();
    core.deposit_cash(1, 2_000_000, 1).unwrap();
    core.issue_passes(2, MARKET, 2, 1).unwrap();
    core.process(2, limit(1, Side::Sell, PRICE, 2));

    let response = core.process(1, market_order(1, Side::Buy, 3, PRICE));
    assert_eq!(response.status, OrderStatus::Cancelled);
    assert_eq!(response.filled_qty, 2);
    assert_eq!(response.remaining_qty, 1);
    assert_eq!(response.fills.len(), 1);

    assert_eq!(core.balance(1, Asset::Cash), cash(990_000, 0));
    assert_eq!(
        core.balance(1, Asset::Pass { market_id: MARKET }),
        pass(MARKET, 2, 0)
    );
    assert_eq!(core.open_buy_qty(1, MARKET), 0);
    let snapshot = core.snapshot(MARKET, 10).unwrap();
    assert!(snapshot.asks.is_empty());
    assert!(snapshot.bids.is_empty());
    core.check_invariants().unwrap();
}

#[test]
fn self_trade_cancellation_releases_resting_collateral() {
    let mut core = core();
    core.deposit_cash(1, 10_000_000, 1).unwrap();
    core.issue_passes(1, MARKET, 2, 1).unwrap();

    let bid = core.process(1, limit(1, Side::Buy, PRICE, 2));
    assert_eq!(bid.status, OrderStatus::Rested);
    assert_eq!(core.open_buy_qty(1, MARKET), 2);
    assert_eq!(core.balance(1, Asset::Cash), cash(9_000_000, 1_000_000));

    let sell = core.process(1, limit(2, Side::Sell, PRICE, 2));
    assert_eq!(sell.status, OrderStatus::Rested);
    assert_eq!(sell.fills.len(), 0);
    assert_eq!(core.open_buy_qty(1, MARKET), 0);
    assert_eq!(core.balance(1, Asset::Cash), cash(10_000_000, 0));
    assert_eq!(
        core.balance(1, Asset::Pass { market_id: MARKET }),
        pass(MARKET, 0, 2)
    );

    let snapshot = core.snapshot(MARKET, 10).unwrap();
    assert!(snapshot.bids.is_empty());
    assert_eq!(
        snapshot.asks,
        vec![BookLevel {
            price_ticks: PRICE,
            qty: 2
        }]
    );
    core.check_invariants().unwrap();
}

#[test]
fn maker_partial_fill_then_cancel_keeps_books_and_ledger_in_sync() {
    let mut core = core();
    core.deposit_cash(1, 2_000_000, 1).unwrap();
    core.issue_passes(2, MARKET, 2, 1).unwrap();

    let bid = core.process(1, limit(1, Side::Buy, PRICE, 2));
    assert_eq!(bid.status, OrderStatus::Rested);
    assert_eq!(core.balance(1, Asset::Cash), cash(1_000_000, 1_000_000));

    let sell = core.process(2, limit(1, Side::Sell, PRICE, 1));
    assert_eq!(sell.status, OrderStatus::Filled);
    assert_eq!(core.balance(1, Asset::Cash), cash(1_000_000, 500_000));
    assert_eq!(core.open_buy_qty(1, MARKET), 1);

    let cancelled = core.process(1, cancel(2, bid.order_id.unwrap()));
    assert_eq!(cancelled.status, OrderStatus::Cancelled);
    assert_eq!(cancelled.remaining_qty, 1);
    assert_eq!(core.balance(1, Asset::Cash), cash(1_500_000, 0));
    assert_eq!(core.open_buy_qty(1, MARKET), 0);
    core.check_invariants().unwrap();
}

#[test]
fn state_hash_is_deterministic_and_changes_with_state() {
    let mut a = core();
    let mut b = core();
    for core in [&mut a, &mut b] {
        core.deposit_cash(1, 1_000_000, 1).unwrap();
        core.process(1, limit(1, Side::Buy, PRICE, 1));
    }
    assert_eq!(a.state_hash(), b.state_hash());

    b.process(1, limit(2, Side::Buy, 4_000, 1));
    assert_ne!(a.state_hash(), b.state_hash());
}

#[test]
fn unknown_cancel_and_wrong_owner_are_rejected() {
    let mut core = core();
    core.deposit_cash(1, 1_000_000, 1).unwrap();
    let bid = core.process(1, limit(1, Side::Buy, PRICE, 1));

    let missing = core.process(1, cancel(2, 999_999));
    assert_rejected(&missing, ReasonCode::OrderNotFound);

    let wrong_owner = core.process(2, cancel(3, bid.order_id.unwrap()));
    assert_rejected(&wrong_owner, ReasonCode::NotOrderOwner);
    core.check_invariants().unwrap();
}

#[test]
fn deltas_reconstruct_snapshots_for_rests_and_cancels() {
    let mut core = core();
    core.deposit_cash(1, 2_000_000, 1).unwrap();

    let before = core.snapshot(MARKET, 10).unwrap();
    let first = core.process_with_deltas(1, limit(1, Side::Buy, 4_900, 2));
    let after = core.snapshot(MARKET, 10).unwrap();
    assert_eq!(first.deltas.len(), 1);
    assert_eq!(
        first.deltas[0].changes,
        vec![LevelChange {
            side: Side::Buy,
            price_ticks: 4_900,
            qty: 2,
        }]
    );
    reconstruct_without_tape(&before, &first.deltas, &after);

    let before = after;
    let second = core.process_with_deltas(1, limit(2, Side::Buy, 4_800, 1));
    let after = core.snapshot(MARKET, 10).unwrap();
    assert_eq!(second.deltas.len(), 1);
    assert_eq!(
        second.deltas[0].changes,
        vec![LevelChange {
            side: Side::Buy,
            price_ticks: 4_800,
            qty: 1,
        }]
    );
    reconstruct_without_tape(&before, &second.deltas, &after);

    let cancelled_id = second.response.order_id.unwrap();
    let before = after;
    let cancelled = core.process_with_deltas(1, cancel(3, cancelled_id));
    let after = core.snapshot(MARKET, 10).unwrap();
    assert_eq!(
        cancelled.deltas[0].changes,
        vec![LevelChange {
            side: Side::Buy,
            price_ticks: 4_800,
            qty: 0,
        }]
    );
    reconstruct_without_tape(&before, &cancelled.deltas, &after);
    core.check_invariants().unwrap();
}

#[test]
fn trade_delta_and_fills_reconstruct_the_next_snapshot() {
    let mut core = core();
    core.deposit_cash(1, 2_000_000, 1).unwrap();
    core.issue_passes(2, MARKET, 2, 1).unwrap();
    core.process(2, limit(1, Side::Sell, PRICE, 2));

    let before = core.snapshot(MARKET, 10).unwrap();
    let outcome = core.process_with_deltas(1, limit(2, Side::Buy, PRICE, 3));
    let after = core.snapshot(MARKET, 10).unwrap();

    assert_eq!(outcome.response.status, OrderStatus::PartiallyFilled);
    assert_eq!(outcome.fills().len(), 1);
    assert_eq!(outcome.deltas.len(), 1);
    assert_eq!(outcome.deltas[0].book_seq, before.book_seq + 1);
    assert_eq!(
        outcome.deltas[0].changes,
        vec![
            LevelChange {
                side: Side::Sell,
                price_ticks: PRICE,
                qty: 0,
            },
            LevelChange {
                side: Side::Buy,
                price_ticks: PRICE,
                qty: 1,
            },
        ]
    );
    assert_eq!(after.book_seq, outcome.deltas[0].book_seq);
    assert_eq!(after.last_trade_price, Some(PRICE));
    reconstruct_with_tape(&outcome, &before, &after);
    core.check_invariants().unwrap();
}

#[test]
fn book_seq_only_advances_on_real_book_changes() {
    let mut core = core();
    core.deposit_cash(1, 2_000_000, 1).unwrap();

    let before = core.snapshot(MARKET, 10).unwrap();
    let outcome = core.process_with_deltas(1, market_order(1, Side::Buy, 2, PRICE));
    let after = core.snapshot(MARKET, 10).unwrap();

    assert_eq!(outcome.response.status, OrderStatus::Cancelled);
    assert!(outcome.response.fills.is_empty());
    assert!(outcome.deltas.is_empty());
    assert_eq!(before.book_seq, 0);
    assert_eq!(after, before);
    assert_eq!(core.engine().book(MARKET).unwrap().book_seq(), 0);

    // The empty market order did not create a gap in the per-market sequence.
    let rested = core.process_with_deltas(1, limit(2, Side::Buy, 4_900, 1));
    assert_eq!(rested.deltas.len(), 1);
    assert_eq!(rested.deltas[0].book_seq, 1);
    assert_eq!(core.snapshot(MARKET, 10).unwrap().book_seq, 1);
    core.check_invariants().unwrap();
}

#[test]
fn duplicate_and_rejected_commands_emit_no_deltas() {
    let mut core = core();
    core.deposit_cash(1, 2_000_000, 1).unwrap();

    let command = limit(1, Side::Buy, PRICE, 1);
    let first = core.process_with_deltas(1, command.clone());
    assert_eq!(first.deltas.len(), 1);
    assert!(!first.cached);
    let after_first = core.snapshot(MARKET, 10).unwrap();
    assert_eq!(after_first.book_seq, 1);

    let duplicate = core.process_with_deltas(1, command);
    assert_eq!(duplicate.response, first.response);
    assert!(duplicate.cached);
    assert!(duplicate.deltas.is_empty());
    assert_eq!(core.snapshot(MARKET, 10).unwrap(), after_first);

    let rejected = core.process_with_deltas(1, limit(2, Side::Buy, 2_499, 1));
    assert_rejected(&rejected.response, ReasonCode::OutsideCollar);
    assert!(rejected.deltas.is_empty());
    assert_eq!(core.snapshot(MARKET, 10).unwrap(), after_first);

    let conflicting_kind = core.process_with_deltas(1, cancel(1, first.response.order_id.unwrap()));
    assert_rejected(
        &conflicting_kind.response,
        ReasonCode::DuplicateClientOrderId,
    );
    assert!(conflicting_kind.deltas.is_empty());
    assert_eq!(core.snapshot(MARKET, 10).unwrap(), after_first);
    core.check_invariants().unwrap();
}

#[test]
fn replay_produces_identical_responses_and_deltas() {
    fn seeded() -> Core {
        let mut core = core();
        core.deposit_cash(1, 10_000_000, 1).unwrap();
        core.issue_passes(2, MARKET, 2, 1).unwrap();
        core
    }

    fn run(core: &mut Core) -> Vec<CommandOutcome> {
        let mut outcomes = vec![
            core.process_with_deltas(2, limit(1, Side::Sell, PRICE, 2)),
            core.process_with_deltas(1, limit(2, Side::Buy, 4_900, 2)),
            core.process_with_deltas(1, limit(2, Side::Buy, 4_900, 2)),
            core.process_with_deltas(1, limit(3, Side::Buy, 2_499, 1)),
        ];
        let bid_id = outcomes[1].response.order_id.unwrap();
        outcomes.push(core.process_with_deltas(1, cancel(4, bid_id)));
        outcomes.push(core.process_with_deltas(1, market_order(5, Side::Buy, 1, PRICE)));
        outcomes
    }

    let mut a = seeded();
    let mut b = seeded();
    let a_outcomes = run(&mut a);
    let b_outcomes = run(&mut b);

    assert_eq!(a_outcomes, b_outcomes);
    assert!(a_outcomes.iter().any(|outcome| !outcome.deltas.is_empty()));
    assert!(a_outcomes[2].deltas.is_empty(), "duplicate must be silent");
    assert!(a_outcomes[3].deltas.is_empty(), "reject must be silent");
    assert_eq!(a.state_hash(), b.state_hash());
    a.check_invariants().unwrap();
    b.check_invariants().unwrap();
}
