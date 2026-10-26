//! Replayable operations for the gateway WAL.
//!
//! State changes are stored as operation records. A full core-state hash checkpoint
//! follows a batch at least every 256 operations. Recovery replays the operations,
//! then checks each checkpoint. That catches a missing record or a non-deterministic
//! replay at the next checkpoint boundary.

use std::path::Path;

use dkx_wal::{LogRecord, Wal};
use dx_core::Core;
use dx_types::{ClientOrderId, Command, Fill, MarketConfig, MarketId, Minor, Qty, UserId};
use serde::{Deserialize, Serialize};

const KIND_OP: u8 = 1;
const KIND_STATE: u8 = 2;
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
}

pub(crate) struct WalSession {
    wal: Wal,
    ops_since_state: usize,
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
}

/// Rebuild a core from WAL records, verifying every post-state checkpoint.
pub(crate) fn replay(records: &[LogRecord], core: &mut Core) -> Result<Vec<Fill>, String> {
    let mut recent_trades = Vec::new();
    let mut ops_since_state = 0usize;

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
            other => return Err(format!("unknown WAL record kind {other}")),
        }
    }

    // A crash can tear the final state-hash record after the operation records were
    // fully written. Those operations were never acknowledged, so replaying them is
    // safe; the client will retry with the same idempotency keys.
    Ok(recent_trades)
}

fn apply_op(core: &mut Core, op: &WalOp, recent_trades: &mut Vec<Fill>) -> Result<(), String> {
    match op {
        WalOp::Command { user_id, command } => {
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
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use dx_types::{Command, PlaceLimit, PlaceMarket, Side, seed_markets};
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
                }),
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
                }),
            },
        );

        let expected_hash = core.state_hash();
        let expected_snapshot = core.snapshot(100, 10).unwrap();
        drop(wal);

        let recovered = dkx_wal::recover(dir.path()).unwrap();
        let mut restored = Core::new(seed_markets()).unwrap();
        let fills = replay(&recovered.replayed_records, &mut restored).unwrap();

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

        let error = replay(&[record], &mut Core::new(seed_markets()).unwrap()).unwrap_err();
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

        let error =
            replay(&[operation, state], &mut Core::new(seed_markets()).unwrap()).unwrap_err();
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
        replay(&recovered.replayed_records, &mut restored).unwrap();

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

        replay(&[record], &mut core).unwrap();
        assert_eq!(core.balance(1, dx_types::Asset::Cash).available, 1_000_000);
    }
}
