-- Create trades table
CREATE TABLE IF NOT EXISTS trades (
    id              UUID PRIMARY KEY,
    opportunity_id  UUID,
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
