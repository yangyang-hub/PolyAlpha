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
    pub details: Option<serde_json::Value>,
}

/// Joined trade + opportunity history row for account-facing history APIs.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct TradeHistoryRow {
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
    pub strategy_type: Option<String>,
    pub condition_id: Option<Vec<u8>>,
    pub question: Option<String>,
    pub account_name: Option<String>,
    pub proxy_wallet: Option<String>,
    pub opportunity_status: Option<String>,
    pub estimated_profit: Option<Decimal>,
    pub actual_profit: Option<Decimal>,
    pub detected_at: Option<DateTime<Utc>>,
    pub executed_at: Option<DateTime<Utc>>,
    pub details: Option<serde_json::Value>,
    pub trade_details: Option<serde_json::Value>,
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

/// Database model for discovered smart-money leader candidates.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct SmartMoneyLeaderCandidateRow {
    pub address: String,
    pub label: String,
    pub source_tags: serde_json::Value,
    pub first_seen_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub leaderboard_rank: Option<i32>,
    pub leaderboard_volume: Decimal,
    pub leaderboard_pnl: Decimal,
    pub open_positions_count: i32,
    pub open_notional: Decimal,
    pub closed_positions_count: i32,
    pub closed_total_bought: Decimal,
    pub closed_realized_pnl: Decimal,
    pub sampled_markets: i32,
    pub market_position_count: i32,
    pub holder_position_count: i32,
    pub activity_volume: Decimal,
    pub activity_pnl: Decimal,
    pub verified: bool,
    pub discovery_score: Decimal,
    pub promoted: bool,
    pub metadata: Option<serde_json::Value>,
    pub updated_at: DateTime<Utc>,
}

/// Database model for the `config_history` table.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct ConfigHistoryRow {
    pub id: i64,
    pub section: String,
    pub data: serde_json::Value,
    pub version: i32,
    pub changed_by: String,
    pub created_at: DateTime<Utc>,
}
