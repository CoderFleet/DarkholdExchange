use dx_core::{CommandOutcome, Core};
use dx_types::{
    AcceptDutchAuction, Asset, AuctionStatus, Balance, BookDelta, BookLevel, BookSnapshot, Cancel,
    Command, CreateDutchAuction, DeliveryReceipt, EscrowState, LevelChange, OrderStatus,
    PlaceLimit, PlaceMarket, Qty, ReasonCode, Side, Ticks, seed_markets,
};
use ed25519_dalek::{Signer, SigningKey};

const MARKET: u32 = 1;
const PRICE: Ticks = 5_000;
const ESCROW_MARKET: u32 = 3;
const ESCROW_PRICE: Ticks = 4_000;

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
        escrow: false,
    })
}

fn escrow_limit(client_order_id: u64, side: Side, price_ticks: Ticks, qty: Qty) -> Command {
    Command::PlaceLimit(PlaceLimit {
        client_order_id,
        market_id: ESCROW_MARKET,
        side,
        price_ticks,
        qty,
        escrow: false,
    })
}

fn signing_key() -> SigningKey {
    SigningKey::from_bytes(&[7u8; 32])
}

fn receipt_for(core: &Core, trade_id: u64, delivered_at_ms: u64) -> DeliveryReceipt {
    let record = core.escrow_record(trade_id).expect("escrow record");
    DeliveryReceipt {
        trade_id: record.trade_id,
        market_id: record.market_id,
        qty: record.qty,
        buyer_id: record.buyer_id,
        seller_id: record.seller_id,
        delivered_at_ms,
    }
}

fn market_order(client_order_id: u64, side: Side, qty: Qty, protect: Ticks) -> Command {
    Command::PlaceMarket(PlaceMarket {
        client_order_id,
        market_id: MARKET,
        side,
        qty,
        protect_price_ticks: protect,
        escrow: false,
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
            escrow: false,
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
fn runtime_market_is_tradable_and_consumes_a_sequence() {
    let mut core = core();
    let mut config = seed_markets().into_iter().next().unwrap();
    config.id = 100;
    config.event_id = 99;
    config.name = "Runtime market".into();

    core.add_market(config.clone()).unwrap();
    assert_eq!(core.last_seq(), 1);
    assert_eq!(
        core.config(100).unwrap().status,
        dx_types::MarketStatus::Open
    );
    assert!(core.issue_passes(2, 100, 2, 1).is_ok());
    core.deposit_cash(1, 2_000_000, 1).unwrap();

    let ask = core.process(
        2,
        Command::PlaceLimit(PlaceLimit {
            client_order_id: 1,
            market_id: 100,
            side: Side::Sell,
            price_ticks: PRICE,
            qty: 2,
            escrow: false,
        }),
    );
    assert_eq!(ask.status, OrderStatus::Rested);

    let buy = core.process(
        1,
        Command::PlaceMarket(PlaceMarket {
            client_order_id: 2,
            market_id: 100,
            side: Side::Buy,
            qty: 2,
            protect_price_ticks: PRICE,
            escrow: false,
        }),
    );
    assert_eq!(buy.status, OrderStatus::Filled);
    assert_eq!(
        core.snapshot(100, 10).unwrap().last_trade_price,
        Some(PRICE)
    );
    assert!(core.add_market(config).is_err());
    core.check_invariants().unwrap();
}

#[test]
fn state_version_tracks_commands_and_journal_operations() {
    let mut core = core();
    assert_eq!(core.state_version(), 0);

    core.deposit_cash(1, 1_000_000, 1).unwrap();
    assert_eq!(core.state_version(), 1);

    // Idempotent duplicate deposits do not post another journal.
    core.deposit_cash(1, 1_000_000, 1).unwrap();
    assert_eq!(core.state_version(), 1);

    let bid = core.process(1, limit(1, Side::Buy, PRICE, 1));
    assert_eq!(bid.status, OrderStatus::Rested);
    assert_eq!(core.state_version(), 2);

    // A cached retry does not create a new version.
    let retry = core.process_with_deltas(1, limit(1, Side::Buy, PRICE, 1));
    assert!(retry.cached);
    assert_eq!(core.state_version(), 2);

    let mut config = seed_markets().into_iter().next().unwrap();
    config.id = 100;
    config.event_id = 99;
    config.name = "Version market".into();
    core.add_market(config).unwrap();
    assert_eq!(core.state_version(), 3);
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

#[test]
fn escrow_sell_requires_a_registered_seller_key() {
    let mut core = core();
    core.deposit_cash(1, 2_000_000, 1).unwrap();
    core.issue_passes(2, ESCROW_MARKET, 2, 1).unwrap();

    let rejected = core.process(2, escrow_limit(1, Side::Sell, ESCROW_PRICE, 2));
    assert_rejected(&rejected, ReasonCode::EscrowNotReady);

    core.register_public_key(2, signing_key().verifying_key().to_bytes())
        .unwrap();
    let rested = core.process(2, escrow_limit(2, Side::Sell, ESCROW_PRICE, 2));
    assert_eq!(rested.status, OrderStatus::Rested);
    core.check_invariants().unwrap();
}

#[test]
fn escrow_fill_holds_cash_and_passes_until_receipt() {
    let mut core = core();
    let key = signing_key();
    core.register_public_key(2, key.verifying_key().to_bytes())
        .unwrap();
    core.set_current_time_ms(10_000);
    core.deposit_cash(1, 2_000_000, 1).unwrap();
    core.issue_passes(2, ESCROW_MARKET, 2, 1).unwrap();

    let sell = core.process(2, escrow_limit(1, Side::Sell, ESCROW_PRICE, 2));
    assert_eq!(sell.status, OrderStatus::Rested);
    let buy = core.process(1, escrow_limit(1, Side::Buy, ESCROW_PRICE, 2));
    assert_eq!(buy.status, OrderStatus::Filled);
    let trade_id = buy.fills[0].trade_id;

    // 400,000 notional + 6,000 taker fee is in escrow, not paid to the seller.
    assert_eq!(core.balance(1, Asset::Cash), cash(1_594_000, 0));
    assert_eq!(core.balance(2, Asset::Cash), cash(0, 0));
    assert_eq!(
        core.balance(
            1,
            Asset::Pass {
                market_id: ESCROW_MARKET
            }
        ),
        pass(ESCROW_MARKET, 0, 0)
    );
    assert_eq!(
        core.balance(
            2,
            Asset::Pass {
                market_id: ESCROW_MARKET
            }
        ),
        pass(ESCROW_MARKET, 0, 0)
    );
    let record = core.escrow_record(trade_id).unwrap();
    assert_eq!(record.state, EscrowState::PendingDelivery);
    assert_eq!(record.deadline_ms, 10_000 + dx_core::ESCROW_TIMEOUT_MS);
    core.check_invariants().unwrap();

    let receipt = receipt_for(&core, trade_id, 20_000);
    let signature = key.sign(&receipt.canonical_bytes()).to_bytes();
    let released = core
        .release_escrow(2, trade_id, 20_000, &signature, 21_000)
        .unwrap();
    assert_eq!(released.state, EscrowState::Delivered);
    assert_eq!(
        core.balance(
            1,
            Asset::Pass {
                market_id: ESCROW_MARKET
            }
        ),
        pass(ESCROW_MARKET, 2, 0)
    );
    assert_eq!(core.balance(2, Asset::Cash), cash(400_000, 0));
    assert_eq!(core.balance(1, Asset::Cash), cash(1_594_000, 0));
    core.check_invariants().unwrap();
}

#[test]
fn escrow_release_rejects_wrong_signature_and_expired_receipt() {
    let mut core = core();
    let key = signing_key();
    core.register_public_key(2, key.verifying_key().to_bytes())
        .unwrap();
    core.set_current_time_ms(1_000);
    core.deposit_cash(1, 2_000_000, 1).unwrap();
    core.issue_passes(2, ESCROW_MARKET, 2, 1).unwrap();
    core.process(2, escrow_limit(1, Side::Sell, ESCROW_PRICE, 2));
    let buy = core.process(1, escrow_limit(1, Side::Buy, ESCROW_PRICE, 2));
    let trade_id = buy.fills[0].trade_id;
    let receipt = receipt_for(&core, trade_id, 2_000);

    let wrong_key = SigningKey::from_bytes(&[9u8; 32]);
    let wrong_signature = wrong_key.sign(&receipt.canonical_bytes()).to_bytes();
    assert_eq!(
        core.release_escrow(2, trade_id, 2_000, &wrong_signature, 3_000),
        Err(ReasonCode::InvalidReceipt)
    );

    let signature = key.sign(&receipt.canonical_bytes()).to_bytes();
    assert_eq!(
        core.release_escrow(
            2,
            trade_id,
            1_000 + dx_core::ESCROW_TIMEOUT_MS + 1,
            &signature,
            1_000 + dx_core::ESCROW_TIMEOUT_MS + 2,
        ),
        Err(ReasonCode::ReceiptExpired)
    );
    assert_eq!(
        core.escrow_record(trade_id).unwrap().state,
        EscrowState::PendingDelivery
    );
    core.check_invariants().unwrap();
}

#[test]
fn escrow_refund_waits_for_deadline_and_returns_both_sides() {
    let mut core = core();
    let key = signing_key();
    core.register_public_key(2, key.verifying_key().to_bytes())
        .unwrap();
    core.set_current_time_ms(1_000);
    core.deposit_cash(1, 2_000_000, 1).unwrap();
    core.issue_passes(2, ESCROW_MARKET, 2, 1).unwrap();
    core.process(2, escrow_limit(1, Side::Sell, ESCROW_PRICE, 2));
    let buy = core.process(1, escrow_limit(1, Side::Buy, ESCROW_PRICE, 2));
    let trade_id = buy.fills[0].trade_id;

    assert_eq!(
        core.refund_escrow(1, trade_id, 2_000),
        Err(ReasonCode::EscrowNotReady)
    );
    let refunded = core
        .refund_escrow(1, trade_id, 1_000 + dx_core::ESCROW_TIMEOUT_MS)
        .unwrap();
    assert_eq!(refunded.state, EscrowState::Refunded);
    assert_eq!(core.balance(1, Asset::Cash), cash(2_000_000, 0));
    assert_eq!(
        core.balance(
            1,
            Asset::Pass {
                market_id: ESCROW_MARKET
            }
        ),
        pass(ESCROW_MARKET, 0, 0)
    );
    assert_eq!(
        core.balance(
            2,
            Asset::Pass {
                market_id: ESCROW_MARKET
            }
        ),
        pass(ESCROW_MARKET, 2, 0)
    );
    core.check_invariants().unwrap();
}

#[test]
fn pending_escrow_passes_count_toward_the_buyer_holding_limit() {
    let mut core = core();
    let key = signing_key();
    core.register_public_key(2, key.verifying_key().to_bytes())
        .unwrap();
    core.set_current_time_ms(10_000);
    core.deposit_cash(1, 2_000_000, 1).unwrap();
    core.issue_passes(2, ESCROW_MARKET, 6, 1).unwrap();

    core.process(2, escrow_limit(1, Side::Sell, ESCROW_PRICE, 2));
    let first = core.process(1, escrow_limit(1, Side::Buy, ESCROW_PRICE, 2));
    assert_eq!(first.status, OrderStatus::Filled);
    assert_eq!(first.fills[0].qty, 2);

    core.process(2, escrow_limit(2, Side::Sell, ESCROW_PRICE, 4));
    let over_limit = core.process(1, escrow_limit(2, Side::Buy, ESCROW_PRICE, 4));
    assert_rejected(&over_limit, ReasonCode::HoldingLimitExceeded);

    let fills_limit = core.process(1, escrow_limit(3, Side::Buy, ESCROW_PRICE, 3));
    assert_eq!(fills_limit.status, OrderStatus::Filled);
    let pending: Qty = core
        .escrow_records(1)
        .iter()
        .filter(|record| record.state == EscrowState::PendingDelivery)
        .map(|record| record.qty)
        .sum();
    assert_eq!(pending, 5);
    core.check_invariants().unwrap();
}

fn auction_create_request(
    client_order_id: u64,
    market_id: u32,
    qty: Qty,
    start: Ticks,
    floor: Ticks,
    duration_ms: u64,
) -> CreateDutchAuction {
    CreateDutchAuction {
        client_order_id,
        market_id,
        qty,
        start_price_ticks: start,
        floor_price_ticks: floor,
        duration_ms,
    }
}

fn auction_accept_request(
    client_order_id: u64,
    auction_id: u64,
    qty: Qty,
    max_price_ticks: Ticks,
) -> AcceptDutchAuction {
    AcceptDutchAuction {
        client_order_id,
        auction_id,
        qty,
        max_price_ticks,
    }
}

#[test]
fn dutch_auction_reserves_passes_and_settles_at_the_decayed_price() {
    let mut core = core();
    core.deposit_cash(1, 1_000_000, 1).unwrap();
    core.issue_passes(2, MARKET, 10, 1).unwrap();

    let start = 1_000_000;
    let auction = core
        .create_dutch_auction(
            2,
            auction_create_request(1, MARKET, 4, 5_000, 2_500, 10_000),
            start,
        )
        .unwrap();
    assert_eq!(auction.id, 1);
    assert_eq!(auction.status, AuctionStatus::Active);
    assert_eq!(auction.qty_remaining, 4);
    assert_eq!(
        core.balance(2, Asset::Pass { market_id: MARKET }),
        pass(MARKET, 6, 4)
    );
    core.check_invariants().unwrap();

    // Halfway through the window the price is 3,750 ticks.
    let accepted = core
        .accept_dutch_auction(
            1,
            auction_accept_request(1, auction.id, 2, 3_800),
            start + 5_000,
        )
        .unwrap();
    assert_eq!(accepted.price_ticks, 3_750);
    assert_eq!(accepted.qty, 2);
    assert_eq!(accepted.notional, 750_000);
    assert_eq!(accepted.fee, 7_500);

    assert_eq!(core.balance(1, Asset::Cash), cash(242_500, 0));
    assert_eq!(
        core.balance(1, Asset::Pass { market_id: MARKET }),
        pass(MARKET, 2, 0)
    );
    assert_eq!(core.balance(2, Asset::Cash), cash(750_000, 0));
    assert_eq!(
        core.balance(2, Asset::Pass { market_id: MARKET }),
        pass(MARKET, 6, 2)
    );

    let updated = core.dutch_auction(auction.id).unwrap();
    assert_eq!(updated.qty_remaining, 2);
    assert_eq!(updated.status, AuctionStatus::Active);
    core.check_invariants().unwrap();
}

#[test]
fn dutch_auction_accept_is_compare_and_set_and_idempotent() {
    let mut core = core();
    core.deposit_cash(1, 1_000_000, 1).unwrap();
    core.issue_passes(2, MARKET, 10, 1).unwrap();
    let start = 1_000;
    let auction = core
        .create_dutch_auction(
            2,
            auction_create_request(1, MARKET, 2, 5_000, 2_500, 10_000),
            start,
        )
        .unwrap();

    assert_eq!(
        core.accept_dutch_auction(1, auction_accept_request(1, auction.id, 3, 5_000), start),
        Err(ReasonCode::InvalidQuantity)
    );
    assert_eq!(
        core.accept_dutch_auction(1, auction_accept_request(2, auction.id, 1, 4_999), start),
        Err(ReasonCode::StaleAuctionPrice)
    );
    assert_eq!(core.balance(1, Asset::Cash), cash(1_000_000, 0));

    let first = core
        .accept_dutch_auction(1, auction_accept_request(3, auction.id, 1, 5_000), start)
        .unwrap();
    let cash_after = core.balance(1, Asset::Cash);
    let retry = core
        .accept_dutch_auction(1, auction_accept_request(3, auction.id, 1, 5_000), start)
        .unwrap();
    assert_eq!(retry, first);
    assert_eq!(core.balance(1, Asset::Cash), cash_after);
    assert_eq!(core.dutch_auction(auction.id).unwrap().qty_remaining, 1);
    core.check_invariants().unwrap();
}

#[test]
fn dutch_auction_sells_out_and_then_closes() {
    let mut core = core();
    core.deposit_cash(1, 2_000_000, 1).unwrap();
    core.issue_passes(2, MARKET, 4, 1).unwrap();
    let start = 5_000;
    let auction = core
        .create_dutch_auction(
            2,
            auction_create_request(1, MARKET, 2, 5_000, 2_500, 10_000),
            start,
        )
        .unwrap();
    core.accept_dutch_auction(1, auction_accept_request(1, auction.id, 2, 5_000), start)
        .unwrap();

    let sold = core.dutch_auction(auction.id).unwrap();
    assert_eq!(sold.status, AuctionStatus::Sold);
    assert_eq!(sold.qty_remaining, 0);
    assert_eq!(
        core.accept_dutch_auction(1, auction_accept_request(2, auction.id, 1, 5_000), start),
        Err(ReasonCode::AuctionClosed)
    );
    assert_eq!(
        core.expire_dutch_auction(2, auction.id, start + 10_000),
        Err(ReasonCode::AuctionNotActive)
    );
    core.check_invariants().unwrap();
}

#[test]
fn dutch_auction_expiry_returns_unsold_passes_to_the_seller() {
    let mut core = core();
    core.issue_passes(2, MARKET, 6, 1).unwrap();
    let start = 10_000;
    let auction = core
        .create_dutch_auction(
            2,
            auction_create_request(1, MARKET, 5, 5_000, 2_500, 10_000),
            start,
        )
        .unwrap();
    assert_eq!(
        core.balance(2, Asset::Pass { market_id: MARKET }),
        pass(MARKET, 1, 5)
    );

    assert_eq!(
        core.expire_dutch_auction(2, auction.id, start + 9_999),
        Err(ReasonCode::AuctionNotActive)
    );
    assert_eq!(
        core.expire_dutch_auction(1, auction.id, start + 10_000),
        Err(ReasonCode::NotOrderOwner)
    );
    let expired = core
        .expire_dutch_auction(2, auction.id, start + 10_000)
        .unwrap();
    assert_eq!(expired.status, AuctionStatus::Expired);
    assert_eq!(expired.qty_remaining, 5);
    assert_eq!(
        core.balance(2, Asset::Pass { market_id: MARKET }),
        pass(MARKET, 6, 0)
    );
    assert_eq!(
        core.accept_dutch_auction(
            1,
            auction_accept_request(1, auction.id, 1, 5_000),
            start + 10_001
        ),
        Err(ReasonCode::AuctionClosed)
    );
    // Retrying expiry is harmless.
    assert_eq!(
        core.expire_dutch_auction(2, auction.id, start + 20_000),
        Ok(expired)
    );
    core.check_invariants().unwrap();
}

#[test]
fn dutch_auction_validates_parameters_and_inventory() {
    let mut core = core();
    core.issue_passes(2, MARKET, 1, 1).unwrap();

    assert_eq!(
        core.create_dutch_auction(
            2,
            auction_create_request(1, MARKET, 1, 2_500, 2_500, 1_000),
            0
        ),
        Err(ReasonCode::InvalidPrice)
    );
    assert_eq!(
        core.create_dutch_auction(2, auction_create_request(2, MARKET, 1, 5_000, 2_500, 0), 0),
        Err(ReasonCode::InvalidRequest)
    );
    assert_eq!(
        core.create_dutch_auction(
            2,
            auction_create_request(3, MARKET, 1, 6_001, 2_500, 1_000),
            0
        ),
        Err(ReasonCode::OutsideCollar)
    );
    assert_eq!(
        core.create_dutch_auction(
            2,
            auction_create_request(4, MARKET, 1, 5_000, 2_499, 1_000),
            0
        ),
        Err(ReasonCode::OutsideCollar)
    );
    assert_eq!(
        core.create_dutch_auction(
            2,
            auction_create_request(5, MARKET, 2, 5_000, 2_500, 1_000),
            0
        ),
        Err(ReasonCode::InsufficientInventory)
    );
    // Creating the same request twice is idempotent and reserves only once.
    let auction = core
        .create_dutch_auction(
            2,
            auction_create_request(6, MARKET, 1, 5_000, 2_500, 1_000),
            0,
        )
        .unwrap();
    let retry = core
        .create_dutch_auction(
            2,
            auction_create_request(6, MARKET, 1, 5_000, 2_500, 1_000),
            0,
        )
        .unwrap();
    assert_eq!(retry, auction);
    assert_eq!(
        core.balance(2, Asset::Pass { market_id: MARKET }),
        pass(MARKET, 0, 1)
    );
    core.check_invariants().unwrap();
}

#[test]
fn dutch_auction_accept_respects_the_holding_limit() {
    let mut core = core();
    core.deposit_cash(1, 5_000_000, 1).unwrap();
    core.issue_passes(2, MARKET, 6, 1).unwrap();
    let auction = core
        .create_dutch_auction(
            2,
            auction_create_request(1, MARKET, 6, 5_000, 2_500, 10_000),
            1_000,
        )
        .unwrap();

    // Market 1 allows four passes per identity.
    assert_eq!(
        core.accept_dutch_auction(1, auction_accept_request(1, auction.id, 5, 5_000), 1_000),
        Err(ReasonCode::HoldingLimitExceeded)
    );
    core.accept_dutch_auction(1, auction_accept_request(2, auction.id, 4, 5_000), 1_000)
        .unwrap();
    core.check_invariants().unwrap();
}

#[test]
fn dutch_auction_on_an_escrow_market_settles_without_a_receipt() {
    let mut core = core();
    core.deposit_cash(1, 2_000_000, 1).unwrap();
    core.issue_passes(2, ESCROW_MARKET, 2, 1).unwrap();
    let auction = core
        .create_dutch_auction(
            2,
            auction_create_request(1, ESCROW_MARKET, 2, 5_000, 4_000, 1_000),
            2_000,
        )
        .unwrap();
    let accepted = core
        .accept_dutch_auction(1, auction_accept_request(1, auction.id, 2, 5_000), 2_000)
        .unwrap();
    assert_eq!(accepted.price_ticks, 5_000);
    assert_eq!(
        core.balance(
            1,
            Asset::Pass {
                market_id: ESCROW_MARKET
            }
        ),
        pass(ESCROW_MARKET, 2, 0)
    );
    assert!(core.escrow_records(1).is_empty());
    core.check_invariants().unwrap();
}

#[test]
fn dutch_auction_state_hash_covers_auction_progress() {
    let mut core = core();
    core.deposit_cash(1, 1_000_000, 1).unwrap();
    core.issue_passes(2, MARKET, 2, 1).unwrap();
    let before = core.state_hash();
    let auction = core
        .create_dutch_auction(
            2,
            auction_create_request(1, MARKET, 2, 5_000, 2_500, 10_000),
            1_000,
        )
        .unwrap();
    let created = core.state_hash();
    assert_ne!(before, created);
    core.accept_dutch_auction(1, auction_accept_request(1, auction.id, 1, 5_000), 1_000)
        .unwrap();
    assert_ne!(created, core.state_hash());
}
