//! HTTP command gateway and MessagePack market-data WebSocket.
//!
//! REST is command-in / response-out. The WebSocket carries snapshots and trades.
//! Identity is simulated for the demo via `x-dev-user` (default user 1).

use std::sync::{Arc, Mutex};

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use dx_core::Core;
use dx_types::{
    Balance, BookSnapshot, ClientOrderId, Command, CommandResponse, Fill, MarketConfig, MarketId,
    Minor, OrderId, OrderStatus, PlaceLimit, PlaceMarket, Qty, ReasonCode, Side, Ticks, UserId,
    WsClientMessage, WsServerMessage, seed_markets,
};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

#[derive(Clone)]
struct AppState {
    core: Arc<Mutex<Core>>,
    tx: broadcast::Sender<WsServerMessage>,
    recent_trades: Arc<Mutex<Vec<Fill>>>,
}

type ApiError = (StatusCode, Json<ErrorBody>);

#[derive(Serialize)]
struct ErrorBody {
    reason: ReasonCode,
}

fn api_error(status: StatusCode, reason: ReasonCode) -> ApiError {
    (status, Json(ErrorBody { reason }))
}

#[derive(Serialize)]
struct MarketView {
    config: MarketConfig,
    snapshot: BookSnapshot,
}

#[derive(Serialize)]
struct OpenOrderView {
    market_id: MarketId,
    order_id: OrderId,
    side: Side,
    price_ticks: Ticks,
    qty: Qty,
    remaining: Qty,
}

#[derive(Serialize)]
struct JournalResponse {
    journal_id: u64,
}

#[derive(Deserialize)]
struct BookQuery {
    depth: Option<usize>,
}

#[derive(Deserialize)]
struct TradeQuery {
    market_id: Option<MarketId>,
    limit: Option<usize>,
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
    };

    let app = Router::new()
        .route("/health", get(health))
        .route("/api/health", get(health))
        .route("/api/markets", get(markets))
        .route("/api/markets/{market_id}/book", get(book))
        .route("/api/markets/{market_id}/trades", get(trades))
        .route("/api/commands", post(command))
        .route("/api/users/{user_id}/balances", get(balances))
        .route("/api/users/{user_id}/orders", get(open_orders))
        .route("/api/dev/deposit", post(dev_deposit))
        .route("/api/dev/issue", post(dev_issue))
        .route("/ws", get(ws))
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

async fn trades(Query(query): Query<TradeQuery>, State(state): State<AppState>) -> Json<Vec<Fill>> {
    let limit = query.limit.unwrap_or(50).clamp(1, 200);
    let trades = state.recent_trades.lock().expect("trade mutex");
    let mut result: Vec<_> = trades
        .iter()
        .rev()
        .filter(|fill| {
            query
                .market_id
                .is_none_or(|market_id| fill.market_id == market_id)
        })
        .take(limit)
        .cloned()
        .collect();
    result.reverse();
    Json(result)
}

async fn command(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(command): Json<Command>,
) -> Result<Json<CommandResponse>, ApiError> {
    let user_id = dev_user(&headers)?;

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

    Ok(Json(response))
}

async fn balances(
    Path(user_id): Path<UserId>,
    State(state): State<AppState>,
) -> Json<Vec<Balance>> {
    let core = state.core.lock().expect("core mutex");
    Json(core.balances(user_id))
}

async fn open_orders(
    Path(user_id): Path<UserId>,
    State(state): State<AppState>,
) -> Json<Vec<OpenOrderView>> {
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
    Json(orders)
}

async fn dev_deposit(
    State(state): State<AppState>,
    Json(body): Json<DevDeposit>,
) -> Result<Json<JournalResponse>, ApiError> {
    let mut core = state.core.lock().expect("core mutex");
    let journal_id = core
        .deposit_cash(body.user_id, body.amount, body.client_request_id)
        .map_err(|reason| api_error(StatusCode::BAD_REQUEST, reason))?;
    Ok(Json(JournalResponse { journal_id }))
}

async fn dev_issue(
    State(state): State<AppState>,
    Json(body): Json<DevIssue>,
) -> Result<Json<JournalResponse>, ApiError> {
    let mut core = state.core.lock().expect("core mutex");
    let journal_id = core
        .issue_passes(
            body.user_id,
            body.market_id,
            body.qty,
            body.client_request_id,
        )
        .map_err(|reason| api_error(StatusCode::BAD_REQUEST, reason))?;
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
    ws.on_upgrade(move |socket| handle_socket(socket, state, query.user_id))
}

async fn handle_socket(mut socket: WebSocket, state: AppState, user_id: Option<UserId>) {
    let mut rx = state.tx.subscribe();

    let markets = {
        let core = state.core.lock().expect("core mutex");
        core.engine()
            .market_ids()
            .into_iter()
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
            .filter_map(|market| core.snapshot(market.id, 20))
            .collect::<Vec<_>>();
        drop(core);
        let trades = state.recent_trades.lock().expect("trade mutex");
        let recent = trades
            .iter()
            .rev()
            .take(20)
            .rev()
            .cloned()
            .collect::<Vec<_>>();
        (snapshots, recent)
    };

    for fill in recent_trades {
        if !send_msg(&mut socket, &WsServerMessage::Trade { fill }).await {
            return;
        }
    }
    for snapshot in snapshots {
        if !send_msg(&mut socket, &WsServerMessage::Snapshot { snapshot }).await {
            return;
        }
    }

    let mut market_filter: Option<Vec<MarketId>> = None;
    let mut depth = 20usize;

    loop {
        tokio::select! {
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(Message::Binary(bytes))) => {
                        match rmp_serde::from_slice::<WsClientMessage>(&bytes) {
                            Ok(WsClientMessage::Subscribe { market_ids, depth: requested_depth }) => {
                                market_filter = if market_ids.is_empty() { None } else { Some(market_ids) };
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
        WsServerMessage::Trade { fill } => {
            if market_filter.is_some_and(|ids| !ids.contains(&fill.market_id)) {
                return None;
            }
            Some(WsServerMessage::Trade { fill })
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
