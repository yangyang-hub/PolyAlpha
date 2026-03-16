CREATE TABLE IF NOT EXISTS weather_forecast_snapshots (
    id           BIGSERIAL PRIMARY KEY,
    provider     TEXT NOT NULL,
    location     TEXT NOT NULL,
    metric       TEXT NOT NULL,
    target_date  DATE NOT NULL,
    recorded_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    target_value DOUBLE PRECISION,
    mean         DOUBLE PRECISION NOT NULL,
    std_dev      DOUBLE PRECISION NOT NULL,
    model_spread DOUBLE PRECISION NOT NULL DEFAULT 0,
    values       JSONB NOT NULL,
    dates        JSONB NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_weather_forecast_snapshots_lookup
ON weather_forecast_snapshots(provider, location, metric, target_date, recorded_at DESC);
