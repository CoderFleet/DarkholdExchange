-- Row 21 read-model schema, created idempotently by `dkx_projection::bootstrap`.
-- The gateway's HTTP routes still serve from the in-memory core; these tables
-- are the PostgreSQL replica the projection worker keeps up to date.

CREATE TABLE IF NOT EXISTS projection_state (
    id INTEGER PRIMARY KEY,
    last_seq BIGINT NOT NULL DEFAULT 0
);

INSERT INTO projection_state (id, last_seq) VALUES (1, 0) ON CONFLICT (id) DO NOTHING;

CREATE TABLE IF NOT EXISTS ledger_entries (
    id BIGINT PRIMARY KEY,
    seq BIGINT NOT NULL,
    idem_key NUMERIC NOT NULL UNIQUE,
    kind TEXT NOT NULL,
    ts BIGINT NOT NULL
);

CREATE TABLE IF NOT EXISTS ledger_legs (
    entry_id BIGINT NOT NULL REFERENCES ledger_entries(id),
    account TEXT NOT NULL,
    asset TEXT NOT NULL,
    amount BIGINT NOT NULL
);

CREATE TABLE IF NOT EXISTS balances (
    user_id BIGINT PRIMARY KEY,
    cash_avail BIGINT NOT NULL,
    cash_locked BIGINT NOT NULL,
    escrow_cash BIGINT NOT NULL,
    updated_seq BIGINT NOT NULL
);

CREATE TABLE IF NOT EXISTS pass_balances (
    user_id BIGINT,
    market_id INTEGER,
    avail BIGINT NOT NULL,
    locked BIGINT NOT NULL,
    held BIGINT NOT NULL,
    PRIMARY KEY (user_id, market_id)
);

CREATE TABLE IF NOT EXISTS trades (
    id BIGINT PRIMARY KEY,
    market_id INTEGER,
    buyer_id BIGINT,
    seller_id BIGINT,
    qty BIGINT,
    price BIGINT,
    fee BIGINT,
    escrow_id BIGINT,
    seq BIGINT,
    ts BIGINT
);

CREATE TABLE IF NOT EXISTS escrows (
    id BIGINT PRIMARY KEY,
    trade_id BIGINT,
    market_id INTEGER,
    buyer_id BIGINT,
    seller_id BIGINT,
    qty INTEGER,
    price BIGINT,
    fee BIGINT,
    state TEXT,
    delivered_at BIGINT,
    deadline BIGINT,
    receipt_bytes BYTEA,
    receipt_sig BYTEA,
    updated_seq BIGINT
);

CREATE TABLE IF NOT EXISTS disputes (
    id BIGINT PRIMARY KEY,
    escrow_id BIGINT,
    reason TEXT,
    note TEXT,
    status TEXT,
    ruling TEXT,
    ruled_by BIGINT,
    ruled_at BIGINT
);
