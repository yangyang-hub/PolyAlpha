use alloy::primitives::U256;
use async_trait::async_trait;

use crate::types::{
    ArbitrageOpportunity, ExecutionResult, MarketInfo, OrderBook, RiskDecision, StrategyType,
};
use crate::Result;

/// Market data feed providing real-time order book data.
#[async_trait]
pub trait MarketDataFeed: Send + Sync {
    /// Subscribe to order book updates for the given token IDs.
    async fn subscribe(&self, token_ids: &[U256]) -> Result<()>;

    /// Unsubscribe from a token's order book updates.
    async fn unsubscribe(&self, token_ids: &[U256]) -> Result<()>;

    /// Get the current order book snapshot for a token.
    async fn get_orderbook(&self, token_id: U256) -> Result<OrderBook>;

    /// Discover and return all active markets matching the filter criteria.
    async fn discover_markets(&self) -> Result<Vec<MarketInfo>>;
}

/// Strategy that scans markets for arbitrage opportunities.
#[async_trait]
pub trait Strategy: Send + Sync {
    /// Human-readable strategy name.
    fn name(&self) -> &str;

    /// The type of strategy.
    fn strategy_type(&self) -> StrategyType;

    /// Scan a list of markets and return detected arbitrage opportunities.
    async fn scan(&self, markets: &[MarketInfo]) -> Result<Vec<ArbitrageOpportunity>>;
}

/// Executor responsible for carrying out arbitrage trades.
#[async_trait]
pub trait Executor: Send + Sync {
    /// Execute a detected arbitrage opportunity.
    async fn execute(&self, opportunity: &ArbitrageOpportunity) -> Result<ExecutionResult>;

    /// Cancel all outstanding orders.
    async fn cancel_all(&self) -> Result<()>;
}

/// Risk manager performing pre-trade checks and position tracking.
#[async_trait]
pub trait RiskManager: Send + Sync {
    /// Check whether the opportunity passes risk limits.
    fn check_pre_trade(&self, opportunity: &ArbitrageOpportunity) -> RiskDecision;

    /// Update internal state after a trade execution.
    fn update_position(&self, result: &ExecutionResult);

    /// Whether the circuit breaker has been triggered.
    fn is_circuit_broken(&self) -> bool;

    /// Reset the daily counters (called at midnight UTC).
    fn reset_daily(&self);
}
