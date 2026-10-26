//! Single-writer core runtime.
//!
//! The gateway submits every operation that touches the core through one bounded
//! queue. A dedicated thread owns the core, processes requests in queue order,
//! and publishes market-data events before the next request starts. This makes
//! queue order the commit order and keeps snapshots synchronized with the event
//! stream without a core mutex.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::wal::{WalOp, WalSession};
use dx_core::Core;
use dx_solvency::SolvencySnapshot;
use dx_types::{
    Asset, Balance, BookDelta, BookSnapshot, ClientOrderId, Command, CommandResponse, Fill,
    MarketConfig, MarketId, MarketView, Minor, OpenOrderView, OrderStatus, Qty, ReasonCode, Seq,
    UserId, WsServerMessage,
};
use hdrhistogram::Histogram;
use tokio::sync::{broadcast, oneshot};

pub(crate) const RECENT_TRADES_LIMIT: usize = 200;
const BASELINE_TRADES_LIMIT: usize = 20;
const SNAPSHOT_CACHE_DEPTH: usize = 200;
const MAX_LATENCY_US: u64 = 60_000_000;
const MAX_COMMAND_BATCH: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WriterError {
    QueueFull,
    Unavailable,
}

struct LatencyHistograms {
    queue_wait: Histogram<u64>,
    core: Histogram<u64>,
}

impl Default for LatencyHistograms {
    fn default() -> Self {
        Self {
            queue_wait: Histogram::new_with_max(MAX_LATENCY_US, 3)
                .expect("valid queue-wait histogram"),
            core: Histogram::new_with_max(MAX_LATENCY_US, 3).expect("valid core histogram"),
        }
    }
}

/// Gateway-owned counters and latency histograms.
pub(crate) struct MetricsState {
    pub(crate) commands_total: AtomicU64,
    pub(crate) rejected_total: AtomicU64,
    pub(crate) ws_clients: AtomicU32,
    pub(crate) slow_consumers: AtomicU64,
    pub(crate) seq: AtomicU64,
    pub(crate) queue_depth: AtomicU32,
    histograms: Mutex<LatencyHistograms>,
}

impl MetricsState {
    fn new(seq: Seq) -> Self {
        Self {
            commands_total: AtomicU64::new(0),
            rejected_total: AtomicU64::new(0),
            ws_clients: AtomicU32::new(0),
            slow_consumers: AtomicU64::new(0),
            seq: AtomicU64::new(seq),
            queue_depth: AtomicU32::new(0),
            histograms: Mutex::new(LatencyHistograms::default()),
        }
    }

    fn record_command(&self, queue_wait: Duration, core: Duration) {
        let mut histograms = self.histograms.lock().expect("latency histogram mutex");
        let _ = histograms.queue_wait.record(duration_micros(queue_wait));
        let _ = histograms.core.record(duration_micros(core));
    }

    pub(crate) fn p99(&self) -> (u64, u64) {
        let histograms = self.histograms.lock().expect("latency histogram mutex");
        (
            histograms.queue_wait.value_at_quantile(0.99),
            histograms.core.value_at_quantile(0.99),
        )
    }

    #[cfg(test)]
    pub(crate) fn latency_samples(&self) -> u64 {
        self.histograms
            .lock()
            .expect("latency histogram mutex")
            .core
            .len()
    }
}

fn duration_micros(duration: Duration) -> u64 {
    duration.as_micros().min(u128::from(MAX_LATENCY_US)) as u64
}

/// A snapshot baseline paired with a subscription positioned at the same commit.
pub(crate) struct StreamBaseline {
    pub(crate) rx: broadcast::Receiver<WsServerMessage>,
    pub(crate) markets: Vec<MarketConfig>,
    pub(crate) snapshots: Vec<BookSnapshot>,
    pub(crate) recent_trades: Vec<Fill>,
}

/// Cached max-depth snapshots owned by the writer.
///
/// Entries are invalidated by book or last-trade changes and rebuilt lazily by
/// the next snapshot request. Serving a shallower depth only slices the cached
/// top levels, so one entry can answer every public depth up to the cache depth.
#[derive(Default)]
struct SnapshotCache {
    entries: HashMap<MarketId, BookSnapshot>,
}

impl SnapshotCache {
    fn invalidate(&mut self, market_id: MarketId) {
        self.entries.remove(&market_id);
    }

    fn snapshot(&mut self, core: &Core, market_id: MarketId, depth: usize) -> Option<BookSnapshot> {
        let depth = depth.clamp(1, SNAPSHOT_CACHE_DEPTH);
        if let std::collections::hash_map::Entry::Vacant(entry) = self.entries.entry(market_id) {
            let snapshot = core.snapshot(market_id, SNAPSHOT_CACHE_DEPTH)?;
            entry.insert(snapshot);
        }

        let cached = self.entries.get(&market_id)?;
        Some(BookSnapshot {
            market_id: cached.market_id,
            book_seq: cached.book_seq,
            bids: cached.bids.iter().take(depth).cloned().collect(),
            asks: cached.asks.iter().take(depth).cloned().collect(),
            last_trade_price: cached.last_trade_price,
        })
    }
}

enum WriterRequest {
    Command {
        user_id: UserId,
        command: Command,
        reply: oneshot::Sender<CommandResponse>,
    },
    CreateMarket {
        config: MarketConfig,
        reply: oneshot::Sender<Result<(), String>>,
    },
    StateHash {
        reply: oneshot::Sender<[u8; 32]>,
    },
    #[cfg(test)]
    Shutdown {
        reply: oneshot::Sender<()>,
    },
    Markets {
        reply: oneshot::Sender<Vec<MarketView>>,
    },
    Snapshot {
        market_id: MarketId,
        depth: usize,
        reply: oneshot::Sender<Option<BookSnapshot>>,
    },
    Trades {
        market_id: MarketId,
        limit: usize,
        reply: oneshot::Sender<Option<Vec<Fill>>>,
    },
    MarketExists {
        market_id: MarketId,
        reply: oneshot::Sender<bool>,
    },
    Balances {
        user_id: UserId,
        reply: oneshot::Sender<Vec<Balance>>,
    },
    Solvency {
        extra_users: Vec<UserId>,
        reply: oneshot::Sender<Result<SolvencySnapshot, String>>,
    },
    OpenOrders {
        user_id: UserId,
        reply: oneshot::Sender<Vec<OpenOrderView>>,
    },
    Deposit {
        user_id: UserId,
        amount: Minor,
        client_request_id: ClientOrderId,
        reply: oneshot::Sender<Result<u64, ReasonCode>>,
    },
    Issue {
        user_id: UserId,
        market_id: MarketId,
        qty: Qty,
        client_request_id: ClientOrderId,
        reply: oneshot::Sender<Result<u64, ReasonCode>>,
    },
    Baseline {
        market_filter: Option<Vec<MarketId>>,
        depth: usize,
        reply: oneshot::Sender<StreamBaseline>,
    },
}

struct WriterJob {
    enqueued_at: Instant,
    request: WriterRequest,
}

/// Cloneable handle used by gateway handlers to submit core work.
#[derive(Clone)]
pub(crate) struct WriterHandle {
    tx: SyncSender<WriterJob>,
    metrics: Arc<MetricsState>,
}

impl WriterHandle {
    pub(crate) async fn command(
        &self,
        user_id: UserId,
        command: Command,
    ) -> Result<CommandResponse, WriterError> {
        self.request(|reply| WriterRequest::Command {
            user_id,
            command,
            reply,
        })
        .await
    }

    pub(crate) async fn create_market(&self, config: MarketConfig) -> Result<(), String> {
        let result = self
            .request(|reply| WriterRequest::CreateMarket { config, reply })
            .await;
        match result {
            Ok(result) => result,
            Err(WriterError::QueueFull) => Err("queue_full".into()),
            Err(WriterError::Unavailable) => Err("writer_unavailable".into()),
        }
    }

    pub(crate) async fn state_hash(&self) -> Result<[u8; 32], WriterError> {
        self.request(|reply| WriterRequest::StateHash { reply })
            .await
    }

    #[cfg(test)]
    pub(crate) async fn shutdown(&self) -> Result<(), WriterError> {
        self.request(|reply| WriterRequest::Shutdown { reply })
            .await
    }

    pub(crate) async fn markets(&self) -> Result<Vec<MarketView>, WriterError> {
        self.request(|reply| WriterRequest::Markets { reply }).await
    }

    pub(crate) async fn snapshot(
        &self,
        market_id: MarketId,
        depth: usize,
    ) -> Result<Option<BookSnapshot>, WriterError> {
        self.request(|reply| WriterRequest::Snapshot {
            market_id,
            depth,
            reply,
        })
        .await
    }

    pub(crate) async fn trades(
        &self,
        market_id: MarketId,
        limit: usize,
    ) -> Result<Option<Vec<Fill>>, WriterError> {
        self.request(|reply| WriterRequest::Trades {
            market_id,
            limit,
            reply,
        })
        .await
    }

    pub(crate) async fn market_exists(&self, market_id: MarketId) -> Result<bool, WriterError> {
        self.request(|reply| WriterRequest::MarketExists { market_id, reply })
            .await
    }

    pub(crate) async fn balances(&self, user_id: UserId) -> Result<Vec<Balance>, WriterError> {
        self.request(|reply| WriterRequest::Balances { user_id, reply })
            .await
    }

    pub(crate) async fn solvency(
        &self,
        extra_users: Vec<UserId>,
    ) -> Result<SolvencySnapshot, WriterError> {
        let result = self
            .request(|reply| WriterRequest::Solvency { extra_users, reply })
            .await?;
        result.map_err(|_| WriterError::Unavailable)
    }

    pub(crate) async fn open_orders(
        &self,
        user_id: UserId,
    ) -> Result<Vec<OpenOrderView>, WriterError> {
        self.request(|reply| WriterRequest::OpenOrders { user_id, reply })
            .await
    }

    pub(crate) async fn deposit(
        &self,
        user_id: UserId,
        amount: Minor,
        client_request_id: ClientOrderId,
    ) -> Result<Result<u64, ReasonCode>, WriterError> {
        self.request(|reply| WriterRequest::Deposit {
            user_id,
            amount,
            client_request_id,
            reply,
        })
        .await
    }

    pub(crate) async fn issue(
        &self,
        user_id: UserId,
        market_id: MarketId,
        qty: Qty,
        client_request_id: ClientOrderId,
    ) -> Result<Result<u64, ReasonCode>, WriterError> {
        self.request(|reply| WriterRequest::Issue {
            user_id,
            market_id,
            qty,
            client_request_id,
            reply,
        })
        .await
    }

    pub(crate) async fn baseline(
        &self,
        market_filter: Option<Vec<MarketId>>,
        depth: usize,
    ) -> Result<StreamBaseline, WriterError> {
        self.request(|reply| WriterRequest::Baseline {
            market_filter,
            depth,
            reply,
        })
        .await
    }

    async fn request<T>(
        &self,
        build: impl FnOnce(oneshot::Sender<T>) -> WriterRequest,
    ) -> Result<T, WriterError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.submit(build(reply_tx))?;
        reply_rx.await.map_err(|_| WriterError::Unavailable)
    }

    fn submit(&self, request: WriterRequest) -> Result<(), WriterError> {
        // Reserve the depth before making the job visible. The writer decrements
        // as soon as it receives the job, so this cannot underflow.
        self.metrics.queue_depth.fetch_add(1, Ordering::Relaxed);
        let job = WriterJob {
            enqueued_at: Instant::now(),
            request,
        };
        match self.tx.try_send(job) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => {
                self.metrics.queue_depth.fetch_sub(1, Ordering::Relaxed);
                Err(WriterError::QueueFull)
            }
            Err(TrySendError::Disconnected(_)) => {
                self.metrics.queue_depth.fetch_sub(1, Ordering::Relaxed);
                Err(WriterError::Unavailable)
            }
        }
    }
}

/// Start the core writer thread and return its handle plus shared metrics.
#[cfg(test)]
pub(crate) fn spawn_writer(
    core: Core,
    tx: broadcast::Sender<WsServerMessage>,
    recent_trades: Vec<Fill>,
    capacity: usize,
) -> (WriterHandle, Arc<MetricsState>) {
    spawn_writer_with_wal(core, tx, recent_trades, capacity, None)
}

pub(crate) fn spawn_writer_with_wal(
    core: Core,
    tx: broadcast::Sender<WsServerMessage>,
    mut recent_trades: Vec<Fill>,
    capacity: usize,
    wal: Option<WalSession>,
) -> (WriterHandle, Arc<MetricsState>) {
    let metrics = Arc::new(MetricsState::new(core.last_seq()));
    let (job_tx, job_rx) = std::sync::mpsc::sync_channel(capacity.max(1));
    let writer_metrics = metrics.clone();
    if recent_trades.len() > RECENT_TRADES_LIMIT {
        let excess = recent_trades.len() - RECENT_TRADES_LIMIT;
        recent_trades.drain(0..excess);
    }

    std::thread::Builder::new()
        .name("dx-core-writer".into())
        .spawn(move || writer_loop(core, tx, recent_trades, writer_metrics, job_rx, wal))
        .expect("spawn core writer");

    let handle = WriterHandle {
        tx: job_tx,
        metrics: metrics.clone(),
    };
    (handle, metrics)
}

fn append_wal(wal: &mut Option<WalSession>, op: &WalOp, state_hash: [u8; 32]) {
    if let Some(wal) = wal
        && let Err(error) = wal.append(op, state_hash)
    {
        panic!("WAL append failed: {error}");
    }
}

struct PendingCommand {
    response: CommandResponse,
    deltas: Vec<BookDelta>,
    cached: bool,
    wal_op: Option<WalOp>,
    reply: oneshot::Sender<CommandResponse>,
}

fn process_command(
    core: &mut Core,
    metrics: &MetricsState,
    user_id: UserId,
    command: Command,
    reply: oneshot::Sender<CommandResponse>,
    enqueued_at: Instant,
    wal_enabled: bool,
) -> PendingCommand {
    let queue_wait = enqueued_at.elapsed();
    let core_started = Instant::now();
    let wal_op = if wal_enabled {
        Some(WalOp::Command {
            user_id,
            command: command.clone(),
        })
    } else {
        None
    };
    let outcome = core.process_with_deltas(user_id, command);
    let core_elapsed = core_started.elapsed();
    metrics.record_command(queue_wait, core_elapsed);

    let wal_op = if outcome.cached { None } else { wal_op };
    let rejected = outcome.response.status == OrderStatus::Rejected;
    metrics.commands_total.fetch_add(1, Ordering::Relaxed);
    if rejected {
        metrics.rejected_total.fetch_add(1, Ordering::Relaxed);
    }
    metrics.seq.store(core.last_seq(), Ordering::Relaxed);

    PendingCommand {
        response: outcome.response,
        deltas: outcome.deltas,
        cached: outcome.cached,
        wal_op,
        reply,
    }
}

#[allow(clippy::too_many_arguments)]
fn process_command_batch(
    core: &mut Core,
    tx: &broadcast::Sender<WsServerMessage>,
    recent_trades: &mut Vec<Fill>,
    metrics: &MetricsState,
    snapshot_cache: &mut SnapshotCache,
    wal: &mut Option<WalSession>,
    jobs: &Receiver<WriterJob>,
    stashed_job: &mut Option<WriterJob>,
    first_enqueued_at: Instant,
    first_user_id: UserId,
    first_command: Command,
    first_reply: oneshot::Sender<CommandResponse>,
) -> bool {
    let wal_enabled = wal.is_some();
    let mut pending = vec![process_command(
        core,
        metrics,
        first_user_id,
        first_command,
        first_reply,
        first_enqueued_at,
        wal_enabled,
    )];
    let mut disconnected = false;

    while pending.len() < MAX_COMMAND_BATCH {
        match jobs.try_recv() {
            Ok(job) => {
                metrics.queue_depth.fetch_sub(1, Ordering::Relaxed);
                let enqueued_at = job.enqueued_at;
                match job.request {
                    WriterRequest::Command {
                        user_id,
                        command,
                        reply,
                    } => {
                        pending.push(process_command(
                            core,
                            metrics,
                            user_id,
                            command,
                            reply,
                            enqueued_at,
                            wal_enabled,
                        ));
                    }
                    request => {
                        *stashed_job = Some(WriterJob {
                            enqueued_at,
                            request,
                        });
                        break;
                    }
                }
            }
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Disconnected) => {
                disconnected = true;
                break;
            }
        }
    }

    let wal_ops: Vec<_> = pending
        .iter()
        .filter_map(|pending| pending.wal_op.clone())
        .collect();
    let checkpoint_due = wal
        .as_ref()
        .is_some_and(|wal| wal.checkpoint_due(wal_ops.len()));
    let state_hash = checkpoint_due.then(|| core.state_hash());
    if let Some(wal) = wal
        && !wal_ops.is_empty()
        && let Err(error) = wal.append_batch(&wal_ops, state_hash)
    {
        panic!("WAL append failed: {error}");
    }

    for pending in pending {
        let PendingCommand {
            response,
            deltas,
            cached,
            reply,
            ..
        } = pending;
        if !cached && response.status != OrderStatus::Rejected {
            for delta in &deltas {
                snapshot_cache.invalidate(delta.market_id);
                let _ = tx.send(WsServerMessage::Delta {
                    delta: delta.clone(),
                });
            }

            if !response.fills.is_empty() {
                recent_trades.extend(response.fills.iter().cloned());
                if recent_trades.len() > RECENT_TRADES_LIMIT {
                    let excess = recent_trades.len() - RECENT_TRADES_LIMIT;
                    recent_trades.drain(0..excess);
                }
            }

            for fill in &response.fills {
                snapshot_cache.invalidate(fill.market_id);
                let _ = tx.send(WsServerMessage::Trade { fill: fill.clone() });
            }
        }
        let _ = reply.send(response);
    }

    disconnected
}

fn writer_loop(
    mut core: Core,
    tx: broadcast::Sender<WsServerMessage>,
    mut recent_trades: Vec<Fill>,
    metrics: Arc<MetricsState>,
    jobs: Receiver<WriterJob>,
    mut wal: Option<WalSession>,
) {
    let mut snapshot_cache = SnapshotCache::default();
    let mut stashed_job: Option<WriterJob> = None;
    loop {
        let job = if let Some(job) = stashed_job.take() {
            job
        } else {
            match jobs.recv() {
                Ok(job) => {
                    metrics.queue_depth.fetch_sub(1, Ordering::Relaxed);
                    job
                }
                Err(_) => break,
            }
        };
        let enqueued_at = job.enqueued_at;

        match job.request {
            WriterRequest::Command {
                user_id,
                command,
                reply,
            } => {
                let disconnected = process_command_batch(
                    &mut core,
                    &tx,
                    &mut recent_trades,
                    &metrics,
                    &mut snapshot_cache,
                    &mut wal,
                    &jobs,
                    &mut stashed_job,
                    enqueued_at,
                    user_id,
                    command,
                    reply,
                );
                if disconnected {
                    break;
                }
            }
            WriterRequest::CreateMarket { config, reply } => {
                let market_id = config.id;
                let wal_op = WalOp::AddMarket {
                    config: config.clone(),
                };
                let result = core.add_market(config);
                if result.is_ok() {
                    append_wal(&mut wal, &wal_op, core.state_hash());
                    snapshot_cache.invalidate(market_id);
                    metrics.seq.store(core.last_seq(), Ordering::Relaxed);
                }
                let _ = reply.send(result);
            }
            WriterRequest::StateHash { reply } => {
                let _ = reply.send(core.state_hash());
            }
            #[cfg(test)]
            WriterRequest::Shutdown { reply } => {
                let _ = reply.send(());
                break;
            }
            WriterRequest::Markets { reply } => {
                let views = core
                    .engine()
                    .market_ids()
                    .into_iter()
                    .filter_map(|market_id| {
                        let config = core.config(market_id)?.clone();
                        let snapshot = snapshot_cache.snapshot(&core, market_id, 20)?;
                        Some(MarketView { config, snapshot })
                    })
                    .collect();
                let _ = reply.send(views);
            }
            WriterRequest::Snapshot {
                market_id,
                depth,
                reply,
            } => {
                let _ = reply.send(snapshot_cache.snapshot(&core, market_id, depth));
            }
            WriterRequest::Trades {
                market_id,
                limit,
                reply,
            } => {
                if core.config(market_id).is_none() {
                    let _ = reply.send(None);
                } else {
                    let market_filter = [market_id];
                    let trades = recent_trades_for(&recent_trades, Some(&market_filter), limit);
                    let _ = reply.send(Some(trades));
                }
            }
            WriterRequest::MarketExists { market_id, reply } => {
                let _ = reply.send(core.config(market_id).is_some());
            }
            WriterRequest::Balances { user_id, reply } => {
                let _ = reply.send(core.balances(user_id));
            }
            WriterRequest::Solvency { extra_users, reply } => {
                let _ = reply.send(build_solvency_snapshot(&core, &extra_users));
            }
            WriterRequest::OpenOrders { user_id, reply } => {
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
                let _ = reply.send(orders);
            }
            WriterRequest::Deposit {
                user_id,
                amount,
                client_request_id,
                reply,
            } => {
                let wal_op = WalOp::Deposit {
                    user_id,
                    amount,
                    client_request_id,
                };
                let result = core.deposit_cash(user_id, amount, client_request_id);
                if result.is_ok() {
                    append_wal(&mut wal, &wal_op, core.state_hash());
                }
                let _ = reply.send(result);
            }
            WriterRequest::Issue {
                user_id,
                market_id,
                qty,
                client_request_id,
                reply,
            } => {
                let wal_op = WalOp::Issue {
                    user_id,
                    market_id,
                    qty,
                    client_request_id,
                };
                let result = core.issue_passes(user_id, market_id, qty, client_request_id);
                if result.is_ok() {
                    append_wal(&mut wal, &wal_op, core.state_hash());
                }
                let _ = reply.send(result);
            }
            WriterRequest::Baseline {
                market_filter,
                depth,
                reply,
            } => {
                let rx = tx.subscribe();
                let market_ids = match market_filter.as_ref() {
                    Some(ids) => ids.clone(),
                    None => core.engine().market_ids(),
                };
                let markets = market_ids
                    .iter()
                    .filter_map(|market_id| core.config(*market_id).cloned())
                    .collect();
                let snapshots = market_ids
                    .iter()
                    .filter_map(|market_id| snapshot_cache.snapshot(&core, *market_id, depth))
                    .collect();
                let recent_trades = recent_trades_for(
                    &recent_trades,
                    market_filter.as_deref(),
                    BASELINE_TRADES_LIMIT,
                );
                let _ = reply.send(StreamBaseline {
                    rx,
                    markets,
                    snapshots,
                    recent_trades,
                });
            }
        }
    }
}

/// Build the per-asset liability tree from the core's current balances.
///
/// Auth users are passed in so zero-balance users are still leaves; otherwise a
/// new sign-up could not produce an inclusion proof until their first deposit.
fn build_solvency_snapshot(
    core: &Core,
    extra_users: &[UserId],
) -> Result<SolvencySnapshot, String> {
    let mut balances = core.solvency_balances()?;
    let mut seen: HashSet<(UserId, Asset)> = balances
        .iter()
        .map(|(user_id, asset, _)| (*user_id, *asset))
        .collect();

    let mut assets = Vec::with_capacity(1 + core.engine().market_ids().len());
    assets.push(Asset::Cash);
    assets.extend(
        core.engine()
            .market_ids()
            .into_iter()
            .map(|market_id| Asset::Pass { market_id }),
    );

    for &user_id in extra_users {
        for &asset in &assets {
            if seen.insert((user_id, asset)) {
                balances.push((user_id, asset, 0));
            }
        }
    }

    SolvencySnapshot::from_balances(core.state_version(), balances)
        .map_err(|error| error.to_string())
}

fn recent_trades_for(
    recent_trades: &[Fill],
    market_filter: Option<&[MarketId]>,
    limit: usize,
) -> Vec<Fill> {
    let mut trades = recent_trades
        .iter()
        .rev()
        .filter(|fill| market_filter.is_none_or(|ids| ids.contains(&fill.market_id)))
        .take(limit)
        .cloned()
        .collect::<Vec<_>>();
    trades.reverse();
    trades
}

#[cfg(test)]
mod tests {
    use super::*;
    use dx_types::{PlaceLimit, PlaceMarket, Side, seed_markets};
    use tempfile::tempdir;

    const MARKET: MarketId = 1;

    fn funded_core() -> Core {
        let mut core = Core::new(seed_markets()).unwrap();
        core.deposit_cash(1, 10_000_000, 1).unwrap();
        core.deposit_cash(2, 10_000_000, 2).unwrap();
        core.issue_passes(2, MARKET, 5, 1).unwrap();
        core
    }

    #[test]
    fn queue_full_is_reported_without_blocking() {
        let (tx, _jobs) = std::sync::mpsc::sync_channel(1);
        let metrics = Arc::new(MetricsState::new(0));
        let handle = WriterHandle {
            tx,
            metrics: metrics.clone(),
        };

        let (reply, _reply_rx) = oneshot::channel();
        handle
            .submit(WriterRequest::MarketExists {
                market_id: 1,
                reply,
            })
            .unwrap();
        assert_eq!(metrics.queue_depth.load(Ordering::Relaxed), 1);

        let (reply, _reply_rx) = oneshot::channel();
        let error = handle
            .submit(WriterRequest::MarketExists {
                market_id: 2,
                reply,
            })
            .unwrap_err();
        assert_eq!(error, WriterError::QueueFull);
        assert_eq!(metrics.queue_depth.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn create_market_reaches_the_writer_and_is_tradable() {
        let core = funded_core();
        let (tx, _rx) = broadcast::channel(16);
        let (writer, metrics) = spawn_writer(core, tx, Vec::new(), 8);
        let mut config = seed_markets().into_iter().next().unwrap();
        config.id = 100;
        config.event_id = 99;
        config.name = "Runtime market".into();

        writer.create_market(config.clone()).await.unwrap();
        assert_eq!(metrics.seq.load(Ordering::Relaxed), 1);
        assert!(writer.market_exists(100).await.unwrap());
        assert_eq!(writer.snapshot(100, 20).await.unwrap().unwrap().book_seq, 0);
        assert!(writer.create_market(config).await.is_err());

        writer.issue(2, 100, 2, 99).await.unwrap().unwrap();
        let response = writer
            .command(
                2,
                Command::PlaceLimit(PlaceLimit {
                    client_order_id: 1,
                    market_id: 100,
                    side: Side::Sell,
                    price_ticks: 5_000,
                    qty: 2,
                }),
            )
            .await
            .unwrap();
        assert_eq!(response.status, OrderStatus::Rested);
        assert_eq!(
            writer.snapshot(100, 20).await.unwrap().unwrap().asks[0].qty,
            2
        );
    }

    #[tokio::test]
    async fn wal_recovery_restores_the_same_state_hash() {
        let dir = tempdir().unwrap();
        let recovered = dkx_wal::recover(dir.path()).unwrap();
        let wal = WalSession::open(dir.path(), recovered.last_seq, recovered.last_hash).unwrap();
        let (tx, _rx) = broadcast::channel(16);
        let (writer, _metrics) =
            spawn_writer_with_wal(funded_core(), tx, Vec::new(), 16, Some(wal));

        writer.deposit(3, 1_000_000, 7).await.unwrap().unwrap();
        let mut config = seed_markets().into_iter().next().unwrap();
        config.id = 100;
        config.event_id = 99;
        config.name = "WAL market".into();
        writer.create_market(config).await.unwrap();
        writer.issue(2, 100, 2, 7).await.unwrap().unwrap();
        writer
            .command(
                2,
                Command::PlaceLimit(PlaceLimit {
                    client_order_id: 1,
                    market_id: 100,
                    side: Side::Sell,
                    price_ticks: 5_000,
                    qty: 2,
                }),
            )
            .await
            .unwrap();
        writer
            .command(
                1,
                Command::PlaceMarket(PlaceMarket {
                    client_order_id: 2,
                    market_id: 100,
                    side: Side::Buy,
                    qty: 2,
                    protect_price_ticks: 5_000,
                }),
            )
            .await
            .unwrap();

        let expected_hash = writer.state_hash().await.unwrap();
        let expected_solvency_seq = writer.solvency(vec![]).await.unwrap().seq;
        writer.shutdown().await.unwrap();
        drop(writer);

        let recovered = dkx_wal::recover(dir.path()).unwrap();
        let mut restored = funded_core();
        let fills = crate::wal::replay(&recovered.replayed_records, &mut restored).unwrap();
        assert_eq!(restored.state_hash(), expected_hash);
        assert_eq!(
            build_solvency_snapshot(&restored, &[]).unwrap().seq,
            expected_solvency_seq
        );
        assert_eq!(fills.len(), 1);
        assert_eq!(restored.balance(3, Asset::Cash).available, 1_000_000);
    }

    #[test]
    fn solvency_snapshot_includes_zero_balance_users_and_verifies() {
        let mut core = funded_core();
        let snapshot = build_solvency_snapshot(&core, &[99]).unwrap();
        assert_eq!(snapshot.seq, core.state_version());

        let before = snapshot.seq;
        core.deposit_cash(1, 1, 2).unwrap();
        let after = build_solvency_snapshot(&core, &[99]).unwrap();
        assert!(after.seq > before);
        assert_eq!(after.seq, core.state_version());

        let cash = snapshot.tree(Asset::Cash).unwrap();
        assert!(cash.proof(1).unwrap().verify().unwrap());
        assert_eq!(cash.proof(99).unwrap().balance, 0);
        assert!(cash.proof(99).unwrap().verify().unwrap());

        let pass_asset = Asset::Pass { market_id: MARKET };
        assert!(snapshot.proof(2, pass_asset).unwrap().verify().unwrap());
        assert!(snapshot.proof(99, pass_asset).is_ok());
    }

    #[test]
    fn latency_histograms_report_live_p99() {
        let metrics = MetricsState::new(0);
        for value in 1..=100u64 {
            metrics.record_command(
                Duration::from_micros(value * 10),
                Duration::from_micros(value),
            );
        }

        let (queue_wait_p99, core_p99) = metrics.p99();
        assert!(queue_wait_p99 >= 990, "{queue_wait_p99}");
        assert!(core_p99 >= 99, "{core_p99}");
        assert_eq!(metrics.latency_samples(), 100);
    }

    #[test]
    fn snapshot_cache_matches_core_at_every_public_depth() {
        let mut core = funded_core();
        let mut cache = SnapshotCache::default();

        for depth in [1, 20, 200] {
            assert_eq!(
                cache.snapshot(&core, MARKET, depth),
                core.snapshot(MARKET, depth)
            );
        }

        let response = core.process(
            2,
            Command::PlaceLimit(PlaceLimit {
                client_order_id: 1,
                market_id: MARKET,
                side: Side::Sell,
                price_ticks: 5_000,
                qty: 2,
            }),
        );
        assert_eq!(response.status, OrderStatus::Rested);
        cache.invalidate(MARKET);

        for depth in [1, 20, 200] {
            assert_eq!(
                cache.snapshot(&core, MARKET, depth),
                core.snapshot(MARKET, depth)
            );
        }
    }

    #[test]
    fn snapshot_cache_refreshes_last_trade_price_after_a_fill() {
        let mut core = funded_core();
        let mut cache = SnapshotCache::default();

        let ask = core.process(
            2,
            Command::PlaceLimit(PlaceLimit {
                client_order_id: 2,
                market_id: MARKET,
                side: Side::Sell,
                price_ticks: 5_000,
                qty: 1,
            }),
        );
        assert_eq!(ask.status, OrderStatus::Rested);
        cache.invalidate(MARKET);
        let before = cache.snapshot(&core, MARKET, 20).unwrap();

        let buy = core.process(
            1,
            Command::PlaceMarket(PlaceMarket {
                client_order_id: 3,
                market_id: MARKET,
                side: Side::Buy,
                qty: 1,
                protect_price_ticks: 5_000,
            }),
        );
        assert_eq!(buy.status, OrderStatus::Filled);
        cache.invalidate(MARKET);

        let after = cache.snapshot(&core, MARKET, 20).unwrap();
        assert_eq!(after.last_trade_price, Some(5_000));
        assert_ne!(before.book_seq, after.book_seq);
        assert_eq!(Some(after), core.snapshot(MARKET, 20));
    }
}
