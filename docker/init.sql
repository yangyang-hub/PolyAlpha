-- PolyAlpha Database Schema

-- Markets table
CREATE TABLE IF NOT EXISTS markets (
    condition_id    BYTEA PRIMARY KEY,
    question_id     BYTEA NOT NULL,
    question        TEXT NOT NULL,
    neg_risk        BOOLEAN DEFAULT FALSE,
    neg_risk_market_id BYTEA,
    tick_size       DECIMAL NOT NULL,
    fee_rate_bps    INTEGER NOT NULL,
    active          BOOLEAN DEFAULT TRUE,
    created_at      TIMESTAMPTZ DEFAULT NOW(),
    updated_at      TIMESTAMPTZ DEFAULT NOW()
);

-- Tokens table
CREATE TABLE IF NOT EXISTS tokens (
    token_id        TEXT PRIMARY KEY,
    condition_id    BYTEA REFERENCES markets(condition_id),
    outcome         VARCHAR(50) NOT NULL,
    complement_id   TEXT NOT NULL
);

-- Order book snapshots (for backtesting)
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

-- Trading opportunities
CREATE TABLE IF NOT EXISTS opportunities (
    id              UUID PRIMARY KEY,
    strategy_type   VARCHAR(50) NOT NULL,
    condition_id    BYTEA REFERENCES markets(condition_id),
    spread          DECIMAL NOT NULL,
    estimated_profit DECIMAL NOT NULL,
    actual_profit   DECIMAL,
    status          VARCHAR(20) NOT NULL,
    detected_at     TIMESTAMPTZ NOT NULL,
    executed_at     TIMESTAMPTZ,
    details         JSONB
);
CREATE INDEX IF NOT EXISTS idx_opp_status ON opportunities(status);
CREATE INDEX IF NOT EXISTS idx_opp_detected ON opportunities(detected_at);

-- Trade records
CREATE TABLE IF NOT EXISTS trades (
    id              UUID PRIMARY KEY,
    opportunity_id  UUID REFERENCES opportunities(id),
    order_id        VARCHAR(100),
    token_id        TEXT NOT NULL,
    side            VARCHAR(10) NOT NULL,
    price           DECIMAL NOT NULL,
    size            DECIMAL NOT NULL,
    filled_size     DECIMAL,
    fee             DECIMAL,
    tx_type         VARCHAR(20) NOT NULL,
    tx_hash         BYTEA,
    status          VARCHAR(20) NOT NULL,
    created_at      TIMESTAMPTZ DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_trades_opp ON trades(opportunity_id);

-- Positions
CREATE TABLE IF NOT EXISTS positions (
    token_id        TEXT PRIMARY KEY,
    condition_id    BYTEA,
    size            DECIMAL NOT NULL DEFAULT 0,
    avg_cost        DECIMAL NOT NULL DEFAULT 0,
    strategy_type   TEXT,
    updated_at      TIMESTAMPTZ DEFAULT NOW()
);

-- PnL log
CREATE TABLE IF NOT EXISTS pnl_log (
    id              BIGSERIAL PRIMARY KEY,
    timestamp       TIMESTAMPTZ NOT NULL,
    realized_pnl    DECIMAL NOT NULL,
    unrealized_pnl  DECIMAL NOT NULL,
    total_exposure  DECIMAL NOT NULL,
    usdc_balance    DECIMAL NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_pnl_ts ON pnl_log(timestamp);
