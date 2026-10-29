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
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::wal::{WalOp, WalSession};
use dkx_escrow::{AdminRuling, DisputeRecord};
use dkx_hash::{
    AuditRecord, GENESIS_PREV_HASH, hash_audit_entry, hash_merkle_leaf, hash_merkle_node,
    pair_with_empty,
};
use dx_core::Core;
use dx_solvency::SolvencySnapshot;
use dx_types::{
    AcceptDutchAuction, AskRecord, Asset, AuctionAccept, Balance, BatchAuction, BatchAuctionFill,
    BidRecord, BookDelta, BookSnapshot, ClientOrderId, Command, CommandResponse, CommitAsk,
    CommitBid, CreateBatchAuction, CreateDutchAuction, DutchAuction, EscrowRecord, Fill,
    MarketConfig, MarketId, MarketView, Minor, OpenOrderView, OrderStatus, Qty, ReasonCode,
    RevealAsk, RevealBid, Seq, SettleBatchAuction, TradeId, UserId, WsServerMessage,
};
use hdrhistogram::Histogram;
use sha2::{Digest, Sha256};
use tokio::sync::{broadcast, oneshot};

pub(crate) const RECENT_TRADES_LIMIT: usize = 200;
const BASELINE_TRADES_LIMIT: usize = 20;
const SNAPSHOT_CACHE_DEPTH: usize = 200;
const MAX_LATENCY_US: u64 = 60_000_000;
const MAX_COMMAND_BATCH: usize = 256;

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

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
        now_ms: u64,
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
    RegisterPublicKey {
        user_id: UserId,
        public_key: [u8; 32],
        reply: oneshot::Sender<Result<(), ReasonCode>>,
    },
    PublicKey {
        user_id: UserId,
        reply: oneshot::Sender<Option<[u8; 32]>>,
    },
    Escrows {
        user_id: UserId,
        reply: oneshot::Sender<Vec<EscrowRecord>>,
    },
    ReleaseEscrow {
        actor_user_id: UserId,
        trade_id: TradeId,
        delivered_at_ms: u64,
        signature: Vec<u8>,
        now_ms: u64,
        reply: oneshot::Sender<Result<EscrowRecord, ReasonCode>>,
    },
    RefundEscrow {
        actor_user_id: UserId,
        trade_id: TradeId,
        now_ms: u64,
        reply: oneshot::Sender<Result<EscrowRecord, ReasonCode>>,
    },
    CreateAuction {
        seller_id: UserId,
        request: CreateDutchAuction,
        now_ms: u64,
        reply: oneshot::Sender<Result<DutchAuction, ReasonCode>>,
    },
    AcceptAuction {
        buyer_id: UserId,
        request: AcceptDutchAuction,
        now_ms: u64,
        reply: oneshot::Sender<Result<AuctionAccept, ReasonCode>>,
    },
    ExpireAuction {
        actor_user_id: UserId,
        auction_id: u64,
        now_ms: u64,
        reply: oneshot::Sender<Result<DutchAuction, ReasonCode>>,
    },
    Auctions {
        market_id: Option<MarketId>,
        reply: oneshot::Sender<Vec<DutchAuction>>,
    },
    Auction {
        auction_id: u64,
        reply: oneshot::Sender<Option<DutchAuction>>,
    },
    CreateBatchAuction {
        created_by: UserId,
        request: CreateBatchAuction,
        now_ms: u64,
        reply: oneshot::Sender<Result<BatchAuction, ReasonCode>>,
    },
    CommitBatchBid {
        bidder_id: UserId,
        request: CommitBid,
        now_ms: u64,
        reply: oneshot::Sender<Result<BidRecord, ReasonCode>>,
    },
    RevealBatchBid {
        bidder_id: UserId,
        request: RevealBid,
        now_ms: u64,
        reply: oneshot::Sender<Result<BidRecord, ReasonCode>>,
    },
    CommitBatchAsk {
        seller_id: UserId,
        request: CommitAsk,
        now_ms: u64,
        reply: oneshot::Sender<Result<AskRecord, ReasonCode>>,
    },
    RevealBatchAsk {
        seller_id: UserId,
        request: RevealAsk,
        now_ms: u64,
        reply: oneshot::Sender<Result<AskRecord, ReasonCode>>,
    },
    SettleBatchAuction {
        caller_id: UserId,
        request: SettleBatchAuction,
        now_ms: u64,
        reply: oneshot::Sender<Result<Vec<BatchAuctionFill>, ReasonCode>>,
    },
    BatchAuctions {
        market_id: Option<MarketId>,
        reply: oneshot::Sender<Vec<BatchAuction>>,
    },
    BatchAuction {
        auction_id: u64,
        reply: oneshot::Sender<Option<BatchAuction>>,
    },
    BatchBids {
        auction_id: u64,
        reply: oneshot::Sender<Vec<BidRecord>>,
    },
    BatchAsks {
        auction_id: u64,
        reply: oneshot::Sender<Vec<AskRecord>>,
    },
    Baseline {
        market_filter: Option<Vec<MarketId>>,
        depth: usize,
        reply: oneshot::Sender<StreamBaseline>,
    },
    // ── Escrow operations ──────────────────────────────────────────────────
    AllEscrows {
        reply: oneshot::Sender<Vec<dkx_escrow::EscrowRecord>>,
    },
    GetEscrow {
        id: u64,
        reply: oneshot::Sender<Option<dkx_escrow::EscrowRecord>>,
    },
    EscrowAccept {
        user_id: UserId,
        id: u64,
        reply: oneshot::Sender<Result<(), ReasonCode>>,
    },
    EscrowReject {
        user_id: UserId,
        id: u64,
        reason: String,
        note: String,
        reply: oneshot::Sender<Result<(), ReasonCode>>,
    },
    EscrowVerify {
        id: u64,
        reply: oneshot::Sender<bool>,
    },
    AllDisputes {
        reply: oneshot::Sender<Vec<DisputeRecord>>,
    },
    AdminRule {
        admin_id: UserId,
        id: u64,
        ruling: AdminRuling,
        note: String,
        reply: oneshot::Sender<Result<(), ReasonCode>>,
    },
    EscrowPublicKey {
        reply: oneshot::Sender<String>,
    },
    AuditLog {
        from_seq: Option<u64>,
        limit: usize,
        reply: oneshot::Sender<Vec<AuditRecord>>,
    },
    AuditHead {
        reply: oneshot::Sender<Option<AuditRecord>>,
    },
    RecordAuditNote {
        kind: u8,
        market_id: u32,
        qty: u32,
        price_paise: i64,
        reply: oneshot::Sender<AuditRecord>,
    },
    ProofSolvency {
        reply: oneshot::Sender<SolvencyRootData>,
    },
    ProofBalance {
        user_id: UserId,
        reply: oneshot::Sender<Option<BalanceProofData>>,
    },
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct SolvencyRootData {
    pub seq: u64,
    pub root_hash_hex: String,
    pub total_paise: u64,
    pub leaf_count: usize,
    pub published_at: u64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct BalanceLeafData {
    pub user_id: u64,
    pub balance_paise: u64,
    pub salt_hex: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct PathStepData {
    pub hash_hex: String,
    pub sum_paise: u64,
    pub is_left: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct BalanceProofData {
    pub leaf: BalanceLeafData,
    pub path: Vec<PathStepData>,
    pub root_hash_hex: String,
    pub total_paise: u64,
    pub seq: u64,
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
    private: Option<broadcast::Sender<PrivateEvent>>,
}

impl WriterHandle {
    /// Subscribe to this user's private event stream. Private events carry the
    /// affected `user_id`; routing to sockets happens in main.rs, keyed by the
    /// token-checked identity, so a connection must know whom to listen for.
    pub(crate) fn subscribe_private(&self) -> Option<broadcast::Receiver<PrivateEvent>> {
        self.private.as_ref().map(|private| private.subscribe())
    }

    pub(crate) async fn command(
        &self,
        user_id: UserId,
        command: Command,
    ) -> Result<CommandResponse, WriterError> {
        self.request(|reply| WriterRequest::Command {
            user_id,
            command,
            now_ms: unix_time_ms(),
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

    pub(crate) async fn register_public_key(
        &self,
        user_id: UserId,
        public_key: [u8; 32],
    ) -> Result<Result<(), ReasonCode>, WriterError> {
        self.request(|reply| WriterRequest::RegisterPublicKey {
            user_id,
            public_key,
            reply,
        })
        .await
    }

    pub(crate) async fn public_key(
        &self,
        user_id: UserId,
    ) -> Result<Option<[u8; 32]>, WriterError> {
        self.request(|reply| WriterRequest::PublicKey { user_id, reply })
            .await
    }

    pub(crate) async fn escrows(&self, user_id: UserId) -> Result<Vec<EscrowRecord>, WriterError> {
        self.request(|reply| WriterRequest::Escrows { user_id, reply })
            .await
    }

    pub(crate) async fn release_escrow(
        &self,
        actor_user_id: UserId,
        trade_id: TradeId,
        delivered_at_ms: u64,
        signature: Vec<u8>,
        now_ms: u64,
    ) -> Result<Result<EscrowRecord, ReasonCode>, WriterError> {
        self.request(|reply| WriterRequest::ReleaseEscrow {
            actor_user_id,
            trade_id,
            delivered_at_ms,
            signature,
            now_ms,
            reply,
        })
        .await
    }

    pub(crate) async fn refund_escrow(
        &self,
        actor_user_id: UserId,
        trade_id: TradeId,
        now_ms: u64,
    ) -> Result<Result<EscrowRecord, ReasonCode>, WriterError> {
        self.request(|reply| WriterRequest::RefundEscrow {
            actor_user_id,
            trade_id,
            now_ms,
            reply,
        })
        .await
    }

    pub(crate) async fn create_auction(
        &self,
        seller_id: UserId,
        request: CreateDutchAuction,
        now_ms: u64,
    ) -> Result<Result<DutchAuction, ReasonCode>, WriterError> {
        self.request(|reply| WriterRequest::CreateAuction {
            seller_id,
            request,
            now_ms,
            reply,
        })
        .await
    }

    pub(crate) async fn accept_auction(
        &self,
        buyer_id: UserId,
        request: AcceptDutchAuction,
        now_ms: u64,
    ) -> Result<Result<AuctionAccept, ReasonCode>, WriterError> {
        self.request(|reply| WriterRequest::AcceptAuction {
            buyer_id,
            request,
            now_ms,
            reply,
        })
        .await
    }

    pub(crate) async fn expire_auction(
        &self,
        actor_user_id: UserId,
        auction_id: u64,
        now_ms: u64,
    ) -> Result<Result<DutchAuction, ReasonCode>, WriterError> {
        self.request(|reply| WriterRequest::ExpireAuction {
            actor_user_id,
            auction_id,
            now_ms,
            reply,
        })
        .await
    }

    pub(crate) async fn auctions(
        &self,
        market_id: Option<MarketId>,
    ) -> Result<Vec<DutchAuction>, WriterError> {
        self.request(|reply| WriterRequest::Auctions { market_id, reply })
            .await
    }

    pub(crate) async fn auction(
        &self,
        auction_id: u64,
    ) -> Result<Option<DutchAuction>, WriterError> {
        self.request(|reply| WriterRequest::Auction { auction_id, reply })
            .await
    }

    pub(crate) async fn create_batch_auction(
        &self,
        created_by: UserId,
        request: CreateBatchAuction,
        now_ms: u64,
    ) -> Result<Result<BatchAuction, ReasonCode>, WriterError> {
        self.request(|reply| WriterRequest::CreateBatchAuction {
            created_by,
            request,
            now_ms,
            reply,
        })
        .await
    }

    pub(crate) async fn commit_batch_ask(
        &self,
        seller_id: UserId,
        request: CommitAsk,
        now_ms: u64,
    ) -> Result<Result<AskRecord, ReasonCode>, WriterError> {
        self.request(|reply| WriterRequest::CommitBatchAsk {
            seller_id,
            request,
            now_ms,
            reply,
        })
        .await
    }

    pub(crate) async fn reveal_batch_ask(
        &self,
        seller_id: UserId,
        request: RevealAsk,
        now_ms: u64,
    ) -> Result<Result<AskRecord, ReasonCode>, WriterError> {
        self.request(|reply| WriterRequest::RevealBatchAsk {
            seller_id,
            request,
            now_ms,
            reply,
        })
        .await
    }

    pub(crate) async fn commit_batch_bid(
        &self,
        bidder_id: UserId,
        request: CommitBid,
        now_ms: u64,
    ) -> Result<Result<BidRecord, ReasonCode>, WriterError> {
        self.request(|reply| WriterRequest::CommitBatchBid {
            bidder_id,
            request,
            now_ms,
            reply,
        })
        .await
    }

    pub(crate) async fn reveal_batch_bid(
        &self,
        bidder_id: UserId,
        request: RevealBid,
        now_ms: u64,
    ) -> Result<Result<BidRecord, ReasonCode>, WriterError> {
        self.request(|reply| WriterRequest::RevealBatchBid {
            bidder_id,
            request,
            now_ms,
            reply,
        })
        .await
    }

    pub(crate) async fn settle_batch_auction(
        &self,
        caller_id: UserId,
        request: SettleBatchAuction,
        now_ms: u64,
    ) -> Result<Result<Vec<BatchAuctionFill>, ReasonCode>, WriterError> {
        self.request(|reply| WriterRequest::SettleBatchAuction {
            caller_id,
            request,
            now_ms,
            reply,
        })
        .await
    }

    pub(crate) async fn batch_auctions(
        &self,
        market_id: Option<MarketId>,
    ) -> Result<Vec<BatchAuction>, WriterError> {
        self.request(|reply| WriterRequest::BatchAuctions { market_id, reply })
            .await
    }

    pub(crate) async fn batch_auction(
        &self,
        auction_id: u64,
    ) -> Result<Option<BatchAuction>, WriterError> {
        self.request(|reply| WriterRequest::BatchAuction { auction_id, reply })
            .await
    }

    pub(crate) async fn batch_bids(&self, auction_id: u64) -> Result<Vec<BidRecord>, WriterError> {
        self.request(|reply| WriterRequest::BatchBids { auction_id, reply })
            .await
    }

    pub(crate) async fn batch_asks(&self, auction_id: u64) -> Result<Vec<AskRecord>, WriterError> {
        self.request(|reply| WriterRequest::BatchAsks { auction_id, reply })
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

    // ── Escrow handle methods ──────────────────────────────────────────────

    pub(crate) async fn all_escrows(&self) -> Result<Vec<dkx_escrow::EscrowRecord>, WriterError> {
        self.request(|reply| WriterRequest::AllEscrows { reply })
            .await
    }

    pub(crate) async fn get_escrow(
        &self,
        id: u64,
    ) -> Result<Option<dkx_escrow::EscrowRecord>, WriterError> {
        self.request(|reply| WriterRequest::GetEscrow { id, reply })
            .await
    }

    pub(crate) async fn escrow_accept(
        &self,
        user_id: UserId,
        id: u64,
    ) -> Result<Result<(), ReasonCode>, WriterError> {
        self.request(|reply| WriterRequest::EscrowAccept { user_id, id, reply })
            .await
    }

    pub(crate) async fn escrow_reject(
        &self,
        user_id: UserId,
        id: u64,
        reason: String,
        note: String,
    ) -> Result<Result<(), ReasonCode>, WriterError> {
        self.request(|reply| WriterRequest::EscrowReject {
            user_id,
            id,
            reason,
            note,
            reply,
        })
        .await
    }

    pub(crate) async fn escrow_verify(&self, id: u64) -> Result<bool, WriterError> {
        self.request(|reply| WriterRequest::EscrowVerify { id, reply })
            .await
    }

    pub(crate) async fn all_disputes(&self) -> Result<Vec<DisputeRecord>, WriterError> {
        self.request(|reply| WriterRequest::AllDisputes { reply })
            .await
    }

    pub(crate) async fn admin_rule(
        &self,
        admin_id: UserId,
        id: u64,
        ruling: AdminRuling,
        note: String,
    ) -> Result<Result<(), ReasonCode>, WriterError> {
        self.request(|reply| WriterRequest::AdminRule {
            admin_id,
            id,
            ruling,
            note,
            reply,
        })
        .await
    }

    pub(crate) async fn escrow_public_key(&self) -> Result<String, WriterError> {
        self.request(|reply| WriterRequest::EscrowPublicKey { reply })
            .await
    }

    pub(crate) async fn audit_log(
        &self,
        from_seq: Option<u64>,
        limit: usize,
    ) -> Result<Vec<AuditRecord>, WriterError> {
        self.request(|reply| WriterRequest::AuditLog {
            from_seq,
            limit,
            reply,
        })
        .await
    }

    pub(crate) async fn audit_head(&self) -> Result<Option<AuditRecord>, WriterError> {
        self.request(|reply| WriterRequest::AuditHead { reply })
            .await
    }

    pub(crate) async fn record_audit_note(
        &self,
        kind: u8,
        market_id: u32,
        qty: u32,
        price_paise: i64,
    ) -> Result<AuditRecord, WriterError> {
        self.request(|reply| WriterRequest::RecordAuditNote {
            kind,
            market_id,
            qty,
            price_paise,
            reply,
        })
        .await
    }

    pub(crate) async fn proof_solvency(&self) -> Result<SolvencyRootData, WriterError> {
        self.request(|reply| WriterRequest::ProofSolvency { reply })
            .await
    }

    pub(crate) async fn proof_balance(
        &self,
        user_id: UserId,
    ) -> Result<Option<BalanceProofData>, WriterError> {
        self.request(|reply| WriterRequest::ProofBalance { user_id, reply })
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
    spawn_writer_with_wal(core, tx, recent_trades, capacity, None, Vec::new(), None)
}

pub(crate) fn spawn_writer_with_wal(
    core: Core,
    tx: broadcast::Sender<WsServerMessage>,
    mut recent_trades: Vec<Fill>,
    capacity: usize,
    wal: Option<WalSession>,
    audit_backfill: Vec<AuditRecord>,
    private: Option<broadcast::Sender<PrivateEvent>>,
) -> (WriterHandle, Arc<MetricsState>) {
    let metrics = Arc::new(MetricsState::new(core.last_seq()));
    let (job_tx, job_rx) = std::sync::mpsc::sync_channel(capacity.max(1));
    let writer_metrics = metrics.clone();
    let writer_private = private.clone();
    if recent_trades.len() > RECENT_TRADES_LIMIT {
        let excess = recent_trades.len() - RECENT_TRADES_LIMIT;
        recent_trades.drain(0..excess);
    }

    std::thread::Builder::new()
        .name("dx-core-writer".into())
        .spawn(move || {
            writer_loop(
                core,
                tx,
                recent_trades,
                writer_metrics,
                job_rx,
                wal,
                audit_backfill,
                writer_private,
            )
        })
        .expect("spawn core writer");

    let handle = WriterHandle {
        tx: job_tx,
        metrics: metrics.clone(),
        private,
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

/// One private push: a state change that matters only to `user_id`. The writer
/// publishes these on a second channel; sockets target them by token, so an
/// anonymous viewer never subscribes and a user only ever sees their own
/// events (row 7).
#[derive(Debug, Clone)]
pub(crate) struct PrivateEvent {
    pub user_id: UserId,
    pub message: WsServerMessage,
}

/// Push one private message to its user's sockets. A send error only means no
/// receiver is subscribed right now, which is fine for push.
fn push_private(
    private: &Option<broadcast::Sender<PrivateEvent>>,
    user_id: UserId,
    message: WsServerMessage,
) {
    if let Some(private) = private {
        let _ = private.send(PrivateEvent { user_id, message });
    }
}

/// Fresh balance snapshot for `user_id`, pushed after his ledger changed.
fn push_balances(private: &Option<broadcast::Sender<PrivateEvent>>, core: &Core, user_id: UserId) {
    push_private(
        private,
        user_id,
        WsServerMessage::Balances {
            user_id,
            balances: core.balances(user_id),
        },
    );
}

/// Both parties of a settled escrow hear the new record state and get a
/// balance refresh, because a release refunds the held notional and a refund
/// returns the held cash.
fn push_escrow_settled(
    private: &Option<broadcast::Sender<PrivateEvent>>,
    core: &Core,
    record: &EscrowRecord,
) {
    for user_id in [record.buyer_id, record.seller_id] {
        push_private(
            private,
            user_id,
            WsServerMessage::EscrowUpdate {
                record: record.clone(),
            },
        );
        push_balances(private, core, user_id);
    }
}

/// Same as `push_escrow_settled` for the paths that settle by trade id and do
/// not hold the record: look the canonical record up in the core.
fn push_escrow_settled_by_trade(
    private: &Option<broadcast::Sender<PrivateEvent>>,
    core: &Core,
    trade_id: u64,
) {
    if let Some(record) = core
        .all_escrow_records()
        .into_iter()
        .find(|record| record.trade_id == trade_id)
    {
        push_escrow_settled(private, core, &record);
    }
}

/// Persist one audit record to the log. The audit chain does not change core
/// state, so no post-state hash follows it; recovery re-verifies the chain
/// itself. Without a WAL (`DX_WAL=0`) the chain stays memory-only, which starts
/// from genesis on every restart.
fn persist_audit(wal: &mut Option<WalSession>, record: &AuditRecord) {
    if let Some(wal) = wal
        && let Err(error) = wal.append_audit(record)
    {
        panic!("audit log append failed: {error}");
    }
}

struct PendingCommand {
    response: CommandResponse,
    deltas: Vec<BookDelta>,
    cached: bool,
    wal_op: Option<WalOp>,
    reply: oneshot::Sender<CommandResponse>,
}

#[allow(clippy::too_many_arguments)]
fn process_command(
    core: &mut Core,
    metrics: &MetricsState,
    user_id: UserId,
    command: Command,
    now_ms: u64,
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
            now_ms,
        })
    } else {
        None
    };
    core.set_current_time_ms(now_ms);
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
    first_now_ms: u64,
    first_reply: oneshot::Sender<CommandResponse>,
) -> bool {
    let wal_enabled = wal.is_some();
    let mut pending = vec![process_command(
        core,
        metrics,
        first_user_id,
        first_command,
        first_now_ms,
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
                        now_ms,
                        reply,
                    } => {
                        pending.push(process_command(
                            core,
                            metrics,
                            user_id,
                            command,
                            now_ms,
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

#[allow(clippy::too_many_arguments)]
fn writer_loop(
    mut core: Core,
    tx: broadcast::Sender<WsServerMessage>,
    mut recent_trades: Vec<Fill>,
    metrics: Arc<MetricsState>,
    jobs: Receiver<WriterJob>,
    mut wal: Option<WalSession>,
    audit_backfill: Vec<AuditRecord>,
    private: Option<broadcast::Sender<PrivateEvent>>,
) {
    let mut snapshot_cache = SnapshotCache::default();
    let mut stashed_job: Option<WriterJob> = None;

    // Stage 7 Audit chain and solvency state. A restart seeds the chain from the
    // persisted audit records so the published head continues to grow instead of
    // restarting at sequence 1.
    let mut audit_chain = audit_backfill;
    let mut next_audit_seq: u64 = audit_chain.last().map(|record| record.seq + 1).unwrap_or(1);
    let mut last_audit_hash: [u8; 32] = audit_chain
        .last()
        .map(|record| record.hash)
        .unwrap_or(GENESIS_PREV_HASH);
    let mut user_salts: HashMap<UserId, [u8; 16]> = HashMap::new();
    let mut current_solvency_tree: Option<SolvencyTreeData> = None;
    let mut last_tree_build_time = Instant::now() - Duration::from_secs(40); // Force initial build on start

    let record_audit = |audit_chain: &mut Vec<AuditRecord>,
                        next_audit_seq: &mut u64,
                        last_audit_hash: &mut [u8; 32],
                        kind: u8,
                        market: u32,
                        qty: u32,
                        price: i64| {
        let seq = *next_audit_seq;
        *next_audit_seq += 1;
        let ts_micros = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_micros() as u64)
            .unwrap_or(0);
        let prev = *last_audit_hash;
        let hash = hash_audit_entry(seq, ts_micros, &prev, kind, market, qty, price);
        *last_audit_hash = hash;
        let record = AuditRecord {
            seq,
            ts_micros,
            prev,
            kind,
            market,
            qty,
            price,
            hash,
        };
        audit_chain.push(record.clone());
        record
    };

    // Escrow state: in-memory store populated from fills and driven by commands.
    let mut escrow_store = EscrowStore::new();
    for fill in &recent_trades {
        if let Some(eid) = fill.escrow_id {
            let tick_size = core
                .config(fill.market_id)
                .map(|c| c.tick_size_minor)
                .unwrap_or(100);
            escrow_store.maybe_create_from_fill(fill, eid, tick_size);
        }
    }
    for record in core.all_escrow_records() {
        escrow_store.maybe_create_from_core_record(&record);
    }
    loop {
        // Periodic 30s Merkle tree rebuild
        if last_tree_build_time.elapsed() >= Duration::from_secs(30)
            || current_solvency_tree.is_none()
        {
            current_solvency_tree = Some(build_stage7_tree(&core, &mut user_salts));
            last_tree_build_time = Instant::now();
        }
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
                now_ms,
                reply,
            } => {
                let prev_fills_count = recent_trades.len();
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
                    now_ms,
                    reply,
                );
                // Audit log trades (Kind 1)
                let new_fills_count = recent_trades.len().saturating_sub(prev_fills_count);
                if new_fills_count > 0 {
                    let start_idx = recent_trades.len() - new_fills_count;
                    let mut balance_users: Vec<UserId> = Vec::new();
                    for fill in &recent_trades[start_idx..] {
                        let tick_size = core
                            .config(fill.market_id)
                            .map(|c| c.tick_size_minor)
                            .unwrap_or(100);
                        let price_paise = (fill.price_ticks as i64).saturating_mul(tick_size);
                        let record = record_audit(
                            &mut audit_chain,
                            &mut next_audit_seq,
                            &mut last_audit_hash,
                            1, // Trade
                            fill.market_id,
                            fill.qty as u32,
                            price_paise,
                        );
                        persist_audit(&mut wal, &record);
                        // Private push: each side of a trade hears about it
                        // even in a market that viewer is not subscribed to.
                        push_private(
                            &private,
                            fill.maker_user_id,
                            WsServerMessage::FillSelf { fill: fill.clone() },
                        );
                        push_private(
                            &private,
                            fill.taker_user_id,
                            WsServerMessage::FillSelf { fill: fill.clone() },
                        );
                        if !balance_users.contains(&fill.maker_user_id) {
                            balance_users.push(fill.maker_user_id);
                        }
                        if !balance_users.contains(&fill.taker_user_id) {
                            balance_users.push(fill.taker_user_id);
                        }
                    }
                    // One fresh balance snapshot per side, after all fills settled.
                    for user_id in &balance_users {
                        push_balances(&private, &core, *user_id);
                    }
                }
                // Populate escrow store from any newly escrowed fills
                for fill in recent_trades.iter().rev().take(32) {
                    if let Some(eid) = fill.escrow_id {
                        let tick_size = core
                            .config(fill.market_id)
                            .map(|c| c.tick_size_minor)
                            .unwrap_or(100);
                        escrow_store.maybe_create_from_fill(fill, eid, tick_size);
                    }
                }
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
                    push_balances(&private, &core, user_id);
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
                    push_balances(&private, &core, user_id);
                    let record = record_audit(
                        &mut audit_chain,
                        &mut next_audit_seq,
                        &mut last_audit_hash,
                        2, // Issue
                        market_id,
                        qty as u32,
                        0,
                    );
                    persist_audit(&mut wal, &record);
                }
                let _ = reply.send(result);
            }
            WriterRequest::RegisterPublicKey {
                user_id,
                public_key,
                reply,
            } => {
                let wal_op = WalOp::RegisterPublicKey {
                    user_id,
                    public_key,
                };
                let result = core.register_public_key(user_id, public_key);
                if result.is_ok() {
                    append_wal(&mut wal, &wal_op, core.state_hash());
                }
                let _ = reply.send(result);
            }
            WriterRequest::PublicKey { user_id, reply } => {
                let _ = reply.send(core.public_key(user_id));
            }
            WriterRequest::Escrows { user_id, reply } => {
                let _ = reply.send(core.escrow_records(user_id));
            }
            WriterRequest::ReleaseEscrow {
                actor_user_id,
                trade_id,
                delivered_at_ms,
                signature,
                now_ms,
                reply,
            } => {
                let wal_op = WalOp::ReleaseEscrow {
                    actor_user_id,
                    trade_id,
                    delivered_at_ms,
                    signature: signature.clone(),
                    now_ms,
                };
                let result = core.release_escrow(
                    actor_user_id,
                    trade_id,
                    delivered_at_ms,
                    &signature,
                    now_ms,
                );
                if let Ok(ref record) = result {
                    append_wal(&mut wal, &wal_op, core.state_hash());
                    let price_paise = if record.qty > 0 {
                        record.notional / (record.qty as i64)
                    } else {
                        0
                    };
                    let audit_record = record_audit(
                        &mut audit_chain,
                        &mut next_audit_seq,
                        &mut last_audit_hash,
                        3, // EscrowReleased
                        record.market_id,
                        record.qty as u32,
                        price_paise,
                    );
                    persist_audit(&mut wal, &audit_record);
                    push_escrow_settled(&private, &core, record);
                }
                let _ = reply.send(result);
            }
            WriterRequest::RefundEscrow {
                actor_user_id,
                trade_id,
                now_ms,
                reply,
            } => {
                let wal_op = WalOp::RefundEscrow {
                    actor_user_id,
                    trade_id,
                    now_ms,
                };
                let result = core.refund_escrow(actor_user_id, trade_id, now_ms);
                if let Ok(ref record) = result {
                    append_wal(&mut wal, &wal_op, core.state_hash());
                    let price_paise = if record.qty > 0 {
                        record.notional / (record.qty as i64)
                    } else {
                        0
                    };
                    let audit_record = record_audit(
                        &mut audit_chain,
                        &mut next_audit_seq,
                        &mut last_audit_hash,
                        4, // EscrowRefunded
                        record.market_id,
                        record.qty as u32,
                        price_paise,
                    );
                    persist_audit(&mut wal, &audit_record);
                    push_escrow_settled(&private, &core, record);
                }
                let _ = reply.send(result);
            }
            WriterRequest::CreateAuction {
                seller_id,
                request,
                now_ms,
                reply,
            } => {
                let wal_op = WalOp::CreateDutchAuction {
                    seller_id,
                    request: request.clone(),
                    now_ms,
                };
                let result = core.create_dutch_auction(seller_id, request, now_ms);
                if result.is_ok() {
                    append_wal(&mut wal, &wal_op, core.state_hash());
                }
                let _ = reply.send(result);
            }
            WriterRequest::AcceptAuction {
                buyer_id,
                request,
                now_ms,
                reply,
            } => {
                let wal_op = WalOp::AcceptDutchAuction {
                    buyer_id,
                    request: request.clone(),
                    now_ms,
                };
                let result = core.accept_dutch_auction(buyer_id, request, now_ms);
                if result.is_ok() {
                    append_wal(&mut wal, &wal_op, core.state_hash());
                }
                let _ = reply.send(result);
            }
            WriterRequest::ExpireAuction {
                actor_user_id,
                auction_id,
                now_ms,
                reply,
            } => {
                let wal_op = WalOp::ExpireDutchAuction {
                    actor_user_id,
                    auction_id,
                    now_ms,
                };
                let result = core.expire_dutch_auction(actor_user_id, auction_id, now_ms);
                if result.is_ok() {
                    append_wal(&mut wal, &wal_op, core.state_hash());
                }
                let _ = reply.send(result);
            }
            WriterRequest::Auctions { market_id, reply } => {
                let _ = reply.send(core.dutch_auctions(market_id));
            }
            WriterRequest::Auction { auction_id, reply } => {
                let _ = reply.send(core.dutch_auction(auction_id).cloned());
            }
            WriterRequest::CreateBatchAuction {
                created_by,
                request,
                now_ms,
                reply,
            } => {
                let wal_op = WalOp::CreateBatchAuction {
                    created_by,
                    request: request.clone(),
                    now_ms,
                };
                let result = core.create_batch_auction(created_by, request, now_ms);
                if result.is_ok() {
                    append_wal(&mut wal, &wal_op, core.state_hash());
                }
                let _ = reply.send(result);
            }
            WriterRequest::CommitBatchBid {
                bidder_id,
                request,
                now_ms,
                reply,
            } => {
                let wal_op = WalOp::CommitBatchBid {
                    bidder_id,
                    request: request.clone(),
                    now_ms,
                };
                let result = core.commit_batch_bid(bidder_id, request, now_ms);
                if result.is_ok() {
                    append_wal(&mut wal, &wal_op, core.state_hash());
                }
                let _ = reply.send(result);
            }
            WriterRequest::RevealBatchBid {
                bidder_id,
                request,
                now_ms,
                reply,
            } => {
                let wal_op = WalOp::RevealBatchBid {
                    bidder_id,
                    request: request.clone(),
                    now_ms,
                };
                let result = core.reveal_batch_bid(bidder_id, request, now_ms);
                if result.is_ok() {
                    append_wal(&mut wal, &wal_op, core.state_hash());
                }
                let _ = reply.send(result);
            }
            WriterRequest::CommitBatchAsk {
                seller_id,
                request,
                now_ms,
                reply,
            } => {
                let wal_op = WalOp::CommitBatchAsk {
                    seller_id,
                    request: request.clone(),
                    now_ms,
                };
                let result = core.commit_batch_ask(seller_id, request, now_ms);
                if result.is_ok() {
                    append_wal(&mut wal, &wal_op, core.state_hash());
                }
                let _ = reply.send(result);
            }
            WriterRequest::RevealBatchAsk {
                seller_id,
                request,
                now_ms,
                reply,
            } => {
                let wal_op = WalOp::RevealBatchAsk {
                    seller_id,
                    request: request.clone(),
                    now_ms,
                };
                let result = core.reveal_batch_ask(seller_id, request, now_ms);
                if result.is_ok() {
                    append_wal(&mut wal, &wal_op, core.state_hash());
                }
                let _ = reply.send(result);
            }
            WriterRequest::SettleBatchAuction {
                caller_id,
                request,
                now_ms,
                reply,
            } => {
                let wal_op = WalOp::SettleBatchAuction {
                    caller_id,
                    request: request.clone(),
                    now_ms,
                };
                let result = core.settle_batch_auction(caller_id, request, now_ms);
                if result.is_ok() {
                    append_wal(&mut wal, &wal_op, core.state_hash());
                }
                let _ = reply.send(result);
            }
            WriterRequest::BatchAuctions { market_id, reply } => {
                let _ = reply.send(core.batch_auctions(market_id));
            }
            WriterRequest::BatchAuction { auction_id, reply } => {
                let _ = reply.send(core.batch_auction(auction_id).cloned());
            }
            WriterRequest::BatchBids { auction_id, reply } => {
                let _ = reply.send(core.batch_bids(auction_id));
            }
            WriterRequest::BatchAsks { auction_id, reply } => {
                let _ = reply.send(core.batch_asks(auction_id));
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
            WriterRequest::AllEscrows { reply } => {
                let items = escrow_store.all();
                let mut records = Vec::new();
                for (record, auto_released) in items {
                    if auto_released {
                        let wal_op = WalOp::AdminSettleEscrow {
                            trade_id: record.trade_id,
                            refund_to_buyer: false,
                        };
                        let settle_res = core.admin_settle_escrow(record.trade_id, false);
                        if settle_res.is_ok() {
                            append_wal(&mut wal, &wal_op, core.state_hash());
                            push_escrow_settled_by_trade(&private, &core, record.trade_id);
                        }
                    }
                    records.push(record);
                }
                let _ = reply.send(records);
            }
            WriterRequest::GetEscrow { id, reply } => {
                let res = escrow_store.get(id).map(|(record, auto_released)| {
                    if auto_released {
                        let wal_op = WalOp::AdminSettleEscrow {
                            trade_id: record.trade_id,
                            refund_to_buyer: false,
                        };
                        let settle_res = core.admin_settle_escrow(record.trade_id, false);
                        if settle_res.is_ok() {
                            append_wal(&mut wal, &wal_op, core.state_hash());
                            push_escrow_settled_by_trade(&private, &core, record.trade_id);
                        }
                    }
                    record
                });
                let _ = reply.send(res);
            }
            WriterRequest::EscrowAccept { user_id, id, reply } => {
                let result = escrow_store.accept(user_id, id);
                let res = match result {
                    Ok(()) => {
                        let wal_op = WalOp::AdminSettleEscrow {
                            trade_id: id,
                            refund_to_buyer: false,
                        };
                        let settle_res = core.admin_settle_escrow(id, false);
                        if settle_res.is_ok() {
                            append_wal(&mut wal, &wal_op, core.state_hash());
                            if let Some((record, _)) = escrow_store.get(id) {
                                push_escrow_settled_by_trade(&private, &core, record.trade_id);
                            }
                        }
                        settle_res.map(|_| ())
                    }
                    Err(e) => Err(e),
                };
                let _ = reply.send(res);
            }
            WriterRequest::EscrowReject {
                user_id,
                id,
                reason,
                note,
                reply,
            } => {
                let result = escrow_store.reject(user_id, id, reason, note);
                let _ = reply.send(result);
            }
            WriterRequest::EscrowVerify { id, reply } => {
                let valid = escrow_store.verify(id);
                let _ = reply.send(valid);
            }
            WriterRequest::AllDisputes { reply } => {
                let _ = reply.send(escrow_store.all_disputes());
            }
            WriterRequest::AdminRule {
                admin_id,
                id,
                ruling,
                note,
                reply,
            } => {
                let result = escrow_store.admin_rule(admin_id, id, ruling, note);
                let res = match result {
                    Ok(escrow_id) => {
                        let refund_to_buyer = ruling == dkx_escrow::AdminRuling::Buyer;
                        let wal_op = WalOp::AdminSettleEscrow {
                            trade_id: escrow_id,
                            refund_to_buyer,
                        };
                        let settle_res = core.admin_settle_escrow(escrow_id, refund_to_buyer);
                        if settle_res.is_ok() {
                            append_wal(&mut wal, &wal_op, core.state_hash());
                            if let Some((record, _)) = escrow_store.get(escrow_id) {
                                push_escrow_settled_by_trade(&private, &core, record.trade_id);
                            }
                        }
                        settle_res.map(|_| ())
                    }
                    Err(e) => Err(e),
                };
                let _ = reply.send(res);
            }
            WriterRequest::EscrowPublicKey { reply } => {
                let _ = reply.send(escrow_store.public_key_hex());
            }
            WriterRequest::AuditLog {
                from_seq,
                limit,
                reply,
            } => {
                let limit = limit.clamp(1, 200);
                let start_seq = from_seq.unwrap_or(1);
                let records: Vec<AuditRecord> = audit_chain
                    .iter()
                    .filter(|r| r.seq >= start_seq)
                    .take(limit)
                    .cloned()
                    .collect();
                let _ = reply.send(records);
            }
            WriterRequest::AuditHead { reply } => {
                let _ = reply.send(audit_chain.last().cloned());
            }
            WriterRequest::RecordAuditNote {
                kind,
                market_id,
                qty,
                price_paise,
                reply,
            } => {
                let audit_record = record_audit(
                    &mut audit_chain,
                    &mut next_audit_seq,
                    &mut last_audit_hash,
                    kind,
                    market_id,
                    qty,
                    price_paise,
                );
                persist_audit(&mut wal, &audit_record);
                let _ = reply.send(audit_record);
            }
            WriterRequest::ProofSolvency { reply } => {
                let root_data = if let Some(ref tree) = current_solvency_tree {
                    SolvencyRootData {
                        seq: tree.seq,
                        root_hash_hex: hex::encode(tree.root_hash),
                        total_paise: tree.total_paise,
                        leaf_count: tree.leaves.len(),
                        published_at: tree.published_at,
                    }
                } else {
                    SolvencyRootData {
                        seq: 0,
                        root_hash_hex: hex::encode([0u8; 32]),
                        total_paise: 0,
                        leaf_count: 0,
                        published_at: 0,
                    }
                };
                let _ = reply.send(root_data);
            }
            WriterRequest::ProofBalance { user_id, reply } => {
                let proof_data = current_solvency_tree
                    .as_ref()
                    .and_then(|tree| tree.proof(user_id));
                let _ = reply.send(proof_data);
            }
        }
    }
}

// ─── Escrow in-memory store ──────────────────────────────────────────────────

struct EscrowStore {
    escrows: std::collections::HashMap<u64, dkx_escrow::EscrowRecord>,
    disputes: std::collections::HashMap<u64, dkx_escrow::DisputeRecord>,
    next_dispute_id: u64,
    signing_key: Option<ed25519_dalek::SigningKey>,
    verifying_key: Option<ed25519_dalek::VerifyingKey>,
    inspection_secs: u64,
}

impl EscrowStore {
    fn new() -> Self {
        let signing_key = dkx_escrow::load_signing_key_from_env()
            .ok()
            .or_else(|| Some(ed25519_dalek::SigningKey::from_bytes(&[42u8; 32])));
        let verifying_key = signing_key.as_ref().map(|sk| sk.verifying_key());
        let inspection_secs = dkx_escrow::parse_inspection_secs_from_env().unwrap_or(300);
        Self {
            escrows: std::collections::HashMap::new(),
            disputes: std::collections::HashMap::new(),
            next_dispute_id: 1,
            signing_key,
            verifying_key,
            inspection_secs,
        }
    }

    fn public_key_hex(&self) -> String {
        self.verifying_key
            .as_ref()
            .map(|vk| hex::encode(vk.to_bytes()))
            .unwrap_or_default()
    }

    fn maybe_create_from_fill(&mut self, fill: &Fill, escrow_id: u64, tick_size_minor: i64) {
        if self.escrows.contains_key(&escrow_id) {
            return;
        }
        let now_micros = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_micros() as u64)
            .unwrap_or(0);
        let delivered_at = now_micros;
        let deadline = delivered_at.saturating_add(self.inspection_secs.saturating_mul(1_000_000));
        let (buyer_id, seller_id) = if fill.maker_side == dx_types::Side::Sell {
            (fill.taker_user_id, fill.maker_user_id)
        } else {
            (fill.maker_user_id, fill.taker_user_id)
        };
        let price_minor = (fill.price_ticks as i64).saturating_mul(tick_size_minor);
        let receipt_bytes = dkx_escrow::build_receipt_bytes(
            escrow_id,
            fill.trade_id,
            fill.market_id,
            buyer_id,
            seller_id,
            fill.qty as u32,
            price_minor,
            delivered_at,
        );
        let receipt_sig = self
            .signing_key
            .as_ref()
            .map(|sk| dkx_escrow::sign_receipt(sk, &receipt_bytes))
            .unwrap_or_default();
        self.escrows.insert(
            escrow_id,
            dkx_escrow::EscrowRecord {
                id: escrow_id,
                trade_id: fill.trade_id,
                market_id: fill.market_id,
                buyer_id,
                seller_id,
                qty: fill.qty as u32,
                price: price_minor,
                fee: 0,
                state: dkx_escrow::EscrowState::Delivered,
                delivered_at,
                deadline,
                receipt_bytes,
                receipt_sig,
                updated_seq: fill.seq,
            },
        );
    }

    fn maybe_create_from_core_record(&mut self, record: &dx_types::EscrowRecord) {
        let escrow_id = record.trade_id;
        if self.escrows.contains_key(&escrow_id) {
            return;
        }
        let now_micros = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_micros() as u64)
            .unwrap_or(0);
        let delivered_at = if let Some(delivered_at_ms) = record.delivered_at_ms {
            delivered_at_ms.saturating_mul(1_000)
        } else {
            record.created_at_ms.saturating_mul(1_000)
        };
        let deadline = record.deadline_ms.saturating_mul(1_000).max(now_micros);
        let price_minor = if record.qty > 0 {
            record.notional / (record.qty as i64)
        } else {
            0
        };
        let receipt_bytes = dkx_escrow::build_receipt_bytes(
            escrow_id,
            record.trade_id,
            record.market_id,
            record.buyer_id,
            record.seller_id,
            record.qty as u32,
            price_minor,
            delivered_at,
        );
        let receipt_sig = self
            .signing_key
            .as_ref()
            .map(|sk| dkx_escrow::sign_receipt(sk, &receipt_bytes))
            .unwrap_or_default();
        let state = match record.state {
            dx_types::EscrowState::PendingDelivery => {
                if now_micros >= deadline {
                    dkx_escrow::EscrowState::Released
                } else {
                    dkx_escrow::EscrowState::Delivered
                }
            }
            dx_types::EscrowState::Delivered => dkx_escrow::EscrowState::Released,
            dx_types::EscrowState::Refunded => dkx_escrow::EscrowState::Refunded,
        };
        self.escrows.insert(
            escrow_id,
            dkx_escrow::EscrowRecord {
                id: escrow_id,
                trade_id: record.trade_id,
                market_id: record.market_id,
                buyer_id: record.buyer_id,
                seller_id: record.seller_id,
                qty: record.qty as u32,
                price: price_minor,
                fee: record.fee,
                state,
                delivered_at,
                deadline,
                receipt_bytes,
                receipt_sig,
                updated_seq: 0,
            },
        );
    }

    fn all(&mut self) -> Vec<(dkx_escrow::EscrowRecord, bool)> {
        let now_micros = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_micros() as u64)
            .unwrap_or(0);
        let mut results = Vec::new();
        for e in self.escrows.values_mut() {
            let mut auto_released = false;
            if e.state == dkx_escrow::EscrowState::Delivered && now_micros >= e.deadline {
                e.state = dkx_escrow::EscrowState::Released;
                auto_released = true;
            }
            results.push((e.clone(), auto_released));
        }
        results
    }
    fn get(&mut self, id: u64) -> Option<(dkx_escrow::EscrowRecord, bool)> {
        let now_micros = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_micros() as u64)
            .unwrap_or(0);
        let e = self.escrows.get_mut(&id)?;
        let mut auto_released = false;
        if e.state == dkx_escrow::EscrowState::Delivered && now_micros >= e.deadline {
            e.state = dkx_escrow::EscrowState::Released;
            auto_released = true;
        }
        Some((e.clone(), auto_released))
    }

    fn accept(&mut self, user_id: UserId, id: u64) -> Result<(), ReasonCode> {
        let e = self.escrows.get_mut(&id).ok_or(ReasonCode::OrderNotFound)?;
        if e.buyer_id != user_id {
            return Err(ReasonCode::NotOrderOwner);
        }
        let now_micros = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_micros() as u64)
            .unwrap_or(0);
        if e.state != dkx_escrow::EscrowState::Delivered || now_micros >= e.deadline {
            return Err(ReasonCode::EscrowNotReady);
        }
        e.state = dkx_escrow::EscrowState::Released;
        Ok(())
    }

    fn reject(
        &mut self,
        user_id: UserId,
        id: u64,
        reason: String,
        note: String,
    ) -> Result<(), ReasonCode> {
        let e = self.escrows.get_mut(&id).ok_or(ReasonCode::OrderNotFound)?;
        if e.buyer_id != user_id {
            return Err(ReasonCode::NotOrderOwner);
        }
        let now_micros = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_micros() as u64)
            .unwrap_or(0);
        if e.state != dkx_escrow::EscrowState::Delivered || now_micros >= e.deadline {
            return Err(ReasonCode::EscrowNotReady);
        }
        e.state = dkx_escrow::EscrowState::Disputed;
        let did = self.next_dispute_id;
        self.next_dispute_id += 1;
        self.disputes.insert(
            id,
            dkx_escrow::DisputeRecord {
                id: did,
                escrow_id: id,
                reason,
                note,
                status: "open".into(),
                ruling: None,
                ruled_by: None,
                ruled_at: None,
            },
        );
        Ok(())
    }

    fn verify(&self, id: u64) -> bool {
        let Some(e) = self.escrows.get(&id) else {
            return false;
        };
        let Some(vk) = &self.verifying_key else {
            return !e.receipt_sig.is_empty();
        };
        dkx_escrow::verify_receipt(vk, &e.receipt_bytes, &e.receipt_sig).unwrap_or(false)
    }

    fn all_disputes(&self) -> Vec<dkx_escrow::DisputeRecord> {
        self.disputes.values().cloned().collect()
    }

    fn admin_rule(
        &mut self,
        admin_id: UserId,
        id: u64,
        ruling: dkx_escrow::AdminRuling,
        note: String,
    ) -> Result<u64, ReasonCode> {
        // `id` is primarily the dispute_id from /api/admin/disputes/{id}/rule,
        // or optionally the escrow_id. Check dispute_id first to avoid collision.
        let escrow_id = if let Some(d) = self.disputes.values().find(|d| d.id == id) {
            d.escrow_id
        } else if self.escrows.contains_key(&id) {
            id
        } else {
            return Err(ReasonCode::OrderNotFound);
        };

        let e = self
            .escrows
            .get_mut(&escrow_id)
            .ok_or(ReasonCode::OrderNotFound)?;
        if e.state != dkx_escrow::EscrowState::Disputed {
            return Err(ReasonCode::EscrowNotReady);
        }
        e.state = match ruling {
            dkx_escrow::AdminRuling::Buyer => dkx_escrow::EscrowState::Refunded,
            dkx_escrow::AdminRuling::Seller => dkx_escrow::EscrowState::Released,
        };
        if let Some(d) = self
            .disputes
            .values_mut()
            .find(|d| d.escrow_id == escrow_id)
        {
            d.status = "resolved".into();
            d.ruling = Some(ruling);
            d.ruled_by = Some(admin_id);
            d.ruled_at = Some(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|t| t.as_micros() as u64)
                    .unwrap_or(0),
            );
            if !note.is_empty() {
                d.note = format!("{}\nAdmin: {}", d.note, note);
            }
        }
        Ok(escrow_id)
    }
}

// ─── Stage 7 Solvency Merkle-Sum Tree ───────────────────────────────────────

#[derive(Clone, Debug)]
struct Stage7Leaf {
    user_id: UserId,
    salt: [u8; 16],
    balance_paise: u64,
}

#[derive(Clone, Debug)]
struct SolvencyTreeData {
    seq: u64,
    total_paise: u64,
    root_hash: [u8; 32],
    leaves: Vec<Stage7Leaf>,
    levels: Vec<Vec<dkx_hash::MerkleNode>>,
    published_at: u64,
}

impl SolvencyTreeData {
    fn proof(&self, user_id: UserId) -> Option<BalanceProofData> {
        let leaf_idx = self
            .leaves
            .binary_search_by_key(&user_id, |l| l.user_id)
            .ok()?;
        let leaf = &self.leaves[leaf_idx];

        let mut path = Vec::new();
        let height = self.levels.len().saturating_sub(1);
        for level in 0..height {
            let idx = leaf_idx >> level;
            let sibling_idx = idx ^ 1;
            let (sibling_hash, sibling_sum, is_left) = match self.levels[level].get(sibling_idx) {
                Some(sib) => (sib.hash, sib.sum, sibling_idx < idx),
                None => ([0u8; 32], 0u64, false), // Empty node on right
            };
            path.push(PathStepData {
                hash_hex: hex::encode(sibling_hash),
                sum_paise: sibling_sum,
                is_left,
            });
        }

        Some(BalanceProofData {
            leaf: BalanceLeafData {
                user_id: leaf.user_id,
                balance_paise: leaf.balance_paise,
                salt_hex: hex::encode(leaf.salt),
            },
            path,
            root_hash_hex: hex::encode(self.root_hash),
            total_paise: self.total_paise,
            seq: self.seq,
        })
    }
}

fn build_stage7_tree(core: &Core, user_salts: &mut HashMap<UserId, [u8; 16]>) -> SolvencyTreeData {
    let mut leaves: Vec<Stage7Leaf> = Vec::new();

    let all_balances = core.solvency_balances().unwrap_or_default();
    let mut cash_by_user: HashMap<UserId, u64> = HashMap::new();
    for (user_id, asset, balance) in all_balances {
        if asset == Asset::Cash {
            *cash_by_user.entry(user_id).or_insert(0) += balance;
        }
    }

    // Include all known users with salts or cash
    for uid in user_salts.keys() {
        cash_by_user.entry(*uid).or_insert(0);
    }

    for (user_id, balance_paise) in cash_by_user {
        let salt = *user_salts.entry(user_id).or_insert_with(|| {
            let mut s = [0u8; 16];
            let rand_val = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(1);
            let mut hasher = Sha256::new();
            hasher.update(b"DKX-USER-SALT-v1");
            hasher.update(user_id.to_be_bytes());
            hasher.update(rand_val.to_be_bytes());
            let digest = hasher.finalize();
            s.copy_from_slice(&digest[..16]);
            s
        });

        leaves.push(Stage7Leaf {
            user_id,
            salt,
            balance_paise,
        });
    }

    leaves.sort_by_key(|l| l.user_id);

    let published_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let seq = core.last_seq();

    if leaves.is_empty() {
        return SolvencyTreeData {
            seq,
            total_paise: 0,
            root_hash: [0u8; 32],
            leaves,
            levels: vec![vec![dkx_hash::MerkleNode {
                hash: [0u8; 32],
                sum: 0,
            }]],
            published_at,
        };
    }

    let mut current_level: Vec<dkx_hash::MerkleNode> = leaves
        .iter()
        .map(|leaf| dkx_hash::MerkleNode {
            hash: hash_merkle_leaf(&leaf.salt, leaf.user_id, leaf.balance_paise),
            sum: leaf.balance_paise,
        })
        .collect();

    let mut levels = vec![current_level.clone()];

    while current_level.len() > 1 {
        let mut next_level = Vec::with_capacity(current_level.len().div_ceil(2));
        for chunk in current_level.chunks(2) {
            let node = if chunk.len() == 2 {
                hash_merkle_node(&chunk[0].hash, chunk[0].sum, &chunk[1].hash, chunk[1].sum)
                    .unwrap_or(dkx_hash::MerkleNode {
                        hash: [0u8; 32],
                        sum: chunk[0].sum.saturating_add(chunk[1].sum),
                    })
            } else {
                pair_with_empty(&chunk[0]).unwrap_or(dkx_hash::MerkleNode {
                    hash: [0u8; 32],
                    sum: chunk[0].sum,
                })
            };
            next_level.push(node);
        }
        levels.push(next_level.clone());
        current_level = next_level;
    }

    let root_node = levels
        .last()
        .and_then(|lvl| lvl.first())
        .cloned()
        .unwrap_or(dkx_hash::MerkleNode {
            hash: [0u8; 32],
            sum: 0,
        });

    SolvencyTreeData {
        seq,
        total_paise: root_node.sum,
        root_hash: root_node.hash,
        leaves,
        levels,
        published_at,
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
    use dx_types::{
        AcceptDutchAuction, AuctionStatus, CreateDutchAuction, PlaceLimit, PlaceMarket, Side,
        seed_markets,
    };
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
            private: None,
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
                    escrow: false,
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
        let (writer, _metrics) = spawn_writer_with_wal(
            funded_core(),
            tx,
            Vec::new(),
            16,
            Some(wal),
            Vec::new(),
            None,
        );

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
                    escrow: false,
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
                    escrow: false,
                }),
            )
            .await
            .unwrap();

        let expected_hash = writer.state_hash().await.unwrap();
        let expected_solvency_seq = writer.solvency(vec![]).await.unwrap().seq;
        // Two audit records so far: the issuance (kind 2) and the trade (kind 1).
        let expected_audit = writer.audit_log(None, 200).await.unwrap();
        writer.shutdown().await.unwrap();
        drop(writer);

        let recovered = dkx_wal::recover(dir.path()).unwrap();
        let mut restored = funded_core();
        let mut replayed_audit = Vec::new();
        let fills = crate::wal::replay_with_audit(
            &recovered.replayed_records,
            &mut restored,
            &mut replayed_audit,
        )
        .unwrap();
        assert_eq!(restored.state_hash(), expected_hash);
        assert_eq!(
            build_solvency_snapshot(&restored, &[]).unwrap().seq,
            expected_solvency_seq
        );
        assert_eq!(fills.len(), 1);
        assert_eq!(restored.balance(3, Asset::Cash).available, 1_000_000);
        // The persisted chain reproduces exactly the chain that was served
        // before the shutdown, so a restart does not reset the published log.
        assert_eq!(replayed_audit, expected_audit);
        assert_eq!(
            replayed_audit.iter().map(|r| r.kind).collect::<Vec<_>>(),
            [2, 1]
        );
        assert_eq!(replayed_audit.first().unwrap().prev, GENESIS_PREV_HASH);
    }

    #[tokio::test]
    async fn audit_chain_continues_across_a_writer_restart() {
        let dir = tempdir().unwrap();
        let initial_recovered = dkx_wal::recover(dir.path()).unwrap();
        let wal = WalSession::open(
            dir.path(),
            initial_recovered.last_seq,
            initial_recovered.last_hash,
        )
        .unwrap();
        let (tx1, _rx1) = broadcast::channel(16);
        let (writer, _metrics) = spawn_writer_with_wal(
            funded_core(),
            tx1,
            Vec::new(),
            16,
            Some(wal),
            Vec::new(),
            None,
        );

        writer.issue(2, MARKET, 2, 7).await.unwrap().unwrap();
        let expected_head = writer.audit_head().await.unwrap().unwrap();
        assert_eq!(expected_head.seq, 1);
        writer.shutdown().await.unwrap();
        drop(writer);

        // Restart: rebuild the core and the audit chain from the log.
        let recovered = dkx_wal::recover(dir.path()).unwrap();
        let mut restored = funded_core();
        let mut audit = Vec::new();
        let fills =
            crate::wal::replay_with_audit(&recovered.replayed_records, &mut restored, &mut audit)
                .unwrap();
        assert_eq!(audit.last().unwrap().hash, expected_head.hash);

        let wal = WalSession::open(dir.path(), recovered.last_seq, recovered.last_hash).unwrap();
        let (tx2, _rx2) = broadcast::channel(16);
        let (writer, _metrics) =
            spawn_writer_with_wal(restored, tx2, fills, 16, Some(wal), audit, None);

        // New records continue the previous head, not a fresh sequence.
        writer.issue(2, MARKET, 1, 77).await.unwrap().unwrap();
        let next_head = writer.audit_head().await.unwrap().unwrap();
        assert_eq!(next_head.seq, expected_head.seq + 1);
        assert_eq!(next_head.prev, expected_head.hash);
        let log = writer.audit_log(None, 200).await.unwrap();
        assert_eq!(log.len(), 2);
        assert_eq!(log.first().unwrap().hash, expected_head.hash);
        writer.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn auctions_round_trip_through_the_writer() {
        let core = funded_core();
        let (tx, _rx) = broadcast::channel(16);
        let (writer, _metrics) = spawn_writer(core, tx, Vec::new(), 8);

        let created = writer
            .create_auction(
                2,
                CreateDutchAuction {
                    client_order_id: 1,
                    market_id: MARKET,
                    qty: 2,
                    start_price_ticks: 5_000,
                    floor_price_ticks: 2_500,
                    duration_ms: 10_000,
                },
                1_000,
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(created.qty_remaining, 2);
        assert_eq!(writer.auctions(Some(MARKET)).await.unwrap().len(), 1);

        let accepted = writer
            .accept_auction(
                1,
                AcceptDutchAuction {
                    client_order_id: 2,
                    auction_id: created.id,
                    qty: 2,
                    max_price_ticks: 5_000,
                },
                6_000,
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(accepted.price_ticks, 3_750);
        assert_eq!(accepted.notional, 750_000);
        assert_eq!(
            writer.auction(created.id).await.unwrap().unwrap().status,
            AuctionStatus::Sold
        );
        assert_eq!(
            writer
                .auctions(None)
                .await
                .unwrap()
                .first()
                .unwrap()
                .qty_remaining,
            0
        );
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
                escrow: false,
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
                escrow: false,
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
                escrow: false,
            }),
        );
        assert_eq!(buy.status, OrderStatus::Filled);
        cache.invalidate(MARKET);

        let after = cache.snapshot(&core, MARKET, 20).unwrap();
        assert_eq!(after.last_trade_price, Some(5_000));
        assert_ne!(before.book_seq, after.book_seq);
        assert_eq!(Some(after), core.snapshot(MARKET, 20));
    }

    /// Drain one private event and destructure it for assertions.
    async fn next_private(rx: &mut broadcast::Receiver<PrivateEvent>) -> (UserId, WsServerMessage) {
        let event = rx.recv().await.unwrap();
        (event.user_id, event.message)
    }

    /// Take everything published so far without waiting.
    fn drain_ready(rx: &mut broadcast::Receiver<PrivateEvent>) -> Vec<(UserId, WsServerMessage)> {
        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push((event.user_id, event.message));
        }
        events
    }

    #[tokio::test]
    async fn ledger_changes_push_balances_to_their_user() {
        let (tx, _rx) = broadcast::channel(16);
        let (private_tx, mut private_rx) = broadcast::channel::<PrivateEvent>(64);
        let (writer, _metrics) = spawn_writer_with_wal(
            funded_core(),
            tx,
            Vec::new(),
            8,
            None,
            Vec::new(),
            Some(private_tx),
        );

        writer.deposit(3, 1_000_000, 7).await.unwrap().unwrap();
        let (user_id, message) = next_private(&mut private_rx).await;
        assert_eq!(user_id, 3);
        assert!(matches!(message, WsServerMessage::Balances { .. }));

        // The recipient is pushed, not the calling issuer.
        writer.issue(2, MARKET, 1, 8).await.unwrap().unwrap();
        let (user_id, message) = next_private(&mut private_rx).await;
        assert_eq!(user_id, 2);
        assert!(matches!(message, WsServerMessage::Balances { .. }));
    }

    #[tokio::test]
    async fn a_trade_pushes_fills_and_balances_to_both_sides() {
        let (tx, _rx) = broadcast::channel(16);
        let (private_tx, mut private_rx) = broadcast::channel::<PrivateEvent>(64);
        let (writer, _metrics) = spawn_writer_with_wal(
            funded_core(),
            tx,
            Vec::new(),
            8,
            None,
            Vec::new(),
            Some(private_tx),
        );

        writer
            .command(
                2,
                Command::PlaceLimit(PlaceLimit {
                    client_order_id: 1,
                    market_id: MARKET,
                    side: Side::Sell,
                    price_ticks: 5_000,
                    qty: 2,
                    escrow: false,
                }),
            )
            .await
            .unwrap();
        let response = writer
            .command(
                1,
                Command::PlaceMarket(PlaceMarket {
                    client_order_id: 2,
                    market_id: MARKET,
                    side: Side::Buy,
                    qty: 2,
                    protect_price_ticks: 5_000,
                    escrow: false,
                }),
            )
            .await
            .unwrap();
        assert_eq!(response.status, OrderStatus::Filled);

        // Each side hears about its own fill even if it is not watching this
        // market's public feed.
        let (maker, message) = next_private(&mut private_rx).await;
        assert_eq!(maker, 2);
        let WsServerMessage::FillSelf { fill } = &message else {
            panic!("expected a private fill, got {message:?}");
        };
        assert_eq!(fill.maker_user_id, 2);
        assert_eq!(fill.taker_user_id, 1);
        assert_eq!(fill.qty, 2);
        let (taker, message) = next_private(&mut private_rx).await;
        assert_eq!(taker, 1);
        assert!(matches!(message, WsServerMessage::FillSelf { .. }));

        // Then one balance refresh per side, after the fill settled.
        let (user_id, message) = next_private(&mut private_rx).await;
        assert_eq!(user_id, 2);
        assert!(matches!(message, WsServerMessage::Balances { .. }));
        let (user_id, message) = next_private(&mut private_rx).await;
        assert_eq!(user_id, 1);
        assert!(matches!(message, WsServerMessage::Balances { .. }));
    }

    #[tokio::test]
    async fn escrow_settlement_pushes_to_both_parties() {
        let (tx, _rx) = broadcast::channel(16);
        let (private_tx, mut private_rx) = broadcast::channel::<PrivateEvent>(64);
        let (writer, _metrics) = spawn_writer_with_wal(
            funded_core(),
            tx,
            Vec::new(),
            8,
            None,
            Vec::new(),
            Some(private_tx),
        );

        // Escrowed fills need an escrow-enabled market and a seller key.
        const ESCROW_MARKET: MarketId = 3;
        let seller_key = ed25519_dalek::SigningKey::from_bytes(&[7u8; 32])
            .verifying_key()
            .to_bytes();
        writer
            .register_public_key(2, seller_key)
            .await
            .unwrap()
            .unwrap();
        writer.deposit(2, 100_000_000, 11).await.unwrap().unwrap();
        writer.deposit(1, 100_000_000, 12).await.unwrap().unwrap();
        writer
            .issue(2, ESCROW_MARKET, 1, 13)
            .await
            .unwrap()
            .unwrap();
        // Clear the setup pushes so the fill events can be asserted in order.
        assert_eq!(drain_ready(&mut private_rx).len(), 3);

        writer
            .command(
                2,
                Command::PlaceLimit(PlaceLimit {
                    client_order_id: 1,
                    market_id: ESCROW_MARKET,
                    side: Side::Sell,
                    price_ticks: 4_000,
                    qty: 1,
                    escrow: true,
                }),
            )
            .await
            .unwrap();
        let response = writer
            .command(
                1,
                Command::PlaceMarket(PlaceMarket {
                    client_order_id: 2,
                    market_id: ESCROW_MARKET,
                    side: Side::Buy,
                    qty: 1,
                    protect_price_ticks: 4_000,
                    escrow: true,
                }),
            )
            .await
            .unwrap();
        let OrderStatus::Filled = response.status else {
            panic!("expected an escrow fill, got {:?}", response.status);
        };

        // Each side hears its own fill, and it carries the escrow id.
        let (maker, message) = next_private(&mut private_rx).await;
        assert_eq!(maker, 2);
        let WsServerMessage::FillSelf { fill } = &message else {
            panic!("expected a private fill, got {message:?}");
        };
        assert_eq!(fill.escrow_id, Some(1));
        assert_eq!(fill.taker_user_id, 1);
        let (taker, message) = next_private(&mut private_rx).await;
        assert_eq!(taker, 1);
        assert!(matches!(message, WsServerMessage::FillSelf { .. }));
        let (user_id, message) = next_private(&mut private_rx).await;
        assert_eq!(user_id, 2);
        assert!(matches!(message, WsServerMessage::Balances { .. }));
        let (user_id, message) = next_private(&mut private_rx).await;
        assert_eq!(user_id, 1);
        assert!(matches!(message, WsServerMessage::Balances { .. }));

        // No settle pushes exist yet.
        assert!(drain_ready(&mut private_rx).is_empty());

        // The buyer disputes the delivery, the admin rules for the seller.
        let escrow = writer
            .escrows(1)
            .await
            .unwrap()
            .into_iter()
            .find(|record| record.market_id == ESCROW_MARKET)
            .expect("escrow created for the fill");
        writer
            .escrow_reject(1, escrow.trade_id, "late delivery".into(), "".into())
            .await
            .unwrap()
            .unwrap();
        writer
            .admin_rule(
                3,
                escrow.trade_id,
                dkx_escrow::AdminRuling::Seller,
                "ok".into(),
            )
            .await
            .unwrap()
            .unwrap();

        // Both parties learn the ruling and get a balance refresh, buyer first.
        for user_id in [1, 2] {
            let (pushed, message) = next_private(&mut private_rx).await;
            assert_eq!(pushed, user_id);
            let WsServerMessage::EscrowUpdate { record } = &message else {
                panic!("expected an escrow push, got {message:?}");
            };
            assert_eq!(record.trade_id, escrow.trade_id);
            let (pushed, message) = next_private(&mut private_rx).await;
            assert_eq!(pushed, user_id);
            assert!(matches!(message, WsServerMessage::Balances { .. }));
        }
        assert!(drain_ready(&mut private_rx).is_empty());
    }
}
