-- Create positions and PnL tables
CREATE TABLE IF NOT EXISTS positions (
    token_id        TEXT PRIMARY KEY,
    condition_id    BYTEA REFERENCES markets(condition_id),
    size            DECIMAL NOT NULL DEFAULT 0,
    avg_cost        DECIMAL NOT NULL DEFAULT 0,
    updated_at      TIMESTAMPTZ DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS pnl_log (
    id              BIGSERIAL PRIMARY KEY,
    timestamp       TIMESTAMPTZ NOT NULL,
    realized_pnl    DECIMAL NOT NULL,
    unrealized_pnl  DECIMAL NOT NULL,
    total_exposure  DECIMAL NOT NULL,
    usdc_balance    DECIMAL NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_pnl_ts ON pnl_log(timestamp);
