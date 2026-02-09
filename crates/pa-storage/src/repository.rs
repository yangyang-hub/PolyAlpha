use sqlx::PgPool;

use crate::models::{OpportunityRow, TradeRow, OrderBookSnapshotRow};

/// Repository for database operations.
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
        sqlx::migrate!("../../migrations")
            .run(&self.pool)
            .await?;
        tracing::info!("Database migrations applied");
        Ok(())
    }

    /// Insert an arbitrage opportunity record.
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
    pub async fn insert_orderbook_snapshot(&self, row: &OrderBookSnapshotRow) -> anyhow::Result<()> {
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
}
