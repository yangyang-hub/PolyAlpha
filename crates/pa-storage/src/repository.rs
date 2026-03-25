use chrono::{DateTime, Utc};
use sqlx::PgPool;

use rust_decimal::Decimal;
use serde_json::Value as JsonValue;

use crate::models::{
    ConfigHistoryRow, MarketRow, OpportunityRow, OrderBookSnapshotRow, PositionRow,
    SmartMoneyLeaderCandidateRow, TokenRow, TradeHistoryRow, TradeRow, WeatherForecastSnapshotRow,
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

    /// Insert or update a trading opportunity record keyed by `id`.
    pub async fn upsert_opportunity(&self, row: &OpportunityRow) -> anyhow::Result<()> {
        sqlx::query(
            r#"INSERT INTO opportunities
            (id, strategy_type, condition_id, spread, estimated_profit, actual_profit, status, detected_at, executed_at, details)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
            ON CONFLICT (id) DO UPDATE
            SET strategy_type = EXCLUDED.strategy_type,
                condition_id = EXCLUDED.condition_id,
                spread = EXCLUDED.spread,
                estimated_profit = EXCLUDED.estimated_profit,
                actual_profit = EXCLUDED.actual_profit,
                status = EXCLUDED.status,
                detected_at = EXCLUDED.detected_at,
                executed_at = EXCLUDED.executed_at,
                details = EXCLUDED.details"#,
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
            (id, opportunity_id, order_id, token_id, side, price, size, filled_size, fee, tx_type, tx_hash, status, created_at, details)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)"#,
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
        .bind(&row.details)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Upsert a market metadata row from live discovery.
    pub async fn upsert_market(
        &self,
        condition_id: &[u8],
        question_id: &[u8],
        question: &str,
        neg_risk: bool,
        neg_risk_market_id: Option<&[u8]>,
        tick_size: Decimal,
        fee_rate_bps: i32,
        active: bool,
    ) -> anyhow::Result<()> {
        sqlx::query(
            r#"INSERT INTO markets
            (condition_id, question_id, question, neg_risk, neg_risk_market_id, tick_size, fee_rate_bps, active, created_at, updated_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, NOW(), NOW())
            ON CONFLICT (condition_id) DO UPDATE
            SET question_id = EXCLUDED.question_id,
                question = EXCLUDED.question,
                neg_risk = EXCLUDED.neg_risk,
                neg_risk_market_id = EXCLUDED.neg_risk_market_id,
                tick_size = EXCLUDED.tick_size,
                fee_rate_bps = EXCLUDED.fee_rate_bps,
                active = EXCLUDED.active,
                updated_at = NOW()"#,
        )
        .bind(condition_id)
        .bind(question_id)
        .bind(question)
        .bind(neg_risk)
        .bind(neg_risk_market_id)
        .bind(tick_size)
        .bind(fee_rate_bps)
        .bind(active)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Upsert a token metadata row from live discovery.
    pub async fn upsert_token(
        &self,
        token_id: &str,
        condition_id: &[u8],
        outcome: &str,
        complement_id: &str,
    ) -> anyhow::Result<()> {
        sqlx::query(
            r#"INSERT INTO tokens
            (token_id, condition_id, outcome, complement_id)
            VALUES ($1, $2, $3, $4)
            ON CONFLICT (token_id) DO UPDATE
            SET condition_id = EXCLUDED.condition_id,
                outcome = EXCLUDED.outcome,
                complement_id = EXCLUDED.complement_id"#,
        )
        .bind(token_id)
        .bind(condition_id)
        .bind(outcome)
        .bind(complement_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Load recent trade history, optionally filtered by strategy/account/proxy wallet.
    pub async fn load_trade_history(
        &self,
        limit: i64,
        strategy_type: Option<&str>,
        account_name: Option<&str>,
        proxy_wallet: Option<&str>,
    ) -> anyhow::Result<Vec<TradeHistoryRow>> {
        let rows = sqlx::query_as::<_, TradeHistoryRow>(
            r#"SELECT
                   t.id,
                   t.opportunity_id,
                   t.order_id,
                   t.token_id,
                   t.side,
                   t.price,
                   t.size,
                   t.filled_size,
                   t.fee,
                   t.tx_type,
                   t.tx_hash,
                   t.status,
                   t.created_at,
                   o.strategy_type,
                   o.condition_id,
                   o.details ->> 'question' AS question,
                   o.details ->> 'account_name' AS account_name,
                   o.details ->> 'proxy_wallet' AS proxy_wallet,
                   o.status AS opportunity_status,
                   o.estimated_profit,
                   o.actual_profit,
                   o.detected_at,
                   o.executed_at,
                   o.details,
                   t.details AS trade_details
               FROM trades t
               LEFT JOIN opportunities o ON o.id = t.opportunity_id
               WHERE ($2::text IS NULL OR o.strategy_type = $2)
                 AND ($3::text IS NULL OR o.details ->> 'account_name' = $3)
                 AND ($4::text IS NULL OR lower(o.details ->> 'proxy_wallet') = lower($4))
               ORDER BY t.created_at DESC
               LIMIT $1"#,
        )
        .bind(limit)
        .bind(strategy_type)
        .bind(account_name)
        .bind(proxy_wallet)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
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

    /// Insert a weather forecast snapshot row for later audit/replay.
    pub async fn insert_weather_forecast_snapshot(
        &self,
        row: &WeatherForecastSnapshotRow,
    ) -> anyhow::Result<()> {
        sqlx::query(
            r#"INSERT INTO weather_forecast_snapshots
            (provider, location, metric, target_date, recorded_at, target_value, mean, std_dev, model_spread, values, dates)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)"#,
        )
        .bind(&row.provider)
        .bind(&row.location)
        .bind(&row.metric)
        .bind(row.target_date)
        .bind(row.recorded_at)
        .bind(row.target_value)
        .bind(row.mean)
        .bind(row.std_dev)
        .bind(row.model_spread)
        .bind(&row.values)
        .bind(&row.dates)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Load the latest archived forecast snapshot for a provider/location/metric/date tuple.
    pub async fn load_latest_weather_forecast_snapshot(
        &self,
        provider: &str,
        location: &str,
        metric: &str,
        target_date: chrono::NaiveDate,
    ) -> anyhow::Result<Option<WeatherForecastSnapshotRow>> {
        let row = sqlx::query_as::<_, WeatherForecastSnapshotRow>(
            r#"SELECT id, provider, location, metric, target_date, recorded_at, target_value,
                      mean, std_dev, model_spread, values, dates
            FROM weather_forecast_snapshots
            WHERE provider = $1 AND location = $2 AND metric = $3 AND target_date = $4
            ORDER BY recorded_at DESC
            LIMIT 1"#,
        )
        .bind(provider)
        .bind(location)
        .bind(metric)
        .bind(target_date)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    /// Insert or update a discovered smart-money leader candidate.
    pub async fn upsert_smart_money_leader_candidate(
        &self,
        row: &SmartMoneyLeaderCandidateRow,
    ) -> anyhow::Result<()> {
        sqlx::query(
            r#"INSERT INTO smart_money_leader_candidates
            (address, label, source_tags, first_seen_at, last_seen_at, leaderboard_rank,
             leaderboard_volume, leaderboard_pnl, open_positions_count, open_notional,
             closed_positions_count, closed_total_bought, closed_realized_pnl, sampled_markets,
             market_position_count, holder_position_count, activity_volume, activity_pnl,
             verified, discovery_score, promoted, metadata, updated_at)
            VALUES
            ($1, $2, $3, $4, $5, $6,
             $7, $8, $9, $10,
             $11, $12, $13, $14,
             $15, $16, $17, $18,
             $19, $20, $21, $22, NOW())
            ON CONFLICT (address) DO UPDATE
            SET label = EXCLUDED.label,
                source_tags = EXCLUDED.source_tags,
                first_seen_at = LEAST(smart_money_leader_candidates.first_seen_at, EXCLUDED.first_seen_at),
                last_seen_at = GREATEST(smart_money_leader_candidates.last_seen_at, EXCLUDED.last_seen_at),
                leaderboard_rank = EXCLUDED.leaderboard_rank,
                leaderboard_volume = EXCLUDED.leaderboard_volume,
                leaderboard_pnl = EXCLUDED.leaderboard_pnl,
                open_positions_count = EXCLUDED.open_positions_count,
                open_notional = EXCLUDED.open_notional,
                closed_positions_count = EXCLUDED.closed_positions_count,
                closed_total_bought = EXCLUDED.closed_total_bought,
                closed_realized_pnl = EXCLUDED.closed_realized_pnl,
                sampled_markets = EXCLUDED.sampled_markets,
                market_position_count = EXCLUDED.market_position_count,
                holder_position_count = EXCLUDED.holder_position_count,
                activity_volume = EXCLUDED.activity_volume,
                activity_pnl = EXCLUDED.activity_pnl,
                verified = EXCLUDED.verified,
                discovery_score = EXCLUDED.discovery_score,
                promoted = EXCLUDED.promoted,
                metadata = EXCLUDED.metadata,
                updated_at = NOW()"#,
        )
        .bind(&row.address)
        .bind(&row.label)
        .bind(&row.source_tags)
        .bind(row.first_seen_at)
        .bind(row.last_seen_at)
        .bind(row.leaderboard_rank)
        .bind(row.leaderboard_volume)
        .bind(row.leaderboard_pnl)
        .bind(row.open_positions_count)
        .bind(row.open_notional)
        .bind(row.closed_positions_count)
        .bind(row.closed_total_bought)
        .bind(row.closed_realized_pnl)
        .bind(row.sampled_markets)
        .bind(row.market_position_count)
        .bind(row.holder_position_count)
        .bind(row.activity_volume)
        .bind(row.activity_pnl)
        .bind(row.verified)
        .bind(row.discovery_score)
        .bind(row.promoted)
        .bind(&row.metadata)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Load top discovered smart-money leader candidates ordered by score.
    pub async fn load_smart_money_leader_candidates(
        &self,
        limit: i64,
    ) -> anyhow::Result<Vec<SmartMoneyLeaderCandidateRow>> {
        let rows = sqlx::query_as::<_, SmartMoneyLeaderCandidateRow>(
            r#"SELECT address, label, source_tags, first_seen_at, last_seen_at, leaderboard_rank,
                      leaderboard_volume, leaderboard_pnl, open_positions_count, open_notional,
                      closed_positions_count, closed_total_bought, closed_realized_pnl, sampled_markets,
                      market_position_count, holder_position_count, activity_volume, activity_pnl,
                      verified, discovery_score, promoted, metadata, updated_at
               FROM smart_money_leader_candidates
               ORDER BY discovery_score DESC, last_seen_at DESC
               LIMIT $1"#,
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// Update the promoted flag for a discovered smart-money leader candidate.
    pub async fn set_smart_money_leader_candidate_promoted(
        &self,
        address: &str,
        promoted: bool,
    ) -> anyhow::Result<()> {
        sqlx::query(
            r#"UPDATE smart_money_leader_candidates
               SET promoted = $2,
                   updated_at = NOW()
               WHERE lower(address) = lower($1)"#,
        )
        .bind(address)
        .bind(promoted)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Load one discovered smart-money leader candidate by address.
    pub async fn load_smart_money_leader_candidate(
        &self,
        address: &str,
    ) -> anyhow::Result<Option<SmartMoneyLeaderCandidateRow>> {
        let row = sqlx::query_as::<_, SmartMoneyLeaderCandidateRow>(
            r#"SELECT address, label, source_tags, first_seen_at, last_seen_at, leaderboard_rank,
                      leaderboard_volume, leaderboard_pnl, open_positions_count, open_notional,
                      closed_positions_count, closed_total_bought, closed_realized_pnl, sampled_markets,
                      market_position_count, holder_position_count, activity_volume, activity_pnl,
                      verified, discovery_score, promoted, metadata, updated_at
               FROM smart_money_leader_candidates
               WHERE lower(address) = lower($1)
               LIMIT 1"#,
        )
        .bind(address)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    /// Upsert one config section into the config store and append a history row.
    pub async fn upsert_config_section(
        &self,
        section: &str,
        data: &JsonValue,
        changed_by: &str,
    ) -> anyhow::Result<()> {
        let version_row: (i32,) = sqlx::query_as(
            r#"INSERT INTO app_config (section, data, version, updated_at)
               VALUES ($1, $2, 1, NOW())
               ON CONFLICT (section) DO UPDATE
               SET data = EXCLUDED.data,
                   version = app_config.version + 1,
                   updated_at = NOW()
               RETURNING version"#,
        )
        .bind(section)
        .bind(data)
        .fetch_one(&self.pool)
        .await?;

        sqlx::query(
            r#"INSERT INTO config_history (section, data, version, changed_by, created_at)
               VALUES ($1, $2, $3, $4, NOW())"#,
        )
        .bind(section)
        .bind(data)
        .bind(version_row.0)
        .bind(changed_by)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Load recent config-history rows for one section, optionally filtered by `changed_by` prefix.
    pub async fn load_config_history(
        &self,
        section: &str,
        changed_by_prefix: Option<&str>,
        limit: i64,
    ) -> anyhow::Result<Vec<ConfigHistoryRow>> {
        let rows = sqlx::query_as::<_, ConfigHistoryRow>(
            r#"SELECT id, section, data, version, changed_by, created_at
               FROM config_history
               WHERE section = $1
                 AND ($2::TEXT IS NULL OR changed_by LIKE ($2 || '%'))
               ORDER BY created_at DESC
               LIMIT $3"#,
        )
        .bind(section)
        .bind(changed_by_prefix)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }
}
