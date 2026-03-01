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
    /// YES/NO outcome prices at discovery time (from Gamma API).
    /// Used for smart WS subscription ordering (filter extreme prices, prioritize mid-range).
    #[serde(default)]
    pub outcome_prices: Option<Vec<Decimal>>,
    /// Best bid price from Gamma API (CLOB top-of-book at discovery time, YES side).
    #[serde(default)]
    pub gamma_best_bid: Option<Decimal>,
    /// Best ask price from Gamma API (CLOB top-of-book at discovery time, YES side).
    #[serde(default)]
    pub gamma_best_ask: Option<Decimal>,
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

/// Result of walking the order book to simulate a fill.
#[derive(Debug, Clone)]
pub struct WalkResult {
    /// Total fillable quantity.
    pub filled: Decimal,
    /// Volume-weighted average price of the fill.
    pub avg_price: Decimal,
    /// Worst price level touched.
    pub worst_price: Decimal,
    /// Number of price levels consumed.
    pub levels_used: usize,
}

/// A single leg's liquidity requirement extracted from an `ExecutionPlan`.
#[derive(Debug, Clone)]
pub struct LiquidityRequirement {
    pub token_id: U256,
    pub side: TradeSide,
    pub size: Decimal,
    pub limit_price: Decimal,
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

    /// Total available depth within `limit_price` on the given side.
    ///
    /// - Buy: walks asks where `level.price <= limit_price`
    /// - Sell: walks bids where `level.price >= limit_price`
    pub fn available_depth(&self, side: TradeSide, limit_price: Decimal) -> Decimal {
        match side {
            TradeSide::Buy => self
                .asks
                .iter()
                .take_while(|l| l.price <= limit_price)
                .map(|l| l.size)
                .sum(),
            TradeSide::Sell => self
                .bids
                .iter()
                .take_while(|l| l.price >= limit_price)
                .map(|l| l.size)
                .sum(),
        }
    }

    /// Simulate filling `target_size` by walking the book.
    ///
    /// Returns `None` if the relevant side is empty.
    pub fn walk_book(&self, side: TradeSide, target_size: Decimal) -> Option<WalkResult> {
        let levels = match side {
            TradeSide::Buy => &self.asks,
            TradeSide::Sell => &self.bids,
        };
        if levels.is_empty() {
            return None;
        }

        let mut filled = Decimal::ZERO;
        let mut cost = Decimal::ZERO; // sum of price * size for VWAP
        let mut worst_price = levels[0].price;
        let mut levels_used = 0usize;

        for level in levels {
            let remaining = target_size - filled;
            if remaining <= Decimal::ZERO {
                break;
            }
            let take = remaining.min(level.size);
            filled += take;
            cost += take * level.price;
            worst_price = level.price;
            levels_used += 1;
        }

        if filled.is_zero() {
            return None;
        }

        Some(WalkResult {
            filled,
            avg_price: cost / filled,
            worst_price,
            levels_used,
        })
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

impl ExecutionPlan {
    /// Returns true if this is an exit/sell order (reduces risk, not increases it).
    pub fn is_exit(&self) -> bool {
        matches!(self, ExecutionPlan::DirectionalBuy { side: TradeSide::Sell, .. })
    }

    /// Extract the liquidity requirements for each leg of this plan.
    ///
    /// Returns one `LiquidityRequirement` per token that needs to be filled.
    pub fn liquidity_requirements(&self) -> Vec<LiquidityRequirement> {
        match self {
            ExecutionPlan::BuyAndMerge {
                yes_token_id,
                no_token_id,
                yes_price,
                no_price,
                merge_amount,
                ..
            } => vec![
                LiquidityRequirement {
                    token_id: *yes_token_id,
                    side: TradeSide::Buy,
                    size: *merge_amount,
                    limit_price: *yes_price,
                },
                LiquidityRequirement {
                    token_id: *no_token_id,
                    side: TradeSide::Buy,
                    size: *merge_amount,
                    limit_price: *no_price,
                },
            ],
            ExecutionPlan::SplitAndSell {
                yes_token_id,
                no_token_id,
                yes_price,
                no_price,
                split_amount,
                ..
            } => vec![
                LiquidityRequirement {
                    token_id: *yes_token_id,
                    side: TradeSide::Sell,
                    size: *split_amount,
                    limit_price: *yes_price,
                },
                LiquidityRequirement {
                    token_id: *no_token_id,
                    side: TradeSide::Sell,
                    size: *split_amount,
                    limit_price: *no_price,
                },
            ],
            ExecutionPlan::NegRiskArbitrage { legs, .. } => legs
                .iter()
                .map(|leg| LiquidityRequirement {
                    token_id: leg.token_id,
                    side: leg.side,
                    size: leg.size,
                    limit_price: leg.price,
                })
                .collect(),
            ExecutionPlan::CrossMarket { leg_a, leg_b, .. } => {
                let mut reqs = Vec::with_capacity(4);
                for leg in [leg_a, leg_b] {
                    match leg.operation {
                        CrossMarketOp::BuyAndMerge => {
                            reqs.push(LiquidityRequirement {
                                token_id: leg.yes_token_id,
                                side: TradeSide::Buy,
                                size: leg.size,
                                limit_price: leg.yes_price,
                            });
                            reqs.push(LiquidityRequirement {
                                token_id: leg.no_token_id,
                                side: TradeSide::Buy,
                                size: leg.size,
                                limit_price: leg.no_price,
                            });
                        }
                        CrossMarketOp::SplitAndSell => {
                            reqs.push(LiquidityRequirement {
                                token_id: leg.yes_token_id,
                                side: TradeSide::Sell,
                                size: leg.size,
                                limit_price: leg.yes_price,
                            });
                            reqs.push(LiquidityRequirement {
                                token_id: leg.no_token_id,
                                side: TradeSide::Sell,
                                size: leg.size,
                                limit_price: leg.no_price,
                            });
                        }
                    }
                }
                reqs
            }
            ExecutionPlan::DirectionalBuy {
                token_id,
                side,
                price,
                size,
                ..
            } => vec![LiquidityRequirement {
                token_id: *token_id,
                side: *side,
                size: *size,
                limit_price: *price,
            }],
        }
    }

    /// Approximate per-unit entry price for exposure estimation.
    pub fn entry_price(&self) -> Option<Decimal> {
        match self {
            ExecutionPlan::DirectionalBuy { price, .. } => Some(*price),
            ExecutionPlan::BuyAndMerge { yes_price, no_price, .. } => Some(*yes_price + *no_price),
            ExecutionPlan::SplitAndSell { .. } => Some(Decimal::ONE),
            ExecutionPlan::NegRiskArbitrage { legs, .. } => {
                Some(legs.iter().map(|l| l.price).sum())
            }
            ExecutionPlan::CrossMarket { .. } => Some(Decimal::ONE),
        }
    }

    /// Estimated USDC cost to execute this plan (price × size for each leg).
    pub fn estimated_cost(&self) -> Decimal {
        match self {
            ExecutionPlan::DirectionalBuy { price, size, .. } => *price * *size,
            ExecutionPlan::BuyAndMerge { yes_price, no_price, merge_amount, .. } => {
                (*yes_price + *no_price) * *merge_amount
            }
            ExecutionPlan::SplitAndSell { split_amount, .. } => *split_amount,
            ExecutionPlan::NegRiskArbitrage { legs, .. } => {
                legs.iter().map(|l| l.price * l.size).sum()
            }
            ExecutionPlan::CrossMarket { leg_a, leg_b, .. } => {
                (leg_a.yes_price + leg_a.no_price) * leg_a.size
                    + (leg_b.yes_price + leg_b.no_price) * leg_b.size
            }
        }
    }
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
    pub strategy_type: StrategyType,
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
    pub condition_id: B256,
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
    ExceedsStrategyExposure,
    ExceedsStrategyMarketCount,
    InsufficientDepth,
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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use rust_decimal_macros::dec;

    fn make_book(bids: Vec<(Decimal, Decimal)>, asks: Vec<(Decimal, Decimal)>) -> OrderBook {
        OrderBook {
            token_id: U256::from(1u64),
            bids: bids
                .into_iter()
                .map(|(price, size)| PriceLevel { price, size })
                .collect(),
            asks: asks
                .into_iter()
                .map(|(price, size)| PriceLevel { price, size })
                .collect(),
            timestamp: Utc::now(),
        }
    }

    #[test]
    fn test_available_depth_buy() {
        let book = make_book(
            vec![(dec!(0.45), dec!(100))],
            vec![
                (dec!(0.50), dec!(50)),
                (dec!(0.55), dec!(30)),
                (dec!(0.60), dec!(20)),
            ],
        );
        // limit 0.55: should get 50 + 30 = 80
        assert_eq!(book.available_depth(TradeSide::Buy, dec!(0.55)), dec!(80));
        // limit 0.50: only first level
        assert_eq!(book.available_depth(TradeSide::Buy, dec!(0.50)), dec!(50));
        // limit 0.60: all levels
        assert_eq!(book.available_depth(TradeSide::Buy, dec!(0.60)), dec!(100));
    }

    #[test]
    fn test_available_depth_sell() {
        let book = make_book(
            vec![
                (dec!(0.55), dec!(40)),
                (dec!(0.50), dec!(30)),
                (dec!(0.45), dec!(20)),
            ],
            vec![(dec!(0.60), dec!(100))],
        );
        // limit 0.50: 40 + 30 = 70
        assert_eq!(book.available_depth(TradeSide::Sell, dec!(0.50)), dec!(70));
        // limit 0.55: only first level
        assert_eq!(book.available_depth(TradeSide::Sell, dec!(0.55)), dec!(40));
    }

    #[test]
    fn test_available_depth_empty() {
        let book = make_book(vec![], vec![]);
        assert_eq!(book.available_depth(TradeSide::Buy, dec!(1.00)), dec!(0));
        assert_eq!(book.available_depth(TradeSide::Sell, dec!(0.01)), dec!(0));
    }

    #[test]
    fn test_walk_book_full_fill() {
        let book = make_book(
            vec![],
            vec![(dec!(0.50), dec!(100))],
        );
        let result = book.walk_book(TradeSide::Buy, dec!(50)).unwrap();
        assert_eq!(result.filled, dec!(50));
        assert_eq!(result.avg_price, dec!(0.50));
        assert_eq!(result.worst_price, dec!(0.50));
        assert_eq!(result.levels_used, 1);
    }

    #[test]
    fn test_walk_book_multi_level() {
        let book = make_book(
            vec![],
            vec![
                (dec!(0.50), dec!(20)),
                (dec!(0.52), dec!(30)),
                (dec!(0.55), dec!(50)),
            ],
        );
        let result = book.walk_book(TradeSide::Buy, dec!(40)).unwrap();
        assert_eq!(result.filled, dec!(40));
        // VWAP: (20*0.50 + 20*0.52) / 40 = (10 + 10.4) / 40 = 0.51
        assert_eq!(result.avg_price, dec!(0.51));
        assert_eq!(result.worst_price, dec!(0.52));
        assert_eq!(result.levels_used, 2);
    }

    #[test]
    fn test_walk_book_partial() {
        let book = make_book(
            vec![],
            vec![(dec!(0.50), dec!(10))],
        );
        let result = book.walk_book(TradeSide::Buy, dec!(100)).unwrap();
        assert_eq!(result.filled, dec!(10));
        assert_eq!(result.avg_price, dec!(0.50));
        assert_eq!(result.levels_used, 1);
    }

    #[test]
    fn test_walk_book_empty() {
        let book = make_book(vec![], vec![]);
        assert!(book.walk_book(TradeSide::Buy, dec!(10)).is_none());
    }

    #[test]
    fn test_liquidity_requirements_directional() {
        let plan = ExecutionPlan::DirectionalBuy {
            token_id: U256::from(42u64),
            side: TradeSide::Buy,
            price: dec!(0.60),
            size: dec!(25),
            condition_id: B256::ZERO,
        };
        let reqs = plan.liquidity_requirements();
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].token_id, U256::from(42u64));
        assert_eq!(reqs[0].side, TradeSide::Buy);
        assert_eq!(reqs[0].size, dec!(25));
        assert_eq!(reqs[0].limit_price, dec!(0.60));
    }

    #[test]
    fn test_liquidity_requirements_buy_and_merge() {
        let plan = ExecutionPlan::BuyAndMerge {
            yes_token_id: U256::from(1u64),
            no_token_id: U256::from(2u64),
            yes_price: dec!(0.45),
            no_price: dec!(0.50),
            merge_amount: dec!(100),
            condition_id: B256::ZERO,
        };
        let reqs = plan.liquidity_requirements();
        assert_eq!(reqs.len(), 2);
        assert_eq!(reqs[0].side, TradeSide::Buy);
        assert_eq!(reqs[0].size, dec!(100));
        assert_eq!(reqs[1].side, TradeSide::Buy);
        assert_eq!(reqs[1].size, dec!(100));
    }

    #[test]
    fn test_liquidity_requirements_neg_risk() {
        let plan = ExecutionPlan::NegRiskArbitrage {
            neg_risk_market_id: B256::ZERO,
            legs: vec![
                NegRiskLeg {
                    token_id: U256::from(1u64),
                    condition_id: B256::ZERO,
                    outcome: Outcome::Yes,
                    side: TradeSide::Buy,
                    price: dec!(0.30),
                    size: dec!(50),
                },
                NegRiskLeg {
                    token_id: U256::from(2u64),
                    condition_id: B256::ZERO,
                    outcome: Outcome::Yes,
                    side: TradeSide::Buy,
                    price: dec!(0.25),
                    size: dec!(50),
                },
                NegRiskLeg {
                    token_id: U256::from(3u64),
                    condition_id: B256::ZERO,
                    outcome: Outcome::Yes,
                    side: TradeSide::Buy,
                    price: dec!(0.40),
                    size: dec!(50),
                },
            ],
            amount: dec!(50),
        };
        let reqs = plan.liquidity_requirements();
        assert_eq!(reqs.len(), 3);
        for req in &reqs {
            assert_eq!(req.side, TradeSide::Buy);
            assert_eq!(req.size, dec!(50));
        }
    }
}
