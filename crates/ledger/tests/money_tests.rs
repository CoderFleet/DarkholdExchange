use dx_ledger::money::*;
use proptest::prelude::*;

#[test]
fn test_topup_and_issue_pass() {
    let mut ledger = Ledger::new();
    let topup_id = ledger.topup(1, 100_000, IdemKey(1)).expect("topup");
    assert_eq!(topup_id, 1);
    assert_eq!(ledger.balance_of(Account::CashAvail(1)), 100_000);
    assert_eq!(ledger.balance_of(Account::FundingSource), -100_000);

    let issue_id = ledger.issue_pass(10, 2, 5, IdemKey(2)).expect("issue");
    assert_eq!(issue_id, 2);
    assert_eq!(ledger.balance_of(Account::PassAvail(2, 10)), 5);
    assert_eq!(ledger.balance_of(Account::IssuanceSource(10)), -5);

    assert!(ledger.check_invariants().is_empty());
}

#[test]
fn test_idempotency_returns_same_result_and_mutates_nothing() {
    let mut ledger = Ledger::new();
    let id1 = ledger.topup(1, 50_000, IdemKey(42)).expect("first topup");
    let id2 = ledger
        .topup(1, 50_000, IdemKey(42))
        .expect("duplicate topup");

    assert_eq!(id1, id2);
    assert_eq!(ledger.balance_of(Account::CashAvail(1)), 50_000);
    assert_eq!(ledger.balance_of(Account::FundingSource), -50_000);
    assert!(ledger.check_invariants().is_empty());
}

#[test]
fn test_rejected_operation_mutates_nothing() {
    let mut ledger = Ledger::new();
    ledger.topup(1, 10_000, IdemKey(1)).expect("topup");

    // Try to reserve more than available
    let res = ledger.reserve_cash(1, 20_000, 101, IdemKey(2));
    assert!(matches!(res, Err(LedgerError::InsufficientFunds { .. })));

    assert_eq!(ledger.balance_of(Account::CashAvail(1)), 10_000);
    assert_eq!(ledger.balance_of(Account::CashLocked(1)), 0);
    assert!(ledger.check_invariants().is_empty());
}

#[test]
fn test_normal_fill_with_100_bps_fee() {
    let mut ledger = Ledger::new();
    let buyer = 1;
    let seller = 2;
    let market = 10;
    let price: Paise = 10_000; // 100.00
    let qty = 2;
    let notional = price * qty; // 20,000

    ledger
        .topup(buyer, 30_000, IdemKey(1))
        .expect("buyer topup");
    ledger
        .issue_pass(market, seller, 5, IdemKey(2))
        .expect("seller issue");

    ledger
        .reserve_cash(buyer, notional, 1001, IdemKey(3))
        .expect("reserve cash");
    ledger
        .reserve_pass(seller, market, qty, 2001, IdemKey(4))
        .expect("reserve pass");

    let fill = Fill {
        market,
        buyer,
        seller,
        buyer_order: 1001,
        seller_order: 2001,
        price,
        qty,
        escrow: false,
        seq: 1,
        ts_micros: 1000,
    };

    let outcome = ledger.post_fill(&fill, IdemKey(5)).expect("post fill");
    assert_eq!(outcome, SettleOutcome::Settled);

    // 100 bps on 20,000 = 200
    let fee = 200;
    let seller_proceeds = 20_000 - fee;

    assert_eq!(ledger.balance_of(Account::CashLocked(buyer)), 0);
    assert_eq!(
        ledger.balance_of(Account::CashAvail(seller)),
        seller_proceeds
    );
    assert_eq!(ledger.balance_of(Account::PlatformFee), fee);
    assert_eq!(ledger.balance_of(Account::PassLocked(seller, market)), 0);
    assert_eq!(ledger.balance_of(Account::PassAvail(buyer, market)), qty);

    assert!(ledger.check_invariants().is_empty());
}

#[test]
fn test_escrow_fill_success_stage_6() {
    let mut ledger = Ledger::new();
    let buyer = 1;
    let seller = 2;
    let market = 10;
    let price: Paise = 10_000;
    let qty = 1;
    let notional = price * qty;

    ledger
        .topup(buyer, 20_000, IdemKey(1))
        .expect("buyer topup");
    ledger
        .issue_pass(market, seller, 2, IdemKey(2))
        .expect("seller issue");
    ledger
        .reserve_cash(buyer, notional, 10, IdemKey(3))
        .expect("reserve cash");
    ledger
        .reserve_pass(seller, market, qty, 20, IdemKey(4))
        .expect("reserve pass");

    let fill = Fill {
        market,
        buyer,
        seller,
        buyer_order: 10,
        seller_order: 20,
        price,
        qty,
        escrow: true,
        seq: 1,
        ts_micros: 1000,
    };

    let res = ledger.post_fill(&fill, IdemKey(99));
    assert!(matches!(res, Ok(SettleOutcome::Escrowed(_))));
}

#[test]
fn test_liability_of_user() {
    let mut ledger = Ledger::new();
    ledger.topup(1, 50_000, IdemKey(1)).expect("topup");
    ledger
        .reserve_cash(1, 20_000, 10, IdemKey(2))
        .expect("reserve");

    assert_eq!(ledger.liability_of(1), 50_000);
}

proptest! {
    #[test]
    fn prop_random_sequences_preserve_invariants(
        ops in prop::collection::vec(
            (0..5u8, 1..4u64, 1..3u32, 1..50_000i64, 1..5i64, 1..1000u64),
            1..80
        )
    ) {
        let mut ledger = Ledger::new();
        let mut key_seq = 1u128;

        for (op, user, market, cash, qty, order) in ops {
            key_seq += 1;
            let key = IdemKey(key_seq);

            match op {
                0 => {
                    let _ = ledger.topup(user, cash, key);
                }
                1 => {
                    let _ = ledger.issue_pass(market, user, qty, key);
                }
                2 => {
                    // Unique order per reservation in the random generator
                    let order_id = order * 2;
                    let _ = ledger.reserve_cash(user, cash, order_id, key);
                }
                3 => {
                    let order_id = order * 2 + 1;
                    let _ = ledger.reserve_pass(user, market, qty, order_id, key);
                }
                4 => {
                    let _ = ledger.release(order * 2, key);
                    let _ = ledger.release(order * 2 + 1, key);
                }
                _ => {}
            }

            let violations = ledger.check_invariants();
            prop_assert!(violations.is_empty(), "Violations found: {:?}", violations);
        }
    }
}
