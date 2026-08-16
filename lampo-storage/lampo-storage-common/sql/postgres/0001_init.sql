-- Schema version 1: the key/value store and the payments table.
CREATE TABLE IF NOT EXISTS kv (
    primary_namespace   TEXT NOT NULL,
    secondary_namespace TEXT NOT NULL,
    key                 TEXT NOT NULL,
    value               BYTEA NOT NULL,
    PRIMARY KEY (primary_namespace, secondary_namespace, key)
);

CREATE TABLE IF NOT EXISTS payments (
    id           TEXT PRIMARY KEY,
    payment_hash TEXT NOT NULL,
    direction    TEXT NOT NULL,
    amount_msat  BIGINT NOT NULL,
    fee_msat     BIGINT,
    status       TEXT NOT NULL,
    created_at   BIGINT NOT NULL,
    invoice      TEXT
);

-- The whole reason payments are not in the key/value table: these turn
-- "every payment in a window" into an index scan.
CREATE INDEX IF NOT EXISTS payments_created_at
    ON payments (created_at);
CREATE INDEX IF NOT EXISTS payments_status_created_at
    ON payments (status, created_at);
