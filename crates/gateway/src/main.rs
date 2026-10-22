//! HTTP command gateway and MessagePack market-data WebSocket.
//!
//! REST is command-in / response-out. The WebSocket streams ordered deltas and
//! trades; snapshots are only baselines and lag-recovery checkpoints. Identity
//! is simulated for the demo via `x-dev-user` (default user 1).

mod writer;

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
use dx_core::Core;
use dx_types::{
    Balance, BookSnapshot, Cancel, ClientOrderId, Command, CommandResponse, ErrorBody, Fill,
    JournalResponse, MarketId, MarketView, MetricsSnapshot, Minor, OpenOrderView, OrderId,
    OrderStatus, PlaceLimit, PlaceMarket, Qty, ReasonCode, Side, UserId, WsClientMessage,
    WsServerMessage, seed_markets,
};
use serde::Deserialize;
use tokio::sync::broadcast;
use writer::{MetricsState, StreamBaseline, WriterError, WriterHandle, spawn_writer};

#[derive(Clone)]
struct AppState {
    writer: WriterHandle,
    metrics: Arc<MetricsState>,
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    body: Json<ErrorBody>,
    retry_after: bool,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        let mut response = (self.status, self.body).into_response();
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
    let (writer, metrics) = spawn_writer(core, tx, seed_fills, queue_capacity());
    let state = AppState { writer, metrics };

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
    let user_id = dev_user(&headers)?;
    let command = json_body(payload)?;
    finish_command(execute_command(&state, user_id, command).await?)
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
    finish_command(execute_command(&state, user_id, command).await?)
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
    finish_command(execute_command(&state, user_id, command).await?)
}

async fn balances(
    Path(user_id): Path<UserId>,
    State(state): State<AppState>,
) -> Result<Json<Vec<Balance>>, ApiError> {
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
    let user_id = dev_user(&headers)?;
    Ok(Json(open_orders_for(&state, user_id).await?))
}

async fn open_orders(
    Path(user_id): Path<UserId>,
    State(state): State<AppState>,
) -> Result<Json<Vec<OpenOrderView>>, ApiError> {
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

async fn dev_deposit(
    State(state): State<AppState>,
    payload: Result<Json<DevDeposit>, JsonRejection>,
) -> Result<Json<JournalResponse>, ApiError> {
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
    let known_market = state
        .writer
        .market_exists(market_id)
        .await
        .map_err(writer_error)?;
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

    if !send_msg(
        &mut socket,
        &WsServerMessage::Hello {
            markets: baseline.markets,
            user_id,
        },
    )
    .await
    {
        return;
    }

    for snapshot in baseline.snapshots {
        if !send_msg(&mut socket, &WsServerMessage::Snapshot { snapshot }).await {
            return;
        }
    }
    for fill in baseline.recent_trades {
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
                                let Ok(baseline) =
                                    stream_baseline(&state, market_filter.as_deref(), depth).await
                                else {
                                    return;
                                };
                                rx = baseline.rx;
                                for snapshot in baseline.snapshots {
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
                        let Ok(baseline) =
                            stream_baseline(&state, market_filter.as_deref(), depth).await
                        else {
                            return;
                        };
                        rx = baseline.rx;
                        for market in &baseline.markets {
                            if !send_msg(
                                &mut socket,
                                &WsServerMessage::ResyncRequired { market_id: market.id },
                            )
                            .await
                            {
                                return;
                            }
                        }
                        for snapshot in baseline.snapshots {
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
    use dx_types::{BookDelta, BookLevel, LevelChange, Ticks};

    const MARKET: MarketId = 1;
    const PRICE: Ticks = 5_000;

    fn test_state() -> (AppState, broadcast::Receiver<WsServerMessage>) {
        let mut core = Core::new(seed_markets()).unwrap();
        core.deposit_cash(1, 10_000_000, 1).unwrap();
        core.deposit_cash(2, 10_000_000, 2).unwrap();
        core.issue_passes(2, MARKET, 5, 1).unwrap();

        let (tx, rx) = broadcast::channel(64);
        let (writer, metrics) = spawn_writer(core, tx, Vec::new(), 64);
        (AppState { writer, metrics }, rx)
    }

    async fn run_command(state: &AppState, user_id: UserId, command: Command) -> CommandResponse {
        execute_command(state, user_id, command).await.unwrap()
    }

    fn limit(client_order_id: ClientOrderId, side: Side, price_ticks: Ticks, qty: Qty) -> Command {
        Command::PlaceLimit(PlaceLimit {
            client_order_id,
            market_id: MARKET,
            side,
            price_ticks,
            qty,
        })
    }

    fn market_order(client_order_id: ClientOrderId, side: Side, qty: Qty) -> Command {
        Command::PlaceMarket(PlaceMarket {
            client_order_id,
            market_id: MARKET,
            side,
            qty,
            protect_price_ticks: PRICE,
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
        assert_eq!(Some(snapshot), expected);
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
}
