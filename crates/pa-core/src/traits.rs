use alloy::primitives::U256;
use async_trait::async_trait;
use rust_decimal::Decimal;

use crate::Result;
use crate::types::{
    ExecutionResult, MarketInfo, OrderBook, RiskDecision, StrategyType, TradingOpportunity,
};

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

/// Strategy that scans markets for trading opportunities.
#[async_trait]
pub trait Strategy: Send + Sync {
    /// Human-readable strategy name.
    fn name(&self) -> &str;

    /// The type of strategy.
    fn strategy_type(&self) -> StrategyType;

    /// Scan a list of markets and return detected trading opportunities.
    async fn scan(&self, markets: &[MarketInfo]) -> Result<Vec<TradingOpportunity>>;
}

/// Executor responsible for carrying out trades.
#[async_trait]
pub trait Executor: Send + Sync {
    /// Execute a detected trading opportunity.
    async fn execute(&self, opportunity: &TradingOpportunity) -> Result<ExecutionResult>;

    /// Cancel all outstanding orders.
    async fn cancel_all(&self) -> Result<()>;

    /// Query the available USDC collateral balance from the exchange.
    ///
    /// Returns the balance held in the Polymarket proxy wallet (not the EOA).
    /// Default implementation returns zero (e.g. for dry-run or backtest).
    async fn get_balance(&self) -> Result<rust_decimal::Decimal> {
        Ok(rust_decimal::Decimal::ZERO)
    }
}

/// Risk manager performing pre-trade checks and position tracking.
#[async_trait]
pub trait RiskManager: Send + Sync {
    /// Check whether the opportunity passes risk limits.
    fn check_pre_trade(&self, opportunity: &TradingOpportunity) -> RiskDecision;

    /// Update internal state after a trade execution.
    fn update_position(&self, result: &ExecutionResult);

    /// Average cost for a currently held token position.
    fn avg_cost(&self, _token_id: &U256) -> Decimal {
        Decimal::ZERO
    }

    /// Whether the circuit breaker has been triggered.
    fn is_circuit_broken(&self) -> bool;

    /// Current total exposure across all open positions (in USDC).
    fn total_exposure(&self) -> rust_decimal::Decimal;

    /// Reset the daily counters (called at midnight UTC).
    fn reset_daily(&self);
}
