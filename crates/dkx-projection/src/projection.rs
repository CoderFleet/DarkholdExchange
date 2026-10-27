use crate::events::ProjectionEvent;
use dx_ledger::money::Account;
use sqlx::PgPool;

pub const BATCH_SIZE: usize = 500;

#[derive(Debug, thiserror::Error)]
pub enum ProjectionError {
    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),

    #[error("Sequence gap detected: expected > {expected}, got {got}")]
    SequenceGap { expected: u64, got: u64 },
}

pub struct ProjectionWorker {
    pool: PgPool,
}

impl ProjectionWorker {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn get_last_seq(&self) -> Result<u64, ProjectionError> {
        let row: (i64,) = sqlx::query_as("SELECT last_seq FROM projection_state WHERE id = 1")
            .fetch_one(&self.pool)
            .await?;
        Ok(row.0 as u64)
    }

    /// Applies a stream of events in idempotent batches of up to 500 per transaction.
    pub async fn apply_batch(&self, events: &[ProjectionEvent]) -> Result<u64, ProjectionError> {
        if events.is_empty() {
            return self.get_last_seq().await;
        }

        let mut tx = self.pool.begin().await?;

        let row: (i64,) =
            sqlx::query_as("SELECT last_seq FROM projection_state WHERE id = 1 FOR UPDATE")
                .fetch_one(&mut *tx)
                .await?;
        let mut last_seq = row.0 as u64;

        for chunk in events.chunks(BATCH_SIZE) {
            for event in chunk {
                let seq = event.seq();
                if seq <= last_seq {
                    // Idempotent: already processed
                    continue;
                }

                match event {
                    ProjectionEvent::LedgerPosted {
                        seq: event_seq,
                        entry,
                    } => {
                        let kind_str = format!("{:?}", entry.kind);
                        let idem_str = entry.key.0.to_string();

                        // 1. Insert header (ON CONFLICT DO NOTHING for idempotency)
                        sqlx::query(
                            r#"
                            INSERT INTO ledger_entries (id, seq, idem_key, kind, ts)
                            VALUES ($1, $2, $3::NUMERIC, $4, $5)
                            ON CONFLICT (idem_key) DO NOTHING
                            "#,
                        )
                        .bind(entry.id as i64)
                        .bind(*event_seq as i64)
                        .bind(
                            sqlx::types::BigDecimal::parse_bytes(idem_str.as_bytes(), 10)
                                .unwrap_or_default(),
                        )
                        .bind(kind_str)
                        .bind(entry.ts_micros as i64)
                        .execute(&mut *tx)
                        .await?;

                        // 2. Insert legs and update balances
                        for leg in &entry.legs {
                            let account_str = format!("{:?}", leg.account);
                            let asset_str = format!("{:?}", leg.asset);

                            sqlx::query(
                                r#"
                                INSERT INTO ledger_legs (entry_id, account, asset, amount)
                                VALUES ($1, $2, $3, $4)
                                "#,
                            )
                            .bind(entry.id as i64)
                            .bind(account_str)
                            .bind(asset_str)
                            .bind(leg.amount)
                            .execute(&mut *tx)
                            .await?;

                            match leg.account {
                                Account::CashAvail(user_id) => {
                                    sqlx::query(
                                        r#"
                                        INSERT INTO balances (user_id, cash_avail, cash_locked, escrow_cash, updated_seq)
                                        VALUES ($1, $2, 0, 0, $3)
                                        ON CONFLICT (user_id) DO UPDATE
                                        SET cash_avail = balances.cash_avail + $2, updated_seq = $3
                                        "#,
                                    )
                                    .bind(user_id as i64)
                                    .bind(leg.amount)
                                    .bind(*event_seq as i64)
                                    .execute(&mut *tx)
                                    .await?;
                                }
                                Account::CashLocked(user_id) => {
                                    sqlx::query(
                                        r#"
                                        INSERT INTO balances (user_id, cash_avail, cash_locked, escrow_cash, updated_seq)
                                        VALUES ($1, 0, $2, 0, $3)
                                        ON CONFLICT (user_id) DO UPDATE
                                        SET cash_locked = balances.cash_locked + $2, updated_seq = $3
                                        "#,
                                    )
                                    .bind(user_id as i64)
                                    .bind(leg.amount)
                                    .bind(*event_seq as i64)
                                    .execute(&mut *tx)
                                    .await?;
                                }
                                Account::PassAvail(user_id, market_id) => {
                                    sqlx::query(
                                        r#"
                                        INSERT INTO pass_balances (user_id, market_id, avail, locked, held)
                                        VALUES ($1, $2, $3, 0, 0)
                                        ON CONFLICT (user_id, market_id) DO UPDATE
                                        SET avail = pass_balances.avail + $3
                                        "#,
                                    )
                                    .bind(user_id as i64)
                                    .bind(market_id as i32)
                                    .bind(leg.amount)
                                    .execute(&mut *tx)
                                    .await?;
                                }
                                Account::PassLocked(user_id, market_id) => {
                                    sqlx::query(
                                        r#"
                                        INSERT INTO pass_balances (user_id, market_id, avail, locked, held)
                                        VALUES ($1, $2, 0, $3, 0)
                                        ON CONFLICT (user_id, market_id) DO UPDATE
                                        SET locked = pass_balances.locked + $3
                                        "#,
                                    )
                                    .bind(user_id as i64)
                                    .bind(market_id as i32)
                                    .bind(leg.amount)
                                    .execute(&mut *tx)
                                    .await?;
                                }
                                Account::PassHeld(user_id, market_id) => {
                                    sqlx::query(
                                        r#"
                                        INSERT INTO pass_balances (user_id, market_id, avail, locked, held)
                                        VALUES ($1, $2, 0, 0, $3)
                                        ON CONFLICT (user_id, market_id) DO UPDATE
                                        SET held = pass_balances.held + $3
                                        "#,
                                    )
                                    .bind(user_id as i64)
                                    .bind(market_id as i32)
                                    .bind(leg.amount)
                                    .execute(&mut *tx)
                                    .await?;
                                }
                                Account::EscrowCash => {}
                                _ => {}
                            }
                        }
                    }
                    ProjectionEvent::TradeExecuted(trade) => {
                        sqlx::query(
                            r#"
                            INSERT INTO trades (id, market_id, buyer_id, seller_id, qty, price, fee, escrow_id, seq, ts)
                            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
                            ON CONFLICT (id) DO NOTHING
                            "#,
                        )
                        .bind(trade.id as i64)
                        .bind(trade.fill.market as i32)
                        .bind(trade.fill.buyer as i64)
                        .bind(trade.fill.seller as i64)
                        .bind(trade.fill.qty)
                        .bind(trade.fill.price)
                        .bind(trade.fee)
                        .bind(trade.escrow_id.map(|e| e as i64))
                        .bind(trade.fill.seq as i64)
                        .bind(trade.fill.ts_micros as i64)
                        .execute(&mut *tx)
                        .await?;
                    }
                    ProjectionEvent::EscrowUpserted { seq: _, escrow } => {
                        let state_str = format!("{:?}", escrow.state);
                        sqlx::query(
                            r#"
                            INSERT INTO escrows (id, trade_id, market_id, buyer_id, seller_id, qty, price, fee, state, delivered_at, deadline, receipt_bytes, receipt_sig, updated_seq)
                            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)
                            ON CONFLICT (id) DO UPDATE
                            SET state = $9, updated_seq = $14
                            "#,
                        )
                        .bind(escrow.id as i64)
                        .bind(escrow.trade_id as i64)
                        .bind(escrow.market_id as i32)
                        .bind(escrow.buyer_id as i64)
                        .bind(escrow.seller_id as i64)
                        .bind(escrow.qty as i32)
                        .bind(escrow.price)
                        .bind(escrow.fee)
                        .bind(state_str)
                        .bind(escrow.delivered_at as i64)
                        .bind(escrow.deadline as i64)
                        .bind(&escrow.receipt_bytes)
                        .bind(&escrow.receipt_sig)
                        .bind(escrow.updated_seq as i64)
                        .execute(&mut *tx)
                        .await?;
                    }
                    ProjectionEvent::DisputeOpened { seq: _, dispute } => {
                        sqlx::query(
                            r#"
                            INSERT INTO disputes (id, escrow_id, reason, note, status, ruling, ruled_by, ruled_at)
                            VALUES ($1, $2, $3, $4, $5, NULL, NULL, NULL)
                            ON CONFLICT (id) DO NOTHING
                            "#,
                        )
                        .bind(dispute.id as i64)
                        .bind(dispute.escrow_id as i64)
                        .bind(&dispute.reason)
                        .bind(&dispute.note)
                        .bind(&dispute.status)
                        .execute(&mut *tx)
                        .await?;
                    }
                    ProjectionEvent::DisputeResolved {
                        seq: _,
                        escrow_id,
                        ruling,
                        ruled_by,
                        ruled_at,
                        note,
                    } => {
                        sqlx::query(
                            r#"
                            UPDATE disputes
                            SET status = 'resolved', ruling = $2, ruled_by = $3, ruled_at = $4,
                                note = note || E'\nAdmin note: ' || $5
                            WHERE escrow_id = $1
                            "#,
                        )
                        .bind(*escrow_id as i64)
                        .bind(ruling)
                        .bind(*ruled_by as i64)
                        .bind(*ruled_at as i64)
                        .bind(note)
                        .execute(&mut *tx)
                        .await?;
                    }
                }

                last_seq = seq;
            }

            sqlx::query("UPDATE projection_state SET last_seq = $1 WHERE id = 1")
                .bind(last_seq as i64)
                .execute(&mut *tx)
                .await?;
        }

        tx.commit().await?;
        Ok(last_seq)
    }

    /// Truncates read model tables and rebuilds projection from scratch using the event log.
    pub async fn rebuild_from_scratch(
        &self,
        all_events: &[ProjectionEvent],
    ) -> Result<u64, ProjectionError> {
        let mut tx = self.pool.begin().await?;

        // Disable triggers temporarily or delete in order
        sqlx::query("ALTER TABLE ledger_entries DISABLE TRIGGER trg_immutable_ledger_entries")
            .execute(&mut *tx)
            .await?;
        sqlx::query("ALTER TABLE ledger_legs DISABLE TRIGGER trg_immutable_ledger_legs")
            .execute(&mut *tx)
            .await?;

        sqlx::query("DELETE FROM ledger_legs")
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM ledger_entries")
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM balances")
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM pass_balances")
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM trades").execute(&mut *tx).await?;
        sqlx::query("DELETE FROM disputes")
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM escrows").execute(&mut *tx).await?;
        sqlx::query("UPDATE projection_state SET last_seq = 0 WHERE id = 1")
            .execute(&mut *tx)
            .await?;

        sqlx::query("ALTER TABLE ledger_entries ENABLE TRIGGER trg_immutable_ledger_entries")
            .execute(&mut *tx)
            .await?;
        sqlx::query("ALTER TABLE ledger_legs ENABLE TRIGGER trg_immutable_ledger_legs")
            .execute(&mut *tx)
            .await?;

        tx.commit().await?;

        self.apply_batch(all_events).await
    }
}
