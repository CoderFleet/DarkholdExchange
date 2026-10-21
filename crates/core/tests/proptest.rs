use dx_core::Core;
use dx_types::{Cancel, Command, OrderStatus, PlaceLimit, Side, seed_markets};
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn invariants_hold_for_random_order_flow(
        ops in prop::collection::vec(
            (1u8..=3, any::<bool>(), 2_500u64..=6_000, 1u64..=3, any::<bool>()),
            0..80,
        ),
    ) {
        let mut core = Core::new(seed_markets()).unwrap();
        for user in 1..=3 {
            core.deposit_cash(user, 100_000_000, 0).unwrap();
            core.issue_passes(user, 1, 2, 0).unwrap();
        }

        let mut open_orders: Vec<(u64, u64)> = Vec::new();

        for (index, (user, is_buy, price_ticks, qty, do_cancel)) in ops.into_iter().enumerate() {
            let user = user as u64;
            let client_order_id = index as u64 + 1;
            if do_cancel && !open_orders.is_empty() {
                let order_index = index % open_orders.len();
                let (owner, order_id) = open_orders.remove(order_index);
                let _ = core.process(
                    owner,
                    Command::Cancel(Cancel {
                        client_order_id,
                        order_id,
                    }),
                );
            } else {
                let side = if is_buy { Side::Buy } else { Side::Sell };
                let response = core.process(
                    user,
                    Command::PlaceLimit(PlaceLimit {
                        client_order_id,
                        market_id: 1,
                        side,
                        price_ticks,
                        qty,
                    }),
                );
                if let Some(order_id) = response.order_id
                    && matches!(
                        response.status,
                        OrderStatus::Rested | OrderStatus::PartiallyFilled
                    )
                {
                    open_orders.push((user, order_id));
                }
            }

            prop_assert!(!core.is_poisoned(), "core poisoned by {user} op");
            core.check_invariants().unwrap();
        }
    }
}
