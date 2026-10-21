use dx_engine::Engine;
use dx_types::{Side, seed_markets};
use proptest::prelude::*;

const MARKET: u32 = 1;
const FLOOR: u64 = 2_500;
const CAP: u64 = 6_000;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn book_never_crosses(ops in prop::collection::vec((any::<bool>(), FLOOR..=CAP, 1u64..=20), 0..200)) {
        let mut engine = Engine::new(seed_markets()).unwrap();
        for (buy, price, qty) in ops {
            let (side, user) = if buy { (Side::Buy, 1) } else { (Side::Sell, 2) };
            let _ = engine.place_limit(user, MARKET, side, price, qty);
            let book = engine.book(MARKET).unwrap();
            if let (Some(bid), Some(ask)) = (book.best_bid(), book.best_ask()) {
                prop_assert!(bid < ask, "crossed book: bid={bid} ask={ask}");
            }
        }
    }

    #[test]
    fn cancel_always_restores_an_empty_book(price in FLOOR..=CAP, qty in 1u64..=50) {
        let mut engine = Engine::new(seed_markets()).unwrap();
        let outcome = engine.place_limit(1, MARKET, Side::Buy, price, qty).unwrap();
        engine.cancel(1, outcome.order_id).unwrap();
        let book = engine.book(MARKET).unwrap();
        prop_assert_eq!(book.open_order_count(), 0);
        prop_assert!(book.best_bid().is_none());
        prop_assert!(book.best_ask().is_none());
    }
}
