//! Repeatable gateway load baseline.
//!
//! The main load rests a one-unit buy below the best bid and immediately
//! cancels it. That keeps users inside holding limits while producing one
//! placement plus one book-changing cancel per cycle. A separate low-rate probe
//! on another market measures command-to-WebSocket visibility without
//! cross-talk from the main stream.

use std::error::Error;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use dx_types::{
    ClientOrderId, Command, CommandResponse, JournalResponse, MarketStatus, MarketView,
    MetricsSnapshot, OrderId, OrderStatus, PlaceLimit, Side, UserId, WsServerMessage,
};
use futures_util::{StreamExt, stream};
use reqwest::{Client, StatusCode};
use serde_json::json;
use tokio::sync::{Mutex, mpsc, watch};
use tokio::task::{JoinHandle, JoinSet};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

use crate::config::{LoadConfig, Workload};
use crate::report::{
    AchievedLoad, ClientReport, EnvironmentReport, FillReport, GatewayReport, LatencySummary,
    LoadReport, OfferedLoad, OutcomeCounts, ProbeReport, SetupReport, WsConnectionReport, WsReport,
};

type LoadResult<T> = Result<T, Box<dyn Error + Send + Sync>>;
type WsStream = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

const SEED_CLIENT_BASE: u64 = 9_000_000_000;
const PLACE_CLIENT_BASE: u64 = 10_000_000_000;
const CANCEL_CLIENT_BASE: u64 = 20_000_000_000;
const PROBE_PLACE_BASE: u64 = 30_000_000_000;
const PROBE_CANCEL_BASE: u64 = 40_000_000_000;
const MAKER_CLIENT_BASE: u64 = 50_000_000_000;
const CANCEL_RETRY_LIMIT: usize = 100;
const SEED_CONCURRENCY: usize = 32;
const MAKER_SETUP_CONCURRENCY: usize = 32;
const TAKER_RETRY_LIMIT: usize = 3;

pub async fn run(config: LoadConfig) -> LoadResult<LoadReport> {
    let http = Client::builder()
        .pool_max_idle_per_host(64)
        .tcp_nodelay(true)
        .timeout(Duration::from_secs(10))
        .build()?;

    let setup = match config.workload {
        Workload::PlaceCancel => {
            seed_place_cancel_users(&http, &config).await?;
            None
        }
        Workload::Fills => {
            validate_fill_market(&http, &config).await?;
            seed_fill_users(&http, &config).await?;
            Some(place_maker_liquidity(&http, &config).await?)
        }
    };

    let (ws_stop_tx, _) = watch::channel(false);
    let mut ws_handles: Vec<JoinHandle<LoadResult<WsStats>>> = Vec::new();
    for _ in 0..config.ws_connections {
        let (stream, last_seq) = connect_baseline(&config, config.market_id).await?;
        let stop = ws_stop_tx.subscribe();
        ws_handles.push(tokio::spawn(read_ws(stream, last_seq, stop)));
    }

    let (probe_delta_tx, probe_delta_rx) = mpsc::channel(256);
    let mut probe_ws_handle = None;
    let mut probe_seq = None;
    if config.probe_rate_per_second > 0 {
        let (stream, last_seq) = connect_baseline(&config, config.probe_market_id).await?;
        probe_seq = Some(last_seq);
        let stop = ws_stop_tx.subscribe();
        probe_ws_handle = Some(tokio::spawn(read_probe_ws(
            stream,
            last_seq,
            probe_delta_tx,
            stop,
        )));
    } else {
        drop(probe_delta_tx);
    }

    let before = fetch_metrics(&http, &config).await?;
    let (load_stop_tx, load_stop_rx) = watch::channel(false);
    let run_started = Instant::now();
    let started_at_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| load_error(error.to_string()))?
        .as_millis() as u64;

    let (tick_rx, pacer_handle) = spawn_pacer(&config, load_stop_rx.clone());
    let tick_rx = Arc::new(Mutex::new(tick_rx));
    let place_ids = Arc::new(AtomicU64::new(PLACE_CLIENT_BASE));
    let cancel_ids = Arc::new(AtomicU64::new(CANCEL_CLIENT_BASE));

    let fill_work =
        (config.workload == Workload::Fills).then(|| Arc::new(fill_taker_work(&config)));
    let fill_next = Arc::new(AtomicUsize::new(0));

    let mut user_tasks = JoinSet::new();
    for index in 0..config.users {
        match config.workload {
            Workload::PlaceCancel => {
                let user_id = config.user_base + index as u64;
                user_tasks.spawn(run_user_task(
                    config.clone(),
                    http.clone(),
                    user_id,
                    tick_rx.clone(),
                    place_ids.clone(),
                    cancel_ids.clone(),
                    load_stop_rx.clone(),
                ));
            }
            Workload::Fills => {
                user_tasks.spawn(run_fill_task(
                    config.clone(),
                    http.clone(),
                    fill_work.clone().expect("fill work list"),
                    fill_next.clone(),
                    tick_rx.clone(),
                    place_ids.clone(),
                    load_stop_rx.clone(),
                ));
            }
        }
    }

    let probe_handle = probe_seq.map(|initial_seq| {
        tokio::spawn(run_probe(
            config.clone(),
            http.clone(),
            initial_seq,
            probe_delta_rx,
            load_stop_rx.clone(),
        ))
    });

    tokio::time::sleep(Duration::from_secs(config.duration_secs)).await;
    let measured_duration = run_started.elapsed();
    let _ = load_stop_tx.send(true);

    let mut client_stats = UserTaskStats::default();
    let mut first_error = None;
    while let Some(result) = user_tasks.join_next().await {
        match result {
            Ok(Ok(stats)) => client_stats.merge(stats),
            Ok(Err(error)) => {
                let _ = load_stop_tx.send(true);
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
            Err(error) => {
                let _ = load_stop_tx.send(true);
                if first_error.is_none() {
                    first_error = Some(load_error(format!("load task failed: {error}")));
                }
            }
        }
    }

    if let Err(error) = pacer_handle.await
        && first_error.is_none()
    {
        first_error = Some(load_error(format!("pacer failed: {error}")));
    }

    let probe_stats = match probe_handle {
        Some(handle) => match handle.await {
            Ok(Ok(stats)) => stats,
            Ok(Err(error)) => {
                if first_error.is_none() {
                    first_error = Some(error);
                }
                ProbeTaskStats::default()
            }
            Err(error) => {
                if first_error.is_none() {
                    first_error = Some(load_error(format!("probe task failed: {error}")));
                }
                ProbeTaskStats::default()
            }
        },
        None => ProbeTaskStats::default(),
    };

    let after = fetch_metrics(&http, &config).await?;

    // Let the sockets drain the tail of the stream before closing them.
    tokio::time::sleep(Duration::from_millis(250)).await;
    let _ = ws_stop_tx.send(true);

    let mut ws_reports = Vec::new();
    for handle in ws_handles {
        match handle.await {
            Ok(Ok(stats)) => ws_reports.push(stats.into_report()),
            Ok(Err(error)) => {
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
            Err(error) => {
                if first_error.is_none() {
                    first_error = Some(load_error(format!("ws task failed: {error}")));
                }
            }
        }
    }

    let probe_ws_stats = match probe_ws_handle {
        Some(handle) => match handle.await {
            Ok(Ok(stats)) => stats,
            Ok(Err(error)) => {
                if first_error.is_none() {
                    first_error = Some(error);
                }
                WsStats::default()
            }
            Err(error) => {
                if first_error.is_none() {
                    first_error = Some(load_error(format!("probe ws task failed: {error}")));
                }
                WsStats::default()
            }
        },
        None => WsStats::default(),
    };

    if let Some(error) = first_error {
        return Err(error);
    }

    let duration_ms = measured_duration.as_millis() as u64;
    let seconds = (duration_ms as f64 / 1_000.0).max(0.001);
    let gateway = GatewayReport { before, after };
    let offered = OfferedLoad {
        placements: client_stats.place.attempts,
        commands: client_stats.place.attempts + client_stats.cancel.attempts,
    };
    let achieved = AchievedLoad {
        placements_per_second: client_stats.place.ok as f64 / seconds,
        commands_per_second: gateway.commands_delta() as f64 / seconds,
    };
    let client = ClientReport {
        place: client_stats.place,
        cancel: client_stats.cancel,
        place_ack: LatencySummary::from_samples(client_stats.place_latencies_us),
        cancel_ack: LatencySummary::from_samples(client_stats.cancel_latencies_us),
        fills: client_stats.fills,
    };
    let ws = WsReport::from_connections(ws_reports);
    let fill = (config.workload == Workload::Fills).then(|| FillReport {
        target_per_second: config.fills_per_second,
        expected_fills: config.fills_per_second * config.duration_secs,
        fills: client_stats.fills,
        fills_per_second: client_stats.fills as f64 / seconds,
        ws_trades_per_connection: ws.trades_per_connection,
        makers: config.fill_makers,
        takers: config.fill_takers,
        passes_per_user: config.fill_passes,
    });
    let probe = ProbeReport {
        enabled: config.probe_rate_per_second > 0,
        place_attempts: probe_stats.place_attempts,
        place_ok: probe_stats.place_ok,
        cancel_attempts: probe_stats.cancel_attempts,
        cancel_ok: probe_stats.cancel_ok,
        overloaded: probe_stats.overloaded,
        place_e2e: LatencySummary::from_samples(probe_stats.place_e2e_us),
        cancel_e2e: LatencySummary::from_samples(probe_stats.cancel_e2e_us),
        ws_messages: probe_ws_stats.messages,
        ws_deltas: probe_ws_stats.deltas,
        ws_resync_required: probe_ws_stats.resync_required,
    };

    Ok(LoadReport {
        config: config.clone(),
        environment: EnvironmentReport {
            os: std::env::consts::OS,
            arch: std::env::consts::ARCH,
            available_parallelism: std::thread::available_parallelism()
                .map(|value| value.get())
                .unwrap_or(1),
            gateway_queue_capacity: std::env::var("DX_COMMAND_QUEUE_CAPACITY")
                .ok()
                .and_then(|value| value.parse().ok()),
        },
        started_at_unix_ms,
        duration_ms,
        offered,
        achieved,
        client,
        probe,
        ws,
        gateway,
        setup,
        fill,
        notes: fill_notes(&config),
    })
}

fn fill_notes(config: &LoadConfig) -> Vec<String> {
    let mut notes = match config.workload {
        Workload::PlaceCancel => vec![
            "Each user slot rests a one-unit buy below the best bid and immediately cancels it; cancels are not counted as placements.".into(),
        ],
        Workload::Fills => vec![
            "Maker asks are seeded before the measured window; offered taker commands and fill rates exclude maker setup.".into(),
            "Each taker buy crosses pre-rested one-unit maker asks, so one accepted taker command produces one fill.".into(),
            "Fills are counted from CommandResponse.fills and cross-checked against WebSocket Trade frames.".into(),
            "The pacer stops at the duration boundary, so acknowledged fills can be a few ticks above or below the nominal target.".into(),
        ],
    };
    notes.push(
        "The end-to-end probe uses a separate market and fixed-market WebSocket, so probe deltas are unambiguous."
            .into(),
    );
    notes.push(
        "Gateway command counters include the low-rate probe and are cumulative since gateway start; scripts/load-test.sh starts a fresh gateway."
            .into(),
    );
    notes.push("Loopback HTTP/WS with no TLS, proxy, or injected network delay.".into());
    notes
}

async fn seed_place_cancel_users(http: &Client, config: &LoadConfig) -> LoadResult<()> {
    for index in 0..config.users {
        let user_id = config.user_base + index as u64;
        let deposit_id = SEED_CLIENT_BASE + (index as u64) * 2;
        let issue_id = deposit_id + 1;

        let response = http
            .post(format!("{}/api/dev/deposit", config.base_url))
            .json(&json!({
                "user_id": user_id,
                "amount": config.seed_cash,
                "client_request_id": deposit_id,
            }))
            .send()
            .await?;
        expect_success(response, "dev deposit").await?;

        let response = http
            .post(format!("{}/api/dev/issue", config.base_url))
            .json(&json!({
                "user_id": user_id,
                "market_id": config.market_id,
                "qty": config.seed_passes,
                "client_request_id": issue_id,
            }))
            .send()
            .await?;
        expect_success(response, "dev issue").await?;
    }
    Ok(())
}

async fn expect_success(response: reqwest::Response, action: &str) -> LoadResult<()> {
    let status = response.status();
    if status.is_success() {
        let _: JournalResponse = response.json().await?;
        return Ok(());
    }
    let body = response.text().await.unwrap_or_default();
    Err(load_error(format!("{action} failed with {status}: {body}")))
}

async fn validate_fill_market(http: &Client, config: &LoadConfig) -> LoadResult<()> {
    let response = http
        .get(format!("{}/api/markets", config.base_url))
        .send()
        .await?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(load_error(format!(
            "market request failed with {status}: {body}"
        )));
    }
    let markets: Vec<MarketView> = response.json().await?;
    let view = markets
        .into_iter()
        .find(|view| view.config.id == config.market_id)
        .ok_or_else(|| load_error(format!("market {} not found", config.market_id)))?;
    let market = &view.config;

    if market.status != MarketStatus::Open {
        return Err(load_error(format!("market {} is not open", market.id)));
    }
    if config.fill_passes > market.holding_limit as u64 {
        return Err(load_error(format!(
            "--fill-passes {} exceeds market {} holding limit {}",
            config.fill_passes, market.id, market.holding_limit
        )));
    }
    if !market.price_in_collar(config.fill_price_ticks) {
        return Err(load_error(format!(
            "--fill-price-ticks {} is outside market {} collar {}..={}",
            config.fill_price_ticks,
            market.id,
            market.floor_ticks(),
            market.cap_ticks()
        )));
    }
    if let Some(best_bid) = view.snapshot.bids.first()
        && config.fill_price_ticks <= best_bid.price_ticks
    {
        return Err(load_error(format!(
            "--fill-price-ticks {} must be above best bid {} so maker asks rest",
            config.fill_price_ticks, best_bid.price_ticks
        )));
    }
    if let Some(best_ask) = view.snapshot.asks.first()
        && config.fill_price_ticks >= best_ask.price_ticks
    {
        return Err(load_error(format!(
            "--fill-price-ticks {} must be below best ask {} so maker asks are the first fill",
            config.fill_price_ticks, best_ask.price_ticks
        )));
    }

    let notional = market
        .notional_minor(config.fill_price_ticks, config.qty)
        .ok_or_else(|| load_error("fill notional overflow"))?;
    let fee = market.taker_fee_minor(notional);
    let per_buy = notional
        .checked_add(fee)
        .ok_or_else(|| load_error("fill cash requirement overflow"))?;
    let passes = i64::try_from(config.fill_passes)
        .map_err(|_| load_error("--fill-passes does not fit i64"))?;
    let required_cash = per_buy
        .checked_mul(passes)
        .ok_or_else(|| load_error("fill cash requirement overflow"))?;
    if config.seed_cash < required_cash {
        return Err(load_error(format!(
            "--seed-cash {} is below the {} needed for {} taker buys",
            config.seed_cash, required_cash, config.fill_passes
        )));
    }
    Ok(())
}

async fn seed_fill_users(http: &Client, config: &LoadConfig) -> LoadResult<()> {
    let mut users = Vec::with_capacity(config.fill_makers + config.fill_takers);
    for index in 0..config.fill_takers {
        users.push((config.user_base + index as u64, false));
    }
    for index in 0..config.fill_makers {
        users.push((
            config.user_base + config.fill_takers as u64 + index as u64,
            true,
        ));
    }

    let mut tasks = stream::iter(users.into_iter().enumerate().map(
        |(index, (user_id, maker))| async move {
            seed_fill_user(http, config, index, user_id, maker).await
        },
    ))
    .buffer_unordered(SEED_CONCURRENCY);

    while let Some(result) = tasks.next().await {
        result?;
    }
    Ok(())
}

async fn seed_fill_user(
    http: &Client,
    config: &LoadConfig,
    index: usize,
    user_id: UserId,
    maker: bool,
) -> LoadResult<()> {
    let deposit_id = SEED_CLIENT_BASE + (index as u64) * 2;
    let response = http
        .post(format!("{}/api/dev/deposit", config.base_url))
        .json(&json!({
            "user_id": user_id,
            "amount": config.seed_cash,
            "client_request_id": deposit_id,
        }))
        .send()
        .await?;
    expect_success(response, "dev deposit").await?;

    if maker {
        let response = http
            .post(format!("{}/api/dev/issue", config.base_url))
            .json(&json!({
                "user_id": user_id,
                "market_id": config.market_id,
                "qty": config.fill_passes,
                "client_request_id": deposit_id + 1,
            }))
            .send()
            .await?;
        expect_success(response, "dev issue").await?;
    }
    Ok(())
}

async fn place_maker_liquidity(http: &Client, config: &LoadConfig) -> LoadResult<SetupReport> {
    let mut orders = Vec::with_capacity(config.fill_makers * config.fill_passes as usize);
    let mut client_order_id = MAKER_CLIENT_BASE;
    for index in 0..config.fill_makers {
        let user_id = config.user_base + config.fill_takers as u64 + index as u64;
        for _ in 0..config.fill_passes {
            orders.push((user_id, client_order_id));
            client_order_id += 1;
        }
    }

    let maker_orders = orders.len() as u64;
    let started = Instant::now();
    let mut tasks = stream::iter(
        orders
            .into_iter()
            .map(|(user_id, client_order_id)| async move {
                send_maker_ask(http, config, user_id, client_order_id).await
            }),
    )
    .buffer_unordered(MAKER_SETUP_CONCURRENCY);

    let mut maker_ok = 0;
    while let Some(result) = tasks.next().await {
        result?;
        maker_ok += 1;
    }

    Ok(SetupReport {
        maker_orders,
        maker_ok,
        elapsed_ms: started.elapsed().as_millis() as u64,
    })
}

async fn send_maker_ask(
    http: &Client,
    config: &LoadConfig,
    user_id: UserId,
    client_order_id: ClientOrderId,
) -> LoadResult<()> {
    for _ in 0..CANCEL_RETRY_LIMIT {
        let call = send_limit(
            http,
            config,
            user_id,
            client_order_id,
            config.market_id,
            Side::Sell,
            config.fill_price_ticks,
            config.qty,
        )
        .await;
        match call.outcome {
            Ok(response) if response.status == OrderStatus::Rested => return Ok(()),
            Err(CommandFailure::Http(status, _))
                if status == StatusCode::TOO_MANY_REQUESTS
                    || status == StatusCode::SERVICE_UNAVAILABLE =>
            {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            Err(CommandFailure::Http(status, body)) => {
                return Err(load_error(format!(
                    "maker ask HTTP status {status}: {body}"
                )));
            }
            Err(CommandFailure::Transport(error)) => {
                return Err(load_error(format!("maker ask transport error: {error}")));
            }
            Err(CommandFailure::Decode(error)) => {
                return Err(load_error(format!("maker ask decode error: {error}")));
            }
            Ok(response) => {
                return Err(load_error(format!(
                    "maker ask status {:?}, expected Rested",
                    response.status
                )));
            }
        }
    }
    Err(load_error("maker ask retry limit exceeded"))
}

fn fill_taker_work(config: &LoadConfig) -> Vec<UserId> {
    let mut work = Vec::with_capacity(config.fill_takers * config.fill_passes as usize);
    for index in 0..config.fill_takers {
        let user_id = config.user_base + index as u64;
        for _ in 0..config.fill_passes {
            work.push(user_id);
        }
    }
    work
}

fn spawn_pacer(
    config: &LoadConfig,
    mut stop: watch::Receiver<bool>,
) -> (mpsc::Receiver<()>, JoinHandle<()>) {
    let (tx, rx) = mpsc::channel(1);
    let rate = match config.workload {
        Workload::PlaceCancel => config.placements_per_second,
        Workload::Fills => config.fills_per_second,
    };
    let handle = tokio::spawn(async move {
        let period = Duration::from_secs_f64(1.0 / rate as f64);
        let mut interval = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                _ = stop.changed() => break,
                _ = interval.tick() => {}
            }
            tokio::select! {
                _ = stop.changed() => break,
                result = tx.send(()) => {
                    if result.is_err() {
                        break;
                    }
                }
            }
        }
    });
    (rx, handle)
}

#[derive(Default)]
struct UserTaskStats {
    place: OutcomeCounts,
    cancel: OutcomeCounts,
    place_latencies_us: Vec<u64>,
    cancel_latencies_us: Vec<u64>,
    fills: u64,
}

impl UserTaskStats {
    fn merge(&mut self, other: Self) {
        self.place.merge(other.place);
        self.cancel.merge(other.cancel);
        self.place_latencies_us.extend(other.place_latencies_us);
        self.cancel_latencies_us.extend(other.cancel_latencies_us);
        self.fills += other.fills;
    }
}

async fn run_user_task(
    config: LoadConfig,
    http: Client,
    user_id: UserId,
    tick_rx: Arc<Mutex<mpsc::Receiver<()>>>,
    place_ids: Arc<AtomicU64>,
    cancel_ids: Arc<AtomicU64>,
    mut stop: watch::Receiver<bool>,
) -> LoadResult<UserTaskStats> {
    let mut stats = UserTaskStats::default();

    loop {
        let permit = tokio::select! {
            _ = stop.changed() => break,
            permit = async {
                let mut ticks = tick_rx.lock().await;
                ticks.recv().await
            } => permit,
        };
        if permit.is_none() {
            break;
        }

        let place_id = place_ids.fetch_add(1, Ordering::Relaxed);
        let call = send_place(&http, &config, user_id, place_id).await;
        stats.place.attempts += 1;
        stats.place_latencies_us.push(call.latency_us);

        match call.outcome {
            Ok(response) if response.status == OrderStatus::Rested => {
                let Some(order_id) = response.order_id else {
                    stats.place.unexpected += 1;
                    return Err(load_error("rested place had no order id"));
                };
                stats.place.ok += 1;
                let cancel_id = cancel_ids.fetch_add(1, Ordering::Relaxed);
                cancel_with_retry(&http, &config, user_id, order_id, cancel_id, &mut stats).await?;
            }
            Err(CommandFailure::Http(status, _)) if status == StatusCode::TOO_MANY_REQUESTS => {
                stats.place.queue_full += 1;
            }
            Err(CommandFailure::Http(status, _)) if status == StatusCode::SERVICE_UNAVAILABLE => {
                stats.place.unavailable += 1;
            }
            Err(CommandFailure::Http(status, body))
                if status == StatusCode::UNPROCESSABLE_ENTITY =>
            {
                stats.place.rejected += 1;
                return Err(load_error(format!("unexpected place rejection: {body}")));
            }
            Err(CommandFailure::Http(status, body)) => {
                stats.place.http_other += 1;
                return Err(load_error(format!(
                    "unexpected place HTTP status {status}: {body}"
                )));
            }
            Err(CommandFailure::Transport(error)) => {
                stats.place.transport_errors += 1;
                return Err(load_error(format!("place transport error: {error}")));
            }
            Err(CommandFailure::Decode(error)) => {
                stats.place.http_other += 1;
                return Err(load_error(format!("place decode error: {error}")));
            }
            Ok(response) => {
                stats.place.unexpected += 1;
                return Err(load_error(format!(
                    "unexpected place status {:?}",
                    response.status
                )));
            }
        }
    }

    Ok(stats)
}

async fn run_fill_task(
    config: LoadConfig,
    http: Client,
    work: Arc<Vec<UserId>>,
    next_work: Arc<AtomicUsize>,
    tick_rx: Arc<Mutex<mpsc::Receiver<()>>>,
    place_ids: Arc<AtomicU64>,
    mut stop: watch::Receiver<bool>,
) -> LoadResult<UserTaskStats> {
    let mut stats = UserTaskStats::default();

    loop {
        let permit = tokio::select! {
            _ = stop.changed() => break,
            permit = async {
                let mut ticks = tick_rx.lock().await;
                ticks.recv().await
            } => permit,
        };
        if permit.is_none() {
            break;
        }

        let work_index = next_work.fetch_add(1, Ordering::Relaxed);
        let Some(&user_id) = work.get(work_index) else {
            break;
        };

        let place_id = place_ids.fetch_add(1, Ordering::Relaxed);
        let mut success = None;
        for _ in 0..TAKER_RETRY_LIMIT {
            let call = send_limit(
                &http,
                &config,
                user_id,
                place_id,
                config.market_id,
                Side::Buy,
                config.fill_price_ticks,
                config.qty,
            )
            .await;
            stats.place.attempts += 1;

            match call.outcome {
                Ok(response) if response.status == OrderStatus::Filled => {
                    stats.place_latencies_us.push(call.latency_us);
                    success = Some(response);
                    break;
                }
                Err(CommandFailure::Http(status, _)) if status == StatusCode::TOO_MANY_REQUESTS => {
                    stats.place.queue_full += 1;
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
                Err(CommandFailure::Http(status, _))
                    if status == StatusCode::SERVICE_UNAVAILABLE =>
                {
                    stats.place.unavailable += 1;
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
                Err(CommandFailure::Http(status, body))
                    if status == StatusCode::UNPROCESSABLE_ENTITY =>
                {
                    stats.place.rejected += 1;
                    return Err(load_error(format!("taker fill rejection: {body}")));
                }
                Err(CommandFailure::Http(status, body)) => {
                    stats.place.http_other += 1;
                    return Err(load_error(format!(
                        "taker fill HTTP status {status}: {body}"
                    )));
                }
                Err(CommandFailure::Transport(error)) => {
                    stats.place.transport_errors += 1;
                    return Err(load_error(format!("taker fill transport error: {error}")));
                }
                Err(CommandFailure::Decode(error)) => {
                    stats.place.http_other += 1;
                    return Err(load_error(format!("taker fill decode error: {error}")));
                }
                Ok(response) => {
                    stats.place.unexpected += 1;
                    return Err(load_error(format!(
                        "taker fill status {:?}, expected Filled",
                        response.status
                    )));
                }
            }
        }

        let Some(response) = success else {
            stats.place.unexpected += 1;
            return Err(load_error("taker fill retry limit exceeded"));
        };
        if response.filled_qty != config.qty || response.remaining_qty != 0 {
            stats.place.unexpected += 1;
            return Err(load_error(format!(
                "taker fill incomplete: filled {} remaining {}",
                response.filled_qty, response.remaining_qty
            )));
        }
        if response.fills.len() as u64 != response.filled_qty {
            stats.place.unexpected += 1;
            return Err(load_error(format!(
                "taker fill count mismatch: {} fills for filled qty {}",
                response.fills.len(),
                response.filled_qty
            )));
        }
        stats.place.ok += 1;
        stats.fills += response.fills.len() as u64;
    }

    Ok(stats)
}

async fn cancel_with_retry(
    http: &Client,
    config: &LoadConfig,
    user_id: UserId,
    order_id: OrderId,
    client_order_id: ClientOrderId,
    stats: &mut UserTaskStats,
) -> LoadResult<()> {
    for _ in 0..CANCEL_RETRY_LIMIT {
        let call = send_cancel(http, config, user_id, order_id, client_order_id).await;
        stats.cancel.attempts += 1;
        stats.cancel_latencies_us.push(call.latency_us);

        match call.outcome {
            Ok(response) if response.status == OrderStatus::Cancelled => {
                stats.cancel.ok += 1;
                return Ok(());
            }
            Err(CommandFailure::Http(status, _)) if status == StatusCode::TOO_MANY_REQUESTS => {
                stats.cancel.queue_full += 1;
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            Err(CommandFailure::Http(status, _)) if status == StatusCode::SERVICE_UNAVAILABLE => {
                stats.cancel.unavailable += 1;
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            Err(CommandFailure::Http(status, body)) => {
                stats.cancel.http_other += 1;
                return Err(load_error(format!(
                    "unexpected cancel HTTP status {status}: {body}"
                )));
            }
            Err(CommandFailure::Transport(error)) => {
                stats.cancel.transport_errors += 1;
                return Err(load_error(format!("cancel transport error: {error}")));
            }
            Err(CommandFailure::Decode(error)) => {
                stats.cancel.http_other += 1;
                return Err(load_error(format!("cancel decode error: {error}")));
            }
            Ok(response) => {
                stats.cancel.unexpected += 1;
                return Err(load_error(format!(
                    "unexpected cancel status {:?}",
                    response.status
                )));
            }
        }
    }
    Err(load_error("cancel retry limit exceeded"))
}

struct CommandCall {
    latency_us: u64,
    outcome: Result<CommandResponse, CommandFailure>,
}

enum CommandFailure {
    Http(StatusCode, String),
    Transport(String),
    Decode(String),
}

async fn send_place(
    http: &Client,
    config: &LoadConfig,
    user_id: UserId,
    client_order_id: ClientOrderId,
) -> CommandCall {
    send_limit(
        http,
        config,
        user_id,
        client_order_id,
        config.market_id,
        Side::Buy,
        config.price_ticks,
        config.qty,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn send_limit(
    http: &Client,
    config: &LoadConfig,
    user_id: UserId,
    client_order_id: ClientOrderId,
    market_id: dx_types::MarketId,
    side: Side,
    price_ticks: dx_types::Ticks,
    qty: dx_types::Qty,
) -> CommandCall {
    let started = Instant::now();
    let command = Command::PlaceLimit(PlaceLimit {
        client_order_id,
        market_id,
        side,
        price_ticks,
        qty,
    });
    let outcome = match http
        .post(format!("{}/api/orders", config.base_url))
        .header("x-dev-user", user_id.to_string())
        .json(&command)
        .send()
        .await
    {
        Ok(response) => decode_response(response).await,
        Err(error) => Err(CommandFailure::Transport(error.to_string())),
    };
    CommandCall {
        latency_us: started.elapsed().as_micros() as u64,
        outcome,
    }
}

async fn send_cancel(
    http: &Client,
    config: &LoadConfig,
    user_id: UserId,
    order_id: OrderId,
    client_order_id: ClientOrderId,
) -> CommandCall {
    let started = Instant::now();
    let outcome = match http
        .delete(format!(
            "{}/api/orders/{order_id}?client_order_id={client_order_id}",
            config.base_url
        ))
        .header("x-dev-user", user_id.to_string())
        .send()
        .await
    {
        Ok(response) => decode_response(response).await,
        Err(error) => Err(CommandFailure::Transport(error.to_string())),
    };
    CommandCall {
        latency_us: started.elapsed().as_micros() as u64,
        outcome,
    }
}

async fn decode_response(response: reqwest::Response) -> Result<CommandResponse, CommandFailure> {
    let status = response.status();
    if status.is_success() {
        response
            .json::<CommandResponse>()
            .await
            .map_err(|error| CommandFailure::Decode(error.to_string()))
    } else {
        let body = response.text().await.unwrap_or_default();
        Err(CommandFailure::Http(status, body))
    }
}

async fn fetch_metrics(http: &Client, config: &LoadConfig) -> LoadResult<MetricsSnapshot> {
    let response = http
        .get(format!("{}/api/metrics/latency", config.base_url))
        .send()
        .await?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(load_error(format!(
            "metrics request failed with {status}: {body}"
        )));
    }
    Ok(response.json().await?)
}

async fn connect_baseline(
    config: &LoadConfig,
    market_id: dx_types::MarketId,
) -> LoadResult<(WsStream, u64)> {
    let url = ws_url(&config.base_url, market_id);
    let (mut stream, _) = connect_async(url).await?;
    let mut last_seq = None;
    let mut saw_metrics = false;
    let deadline = tokio::time::sleep(Duration::from_secs(5));
    tokio::pin!(deadline);

    loop {
        tokio::select! {
            _ = &mut deadline => {
                return Err(load_error(format!(
                    "timed out waiting for baseline on market {market_id}"
                )));
            }
            message = stream.next() => {
                match message {
                    Some(Ok(Message::Binary(bytes))) => {
                        let message: WsServerMessage = rmp_serde::from_slice(&bytes)?;
                        match message {
                            WsServerMessage::Snapshot { snapshot } => {
                                last_seq = Some(snapshot.book_seq);
                            }
                            WsServerMessage::Delta { delta } => {
                                last_seq = Some(delta.book_seq);
                            }
                            WsServerMessage::Metrics { .. } => {
                                saw_metrics = true;
                            }
                            _ => {}
                        }
                        if saw_metrics
                            && let Some(last_seq) = last_seq
                        {
                            return Ok((stream, last_seq));
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => {
                        return Err(load_error(format!(
                            "websocket closed before baseline on market {market_id}"
                        )));
                    }
                    Some(Ok(_)) => {}
                    Some(Err(error)) => {
                        return Err(load_error(format!("websocket error: {error}")));
                    }
                }
            }
        }
    }
}

fn ws_url(base_url: &str, market_id: dx_types::MarketId) -> String {
    let base = base_url
        .strip_prefix("http://")
        .map(|rest| format!("ws://{rest}"))
        .unwrap_or_else(|| base_url.to_string());
    format!("{base}/ws/market/{market_id}")
}

#[derive(Debug, Default)]
struct WsStats {
    messages: u64,
    deltas: u64,
    trades: u64,
    snapshots: u64,
    metrics: u64,
    resync_required: u64,
    seq_gaps: u64,
    bytes: u64,
    elapsed_ms: u64,
}

impl WsStats {
    fn into_report(self) -> WsConnectionReport {
        let seconds = self.elapsed_ms as f64 / 1_000.0;
        WsConnectionReport {
            messages: self.messages,
            deltas: self.deltas,
            trades: self.trades,
            snapshots: self.snapshots,
            metrics: self.metrics,
            resync_required: self.resync_required,
            seq_gaps: self.seq_gaps,
            bytes: self.bytes,
            elapsed_ms: self.elapsed_ms,
            deltas_per_second: if seconds > 0.0 {
                self.deltas as f64 / seconds
            } else {
                0.0
            },
            bytes_per_second: if seconds > 0.0 {
                self.bytes as f64 / seconds
            } else {
                0.0
            },
        }
    }
}

async fn read_ws(
    mut stream: WsStream,
    mut last_seq: u64,
    mut stop: watch::Receiver<bool>,
) -> LoadResult<WsStats> {
    let started = Instant::now();
    let mut stats = WsStats::default();

    loop {
        tokio::select! {
            _ = stop.changed() => break,
            message = stream.next() => {
                match message {
                    Some(Ok(Message::Binary(bytes))) => {
                        stats.messages += 1;
                        stats.bytes += bytes.len() as u64;
                        let message: WsServerMessage = rmp_serde::from_slice(&bytes)?;
                        match message {
                            WsServerMessage::Delta { delta } => {
                                stats.deltas += 1;
                                if delta.book_seq != last_seq.saturating_add(1) {
                                    stats.seq_gaps += 1;
                                }
                                last_seq = delta.book_seq;
                            }
                            WsServerMessage::Trade { .. } => stats.trades += 1,
                            WsServerMessage::Snapshot { snapshot } => {
                                stats.snapshots += 1;
                                last_seq = snapshot.book_seq;
                            }
                            WsServerMessage::Metrics { .. } => stats.metrics += 1,
                            WsServerMessage::ResyncRequired { .. } => stats.resync_required += 1,
                            _ => {}
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(_)) => {}
                    Some(Err(error)) => return Err(load_error(format!("websocket error: {error}"))),
                }
            }
        }
    }

    stats.elapsed_ms = started.elapsed().as_millis() as u64;
    Ok(stats)
}

async fn read_probe_ws(
    mut stream: WsStream,
    mut last_seq: u64,
    delta_tx: mpsc::Sender<u64>,
    mut stop: watch::Receiver<bool>,
) -> LoadResult<WsStats> {
    let started = Instant::now();
    let mut stats = WsStats::default();
    let mut forward_deltas = true;

    loop {
        tokio::select! {
            _ = stop.changed() => break,
            message = stream.next() => {
                match message {
                    Some(Ok(Message::Binary(bytes))) => {
                        stats.messages += 1;
                        stats.bytes += bytes.len() as u64;
                        let message: WsServerMessage = rmp_serde::from_slice(&bytes)?;
                        match message {
                            WsServerMessage::Delta { delta } => {
                                stats.deltas += 1;
                                if delta.book_seq != last_seq.saturating_add(1) {
                                    stats.seq_gaps += 1;
                                }
                                last_seq = delta.book_seq;
                                if forward_deltas && delta_tx.send(delta.book_seq).await.is_err() {
                                    forward_deltas = false;
                                }
                            }
                            WsServerMessage::Trade { .. } => stats.trades += 1,
                            WsServerMessage::Snapshot { snapshot } => {
                                stats.snapshots += 1;
                                last_seq = snapshot.book_seq;
                            }
                            WsServerMessage::Metrics { .. } => stats.metrics += 1,
                            WsServerMessage::ResyncRequired { .. } => stats.resync_required += 1,
                            _ => {}
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(_)) => {}
                    Some(Err(error)) => return Err(load_error(format!("probe websocket error: {error}"))),
                }
            }
        }
    }

    stats.elapsed_ms = started.elapsed().as_millis() as u64;
    Ok(stats)
}

#[derive(Default)]
struct ProbeTaskStats {
    place_attempts: u64,
    place_ok: u64,
    cancel_attempts: u64,
    cancel_ok: u64,
    overloaded: u64,
    place_e2e_us: Vec<u64>,
    cancel_e2e_us: Vec<u64>,
}

async fn run_probe(
    config: LoadConfig,
    http: Client,
    initial_seq: u64,
    mut deltas: mpsc::Receiver<u64>,
    mut stop: watch::Receiver<bool>,
) -> LoadResult<ProbeTaskStats> {
    let mut stats = ProbeTaskStats::default();
    let mut expected_seq = initial_seq;
    let mut place_id = PROBE_PLACE_BASE;
    let mut cancel_id = PROBE_CANCEL_BASE;
    let period = Duration::from_secs_f64(1.0 / config.probe_rate_per_second as f64);
    let mut interval = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            _ = stop.changed() => break,
            _ = interval.tick() => {}
        }

        let place_started = Instant::now();
        let place = send_place_for_probe(&http, &config, place_id).await;
        stats.place_attempts += 1;
        place_id += 1;
        match place.outcome {
            Ok(response) if response.status == OrderStatus::Rested => {
                let Some(order_id) = response.order_id else {
                    return Err(load_error("probe place had no order id"));
                };
                stats.place_ok += 1;
                let want = expected_seq.saturating_add(1);
                if !wait_for_delta(&mut deltas, want, &mut stop).await? {
                    break;
                }
                stats
                    .place_e2e_us
                    .push(place_started.elapsed().as_micros() as u64);
                expected_seq = want;

                let cancel_started = Instant::now();
                let this_cancel_id = cancel_id;
                cancel_id += 1;
                let mut cancelled = false;
                for _ in 0..CANCEL_RETRY_LIMIT {
                    let call =
                        send_cancel_for_probe(&http, &config, order_id, this_cancel_id).await;
                    stats.cancel_attempts += 1;
                    match call.outcome {
                        Ok(response) if response.status == OrderStatus::Cancelled => {
                            stats.cancel_ok += 1;
                            cancelled = true;
                            break;
                        }
                        Err(CommandFailure::Http(status, _))
                            if status == StatusCode::TOO_MANY_REQUESTS
                                || status == StatusCode::SERVICE_UNAVAILABLE =>
                        {
                            stats.overloaded += 1;
                            tokio::time::sleep(Duration::from_millis(1)).await;
                        }
                        Err(CommandFailure::Http(status, body)) => {
                            return Err(load_error(format!(
                                "probe cancel HTTP status {status}: {body}"
                            )));
                        }
                        Err(CommandFailure::Transport(error)) => {
                            return Err(load_error(format!(
                                "probe cancel transport error: {error}"
                            )));
                        }
                        Err(CommandFailure::Decode(error)) => {
                            return Err(load_error(format!("probe cancel decode error: {error}")));
                        }
                        Ok(response) => {
                            return Err(load_error(format!(
                                "probe cancel status {:?}",
                                response.status
                            )));
                        }
                    }
                }
                if !cancelled {
                    return Err(load_error("probe cancel retry limit exceeded"));
                }
                let want = expected_seq.saturating_add(1);
                if !wait_for_delta(&mut deltas, want, &mut stop).await? {
                    break;
                }
                stats
                    .cancel_e2e_us
                    .push(cancel_started.elapsed().as_micros() as u64);
                expected_seq = want;
            }
            Err(CommandFailure::Http(status, _))
                if status == StatusCode::TOO_MANY_REQUESTS
                    || status == StatusCode::SERVICE_UNAVAILABLE =>
            {
                stats.overloaded += 1;
            }
            Err(CommandFailure::Http(status, body)) => {
                return Err(load_error(format!(
                    "probe place HTTP status {status}: {body}"
                )));
            }
            Err(CommandFailure::Transport(error)) => {
                return Err(load_error(format!("probe place transport error: {error}")));
            }
            Err(CommandFailure::Decode(error)) => {
                return Err(load_error(format!("probe place decode error: {error}")));
            }
            Ok(response) => {
                return Err(load_error(format!(
                    "probe place status {:?}",
                    response.status
                )));
            }
        }
    }

    Ok(stats)
}

async fn wait_for_delta(
    deltas: &mut mpsc::Receiver<u64>,
    want: u64,
    stop: &mut watch::Receiver<bool>,
) -> LoadResult<bool> {
    let deadline = tokio::time::sleep(Duration::from_secs(2));
    tokio::pin!(deadline);

    tokio::select! {
        _ = &mut deadline => {
            Err(load_error(format!(
                "timed out waiting for probe book_seq {want}"
            )))
        }
        _ = stop.changed() => Ok(false),
        seq = deltas.recv() => {
            match seq {
                Some(seq) if seq == want => Ok(true),
                Some(seq) => Err(load_error(format!(
                    "probe expected book_seq {want}, got {seq}"
                ))),
                None => Err(load_error("probe websocket closed")),
            }
        }
    }
}

async fn send_place_for_probe(
    http: &Client,
    config: &LoadConfig,
    client_order_id: ClientOrderId,
) -> CommandCall {
    send_limit(
        http,
        config,
        config.probe_user,
        client_order_id,
        config.probe_market_id,
        Side::Buy,
        config.probe_price_ticks,
        config.qty,
    )
    .await
}

async fn send_cancel_for_probe(
    http: &Client,
    config: &LoadConfig,
    order_id: OrderId,
    client_order_id: ClientOrderId,
) -> CommandCall {
    let started = Instant::now();
    let outcome = match http
        .delete(format!(
            "{}/api/orders/{order_id}?client_order_id={client_order_id}",
            config.base_url
        ))
        .header("x-dev-user", config.probe_user.to_string())
        .send()
        .await
    {
        Ok(response) => decode_response(response).await,
        Err(error) => Err(CommandFailure::Transport(error.to_string())),
    };
    CommandCall {
        latency_us: started.elapsed().as_micros() as u64,
        outcome,
    }
}

fn load_error(message: impl Into<String>) -> Box<dyn Error + Send + Sync> {
    Box::new(std::io::Error::other(message.into()))
}
