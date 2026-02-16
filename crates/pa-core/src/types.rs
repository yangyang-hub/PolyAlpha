use alloy::primitives::{B256, U256};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ──── Event Calendar Types ────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EventCategory {
    Macro,      // FOMC, CPI, NFP, GDP
    Crypto,     // Token unlocks, forks, ETF decisions
    Political,  // Elections, hearings, legislation
    Sports,     // Matches, tournaments
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum EventImpact {
    Low,
    Medium,
    High,
}

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
    /// Market liquidity in USD (from Gamma API). Used for prioritizing subscriptions.
    #[serde(default)]
    pub liquidity: Decimal,
    /// For NegRisk markets, the parent event's title (used for weather detection).
    #[serde(default)]
    pub event_title: Option<String>,
    /// Market resolution/end date (from Gamma API). Used by convergence strategy.
    #[serde(default)]
    pub end_date: Option<DateTime<Utc>>,
    /// Market category hint (e.g. "crypto", "politics"). Used by event calendar filter.
    #[serde(default)]
    pub category: Option<String>,
}

/// A NegRisk event containing multiple outcome markets.
///
/// In Polymarket, NegRisk events (e.g. "Who will win the election?") have N outcomes,
/// each represented as a binary market with YES/NO tokens. The NegRiskAdapter enforces
/// that the sum of all YES prices should equal $1.00.
///
/// Arbitrage opportunity: if `sum(YES_ask[i]) < $1.00`, buy all YES tokens and merge.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NegRiskEvent {
    /// The NegRisk market ID that groups all outcomes.
    pub neg_risk_market_id: B256,
    /// The event title (e.g. "Highest temperature in NYC on February 14?").
    pub title: String,
    /// All outcome markets within this event.
    pub markets: Vec<MarketInfo>,
    /// Fee rate from any constituent market (they share the same rate).
    pub fee_rate_bps: u32,
}

/// A group of related binary markets sharing the same event (non-NegRisk).
///
/// Unlike `NegRiskEvent`, these markets are independent — not mutually exclusive.
/// Example event: "What price will Bitcoin hit in 2026?" groups:
///   - "Will Bitcoin reach $200,000 by December 31, 2026?"
///   - "Will Bitcoin reach $150,000 by December 31, 2026?"
///   - "Will Bitcoin dip to $85,000 by December 31, 2026?"
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BinaryEventGroup {
    /// The shared event title.
    pub title: String,
    /// All binary markets within this event group.
    pub markets: Vec<MarketInfo>,
}

/// A pair of correlated binary markets suitable for cross-market arbitrage.
///
/// Example: "Will X happen by June?" and "Will X happen by December?"
/// If the prices of correlated outcomes deviate from their expected sum,
/// an arbitrage opportunity exists.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrossMarketPair {
    /// Unique identifier for this pair (deterministic from condition_ids).
    pub pair_id: B256,
    /// First market in the pair.
    pub market_a: MarketInfo,
    /// Second market in the pair.
    pub market_b: MarketInfo,
    /// The theoretical sum constraint (typically $1.00 for complementary outcomes).
    pub expected_sum: Decimal,
    /// How the markets are correlated.
    pub correlation: CrossMarketCorrelation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CrossMarketCorrelation {
    /// market_a YES + market_b YES = expected_sum
    ComplementaryYes,
    /// market_a YES + market_b NO = expected_sum
    InverseYesNo,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum StrategyType {
    /// YES+NO < $1.00 → buy both, merge to USDC
    YesNoMerge,
    /// YES+NO > $1.00 → split USDC, sell both
    YesNoSplit,
    /// NegRisk multi-outcome arbitrage
    NegRiskConvert,
    /// Cross-market correlation arbitrage
    CrossMarket,
    /// Weather forecast-based directional alpha
    Weather,
    /// Resolution convergence: buy tokens near 0/1 as markets approach resolution
    ResolutionConvergence,
    /// Crypto price-based directional alpha
    CryptoAlpha,
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
    /// NegRisk multi-outcome: buy YES tokens across all outcomes, merge.
    NegRiskArbitrage {
        /// The NegRisk market ID for the event.
        neg_risk_market_id: B256,
        /// One leg per outcome: buy YES at ask price.
        legs: Vec<NegRiskLeg>,
        /// Total amount to buy per outcome (min of all leg sizes).
        amount: Decimal,
    },
    /// Cross-market arbitrage: execute paired trades on two independent markets.
    CrossMarket {
        pair_id: B256,
        /// Leg on market A.
        leg_a: CrossMarketLeg,
        /// Leg on market B.
        leg_b: CrossMarketLeg,
        /// Total size to trade (min of both legs).
        amount: Decimal,
    },
    /// Directional buy: purchase a single token (YES or NO) via CLOB only.
    /// No on-chain CTF operation needed — just a CLOB FOK order.
    DirectionalBuy {
        token_id: U256,
        side: TradeSide,
        price: Decimal,
        size: Decimal,
        condition_id: B256,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NegRiskLeg {
    pub token_id: U256,
    pub condition_id: B256,
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

/// A single leg of a cross-market arbitrage trade.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrossMarketLeg {
    pub condition_id: B256,
    pub yes_token_id: U256,
    pub no_token_id: U256,
    /// The operation on this market.
    pub operation: CrossMarketOp,
    /// Price for the YES token (ask if buying, bid if selling).
    pub yes_price: Decimal,
    /// Price for the NO token.
    pub no_price: Decimal,
    pub size: Decimal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CrossMarketOp {
    /// Buy YES+NO, merge for $1.00 (when YES_ask + NO_ask < 1.00).
    BuyAndMerge,
    /// Split $1.00, sell YES+NO (when YES_bid + NO_bid > 1.00).
    SplitAndSell,
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
    BelowMinOrder,
    InsufficientBalance,
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
