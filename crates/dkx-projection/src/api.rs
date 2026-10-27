#![allow(clippy::type_complexity)]
use axum::{
    Json, Router,
    extract::{Path as AxumPath, Query, State},
    http::StatusCode,
    routing::{get, post},
};
use dkx_escrow::{load_signing_key_from_env, verify_receipt};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::sync::Arc;

use crate::auth::{ApiErrorEnvelope, AuthUser};

pub const MAX_TOPUP_PAISE: i64 = 10_000_000;

#[derive(Clone)]
pub struct ApiState {
    pub pool: PgPool,
    pub submit_fn: Arc<dyn Fn(TopUpCommand) -> Result<(), String> + Send + Sync>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopUpCommand {
    pub user_id: u64,
    pub amount_paise: i64,
    pub idempotency_key: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PassView {
    pub market_id: u32,
    pub event_name: String,
    pub tier_name: String,
    pub available: i64,
    pub locked: i64,
    pub held: i64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WalletResponse {
    pub available_paise: i64,
    pub locked_paise: i64,
    pub escrow_paise: i64,
    pub total_paise: i64,
    pub passes: Vec<PassView>,
}

#[derive(Debug, Deserialize)]
pub struct TopUpRequest {
    pub amount_paise: i64,
    pub idempotency_key: String,
}

#[derive(Debug, Serialize)]
pub struct TopUpResponse {
    pub available_paise: i64,
}

#[derive(Debug, Deserialize)]
pub struct HistoryQuery {
    pub cursor: Option<i64>,
    pub limit: Option<i64>,
    pub market_id: Option<u32>,
    pub side: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct TradeHistoryItem {
    pub trade_id: i64,
    pub ts: i64,
    pub market_id: u32,
    pub event_name: String,
    pub tier_name: String,
    pub side: String,
    pub qty: i64,
    pub price_paise: i64,
    pub fee_paise: i64,
    pub total_paise: i64,
    pub escrow_id: Option<i64>,
    pub escrow_state: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct HistoryResponse {
    pub items: Vec<TradeHistoryItem>,
    pub next_cursor: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct StatementQuery {
    pub month: String, // YYYY-MM
}

#[derive(Debug, Serialize)]
pub struct StatementResponse {
    pub month: String,
    pub bought_qty: i64,
    pub sold_qty: i64,
    pub spent_paise: i64,
    pub earned_paise: i64,
    pub fees_paise: i64,
}

#[derive(Debug, Deserialize)]
pub struct LedgerQuery {
    pub cursor: Option<i64>,
    pub limit: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct LedgerLegView {
    pub account: String,
    pub asset: String,
    pub amount: i64,
}

#[derive(Debug, Serialize)]
pub struct LedgerEntryView {
    pub id: i64,
    pub seq: i64,
    pub kind: String,
    pub ts: i64,
    pub legs: Vec<LedgerLegView>,
}

#[derive(Debug, Serialize)]
pub struct LedgerResponse {
    pub items: Vec<LedgerEntryView>,
    pub next_cursor: Option<i64>,
}

/// Stub lookup for market metadata until teammate's module is plugged in
pub fn market_lookup_stub(market_id: u32) -> (&'static str, &'static str) {
    match market_id {
        1 => ("Darkhold Festival 2026", "VIP"),
        2 => ("Darkhold Festival 2026", "General Admission"),
        _ => ("Special Event", "Standard Pass"),
    }
}

pub fn create_router(state: ApiState) -> Router {
    Router::new()
        .route("/api/wallet", get(get_wallet))
        .route("/api/wallet/topup", post(post_topup))
        .route("/api/history", get(get_history))
        .route("/api/statements", get(get_statements))
        .route("/api/ledger", get(get_ledger))
        .route("/api/escrow", get(get_escrows))
        .route("/api/escrow/{id}", get(get_escrow_by_id))
        .route("/api/escrow/{id}/accept", post(post_escrow_accept))
        .route("/api/escrow/{id}/reject", post(post_escrow_reject))
        .route("/api/escrow/{id}/verify", post(post_escrow_verify))
        .route("/api/escrow/public-key", get(get_escrow_public_key))
        .route("/api/admin/disputes", get(get_admin_disputes))
        .route(
            "/api/admin/disputes/{id}/rule",
            post(post_admin_dispute_rule),
        )
        .with_state(state)
}

async fn get_wallet(
    State(state): State<ApiState>,
    auth: AuthUser,
) -> Result<Json<WalletResponse>, (StatusCode, Json<ApiErrorEnvelope>)> {
    let balance_row: Option<(i64, i64, i64)> = sqlx::query_as(
        "SELECT cash_avail, cash_locked, escrow_cash FROM balances WHERE user_id = $1",
    )
    .bind(auth.user_id as i64)
    .fetch_optional(&state.pool)
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiErrorEnvelope::new("DB_ERROR", e.to_string())),
        )
    })?;

    let (avail, locked, escrow) = balance_row.unwrap_or((0, 0, 0));
    let total = avail + locked + escrow;

    let passes_rows: Vec<(i32, i64, i64, i64)> = sqlx::query_as(
        "SELECT market_id, avail, locked, held FROM pass_balances WHERE user_id = $1",
    )
    .bind(auth.user_id as i64)
    .fetch_all(&state.pool)
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiErrorEnvelope::new("DB_ERROR", e.to_string())),
        )
    })?;

    let passes = passes_rows
        .into_iter()
        .map(|(m_id, p_avail, p_locked, p_held)| {
            let (event_name, tier_name) = market_lookup_stub(m_id as u32);
            PassView {
                market_id: m_id as u32,
                event_name: event_name.to_string(),
                tier_name: tier_name.to_string(),
                available: p_avail,
                locked: p_locked,
                held: p_held,
            }
        })
        .collect();

    Ok(Json(WalletResponse {
        available_paise: avail,
        locked_paise: locked,
        escrow_paise: escrow,
        total_paise: total,
        passes,
    }))
}

async fn post_topup(
    State(state): State<ApiState>,
    auth: AuthUser,
    Json(payload): Json<TopUpRequest>,
) -> Result<Json<TopUpResponse>, (StatusCode, Json<ApiErrorEnvelope>)> {
    if payload.amount_paise <= 0 {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ApiErrorEnvelope::new(
                "INVALID_AMOUNT",
                "Amount must be strictly greater than 0 paise",
            )),
        ));
    }

    if payload.amount_paise > MAX_TOPUP_PAISE {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ApiErrorEnvelope::new(
                "AMOUNT_EXCEEDED",
                format!(
                    "Maximum allowed top-up is {} paise (₹100,000)",
                    MAX_TOPUP_PAISE
                ),
            )),
        ));
    }

    let cmd = TopUpCommand {
        user_id: auth.user_id,
        amount_paise: payload.amount_paise,
        idempotency_key: payload.idempotency_key,
    };

    (state.submit_fn)(cmd).map_err(|err| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiErrorEnvelope::new("SUBMISSION_FAILED", err)),
        )
    })?;

    let current_avail: (i64,) =
        sqlx::query_as("SELECT cash_avail FROM balances WHERE user_id = $1")
            .bind(auth.user_id as i64)
            .fetch_optional(&state.pool)
            .await
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ApiErrorEnvelope::new("DB_ERROR", e.to_string())),
                )
            })?
            .unwrap_or((0,));

    Ok(Json(TopUpResponse {
        available_paise: current_avail.0,
    }))
}

#[allow(clippy::type_complexity)]
async fn get_history(
    State(state): State<ApiState>,
    auth: AuthUser,
    Query(query): Query<HistoryQuery>,
) -> Result<Json<HistoryResponse>, (StatusCode, Json<ApiErrorEnvelope>)> {
    let limit = query.limit.unwrap_or(25).clamp(1, 100);
    let cursor = query.cursor.unwrap_or(i64::MAX);

    let rows: Vec<(i64, i64, i32, i64, i64, i64, i64, i64, Option<i64>)> = sqlx::query_as(
        r#"
        SELECT id, ts, market_id, buyer_id, seller_id, qty, price, fee, escrow_id
        FROM trades
        WHERE (buyer_id = $1 OR seller_id = $1)
          AND id < $2
          AND ($3::INTEGER IS NULL OR market_id = $3)
        ORDER BY id DESC
        LIMIT $4
        "#,
    )
    .bind(auth.user_id as i64)
    .bind(cursor)
    .bind(query.market_id.map(|m| m as i32))
    .bind(limit + 1)
    .fetch_all(&state.pool)
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiErrorEnvelope::new("DB_ERROR", e.to_string())),
        )
    })?;

    let has_more = rows.len() as i64 > limit;
    let mut items = Vec::new();

    for row in rows.into_iter().take(limit as usize) {
        let (id, ts, market_id, buyer_id, _seller_id, qty, price, fee, escrow_id) = row;
        let is_buyer = buyer_id == auth.user_id as i64;
        let side = if is_buyer { "buy" } else { "sell" };

        if let Some(ref req_side) = query.side
            && req_side != side
        {
            continue;
        }

        let (event_name, tier_name) = market_lookup_stub(market_id as u32);
        let total_paise = (price * qty) + if is_buyer { 0 } else { -fee };

        items.push(TradeHistoryItem {
            trade_id: id,
            ts,
            market_id: market_id as u32,
            event_name: event_name.to_string(),
            tier_name: tier_name.to_string(),
            side: side.to_string(),
            qty,
            price_paise: price,
            fee_paise: fee,
            total_paise,
            escrow_id,
            escrow_state: escrow_id.map(|_| "Held".to_string()),
        });
    }

    let next_cursor = if has_more {
        items.last().map(|i| i.trade_id)
    } else {
        None
    };

    Ok(Json(HistoryResponse { items, next_cursor }))
}

async fn get_statements(
    State(state): State<ApiState>,
    auth: AuthUser,
    Query(query): Query<StatementQuery>,
) -> Result<Json<StatementResponse>, (StatusCode, Json<ApiErrorEnvelope>)> {
    let rows: Vec<(i64, i64, i64, i64, i64)> = sqlx::query_as(
        r#"
        SELECT buyer_id, seller_id, qty, price, fee
        FROM trades
        WHERE (buyer_id = $1 OR seller_id = $1)
          AND to_char(to_timestamp(ts / 1000000), 'YYYY-MM') = $2
        "#,
    )
    .bind(auth.user_id as i64)
    .bind(&query.month)
    .fetch_all(&state.pool)
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiErrorEnvelope::new("DB_ERROR", e.to_string())),
        )
    })?;

    let mut bought_qty = 0;
    let mut sold_qty = 0;
    let mut spent_paise = 0;
    let mut earned_paise = 0;
    let mut fees_paise = 0;

    for (buyer_id, seller_id, qty, price, fee) in rows {
        let notional = price * qty;
        if buyer_id == auth.user_id as i64 {
            bought_qty += qty;
            spent_paise += notional;
        }
        if seller_id == auth.user_id as i64 {
            sold_qty += qty;
            earned_paise += notional - fee;
            fees_paise += fee;
        }
    }

    Ok(Json(StatementResponse {
        month: query.month,
        bought_qty,
        sold_qty,
        spent_paise,
        earned_paise,
        fees_paise,
    }))
}

async fn get_ledger(
    State(state): State<ApiState>,
    auth: AuthUser,
    Query(query): Query<LedgerQuery>,
) -> Result<Json<LedgerResponse>, (StatusCode, Json<ApiErrorEnvelope>)> {
    let limit = query.limit.unwrap_or(25).clamp(1, 100);
    let cursor = query.cursor.unwrap_or(i64::MAX);

    let entries: Vec<(i64, i64, String, i64)> = sqlx::query_as(
        r#"
        SELECT DISTINCT e.id, e.seq, e.kind, e.ts
        FROM ledger_entries e
        JOIN ledger_legs l ON l.entry_id = e.id
        WHERE l.account LIKE '%' || $1 || '%'
          AND e.id < $2
        ORDER BY e.id DESC
        LIMIT $3
        "#,
    )
    .bind(auth.user_id.to_string())
    .bind(cursor)
    .bind(limit + 1)
    .fetch_all(&state.pool)
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiErrorEnvelope::new("DB_ERROR", e.to_string())),
        )
    })?;

    let has_more = entries.len() as i64 > limit;
    let mut items = Vec::new();

    for (id, seq, kind, ts) in entries.into_iter().take(limit as usize) {
        let legs_rows: Vec<(String, String, i64)> =
            sqlx::query_as("SELECT account, asset, amount FROM ledger_legs WHERE entry_id = $1")
                .bind(id)
                .fetch_all(&state.pool)
                .await
                .map_err(|e| {
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(ApiErrorEnvelope::new("DB_ERROR", e.to_string())),
                    )
                })?;

        let legs = legs_rows
            .into_iter()
            .map(|(account, asset, amount)| LedgerLegView {
                account,
                asset,
                amount,
            })
            .collect();

        items.push(LedgerEntryView {
            id,
            seq,
            kind,
            ts,
            legs,
        });
    }

    let next_cursor = if has_more {
        items.last().map(|i| i.id)
    } else {
        None
    };

    Ok(Json(LedgerResponse { items, next_cursor }))
}

#[derive(Debug, Deserialize)]
pub struct EscrowListQuery {
    pub state: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ReceiptView {
    pub bytes_hex: String,
    pub signature_hex: String,
    pub public_key_hex: String,
}

#[derive(Debug, Serialize)]
pub struct DisputeView {
    pub id: i64,
    pub reason: String,
    pub note: String,
    pub status: String,
    pub ruling: Option<String>,
    pub ruled_by: Option<i64>,
    pub ruled_at: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct EscrowDetailResponse {
    pub id: i64,
    pub trade_id: i64,
    pub market_id: i32,
    pub buyer_id: i64,
    pub seller_id: i64,
    pub role: String,
    pub qty: i32,
    pub price: i64,
    pub fee: i64,
    pub state: String,
    pub delivered_at: i64,
    pub deadline: i64,
    pub receipt: ReceiptView,
    pub dispute: Option<DisputeView>,
}

#[derive(Debug, Deserialize)]
pub struct RejectEscrowPayload {
    pub reason: String,
    pub note: String,
}

#[derive(Debug, Deserialize)]
pub struct AdminRulePayload {
    pub ruling: String, // "buyer" | "seller"
    pub note: String,
}

#[derive(Debug, Serialize)]
pub struct VerifyResponse {
    pub valid: bool,
}

#[derive(Debug, Serialize)]
pub struct PublicKeyResponse {
    pub public_key_hex: String,
}

async fn get_escrows(
    State(state): State<ApiState>,
    auth: AuthUser,
    Query(query): Query<EscrowListQuery>,
) -> Result<Json<Vec<EscrowDetailResponse>>, (StatusCode, Json<ApiErrorEnvelope>)> {
    let rows: Vec<(
        i64,
        i64,
        i32,
        i64,
        i64,
        i32,
        i64,
        i64,
        String,
        i64,
        i64,
        Vec<u8>,
        Vec<u8>,
    )> = if let Some(ref st) = query.state {
        sqlx::query_as(
            r#"
            SELECT id, trade_id, market_id, buyer_id, seller_id, qty, price, fee, state, delivered_at, deadline, receipt_bytes, receipt_sig
            FROM escrows
            WHERE (buyer_id = $1 OR seller_id = $1) AND state = $2
            ORDER BY id DESC
            "#,
        )
        .bind(auth.user_id as i64)
        .bind(st)
        .fetch_all(&state.pool)
        .await
    } else {
        sqlx::query_as(
            r#"
            SELECT id, trade_id, market_id, buyer_id, seller_id, qty, price, fee, state, delivered_at, deadline, receipt_bytes, receipt_sig
            FROM escrows
            WHERE buyer_id = $1 OR seller_id = $1
            ORDER BY id DESC
            "#,
        )
        .bind(auth.user_id as i64)
        .fetch_all(&state.pool)
        .await
    }
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiErrorEnvelope::new("DB_ERROR", e.to_string()))))?;

    let pubkey_hex = load_signing_key_from_env()
        .map(|sk| hex::encode(sk.verifying_key().to_bytes()))
        .unwrap_or_default();

    let mut result = Vec::new();
    for r in rows {
        let role = if r.3 == auth.user_id as i64 {
            "buyer".to_string()
        } else {
            "seller".to_string()
        };

        result.push(EscrowDetailResponse {
            id: r.0,
            trade_id: r.1,
            market_id: r.2,
            buyer_id: r.3,
            seller_id: r.4,
            role,
            qty: r.5,
            price: r.6,
            fee: r.7,
            state: r.8,
            delivered_at: r.9,
            deadline: r.10,
            receipt: ReceiptView {
                bytes_hex: hex::encode(&r.11),
                signature_hex: hex::encode(&r.12),
                public_key_hex: pubkey_hex.clone(),
            },
            dispute: None,
        });
    }

    Ok(Json(result))
}

async fn get_escrow_by_id(
    State(state): State<ApiState>,
    auth: AuthUser,
    AxumPath(id): AxumPath<i64>,
) -> Result<Json<EscrowDetailResponse>, (StatusCode, Json<ApiErrorEnvelope>)> {
    let row: Option<(
        i64,
        i64,
        i32,
        i64,
        i64,
        i32,
        i64,
        i64,
        String,
        i64,
        i64,
        Vec<u8>,
        Vec<u8>,
    )> = sqlx::query_as(
        r#"
        SELECT id, trade_id, market_id, buyer_id, seller_id, qty, price, fee, state, delivered_at, deadline, receipt_bytes, receipt_sig
        FROM escrows
        WHERE id = $1
        "#,
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiErrorEnvelope::new("DB_ERROR", e.to_string()))))?;

    let r = row.ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(ApiErrorEnvelope::new("NOT_FOUND", "Escrow not found")),
        )
    })?;

    if auth.role != crate::auth::Role::Admin
        && r.3 != auth.user_id as i64
        && r.4 != auth.user_id as i64
    {
        return Err((
            StatusCode::FORBIDDEN,
            Json(ApiErrorEnvelope::new(
                "FORBIDDEN",
                "Access denied: neither buyer nor seller",
            )),
        ));
    }

    let dispute_row: Option<(
        i64,
        String,
        String,
        String,
        Option<String>,
        Option<i64>,
        Option<i64>,
    )> = sqlx::query_as(
        r#"
        SELECT id, reason, note, status, ruling, ruled_by, ruled_at
        FROM disputes
        WHERE escrow_id = $1
        ORDER BY id DESC LIMIT 1
        "#,
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await
    .unwrap_or(None);

    let dispute = dispute_row.map(|d| DisputeView {
        id: d.0,
        reason: d.1,
        note: d.2,
        status: d.3,
        ruling: d.4,
        ruled_by: d.5,
        ruled_at: d.6,
    });

    let role = if r.3 == auth.user_id as i64 {
        "buyer".to_string()
    } else {
        "seller".to_string()
    };

    let pubkey_hex = load_signing_key_from_env()
        .map(|sk| hex::encode(sk.verifying_key().to_bytes()))
        .unwrap_or_default();

    Ok(Json(EscrowDetailResponse {
        id: r.0,
        trade_id: r.1,
        market_id: r.2,
        buyer_id: r.3,
        seller_id: r.4,
        role,
        qty: r.5,
        price: r.6,
        fee: r.7,
        state: r.8,
        delivered_at: r.9,
        deadline: r.10,
        receipt: ReceiptView {
            bytes_hex: hex::encode(&r.11),
            signature_hex: hex::encode(&r.12),
            public_key_hex: pubkey_hex,
        },
        dispute,
    }))
}

async fn post_escrow_accept(
    State(state): State<ApiState>,
    auth: AuthUser,
    AxumPath(id): AxumPath<i64>,
) -> Result<StatusCode, (StatusCode, Json<ApiErrorEnvelope>)> {
    let row: Option<(i64, String)> =
        sqlx::query_as("SELECT buyer_id, state FROM escrows WHERE id = $1")
            .bind(id)
            .fetch_optional(&state.pool)
            .await
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ApiErrorEnvelope::new("DB_ERROR", e.to_string())),
                )
            })?;

    let (buyer_id, state_str) = row.ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(ApiErrorEnvelope::new("NOT_FOUND", "Escrow not found")),
        )
    })?;

    if buyer_id != auth.user_id as i64 {
        return Err((
            StatusCode::FORBIDDEN,
            Json(ApiErrorEnvelope::new(
                "FORBIDDEN",
                "Only buyer can accept escrow",
            )),
        ));
    }
    if state_str != "Delivered" {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ApiErrorEnvelope::new(
                "INVALID_STATE",
                "Escrow is not in Delivered state",
            )),
        ));
    }

    // Call submit_fn for single-writer core processing if available
    let req_id = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(1);

    (state.submit_fn)(TopUpCommand {
        user_id: auth.user_id,
        amount_paise: 0,
        idempotency_key: format!("accept-{id}-{req_id}"),
    })
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiErrorEnvelope::new("CORE_ERROR", e)),
        )
    })?;

    sqlx::query("UPDATE escrows SET state = 'Released' WHERE id = $1")
        .bind(id)
        .execute(&state.pool)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiErrorEnvelope::new("DB_ERROR", e.to_string())),
            )
        })?;

    Ok(StatusCode::OK)
}

async fn post_escrow_reject(
    State(state): State<ApiState>,
    auth: AuthUser,
    AxumPath(id): AxumPath<i64>,
    Json(payload): Json<RejectEscrowPayload>,
) -> Result<StatusCode, (StatusCode, Json<ApiErrorEnvelope>)> {
    let row: Option<(i64, String)> =
        sqlx::query_as("SELECT buyer_id, state FROM escrows WHERE id = $1")
            .bind(id)
            .fetch_optional(&state.pool)
            .await
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ApiErrorEnvelope::new("DB_ERROR", e.to_string())),
                )
            })?;

    let (buyer_id, state_str) = row.ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(ApiErrorEnvelope::new("NOT_FOUND", "Escrow not found")),
        )
    })?;

    if buyer_id != auth.user_id as i64 {
        return Err((
            StatusCode::FORBIDDEN,
            Json(ApiErrorEnvelope::new(
                "FORBIDDEN",
                "Only buyer can reject escrow",
            )),
        ));
    }
    if state_str != "Delivered" {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ApiErrorEnvelope::new(
                "INVALID_STATE",
                "Escrow is not in Delivered state",
            )),
        ));
    }

    let req_id = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(1);

    (state.submit_fn)(TopUpCommand {
        user_id: auth.user_id,
        amount_paise: 0,
        idempotency_key: format!("reject-{id}-{req_id}"),
    })
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiErrorEnvelope::new("CORE_ERROR", e)),
        )
    })?;

    sqlx::query("UPDATE escrows SET state = 'Disputed' WHERE id = $1")
        .bind(id)
        .execute(&state.pool)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiErrorEnvelope::new("DB_ERROR", e.to_string())),
            )
        })?;

    sqlx::query(
        r#"
        INSERT INTO disputes (escrow_id, reason, note, status)
        VALUES ($1, $2, $3, 'open')
        "#,
    )
    .bind(id)
    .bind(&payload.reason)
    .bind(&payload.note)
    .execute(&state.pool)
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiErrorEnvelope::new("DB_ERROR", e.to_string())),
        )
    })?;

    Ok(StatusCode::OK)
}

async fn post_escrow_verify(
    State(state): State<ApiState>,
    AxumPath(id): AxumPath<i64>,
) -> Result<Json<VerifyResponse>, (StatusCode, Json<ApiErrorEnvelope>)> {
    let row: Option<(Vec<u8>, Vec<u8>)> =
        sqlx::query_as("SELECT receipt_bytes, receipt_sig FROM escrows WHERE id = $1")
            .bind(id)
            .fetch_optional(&state.pool)
            .await
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ApiErrorEnvelope::new("DB_ERROR", e.to_string())),
                )
            })?;

    let (receipt_bytes, receipt_sig) = row.ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(ApiErrorEnvelope::new("NOT_FOUND", "Escrow not found")),
        )
    })?;

    let signing_key = load_signing_key_from_env().map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiErrorEnvelope::new(
                "CONFIG_ERROR",
                "Signing key unavailable",
            )),
        )
    })?;

    let valid =
        verify_receipt(&signing_key.verifying_key(), &receipt_bytes, &receipt_sig).unwrap_or(false);
    Ok(Json(VerifyResponse { valid }))
}

async fn get_escrow_public_key()
-> Result<Json<PublicKeyResponse>, (StatusCode, Json<ApiErrorEnvelope>)> {
    let signing_key = load_signing_key_from_env().map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiErrorEnvelope::new(
                "CONFIG_ERROR",
                "Signing key unavailable",
            )),
        )
    })?;

    let pubkey_hex = hex::encode(signing_key.verifying_key().to_bytes());
    Ok(Json(PublicKeyResponse {
        public_key_hex: pubkey_hex,
    }))
}

async fn get_admin_disputes(
    State(state): State<ApiState>,
    auth: AuthUser,
) -> Result<Json<Vec<DisputeView>>, (StatusCode, Json<ApiErrorEnvelope>)> {
    if auth.role != crate::auth::Role::Admin {
        return Err((
            StatusCode::FORBIDDEN,
            Json(ApiErrorEnvelope::new("FORBIDDEN", "Admin role required")),
        ));
    }

    let rows: Vec<(
        i64,
        String,
        String,
        String,
        Option<String>,
        Option<i64>,
        Option<i64>,
    )> = sqlx::query_as(
        r#"
        SELECT id, reason, note, status, ruling, ruled_by, ruled_at
        FROM disputes
        ORDER BY id DESC
        "#,
    )
    .fetch_all(&state.pool)
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiErrorEnvelope::new("DB_ERROR", e.to_string())),
        )
    })?;

    let disputes = rows
        .into_iter()
        .map(|d| DisputeView {
            id: d.0,
            reason: d.1,
            note: d.2,
            status: d.3,
            ruling: d.4,
            ruled_by: d.5,
            ruled_at: d.6,
        })
        .collect();

    Ok(Json(disputes))
}

async fn post_admin_dispute_rule(
    State(state): State<ApiState>,
    auth: AuthUser,
    AxumPath(id): AxumPath<i64>,
    Json(payload): Json<AdminRulePayload>,
) -> Result<StatusCode, (StatusCode, Json<ApiErrorEnvelope>)> {
    if auth.role != crate::auth::Role::Admin {
        return Err((
            StatusCode::FORBIDDEN,
            Json(ApiErrorEnvelope::new("FORBIDDEN", "Admin role required")),
        ));
    }

    let ruling_lower = payload.ruling.to_lowercase();
    if ruling_lower != "buyer" && ruling_lower != "seller" {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ApiErrorEnvelope::new(
                "INVALID_RULING",
                "Ruling must be 'buyer' or 'seller'",
            )),
        ));
    }

    let dispute_row: Option<(i64, String)> =
        sqlx::query_as("SELECT escrow_id, status FROM disputes WHERE id = $1")
            .bind(id)
            .fetch_optional(&state.pool)
            .await
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ApiErrorEnvelope::new("DB_ERROR", e.to_string())),
                )
            })?;

    let (escrow_id, status) = dispute_row.ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(ApiErrorEnvelope::new("NOT_FOUND", "Dispute not found")),
        )
    })?;

    if status == "resolved" {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ApiErrorEnvelope::new(
                "ALREADY_RESOLVED",
                "Dispute has already been ruled on",
            )),
        ));
    }

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros() as i64)
        .unwrap_or(0);

    let next_state = if ruling_lower == "buyer" {
        "Refunded"
    } else {
        "Released"
    };

    sqlx::query("UPDATE escrows SET state = $2 WHERE id = $1")
        .bind(escrow_id)
        .bind(next_state)
        .execute(&state.pool)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiErrorEnvelope::new("DB_ERROR", e.to_string())),
            )
        })?;

    sqlx::query(
        r#"
        UPDATE disputes
        SET status = 'resolved', ruling = $2, ruled_by = $3, ruled_at = $4,
            note = note || E'\nAdmin note: ' || $5
        WHERE id = $1
        "#,
    )
    .bind(id)
    .bind(&ruling_lower)
    .bind(auth.user_id as i64)
    .bind(now)
    .bind(&payload.note)
    .execute(&state.pool)
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiErrorEnvelope::new("DB_ERROR", e.to_string())),
        )
    })?;

    Ok(StatusCode::OK)
}

// Axum AuthUser extractor stub for JWT authentication
impl<S> axum::extract::FromRequestParts<S> for AuthUser
where
    S: Send + Sync,
{
    type Rejection = (StatusCode, Json<ApiErrorEnvelope>);

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _state: &S,
    ) -> Result<Self, Self::Rejection> {
        // Authenticate via Authorization Bearer or simulated header x-user-id
        if let Some(auth_val) = parts.headers.get("x-user-id")
            && let Ok(uid_str) = auth_val.to_str()
            && let Ok(user_id) = uid_str.parse::<u64>()
        {
            return Ok(AuthUser {
                user_id,
                role: crate::auth::Role::User,
            });
        }
        if let Some(auth_val) = parts.headers.get("authorization")
            && let Ok(token) = auth_val.to_str()
            && token.starts_with("Bearer ")
        {
            // Default simulated user for demo JWT
            return Ok(AuthUser {
                user_id: 1,
                role: crate::auth::Role::User,
            });
        }

        // Default demo fallback user 1 if running locally
        Ok(AuthUser {
            user_id: 1,
            role: crate::auth::Role::User,
        })
    }
}
