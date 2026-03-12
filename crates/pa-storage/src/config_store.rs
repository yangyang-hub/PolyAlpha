use std::collections::HashMap;

use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgPool;

/// Persistent config storage backed by PostgreSQL.
pub struct ConfigStore {
    pool: PgPool,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ConfigHistoryRow {
    pub id: i32,
    pub section: String,
    pub data: Value,
    pub version: i32,
    pub changed_by: String,
    pub created_at: DateTime<Utc>,
}

impl ConfigStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Load a config section. Returns (data, version) if found.
    pub async fn load_section(&self, section: &str) -> Result<Option<(Value, i32)>> {
        let row: Option<(Value, i32)> =
            sqlx::query_as("SELECT data, version FROM app_config WHERE section = $1")
                .bind(section)
                .fetch_optional(&self.pool)
                .await
                .context("load_section query failed")?;
        Ok(row)
    }

    /// Save a config section. Bumps version, writes history. Returns new version.
    pub async fn save_section(&self, section: &str, data: &Value) -> Result<i32> {
        let mut tx = self.pool.begin().await.context("begin tx")?;

        // Upsert into app_config
        let row: (i32,) = sqlx::query_as(
            r#"INSERT INTO app_config (section, data, version, updated_at)
               VALUES ($1, $2, 1, NOW())
               ON CONFLICT (section) DO UPDATE
                 SET data = $2,
                     version = app_config.version + 1,
                     updated_at = NOW()
               RETURNING version"#,
        )
        .bind(section)
        .bind(data)
        .fetch_one(&mut *tx)
        .await
        .context("upsert app_config")?;

        let new_version = row.0;

        // Write history
        sqlx::query(
            r#"INSERT INTO config_history (section, data, version, changed_by, created_at)
               VALUES ($1, $2, $3, 'web', NOW())"#,
        )
        .bind(section)
        .bind(data)
        .bind(new_version)
        .execute(&mut *tx)
        .await
        .context("insert config_history")?;

        tx.commit().await.context("commit tx")?;

        Ok(new_version)
    }

    /// Load all config sections into a HashMap.
    pub async fn load_all(&self) -> Result<HashMap<String, Value>> {
        let rows: Vec<(String, Value)> = sqlx::query_as("SELECT section, data FROM app_config")
            .fetch_all(&self.pool)
            .await
            .context("load_all query failed")?;

        Ok(rows.into_iter().collect())
    }

    /// Get change history for a section.
    pub async fn history(&self, section: &str, limit: i32) -> Result<Vec<ConfigHistoryRow>> {
        let rows = sqlx::query_as::<_, ConfigHistoryRow>(
            "SELECT id, section, data, version, changed_by, created_at
             FROM config_history
             WHERE section = $1
             ORDER BY created_at DESC
             LIMIT $2",
        )
        .bind(section)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .context("history query failed")?;

        Ok(rows)
    }

    /// Merge DB overrides into a base Settings loaded from TOML.
    pub fn apply_overrides(
        base: &mut pa_core::config::Settings,
        overrides: &HashMap<String, Value>,
    ) -> Result<()> {
        use pa_core::config::*;

        for (section, data) in overrides {
            match section.as_str() {
                "strategy" => {
                    base.strategy = serde_json::from_value::<StrategyConfig>(data.clone())
                        .context("invalid strategy override")?;
                }
                "risk" => {
                    base.risk = serde_json::from_value::<RiskConfig>(data.clone())
                        .context("invalid risk override")?;
                }
                "market_filter" => {
                    base.market_filter = serde_json::from_value::<MarketFilterConfig>(data.clone())
                        .context("invalid market_filter override")?;
                }
                "weather" => {
                    base.weather = serde_json::from_value::<WeatherConfig>(data.clone())
                        .context("invalid weather override")?;
                }
                "crypto_alpha" => {
                    base.crypto_alpha = serde_json::from_value::<CryptoAlphaConfig>(data.clone())
                        .context("invalid crypto_alpha override")?;
                }
                "event_calendar" => {
                    base.event_calendar =
                        serde_json::from_value::<EventCalendarConfig>(data.clone())
                            .context("invalid event_calendar override")?;
                }
                "liquidity_rewards" => {
                    base.liquidity_rewards =
                        serde_json::from_value::<LiquidityRewardsConfig>(data.clone())
                            .context("invalid liquidity_rewards override")?;
                }
                "smart_money" => {
                    base.smart_money = serde_json::from_value::<SmartMoneyConfig>(data.clone())
                        .context("invalid smart_money override")?;
                }
                other => {
                    tracing::warn!(section = other, "Unknown config section in DB, skipping");
                }
            }
        }
        Ok(())
    }
}

/// Validate a section by deserializing into the typed struct.
pub fn validate_section(section: &str, data: &Value) -> Result<()> {
    use pa_core::config::*;

    match section {
        "strategy" => {
            let _ = serde_json::from_value::<StrategyConfig>(data.clone())?;
        }
        "risk" => {
            let _ = serde_json::from_value::<RiskConfig>(data.clone())?;
        }
        "market_filter" => {
            let _ = serde_json::from_value::<MarketFilterConfig>(data.clone())?;
        }
        "weather" => {
            let _ = serde_json::from_value::<WeatherConfig>(data.clone())?;
        }
        "crypto_alpha" => {
            let _ = serde_json::from_value::<CryptoAlphaConfig>(data.clone())?;
        }
        "event_calendar" => {
            let _ = serde_json::from_value::<EventCalendarConfig>(data.clone())?;
        }
        "liquidity_rewards" => {
            let _ = serde_json::from_value::<LiquidityRewardsConfig>(data.clone())?;
        }
        "smart_money" => {
            let _ = serde_json::from_value::<SmartMoneyConfig>(data.clone())?;
        }
        _ => return Err(anyhow!("Unknown config section: {}", section)),
    }
    Ok(())
}

/// Extract a single section from Settings as JSON Value.
pub fn extract_section(settings: &pa_core::config::Settings, section: &str) -> Result<Value> {
    match section {
        "strategy" => Ok(serde_json::to_value(&settings.strategy)?),
        "risk" => Ok(serde_json::to_value(&settings.risk)?),
        "market_filter" => Ok(serde_json::to_value(&settings.market_filter)?),
        "weather" => Ok(serde_json::to_value(&settings.weather)?),
        "crypto_alpha" => Ok(serde_json::to_value(&settings.crypto_alpha)?),
        "event_calendar" => Ok(serde_json::to_value(&settings.event_calendar)?),
        "liquidity_rewards" => Ok(serde_json::to_value(&settings.liquidity_rewards)?),
        "smart_money" => Ok(serde_json::to_value(&settings.smart_money)?),
        _ => Err(anyhow!("Unknown config section: {}", section)),
    }
}
