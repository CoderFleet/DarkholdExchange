//! Full-state snapshots for WAL compaction.
//!
//! A snapshot captures the entire core state so recovery can rebuild from it and
//! replay only the log records written after the snapshot. The state is split into
//! three opaque blobs that match `dkx_wal::SnapshotData`'s fields:
//!
//! - **ledger bytes**: the double-entry ledger (accounts and journals).
//! - **escrow bytes**: every escrow record.
//! - **rest bytes**: the matching engine, all auxiliary maps (idempotency cache,
//!   holding limits, auctions, batch-auction state), the observational audit chain
//!   and recent-trade tape carried so log compaction cannot lose them, and the
//!   core state hash at snapshot time.
//!
//! `from_wal_snapshot` verifies the rebuilt state hash, so any serialization or
//! determinism mistake is caught at startup instead of silently diverging.

use rmp_serde::{Deserializer, Serializer};
use serde::{Deserialize, Serialize};

use dkx_hash::{AuditRecord, GENESIS_PREV_HASH};
use dx_engine::Engine;
use dx_ledger::Ledger;
use dx_types::{
    AskRecord, AuctionAccept, BatchAuction, BatchAuctionFill, BidRecord, ClientOrderId,
    CommandResponse, DutchAuction, EscrowRecord, Fill, MarketConfig, MarketId, Qty, UserId,
};

use super::{CachedCommand, CommandKind, Core};

/// Wire mirror of the private [`CommandKind`] idempotency tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SnapshotCommandKind {
    PlaceLimit,
    PlaceMarket,
    Cancel,
}

impl From<CommandKind> for SnapshotCommandKind {
    fn from(kind: CommandKind) -> Self {
        match kind {
            CommandKind::PlaceLimit => Self::PlaceLimit,
            CommandKind::PlaceMarket => Self::PlaceMarket,
            CommandKind::Cancel => Self::Cancel,
        }
    }
}

impl From<SnapshotCommandKind> for CommandKind {
    fn from(kind: SnapshotCommandKind) -> Self {
        match kind {
            SnapshotCommandKind::PlaceLimit => Self::PlaceLimit,
            SnapshotCommandKind::PlaceMarket => Self::PlaceMarket,
            SnapshotCommandKind::Cancel => Self::Cancel,
        }
    }
}

/// One entry of the idempotency cache in snapshot form.
pub type IdempotencyEntry = (UserId, ClientOrderId, SnapshotCommandKind, CommandResponse);

/// The observational state carried through a snapshot because log compaction
/// deletes the log records that produced it: the audit chain prefix and the
/// recent-trade tape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObservationSnapshot {
    pub audit_chain: Vec<AuditRecord>,
    pub recent_trades: Vec<Fill>,
}

/// Everything outside the ledger and escrow blobs: the matching engine, the
/// auxiliary maps, observational state, and the state hash to verify against.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct RestSnapshot {
    pub engine: Engine,
    pub markets: Vec<MarketConfig>,
    pub next_seq: u64,
    pub state_version: u64,
    pub idempotency: Vec<IdempotencyEntry>,
    pub open_buy_qty: Vec<((UserId, MarketId), Qty)>,
    pub public_keys: Vec<(UserId, [u8; 32])>,
    pub auctions: Vec<DutchAuction>,
    pub next_auction_id: u64,
    pub auction_accepts: Vec<((UserId, ClientOrderId), AuctionAccept)>,
    pub auction_creates: Vec<((UserId, ClientOrderId), u64)>,
    pub batch_auctions: Vec<(u64, BatchAuction)>,
    pub next_batch_auction_id: u64,
    pub batch_bids: Vec<((u64, UserId, ClientOrderId), BidRecord)>,
    pub batch_asks: Vec<((u64, UserId, ClientOrderId), AskRecord)>,
    pub batch_commits: Vec<((UserId, ClientOrderId), u64)>,
    pub batch_ask_commits: Vec<((UserId, ClientOrderId), u64)>,
    pub batch_reveals: Vec<((UserId, ClientOrderId), u64)>,
    pub batch_ask_reveals: Vec<((UserId, ClientOrderId), u64)>,
    pub batch_creates: Vec<((UserId, ClientOrderId), u64)>,
    pub batch_settlements: Vec<(u64, Vec<BatchAuctionFill>)>,
    pub observations: ObservationSnapshot,
    pub state_hash: [u8; 32],
}

/// The decoded result of a snapshot restore: the rebuilt core plus the
/// observational state the writer needs to resume where it left off.
pub struct RestoredState {
    pub core: Core,
    pub audit_chain: Vec<AuditRecord>,
    pub recent_trades: Vec<Fill>,
}

fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    value
        .serialize(&mut Serializer::new(&mut bytes))
        .map_err(|error| format!("snapshot encode: {error}"))?;
    Ok(bytes)
}

fn decode<T: for<'de> Deserialize<'de>>(bytes: &[u8], what: &str) -> Result<T, String> {
    T::deserialize(&mut Deserializer::new(bytes))
        .map_err(|error| format!("snapshot decode {what}: {error}"))
}

impl Core {
    /// Encode the full core state as the three WAL snapshot blobs. The caller    /// supplies the observational state (audit chain, recent-trade tape) that the
    /// snapshot must preserve while old log segments are compacted away.
    ///
    /// The blobs are only valid together with the WAL sequence/hash pair the
    /// caller records from `Wal::last_seq()`/`last_hash()` at this exact moment.
    #[allow(clippy::type_complexity)]
    pub fn wal_snapshot_blobs(
        &self,
        audit_chain: &[AuditRecord],
        recent_trades: &[Fill],
    ) -> Result<(Vec<u8>, Vec<u8>, Vec<u8>), String> {
        let ledger_bytes = encode(&self.ledger)?;
        let escrow: Vec<EscrowRecord> = self.escrow.values().cloned().collect();
        let escrow_bytes = encode(&escrow)?;

        let rest = RestSnapshot {
            engine: self.engine.clone(),
            markets: self.markets.values().cloned().collect(),
            next_seq: self.next_seq,
            state_version: self.state_version,
            idempotency: self
                .idempotency
                .iter()
                .map(|((user_id, client_order_id), cached)| {
                    (
                        *user_id,
                        *client_order_id,
                        SnapshotCommandKind::from(cached.kind),
                        cached.response.clone(),
                    )
                })
                .collect(),
            open_buy_qty: self
                .open_buy_qty
                .iter()
                .map(|(key, qty)| (*key, *qty))
                .collect(),
            public_keys: self
                .public_keys
                .iter()
                .map(|(user_id, key)| (*user_id, *key))
                .collect(),
            auctions: self.auctions.values().cloned().collect(),
            next_auction_id: self.next_auction_id,
            auction_accepts: self
                .auction_accepts
                .iter()
                .map(|(key, accept)| (*key, accept.clone()))
                .collect(),
            auction_creates: self
                .auction_creates
                .iter()
                .map(|(key, auction_id)| (*key, *auction_id))
                .collect(),
            batch_auctions: self
                .batch_auctions
                .iter()
                .map(|(auction_id, auction)| (*auction_id, auction.clone()))
                .collect(),
            next_batch_auction_id: self.next_batch_auction_id,
            batch_bids: self
                .batch_bids
                .iter()
                .map(|(key, bid)| (*key, bid.clone()))
                .collect(),
            batch_asks: self
                .batch_asks
                .iter()
                .map(|(key, ask)| (*key, ask.clone()))
                .collect(),
            batch_commits: self
                .batch_commits
                .iter()
                .map(|(key, auction_id)| (*key, *auction_id))
                .collect(),
            batch_ask_commits: self
                .batch_ask_commits
                .iter()
                .map(|(key, auction_id)| (*key, *auction_id))
                .collect(),
            batch_reveals: self
                .batch_reveals
                .iter()
                .map(|(key, auction_id)| (*key, *auction_id))
                .collect(),
            batch_ask_reveals: self
                .batch_ask_reveals
                .iter()
                .map(|(key, auction_id)| (*key, *auction_id))
                .collect(),
            batch_creates: self
                .batch_creates
                .iter()
                .map(|(key, auction_id)| (*key, *auction_id))
                .collect(),
            batch_settlements: self
                .batch_settlements
                .iter()
                .map(|(auction_id, fills)| (*auction_id, fills.clone()))
                .collect(),
            observations: ObservationSnapshot {
                audit_chain: audit_chain.to_vec(),
                recent_trades: recent_trades.to_vec(),
            },
            state_hash: self.state_hash(),
        };

        let rest_bytes = encode(&rest)?;
        Ok((ledger_bytes, escrow_bytes, rest_bytes))
    }

    /// Rebuild a core from the three snapshot blobs and verify the reconstructed
    /// state hash. Any mismatch means the snapshot bytes are corrupt or the
    /// serialization is not replay-deterministic, so recovery fails loudly.
    pub fn from_wal_snapshot(
        ledger_bytes: &[u8],
        escrow_bytes: &[u8],
        rest_bytes: &[u8],
    ) -> Result<RestoredState, String> {
        let ledger: Ledger = decode(ledger_bytes, "ledger")?;
        let escrow: Vec<EscrowRecord> = decode(escrow_bytes, "escrow")?;
        let rest: RestSnapshot = decode(rest_bytes, "rest")?;

        let core = Core {
            engine: rest.engine,
            ledger,
            markets: rest
                .markets
                .iter()
                .map(|market| (market.id, market.clone()))
                .collect(),
            next_seq: rest.next_seq,
            state_version: rest.state_version,
            idempotency: rest
                .idempotency
                .iter()
                .map(|(user_id, client_order_id, kind, response)| {
                    (
                        (*user_id, *client_order_id),
                        CachedCommand {
                            kind: CommandKind::from(*kind),
                            response: response.clone(),
                        },
                    )
                })
                .collect(),
            open_buy_qty: rest.open_buy_qty.iter().copied().collect(),
            escrow: escrow
                .iter()
                .map(|record| (record.trade_id, record.clone()))
                .collect(),
            public_keys: rest.public_keys.iter().copied().collect(),
            auctions: rest
                .auctions
                .iter()
                .map(|auction| (auction.id, auction.clone()))
                .collect(),
            next_auction_id: rest.next_auction_id,
            auction_accepts: rest.auction_accepts.iter().cloned().collect(),
            auction_creates: rest.auction_creates.iter().copied().collect(),
            batch_auctions: rest.batch_auctions.iter().cloned().collect(),
            next_batch_auction_id: rest.next_batch_auction_id,
            batch_bids: rest.batch_bids.iter().cloned().collect(),
            batch_asks: rest.batch_asks.iter().cloned().collect(),
            batch_commits: rest.batch_commits.iter().copied().collect(),
            batch_ask_commits: rest.batch_ask_commits.iter().copied().collect(),
            batch_reveals: rest.batch_reveals.iter().copied().collect(),
            batch_ask_reveals: rest.batch_ask_reveals.iter().copied().collect(),
            batch_creates: rest.batch_creates.iter().copied().collect(),
            batch_settlements: rest.batch_settlements.iter().cloned().collect(),
            current_time_ms: 0,
            poisoned: false,
        };

        if core.state_hash() != rest.state_hash {
            return Err("snapshot state hash mismatch: rebuilt state differs from snapshot".into());
        }

        // The snapshot's audit chain must still start at the genesis hash so the
        // replayed records can be appended and verified as one unbroken chain.
        if let Some(first) = rest.observations.audit_chain.first()
            && first.prev != GENESIS_PREV_HASH
        {
            return Err("snapshot audit chain does not start from the genesis hash".into());
        }

        core.check_invariants()?;

        Ok(RestoredState {
            audit_chain: rest.observations.audit_chain,
            recent_trades: rest.observations.recent_trades,
            core,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dx_types::{
        Command, CommitBid, CreateBatchAuction, PlaceLimit, RevealBid, Side, seed_markets,
    };
    use sha2::{Digest, Sha256};

    fn recorded() -> Core {
        let mut core = Core::new(seed_markets()).unwrap();
        let market = seed_markets()[0].id;
        let escrow_market = seed_markets()[2].id; // the only escrow-enabled seed

        // Seller's delivery-receipt key (escrow trades need it registered).
        let signing = ed25519_dalek::SigningKey::from_bytes(&[42u8; 32]);
        core.register_public_key(2, signing.verifying_key().to_bytes())
            .unwrap();

        core.deposit_cash(1, 5_000_000, 1).unwrap();
        core.deposit_cash(2, 5_000_000, 2).unwrap();
        core.issue_passes(1, market, 10, 3).unwrap();
        core.issue_passes(2, market, 10, 4).unwrap();
        core.issue_passes(2, escrow_market, 10, 5).unwrap();

        // Crossing fills one escrow trade and leaves orders on both sides.
        core.process(
            2,
            Command::PlaceLimit(PlaceLimit {
                client_order_id: 4,
                market_id: escrow_market,
                side: Side::Sell,
                price_ticks: 4_800,
                qty: 3,
                escrow: true,
            }),
        );
        core.process(
            1,
            Command::PlaceLimit(PlaceLimit {
                client_order_id: 5,
                market_id: escrow_market,
                side: Side::Buy,
                price_ticks: 4_900,
                qty: 2,
                escrow: true,
            }),
        );

        // Batch-window state (idempotency + parked orders) must survive too.
        let auction = core
            .create_batch_auction(
                1,
                CreateBatchAuction {
                    client_order_id: 6,
                    market_id: market,
                    floor_price_ticks: 3_000,
                    commit_duration_ms: 1_000,
                    reveal_duration_ms: 1_000,
                },
                0,
            )
            .unwrap();
        let salt = [7u8; 32];
        let commitment = Sha256::digest(CommitBid::commitment_preimage(5_000, 2, &salt)).into();
        core.commit_batch_bid(
            2,
            CommitBid {
                client_order_id: 7,
                auction_id: auction.id,
                commitment,
            },
            500,
        )
        .unwrap();
        core.reveal_batch_bid(
            2,
            RevealBid {
                client_order_id: 7,
                auction_id: auction.id,
                price_ticks: 5_000,
                qty: 2,
                salt,
            },
            1_500,
        )
        .unwrap();

        // One more fresh order so the idempotency cache is non-trivial.
        core.process(
            1,
            Command::PlaceLimit(PlaceLimit {
                client_order_id: 8,
                market_id: market,
                side: Side::Buy,
                price_ticks: 4_600,
                qty: 1,
                escrow: false,
            }),
        );

        core
    }

    fn audit_record(seq: u64) -> AuditRecord {
        let prev = GENESIS_PREV_HASH;
        let hash = dkx_hash::hash_audit_entry(seq, 1000, &prev, 1, 3, 2, 480_000);
        AuditRecord {
            seq,
            ts_micros: 1000,
            prev,
            kind: 1,
            market: 3,
            qty: 2,
            price: 480_000,
            hash,
        }
    }

    #[test]
    fn snapshot_roundtrip_restores_the_full_state() {
        let mut core = recorded();
        core.check_invariants().unwrap(); // TEMP probe: is the source state clean?
        let audit = vec![audit_record(1)];
        let tape: Vec<Fill> = Vec::new();

        let (ledger_bytes, escrow_bytes, rest_bytes) =
            core.wal_snapshot_blobs(&audit, &tape).unwrap();
        let mut restored =
            Core::from_wal_snapshot(&ledger_bytes, &escrow_bytes, &rest_bytes).unwrap();

        // Everything the state hash covers must reproduce bit-for-bit.
        assert_eq!(restored.core.state_hash(), core.state_hash());

        // Hash-uncovered state survives because it lives in the blobs.
        let auction_id = core.batch_auctions(None).first().expect("auction").id;
        assert_eq!(
            restored.core.batch_auction(auction_id),
            core.batch_auction(auction_id)
        );
        assert!(!restored.core.all_escrow_records().is_empty());
        assert_eq!(restored.audit_chain, audit);

        // The idempotency cache is live: a retry on the restored core returns
        // the same cached response the original core produced.
        let retry = Command::PlaceLimit(PlaceLimit {
            client_order_id: 8,
            market_id: seed_markets()[0].id,
            side: Side::Buy,
            price_ticks: 4_600,
            qty: 1,
            escrow: false,
        });
        assert_eq!(
            core.process(1, retry.clone()),
            restored.core.process(1, retry.clone())
        );

        // Both cores accept the same follow-up command and stay in lockstep.
        let next = Command::PlaceLimit(PlaceLimit {
            client_order_id: 9,
            market_id: seed_markets()[0].id,
            side: Side::Sell,
            price_ticks: 5_100,
            qty: 1,
            escrow: false,
        });
        core.process(2, next.clone());
        restored.core.process(2, next);
        assert_eq!(restored.core.state_hash(), core.state_hash());
    }

    #[test]
    fn snapshot_rejects_corrupted_blobs() {
        let core = recorded();
        let (ledger_bytes, escrow_bytes, mut rest_bytes) =
            core.wal_snapshot_blobs(&[], &[]).unwrap();

        // Flip one payload byte in the rest blob: decode corrupts some field,
        // and either decode fails or the rebuilt state hash mismatches.
        let last = rest_bytes.len() - 1;
        rest_bytes[last] ^= 0xff;
        assert!(Core::from_wal_snapshot(&ledger_bytes, &escrow_bytes, &rest_bytes).is_err());
    }

    #[test]
    fn snapshot_rejects_a_broken_audit_chain_prefix() {
        let core = recorded();
        let mut audit = vec![audit_record(1)];
        audit[0].prev = [9u8; 32]; // no longer links the genesis hash
        let (ledger_bytes, escrow_bytes, rest_bytes) =
            core.wal_snapshot_blobs(&audit, &[]).unwrap();
        assert!(Core::from_wal_snapshot(&ledger_bytes, &escrow_bytes, &rest_bytes).is_err());
    }
}
