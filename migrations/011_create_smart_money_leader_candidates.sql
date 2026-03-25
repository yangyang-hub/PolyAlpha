CREATE TABLE IF NOT EXISTS smart_money_leader_candidates (
    address TEXT PRIMARY KEY,
    label TEXT NOT NULL DEFAULT '',
    source_tags JSONB NOT NULL DEFAULT '[]'::jsonb,
    first_seen_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_seen_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    leaderboard_rank INTEGER,
    leaderboard_volume DECIMAL NOT NULL DEFAULT 0,
    leaderboard_pnl DECIMAL NOT NULL DEFAULT 0,
    open_positions_count INTEGER NOT NULL DEFAULT 0,
    open_notional DECIMAL NOT NULL DEFAULT 0,
    closed_positions_count INTEGER NOT NULL DEFAULT 0,
    closed_total_bought DECIMAL NOT NULL DEFAULT 0,
    closed_realized_pnl DECIMAL NOT NULL DEFAULT 0,
    sampled_markets INTEGER NOT NULL DEFAULT 0,
    market_position_count INTEGER NOT NULL DEFAULT 0,
    holder_position_count INTEGER NOT NULL DEFAULT 0,
    activity_volume DECIMAL NOT NULL DEFAULT 0,
    activity_pnl DECIMAL NOT NULL DEFAULT 0,
    verified BOOLEAN NOT NULL DEFAULT FALSE,
    discovery_score DECIMAL NOT NULL DEFAULT 0,
    promoted BOOLEAN NOT NULL DEFAULT FALSE,
    metadata JSONB,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_sm_leader_candidates_score
ON smart_money_leader_candidates(discovery_score DESC, last_seen_at DESC);
