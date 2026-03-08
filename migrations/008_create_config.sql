-- Config management tables for web frontend hot-reload.
CREATE TABLE IF NOT EXISTS app_config (
    section    TEXT PRIMARY KEY,
    data       JSONB NOT NULL,
    version    INTEGER NOT NULL DEFAULT 1,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS config_history (
    id         SERIAL PRIMARY KEY,
    section    TEXT NOT NULL,
    data       JSONB NOT NULL,
    version    INTEGER NOT NULL,
    changed_by TEXT NOT NULL DEFAULT 'web',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_config_history_section ON config_history(section);
