-- Create order book snapshots table
CREATE TABLE IF NOT EXISTS orderbook_snapshots (
    id              BIGSERIAL PRIMARY KEY,
    token_id        TEXT NOT NULL,
    timestamp       TIMESTAMPTZ NOT NULL,
    bids            JSONB NOT NULL,
    asks            JSONB NOT NULL,
    best_bid        DECIMAL,
    best_ask        DECIMAL,
    midpoint        DECIMAL
);
CREATE INDEX IF NOT EXISTS idx_ob_snap_token_ts ON orderbook_snapshots(token_id, timestamp);
