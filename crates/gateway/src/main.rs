//! HTTP command gateway and MessagePack market-data WebSocket.
//!
//! REST is command-in / response-out. The WebSocket streams ordered deltas and
//! trades; snapshots are only baselines and lag-recovery checkpoints. Identity
//! comes from the login token (`Authorization: Bearer`, docs/AUTH.md). While `DX_DEV_AUTH` is on
//! (the default locally), a request without a token may identify itself with an explicit
//! `x-dev-user` header; there is no default dev user.

mod projection;
mod rate;
mod wal;
mod writer;

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use axum::extract::rejection::JsonRejection;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::header;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use dkx_hash::AuditRecord;
use dx_core::Core;
use dx_solvency::{PathDirection, SolvencyProof, SolvencyRoot, SolvencySnapshot};
use dx_types::{
    AcceptDutchAuction, AskRecord, AuctionStatus, Balance, BatchAuction, BatchAuctionFill,
    BatchAuctionPhase, BidRecord, BookSnapshot, Cancel, ClientOrderId, Command, CommandResponse,
    CommitAsk, CommitBid, CreateBatchAuction, CreateDutchAuction, DutchAuction, ErrorBody,
    EscrowRecord, EscrowState, Fill, JournalResponse, MarketId, MarketView, MetricsSnapshot, Minor,
    OpenOrderView, OrderId, OrderStatus, PlaceLimit, PlaceMarket, Qty, ReasonCode, RevealAsk,
    RevealBid, SettleBatchAuction, Side, Ticks, TradeId, UserId, WsClientMessage, WsServerMessage,
    seed_markets,
};
use futures_util::SinkExt;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::TryRecvError;
use writer::{
    MetricsState, PrivateEvent, StreamBaseline, WriterError, WriterHandle, spawn_writer_with_wal,
};

#[derive(Clone)]
struct AppState {
    writer: WriterHandle,
    metrics: Arc<MetricsState>,
    auth: dx_auth::AuthState,
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    body: Json<ErrorBody>,
    retry_after: bool,
    /// Login errors (`unauthorized`, `forbidden`) are not in the shared `ReasonCode` list yet
    /// (docs/AUTH.md), so they are sent as plain text instead of `body`.
    raw_reason: Option<&'static str>,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        let mut response = match self.raw_reason {
            Some(reason) => {
                (self.status, Json(serde_json::json!({ "reason": reason }))).into_response()
            }
            None => (self.status, self.body).into_response(),
        };
        if self.retry_after {
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
        }
        response
    }
}

fn api_error(status: StatusCode, reason: ReasonCode) -> ApiError {
    ApiError {
        status,
        body: Json(ErrorBody { reason }),
        retry_after: matches!(
            status,
            StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE
        ),
        raw_reason: None,
    }
}

fn auth_error(status: StatusCode, reason: &'static str) -> ApiError {
    ApiError {
        status,
        body: Json(ErrorBody {
            reason: ReasonCode::InvalidRequest,
        }),
        retry_after: false,
        raw_reason: Some(reason),
    }
}

fn unauthorized() -> ApiError {
    auth_error(StatusCode::UNAUTHORIZED, "unauthorized")
}

fn forbidden() -> ApiError {
    auth_error(StatusCode::FORBIDDEN, "forbidden")
}

fn unknown_user() -> ApiError {
    auth_error(StatusCode::NOT_FOUND, "unknown_user")
}

fn market_publisher(writer: WriterHandle) -> dx_admin::Publisher {
    Arc::new(move |config| {
        let writer = writer.clone();
        Box::pin(async move {
            let market_id = config.id;
            let face_value = config.face_value_minor;
            let res = writer.create_market(config).await;
            if res.is_ok() {
                // Audit note: Kind 5 MarketPublished
                let _ = writer.record_audit_note(5, market_id, 0, face_value).await;
            }
            res
        })
    })
}

fn admin_state(writer: WriterHandle) -> dx_admin::AdminState {
    dx_admin::AdminState::new(market_publisher(writer))
}

/// Accounts, events and tiers are saved in PostgreSQL when `DATABASE_URL` is set, so they
/// survive a restart (the engine's money and markets already do, through the log). Without it
/// they stay in memory, which is fine for a local try-out but not for a real run: after a
/// restart the engine would still hold the old accounts' money while their ids are handed out
/// again. So a real run (`DX_DEV_AUTH=0`) refuses to start without a database.
async fn attach_database(auth: &dx_auth::AuthState, admin: &dx_admin::AdminState) {
    match std::env::var("DATABASE_URL") {
        Ok(url) => {
            if let Err(error) = dx_store::attach(&url, auth, admin).await {
                if auth.dev_auth_enabled() {
                    eprintln!(
                        "database: could not connect to PostgreSQL ({error}). Falling back to in-memory mode."
                    );
                } else {
                    panic!("database: {error}");
                }
            } else {
                println!("database: accounts, events and tiers are saved in PostgreSQL");
            }
        }
        Err(_) if !auth.dev_auth_enabled() => {
            panic!("DATABASE_URL must be set when DX_DEV_AUTH=0 (see docs/AUTH.md)")
        }
        Err(_) => eprintln!(
            "database: DATABASE_URL is not set, so accounts and events are kept in memory only \
             and are lost on restart. If the write-ahead log is on, delete the data folder \
             before restarting, or new sign-ups will be given the old accounts' ids and money."
        ),
    }
}

fn writer_error(error: WriterError) -> ApiError {
    match error {
        WriterError::QueueFull => api_error(StatusCode::TOO_MANY_REQUESTS, ReasonCode::QueueFull),
        WriterError::Unavailable => {
            api_error(StatusCode::SERVICE_UNAVAILABLE, ReasonCode::InternalError)
        }
    }
}

/// Transport mapping for a sequenced business rejection. The core still returns
/// the original `CommandResponse`; the gateway translates it once at the edge.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

fn status_for_reason(reason: ReasonCode) -> StatusCode {
    use ReasonCode::*;
    match reason {
        OutsideCollar
        | InsufficientFunds
        | InsufficientInventory
        | HoldingLimitExceeded
        | SelfTrade => StatusCode::UNPROCESSABLE_ENTITY,
        UnknownMarket | UnknownUser | OrderNotFound => StatusCode::NOT_FOUND,
        NotOrderOwner => StatusCode::FORBIDDEN,
        DuplicateClientOrderId
        | MarketClosed
        | CancelNotAllowed
        | AuctionNotActive
        | AuctionClosed
        | StaleAuctionPrice
        | AuctionPhaseMismatch
        | EscrowNotReady => StatusCode::CONFLICT,
        RateLimited | QueueFull => StatusCode::TOO_MANY_REQUESTS,
        InternalError => StatusCode::INTERNAL_SERVER_ERROR,
        InvalidQuantity | InvalidPrice | PriceNotTickAligned | InvalidRequest | InvalidReceipt
        | ReceiptExpired | InvalidCommitment => StatusCode::BAD_REQUEST,
    }
}

#[derive(Deserialize)]
struct BookQuery {
    depth: Option<usize>,
}

#[derive(Deserialize)]
struct TradeQuery {
    limit: Option<usize>,
}

#[derive(Deserialize)]
struct CancelQuery {
    client_order_id: Option<ClientOrderId>,
}

#[derive(Deserialize)]
struct WsQuery {
    user_id: Option<UserId>,
    /// The login token. Browsers cannot set headers on a WebSocket, so it travels in the address.
    token: Option<String>,
}

#[derive(Deserialize)]
struct DevDeposit {
    user_id: UserId,
    amount: Minor,
    client_request_id: ClientOrderId,
}

#[derive(Deserialize)]
struct DevIssue {
    user_id: UserId,
    market_id: MarketId,
    qty: Qty,
    client_request_id: ClientOrderId,
}

/// Most passes one issue request may create. A typo (an extra zero) cannot flood a market, and
/// a bigger batch simply takes several requests.
const MAX_ISSUE_QTY: Qty = 1_000;

#[derive(Deserialize)]
struct IssuePasses {
    username: String,
    market_id: MarketId,
    qty: Qty,
    client_request_id: ClientOrderId,
}

#[derive(Serialize)]
struct SupplyRow {
    market_id: MarketId,
    name: String,
    /// Passes that users hold right now (available and reserved). Passes are only ever created
    /// by issuing, so this is the number issued so far.
    passes_held: u64,
}

fn seeded_core() -> (Core, Vec<Fill>) {
    let mut core = Core::new(seed_markets()).expect("valid seed markets");
    let seed_fills = seed_demo(&mut core);
    (core, seed_fills)
}

fn open_state(
    wal_dir: &str,
) -> Result<(Core, Vec<Fill>, Vec<AuditRecord>, wal::WalSession), String> {
    let recovered = dkx_wal::recover(wal_dir).map_err(|error| format!("WAL recovery: {error}"))?;

    let (core, recent_trades, replayed_audit) =
        wal::restore_core_and_audit(&recovered, seeded_core().0)?;
    core.check_invariants()?;
    let session = wal::WalSession::open(wal_dir, recovered.last_seq, recovered.last_hash)?;
    Ok((core, recent_trades, replayed_audit, session))
}

#[tokio::main]
async fn main() {
    let addr = std::env::var("GATEWAY_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".into());
    let wal_enabled = std::env::var("DX_WAL")
        .map(|value| value != "0")
        .unwrap_or(true);
    let wal_dir = std::env::var("DX_WAL_DIR").unwrap_or_else(|_| ".".into());

    let (core, seed_fills, audit_backfill, wal) = if wal_enabled {
        let (core, fills, audits, wal) = open_state(&wal_dir).expect("WAL recovery");
        (core, fills, audits, Some(wal))
    } else {
        let (core, fills) = seeded_core();
        (core, fills, Vec::new(), None)
    };
    println!("core state hash: {}", hex::encode(core.state_hash()));

    // Private per-user push (row 7): the writer knows exactly whose state each
    // event touches; sockets route the events by token-checked identity.
    let (private_tx, _) = broadcast::channel::<writer::PrivateEvent>(512);

    let (tx, _) = broadcast::channel(512);
    // Row 21: PostgreSQL projection feed — None when disabled or unreachable.
    let projection = projection::spawn_projection_worker().await;
    let (writer, metrics) = spawn_writer_with_wal(
        core,
        tx,
        seed_fills,
        queue_capacity(),
        wal,
        audit_backfill,
        Some(private_tx),
        projection,
    );
    // Events and tiers for the admin console. Publishing creates a live market
    // through the same single-writer queue as every other core change.
    let admin = admin_state(writer.clone());
    // Login, sign-up and /api/me (docs/AUTH.md), and the check every other endpoint uses.
    let auth = dx_auth::AuthState::from_env();
    attach_database(&auth, &admin).await;
    let state = AppState {
        writer,
        metrics,
        auth: auth.clone(),
    };

    let rate_state = rate::RateState::new(rate::RateLimits::from_env(), auth.clone());
    let app = Router::new()
        .route("/health", get(health))
        .route("/api/health", get(health))
        .route("/api/state/hash", get(state_hash))
        .route("/api/markets", get(markets))
        .route("/api/markets/{market_id}/book", get(book))
        .route("/api/markets/{market_id}/trades", get(trades))
        .route("/api/commands", post(command))
        .route("/api/orders", post(place_order))
        .route("/api/orders/open", get(open_orders_current))
        .route("/api/orders/{order_id}", delete(cancel_order))
        .route("/api/metrics/latency", get(latency_metrics))
        .route("/api/solvency", get(solvency_roots))
        .route("/api/me/solvency", get(solvency_current))
        .route("/api/users/{user_id}/balances", get(balances))
        .route("/api/users/{user_id}/orders", get(open_orders))
        .route("/api/wallet", get(wallet_current))
        .route("/api/wallet/topup", post(wallet_topup))
        .route(
            "/api/me/public-key",
            get(public_key_current).post(register_public_key),
        )
        .route("/api/me/escrow", get(escrow_current))
        .route("/api/escrow/{trade_id}/receipt", post(escrow_receipt))
        .route("/api/escrow/{trade_id}/refund", post(escrow_refund))
        .route("/api/auctions", get(auctions_current).post(auction_create))
        .route("/api/auctions/{auction_id}/accept", post(auction_accept))
        .route("/api/auctions/{auction_id}/expire", post(auction_expire))
        // Batch auction endpoints
        .route(
            "/api/batch-auctions",
            get(batch_auctions_current).post(batch_auction_create),
        )
        .route("/api/batch-auctions/{auction_id}", get(batch_auction_get))
        .route(
            "/api/batch-auctions/{auction_id}/commit",
            post(batch_auction_commit),
        )
        .route(
            "/api/batch-auctions/{auction_id}/reveal",
            post(batch_auction_reveal),
        )
        .route(
            "/api/batch-auctions/{auction_id}/settle",
            post(batch_auction_settle),
        )
        .route(
            "/api/batch-auctions/{auction_id}/bids",
            get(batch_auction_bids),
        )
        .route(
            "/api/batch-auctions/{auction_id}/asks",
            get(batch_auction_asks),
        )
        .route(
            "/api/batch-auctions/{auction_id}/ask/commit",
            post(batch_auction_ask_commit),
        )
        .route(
            "/api/batch-auctions/{auction_id}/ask/reveal",
            post(batch_auction_ask_reveal),
        )
        .route("/api/history", get(history_current))
        .route("/api/statements", get(statements_current))
        .route("/api/dev/deposit", post(dev_deposit))
        .route("/api/dev/issue", post(dev_issue))
        .route("/api/issuer/issue", post(issuer_issue))
        .route("/api/issuer/supply", get(issuer_supply))
        // Escrow endpoints
        .route("/api/escrow", get(escrow_list))
        .route("/api/escrow/public-key", get(escrow_public_key))
        .route("/api/escrow/{id}", get(escrow_get))
        .route("/api/escrow/{id}/accept", post(escrow_accept))
        .route("/api/escrow/{id}/reject", post(escrow_reject))
        .route("/api/escrow/{id}/verify", post(escrow_verify))
        .route("/api/admin/disputes", get(admin_disputes_list))
        .route("/api/admin/disputes/{id}/rule", post(admin_disputes_rule))
        // Stage 7 Proof & Audit endpoints
        .route("/api/proof/solvency", get(proof_solvency))
        .route("/api/proof/balance", get(proof_balance))
        .route("/api/audit/log", get(audit_log))
        .route("/api/audit/head", get(audit_head))
        .route("/ws", get(ws))
        .route("/ws/market/{market_id}", get(ws_market))
        .with_state(state)
        .merge(dx_auth::router(auth.clone()))
        .merge(dx_admin::router(admin, auth))
        .merge(rate::router(rate_state.clone()))
        // Last, so it wraps every route above: rate limits (src/rate.rs, docs/AUTH.md section 8).
        .layer(axum::middleware::from_fn_with_state(
            rate_state,
            rate::limit,
        ));

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .expect("bind gateway");
    println!("Darkhold Exchange gateway listening on {addr}");
    // `ConnectInfo` hands the rate limiter each request's source address.
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .expect("gateway server");
}

fn queue_capacity() -> usize {
    std::env::var("DX_COMMAND_QUEUE_CAPACITY")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|capacity| *capacity > 0)
        .unwrap_or(1024)
}

async fn health() -> &'static str {
    "ok"
}

#[derive(Serialize)]
struct StateHashView {
    state_hash: String,
}

async fn state_hash(State(state): State<AppState>) -> Result<Json<StateHashView>, ApiError> {
    let state_hash = state.writer.state_hash().await.map_err(writer_error)?;
    Ok(Json(StateHashView {
        state_hash: hex::encode(state_hash),
    }))
}

async fn markets(State(state): State<AppState>) -> Result<Json<Vec<MarketView>>, ApiError> {
    let views = state.writer.markets().await.map_err(writer_error)?;
    Ok(Json(views))
}

async fn book(
    Path(market_id): Path<MarketId>,
    Query(query): Query<BookQuery>,
    State(state): State<AppState>,
) -> Result<Json<BookSnapshot>, ApiError> {
    let depth = query.depth.unwrap_or(20).clamp(1, 200);
    let snapshot = state
        .writer
        .snapshot(market_id, depth)
        .await
        .map_err(writer_error)?;
    match snapshot {
        Some(snapshot) => Ok(Json(snapshot)),
        None => Err(api_error(StatusCode::NOT_FOUND, ReasonCode::UnknownMarket)),
    }
}

async fn trades(
    Path(market_id): Path<MarketId>,
    Query(query): Query<TradeQuery>,
    State(state): State<AppState>,
) -> Result<Json<Vec<Fill>>, ApiError> {
    let limit = query.limit.unwrap_or(50).clamp(1, 200);
    let trades = state
        .writer
        .trades(market_id, limit)
        .await
        .map_err(writer_error)?;
    match trades {
        Some(trades) => Ok(Json(trades)),
        None => Err(api_error(StatusCode::NOT_FOUND, ReasonCode::UnknownMarket)),
    }
}

async fn execute_command(
    state: &AppState,
    user_id: UserId,
    command: Command,
) -> Result<CommandResponse, ApiError> {
    state
        .writer
        .command(user_id, command)
        .await
        .map_err(writer_error)
}

fn finish_command(response: CommandResponse) -> Result<Json<CommandResponse>, ApiError> {
    if response.status == OrderStatus::Rejected {
        let reason = response.reason.unwrap_or(ReasonCode::InternalError);
        return Err(api_error(status_for_reason(reason), reason));
    }
    Ok(Json(response))
}

fn json_body<T>(payload: Result<Json<T>, JsonRejection>) -> Result<T, ApiError> {
    payload
        .map(|Json(body)| body)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, ReasonCode::InvalidRequest))
}

async fn command(
    State(state): State<AppState>,
    headers: HeaderMap,
    payload: Result<Json<Command>, JsonRejection>,
) -> Result<Json<CommandResponse>, ApiError> {
    let user_id = caller_user(&state, &headers)?;
    let command = json_body(payload)?;
    finish_command(execute_command(&state, user_id, command).await?)
}

async fn place_order(
    State(state): State<AppState>,
    headers: HeaderMap,
    payload: Result<Json<Command>, JsonRejection>,
) -> Result<Json<CommandResponse>, ApiError> {
    let user_id = caller_user(&state, &headers)?;
    let command = json_body(payload)?;
    if matches!(command, Command::Cancel(_)) {
        return Err(api_error(
            StatusCode::METHOD_NOT_ALLOWED,
            ReasonCode::CancelNotAllowed,
        ));
    }
    finish_command(execute_command(&state, user_id, command).await?)
}

async fn cancel_order(
    Path(order_id): Path<OrderId>,
    Query(query): Query<CancelQuery>,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<CommandResponse>, ApiError> {
    let user_id = caller_user(&state, &headers)?;
    let client_order_id = query
        .client_order_id
        .ok_or_else(|| api_error(StatusCode::BAD_REQUEST, ReasonCode::InvalidReceipt))?;
    let command = Command::Cancel(Cancel {
        client_order_id,
        order_id,
    });
    finish_command(execute_command(&state, user_id, command).await?)
}

async fn balances(
    Path(user_id): Path<UserId>,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<Balance>>, ApiError> {
    authorize_user(&state.auth, &headers, user_id)?;
    let balances = state.writer.balances(user_id).await.map_err(writer_error)?;
    Ok(Json(balances))
}

async fn open_orders_for(
    state: &AppState,
    user_id: UserId,
) -> Result<Vec<OpenOrderView>, ApiError> {
    state
        .writer
        .open_orders(user_id)
        .await
        .map_err(writer_error)
}

async fn open_orders_current(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<OpenOrderView>>, ApiError> {
    let user_id = caller_user(&state, &headers)?;
    Ok(Json(open_orders_for(&state, user_id).await?))
}

async fn open_orders(
    Path(user_id): Path<UserId>,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<OpenOrderView>>, ApiError> {
    authorize_user(&state.auth, &headers, user_id)?;
    Ok(Json(open_orders_for(&state, user_id).await?))
}

fn metrics_snapshot(state: &AppState) -> MetricsSnapshot {
    let (queue_wait_p99_us, core_p99_us) = state.metrics.p99();
    MetricsSnapshot {
        seq: state.metrics.seq.load(Ordering::Relaxed),
        queue_depth: state.metrics.queue_depth.load(Ordering::Relaxed),
        commands_total: state.metrics.commands_total.load(Ordering::Relaxed),
        rejected_total: state.metrics.rejected_total.load(Ordering::Relaxed),
        queue_wait_p99_us,
        core_p99_us,
        ws_clients: state.metrics.ws_clients.load(Ordering::Relaxed),
        slow_consumers: state.metrics.slow_consumers.load(Ordering::Relaxed),
    }
}

async fn latency_metrics(State(state): State<AppState>) -> Json<MetricsSnapshot> {
    Json(metrics_snapshot(&state))
}

#[derive(Serialize)]
struct SolvencyRootView {
    asset: dx_types::Asset,
    root_hash: String,
    total_liabilities: u64,
    leaf_count: usize,
}

#[derive(Serialize)]
struct SolvencySnapshotView {
    seq: u64,
    roots: Vec<SolvencyRootView>,
}

#[derive(Serialize)]
struct SolvencyProofStepView {
    sibling_hash: String,
    sibling_sum: u64,
    sibling_direction: &'static str,
}

#[derive(Serialize)]
struct SolvencyProofView {
    asset: dx_types::Asset,
    snapshot_seq: u64,
    user_id: UserId,
    salt: String,
    balance: u64,
    path: Vec<SolvencyProofStepView>,
    root_hash: String,
    total_liabilities: u64,
}

#[derive(Serialize)]
struct MySolvencyView {
    seq: u64,
    roots: Vec<SolvencyRootView>,
    proofs: Vec<SolvencyProofView>,
}

fn root_view(root: SolvencyRoot) -> SolvencyRootView {
    SolvencyRootView {
        asset: root.asset,
        root_hash: hex::encode(root.root_hash),
        total_liabilities: root.total_liabilities,
        leaf_count: root.leaf_count,
    }
}

fn snapshot_view(snapshot: &SolvencySnapshot) -> SolvencySnapshotView {
    SolvencySnapshotView {
        seq: snapshot.seq,
        roots: snapshot.roots().into_iter().map(root_view).collect(),
    }
}

fn proof_view(proof: SolvencyProof) -> SolvencyProofView {
    SolvencyProofView {
        asset: proof.asset,
        snapshot_seq: proof.snapshot_seq,
        user_id: proof.user_id,
        salt: hex::encode(proof.salt),
        balance: proof.balance,
        path: proof
            .path
            .into_iter()
            .map(|step| SolvencyProofStepView {
                sibling_hash: hex::encode(step.sibling_hash),
                sibling_sum: step.sibling_sum,
                sibling_direction: match step.sibling_direction {
                    PathDirection::Left => "Left",
                    PathDirection::Right => "Right",
                },
            })
            .collect(),
        root_hash: hex::encode(proof.root_hash),
        total_liabilities: proof.total_liabilities,
    }
}

async fn solvency_roots(
    State(state): State<AppState>,
) -> Result<Json<SolvencySnapshotView>, ApiError> {
    let users = state.auth.user_ids();
    let snapshot = state.writer.solvency(users).await.map_err(writer_error)?;
    Ok(Json(snapshot_view(&snapshot)))
}

async fn solvency_current(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<MySolvencyView>, ApiError> {
    let user_id = caller_user(&state, &headers)?;
    let mut users = state.auth.user_ids();
    if !users.contains(&user_id) {
        users.push(user_id);
    }
    let snapshot = state.writer.solvency(users).await.map_err(writer_error)?;
    let roots: Vec<_> = snapshot.roots().into_iter().map(root_view).collect();
    let proofs = snapshot
        .roots()
        .into_iter()
        .map(|root| {
            snapshot
                .proof(user_id, root.asset)
                .map(proof_view)
                .map_err(|_| {
                    api_error(StatusCode::INTERNAL_SERVER_ERROR, ReasonCode::InternalError)
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Json(MySolvencyView {
        seq: snapshot.seq,
        roots,
        proofs,
    }))
}

// ─── Stage 7 Public Audit Chain & Solvency Proof Handlers ─────────────────

#[derive(Deserialize)]
struct AuditLogQuery {
    from_seq: Option<u64>,
    limit: Option<usize>,
}

#[derive(Serialize)]
struct AuditRecordView {
    seq: u64,
    ts_micros: u64,
    prev_hash_hex: String,
    kind: u8,
    market_id: u32,
    qty: u32,
    price_paise: i64,
    hash_hex: String,
}

fn audit_record_view(r: dkx_hash::AuditRecord) -> AuditRecordView {
    AuditRecordView {
        seq: r.seq,
        ts_micros: r.ts_micros,
        prev_hash_hex: hex::encode(r.prev),
        kind: r.kind,
        market_id: r.market,
        qty: r.qty,
        price_paise: r.price,
        hash_hex: hex::encode(r.hash),
    }
}

async fn audit_log(
    State(state): State<AppState>,
    Query(query): Query<AuditLogQuery>,
) -> Result<Json<Vec<AuditRecordView>>, ApiError> {
    let limit = query.limit.unwrap_or(50).clamp(1, 200);
    let records = state
        .writer
        .audit_log(query.from_seq, limit)
        .await
        .map_err(writer_error)?;
    Ok(Json(records.into_iter().map(audit_record_view).collect()))
}

async fn audit_head(
    State(state): State<AppState>,
) -> Result<Json<Option<AuditRecordView>>, ApiError> {
    let head = state.writer.audit_head().await.map_err(writer_error)?;
    Ok(Json(head.map(audit_record_view)))
}

async fn proof_solvency(
    State(state): State<AppState>,
) -> Result<Json<writer::SolvencyRootData>, ApiError> {
    let root = state.writer.proof_solvency().await.map_err(writer_error)?;
    Ok(Json(root))
}

async fn proof_balance(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<writer::BalanceProofData>, ApiError> {
    let user_id = caller_user(&state, &headers)?;
    let proof = state
        .writer
        .proof_balance(user_id)
        .await
        .map_err(writer_error)?;
    match proof {
        Some(p) => Ok(Json(p)),
        None => Err(api_error(StatusCode::NOT_FOUND, ReasonCode::OrderNotFound)),
    }
}

async fn dev_deposit(
    State(state): State<AppState>,
    payload: Result<Json<DevDeposit>, JsonRejection>,
) -> Result<Json<JournalResponse>, ApiError> {
    require_dev_auth(&state.auth)?;
    let body = json_body(payload)?;
    let journal_id = state
        .writer
        .deposit(body.user_id, body.amount, body.client_request_id)
        .await
        .map_err(writer_error)?
        .map_err(|reason| api_error(status_for_reason(reason), reason))?;
    Ok(Json(JournalResponse { journal_id }))
}

async fn dev_issue(
    State(state): State<AppState>,
    payload: Result<Json<DevIssue>, JsonRejection>,
) -> Result<Json<JournalResponse>, ApiError> {
    require_dev_auth(&state.auth)?;
    let body = json_body(payload)?;
    let journal_id = state
        .writer
        .issue(
            body.user_id,
            body.market_id,
            body.qty,
            body.client_request_id,
        )
        .await
        .map_err(writer_error)?
        .map_err(|reason| api_error(status_for_reason(reason), reason))?;
    Ok(Json(JournalResponse { journal_id }))
}

/// Credits passes to a user. Only a logged-in `issuer` may call it, in dev mode too: unlike the
/// `/api/dev/*` endpoints it never accepts the `x-dev-user` header, so every issue is tied to a
/// real login. Sending the same `client_request_id` again changes nothing (the ledger replies
/// with the first answer), so a retried request cannot create passes twice.
async fn issuer_issue(
    State(state): State<AppState>,
    headers: HeaderMap,
    payload: Result<Json<IssuePasses>, JsonRejection>,
) -> Result<Json<JournalResponse>, ApiError> {
    let issuer_id = require_issuer(&state.auth, &headers)?;
    let body = json_body(payload)?;
    if body.qty == 0 || body.qty > MAX_ISSUE_QTY {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            ReasonCode::InvalidQuantity,
        ));
    }
    let user_id = state
        .auth
        .user_id_of(&body.username)
        .ok_or_else(unknown_user)?;
    let journal_id = state
        .writer
        .issue(user_id, body.market_id, body.qty, body.client_request_id)
        .await
        .map_err(writer_error)?
        .map_err(|reason| api_error(status_for_reason(reason), reason))?;
    println!(
        "issuer {issuer_id}: {} passes of market {} to user {user_id} (journal {journal_id})",
        body.qty, body.market_id
    );
    Ok(Json(JournalResponse { journal_id }))
}

/// For the issuer console: every market and how many of its passes users hold.
async fn issuer_supply(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<SupplyRow>>, ApiError> {
    require_issuer(&state.auth, &headers)?;
    let markets = state.writer.markets().await.map_err(writer_error)?;
    let snapshot = state
        .writer
        .solvency(state.auth.user_ids())
        .await
        .map_err(writer_error)?;
    let roots = snapshot.roots();
    let mut rows: Vec<SupplyRow> = markets
        .into_iter()
        .map(|market| {
            let asset = dx_types::Asset::Pass {
                market_id: market.config.id,
            };
            SupplyRow {
                market_id: market.config.id,
                name: market.config.name,
                passes_held: roots
                    .iter()
                    .find(|root| root.asset == asset)
                    .map_or(0, |root| root.total_liabilities),
            }
        })
        .collect();
    rows.sort_by_key(|row| row.market_id);
    Ok(Json(rows))
}

/// Who is calling: a logged-in user (token) or, in dev only, whoever the old header says.
#[derive(Debug, PartialEq, Eq)]
enum Caller {
    Token {
        user_id: UserId,
        role: dx_auth::Role,
    },
    Dev(UserId),
}

/// A token, if there is one, always wins and must be valid: a bad token is refused, never
/// quietly replaced by the dev header. Without a token, an explicit dev header works only while
/// `DX_DEV_AUTH` is on.
fn identify(auth: &dx_auth::AuthState, headers: &HeaderMap) -> Result<Caller, ApiError> {
    if let Some(value) = headers.get(header::AUTHORIZATION) {
        let token = value
            .to_str()
            .ok()
            .and_then(|text| text.strip_prefix("Bearer "))
            .ok_or_else(unauthorized)?;
        let user = auth
            .verify_token(token.trim())
            .map_err(|_| unauthorized())?;
        return Ok(Caller::Token {
            user_id: user.user_id,
            role: user.role,
        });
    }
    if auth.dev_auth_enabled() {
        return legacy_dev_user(headers).map(Caller::Dev);
    }
    Err(unauthorized())
}

/// The user id an endpoint should act for.
fn caller_user(state: &AppState, headers: &HeaderMap) -> Result<UserId, ApiError> {
    Ok(match identify(&state.auth, headers)? {
        Caller::Token { user_id, .. } | Caller::Dev(user_id) => user_id,
    })
}

/// For endpoints that name a user in the address: a logged-in user may only read their own data
/// (admins may read anyone's). Dev mode keeps working as before.
fn authorize_user(
    auth: &dx_auth::AuthState,
    headers: &HeaderMap,
    user_id: UserId,
) -> Result<(), ApiError> {
    match identify(auth, headers)? {
        Caller::Token { user_id: me, role } if me == user_id || role == dx_auth::Role::Admin => {
            Ok(())
        }
        Caller::Token { .. } => Err(forbidden()),
        Caller::Dev(_) => Ok(()),
    }
}

/// The caller must be logged in as an `issuer`. No token (even in dev mode) is 401, any other
/// role is 403. Returns the issuer's user id.
fn require_issuer(auth: &dx_auth::AuthState, headers: &HeaderMap) -> Result<UserId, ApiError> {
    match identify(auth, headers)? {
        Caller::Token {
            user_id,
            role: dx_auth::Role::Issuer,
        } => Ok(user_id),
        Caller::Token { .. } => Err(forbidden()),
        Caller::Dev(_) => Err(unauthorized()),
    }
}

/// The dev money endpoints exist only while dev auth is on.
fn require_dev_auth(auth: &dx_auth::AuthState) -> Result<(), ApiError> {
    if auth.dev_auth_enabled() {
        Ok(())
    } else {
        Err(forbidden())
    }
}

/// The old header is honoured only in dev, and only when it is sent explicitly. There is no
/// default identity: a tokenless request without `x-dev-user` is refused instead of becoming
/// user 1.
fn legacy_dev_user(headers: &HeaderMap) -> Result<UserId, ApiError> {
    match headers
        .get("x-user-id")
        .or_else(|| headers.get("x-dev-user"))
    {
        Some(val) => val
            .to_str()
            .ok()
            .and_then(|value| value.parse::<UserId>().ok())
            .ok_or_else(|| api_error(StatusCode::BAD_REQUEST, ReasonCode::InvalidReceipt)),
        None => Err(unauthorized()),
    }
}

#[derive(Serialize)]
struct PassWalletView {
    market_id: MarketId,
    event_name: String,
    tier_name: String,
    avail: Qty,
    locked: Qty,
    held: Qty,
}

#[derive(Serialize)]
struct UserWalletView {
    user_id: UserId,
    cash_avail: Minor,
    cash_locked: Minor,
    escrow_cash: Minor,
    passes: Vec<PassWalletView>,
}

#[derive(Deserialize)]
struct TopupPayload {
    amount: Minor,
}

async fn wallet_current(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<UserWalletView>, ApiError> {
    let user_id = caller_user(&state, &headers)?;
    let balances = state.writer.balances(user_id).await.map_err(writer_error)?;
    let markets = state.writer.markets().await.map_err(writer_error)?;
    let escrows = state.writer.escrows(user_id).await.map_err(writer_error)?;
    let escrow_cash = escrows
        .iter()
        .filter(|record| record.state == EscrowState::PendingDelivery && record.buyer_id == user_id)
        .map(|record| {
            record.notional
                + if record.fee_payer == record.buyer_id {
                    record.fee
                } else {
                    0
                }
        })
        .sum();

    let mut cash_avail = 0;
    let mut cash_locked = 0;
    let mut passes = Vec::new();

    for b in balances {
        match b.asset {
            dx_types::Asset::Cash => {
                cash_avail = b.available;
                cash_locked = b.held;
            }
            dx_types::Asset::Pass { market_id } => {
                let m_name = markets
                    .iter()
                    .find(|m| m.config.id == market_id)
                    .map(|m| m.config.name.clone())
                    .unwrap_or_else(|| format!("Market #{market_id}"));
                passes.push(PassWalletView {
                    market_id,
                    event_name: m_name,
                    tier_name: "General".into(),
                    avail: b.available as Qty,
                    locked: b.held as Qty,
                    held: 0,
                });
            }
        }
    }

    for record in escrows.iter().filter(|record| {
        record.state == EscrowState::PendingDelivery
            && (record.buyer_id == user_id || record.seller_id == user_id)
    }) {
        let entry = if let Some(entry) = passes
            .iter_mut()
            .find(|pass| pass.market_id == record.market_id)
        {
            entry
        } else {
            let market_name = markets
                .iter()
                .find(|market| market.config.id == record.market_id)
                .map(|market| market.config.name.clone())
                .unwrap_or_else(|| format!("Market #{}", record.market_id));
            passes.push(PassWalletView {
                market_id: record.market_id,
                event_name: market_name,
                tier_name: "General".into(),
                avail: 0,
                locked: 0,
                held: 0,
            });
            passes.last_mut().expect("escrow pass row")
        };
        entry.held = entry.held.saturating_add(record.qty);
    }

    Ok(Json(UserWalletView {
        user_id,
        cash_avail,
        cash_locked,
        escrow_cash,
        passes,
    }))
}

async fn wallet_topup(
    State(state): State<AppState>,
    headers: HeaderMap,
    payload: Result<Json<TopupPayload>, JsonRejection>,
) -> Result<Json<UserWalletView>, ApiError> {
    let user_id = caller_user(&state, &headers)?;
    let body = json_body(payload)?;
    if body.amount <= 0 {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            ReasonCode::InvalidQuantity,
        ));
    }
    let req_id = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(1);

    state
        .writer
        .deposit(user_id, body.amount, req_id)
        .await
        .map_err(writer_error)?
        .map_err(|reason| api_error(status_for_reason(reason), reason))?;

    wallet_current(State(state), headers).await
}

#[derive(Serialize)]
struct PublicKeyView {
    public_key: Option<String>,
}

#[derive(Deserialize)]
struct PublicKeyPayload {
    public_key: String,
}

async fn public_key_current(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<PublicKeyView>, ApiError> {
    let user_id = caller_user(&state, &headers)?;
    let public_key = state
        .writer
        .public_key(user_id)
        .await
        .map_err(writer_error)?;
    Ok(Json(PublicKeyView {
        public_key: public_key.map(hex::encode),
    }))
}

async fn register_public_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    payload: Result<Json<PublicKeyPayload>, JsonRejection>,
) -> Result<Json<PublicKeyView>, ApiError> {
    let user_id = caller_user(&state, &headers)?;
    let body = json_body(payload)?;
    let bytes = hex::decode(body.public_key.trim())
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, ReasonCode::InvalidReceipt))?;
    let public_key: [u8; 32] = bytes
        .try_into()
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, ReasonCode::InvalidReceipt))?;
    state
        .writer
        .register_public_key(user_id, public_key)
        .await
        .map_err(writer_error)?
        .map_err(|reason| api_error(status_for_reason(reason), reason))?;
    Ok(Json(PublicKeyView {
        public_key: Some(hex::encode(public_key)),
    }))
}

#[derive(Serialize)]
struct EscrowView {
    trade_id: TradeId,
    market_id: MarketId,
    market_name: String,
    buyer_id: UserId,
    seller_id: UserId,
    qty: Qty,
    notional: Minor,
    fee: Minor,
    fee_payer: UserId,
    state: EscrowState,
    created_at_ms: u64,
    deadline_ms: u64,
    delivered_at_ms: Option<u64>,
    role: &'static str,
    buyer_cash: Minor,
    seller_cash: Minor,
}

#[derive(Serialize)]
struct EscrowResponse {
    items: Vec<EscrowView>,
    now_ms: u64,
}

fn escrow_view(record: EscrowRecord, user_id: UserId, market_name: String) -> EscrowView {
    let buyer_fee = if record.fee_payer == record.buyer_id {
        record.fee
    } else {
        0
    };
    let seller_fee = if record.fee_payer == record.seller_id {
        record.fee
    } else {
        0
    };
    EscrowView {
        trade_id: record.trade_id,
        market_id: record.market_id,
        market_name,
        buyer_id: record.buyer_id,
        seller_id: record.seller_id,
        qty: record.qty,
        notional: record.notional,
        fee: record.fee,
        fee_payer: record.fee_payer,
        state: record.state,
        created_at_ms: record.created_at_ms,
        deadline_ms: record.deadline_ms,
        delivered_at_ms: record.delivered_at_ms,
        role: if record.buyer_id == user_id {
            "buyer"
        } else {
            "seller"
        },
        buyer_cash: record.notional.saturating_add(buyer_fee),
        seller_cash: record.notional.saturating_sub(seller_fee),
    }
}

async fn escrow_current(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<EscrowResponse>, ApiError> {
    let user_id = caller_user(&state, &headers)?;
    let records = state.writer.escrows(user_id).await.map_err(writer_error)?;
    let markets = state.writer.markets().await.map_err(writer_error)?;
    let names: std::collections::HashMap<_, _> = markets
        .into_iter()
        .map(|market| (market.config.id, market.config.name))
        .collect();
    let items = records
        .into_iter()
        .map(|record| {
            let name = names
                .get(&record.market_id)
                .cloned()
                .unwrap_or_else(|| format!("Market #{}", record.market_id));
            escrow_view(record, user_id, name)
        })
        .collect();
    Ok(Json(EscrowResponse {
        items,
        now_ms: now_ms(),
    }))
}

#[derive(Deserialize)]
struct ReceiptPayload {
    delivered_at_ms: u64,
    signature: String,
}

#[derive(Serialize)]
struct EscrowActionView {
    trade_id: TradeId,
    state: EscrowState,
    delivered_at_ms: Option<u64>,
}

async fn escrow_receipt(
    Path(trade_id): Path<TradeId>,
    State(state): State<AppState>,
    headers: HeaderMap,
    payload: Result<Json<ReceiptPayload>, JsonRejection>,
) -> Result<Json<EscrowActionView>, ApiError> {
    let user_id = caller_user(&state, &headers)?;
    let body = json_body(payload)?;
    if body.delivered_at_ms == 0 {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            ReasonCode::InvalidReceipt,
        ));
    }
    let signature = hex::decode(body.signature.trim())
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, ReasonCode::InvalidReceipt))?;
    if signature.len() != 64 {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            ReasonCode::InvalidReceipt,
        ));
    }
    let record = state
        .writer
        .release_escrow(user_id, trade_id, body.delivered_at_ms, signature, now_ms())
        .await
        .map_err(writer_error)?
        .map_err(|reason| api_error(status_for_reason(reason), reason))?;
    Ok(Json(EscrowActionView {
        trade_id: record.trade_id,
        state: record.state,
        delivered_at_ms: record.delivered_at_ms,
    }))
}

async fn escrow_refund(
    Path(trade_id): Path<TradeId>,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<EscrowActionView>, ApiError> {
    let user_id = caller_user(&state, &headers)?;
    let record = state
        .writer
        .refund_escrow(user_id, trade_id, now_ms())
        .await
        .map_err(writer_error)?
        .map_err(|reason| api_error(status_for_reason(reason), reason))?;
    Ok(Json(EscrowActionView {
        trade_id: record.trade_id,
        state: record.state,
        delivered_at_ms: record.delivered_at_ms,
    }))
}

#[derive(Deserialize)]
struct AuctionQuery {
    market_id: Option<MarketId>,
}

#[derive(Serialize)]
struct AuctionView {
    id: u64,
    market_id: MarketId,
    market_name: String,
    seller_id: UserId,
    qty_total: Qty,
    qty_remaining: Qty,
    start_price_ticks: Ticks,
    floor_price_ticks: Ticks,
    start_ms: u64,
    end_ms: u64,
    status: AuctionStatus,
    /// Price the server would settle at right now, for this poll.
    current_price_ticks: Ticks,
    current_price_minor: Minor,
    tick_size_minor: Minor,
}

#[derive(Serialize)]
struct AuctionsResponse {
    items: Vec<AuctionView>,
    now_ms: u64,
}

fn auction_view(auction: &DutchAuction, markets: &[MarketView], at_ms: u64) -> AuctionView {
    let market = markets
        .iter()
        .find(|market| market.config.id == auction.market_id);
    let market_name = market
        .map(|market| market.config.name.clone())
        .unwrap_or_else(|| format!("Market #{}", auction.market_id));
    let tick_size_minor = market
        .map(|market| market.config.tick_size_minor)
        .unwrap_or(1)
        .max(1);
    let current_price_ticks = auction.price_at(at_ms);
    let current_price_minor =
        i64::try_from((current_price_ticks as u128) * (tick_size_minor as u128))
            .unwrap_or(i64::MAX);
    AuctionView {
        id: auction.id,
        market_id: auction.market_id,
        market_name,
        seller_id: auction.seller_id,
        qty_total: auction.qty_total,
        qty_remaining: auction.qty_remaining,
        start_price_ticks: auction.start_price_ticks,
        floor_price_ticks: auction.floor_price_ticks,
        start_ms: auction.start_ms,
        end_ms: auction.end_ms,
        status: auction.status,
        current_price_ticks,
        current_price_minor,
        tick_size_minor,
    }
}

async fn auctions_current(
    State(state): State<AppState>,
    Query(query): Query<AuctionQuery>,
) -> Result<Json<AuctionsResponse>, ApiError> {
    let auctions = state
        .writer
        .auctions(query.market_id)
        .await
        .map_err(writer_error)?;
    let markets = state.writer.markets().await.map_err(writer_error)?;
    let at_ms = now_ms();
    let items = auctions
        .iter()
        .map(|auction| auction_view(auction, &markets, at_ms))
        .collect();
    Ok(Json(AuctionsResponse {
        items,
        now_ms: at_ms,
    }))
}

#[derive(Deserialize)]
struct CreateAuctionPayload {
    client_order_id: ClientOrderId,
    market_id: MarketId,
    qty: Qty,
    start_price_ticks: Ticks,
    floor_price_ticks: Ticks,
    duration_ms: u64,
}

async fn auction_create(
    State(state): State<AppState>,
    headers: HeaderMap,
    payload: Result<Json<CreateAuctionPayload>, JsonRejection>,
) -> Result<Json<AuctionView>, ApiError> {
    let seller_id = caller_user(&state, &headers)?;
    let body = json_body(payload)?;
    let request = CreateDutchAuction {
        client_order_id: body.client_order_id,
        market_id: body.market_id,
        qty: body.qty,
        start_price_ticks: body.start_price_ticks,
        floor_price_ticks: body.floor_price_ticks,
        duration_ms: body.duration_ms,
    };
    let at_ms = now_ms();
    let auction = state
        .writer
        .create_auction(seller_id, request, at_ms)
        .await
        .map_err(writer_error)?
        .map_err(|reason| api_error(status_for_reason(reason), reason))?;
    let markets = state.writer.markets().await.map_err(writer_error)?;
    Ok(Json(auction_view(&auction, &markets, at_ms)))
}

#[derive(Deserialize)]
struct AcceptAuctionPayload {
    client_order_id: ClientOrderId,
    qty: Qty,
    max_price_ticks: Ticks,
}

#[derive(Serialize)]
struct AuctionAcceptView {
    auction: AuctionView,
    trade_id: TradeId,
    qty: Qty,
    price_ticks: Ticks,
    notional: Minor,
    fee: Minor,
}

async fn auction_accept(
    Path(auction_id): Path<u64>,
    State(state): State<AppState>,
    headers: HeaderMap,
    payload: Result<Json<AcceptAuctionPayload>, JsonRejection>,
) -> Result<Json<AuctionAcceptView>, ApiError> {
    let buyer_id = caller_user(&state, &headers)?;
    let body = json_body(payload)?;
    let request = AcceptDutchAuction {
        client_order_id: body.client_order_id,
        auction_id,
        qty: body.qty,
        max_price_ticks: body.max_price_ticks,
    };
    let at_ms = now_ms();
    let accepted = state
        .writer
        .accept_auction(buyer_id, request, at_ms)
        .await
        .map_err(writer_error)?
        .map_err(|reason| api_error(status_for_reason(reason), reason))?;
    let auction = state
        .writer
        .auction(auction_id)
        .await
        .map_err(writer_error)?
        .ok_or_else(|| api_error(StatusCode::NOT_FOUND, ReasonCode::OrderNotFound))?;
    let markets = state.writer.markets().await.map_err(writer_error)?;
    Ok(Json(AuctionAcceptView {
        auction: auction_view(&auction, &markets, at_ms),
        trade_id: accepted.trade_id,
        qty: accepted.qty,
        price_ticks: accepted.price_ticks,
        notional: accepted.notional,
        fee: accepted.fee,
    }))
}

async fn auction_expire(
    Path(auction_id): Path<u64>,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<AuctionView>, ApiError> {
    let actor_user_id = caller_user(&state, &headers)?;
    let at_ms = now_ms();
    let auction = state
        .writer
        .expire_auction(actor_user_id, auction_id, at_ms)
        .await
        .map_err(writer_error)?
        .map_err(|reason| api_error(status_for_reason(reason), reason))?;
    let markets = state.writer.markets().await.map_err(writer_error)?;
    Ok(Json(auction_view(&auction, &markets, at_ms)))
}

// ---------------------------------------------------------------------------
// Batch Auction Handlers
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct BatchAuctionView {
    id: u64,
    market_id: MarketId,
    market_name: String,
    /// The admin/issuer that scheduled this window.
    created_by: UserId,
    floor_price_ticks: Ticks,
    floor_price_minor: Minor,
    commit_end_ms: u64,
    reveal_end_ms: u64,
    clearing_price_ticks: Option<Ticks>,
    clearing_price_minor: Option<Minor>,
    phase: BatchAuctionPhase,
    /// Aggregated over valid revealed asks; `None` before settlement.
    qty_total: Option<Qty>,
    qty_remaining: Option<Qty>,
}

fn batch_auction_view(auction: &BatchAuction, markets: &[MarketView]) -> BatchAuctionView {
    let market = markets
        .iter()
        .find(|market| market.config.id == auction.market_id);
    let market_name = market
        .map(|market| market.config.name.clone())
        .unwrap_or_else(|| format!("Market #{}", auction.market_id));
    let tick_size_minor = market
        .map(|market| market.config.tick_size_minor)
        .unwrap_or(1)
        .max(1);

    let floor_price_minor =
        i64::try_from((auction.floor_price_ticks as u128) * (tick_size_minor as u128))
            .unwrap_or(i64::MAX);

    let clearing_price_minor = auction.clearing_price_ticks.map(|ticks| {
        i64::try_from((ticks as u128) * (tick_size_minor as u128)).unwrap_or(i64::MAX)
    });

    BatchAuctionView {
        id: auction.id,
        market_id: auction.market_id,
        market_name,
        created_by: auction.created_by,
        floor_price_ticks: auction.floor_price_ticks,
        floor_price_minor,
        commit_end_ms: auction.commit_end_ms,
        reveal_end_ms: auction.reveal_end_ms,
        clearing_price_ticks: auction.clearing_price_ticks,
        clearing_price_minor,
        phase: auction.phase.clone(),
        qty_total: auction.qty_total,
        qty_remaining: auction.qty_remaining,
    }
}

#[derive(Serialize)]
struct BatchAuctionsResponse {
    items: Vec<BatchAuctionView>,
    now_ms: u64,
}

async fn batch_auctions_current(
    State(state): State<AppState>,
    Query(query): Query<AuctionQuery>,
) -> Result<Json<BatchAuctionsResponse>, ApiError> {
    let auctions = state
        .writer
        .batch_auctions(query.market_id)
        .await
        .map_err(writer_error)?;
    let markets = state.writer.markets().await.map_err(writer_error)?;
    let at_ms = now_ms();
    let items = auctions
        .iter()
        .map(|auction| batch_auction_view(auction, &markets))
        .collect();
    Ok(Json(BatchAuctionsResponse {
        items,
        now_ms: at_ms,
    }))
}

async fn batch_auction_get(
    Path(auction_id): Path<u64>,
    State(state): State<AppState>,
) -> Result<Json<BatchAuctionView>, ApiError> {
    let auction = state
        .writer
        .batch_auction(auction_id)
        .await
        .map_err(writer_error)?
        .ok_or_else(|| api_error(StatusCode::NOT_FOUND, ReasonCode::OrderNotFound))?;
    let markets = state.writer.markets().await.map_err(writer_error)?;
    Ok(Json(batch_auction_view(&auction, &markets)))
}

#[derive(Deserialize)]
struct CreateBatchAuctionPayload {
    client_order_id: ClientOrderId,
    market_id: MarketId,
    floor_price_ticks: Ticks,
    commit_duration_ms: u64,
    reveal_duration_ms: u64,
}

/// Only admin/issuer schedule the window: a seller picking the start time
/// could prearrange bots or friends, and the whole point of the shared window
/// is fairness. Supply enters through sealed asks, never through the scheduler,
/// so nothing is minted here.
async fn batch_auction_create(
    State(state): State<AppState>,
    headers: HeaderMap,
    payload: Result<Json<CreateBatchAuctionPayload>, JsonRejection>,
) -> Result<Json<BatchAuctionView>, ApiError> {
    let created_by = match identify(&state.auth, &headers)? {
        Caller::Token {
            role: dx_auth::Role::Admin | dx_auth::Role::Issuer,
            user_id,
        } => user_id,
        Caller::Dev(user_id) => user_id,
        _ => return Err(forbidden()),
    };
    let body = json_body(payload)?;
    let request = CreateBatchAuction {
        client_order_id: body.client_order_id,
        market_id: body.market_id,
        floor_price_ticks: body.floor_price_ticks,
        commit_duration_ms: body.commit_duration_ms,
        reveal_duration_ms: body.reveal_duration_ms,
    };
    let auction = state
        .writer
        .create_batch_auction(created_by, request, now_ms())
        .await
        .map_err(writer_error)?
        .map_err(|reason| api_error(status_for_reason(reason), reason))?;
    let markets = state.writer.markets().await.map_err(writer_error)?;
    Ok(Json(batch_auction_view(&auction, &markets)))
}

#[derive(Deserialize)]
struct CommitBatchPayload {
    client_order_id: ClientOrderId,
    commitment: String, // hex encoded 32 bytes
}

async fn batch_auction_commit(
    Path(auction_id): Path<u64>,
    State(state): State<AppState>,
    headers: HeaderMap,
    payload: Result<Json<CommitBatchPayload>, JsonRejection>,
) -> Result<Json<BidRecord>, ApiError> {
    let bidder_id = caller_user(&state, &headers)?;
    let body = json_body(payload)?;
    let bytes = hex::decode(body.commitment.trim())
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, ReasonCode::InvalidRequest))?;
    if bytes.len() != 32 {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            ReasonCode::InvalidRequest,
        ));
    }
    let mut commitment = [0u8; 32];
    commitment.copy_from_slice(&bytes);

    let request = CommitBid {
        client_order_id: body.client_order_id,
        auction_id,
        commitment,
    };
    let at_ms = now_ms();
    let record = state
        .writer
        .commit_batch_bid(bidder_id, request, at_ms)
        .await
        .map_err(writer_error)?
        .map_err(|reason| api_error(status_for_reason(reason), reason))?;
    Ok(Json(record))
}

/// Seller side of the sealed window: submit an ask commitment. The payload is
/// identical to the bid side — same SHA-256 pre-image, salt and hex handling —
/// only the ledger reserve at reveal differs (passes instead of cash).
async fn batch_auction_ask_commit(
    Path(auction_id): Path<u64>,
    State(state): State<AppState>,
    headers: HeaderMap,
    payload: Result<Json<CommitBatchPayload>, JsonRejection>,
) -> Result<Json<AskRecord>, ApiError> {
    let seller_id = caller_user(&state, &headers)?;
    let body = json_body(payload)?;
    let bytes = hex::decode(body.commitment.trim())
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, ReasonCode::InvalidRequest))?;
    if bytes.len() != 32 {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            ReasonCode::InvalidRequest,
        ));
    }
    let mut commitment = [0u8; 32];
    commitment.copy_from_slice(&bytes);

    let request = CommitAsk {
        client_order_id: body.client_order_id,
        auction_id,
        commitment,
    };
    let at_ms = now_ms();
    let record = state
        .writer
        .commit_batch_ask(seller_id, request, at_ms)
        .await
        .map_err(writer_error)?
        .map_err(|reason| api_error(status_for_reason(reason), reason))?;
    Ok(Json(record))
}

/// Seller side reveal: on a valid pre-image the seller's passes are reserved.
async fn batch_auction_ask_reveal(
    Path(auction_id): Path<u64>,
    State(state): State<AppState>,
    headers: HeaderMap,
    payload: Result<Json<RevealBatchPayload>, JsonRejection>,
) -> Result<Json<AskRecord>, ApiError> {
    let seller_id = caller_user(&state, &headers)?;
    let body = json_body(payload)?;
    let bytes = hex::decode(body.salt.trim())
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, ReasonCode::InvalidRequest))?;
    if bytes.len() != 32 {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            ReasonCode::InvalidRequest,
        ));
    }
    let mut salt = [0u8; 32];
    salt.copy_from_slice(&bytes);

    let request = RevealAsk {
        client_order_id: body.client_order_id,
        auction_id,
        price_ticks: body.price_ticks,
        qty: body.qty,
        salt,
    };
    let at_ms = now_ms();
    let record = state
        .writer
        .reveal_batch_ask(seller_id, request, at_ms)
        .await
        .map_err(writer_error)?
        .map_err(|reason| api_error(status_for_reason(reason), reason))?;
    Ok(Json(record))
}

async fn batch_auction_asks(
    Path(auction_id): Path<u64>,
    State(state): State<AppState>,
) -> Result<Json<Vec<AskRecord>>, ApiError> {
    let asks = state
        .writer
        .batch_asks(auction_id)
        .await
        .map_err(writer_error)?;
    Ok(Json(asks))
}

#[derive(Deserialize)]
struct RevealBatchPayload {
    client_order_id: ClientOrderId,
    price_ticks: Ticks,
    qty: Qty,
    salt: String, // hex encoded 32 bytes
}

async fn batch_auction_reveal(
    Path(auction_id): Path<u64>,
    State(state): State<AppState>,
    headers: HeaderMap,
    payload: Result<Json<RevealBatchPayload>, JsonRejection>,
) -> Result<Json<BidRecord>, ApiError> {
    let bidder_id = caller_user(&state, &headers)?;
    let body = json_body(payload)?;
    let bytes = hex::decode(body.salt.trim())
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, ReasonCode::InvalidRequest))?;
    if bytes.len() != 32 {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            ReasonCode::InvalidRequest,
        ));
    }
    let mut salt = [0u8; 32];
    salt.copy_from_slice(&bytes);

    let request = RevealBid {
        client_order_id: body.client_order_id,
        auction_id,
        price_ticks: body.price_ticks,
        qty: body.qty,
        salt,
    };
    let at_ms = now_ms();
    let record = state
        .writer
        .reveal_batch_bid(bidder_id, request, at_ms)
        .await
        .map_err(writer_error)?
        .map_err(|reason| api_error(status_for_reason(reason), reason))?;
    Ok(Json(record))
}

#[derive(Deserialize)]
struct SettleBatchPayload {
    client_order_id: ClientOrderId,
}

#[derive(Serialize)]
struct BatchSettleResponse {
    fills: Vec<BatchAuctionFill>,
}

async fn batch_auction_settle(
    Path(auction_id): Path<u64>,
    State(state): State<AppState>,
    headers: HeaderMap,
    payload: Result<Json<SettleBatchPayload>, JsonRejection>,
) -> Result<Json<BatchSettleResponse>, ApiError> {
    let caller_id = caller_user(&state, &headers)?;
    let body = json_body(payload)?;
    let request = SettleBatchAuction {
        client_order_id: body.client_order_id,
        auction_id,
    };
    let at_ms = now_ms();
    let fills = state
        .writer
        .settle_batch_auction(caller_id, request, at_ms)
        .await
        .map_err(writer_error)?
        .map_err(|reason| api_error(status_for_reason(reason), reason))?;
    Ok(Json(BatchSettleResponse { fills }))
}

async fn batch_auction_bids(
    Path(auction_id): Path<u64>,
    State(state): State<AppState>,
) -> Result<Json<Vec<BidRecord>>, ApiError> {
    let bids = state
        .writer
        .batch_bids(auction_id)
        .await
        .map_err(writer_error)?;
    Ok(Json(bids))
}

#[derive(Deserialize)]
struct HistoryQuery {
    market_id: Option<MarketId>,
    side: Option<String>,
    limit: Option<usize>,
}

#[derive(Serialize)]
struct HistoryItem {
    id: u64,
    market_id: MarketId,
    event_name: String,
    tier_name: String,
    side: String,
    price: Minor,
    qty: Qty,
    fee: Minor,
    total_paise: Minor,
    ts: u64,
    escrowed: bool,
}

#[derive(Serialize)]
struct HistoryResponse {
    items: Vec<HistoryItem>,
    next_cursor: Option<u64>,
}

#[derive(Serialize)]
struct StatementItem {
    id: u64,
    ts: u64,
    kind: String,
    account: String,
    asset: String,
    amount: Minor,
}

#[derive(Serialize)]
struct StatementResponse {
    items: Vec<StatementItem>,
    next_cursor: Option<u64>,
}

async fn history_current(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<HistoryQuery>,
) -> Result<Json<HistoryResponse>, ApiError> {
    let user_id = caller_user(&state, &headers)?;
    let markets = state.writer.markets().await.map_err(writer_error)?;
    let limit = query.limit.unwrap_or(50).clamp(1, 200);

    let market_ids: Vec<MarketId> = if let Some(mid) = query.market_id {
        vec![mid]
    } else {
        markets.iter().map(|m| m.config.id).collect()
    };

    let escrow_trades: std::collections::HashSet<_> = state
        .writer
        .escrows(user_id)
        .await
        .map_err(writer_error)?
        .into_iter()
        .map(|record| record.trade_id)
        .collect();
    let mut items = Vec::new();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    for mid in market_ids {
        if let Ok(Some(trades)) = state.writer.trades(mid, limit).await {
            let m_config = markets
                .iter()
                .find(|m| m.config.id == mid)
                .map(|m| &m.config);
            let tick_size = m_config.map(|c| c.tick_size_minor).unwrap_or(100);
            let m_name = m_config
                .map(|c| c.name.clone())
                .unwrap_or_else(|| format!("Market #{mid}"));

            for f in trades {
                if f.maker_user_id == user_id || f.taker_user_id == user_id {
                    let is_taker = f.taker_user_id == user_id;
                    let is_buyer = (f.maker_side == Side::Sell && is_taker)
                        || (f.maker_side == Side::Buy && !is_taker);

                    let side_str = if is_buyer { "buy" } else { "sell" };
                    if let Some(ref req_side) = query.side
                        && req_side.to_lowercase() != side_str
                    {
                        continue;
                    }

                    let price_paise = (f.price_ticks as i64) * tick_size;
                    let notional_paise = price_paise * (f.qty as i64);
                    let fee_paise = if is_taker {
                        notional_paise * (m_config.map(|c| c.taker_fee_bps).unwrap_or(100) as i64)
                            / 10_000
                    } else {
                        0
                    };
                    let total_paise = if is_buyer {
                        notional_paise + fee_paise
                    } else {
                        notional_paise - fee_paise
                    };

                    items.push(HistoryItem {
                        id: f.trade_id,
                        market_id: mid,
                        event_name: m_name.clone(),
                        tier_name: "General".into(),
                        side: side_str.into(),
                        price: price_paise,
                        qty: f.qty,
                        fee: fee_paise,
                        total_paise,
                        ts: now.saturating_sub(f.seq.saturating_mul(1000)),
                        escrowed: escrow_trades.contains(&f.trade_id),
                    });
                }
            }
        }
    }

    items.sort_by_key(|a| std::cmp::Reverse(a.id));
    if items.len() > limit {
        items.truncate(limit);
    }

    Ok(Json(HistoryResponse {
        items,
        next_cursor: None,
    }))
}

async fn statements_current(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<StatementResponse>, ApiError> {
    let user_id = caller_user(&state, &headers)?;
    let balances = state.writer.balances(user_id).await.map_err(writer_error)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    let mut items = Vec::new();
    let mut seq = 1u64;

    for b in balances {
        match b.asset {
            dx_types::Asset::Cash => {
                if b.available > 0 {
                    items.push(StatementItem {
                        id: seq,
                        ts: now.saturating_sub(120_000),
                        kind: "Deposit".into(),
                        account: format!("CashAvail({user_id})"),
                        asset: "Cash".into(),
                        amount: b.available,
                    });
                    seq += 1;
                }
                if b.held > 0 {
                    items.push(StatementItem {
                        id: seq,
                        ts: now.saturating_sub(60_000),
                        kind: "ReserveCash".into(),
                        account: format!("CashLocked({user_id})"),
                        asset: "Cash".into(),
                        amount: b.held,
                    });
                    seq += 1;
                }
            }
            dx_types::Asset::Pass { market_id } => {
                if b.available > 0 {
                    items.push(StatementItem {
                        id: seq,
                        ts: now.saturating_sub(90_000),
                        kind: "IssuePass".into(),
                        account: format!("PassAvail({user_id}, {market_id})"),
                        asset: format!("Pass({market_id})"),
                        amount: b.available,
                    });
                    seq += 1;
                }
            }
        }
    }

    items.sort_by_key(|a| std::cmp::Reverse(a.id));
    Ok(Json(StatementResponse {
        items,
        next_cursor: None,
    }))
}

/// Who is connecting to a feed (docs/AUTH.md section 2.5). `None` is an anonymous viewer, who may
/// watch the public market data.
///
/// * A valid `token` says who it is. A bad token is refused, and so is a `user_id` that names
///   someone else: nobody can connect as another user.
/// * Without a token, a bare `user_id` is believed only while `DX_DEV_AUTH` is on (the old dev
///   behaviour). In a real run it is refused.
/// * Neither: an anonymous viewer.
fn ws_user(auth: &dx_auth::AuthState, query: &WsQuery) -> Result<Option<UserId>, ApiError> {
    if let Some(token) = &query.token {
        let user = auth
            .verify_token(token.trim())
            .map_err(|_| unauthorized())?;
        return match query.user_id {
            Some(claimed) if claimed != user.user_id => Err(forbidden()),
            _ => Ok(Some(user.user_id)),
        };
    }
    match query.user_id {
        Some(claimed) if auth.dev_auth_enabled() => Ok(Some(claimed)),
        Some(_) => Err(unauthorized()),
        None => Ok(None),
    }
}

async fn ws(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Query(query): Query<WsQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = ws_user(&state.auth, &query)?;
    Ok(ws.on_upgrade(move |socket| handle_socket(socket, state, user_id, None)))
}

async fn ws_market(
    Path(market_id): Path<MarketId>,
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Query(query): Query<WsQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = ws_user(&state.auth, &query)?;
    let known_market = state
        .writer
        .market_exists(market_id)
        .await
        .map_err(writer_error)?;
    if !known_market {
        return Err(api_error(StatusCode::NOT_FOUND, ReasonCode::UnknownMarket));
    }
    Ok(ws.on_upgrade(move |socket| handle_socket(socket, state, user_id, Some(market_id))))
}

struct WsClientGuard {
    metrics: Arc<MetricsState>,
}

impl Drop for WsClientGuard {
    fn drop(&mut self) {
        self.metrics.ws_clients.fetch_sub(1, Ordering::Relaxed);
    }
}

async fn stream_baseline(
    state: &AppState,
    market_filter: Option<&[MarketId]>,
    depth: usize,
) -> Result<StreamBaseline, ApiError> {
    state
        .writer
        .baseline(market_filter.map(|ids| ids.to_vec()), depth)
        .await
        .map_err(writer_error)
}

const WS_MAX_BATCH_MESSAGES: usize = 64;
const WS_MAX_BATCH_BYTES: usize = 256 * 1024;

async fn handle_socket(
    mut socket: WebSocket,
    state: AppState,
    user_id: Option<UserId>,
    fixed_market: Option<MarketId>,
) {
    state.metrics.ws_clients.fetch_add(1, Ordering::Relaxed);
    let _client_guard = WsClientGuard {
        metrics: state.metrics.clone(),
    };

    let mut market_filter = fixed_market.map(|market_id| vec![market_id]);
    let mut depth = 20usize;
    let Ok(baseline) = stream_baseline(&state, market_filter.as_deref(), depth).await else {
        eprintln!("ws baseline unavailable");
        return;
    };
    let mut rx = baseline.rx;

    let mut initial =
        Vec::with_capacity(2 + baseline.snapshots.len() + baseline.recent_trades.len());
    initial.push(WsServerMessage::Hello {
        markets: baseline.markets,
        user_id,
    });
    initial.extend(
        baseline
            .snapshots
            .into_iter()
            .map(|snapshot| WsServerMessage::Snapshot { snapshot }),
    );
    initial.extend(
        baseline
            .recent_trades
            .into_iter()
            .map(|fill| WsServerMessage::Trade { fill }),
    );
    initial.push(WsServerMessage::Metrics {
        metrics: metrics_snapshot(&state),
    });
    // Private baseline for a token-checked user: balances and escrow state,
    // so the wallet and escrow views start consistent before any push.
    if let Some(user_id) = user_id {
        initial.extend(private_baseline_messages(&state, user_id).await);
    }
    if !send_batch(&mut socket, initial).await {
        return;
    }

    // Only a signed-in socket listens for private events; an anonymous viewer
    // can never receive one (row 7).
    let mut private_rx = user_id.and_then(|_| state.writer.subscribe_private());

    loop {
        tokio::select! {
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(Message::Binary(bytes))) => {
                        match rmp_serde::from_slice::<WsClientMessage>(&bytes) {
                            Ok(WsClientMessage::Subscribe { market_ids, depth: requested_depth }) => {
                                if fixed_market.is_none() {
                                    market_filter = if market_ids.is_empty() { None } else { Some(market_ids) };
                                }
                                depth = (requested_depth as usize).clamp(1, 200);
                                let Ok(baseline) =
                                    stream_baseline(&state, market_filter.as_deref(), depth).await
                                else {
                                    return;
                                };
                                rx = baseline.rx;
                                let snapshots: Vec<WsServerMessage> = baseline
                                    .snapshots
                                    .into_iter()
                                    .map(|snapshot| WsServerMessage::Snapshot { snapshot })
                                    .collect();
                                if !send_batch(&mut socket, snapshots).await {
                                    return;
                                }
                            }
                            Ok(WsClientMessage::Ping) => {
                                if !send_msg(&mut socket, &WsServerMessage::Pong).await {
                                    return;
                                }
                            }
                            Err(_) => {
                                // Ignore malformed client frames.
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => return,
                    Some(Ok(Message::Ping(payload))) => {
                        if socket.send(Message::Pong(payload)).await.is_err() {
                            return;
                        }
                    }
                    Some(Err(_)) => return,
                    _ => {}
                }
            }
            broadcast = rx.recv() => {
                match broadcast {
                    Ok(message) => {
                        let mut batch = Vec::with_capacity(WS_MAX_BATCH_MESSAGES);
                        if let Some(message) = filter_message(message, market_filter.as_deref(), depth) {
                            batch.push(message);
                        }

                        let mut lagged = None;
                        for _ in 1..WS_MAX_BATCH_MESSAGES {
                            match rx.try_recv() {
                                Ok(message) => {
                                    if let Some(message) =
                                        filter_message(message, market_filter.as_deref(), depth)
                                    {
                                        batch.push(message);
                                    }
                                }
                                Err(TryRecvError::Empty) => break,
                                Err(TryRecvError::Lagged(skipped)) => {
                                    lagged = Some(skipped);
                                    break;
                                }
                                Err(TryRecvError::Closed) => return,
                            }
                        }

                        if !batch.is_empty() && !send_batch(&mut socket, batch).await {
                            return;
                        }
                        if let Some(skipped) = lagged {
                            let Some(new_rx) = resync_socket(
                                &state,
                                &mut socket,
                                market_filter.as_deref(),
                                depth,
                                skipped,
                            )
                            .await
                            else {
                                return;
                            };
                            rx = new_rx;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        let Some(new_rx) = resync_socket(
                            &state,
                            &mut socket,
                            market_filter.as_deref(),
                            depth,
                            skipped,
                        )
                        .await
                        else {
                            return;
                        };
                        rx = new_rx;
                    }
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            }
            private = poll_private(&mut private_rx) => {
                match private {
                    PrivatePoll::Event(event) => {
                        // Targeted by user id: another user's event is dropped
                        // server-side, never sent to this socket.
                        if owned_by_socket(&event, user_id)
                            && !send_msg(&mut socket, &event.message).await
                        {
                            return;
                        }
                    }
                    PrivatePoll::Lagged => {
                        // Missed events include balance updates, so a lagged
                        // private stream must re-baseline, not just continue.
                        if !resync_private_socket(&state, &mut socket, user_id).await {
                            return;
                        }
                    }
                    PrivatePoll::Closed => private_rx = None,
                }
            }
        }
    }
}

/// What a select-branch poll of the private stream returned. With no
/// subscription (anonymous viewer, or a writer without a private channel) the
/// future never resolves, so the branch stays idle.
#[derive(Debug)]
enum PrivatePoll {
    Event(PrivateEvent),
    Lagged,
    Closed,
}

async fn poll_private(private_rx: &mut Option<broadcast::Receiver<PrivateEvent>>) -> PrivatePoll {
    match private_rx {
        Some(rx) => match rx.recv().await {
            Ok(event) => PrivatePoll::Event(event),
            Err(broadcast::error::RecvError::Lagged(_)) => PrivatePoll::Lagged,
            Err(broadcast::error::RecvError::Closed) => PrivatePoll::Closed,
        },
        None => std::future::pending().await,
    }
}

/// Whether a private event belongs to this socket's token-checked user. The
/// anonymous case (`None`) never matches, so a viewer without a token can
/// never receive private state (row 7).
fn owned_by_socket(event: &PrivateEvent, socket_user: Option<UserId>) -> bool {
    socket_user.is_some_and(|socket_user| socket_user == event.user_id)
}

/// Reply with this user's full private state: balances plus escrow records.
async fn private_baseline_messages(state: &AppState, user_id: UserId) -> Vec<WsServerMessage> {
    let mut messages = Vec::new();
    if let Ok(balances) = state.writer.balances(user_id).await {
        messages.push(WsServerMessage::Balances { user_id, balances });
    }
    if let Ok(records) = state.writer.escrows(user_id).await {
        messages.extend(
            records
                .into_iter()
                .map(|record| WsServerMessage::EscrowUpdate { record }),
        );
    }
    messages
}

/// After a private stream falls behind, resend the full private state: a
/// missed balance update seen as a delta would be a lie about money.
async fn resync_private_socket(
    state: &AppState,
    socket: &mut WebSocket,
    user_id: Option<UserId>,
) -> bool {
    let Some(user_id) = user_id else {
        return true;
    };
    state.metrics.slow_consumers.fetch_add(1, Ordering::Relaxed);
    let messages = private_baseline_messages(state, user_id).await;
    messages.is_empty() || send_batch(socket, messages).await
}

async fn resync_socket(
    state: &AppState,
    socket: &mut WebSocket,
    market_filter: Option<&[MarketId]>,
    depth: usize,
    skipped: u64,
) -> Option<broadcast::Receiver<WsServerMessage>> {
    eprintln!("ws client lagged by {skipped} messages; resyncing");
    state.metrics.slow_consumers.fetch_add(1, Ordering::Relaxed);

    let baseline = stream_baseline(state, market_filter, depth).await.ok()?;
    let mut messages = Vec::with_capacity(baseline.markets.len() + baseline.snapshots.len());
    for market in &baseline.markets {
        messages.push(WsServerMessage::ResyncRequired {
            market_id: market.id,
        });
    }
    messages.extend(
        baseline
            .snapshots
            .into_iter()
            .map(|snapshot| WsServerMessage::Snapshot { snapshot }),
    );

    if !send_batch(socket, messages).await {
        return None;
    }
    Some(baseline.rx)
}

fn filter_message(
    message: WsServerMessage,
    market_filter: Option<&[MarketId]>,
    depth: usize,
) -> Option<WsServerMessage> {
    match message {
        WsServerMessage::Snapshot { mut snapshot } => {
            if market_filter.is_some_and(|ids| !ids.contains(&snapshot.market_id)) {
                return None;
            }
            snapshot.bids.truncate(depth);
            snapshot.asks.truncate(depth);
            Some(WsServerMessage::Snapshot { snapshot })
        }
        WsServerMessage::Delta { delta } => {
            if market_filter.is_some_and(|ids| !ids.contains(&delta.market_id)) {
                return None;
            }
            Some(WsServerMessage::Delta { delta })
        }
        WsServerMessage::Trade { fill } => {
            if market_filter.is_some_and(|ids| !ids.contains(&fill.market_id)) {
                return None;
            }
            Some(WsServerMessage::Trade { fill })
        }
        WsServerMessage::ResyncRequired { market_id } => {
            if market_filter.is_some_and(|ids| !ids.contains(&market_id)) {
                return None;
            }
            Some(WsServerMessage::ResyncRequired { market_id })
        }
        other => Some(other),
    }
}

async fn send_batch(
    socket: &mut WebSocket,
    messages: impl IntoIterator<Item = WsServerMessage>,
) -> bool {
    let mut batch_messages = 0usize;
    let mut batch_bytes = 0usize;

    for message in messages {
        let Ok(bytes) = rmp_serde::to_vec_named(&message) else {
            eprintln!("failed to encode ws message");
            return false;
        };
        let bytes_len = bytes.len();

        if batch_messages > 0
            && (batch_messages >= WS_MAX_BATCH_MESSAGES
                || batch_bytes.saturating_add(bytes_len) > WS_MAX_BATCH_BYTES)
        {
            if socket.flush().await.is_err() {
                return false;
            }
            batch_messages = 0;
            batch_bytes = 0;
        }

        if socket.feed(Message::Binary(bytes.into())).await.is_err() {
            return false;
        }
        batch_messages += 1;
        batch_bytes = batch_bytes.saturating_add(bytes_len);
    }

    if batch_messages == 0 {
        return true;
    }
    socket.flush().await.is_ok()
}

async fn send_msg(socket: &mut WebSocket, message: &WsServerMessage) -> bool {
    let Ok(bytes) = rmp_serde::to_vec_named(message) else {
        eprintln!("failed to encode ws message");
        return false;
    };
    socket.send(Message::Binary(bytes.into())).await.is_ok()
}

fn seed_demo(core: &mut Core) -> Vec<Fill> {
    for user_id in 1..=6 {
        core.deposit_cash(user_id, 25_000_000, 1)
            .expect("seed deposit");
        core.issue_passes(user_id, 1, 2, 1)
            .expect("seed market 1 passes");
        core.issue_passes(user_id, 2, 1, 1)
            .expect("seed market 2 passes");
        core.issue_passes(user_id, 3, 2, 1)
            .expect("seed market 3 passes");
        let dummy_key = ed25519_dalek::SigningKey::from_bytes(&[user_id as u8 + 1; 32])
            .verifying_key()
            .to_bytes();
        let _ = core.register_public_key(user_id, dummy_key);
    }

    let seed_orders = [
        (1, 1, Side::Buy, 4_900, 2),
        (3, 1, Side::Buy, 4_800, 2),
        (2, 1, Side::Sell, 5_100, 2),
        (4, 1, Side::Sell, 5_200, 2),
        (1, 2, Side::Buy, 9_800, 1),
        (3, 2, Side::Buy, 9_700, 1),
        (2, 2, Side::Sell, 10_200, 1),
        (4, 2, Side::Sell, 10_300, 1),
    ];

    for (user_id, market_id, side, price_ticks, qty) in seed_orders {
        let response = core.process(
            user_id,
            Command::PlaceLimit(PlaceLimit {
                client_order_id: user_id * 100 + market_id as u64,
                market_id,
                side,
                price_ticks,
                qty,
                escrow: false,
            }),
        );
        assert_eq!(
            response.status,
            OrderStatus::Rested,
            "seed order failed: {response:?}"
        );
    }

    // One completed trade so the tape is not empty on first load. This sweeps
    // one unit of user 2's ask; user 6 replenishes the level.
    let mut fills = Vec::new();
    let sweep = core.process(
        5,
        Command::PlaceMarket(PlaceMarket {
            client_order_id: 501,
            market_id: 1,
            side: Side::Buy,
            qty: 1,
            protect_price_ticks: 5_200,
            escrow: false,
        }),
    );
    assert_eq!(sweep.status, OrderStatus::Filled, "seed sweep: {sweep:?}");
    fills.extend(sweep.fills);

    let replenish = core.process(
        6,
        Command::PlaceLimit(PlaceLimit {
            client_order_id: 601,
            market_id: 1,
            side: Side::Sell,
            price_ticks: 5_100,
            qty: 1,
            escrow: false,
        }),
    );
    assert_eq!(
        replenish.status,
        OrderStatus::Rested,
        "seed replenish: {replenish:?}"
    );

    core.check_invariants().expect("seed invariants");
    fills
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::writer::spawn_writer;
    use dx_types::{BookDelta, BookLevel, LevelChange, Ticks};
    use tempfile::tempdir;

    const MARKET: MarketId = 1;
    const PRICE: Ticks = 5_000;
    const TEST_SECRET: &[u8] = b"gateway-test-secret-gateway-test-123";

    fn test_state() -> (AppState, broadcast::Receiver<WsServerMessage>) {
        let mut core = Core::new(seed_markets()).unwrap();
        core.deposit_cash(1, 10_000_000, 1).unwrap();
        core.deposit_cash(2, 10_000_000, 2).unwrap();
        core.issue_passes(2, MARKET, 5, 1).unwrap();

        let (tx, rx) = broadcast::channel(64);
        let (writer, metrics) = spawn_writer(core, tx, Vec::new(), 64);
        let auth = dx_auth::AuthState::new(TEST_SECRET, std::time::Duration::from_secs(60), true);
        (
            AppState {
                writer,
                metrics,
                auth,
            },
            rx,
        )
    }

    async fn run_command(state: &AppState, user_id: UserId, command: Command) -> CommandResponse {
        execute_command(state, user_id, command).await.unwrap()
    }

    /// Row 7: private pushes are routed by the token-checked identity, so an
    /// anonymous socket (`None`, dev-header fallback) never matches, and a
    /// user only matches their own events.
    #[test]
    fn private_events_route_to_the_token_user_only() {
        let event = PrivateEvent {
            user_id: 7,
            message: WsServerMessage::Balances {
                user_id: 7,
                balances: Vec::new(),
            },
        };
        assert!(owned_by_socket(&event, Some(7)));
        assert!(!owned_by_socket(&event, Some(8)));
        assert!(!owned_by_socket(&event, None));
    }

    #[tokio::test]
    async fn wal_open_state_replays_on_the_seeded_baseline() {
        let dir = tempdir().unwrap();
        let recovered = dkx_wal::recover(dir.path()).unwrap();
        let wal =
            wal::WalSession::open(dir.path(), recovered.last_seq, recovered.last_hash).unwrap();
        let (core, seed_fills) = seeded_core();
        let seed_fill_count = seed_fills.len();
        let (tx, _rx) = broadcast::channel(16);
        let (writer, _metrics) =
            spawn_writer_with_wal(core, tx, seed_fills, 16, Some(wal), Vec::new(), None, None);

        let response = writer
            .command(1, limit(900001, Side::Sell, 5_200, 1))
            .await
            .unwrap();
        assert_eq!(response.status, OrderStatus::Rested);
        let expected_hash = writer.state_hash().await.unwrap();
        writer.shutdown().await.unwrap();
        drop(writer);

        let (restored, recent_trades, replayed_audit, _session) =
            open_state(dir.path().to_str().unwrap()).unwrap();
        assert_eq!(restored.state_hash(), expected_hash);
        assert_eq!(recent_trades.len(), seed_fill_count);
        // A rest is not part of the audit chain (no fills), so nothing was logged.
        assert!(replayed_audit.is_empty());
    }

    fn limit(client_order_id: ClientOrderId, side: Side, price_ticks: Ticks, qty: Qty) -> Command {
        Command::PlaceLimit(PlaceLimit {
            client_order_id,
            market_id: MARKET,
            side,
            price_ticks,
            qty,
            escrow: false,
        })
    }

    fn market_order(client_order_id: ClientOrderId, side: Side, qty: Qty) -> Command {
        Command::PlaceMarket(PlaceMarket {
            client_order_id,
            market_id: MARKET,
            side,
            qty,
            protect_price_ticks: PRICE,
            escrow: false,
        })
    }

    fn drain(rx: &mut broadcast::Receiver<WsServerMessage>) -> Vec<WsServerMessage> {
        let mut messages = Vec::new();
        while let Ok(message) = rx.try_recv() {
            messages.push(message);
        }
        messages
    }

    fn apply_delta(snapshot: &mut BookSnapshot, delta: &BookDelta) {
        assert_eq!(snapshot.market_id, delta.market_id);
        assert_eq!(delta.book_seq, snapshot.book_seq + 1);

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
                Side::Buy => {
                    levels.sort_unstable_by_key(|level| std::cmp::Reverse(level.price_ticks));
                }
                Side::Sell => levels.sort_unstable_by_key(|level| level.price_ticks),
            }
        }
        snapshot.book_seq = delta.book_seq;
    }

    #[tokio::test]
    async fn admin_publisher_creates_a_live_market() {
        let (state, _rx) = test_state();
        let publisher = market_publisher(state.writer.clone());
        let mut config = seed_markets().into_iter().next().unwrap();
        config.id = 100;
        config.event_id = 99;
        config.name = "Published market".into();
        config.status = dx_types::MarketStatus::Open;

        publisher(config.clone()).await.unwrap();

        assert!(state.writer.market_exists(100).await.unwrap());
        assert_eq!(state.metrics.seq.load(Ordering::Relaxed), 1);
        let markets = state.writer.markets().await.unwrap();
        let market = markets
            .iter()
            .find(|market| market.config.id == 100)
            .expect("published market is visible");
        assert_eq!(market.config.name, "Published market");
        assert_eq!(market.snapshot.book_seq, 0);
        assert!(publisher(config).await.is_err());
    }

    #[tokio::test]
    async fn commands_publish_delta_without_snapshot() {
        let (state, mut rx) = test_state();

        let response = run_command(&state, 1, limit(1, Side::Buy, 4_900, 2)).await;

        assert_eq!(response.status, OrderStatus::Rested);
        let messages = drain(&mut rx);
        assert_eq!(messages.len(), 1);
        let WsServerMessage::Delta { delta } = &messages[0] else {
            panic!("expected delta, got {:?}", messages[0]);
        };
        assert_eq!(delta.market_id, MARKET);
        assert_eq!(delta.book_seq, 1);
        assert_eq!(
            delta.changes,
            vec![LevelChange {
                side: Side::Buy,
                price_ticks: 4_900,
                qty: 2,
            }]
        );

        let metrics = metrics_snapshot(&state);
        assert_eq!(metrics.seq, 1);
        assert_eq!(metrics.commands_total, 1);
        assert_eq!(metrics.queue_depth, 0);
        assert!(state.metrics.latency_samples() > 0);
    }

    #[tokio::test]
    async fn fills_publish_delta_then_trade() {
        let (state, mut rx) = test_state();
        run_command(&state, 2, limit(1, Side::Sell, PRICE, 2)).await;
        drain(&mut rx);

        let command = market_order(1, Side::Buy, 2);
        let response = run_command(&state, 1, command.clone()).await;

        assert_eq!(response.status, OrderStatus::Filled);
        let messages = drain(&mut rx);
        assert_eq!(messages.len(), 2, "{messages:?}");

        let WsServerMessage::Delta { delta } = &messages[0] else {
            panic!("expected delta first, got {:?}", messages[0]);
        };
        assert_eq!(delta.book_seq, 2);
        assert_eq!(
            delta.changes,
            vec![LevelChange {
                side: Side::Sell,
                price_ticks: PRICE,
                qty: 0,
            }]
        );

        let WsServerMessage::Trade { fill } = &messages[1] else {
            panic!("expected trade second, got {:?}", messages[1]);
        };
        assert_eq!(fill.qty, 2);
        assert_eq!(fill.price_ticks, PRICE);
        assert_eq!(
            state
                .writer
                .trades(MARKET, 10)
                .await
                .unwrap()
                .unwrap()
                .len(),
            1
        );

        let duplicate = run_command(&state, 1, command).await;
        assert_eq!(duplicate, response);
        assert!(drain(&mut rx).is_empty());
        assert_eq!(
            state
                .writer
                .trades(MARKET, 10)
                .await
                .unwrap()
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn cached_and_rejected_commands_publish_nothing() {
        let (state, mut rx) = test_state();
        let command = limit(1, Side::Buy, 4_900, 2);
        let first = run_command(&state, 1, command.clone()).await;
        assert_eq!(first.status, OrderStatus::Rested);
        drain(&mut rx);

        let duplicate = run_command(&state, 1, command).await;
        assert_eq!(duplicate, first);
        assert!(drain(&mut rx).is_empty());

        let rejected = run_command(&state, 1, limit(2, Side::Buy, 2_499, 1)).await;
        assert_eq!(rejected.status, OrderStatus::Rejected);
        assert!(drain(&mut rx).is_empty());

        let conflicting = run_command(
            &state,
            1,
            Command::Cancel(Cancel {
                client_order_id: 1,
                order_id: first.order_id.unwrap(),
            }),
        )
        .await;
        assert_eq!(conflicting.status, OrderStatus::Rejected);
        assert_eq!(conflicting.reason, Some(ReasonCode::DuplicateClientOrderId));
        assert!(drain(&mut rx).is_empty());

        assert_eq!(state.metrics.commands_total.load(Ordering::Relaxed), 4);
        assert_eq!(state.metrics.rejected_total.load(Ordering::Relaxed), 2);
        let snapshot = state.writer.snapshot(MARKET, 10).await.unwrap().unwrap();
        assert_eq!(snapshot.book_seq, 1);
    }

    #[tokio::test]
    async fn baseline_snapshot_and_stream_are_joined_without_a_gap() {
        let (state, _old_rx) = test_state();
        run_command(&state, 2, limit(1, Side::Sell, PRICE, 2)).await;

        let baseline = stream_baseline(&state, Some(&[MARKET]), 10).await.unwrap();
        assert_eq!(baseline.markets.len(), 1);
        assert_eq!(baseline.snapshots.len(), 1);
        let mut snapshot = baseline.snapshots.into_iter().next().unwrap();
        assert_eq!(snapshot.book_seq, 1);
        let mut rx = baseline.rx;

        let repeat = stream_baseline(&state, Some(&[MARKET]), 10).await.unwrap();
        assert_eq!(repeat.snapshots[0], snapshot);

        let response = run_command(&state, 1, market_order(1, Side::Buy, 1)).await;
        assert_eq!(response.status, OrderStatus::Filled);

        let messages = drain(&mut rx);
        assert_eq!(messages.len(), 2, "{messages:?}");
        let WsServerMessage::Delta { delta } = &messages[0] else {
            panic!("expected delta first, got {:?}", messages[0]);
        };
        apply_delta(&mut snapshot, delta);
        let WsServerMessage::Trade { fill } = &messages[1] else {
            panic!("expected trade second, got {:?}", messages[1]);
        };
        snapshot.last_trade_price = Some(fill.price_ticks);

        let expected = state.writer.snapshot(MARKET, 10).await.unwrap();
        assert_eq!(Some(snapshot.clone()), expected);

        let after = stream_baseline(&state, Some(&[MARKET]), 10).await.unwrap();
        assert_eq!(after.snapshots[0], snapshot);
        assert_eq!(after.snapshots[0].book_seq, 2);
    }

    #[tokio::test]
    async fn concurrent_commands_publish_monotonic_book_sequences() {
        let (state, mut rx) = test_state();
        let mut tasks = Vec::new();
        for (client_order_id, price_ticks) in [(10, 4_900), (11, 4_800)] {
            let state = state.clone();
            tasks.push(tokio::spawn(async move {
                run_command(&state, 1, limit(client_order_id, Side::Buy, price_ticks, 1)).await
            }));
        }
        for task in tasks {
            assert_eq!(task.await.unwrap().status, OrderStatus::Rested);
        }

        let messages = drain(&mut rx);
        let sequences = messages
            .iter()
            .filter_map(|message| match message {
                WsServerMessage::Delta { delta } => Some(delta.book_seq),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(sequences, vec![1, 2]);
    }

    #[test]
    fn maps_queue_overload_to_429_with_retry_after() {
        let response = writer_error(WriterError::QueueFull).into_response();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            response.headers().get(header::RETRY_AFTER).unwrap(),
            HeaderValue::from_static("1")
        );
    }

    #[test]
    fn maps_business_rejections_to_http_statuses() {
        assert_eq!(
            status_for_reason(ReasonCode::UnknownMarket),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            status_for_reason(ReasonCode::NotOrderOwner),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            status_for_reason(ReasonCode::DuplicateClientOrderId),
            StatusCode::CONFLICT
        );
        assert_eq!(
            status_for_reason(ReasonCode::OutsideCollar),
            StatusCode::UNPROCESSABLE_ENTITY
        );
        assert_eq!(
            status_for_reason(ReasonCode::QueueFull),
            StatusCode::TOO_MANY_REQUESTS
        );
        assert_eq!(
            status_for_reason(ReasonCode::InternalError),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(
            status_for_reason(ReasonCode::InvalidRequest),
            StatusCode::BAD_REQUEST
        );
    }

    #[test]
    fn filters_delta_and_resync_by_market() {
        let delta = WsServerMessage::Delta {
            delta: BookDelta {
                market_id: 1,
                book_seq: 11,
                changes: vec![LevelChange {
                    side: Side::Buy,
                    price_ticks: 5_000,
                    qty: 3,
                }],
            },
        };
        assert!(filter_message(delta.clone(), Some(&[2]), 20).is_none());
        let expected = delta.clone();
        assert_eq!(filter_message(delta, Some(&[1]), 20), Some(expected));

        let resync = WsServerMessage::ResyncRequired { market_id: 1 };
        assert!(filter_message(resync.clone(), Some(&[2]), 20).is_none());
        assert_eq!(
            filter_message(resync, Some(&[1]), 20),
            Some(WsServerMessage::ResyncRequired { market_id: 1 })
        );
    }

    // ---- who is calling (docs/AUTH.md) ----

    fn auth_state(dev_auth: bool) -> dx_auth::AuthState {
        dx_auth::AuthState::new(TEST_SECRET, std::time::Duration::from_secs(60), dev_auth)
    }

    fn token_for(auth: &dx_auth::AuthState, user_id: UserId, role: dx_auth::Role) -> String {
        let user = dx_auth::PublicUser {
            user_id,
            username: "someone".into(),
            role,
        };
        auth.issue_token(&user).unwrap()
    }

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(
                axum::http::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_str(value).unwrap(),
            );
        }
        map
    }

    fn refused(result: Result<Caller, ApiError>, status: StatusCode, reason: &str) {
        let error = result.expect_err("should be refused");
        assert_eq!(error.status, status);
        assert_eq!(error.raw_reason, Some(reason));
    }

    #[test]
    fn a_token_identifies_the_caller_and_beats_the_dev_header() {
        let auth = auth_state(true);
        let token = token_for(&auth, 7, dx_auth::Role::User);
        let map = headers(&[
            ("authorization", &format!("Bearer {token}")),
            ("x-dev-user", "99"),
        ]);
        assert_eq!(
            identify(&auth, &map).unwrap(),
            Caller::Token {
                user_id: 7,
                role: dx_auth::Role::User
            }
        );
    }

    #[test]
    fn a_bad_token_is_refused_even_with_the_dev_header() {
        let auth = auth_state(true);
        for value in ["Bearer junk", "Basic abc", "Bearer "] {
            let map = headers(&[("authorization", value), ("x-dev-user", "1")]);
            refused(
                identify(&auth, &map),
                StatusCode::UNAUTHORIZED,
                "unauthorized",
            );
        }
        // A token signed with a different secret is a forgery.
        let other = dx_auth::AuthState::new(
            b"another-secret-another-secret-12345",
            std::time::Duration::from_secs(60),
            true,
        );
        let forged = token_for(&other, 1, dx_auth::Role::Admin);
        let map = headers(&[("authorization", &format!("Bearer {forged}"))]);
        refused(
            identify(&auth, &map),
            StatusCode::UNAUTHORIZED,
            "unauthorized",
        );
    }

    #[test]
    fn without_a_token_the_dev_header_works_only_while_dev_auth_is_on() {
        let on = auth_state(true);
        assert_eq!(
            identify(&on, &headers(&[("x-dev-user", "5")])).unwrap(),
            Caller::Dev(5)
        );
        // No header and no token is not an identity, even in dev.
        refused(
            identify(&on, &HeaderMap::new()),
            StatusCode::UNAUTHORIZED,
            "unauthorized",
        );

        let off = auth_state(false);
        refused(
            identify(&off, &headers(&[("x-dev-user", "5")])),
            StatusCode::UNAUTHORIZED,
            "unauthorized",
        );
        refused(
            identify(&off, &HeaderMap::new()),
            StatusCode::UNAUTHORIZED,
            "unauthorized",
        );
    }

    #[test]
    fn logged_in_users_may_only_read_their_own_data() {
        let auth = auth_state(false);
        let user = headers(&[(
            "authorization",
            &format!("Bearer {}", token_for(&auth, 7, dx_auth::Role::User)),
        )]);
        assert!(authorize_user(&auth, &user, 7).is_ok());
        let error = authorize_user(&auth, &user, 8).unwrap_err();
        assert_eq!(
            (error.status, error.raw_reason),
            (StatusCode::FORBIDDEN, Some("forbidden"))
        );

        let admin = headers(&[(
            "authorization",
            &format!("Bearer {}", token_for(&auth, 100, dx_auth::Role::Admin)),
        )]);
        assert!(authorize_user(&auth, &admin, 8).is_ok());

        // No token and no dev auth: refused.
        assert!(authorize_user(&auth, &HeaderMap::new(), 7).is_err());
        // Dev mode still needs an explicit dev header; it never defaults to user 1.
        assert!(authorize_user(&auth_state(true), &HeaderMap::new(), 7).is_err());
        assert!(authorize_user(&auth_state(true), &headers(&[("x-dev-user", "7")]), 7).is_ok());
    }

    #[test]
    fn a_feed_connection_is_identified_by_its_token_and_never_by_a_claimed_id() {
        let ws_query = |user_id, token: Option<String>| WsQuery { user_id, token };
        let dev = auth_state(true);
        let real = auth_state(false);
        let token = token_for(&dev, 7, dx_auth::Role::User);
        let real_token = token_for(&real, 7, dx_auth::Role::User);
        let refused = |result: Result<Option<UserId>, ApiError>| {
            let error = result.unwrap_err();
            (error.status, error.raw_reason)
        };

        // A valid token says who it is, with or without the matching id.
        assert_eq!(
            ws_user(&dev, &ws_query(None, Some(token.clone()))).unwrap(),
            Some(7)
        );
        assert_eq!(
            ws_user(&dev, &ws_query(Some(7), Some(token.clone()))).unwrap(),
            Some(7)
        );
        assert_eq!(
            ws_user(&real, &ws_query(None, Some(real_token.clone()))).unwrap(),
            Some(7)
        );
        // Someone else's id next to a valid token is refused, in dev mode too.
        for auth in [&dev, &real] {
            let token = token_for(auth, 7, dx_auth::Role::User);
            assert_eq!(
                refused(ws_user(auth, &ws_query(Some(8), Some(token)))),
                (StatusCode::FORBIDDEN, Some("forbidden"))
            );
        }
        // A bad token is refused, never replaced by the id, even in dev mode.
        assert_eq!(
            refused(ws_user(&dev, &ws_query(Some(7), Some("junk".into())))),
            (StatusCode::UNAUTHORIZED, Some("unauthorized"))
        );
        // A token made with another secret is a bad token.
        let stranger = dx_auth::AuthState::new(
            b"some-other-secret-some-other-secret-1",
            std::time::Duration::from_secs(60),
            false,
        );
        let foreign = token_for(&stranger, 7, dx_auth::Role::User);
        assert_eq!(
            refused(ws_user(&real, &ws_query(None, Some(foreign)))),
            (StatusCode::UNAUTHORIZED, Some("unauthorized"))
        );
        // A bare id counts only in dev mode.
        assert_eq!(ws_user(&dev, &ws_query(Some(5), None)).unwrap(), Some(5));
        assert_eq!(
            refused(ws_user(&real, &ws_query(Some(5), None))),
            (StatusCode::UNAUTHORIZED, Some("unauthorized"))
        );
        // No identity at all is an anonymous viewer, in both modes.
        assert_eq!(ws_user(&dev, &ws_query(None, None)).unwrap(), None);
        assert_eq!(ws_user(&real, &ws_query(None, None)).unwrap(), None);
    }

    #[test]
    fn the_dev_money_endpoints_need_dev_auth() {
        assert!(require_dev_auth(&auth_state(true)).is_ok());
        let error = require_dev_auth(&auth_state(false)).unwrap_err();
        assert_eq!(
            (error.status, error.raw_reason),
            (StatusCode::FORBIDDEN, Some("forbidden"))
        );
    }

    #[test]
    fn only_a_logged_in_issuer_passes_the_issuer_check() {
        let auth = auth_state(true);
        let bearer = |role| {
            let token = token_for(&auth, 101, role);
            headers(&[("authorization", &format!("Bearer {token}"))])
        };
        assert_eq!(
            require_issuer(&auth, &bearer(dx_auth::Role::Issuer)).unwrap(),
            101
        );
        for role in [dx_auth::Role::User, dx_auth::Role::Admin] {
            let error = require_issuer(&auth, &bearer(role)).unwrap_err();
            assert_eq!(
                (error.status, error.raw_reason),
                (StatusCode::FORBIDDEN, Some("forbidden"))
            );
        }
        // The dev header is never enough, even while dev auth is on, and so is no header at all.
        for map in [headers(&[("x-dev-user", "101")]), headers(&[])] {
            let error = require_issuer(&auth, &map).unwrap_err();
            assert_eq!(
                (error.status, error.raw_reason),
                (StatusCode::UNAUTHORIZED, Some("unauthorized"))
            );
        }
    }

    #[tokio::test]
    async fn an_issuer_credits_passes_by_username_and_a_retry_changes_nothing() {
        let (state, _rx) = test_state();
        state
            .auth
            .seed_user(7, "buyer", "password-123", dx_auth::Role::User);
        let token = token_for(&state.auth, 101, dx_auth::Role::Issuer);
        let map = headers(&[("authorization", &format!("Bearer {token}"))]);
        let issue = |username: &str, market_id: MarketId, qty: Qty, request: ClientOrderId| {
            issuer_issue(
                State(state.clone()),
                map.clone(),
                Ok(Json(IssuePasses {
                    username: username.into(),
                    market_id,
                    qty,
                    client_request_id: request,
                })),
            )
        };
        let passes_of_7 = || async {
            state
                .writer
                .balances(7)
                .await
                .unwrap()
                .into_iter()
                .find(|balance| balance.asset == dx_types::Asset::Pass { market_id: 1 })
                .map_or(0, |balance| balance.available)
        };

        // The name is matched ignoring capitals and spaces.
        let Json(first) = issue(" Buyer ", 1, 3, 900).await.unwrap();
        assert_eq!(passes_of_7().await, 3);

        // The same request again gives the same answer and creates nothing.
        let Json(again) = issue("buyer", 1, 3, 900).await.unwrap();
        assert_eq!(again.journal_id, first.journal_id);
        assert_eq!(passes_of_7().await, 3);

        // A new request id is a new issue.
        let _ = issue("buyer", 1, 2, 901).await.unwrap();
        assert_eq!(passes_of_7().await, 5);

        // Bad requests are refused and change nothing.
        for qty in [0, MAX_ISSUE_QTY + 1] {
            let error = issue("buyer", 1, qty, 902).await.unwrap_err();
            assert_eq!(error.status, StatusCode::BAD_REQUEST);
        }
        let error = issue("nobody", 1, 1, 903).await.unwrap_err();
        assert_eq!(
            (error.status, error.raw_reason),
            (StatusCode::NOT_FOUND, Some("unknown_user"))
        );
        let error = issue("buyer", 99, 1, 904).await.unwrap_err();
        assert_eq!(error.status, StatusCode::NOT_FOUND);
        assert_eq!(passes_of_7().await, 5);
    }

    #[tokio::test]
    async fn the_supply_view_counts_the_passes_users_hold() {
        let (state, _rx) = test_state();
        state
            .auth
            .seed_user(7, "buyer", "password-123", dx_auth::Role::User);
        let token = token_for(&state.auth, 101, dx_auth::Role::Issuer);
        let map = headers(&[("authorization", &format!("Bearer {token}"))]);

        let Json(before) = issuer_supply(State(state.clone()), map.clone())
            .await
            .unwrap();
        let held = |rows: &[SupplyRow]| {
            rows.iter()
                .find(|row| row.market_id == MARKET)
                .map(|row| row.passes_held)
        };
        // `test_state` already gave user 2 five passes of this market.
        assert_eq!(held(&before), Some(5));
        assert!(
            before
                .windows(2)
                .all(|pair| pair[0].market_id < pair[1].market_id)
        );

        state.writer.issue(7, MARKET, 4, 1).await.unwrap().unwrap();
        let Json(after) = issuer_supply(State(state.clone()), map).await.unwrap();
        assert_eq!(held(&after), Some(9));

        // An ordinary user cannot see it.
        let user_token = token_for(&state.auth, 7, dx_auth::Role::User);
        let user_map = headers(&[("authorization", &format!("Bearer {user_token}"))]);
        let error = issuer_supply(State(state), user_map).await.err().unwrap();
        assert_eq!(error.status, StatusCode::FORBIDDEN);
    }

    fn batch_payload(
        client_order_id: ClientOrderId,
        market_id: MarketId,
        floor: Ticks,
    ) -> CreateBatchAuctionPayload {
        CreateBatchAuctionPayload {
            client_order_id,
            market_id,
            floor_price_ticks: floor,
            commit_duration_ms: 5_000,
            reveal_duration_ms: 5_000,
        }
    }

    fn bearer(auth: &dx_auth::AuthState, user_id: UserId, role: dx_auth::Role) -> HeaderMap {
        let token = token_for(auth, user_id, role);
        headers(&[("authorization", &format!("Bearer {token}"))])
    }

    /// Windows are scheduled by admin/issuer only; the scheduler holds no
    /// supply, so scheduling never mints anything and users cannot start
    /// windows of their own.
    #[tokio::test]
    async fn batch_window_is_admin_scheduled_with_no_sponsorship_mint() {
        let (state, _rx) = test_state();

        // Issuer schedules a pooled window; no inventory is reserved anywhere.
        let Json(view) = batch_auction_create(
            State(state.clone()),
            bearer(&state.auth, 2, dx_auth::Role::Issuer),
            Ok(Json(batch_payload(5001, MARKET, PRICE))),
        )
        .await
        .unwrap();
        assert_eq!(view.created_by, 2);
        assert!(view.qty_total.is_none());
        for user_id in [1, 2, 3] {
            let balances = state.writer.balances(user_id).await.unwrap();
            assert!(
                balances
                    .iter()
                    .all(|b| b.asset != dx_types::Asset::Pass { market_id: MARKET } || b.held == 0),
                "scheduling must not move any passes"
            );
        }

        // An ordinary user cannot schedule a window.
        let error = batch_auction_create(
            State(state.clone()),
            bearer(&state.auth, 1, dx_auth::Role::User),
            Ok(Json(batch_payload(5002, MARKET, PRICE))),
        )
        .await
        .err()
        .unwrap();
        assert_eq!(error.status, StatusCode::FORBIDDEN);
    }
}

// ─── Escrow HTTP handlers ─────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct EscrowStateQuery {
    state: Option<String>,
}

#[derive(Debug, Serialize)]
struct Stage6EscrowReceiptView {
    bytes_hex: String,
    signature_hex: String,
    public_key_hex: String,
}

#[derive(Debug, Serialize)]
struct Stage6EscrowView {
    id: u64,
    trade_id: u64,
    market_id: u32,
    buyer_id: UserId,
    seller_id: UserId,
    buyer_username: Option<String>,
    seller_username: Option<String>,
    qty: u32,
    price: i64,
    fee: i64,
    state: String,
    delivered_at: u64,
    deadline: u64,
    receipt: Stage6EscrowReceiptView,
    role: String,
}

fn escrow_to_view(
    e: &dkx_escrow::EscrowRecord,
    role: &str,
    pub_key: &str,
    buyer_username: Option<String>,
    seller_username: Option<String>,
) -> Stage6EscrowView {
    Stage6EscrowView {
        id: e.id,
        trade_id: e.trade_id,
        market_id: e.market_id,
        buyer_id: e.buyer_id,
        seller_id: e.seller_id,
        buyer_username,
        seller_username,
        qty: e.qty,
        price: e.price,
        fee: e.fee,
        state: format!("{:?}", e.state),
        delivered_at: e.delivered_at,
        deadline: e.deadline,
        receipt: Stage6EscrowReceiptView {
            bytes_hex: hex::encode(&e.receipt_bytes),
            signature_hex: hex::encode(&e.receipt_sig),
            public_key_hex: pub_key.to_string(),
        },
        role: role.to_string(),
    }
}

async fn escrow_public_key(State(state): State<AppState>) -> impl IntoResponse {
    let key = state.writer.escrow_public_key().await.unwrap_or_default();
    Json(serde_json::json!({ "public_key_hex": key }))
}

async fn escrow_list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<EscrowStateQuery>,
) -> Result<Json<Vec<Stage6EscrowView>>, ApiError> {
    let user_id = caller_user(&state, &headers)?;
    let escrows = state.writer.all_escrows().await.map_err(writer_error)?;
    let pub_key = state.writer.escrow_public_key().await.unwrap_or_default();
    let filter = q.state.as_deref().unwrap_or("").to_lowercase();
    let views = escrows
        .iter()
        .filter(|e| e.buyer_id == user_id || e.seller_id == user_id)
        .filter(|e| filter.is_empty() || format!("{:?}", e.state).to_lowercase() == filter)
        .map(|e| {
            let role = if e.buyer_id == user_id {
                "buyer"
            } else {
                "seller"
            };
            let buyer_username = state.auth.username_of(e.buyer_id);
            let seller_username = state.auth.username_of(e.seller_id);
            escrow_to_view(e, role, &pub_key, buyer_username, seller_username)
        })
        .collect();
    Ok(Json(views))
}

async fn escrow_get(
    Path(id): Path<u64>,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Stage6EscrowView>, ApiError> {
    let user_id = caller_user(&state, &headers)?;
    let e = state
        .writer
        .get_escrow(id)
        .await
        .map_err(writer_error)?
        .ok_or_else(|| api_error(StatusCode::NOT_FOUND, ReasonCode::OrderNotFound))?;
    let is_party = e.buyer_id == user_id || e.seller_id == user_id;
    if !is_party {
        match identify(&state.auth, &headers)? {
            Caller::Token {
                role: dx_auth::Role::Admin,
                ..
            } => {}
            Caller::Dev(_) => {}
            _ => return Err(forbidden()),
        }
    }
    let pub_key = state.writer.escrow_public_key().await.unwrap_or_default();
    let role = if e.buyer_id == user_id {
        "buyer"
    } else if e.seller_id == user_id {
        "seller"
    } else {
        "admin"
    };
    let buyer_username = state.auth.username_of(e.buyer_id);
    let seller_username = state.auth.username_of(e.seller_id);
    Ok(Json(escrow_to_view(
        &e,
        role,
        &pub_key,
        buyer_username,
        seller_username,
    )))
}

#[derive(Debug, Deserialize)]
struct EscrowRejectBody {
    reason: String,
    note: String,
}

async fn escrow_accept(
    Path(id): Path<u64>,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, ApiError> {
    let user_id = caller_user(&state, &headers)?;
    state
        .writer
        .escrow_accept(user_id, id)
        .await
        .map_err(writer_error)?
        .map_err(|r| api_error(status_for_reason(r), r))?;
    Ok(Json(serde_json::json!({ "status": "Released" })))
}

async fn escrow_reject(
    Path(id): Path<u64>,
    State(state): State<AppState>,
    headers: HeaderMap,
    payload: Result<Json<EscrowRejectBody>, JsonRejection>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let user_id = caller_user(&state, &headers)?;
    let body = json_body(payload)?;
    state
        .writer
        .escrow_reject(user_id, id, body.reason, body.note)
        .await
        .map_err(writer_error)?
        .map_err(|r| api_error(status_for_reason(r), r))?;
    Ok(Json(serde_json::json!({ "status": "Disputed" })))
}

async fn escrow_verify(Path(id): Path<u64>, State(state): State<AppState>) -> impl IntoResponse {
    let valid = state.writer.escrow_verify(id).await.unwrap_or(false);
    Json(serde_json::json!({ "valid": valid }))
}

async fn admin_disputes_list(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<serde_json::Value>>, ApiError> {
    match identify(&state.auth, &headers)? {
        Caller::Token {
            role: dx_auth::Role::Admin,
            ..
        } => {}
        Caller::Dev(_) => {}
        _ => return Err(forbidden()),
    }
    let disputes = state.writer.all_disputes().await.map_err(writer_error)?;
    let views = disputes
        .iter()
        .map(|d| {
            serde_json::json!({
                "id": d.id,
                "escrow_id": d.escrow_id,
                "reason": d.reason,
                "note": d.note,
                "status": d.status,
                "ruling": d.ruling,
                "ruled_by": d.ruled_by,
                "ruled_at": d.ruled_at,
            })
        })
        .collect();
    Ok(Json(views))
}

#[derive(Debug, Deserialize)]
struct AdminRuleBody {
    ruling: String,
    note: String,
}

async fn admin_disputes_rule(
    Path(id): Path<u64>,
    State(state): State<AppState>,
    headers: HeaderMap,
    payload: Result<Json<AdminRuleBody>, JsonRejection>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let admin_id = match identify(&state.auth, &headers)? {
        Caller::Token {
            user_id,
            role: dx_auth::Role::Admin,
        } => user_id,
        Caller::Dev(uid) => uid,
        _ => return Err(forbidden()),
    };
    let body = json_body(payload)?;
    let ruling = match body.ruling.to_lowercase().as_str() {
        "buyer" => dkx_escrow::AdminRuling::Buyer,
        "seller" => dkx_escrow::AdminRuling::Seller,
        _ => {
            return Err(api_error(
                StatusCode::BAD_REQUEST,
                ReasonCode::InvalidRequest,
            ));
        }
    };
    state
        .writer
        .admin_rule(admin_id, id, ruling, body.note)
        .await
        .map_err(writer_error)?
        .map_err(|r| api_error(status_for_reason(r), r))?;
    Ok(Json(serde_json::json!({ "status": "ok" })))
}
