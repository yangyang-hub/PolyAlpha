-- Create markets table
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

CREATE TABLE IF NOT EXISTS tokens (
    token_id        TEXT PRIMARY KEY,
    condition_id    BYTEA REFERENCES markets(condition_id),
    outcome         VARCHAR(50) NOT NULL,
    complement_id   TEXT NOT NULL
);
