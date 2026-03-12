use chrono::{DateTime, Utc};
use sqlx::PgPool;

use rust_decimal::Decimal;

use crate::models::{
    MarketRow, OpportunityRow, OrderBookSnapshotRow, PositionRow, TokenRow, TradeRow,
};

/// Repository for database operations.
#[derive(Clone)]
pub struct Repository {
    pool: PgPool,
}

impl Repository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Create a new database connection pool.
    pub async fn connect(database_url: &str, max_connections: u32) -> anyhow::Result<Self> {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(max_connections)
            .connect(database_url)
            .await?;

        tracing::info!("Database connection pool established");
        Ok(Self { pool })
    }

    /// Run pending migrations.
    pub async fn migrate(&self) -> anyhow::Result<()> {
        sqlx::migrate!("../../migrations").run(&self.pool).await?;
        tracing::info!("Database migrations applied");
        Ok(())
    }

    /// Insert a trading opportunity record.
    pub async fn insert_opportunity(&self, row: &OpportunityRow) -> anyhow::Result<()> {
        sqlx::query(
            r#"INSERT INTO opportunities
            (id, strategy_type, condition_id, spread, estimated_profit, actual_profit, status, detected_at, executed_at, details)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)"#,
        )
        .bind(row.id)
        .bind(&row.strategy_type)
        .bind(&row.condition_id)
        .bind(row.spread)
        .bind(row.estimated_profit)
        .bind(row.actual_profit)
        .bind(&row.status)
        .bind(row.detected_at)
        .bind(row.executed_at)
        .bind(&row.details)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Insert a trade record.
    pub async fn insert_trade(&self, row: &TradeRow) -> anyhow::Result<()> {
        sqlx::query(
            r#"INSERT INTO trades
            (id, opportunity_id, order_id, token_id, side, price, size, filled_size, fee, tx_type, tx_hash, status, created_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)"#,
        )
        .bind(row.id)
        .bind(row.opportunity_id)
        .bind(&row.order_id)
        .bind(&row.token_id)
        .bind(&row.side)
        .bind(row.price)
        .bind(row.size)
        .bind(row.filled_size)
        .bind(row.fee)
        .bind(&row.tx_type)
        .bind(&row.tx_hash)
        .bind(&row.status)
        .bind(row.created_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Insert an order book snapshot (for backtesting).
    pub async fn insert_orderbook_snapshot(
        &self,
        row: &OrderBookSnapshotRow,
    ) -> anyhow::Result<()> {
        sqlx::query(
            r#"INSERT INTO orderbook_snapshots
            (token_id, timestamp, bids, asks, best_bid, best_ask, midpoint)
            VALUES ($1, $2, $3, $4, $5, $6, $7)"#,
        )
        .bind(&row.token_id)
        .bind(row.timestamp)
        .bind(&row.bids)
        .bind(&row.asks)
        .bind(row.best_bid)
        .bind(row.best_ask)
        .bind(row.midpoint)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Get a reference to the connection pool.
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Load order book snapshots for given tokens within a time range.
    ///
    /// Results are ordered by timestamp ascending for chronological replay.
    pub async fn load_snapshots(
        &self,
        token_ids: &[String],
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> anyhow::Result<Vec<OrderBookSnapshotRow>> {
        let rows = sqlx::query_as::<_, OrderBookSnapshotRow>(
            r#"SELECT id, token_id, timestamp, bids, asks, best_bid, best_ask, midpoint
            FROM orderbook_snapshots
            WHERE token_id = ANY($1) AND timestamp >= $2 AND timestamp <= $3
            ORDER BY timestamp ASC"#,
        )
        .bind(token_ids)
        .bind(from)
        .bind(to)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// Load market metadata for given condition IDs.
    pub async fn load_markets(&self, condition_ids: &[Vec<u8>]) -> anyhow::Result<Vec<MarketRow>> {
        let rows = sqlx::query_as::<_, MarketRow>(
            r#"SELECT condition_id, question_id, question, neg_risk, neg_risk_market_id,
                      tick_size, fee_rate_bps, active, created_at, updated_at
            FROM markets
            WHERE condition_id = ANY($1)"#,
        )
        .bind(condition_ids)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// Load token info for given condition IDs.
    pub async fn load_tokens(&self, condition_ids: &[Vec<u8>]) -> anyhow::Result<Vec<TokenRow>> {
        let rows = sqlx::query_as::<_, TokenRow>(
            r#"SELECT token_id, condition_id, outcome, complement_id
            FROM tokens
            WHERE condition_id = ANY($1)"#,
        )
        .bind(condition_ids)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// Load all markets from the database.
    pub async fn load_all_markets(&self) -> anyhow::Result<Vec<MarketRow>> {
        let rows = sqlx::query_as::<_, MarketRow>(
            r#"SELECT condition_id, question_id, question, neg_risk, neg_risk_market_id,
                      tick_size, fee_rate_bps, active, created_at, updated_at
            FROM markets WHERE active = true"#,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// Load all tokens from the database.
    pub async fn load_all_tokens(&self) -> anyhow::Result<Vec<TokenRow>> {
        let rows = sqlx::query_as::<_, TokenRow>(
            r#"SELECT token_id, condition_id, outcome, complement_id
            FROM tokens"#,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// Get all distinct token IDs that have snapshots in the given time range.
    pub async fn snapshot_token_ids(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> anyhow::Result<Vec<String>> {
        let rows: Vec<(String,)> = sqlx::query_as(
            r#"SELECT DISTINCT token_id FROM orderbook_snapshots
            WHERE timestamp >= $1 AND timestamp <= $2"#,
        )
        .bind(from)
        .bind(to)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|(id,)| id).collect())
    }

    /// Load all non-zero positions from the database.
    pub async fn load_positions(&self) -> anyhow::Result<Vec<PositionRow>> {
        let rows = sqlx::query_as::<_, PositionRow>(
            r#"SELECT token_id, size, avg_cost, updated_at, strategy_type, condition_id
            FROM positions WHERE size > 0"#,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// Upsert a position row. Uses COALESCE to preserve existing condition_id/strategy_type.
    pub async fn upsert_position(
        &self,
        token_id: &str,
        size: Decimal,
        avg_cost: Decimal,
        strategy_type: Option<&str>,
        condition_id: Option<&[u8]>,
    ) -> anyhow::Result<()> {
        sqlx::query(
            r#"INSERT INTO positions (token_id, size, avg_cost, strategy_type, condition_id, updated_at)
            VALUES ($1, $2, $3, $4, $5, NOW())
            ON CONFLICT (token_id) DO UPDATE
            SET size = $2, avg_cost = $3,
                strategy_type = COALESCE($4, positions.strategy_type),
                condition_id = COALESCE($5, positions.condition_id),
                updated_at = NOW()"#,
        )
        .bind(token_id)
        .bind(size)
        .bind(avg_cost)
        .bind(strategy_type)
        .bind(condition_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Delete positions with zero or negative size.
    pub async fn cleanup_zero_positions(&self) -> anyhow::Result<u64> {
        let result = sqlx::query("DELETE FROM positions WHERE size <= 0")
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }
}
