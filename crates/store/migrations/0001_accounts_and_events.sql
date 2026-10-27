-- Accounts, events and tiers (dx-store). Safe to run on every start: every statement is
-- "if not exists".

CREATE TABLE IF NOT EXISTS app_users (
    user_id       BIGINT      PRIMARY KEY,
    username      TEXT        NOT NULL UNIQUE,
    role          TEXT        NOT NULL CHECK (role IN ('user', 'admin', 'issuer')),
    -- Only the Argon2id hash, never the password.
    password_hash TEXT        NOT NULL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS admin_events (
    event_id   BIGINT      PRIMARY KEY,
    name       TEXT        NOT NULL,
    venue      TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS admin_tiers (
    market_id  BIGINT      PRIMARY KEY,
    event_id   BIGINT      NOT NULL REFERENCES admin_events (event_id),
    tier_name  TEXT        NOT NULL,
    -- The whole MarketConfig the engine was (or will be) given.
    config     JSONB       NOT NULL,
    published  BOOLEAN     NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- One tier name per event, ignoring capitals ("General" and "general" are the same tier).
CREATE UNIQUE INDEX IF NOT EXISTS admin_tiers_event_name
    ON admin_tiers (event_id, lower(tier_name));