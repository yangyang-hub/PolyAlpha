-- Create opportunities table
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

-- Add FK now that both tables exist
ALTER TABLE trades ADD CONSTRAINT fk_trades_opp FOREIGN KEY (opportunity_id) REFERENCES opportunities(id);
CREATE INDEX IF NOT EXISTS idx_trades_opp ON trades(opportunity_id);
