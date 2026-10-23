-- Darkhold Exchange - Stage 3 Projection & Read Models

-- 1. Ledger Entries (Append-only journal header)
CREATE TABLE IF NOT EXISTS ledger_entries (
    id BIGINT PRIMARY KEY,
    seq BIGINT NOT NULL,
    idem_key NUMERIC(39, 0) UNIQUE NOT NULL,
    kind VARCHAR(32) NOT NULL,
    ts BIGINT NOT NULL
);

-- 2. Ledger Legs (Individual balanced transfers)
CREATE TABLE IF NOT EXISTS ledger_legs (
    id BIGSERIAL PRIMARY KEY,
    entry_id BIGINT NOT NULL REFERENCES ledger_entries(id) ON DELETE RESTRICT,
    account VARCHAR(64) NOT NULL,
    asset VARCHAR(32) NOT NULL,
    amount BIGINT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_ledger_legs_entry_id ON ledger_legs(entry_id);
CREATE INDEX IF NOT EXISTS idx_ledger_legs_account ON ledger_legs(account);

-- 3. Balances (User cash and locked balances)
CREATE TABLE IF NOT EXISTS balances (
    user_id BIGINT PRIMARY KEY,
    cash_avail BIGINT NOT NULL DEFAULT 0 CHECK (cash_avail >= 0),
    cash_locked BIGINT NOT NULL DEFAULT 0 CHECK (cash_locked >= 0),
    escrow_cash BIGINT NOT NULL DEFAULT 0 CHECK (escrow_cash >= 0),
    updated_seq BIGINT NOT NULL DEFAULT 0
);

-- 4. Pass Balances (User tier pass quantities)
CREATE TABLE IF NOT EXISTS pass_balances (
    user_id BIGINT NOT NULL,
    market_id INTEGER NOT NULL,
    avail BIGINT NOT NULL DEFAULT 0 CHECK (avail >= 0),
    locked BIGINT NOT NULL DEFAULT 0 CHECK (locked >= 0),
    held BIGINT NOT NULL DEFAULT 0 CHECK (held >= 0),
    PRIMARY KEY (user_id, market_id)
);

-- 5. Trades Read Model
CREATE TABLE IF NOT EXISTS trades (
    id BIGINT PRIMARY KEY,
    market_id INTEGER NOT NULL,
    buyer_id BIGINT NOT NULL,
    seller_id BIGINT NOT NULL,
    qty BIGINT NOT NULL CHECK (qty > 0),
    price BIGINT NOT NULL CHECK (price > 0),
    fee BIGINT NOT NULL DEFAULT 0 CHECK (fee >= 0),
    escrow_id BIGINT NULL,
    seq BIGINT NOT NULL,
    ts BIGINT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_trades_buyer ON trades(buyer_id);
CREATE INDEX IF NOT EXISTS idx_trades_seller ON trades(seller_id);
CREATE INDEX IF NOT EXISTS idx_trades_market ON trades(market_id);
CREATE INDEX IF NOT EXISTS idx_trades_ts ON trades(ts);

-- 6. Projection State (Checkpoint tracker)
CREATE TABLE IF NOT EXISTS projection_state (
    id INTEGER PRIMARY KEY DEFAULT 1 CHECK (id = 1),
    last_seq BIGINT NOT NULL DEFAULT 0
);

INSERT INTO projection_state (id, last_seq)
VALUES (1, 0)
ON CONFLICT (id) DO NOTHING;

-- 7. Immutability Triggers on Ledger Tables (No UPDATE or DELETE allowed)
CREATE OR REPLACE FUNCTION prevent_ledger_modification()
RETURNS TRIGGER AS $$
BEGIN
    RAISE EXCEPTION 'Ledger tables are append-only. UPDATE and DELETE operations are forbidden.';
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS trg_immutable_ledger_entries ON ledger_entries;
CREATE TRIGGER trg_immutable_ledger_entries
BEFORE UPDATE OR DELETE ON ledger_entries
FOR EACH ROW
EXECUTE FUNCTION prevent_ledger_modification();

DROP TRIGGER IF EXISTS trg_immutable_ledger_legs ON ledger_legs;
CREATE TRIGGER trg_immutable_ledger_legs
BEFORE UPDATE OR DELETE ON ledger_legs
FOR EACH ROW
EXECUTE FUNCTION prevent_ledger_modification();
