use dx_ledger::Ledger;
use dx_types::Asset;
use proptest::prelude::*;

proptest! {
    #[test]
    fn invariants_hold_after_random_deposits_and_reservations(
        deposits in prop::collection::vec(1i64..=100_000, 0..20),
        reservations in prop::collection::vec(1i64..=50_000, 0..30),
    ) {
        let mut ledger = Ledger::new();
        for (index, amount) in deposits.iter().enumerate() {
            ledger.deposit_cash(1, *amount, index as u64).unwrap();
        }
        for amount in reservations {
            let _ = ledger.reserve(1, Asset::Cash, amount);
        }
        ledger.check_invariants().unwrap();
    }
}
