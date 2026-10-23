use dkx_ledger::{Account, Asset, EntryKind, Fill, IdemKey, JournalEntry, Leg};
use dkx_projection::{
    ApiErrorEnvelope, AuthUser, MAX_TOPUP_PAISE, ProjectionEvent, Role, TradeExecuted, market_lookup_stub,
};

#[test]
fn test_market_lookup_stub() {
    let (event_name, tier_name) = market_lookup_stub(1);
    assert_eq!(event_name, "Darkhold Festival 2026");
    assert_eq!(tier_name, "VIP");
}

#[test]
fn test_auth_user_creation() {
    let user = AuthUser {
        user_id: 42,
        role: Role::User,
    };
    assert_eq!(user.user_id, 42);
    assert_eq!(user.role, Role::User);
}

#[test]
fn test_api_error_envelope_format() {
    let err = ApiErrorEnvelope::new("INVALID_AMOUNT", "Amount must be positive");
    assert_eq!(err.error.code, "INVALID_AMOUNT");
    assert_eq!(err.error.message, "Amount must be positive");
}

#[test]
fn test_projection_events_generation() {
    let entry = JournalEntry {
        id: 1,
        key: IdemKey(100),
        ts_micros: 1000,
        kind: EntryKind::TopUp,
        legs: vec![
            Leg {
                account: Account::FundingSource,
                asset: Asset::Cash,
                amount: -50_000,
            },
            Leg {
                account: Account::CashAvail(1),
                asset: Asset::Cash,
                amount: 50_000,
            },
        ],
    };

    let p_event = ProjectionEvent::LedgerPosted { seq: 10, entry };
    assert_eq!(p_event.seq(), 10);

    let trade_event = ProjectionEvent::TradeExecuted(TradeExecuted {
        id: 1,
        fill: Fill {
            market: 1,
            buyer: 1,
            seller: 2,
            buyer_order: 10,
            seller_order: 20,
            price: 5000,
            qty: 2,
            escrow: false,
            seq: 11,
            ts_micros: 1005,
        },
        fee: 100,
        escrow_id: None,
    });

    assert_eq!(trade_event.seq(), 11);
}

#[test]
fn test_topup_validation_bounds() {
    assert_eq!(MAX_TOPUP_PAISE, 10_000_000);
}
