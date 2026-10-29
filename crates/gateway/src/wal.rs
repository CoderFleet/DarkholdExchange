//! Replayable operations for the gateway WAL.
//!
//! State changes are stored as operation records. A full core-state hash checkpoint
//! follows a batch at least every 256 operations. Recovery replays the operations,
//! then checks each checkpoint. That catches a missing record or a non-deterministic
//! replay at the next checkpoint boundary.

use std::path::Path;

use dkx_hash::AuditRecord;
use dkx_wal::{LogRecord, Wal};
use dx_core::Core;
use dx_types::{
    AcceptDutchAuction, ClientOrderId, Command, CommitAsk, CommitBid, CreateBatchAuction,
    CreateDutchAuction, Fill, MarketConfig, MarketId, Minor, Qty, RevealAsk, RevealBid,
    SettleBatchAuction, TradeId, UserId,
};
use serde::{Deserialize, Serialize};

const KIND_OP: u8 = 1;
const KIND_STATE: u8 = 2;
/// Observational audit records. They do not change core state, so no
/// post-state hash follows them; recovery still verifies their own hash chain.
const KIND_AUDIT: u8 = 3;
// Tests force a checkpoint after every operation; production amortizes the full-state hash.
#[cfg(test)]
const CHECKPOINT_EVERY: usize = 1;
#[cfg(not(test))]
const CHECKPOINT_EVERY: usize = 256;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) enum WalOp {
    Command {
        user_id: UserId,
        command: Command,
        // Wall-clock time captured when the command was submitted. Defaults to
        // zero for WAL files written before escrow existed.
        #[serde(default)]
        now_ms: u64,
    },
    Deposit {
        user_id: UserId,
        amount: Minor,
        client_request_id: ClientOrderId,
    },
    Issue {
        user_id: UserId,
        market_id: MarketId,
        qty: Qty,
        client_request_id: ClientOrderId,
    },
    AddMarket {
        config: MarketConfig,
    },
    RegisterPublicKey {
        user_id: UserId,
        public_key: [u8; 32],
    },
    ReleaseEscrow {
        actor_user_id: UserId,
        trade_id: TradeId,
        delivered_at_ms: u64,
        signature: Vec<u8>,
        now_ms: u64,
    },
    RefundEscrow {
        actor_user_id: UserId,
        trade_id: TradeId,
        now_ms: u64,
    },
    AdminSettleEscrow {
        trade_id: TradeId,
        refund_to_buyer: bool,
    },
    CreateDutchAuction {
        seller_id: UserId,
        request: CreateDutchAuction,
        now_ms: u64,
    },
    AcceptDutchAuction {
        buyer_id: UserId,
        request: AcceptDutchAuction,
        now_ms: u64,
    },
    ExpireDutchAuction {
        actor_user_id: UserId,
        auction_id: u64,
        now_ms: u64,
    },
    CreateBatchAuction {
        created_by: UserId,
        request: CreateBatchAuction,
        now_ms: u64,
    },
    CommitBatchBid {
        bidder_id: UserId,
        request: CommitBid,
        now_ms: u64,
    },
    RevealBatchBid {
        bidder_id: UserId,
        request: RevealBid,
        now_ms: u64,
    },
    CommitBatchAsk {
        seller_id: UserId,
        request: CommitAsk,
        now_ms: u64,
    },
    RevealBatchAsk {
        seller_id: UserId,
        request: RevealAsk,
        now_ms: u64,
    },
    SettleBatchAuction {
        caller_id: UserId,
        request: SettleBatchAuction,
        now_ms: u64,
    },
}

pub(crate) struct WalSession {
    wal: Wal,
    ops_since_state: usize,
}

/// The audit entry as it is persisted to the log: the exact fields of
/// `dkx_hash::AuditRecord`, plus the terminal hash, so recovery can rebuild and
/// re-verify the published chain instead of re-deriving it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct AuditLogEntry {
    pub seq: u64,
    pub ts_micros: u64,
    pub prev: [u8; 32],
    pub kind: u8,
    pub market: u32,
    pub qty: u32,
    pub price: i64,
    pub hash: [u8; 32],
}

impl From<&AuditRecord> for AuditLogEntry {
    fn from(record: &AuditRecord) -> Self {
        Self {
            seq: record.seq,
            ts_micros: record.ts_micros,
            prev: record.prev,
            kind: record.kind,
            market: record.market,
            qty: record.qty,
            price: record.price,
            hash: record.hash,
        }
    }
}

impl From<AuditLogEntry> for AuditRecord {
    fn from(entry: AuditLogEntry) -> Self {
        Self {
            seq: entry.seq,
            ts_micros: entry.ts_micros,
            prev: entry.prev,
            kind: entry.kind,
            market: entry.market,
            qty: entry.qty,
            price: entry.price,
            hash: entry.hash,
        }
    }
}

impl WalSession {
    pub(crate) fn open(
        base_dir: impl AsRef<Path>,
        last_seq: u64,
        last_hash: [u8; 32],
    ) -> Result<Self, String> {
        Wal::open(base_dir, last_seq, last_hash)
            .map(|wal| Self {
                wal,
                ops_since_state: 0,
            })
            .map_err(|error| format!("open WAL: {error}"))
    }

    /// Append one operation and its post-state hash as one fsynced batch.
    pub(crate) fn append(&mut self, op: &WalOp, state_hash: [u8; 32]) -> Result<(), String> {
        self.append_batch(std::slice::from_ref(op), Some(state_hash))
    }

    /// Whether appending this many operations should also write a state hash.
    pub(crate) fn checkpoint_due(&self, ops: usize) -> bool {
        self.ops_since_state.saturating_add(ops) >= CHECKPOINT_EVERY
    }

    /// Append a batch of operations, optionally followed by a post-state hash.
    ///
    /// A checkpoint is written at most every `CHECKPOINT_EVERY` operations so a
    /// full-state hash is amortized across several writer batches.
    pub(crate) fn append_batch(
        &mut self,
        ops: &[WalOp],
        state_hash: Option<[u8; 32]>,
    ) -> Result<(), String> {
        if ops.is_empty() {
            return Ok(());
        }

        let mut records = Vec::with_capacity(ops.len() + 1);
        for op in ops {
            let payload = rmp_serde::to_vec_named(op)
                .map_err(|error| format!("encode WAL operation: {error}"))?;
            records.push((KIND_OP, payload));
        }
        if let Some(state_hash) = state_hash {
            records.push((KIND_STATE, state_hash.to_vec()));
            self.ops_since_state = 0;
        } else {
            self.ops_since_state = self.ops_since_state.saturating_add(ops.len());
        }
        self.wal
            .append_batch(&records)
            .map(|_| ())
            .map_err(|error| format!("append WAL: {error}"))
    }

    /// Append one audit record as its own log entry. It carries no post-state
    /// hash because it does not change core state; recovery re-verifies the
    /// audit record's own hash chain instead.
    pub(crate) fn append_audit(&mut self, record: &AuditRecord) -> Result<(), String> {
        let entry = AuditLogEntry::from(record);
        let payload = rmp_serde::to_vec_named(&entry)
            .map_err(|error| format!("encode audit log entry: {error}"))?;
        self.wal
            .append_batch(&[(KIND_AUDIT, payload)])
            .map(|_| ())
            .map_err(|error| format!("append audit record: {error}"))
    }
}

/// Rebuild a core from WAL records, verifying every post-state checkpoint, and
/// collect the persisted audit chain so a restart can continue the published
/// log instead of starting from genesis.
pub(crate) fn replay_with_audit(
    records: &[LogRecord],
    core: &mut Core,
    audit: &mut Vec<AuditRecord>,
) -> Result<Vec<Fill>, String> {
    let mut recent_trades = Vec::new();
    let mut ops_since_state = 0usize;
    audit.clear();

    for record in records {
        match record.kind {
            KIND_OP => {
                let op: WalOp = rmp_serde::from_slice(&record.payload)
                    .map_err(|error| format!("decode WAL record {}: {error}", record.seq))?;
                apply_op(core, &op, &mut recent_trades)?;
                ops_since_state += 1;
            }
            KIND_STATE => {
                if ops_since_state == 0 {
                    return Err(format!(
                        "WAL record {} has a state hash without any operation",
                        record.seq
                    ));
                }
                let expected = core.state_hash();
                if record.payload.as_slice() != expected.as_slice() {
                    return Err(format!(
                        "WAL state hash mismatch at record {}: expected {}, got {}",
                        record.seq,
                        hex::encode(expected),
                        hex::encode(&record.payload)
                    ));
                }
                ops_since_state = 0;
            }
            KIND_AUDIT => {
                let entry: AuditLogEntry = rmp_serde::from_slice(&record.payload)
                    .map_err(|error| format!("decode audit record {}: {error}", record.seq))?;
                audit.push(AuditRecord::from(entry));
            }
            other => return Err(format!("unknown WAL record kind {other}")),
        }
    }

    if let Some(first) = audit.first()
        && first.prev != dkx_hash::GENESIS_PREV_HASH
    {
        return Err("WAL audit chain does not start from the genesis hash".into());
    }
    dkx_hash::verify_audit_chain(audit).map_err(|error| format!("WAL audit chain: {error}"))?;

    // A crash can tear the final state-hash record after the operation records were
    // fully written. Those operations were never acknowledged, so replaying them is
    // safe; the client will retry with the same idempotency keys.
    Ok(recent_trades)
}

fn apply_op(core: &mut Core, op: &WalOp, recent_trades: &mut Vec<Fill>) -> Result<(), String> {
    match op {
        WalOp::Command {
            user_id,
            command,
            now_ms,
        } => {
            core.set_current_time_ms(*now_ms);
            let outcome = core.process_with_deltas(*user_id, command.clone());
            recent_trades.extend(outcome.fills().iter().cloned());
        }
        WalOp::Deposit {
            user_id,
            amount,
            client_request_id,
        } => {
            core.deposit_cash(*user_id, *amount, *client_request_id)
                .map_err(|reason| format!("WAL deposit replay rejected: {reason:?}"))?;
        }
        WalOp::Issue {
            user_id,
            market_id,
            qty,
            client_request_id,
        } => {
            core.issue_passes(*user_id, *market_id, *qty, *client_request_id)
                .map_err(|reason| format!("WAL issue replay rejected: {reason:?}"))?;
        }
        WalOp::AddMarket { config } => {
            core.add_market(config.clone())?;
        }
        WalOp::RegisterPublicKey {
            user_id,
            public_key,
        } => {
            core.register_public_key(*user_id, *public_key)
                .map_err(|reason| format!("WAL public-key replay rejected: {reason:?}"))?;
        }
        WalOp::ReleaseEscrow {
            actor_user_id,
            trade_id,
            delivered_at_ms,
            signature,
            now_ms,
        } => {
            core.release_escrow(
                *actor_user_id,
                *trade_id,
                *delivered_at_ms,
                signature,
                *now_ms,
            )
            .map_err(|reason| format!("WAL escrow release replay rejected: {reason:?}"))?;
        }
        WalOp::RefundEscrow {
            actor_user_id,
            trade_id,
            now_ms,
        } => {
            core.refund_escrow(*actor_user_id, *trade_id, *now_ms)
                .map_err(|reason| format!("WAL escrow refund replay rejected: {reason:?}"))?;
        }
        WalOp::AdminSettleEscrow {
            trade_id,
            refund_to_buyer,
        } => {
            core.admin_settle_escrow(*trade_id, *refund_to_buyer)
                .map_err(|reason| format!("WAL admin settle escrow replay rejected: {reason:?}"))?;
        }
        WalOp::CreateDutchAuction {
            seller_id,
            request,
            now_ms,
        } => {
            core.create_dutch_auction(*seller_id, request.clone(), *now_ms)
                .map_err(|reason| format!("WAL auction create replay rejected: {reason:?}"))?;
        }
        WalOp::AcceptDutchAuction {
            buyer_id,
            request,
            now_ms,
        } => {
            core.accept_dutch_auction(*buyer_id, request.clone(), *now_ms)
                .map_err(|reason| format!("WAL auction accept replay rejected: {reason:?}"))?;
        }
        WalOp::ExpireDutchAuction {
            actor_user_id,
            auction_id,
            now_ms,
        } => {
            core.expire_dutch_auction(*actor_user_id, *auction_id, *now_ms)
                .map_err(|reason| format!("WAL auction expire replay rejected: {reason:?}"))?;
        }
        WalOp::CreateBatchAuction {
            created_by,
            request,
            now_ms,
        } => {
            core.create_batch_auction(*created_by, request.clone(), *now_ms)
                .map_err(|reason| {
                    format!("WAL batch auction create replay rejected: {reason:?}")
                })?;
        }
        WalOp::CommitBatchBid {
            bidder_id,
            request,
            now_ms,
        } => {
            core.commit_batch_bid(*bidder_id, request.clone(), *now_ms)
                .map_err(|reason| {
                    format!("WAL batch auction commit replay rejected: {reason:?}")
                })?;
        }
        WalOp::RevealBatchBid {
            bidder_id,
            request,
            now_ms,
        } => {
            core.reveal_batch_bid(*bidder_id, request.clone(), *now_ms)
                .map_err(|reason| {
                    format!("WAL batch auction reveal replay rejected: {reason:?}")
                })?;
        }
        WalOp::CommitBatchAsk {
            seller_id,
            request,
            now_ms,
        } => {
            core.commit_batch_ask(*seller_id, request.clone(), *now_ms)
                .map_err(|reason| format!("WAL batch ask commit replay rejected: {reason:?}"))?;
        }
        WalOp::RevealBatchAsk {
            seller_id,
            request,
            now_ms,
        } => {
            core.reveal_batch_ask(*seller_id, request.clone(), *now_ms)
                .map_err(|reason| format!("WAL batch ask reveal replay rejected: {reason:?}"))?;
        }
        WalOp::SettleBatchAuction {
            caller_id,
            request,
            now_ms,
        } => {
            core.settle_batch_auction(*caller_id, request.clone(), *now_ms)
                .map_err(|reason| {
                    format!("WAL batch auction settle replay rejected: {reason:?}")
                })?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use dx_types::{
        AcceptDutchAuction, AuctionStatus, Command, CreateDutchAuction, DeliveryReceipt,
        EscrowState, PlaceLimit, PlaceMarket, Side, seed_markets,
    };
    use ed25519_dalek::{Signer, SigningKey};
    use tempfile::tempdir;

    fn apply_and_log(core: &mut Core, wal: &mut WalSession, op: WalOp) {
        let mut fills = Vec::new();
        apply_op(core, &op, &mut fills).unwrap();
        wal.append(&op, core.state_hash()).unwrap();
    }

    #[test]
    fn replay_rebuilds_markets_ledger_and_fills() {
        let dir = tempdir().unwrap();
        let recovered = dkx_wal::recover(dir.path()).unwrap();
        let mut wal =
            WalSession::open(dir.path(), recovered.last_seq, recovered.last_hash).unwrap();
        let mut core = Core::new(seed_markets()).unwrap();

        let mut config = seed_markets().into_iter().next().unwrap();
        config.id = 100;
        config.event_id = 99;
        config.name = "WAL market".into();
        apply_and_log(&mut core, &mut wal, WalOp::AddMarket { config });
        apply_and_log(
            &mut core,
            &mut wal,
            WalOp::Deposit {
                user_id: 1,
                amount: 2_000_000,
                client_request_id: 1,
            },
        );
        apply_and_log(
            &mut core,
            &mut wal,
            WalOp::Issue {
                user_id: 2,
                market_id: 100,
                qty: 2,
                client_request_id: 1,
            },
        );
        apply_and_log(
            &mut core,
            &mut wal,
            WalOp::Command {
                user_id: 2,
                command: Command::PlaceLimit(PlaceLimit {
                    client_order_id: 1,
                    market_id: 100,
                    side: Side::Sell,
                    price_ticks: 5_000,
                    qty: 2,
                    escrow: false,
                }),
                now_ms: 0,
            },
        );
        apply_and_log(
            &mut core,
            &mut wal,
            WalOp::Command {
                user_id: 1,
                command: Command::PlaceMarket(PlaceMarket {
                    client_order_id: 2,
                    market_id: 100,
                    side: Side::Buy,
                    qty: 2,
                    protect_price_ticks: 5_000,
                    escrow: false,
                }),
                now_ms: 0,
            },
        );

        let expected_hash = core.state_hash();
        let expected_snapshot = core.snapshot(100, 10).unwrap();
        drop(wal);

        let recovered = dkx_wal::recover(dir.path()).unwrap();
        let mut restored = Core::new(seed_markets()).unwrap();
        let fills =
            replay_with_audit(&recovered.replayed_records, &mut restored, &mut Vec::new()).unwrap();

        assert_eq!(restored.state_hash(), expected_hash);
        assert_eq!(restored.snapshot(100, 10).unwrap(), expected_snapshot);
        assert_eq!(fills.len(), 1);
        assert_eq!(
            restored.balance(1, dx_types::Asset::Cash).available,
            990_000
        );
        assert!(restored.config(100).is_some());
    }

    #[test]
    fn replay_rejects_a_state_hash_without_an_operation() {
        let record =
            dkx_wal::LogRecord::new(1, 1, dkx_wal::GENESIS_PREV_HASH, KIND_STATE, vec![0; 32])
                .unwrap();

        let error = replay_with_audit(
            &[record],
            &mut Core::new(seed_markets()).unwrap(),
            &mut Vec::new(),
        )
        .unwrap_err();
        assert!(error.contains("without any operation"), "{error}");
    }

    #[test]
    fn replay_rejects_a_wrong_state_hash() {
        let op = WalOp::Deposit {
            user_id: 1,
            amount: 1_000_000,
            client_request_id: 1,
        };
        let payload = rmp_serde::to_vec_named(&op).unwrap();
        let operation =
            dkx_wal::LogRecord::new(1, 1, dkx_wal::GENESIS_PREV_HASH, KIND_OP, payload).unwrap();
        let state = dkx_wal::LogRecord::new(2, 2, operation.hash, KIND_STATE, vec![0; 32]).unwrap();

        let error = replay_with_audit(
            &[operation, state],
            &mut Core::new(seed_markets()).unwrap(),
            &mut Vec::new(),
        )
        .unwrap_err();
        assert!(error.contains("state hash mismatch"), "{error}");
    }

    #[test]
    fn replay_verifies_a_state_hash_after_a_batch_of_operations() {
        let dir = tempdir().unwrap();
        let recovered = dkx_wal::recover(dir.path()).unwrap();
        let mut wal =
            WalSession::open(dir.path(), recovered.last_seq, recovered.last_hash).unwrap();
        let mut core = Core::new(seed_markets()).unwrap();
        let ops = [
            WalOp::Deposit {
                user_id: 1,
                amount: 1_000_000,
                client_request_id: 1,
            },
            WalOp::Deposit {
                user_id: 1,
                amount: 2_000_000,
                client_request_id: 2,
            },
        ];
        for op in &ops {
            apply_op(&mut core, op, &mut Vec::new()).unwrap();
        }
        let expected_hash = core.state_hash();
        wal.append_batch(&ops, Some(expected_hash)).unwrap();
        drop(wal);

        let recovered = dkx_wal::recover(dir.path()).unwrap();
        let mut restored = Core::new(seed_markets()).unwrap();
        replay_with_audit(&recovered.replayed_records, &mut restored, &mut Vec::new()).unwrap();

        assert_eq!(restored.state_hash(), expected_hash);
        assert_eq!(
            restored.balance(1, dx_types::Asset::Cash).available,
            3_000_000
        );
    }

    #[test]
    fn replay_accepts_a_trailing_operation_without_its_state_hash() {
        let op = WalOp::Deposit {
            user_id: 1,
            amount: 1_000_000,
            client_request_id: 1,
        };
        let payload = rmp_serde::to_vec_named(&op).unwrap();
        let record =
            dkx_wal::LogRecord::new(1, 1, dkx_wal::GENESIS_PREV_HASH, KIND_OP, payload).unwrap();
        let mut core = Core::new(seed_markets()).unwrap();

        replay_with_audit(&[record], &mut core, &mut Vec::new()).unwrap();
        assert_eq!(core.balance(1, dx_types::Asset::Cash).available, 1_000_000);
    }
    #[test]
    fn replay_rebuilds_an_escrow_release() {
        let dir = tempdir().unwrap();
        let recovered = dkx_wal::recover(dir.path()).unwrap();
        let mut wal =
            WalSession::open(dir.path(), recovered.last_seq, recovered.last_hash).unwrap();
        let mut core = Core::new(seed_markets()).unwrap();
        let key = SigningKey::from_bytes(&[7u8; 32]);
        let public_key = key.verifying_key().to_bytes();

        apply_and_log(
            &mut core,
            &mut wal,
            WalOp::RegisterPublicKey {
                user_id: 2,
                public_key,
            },
        );
        apply_and_log(
            &mut core,
            &mut wal,
            WalOp::Deposit {
                user_id: 1,
                amount: 2_000_000,
                client_request_id: 1,
            },
        );
        apply_and_log(
            &mut core,
            &mut wal,
            WalOp::Issue {
                user_id: 2,
                market_id: 3,
                qty: 2,
                client_request_id: 1,
            },
        );
        apply_and_log(
            &mut core,
            &mut wal,
            WalOp::Command {
                user_id: 2,
                command: Command::PlaceLimit(PlaceLimit {
                    client_order_id: 1,
                    market_id: 3,
                    side: Side::Sell,
                    price_ticks: 4_000,
                    qty: 2,
                    escrow: false,
                }),
                now_ms: 1_000,
            },
        );
        apply_and_log(
            &mut core,
            &mut wal,
            WalOp::Command {
                user_id: 1,
                command: Command::PlaceLimit(PlaceLimit {
                    client_order_id: 2,
                    market_id: 3,
                    side: Side::Buy,
                    price_ticks: 4_000,
                    qty: 2,
                    escrow: false,
                }),
                now_ms: 1_000,
            },
        );

        let record = core.escrow_records(1)[0].clone();
        let receipt = DeliveryReceipt {
            trade_id: record.trade_id,
            market_id: record.market_id,
            qty: record.qty,
            buyer_id: record.buyer_id,
            seller_id: record.seller_id,
            delivered_at_ms: 2_000,
        };
        let signature = key.sign(&receipt.canonical_bytes()).to_bytes().to_vec();
        apply_and_log(
            &mut core,
            &mut wal,
            WalOp::ReleaseEscrow {
                actor_user_id: 2,
                trade_id: record.trade_id,
                delivered_at_ms: 2_000,
                signature,
                now_ms: 2_000,
            },
        );

        let expected_hash = core.state_hash();
        drop(wal);

        let recovered = dkx_wal::recover(dir.path()).unwrap();
        let mut restored = Core::new(seed_markets()).unwrap();
        replay_with_audit(&recovered.replayed_records, &mut restored, &mut Vec::new()).unwrap();

        assert_eq!(restored.state_hash(), expected_hash);
        assert_eq!(
            restored.escrow_record(record.trade_id).unwrap().state,
            EscrowState::Delivered
        );
        assert_eq!(
            restored.balance(2, dx_types::Asset::Cash).available,
            400_000
        );
        assert_eq!(
            restored
                .balance(1, dx_types::Asset::Pass { market_id: 3 })
                .available,
            2
        );
    }

    #[test]
    fn replay_rebuilds_a_dutch_auction_sale_and_expiry() {
        let dir = tempdir().unwrap();
        let recovered = dkx_wal::recover(dir.path()).unwrap();
        let mut wal =
            WalSession::open(dir.path(), recovered.last_seq, recovered.last_hash).unwrap();
        let mut core = Core::new(seed_markets()).unwrap();

        apply_and_log(
            &mut core,
            &mut wal,
            WalOp::Issue {
                user_id: 2,
                market_id: 1,
                qty: 6,
                client_request_id: 1,
            },
        );
        apply_and_log(
            &mut core,
            &mut wal,
            WalOp::Deposit {
                user_id: 1,
                amount: 2_000_000,
                client_request_id: 1,
            },
        );
        apply_and_log(
            &mut core,
            &mut wal,
            WalOp::CreateDutchAuction {
                seller_id: 2,
                request: CreateDutchAuction {
                    client_order_id: 1,
                    market_id: 1,
                    qty: 6,
                    start_price_ticks: 5_000,
                    floor_price_ticks: 2_500,
                    duration_ms: 10_000,
                },
                now_ms: 1_000,
            },
        );
        apply_and_log(
            &mut core,
            &mut wal,
            WalOp::AcceptDutchAuction {
                buyer_id: 1,
                request: AcceptDutchAuction {
                    client_order_id: 1,
                    auction_id: 1,
                    qty: 2,
                    max_price_ticks: 5_000,
                },
                now_ms: 1_000,
            },
        );
        apply_and_log(
            &mut core,
            &mut wal,
            WalOp::ExpireDutchAuction {
                actor_user_id: 2,
                auction_id: 1,
                now_ms: 11_000,
            },
        );
        let expected_hash = core.state_hash();
        let settled = core.dutch_auction(1).unwrap().clone();
        drop(wal);

        let recovered = dkx_wal::recover(dir.path()).unwrap();
        let mut restored = Core::new(seed_markets()).unwrap();
        replay_with_audit(&recovered.replayed_records, &mut restored, &mut Vec::new()).unwrap();

        assert_eq!(restored.state_hash(), expected_hash);
        assert_eq!(restored.dutch_auction(1).unwrap(), &settled);
        assert_eq!(settled.status, AuctionStatus::Expired);
        assert_eq!(settled.qty_remaining, 4);
        assert_eq!(
            restored.balance(1, dx_types::Asset::Pass { market_id: 1 }),
            dx_types::Balance {
                asset: dx_types::Asset::Pass { market_id: 1 },
                available: 2,
                held: 0,
            }
        );
        assert_eq!(
            restored.balance(2, dx_types::Asset::Pass { market_id: 1 }),
            dx_types::Balance {
                asset: dx_types::Asset::Pass { market_id: 1 },
                available: 4,
                held: 0,
            }
        );
    }

    /// Build a self-consistent audit record the way the writer does.
    fn audit_record(
        seq: u64,
        prev: &[u8; 32],
        kind: u8,
        market: u32,
        qty: u32,
        price: i64,
    ) -> AuditRecord {
        let ts_micros = seq * 1_000_000;
        let hash = dkx_hash::hash_audit_entry(seq, ts_micros, prev, kind, market, qty, price);
        AuditRecord {
            seq,
            ts_micros,
            prev: *prev,
            kind,
            market,
            qty,
            price,
            hash,
        }
    }

    #[test]
    fn replay_carries_the_persisted_audit_chain() {
        let dir = tempdir().unwrap();
        let recovered = dkx_wal::recover(dir.path()).unwrap();
        let mut wal =
            WalSession::open(dir.path(), recovered.last_seq, recovered.last_hash).unwrap();
        let mut core = Core::new(seed_markets()).unwrap();

        // The writer logs each audit record right after the operation that
        // produced it, so replay sees the same interleaving.
        apply_and_log(
            &mut core,
            &mut wal,
            WalOp::Deposit {
                user_id: 1,
                amount: 2_000_000,
                client_request_id: 1,
            },
        );
        let issue_op = WalOp::Issue {
            user_id: 1,
            market_id: 1,
            qty: 2,
            client_request_id: 2,
        };
        let mut fills = Vec::new();
        apply_op(&mut core, &issue_op, &mut fills).unwrap();
        wal.append(&issue_op, core.state_hash()).unwrap();
        let issued = audit_record(1, &dkx_hash::GENESIS_PREV_HASH, 2, 1, 2, 0);
        wal.append_audit(&issued).unwrap();

        let expected = vec![issued];
        drop(wal);

        let recovered = dkx_wal::recover(dir.path()).unwrap();
        let mut restored = Core::new(seed_markets()).unwrap();
        let mut replayed_audit = Vec::new();
        replay_with_audit(
            &recovered.replayed_records,
            &mut restored,
            &mut replayed_audit,
        )
        .unwrap();
        assert_eq!(replayed_audit, expected);

        // A restarted writer continues the chain from the recovered head.
        let next = audit_record(2, &replayed_audit[0].hash, 1, 1, 2, 10_000);
        assert_eq!(next.prev, expected[0].hash);
    }

    #[test]
    fn replay_rejects_a_forged_audit_hash() {
        let entry = AuditLogEntry {
            seq: 1,
            ts_micros: 1_000_000,
            prev: dkx_hash::GENESIS_PREV_HASH,
            kind: 2,
            market: 1,
            qty: 2,
            price: 0,
            // Does not match the record's fields.
            hash: [7; 32],
        };
        let payload = rmp_serde::to_vec_named(&entry).unwrap();
        let record =
            dkx_wal::LogRecord::new(1, 1, dkx_wal::GENESIS_PREV_HASH, KIND_AUDIT, payload).unwrap();

        let error = replay_with_audit(
            &[record],
            &mut Core::new(seed_markets()).unwrap(),
            &mut Vec::new(),
        )
        .unwrap_err();
        assert!(error.contains("audit chain"), "{error}");
    }

    #[test]
    fn replay_rejects_an_audit_chain_that_skips_genesis() {
        // The hash is self-consistent, but the first record of the chain must
        // link to the genesis hash, not to a hash of nothing.
        let record = audit_record(1, &[9; 32], 2, 1, 2, 0);
        assert_ne!(record.prev, dkx_hash::GENESIS_PREV_HASH);
        let payload = rmp_serde::to_vec_named(&AuditLogEntry::from(&record)).unwrap();
        let log_record =
            dkx_wal::LogRecord::new(1, 1, dkx_wal::GENESIS_PREV_HASH, KIND_AUDIT, payload).unwrap();

        let error = replay_with_audit(
            &[log_record],
            &mut Core::new(seed_markets()).unwrap(),
            &mut Vec::new(),
        )
        .unwrap_err();
        assert!(error.contains("genesis hash"), "{error}");
    }
}
