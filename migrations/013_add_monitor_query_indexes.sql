CREATE INDEX IF NOT EXISTS idx_trades_created_at_desc
ON trades(created_at DESC);

CREATE INDEX IF NOT EXISTS idx_config_history_section_created_at_desc
ON config_history(section, created_at DESC);

CREATE INDEX IF NOT EXISTS idx_config_history_section_changed_by_created_at_desc
ON config_history(section, changed_by, created_at DESC);
