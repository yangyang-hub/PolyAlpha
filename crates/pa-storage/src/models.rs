use chrono::NaiveDate;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Database model for the `markets` table.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct MarketRow {
    pub condition_id: Vec<u8>,
    pub question_id: Vec<u8>,
    pub question: String,
    pub neg_risk: bool,
    pub neg_risk_market_id: Option<Vec<u8>>,
    pub tick_size: Decimal,
    pub fee_rate_bps: i32,
    pub active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Database model for the `tokens` table.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct TokenRow {
    pub token_id: String,
    pub condition_id: Vec<u8>,
    pub outcome: String,
    pub complement_id: String,
}

/// Database model for the `opportunities` table.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct OpportunityRow {
    pub id: Uuid,
    pub strategy_type: String,
    pub condition_id: Vec<u8>,
    pub spread: Decimal,
    pub estimated_profit: Decimal,
    pub actual_profit: Option<Decimal>,
    pub status: String,
    pub detected_at: DateTime<Utc>,
    pub executed_at: Option<DateTime<Utc>>,
    pub details: Option<serde_json::Value>,
}

/// Database model for the `trades` table.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct TradeRow {
    pub id: Uuid,
    pub opportunity_id: Option<Uuid>,
    pub order_id: Option<String>,
    pub token_id: String,
    pub side: String,
    pub price: Decimal,
    pub size: Decimal,
    pub filled_size: Option<Decimal>,
    pub fee: Option<Decimal>,
    pub tx_type: String,
    pub tx_hash: Option<Vec<u8>>,
    pub status: String,
    pub created_at: DateTime<Utc>,
}

/// Database model for the `orderbook_snapshots` table.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct OrderBookSnapshotRow {
    pub id: i64,
    pub token_id: String,
    pub timestamp: DateTime<Utc>,
    pub bids: serde_json::Value,
    pub asks: serde_json::Value,
    pub best_bid: Option<Decimal>,
    pub best_ask: Option<Decimal>,
    pub midpoint: Option<Decimal>,
}

/// Database model for the `positions` table.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct PositionRow {
    pub token_id: String,
    pub size: Decimal,
    pub avg_cost: Decimal,
    pub updated_at: DateTime<Utc>,
    pub strategy_type: Option<String>,
    pub condition_id: Option<Vec<u8>>,
}

/// Database model for the `pnl_log` table.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct PnlLogRow {
    pub id: i64,
    pub timestamp: DateTime<Utc>,
    pub realized_pnl: Decimal,
    pub unrealized_pnl: Decimal,
    pub total_exposure: Decimal,
    pub usdc_balance: Decimal,
}

/// Database model for the `weather_forecast_snapshots` table.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct WeatherForecastSnapshotRow {
    pub id: i64,
    pub provider: String,
    pub location: String,
    pub metric: String,
    pub target_date: NaiveDate,
    pub recorded_at: DateTime<Utc>,
    pub target_value: Option<f64>,
    pub mean: f64,
    pub std_dev: f64,
    pub model_spread: f64,
    pub values: serde_json::Value,
    pub dates: serde_json::Value,
}
