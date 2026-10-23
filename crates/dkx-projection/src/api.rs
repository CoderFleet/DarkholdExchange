use axum::{
    Json, Router,
    extract::{Query, State},
    http::StatusCode,
    routing::{get, post},
};
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
            && req_side != side {
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
                && let Ok(user_id) = uid_str.parse::<u64>() {
                    return Ok(AuthUser {
                        user_id,
                        role: crate::auth::Role::User,
                    });
                }
        if let Some(auth_val) = parts.headers.get("authorization")
            && let Ok(token) = auth_val.to_str()
                && token.starts_with("Bearer ") {
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
