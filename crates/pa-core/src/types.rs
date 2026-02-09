use alloy::primitives::{B256, U256};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ──── Market Types ────

/// Metadata for a Polymarket binary market.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketInfo {
    pub condition_id: B256,
    pub question_id: B256,
    pub question: String,
    pub neg_risk: bool,
    /// For NegRisk markets, the parent event's market ID.
    pub neg_risk_market_id: Option<B256>,
    pub tokens: Vec<TokenInfo>,
    pub tick_size: Decimal,
    pub fee_rate_bps: u32,
    pub active: bool,
}

/// YES or NO token information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenInfo {
    pub token_id: U256,
    pub outcome: Outcome,
    /// The complementary token's ID (YES↔NO).
    pub complement_id: U256,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Outcome {
    Yes,
    No,
}

// ──── Order Book Types ────

/// A local snapshot of an order book for a single token.
#[derive(Debug, Clone, Default)]
pub struct OrderBook {
    pub token_id: U256,
    /// Sorted descending by price (best bid first).
    pub bids: Vec<PriceLevel>,
    /// Sorted ascending by price (best ask first).
    pub asks: Vec<PriceLevel>,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PriceLevel {
    pub price: Decimal,
    pub size: Decimal,
}

impl OrderBook {
    pub fn best_bid(&self) -> Option<&PriceLevel> {
        self.bids.first()
    }

    pub fn best_ask(&self) -> Option<&PriceLevel> {
        self.asks.first()
    }

    pub fn midpoint(&self) -> Option<Decimal> {
        let bid = self.best_bid()?.price;
        let ask = self.best_ask()?.price;
        Some((bid + ask) / Decimal::TWO)
    }

    pub fn spread(&self) -> Option<Decimal> {
        let bid = self.best_bid()?.price;
        let ask = self.best_ask()?.price;
        Some(ask - bid)
    }
}

// ──── Arbitrage Types ────

/// A detected arbitrage opportunity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArbitrageOpportunity {
    pub id: Uuid,
    pub strategy_type: StrategyType,
    pub condition_id: B256,
    pub question: String,
    /// How much YES+NO deviates from $1.00 (absolute value).
    pub spread: Decimal,
    /// Expected profit after fees and gas.
    pub estimated_profit: Decimal,
    /// Maximum executable quantity.
    pub size: Decimal,
    pub detected_at: DateTime<Utc>,
    pub execution_plan: ExecutionPlan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StrategyType {
    /// YES+NO < $1.00 → buy both, merge to USDC
    YesNoMerge,
    /// YES+NO > $1.00 → split USDC, sell both
    YesNoSplit,
    /// NegRisk multi-outcome arbitrage
    NegRiskConvert,
    /// Cross-market correlation arbitrage
    CrossMarket,
}

/// Concrete execution plan for an arbitrage opportunity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ExecutionPlan {
    /// Buy YES + NO tokens via CLOB, then merge on-chain.
    BuyAndMerge {
        yes_token_id: U256,
        no_token_id: U256,
        yes_price: Decimal,
        no_price: Decimal,
        merge_amount: Decimal,
        condition_id: B256,
    },
    /// Split USDC on-chain into YES+NO, then sell via CLOB.
    SplitAndSell {
        yes_token_id: U256,
        no_token_id: U256,
        yes_price: Decimal,
        no_price: Decimal,
        split_amount: Decimal,
        condition_id: B256,
    },
    /// NegRisk multi-outcome conversion.
    NegRiskArbitrage {
        legs: Vec<NegRiskLeg>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NegRiskLeg {
    pub token_id: U256,
    pub outcome: Outcome,
    pub side: TradeSide,
    pub price: Decimal,
    pub size: Decimal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TradeSide {
    Buy,
    Sell,
}

// ──── Execution Result Types ────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionResult {
    pub opportunity_id: Uuid,
    pub status: ExecutionStatus,
    pub trades: Vec<TradeRecord>,
    pub realized_profit: Decimal,
    pub total_fees: Decimal,
    pub total_gas: Decimal,
    pub executed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExecutionStatus {
    Success,
    PartialFill,
    NoFill,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeRecord {
    pub id: Uuid,
    pub token_id: U256,
    pub side: TradeSide,
    pub price: Decimal,
    pub size: Decimal,
    pub filled_size: Decimal,
    pub fee: Decimal,
    pub tx_type: TxType,
    pub tx_hash: Option<B256>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TxType {
    ClobOrder,
    CtfSplit,
    CtfMerge,
    CtfRedeem,
}

// ──── Risk Types ────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RiskDecision {
    Approve,
    Reject(RiskRejectReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RiskRejectReason {
    ExceedsTradeLimit,
    ExceedsMarketPositionLimit,
    ExceedsTotalExposure,
    BelowMinProfit,
    CircuitBroken,
    ExceedsSlippage,
}

// ──── Profit Estimation ────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfitEstimate {
    pub gross_profit: Decimal,
    pub fees: Decimal,
    pub gas: Decimal,
    pub net_profit: Decimal,
    /// Net profit / total cost
    pub roi: Decimal,
}
