-- Darkhold Exchange - Stage 7 Public Audit Chain & Solvency Roots

-- 1. Public Audit Records (DKX-AUD-v1 hash chain, no user identifiers)
CREATE TABLE IF NOT EXISTS audit_records (
    seq BIGINT PRIMARY KEY,
    ts_micros BIGINT NOT NULL,
    prev_hash BYTEA NOT NULL,
    kind SMALLINT NOT NULL,
    market_id INTEGER NOT NULL,
    qty BIGINT NOT NULL,
    price_paise BIGINT NOT NULL,
    hash BYTEA NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_audit_records_seq ON audit_records(seq);

-- 2. Solvency Roots (Merkle-sum tree roots published periodically)
CREATE TABLE IF NOT EXISTS solvency_roots (
    seq BIGINT PRIMARY KEY,
    root_hash BYTEA NOT NULL,
    total_paise BIGINT NOT NULL,
    leaf_count INTEGER NOT NULL,
    published_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- 3. Stable Random 16-byte Salts for Users
CREATE TABLE IF NOT EXISTS user_salts (
    user_id BIGINT PRIMARY KEY,
    salt BYTEA NOT NULL
);
