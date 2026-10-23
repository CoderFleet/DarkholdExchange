//! Single-writer core runtime.
//!
//! The gateway submits every operation that touches the core through one bounded
//! queue. A dedicated thread owns the core, processes requests in queue order,
//! and publishes market-data events before the next request starts. This makes
//! queue order the commit order and keeps snapshots synchronized with the event
//! stream without a core mutex.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dx_core::Core;
use dx_types::{
    Balance, BookSnapshot, ClientOrderId, Command, CommandResponse, Fill, MarketConfig, MarketId,
    MarketView, Minor, OpenOrderView, OrderStatus, Qty, ReasonCode, Seq, UserId, WsServerMessage,
};
use hdrhistogram::Histogram;
use tokio::sync::{broadcast, oneshot};

pub(crate) const RECENT_TRADES_LIMIT: usize = 200;
const BASELINE_TRADES_LIMIT: usize = 20;
const SNAPSHOT_CACHE_DEPTH: usize = 200;
const MAX_LATENCY_US: u64 = 60_000_000;

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
pub(crate) fn spawn_writer(
    core: Core,
    tx: broadcast::Sender<WsServerMessage>,
    recent_trades: Vec<Fill>,
    capacity: usize,
) -> (WriterHandle, Arc<MetricsState>) {
    let metrics = Arc::new(MetricsState::new(core.last_seq()));
    let (job_tx, job_rx) = std::sync::mpsc::sync_channel(capacity.max(1));
    let writer_metrics = metrics.clone();

    std::thread::Builder::new()
        .name("dx-core-writer".into())
        .spawn(move || writer_loop(core, tx, recent_trades, writer_metrics, job_rx))
        .expect("spawn core writer");

    let handle = WriterHandle {
        tx: job_tx,
        metrics: metrics.clone(),
    };
    (handle, metrics)
}

fn writer_loop(
    mut core: Core,
    tx: broadcast::Sender<WsServerMessage>,
    mut recent_trades: Vec<Fill>,
    metrics: Arc<MetricsState>,
    jobs: Receiver<WriterJob>,
) {
    let mut snapshot_cache = SnapshotCache::default();
    while let Ok(job) = jobs.recv() {
        metrics.queue_depth.fetch_sub(1, Ordering::Relaxed);
        let enqueued_at = job.enqueued_at;

        match job.request {
            WriterRequest::Command {
                user_id,
                command,
                reply,
            } => {
                let queue_wait = enqueued_at.elapsed();
                let core_started = Instant::now();
                let outcome = core.process_with_deltas(user_id, command);
                let core_elapsed = core_started.elapsed();
                metrics.record_command(queue_wait, core_elapsed);

                let publish = !outcome.cached && outcome.response.status != OrderStatus::Rejected;
                if publish {
                    for delta in &outcome.deltas {
                        snapshot_cache.invalidate(delta.market_id);
                        let _ = tx.send(WsServerMessage::Delta {
                            delta: delta.clone(),
                        });
                    }

                    if !outcome.response.fills.is_empty() {
                        recent_trades.extend(outcome.response.fills.iter().cloned());
                        if recent_trades.len() > RECENT_TRADES_LIMIT {
                            let excess = recent_trades.len() - RECENT_TRADES_LIMIT;
                            recent_trades.drain(0..excess);
                        }
                    }

                    for fill in &outcome.response.fills {
                        snapshot_cache.invalidate(fill.market_id);
                        let _ = tx.send(WsServerMessage::Trade { fill: fill.clone() });
                    }
                }

                let rejected = outcome.response.status == OrderStatus::Rejected;
                metrics.commands_total.fetch_add(1, Ordering::Relaxed);
                if rejected {
                    metrics.rejected_total.fetch_add(1, Ordering::Relaxed);
                }
                metrics.seq.store(core.last_seq(), Ordering::Relaxed);
                let _ = reply.send(outcome.response);
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
                let result = core.deposit_cash(user_id, amount, client_request_id);
                let _ = reply.send(result);
            }
            WriterRequest::Issue {
                user_id,
                market_id,
                qty,
                client_request_id,
                reply,
            } => {
                let result = core.issue_passes(user_id, market_id, qty, client_request_id);
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
