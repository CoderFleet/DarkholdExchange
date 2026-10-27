-- Darkhold Exchange - Stage 6 Escrow and Disputes Read Models

CREATE TABLE IF NOT EXISTS escrows (
    id BIGINT PRIMARY KEY,
    trade_id BIGINT NOT NULL,
    market_id INTEGER NOT NULL,
    buyer_id BIGINT NOT NULL,
    seller_id BIGINT NOT NULL,
    qty INTEGER NOT NULL CHECK (qty > 0),
    price BIGINT NOT NULL CHECK (price > 0),
    fee BIGINT NOT NULL DEFAULT 0,
    state VARCHAR(32) NOT NULL,
    delivered_at BIGINT NOT NULL,
    deadline BIGINT NOT NULL,
    receipt_bytes BYTEA NOT NULL,
    receipt_sig BYTEA NOT NULL,
    updated_seq BIGINT NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_escrows_buyer_id ON escrows(buyer_id);
CREATE INDEX IF NOT EXISTS idx_escrows_seller_id ON escrows(seller_id);
CREATE INDEX IF NOT EXISTS idx_escrows_state ON escrows(state);
CREATE INDEX IF NOT EXISTS idx_escrows_trade_id ON escrows(trade_id);

CREATE TABLE IF NOT EXISTS disputes (
    id BIGSERIAL PRIMARY KEY,
    escrow_id BIGINT NOT NULL REFERENCES escrows(id),
    reason TEXT NOT NULL,
    note TEXT NOT NULL,
    status VARCHAR(32) NOT NULL,
    ruling VARCHAR(32) NULL,
    ruled_by BIGINT NULL,
    ruled_at BIGINT NULL
);

CREATE INDEX IF NOT EXISTS idx_disputes_escrow_id ON disputes(escrow_id);
CREATE INDEX IF NOT EXISTS idx_disputes_status ON disputes(status);
