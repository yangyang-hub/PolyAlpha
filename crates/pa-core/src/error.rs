use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("Configuration error: {0}")]
    Config(#[from] config::ConfigError),

    #[error("Strategy error: {0}")]
    Strategy(String),

    #[error("Execution error: {0}")]
    Execution(String),

    #[error("Risk check failed: {0}")]
    RiskCheck(String),

    #[error("Market data error: {0}")]
    MarketData(String),

    #[error("Insufficient liquidity: {0}")]
    InsufficientLiquidity(String),

    #[error("Order failed: {0}")]
    OrderFailed(String),

    #[error("On-chain transaction failed: {0}")]
    OnChainTxFailed(String),

    #[error("Database error: {0}")]
    Database(String),

    #[error("Circuit breaker triggered")]
    CircuitBroken,
}
