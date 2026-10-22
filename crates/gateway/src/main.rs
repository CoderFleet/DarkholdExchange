//! HTTP command gateway and MessagePack market-data WebSocket.
//!
//! REST is command-in / response-out. The WebSocket carries snapshots and trades.
//! Identity is simulated for the demo via `x-dev-user` (default user 1).

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::rejection::JsonRejection;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use dx_core::Core;
use dx_types::{
    Balance, BookSnapshot, Cancel, ClientOrderId, Command, CommandResponse, ErrorBody, Fill,
    JournalResponse, MarketId, MarketView, MetricsSnapshot, Minor, OpenOrderView, OrderId,
    OrderStatus, PlaceLimit, PlaceMarket, Qty, ReasonCode, Side, UserId, WsClientMessage,
    WsServerMessage, seed_markets,
};
use serde::Deserialize;
use tokio::sync::broadcast;

/// Gateway-owned counters. The queue and histogram metrics are filled in when
/// the single-writer sequencer lands; keeping the fields here lets the REST and
/// WS contract stay stable in the meantime.
#[derive(Default)]
struct MetricsState {
    commands_total: AtomicU64,
    rejected_total: AtomicU64,
    ws_clients: AtomicU32,
    slow_consumers: AtomicU64,
}

#[derive(Clone)]
struct AppState {
    core: Arc<Mutex<Core>>,
    tx: broadcast::Sender<WsServerMessage>,
    recent_trades: Arc<Mutex<Vec<Fill>>>,
    metrics: Arc<MetricsState>,
}

type ApiError = (StatusCode, Json<ErrorBody>);

fn api_error(status: StatusCode, reason: ReasonCode) -> ApiError {
    (status, Json(ErrorBody { reason }))
}

/// Transport mapping for a sequenced business rejection. The core still returns
/// the original `CommandResponse`; the gateway translates it once at the edge.
fn status_for_reason(reason: ReasonCode) -> StatusCode {
    use ReasonCode::*;
    match reason {
        OutsideCollar
        | InsufficientFunds
        | InsufficientInventory
        | HoldingLimitExceeded
        | SelfTrade => StatusCode::UNPROCESSABLE_ENTITY,
        UnknownMarket | OrderNotFound => StatusCode::NOT_FOUND,
        NotOrderOwner => StatusCode::FORBIDDEN,
        DuplicateClientOrderId
        | MarketClosed
        | CancelNotAllowed
        | AuctionNotActive
        | AuctionClosed
        | StaleAuctionPrice
        | EscrowNotReady => StatusCode::CONFLICT,
        RateLimited | QueueFull => StatusCode::TOO_MANY_REQUESTS,
        InternalError => StatusCode::INTERNAL_SERVER_ERROR,
        InvalidQuantity | InvalidPrice | PriceNotTickAligned | InvalidRequest | InvalidReceipt
        | ReceiptExpired => StatusCode::BAD_REQUEST,
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

#[tokio::main]
async fn main() {
    let addr = std::env::var("GATEWAY_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".into());

    let mut core = Core::new(seed_markets()).expect("valid seed markets");
    let seed_fills = seed_demo(&mut core);

    let (tx, _) = broadcast::channel(512);
    let state = AppState {
        core: Arc::new(Mutex::new(core)),
        tx,
        recent_trades: Arc::new(Mutex::new(seed_fills)),
        metrics: Arc::new(MetricsState::default()),
    };

    let app = Router::new()
        .route("/health", get(health))
        .route("/api/health", get(health))
        .route("/api/markets", get(markets))
        .route("/api/markets/{market_id}/book", get(book))
        .route("/api/markets/{market_id}/trades", get(trades))
        .route("/api/commands", post(command))
        .route("/api/orders", post(place_order))
        .route("/api/orders/open", get(open_orders_current))
        .route("/api/orders/{order_id}", delete(cancel_order))
        .route("/api/metrics/latency", get(latency_metrics))
        .route("/api/users/{user_id}/balances", get(balances))
        .route("/api/users/{user_id}/orders", get(open_orders))
        .route("/api/dev/deposit", post(dev_deposit))
        .route("/api/dev/issue", post(dev_issue))
        .route("/ws", get(ws))
        .route("/ws/market/{market_id}", get(ws_market))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .expect("bind gateway");
    println!("Darkhold Exchange gateway listening on {addr}");
    axum::serve(listener, app).await.expect("gateway server");
}

async fn health() -> &'static str {
    "ok"
}

async fn markets(State(state): State<AppState>) -> Json<Vec<MarketView>> {
    let core = state.core.lock().expect("core mutex");
    let views = core
        .engine()
        .market_ids()
        .into_iter()
        .filter_map(|market_id| {
            let config = core.config(market_id)?.clone();
            let snapshot = core.snapshot(market_id, 20)?;
            Some(MarketView { config, snapshot })
        })
        .collect();
    Json(views)
}

async fn book(
    Path(market_id): Path<MarketId>,
    Query(query): Query<BookQuery>,
    State(state): State<AppState>,
) -> Result<Json<BookSnapshot>, ApiError> {
    let depth = query.depth.unwrap_or(20).clamp(1, 200);
    let core = state.core.lock().expect("core mutex");
    match core.snapshot(market_id, depth) {
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
    let known_market = {
        let core = state.core.lock().expect("core mutex");
        core.config(market_id).is_some()
    };
    if !known_market {
        return Err(api_error(StatusCode::NOT_FOUND, ReasonCode::UnknownMarket));
    }

    let trades = state.recent_trades.lock().expect("trade mutex");
    let mut result: Vec<_> = trades
        .iter()
        .rev()
        .filter(|fill| fill.market_id == market_id)
        .take(limit)
        .cloned()
        .collect();
    result.reverse();
    Ok(Json(result))
}

fn execute_command(state: &AppState, user_id: UserId, command: Command) -> CommandResponse {
    let (response, snapshots, fills) = {
        let mut core = state.core.lock().expect("core mutex");
        let response = core.process(user_id, command);
        let changed = response.status != OrderStatus::Rejected;
        let snapshots = if changed {
            core.engine()
                .market_ids()
                .into_iter()
                .filter_map(|market_id| core.snapshot(market_id, 50))
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let fills = response.fills.clone();
        (response, snapshots, fills)
    };

    state.metrics.commands_total.fetch_add(1, Ordering::Relaxed);
    if response.status == OrderStatus::Rejected {
        state.metrics.rejected_total.fetch_add(1, Ordering::Relaxed);
    }

    if !fills.is_empty() {
        let mut recent = state.recent_trades.lock().expect("trade mutex");
        recent.extend(fills.iter().cloned());
        if recent.len() > 200 {
            let excess = recent.len() - 200;
            recent.drain(0..excess);
        }
    }

    for snapshot in snapshots {
        let _ = state.tx.send(WsServerMessage::Snapshot { snapshot });
    }
    for fill in fills {
        let _ = state.tx.send(WsServerMessage::Trade { fill });
    }

    response
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
    let user_id = dev_user(&headers)?;
    let command = json_body(payload)?;
    finish_command(execute_command(&state, user_id, command))
}

async fn place_order(
    State(state): State<AppState>,
    headers: HeaderMap,
    payload: Result<Json<Command>, JsonRejection>,
) -> Result<Json<CommandResponse>, ApiError> {
    let user_id = dev_user(&headers)?;
    let command = json_body(payload)?;
    if matches!(command, Command::Cancel(_)) {
        return Err(api_error(
            StatusCode::METHOD_NOT_ALLOWED,
            ReasonCode::CancelNotAllowed,
        ));
    }
    finish_command(execute_command(&state, user_id, command))
}

async fn cancel_order(
    Path(order_id): Path<OrderId>,
    Query(query): Query<CancelQuery>,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<CommandResponse>, ApiError> {
    let user_id = dev_user(&headers)?;
    let client_order_id = query
        .client_order_id
        .ok_or_else(|| api_error(StatusCode::BAD_REQUEST, ReasonCode::InvalidReceipt))?;
    let command = Command::Cancel(Cancel {
        client_order_id,
        order_id,
    });
    finish_command(execute_command(&state, user_id, command))
}

async fn balances(
    Path(user_id): Path<UserId>,
    State(state): State<AppState>,
) -> Json<Vec<Balance>> {
    let core = state.core.lock().expect("core mutex");
    Json(core.balances(user_id))
}

fn open_orders_for(state: &AppState, user_id: UserId) -> Vec<OpenOrderView> {
    let core = state.core.lock().expect("core mutex");
    let mut orders = Vec::new();
    for market_id in core.engine().market_ids() {
        let Some(book) = core.engine().book(market_id) else {
            continue;
        };
        for order in book.orders() {
            if order.user_id == user_id {
                orders.push(OpenOrderView {
                    market_id,
                    order_id: order.id,
                    side: order.side,
                    price_ticks: order.price_ticks,
                    qty: order.qty,
                    remaining: order.remaining,
                });
            }
        }
    }
    orders
}

async fn open_orders_current(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<OpenOrderView>>, ApiError> {
    let user_id = dev_user(&headers)?;
    Ok(Json(open_orders_for(&state, user_id)))
}

async fn open_orders(
    Path(user_id): Path<UserId>,
    State(state): State<AppState>,
) -> Json<Vec<OpenOrderView>> {
    Json(open_orders_for(&state, user_id))
}

fn metrics_snapshot(state: &AppState) -> MetricsSnapshot {
    let seq = state.core.lock().expect("core mutex").last_seq();
    MetricsSnapshot {
        seq,
        queue_depth: 0,
        commands_total: state.metrics.commands_total.load(Ordering::Relaxed),
        rejected_total: state.metrics.rejected_total.load(Ordering::Relaxed),
        queue_wait_p99_us: 0,
        core_p99_us: 0,
        ws_clients: state.metrics.ws_clients.load(Ordering::Relaxed),
        slow_consumers: state.metrics.slow_consumers.load(Ordering::Relaxed),
    }
}

async fn latency_metrics(State(state): State<AppState>) -> Json<MetricsSnapshot> {
    Json(metrics_snapshot(&state))
}

async fn dev_deposit(
    State(state): State<AppState>,
    payload: Result<Json<DevDeposit>, JsonRejection>,
) -> Result<Json<JournalResponse>, ApiError> {
    let body = json_body(payload)?;
    let mut core = state.core.lock().expect("core mutex");
    let journal_id = core
        .deposit_cash(body.user_id, body.amount, body.client_request_id)
        .map_err(|reason| api_error(status_for_reason(reason), reason))?;
    Ok(Json(JournalResponse { journal_id }))
}

async fn dev_issue(
    State(state): State<AppState>,
    payload: Result<Json<DevIssue>, JsonRejection>,
) -> Result<Json<JournalResponse>, ApiError> {
    let body = json_body(payload)?;
    let mut core = state.core.lock().expect("core mutex");
    let journal_id = core
        .issue_passes(
            body.user_id,
            body.market_id,
            body.qty,
            body.client_request_id,
        )
        .map_err(|reason| api_error(status_for_reason(reason), reason))?;
    Ok(Json(JournalResponse { journal_id }))
}

fn dev_user(headers: &HeaderMap) -> Result<UserId, ApiError> {
    match headers.get("x-dev-user") {
        None => Ok(1),
        Some(value) => value
            .to_str()
            .ok()
            .and_then(|value| value.parse::<UserId>().ok())
            .ok_or_else(|| api_error(StatusCode::BAD_REQUEST, ReasonCode::InvalidReceipt)),
    }
}

async fn ws(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Query(query): Query<WsQuery>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state, query.user_id, None))
}

async fn ws_market(
    Path(market_id): Path<MarketId>,
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Query(query): Query<WsQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let known_market = {
        let core = state.core.lock().expect("core mutex");
        core.config(market_id).is_some()
    };
    if !known_market {
        return Err(api_error(StatusCode::NOT_FOUND, ReasonCode::UnknownMarket));
    }
    Ok(ws.on_upgrade(move |socket| handle_socket(socket, state, query.user_id, Some(market_id))))
}

struct WsClientGuard {
    metrics: Arc<MetricsState>,
}

impl Drop for WsClientGuard {
    fn drop(&mut self) {
        self.metrics.ws_clients.fetch_sub(1, Ordering::Relaxed);
    }
}

async fn handle_socket(
    mut socket: WebSocket,
    state: AppState,
    user_id: Option<UserId>,
    fixed_market: Option<MarketId>,
) {
    let mut rx = state.tx.subscribe();
    state.metrics.ws_clients.fetch_add(1, Ordering::Relaxed);
    let _client_guard = WsClientGuard {
        metrics: state.metrics.clone(),
    };

    let mut market_filter = fixed_market.map(|market_id| vec![market_id]);
    let mut depth = 20usize;
    let markets = {
        let core = state.core.lock().expect("core mutex");
        core.engine()
            .market_ids()
            .into_iter()
            .filter(|market_id| fixed_market.is_none_or(|fixed| *market_id == fixed))
            .filter_map(|market_id| core.config(market_id).cloned())
            .collect::<Vec<_>>()
    };

    if !send_msg(
        &mut socket,
        &WsServerMessage::Hello {
            markets: markets.clone(),
            user_id,
        },
    )
    .await
    {
        return;
    }

    let (snapshots, recent_trades) = {
        let core = state.core.lock().expect("core mutex");
        let snapshots = markets
            .iter()
            .filter_map(|market| core.snapshot(market.id, depth))
            .collect::<Vec<_>>();
        drop(core);
        let trades = state.recent_trades.lock().expect("trade mutex");
        let mut recent = trades
            .iter()
            .rev()
            .filter(|fill| fixed_market.is_none_or(|market_id| fill.market_id == market_id))
            .take(20)
            .cloned()
            .collect::<Vec<_>>();
        recent.reverse();
        (snapshots, recent)
    };

    for snapshot in snapshots {
        if !send_msg(&mut socket, &WsServerMessage::Snapshot { snapshot }).await {
            return;
        }
    }
    for fill in recent_trades {
        if !send_msg(&mut socket, &WsServerMessage::Trade { fill }).await {
            return;
        }
    }
    if !send_msg(
        &mut socket,
        &WsServerMessage::Metrics {
            metrics: metrics_snapshot(&state),
        },
    )
    .await
    {
        return;
    }

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
                                let snapshots = {
                                    let core = state.core.lock().expect("core mutex");
                                    let ids = market_filter
                                        .clone()
                                        .unwrap_or_else(|| core.engine().market_ids());
                                    ids.into_iter()
                                        .filter_map(|market_id| core.snapshot(market_id, depth))
                                        .collect::<Vec<_>>()
                                };
                                for snapshot in snapshots {
                                    if !send_msg(&mut socket, &WsServerMessage::Snapshot { snapshot }).await {
                                        return;
                                    }
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
                        let Some(message) = filter_message(message, market_filter.as_deref(), depth) else {
                            continue;
                        };
                        if !send_msg(&mut socket, &message).await {
                            return;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        eprintln!("ws client lagged by {skipped} messages; resyncing");
                        state.metrics.slow_consumers.fetch_add(1, Ordering::Relaxed);
                        let ids = {
                            let core = state.core.lock().expect("core mutex");
                            market_filter
                                .clone()
                                .unwrap_or_else(|| core.engine().market_ids())
                        };
                        for market_id in &ids {
                            if !send_msg(
                                &mut socket,
                                &WsServerMessage::ResyncRequired { market_id: *market_id },
                            )
                            .await
                            {
                                return;
                            }
                        }
                        let snapshots = {
                            let core = state.core.lock().expect("core mutex");
                            ids.into_iter()
                                .filter_map(|market_id| core.snapshot(market_id, depth))
                                .collect::<Vec<_>>()
                        };
                        for snapshot in snapshots {
                            if !send_msg(&mut socket, &WsServerMessage::Snapshot { snapshot }).await {
                                return;
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            }
        }
    }
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
        (1, 3, Side::Buy, 4_800, 2),
        (3, 3, Side::Buy, 4_700, 2),
        (2, 3, Side::Sell, 5_200, 2),
        (4, 3, Side::Sell, 5_300, 2),
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
    use dx_types::{BookDelta, LevelChange};

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
}
