use dx_ledger::*;
use dx_types::{Asset, Balance};

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

#[test]
fn deposit_is_idempotent_and_balanced() {
    let mut ledger = Ledger::new();
    ledger.deposit_cash(1, 10_000, 7).unwrap();

    assert_eq!(ledger.balance(1, Asset::Cash), cash(10_000, 0));
    assert_eq!(ledger.journal_count(), 1);
    ledger.check_invariants().unwrap();

    // Same key, even with a different amount, must not double-credit.
    ledger.deposit_cash(1, 99_999, 7).unwrap();
    assert_eq!(ledger.balance(1, Asset::Cash), cash(10_000, 0));
    assert_eq!(ledger.journal_count(), 1);
}

#[test]
fn reserve_and_release_move_between_buckets() {
    let mut ledger = Ledger::new();
    ledger.deposit_cash(1, 10_000, 1).unwrap();
    ledger.reserve(1, Asset::Cash, 4_000).unwrap();

    assert_eq!(ledger.balance(1, Asset::Cash), cash(6_000, 4_000));
    assert_eq!(ledger.total_user_held(Asset::Cash), 4_000);

    ledger.release(1, Asset::Cash, 1_000).unwrap();
    assert_eq!(ledger.balance(1, Asset::Cash), cash(7_000, 3_000));
    assert_eq!(ledger.total_user_held(Asset::Cash), 3_000);
    ledger.check_invariants().unwrap();
}

#[test]
fn reserve_rejects_insufficient_funds_without_mutating() {
    let mut ledger = Ledger::new();
    ledger.deposit_cash(1, 1_000, 1).unwrap();

    let err = ledger.reserve(1, Asset::Cash, 1_001).unwrap_err();
    assert!(matches!(
        err,
        LedgerError::InsufficientFunds {
            needed: 1_001,
            available: 1_000,
            ..
        }
    ));
    assert_eq!(ledger.balance(1, Asset::Cash), cash(1_000, 0));
}

#[test]
fn issue_passes_conserves_supply() {
    let mut ledger = Ledger::new();
    ledger.issue_passes(2, 1, 3, 1).unwrap();

    assert_eq!(
        ledger.balance(2, Asset::Pass { market_id: 1 }),
        pass(1, 3, 0)
    );
    ledger.check_invariants().unwrap();
}

#[test]
fn settle_trade_moves_cash_and_passes_and_pays_buyer_fee() {
    let mut ledger = Ledger::new();
    ledger.deposit_cash(1, 10_000, 1).unwrap();
    ledger.issue_passes(2, 1, 2, 1).unwrap();
    ledger.reserve(1, Asset::Cash, 6_000).unwrap();
    ledger.reserve(2, Asset::Pass { market_id: 1 }, 2).unwrap();

    ledger
        .settle_trade(TradeSettlement {
            trade_id: 1,
            market_id: 1,
            buyer_id: 1,
            seller_id: 2,
            qty: 2,
            notional: 5_000,
            fee: 50,
            fee_payer: 1,
            escrow: false,
        })
        .unwrap();

    assert_eq!(ledger.balance(1, Asset::Cash), cash(4_000, 950));
    assert_eq!(ledger.balance(2, Asset::Cash), cash(5_000, 0));
    assert_eq!(
        ledger.balance(1, Asset::Pass { market_id: 1 }),
        pass(1, 2, 0)
    );
    assert_eq!(
        ledger.balance(2, Asset::Pass { market_id: 1 }),
        pass(1, 0, 0)
    );
    assert_eq!(
        ledger.balance(1, Asset::Cash).held,
        950,
        "unused cash reservation stays held until released"
    );

    ledger.release(1, Asset::Cash, 950).unwrap();
    assert_eq!(ledger.balance(1, Asset::Cash), cash(4_950, 0));
    ledger.check_invariants().unwrap();
}

#[test]
fn escrow_holds_then_releases_cash_and_passes() {
    let mut ledger = Ledger::new();
    ledger.deposit_cash(1, 10_000, 1).unwrap();
    ledger.issue_passes(2, 1, 2, 1).unwrap();
    ledger.reserve(1, Asset::Cash, 5_050).unwrap();
    ledger.reserve(2, Asset::Pass { market_id: 1 }, 2).unwrap();

    let settlement = TradeSettlement {
        trade_id: 10,
        market_id: 1,
        buyer_id: 1,
        seller_id: 2,
        qty: 2,
        notional: 5_000,
        fee: 50,
        fee_payer: 1,
    };
    ledger.hold_escrow(settlement).unwrap();

    assert_eq!(ledger.balance(1, Asset::Cash), cash(4_950, 0));
    assert_eq!(ledger.balance(2, Asset::Cash), cash(0, 0));
    assert_eq!(
        ledger.balance(1, Asset::Pass { market_id: 1 }),
        pass(1, 0, 0)
    );
    assert_eq!(
        ledger.balance(2, Asset::Pass { market_id: 1 }),
        pass(1, 0, 0)
    );
    assert_eq!(ledger.system_balance(Owner::Escrow, Asset::Cash), 5_050);
    assert_eq!(
        ledger.system_balance(Owner::Escrow, Asset::Pass { market_id: 1 }),
        2
    );
    ledger.check_invariants().unwrap();

    ledger.release_escrow(settlement).unwrap();

    assert_eq!(ledger.balance(1, Asset::Cash), cash(4_950, 0));
    assert_eq!(ledger.balance(2, Asset::Cash), cash(5_000, 0));
    assert_eq!(
        ledger.balance(1, Asset::Pass { market_id: 1 }),
        pass(1, 2, 0)
    );
    assert_eq!(
        ledger.balance(2, Asset::Pass { market_id: 1 }),
        pass(1, 0, 0)
    );
    assert_eq!(ledger.system_balance(Owner::Escrow, Asset::Cash), 0);
    assert_eq!(
        ledger.system_balance(Owner::Fees, Asset::Cash),
        50,
        "the fee is paid only on delivery"
    );
    ledger.check_invariants().unwrap();
}

#[test]
fn escrow_refund_returns_both_sides() {
    let mut ledger = Ledger::new();
    ledger.deposit_cash(1, 10_000, 1).unwrap();
    ledger.issue_passes(2, 1, 2, 1).unwrap();
    ledger.reserve(1, Asset::Cash, 5_050).unwrap();
    ledger.reserve(2, Asset::Pass { market_id: 1 }, 2).unwrap();

    let settlement = TradeSettlement {
        trade_id: 11,
        market_id: 1,
        buyer_id: 1,
        seller_id: 2,
        qty: 2,
        notional: 5_000,
        fee: 50,
        fee_payer: 1,
    };
    ledger.hold_escrow(settlement).unwrap();
    ledger.refund_escrow(settlement).unwrap();

    assert_eq!(ledger.balance(1, Asset::Cash), cash(10_000, 0));
    assert_eq!(
        ledger.balance(1, Asset::Pass { market_id: 1 }),
        pass(1, 0, 0)
    );
    assert_eq!(
        ledger.balance(2, Asset::Pass { market_id: 1 }),
        pass(1, 2, 0)
    );
    assert_eq!(ledger.system_balance(Owner::Escrow, Asset::Cash), 0);
    assert_eq!(
        ledger.system_balance(Owner::Escrow, Asset::Pass { market_id: 1 }),
        0
    );
    ledger.check_invariants().unwrap();
}

#[test]
fn settle_trade_can_charge_the_seller() {
    let mut ledger = Ledger::new();
    ledger.deposit_cash(1, 10_000, 1).unwrap();
    ledger.issue_passes(2, 1, 2, 1).unwrap();
    ledger.reserve(1, Asset::Cash, 5_000).unwrap();
    ledger.reserve(2, Asset::Pass { market_id: 1 }, 2).unwrap();

    ledger
        .settle_trade(TradeSettlement {
            trade_id: 2,
            market_id: 1,
            buyer_id: 1,
            seller_id: 2,
            qty: 2,
            notional: 5_000,
            fee: 50,
            fee_payer: 2,
            escrow: false,
        })
        .unwrap();

    assert_eq!(ledger.balance(1, Asset::Cash), cash(5_000, 0));
    assert_eq!(ledger.balance(2, Asset::Cash), cash(4_950, 0));
    assert_eq!(
        ledger.balance(1, Asset::Pass { market_id: 1 }),
        pass(1, 2, 0)
    );
    ledger.check_invariants().unwrap();
}

#[test]
fn settlement_without_reservation_is_rejected_atomically() {
    let mut ledger = Ledger::new();
    ledger.deposit_cash(1, 10_000, 1).unwrap();
    ledger.issue_passes(2, 1, 1, 1).unwrap();

    let err = ledger
        .settle_trade(TradeSettlement {
            trade_id: 3,
            market_id: 1,
            buyer_id: 1,
            seller_id: 2,
            qty: 1,
            notional: 5_000,
            fee: 0,
            fee_payer: 1,
            escrow: false,
        })
        .unwrap_err();
    assert!(matches!(err, LedgerError::InsufficientHeld { .. }));

    assert_eq!(ledger.balance(1, Asset::Cash), cash(10_000, 0));
    assert_eq!(
        ledger.balance(2, Asset::Pass { market_id: 1 }),
        pass(1, 1, 0)
    );
    assert_eq!(ledger.journal_count(), 2);
}

#[test]
fn duplicate_trade_settlement_is_idempotent() {
    let mut ledger = Ledger::new();
    ledger.deposit_cash(1, 10_000, 1).unwrap();
    ledger.issue_passes(2, 1, 1, 1).unwrap();
    ledger.reserve(1, Asset::Cash, 5_000).unwrap();
    ledger.reserve(2, Asset::Pass { market_id: 1 }, 1).unwrap();

    let settlement = TradeSettlement {
        trade_id: 4,
        market_id: 1,
        buyer_id: 1,
        seller_id: 2,
        qty: 1,
        notional: 5_000,
        fee: 0,
        fee_payer: 1,
        escrow: false,
    };
    let first = ledger.settle_trade(settlement).unwrap();
    let second = ledger.settle_trade(settlement).unwrap();
    assert_eq!(first, second);
    assert_eq!(ledger.journal_count(), 3);
    assert_eq!(ledger.balance(1, Asset::Cash), cash(5_000, 0));
    ledger.check_invariants().unwrap();
}

#[test]
fn unbalanced_journal_is_rejected() {
    let mut ledger = Ledger::new();
    let postings = vec![Posting::new(
        AccountKey::user(1, Asset::Cash),
        Bucket::Available,
        100,
    )];
    let err = ledger
        .post(
            JournalKey::Deposit {
                user_id: 1,
                client_request_id: 1,
            },
            postings,
        )
        .unwrap_err();
    assert!(matches!(err, LedgerError::UnbalancedJournal { .. }));
    assert_eq!(ledger.balance(1, Asset::Cash), cash(0, 0));
    assert_eq!(ledger.journal_count(), 0);
}
