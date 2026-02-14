use std::collections::HashSet;

use alloy::primitives::{B256, U256};
use async_trait::async_trait;
use chrono::Utc;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use uuid::Uuid;

use pa_core::traits::Strategy;
use pa_core::types::{
    ArbitrageOpportunity, CrossMarketCorrelation, CrossMarketLeg, CrossMarketOp, CrossMarketPair,
    ExecutionPlan, MarketInfo, OrderBook, StrategyType,
};
use pa_core::Result;

use crate::profitability::ProfitCalculator;

/// Cross-market correlation arbitrage strategy.
///
/// Detects when two independent binary markets with correlated outcomes
/// deviate from their theoretical price relationship.
pub struct CrossMarketArbitrage {
    min_spread_bps: u32,
    max_trade_size: Decimal,
    min_profit: Decimal,
    profit_calc: ProfitCalculator,
    pairs: Vec<CrossMarketPair>,
    get_orderbook: Box<dyn Fn(U256) -> Option<OrderBook> + Send + Sync>,
    /// Returns available capital (balance - exposure) for position sizing.
    get_available_capital: Box<dyn Fn() -> Decimal + Send + Sync>,
}

impl CrossMarketArbitrage {
    pub fn new(
        min_spread_bps: u32,
        max_trade_size: Decimal,
        min_profit: Decimal,
        gas_cost_usd: Decimal,
        pairs: Vec<CrossMarketPair>,
        get_orderbook: Box<dyn Fn(U256) -> Option<OrderBook> + Send + Sync>,
        get_available_capital: Box<dyn Fn() -> Decimal + Send + Sync>,
    ) -> Self {
        Self {
            min_spread_bps,
            max_trade_size,
            min_profit,
            profit_calc: ProfitCalculator::new(gas_cost_usd),
            pairs,
            get_orderbook,
            get_available_capital,
        }
    }

    /// Detect arbitrage opportunity for a correlated market pair.
    fn detect_pair(&self, pair: &CrossMarketPair) -> Option<ArbitrageOpportunity> {
        if pair.market_a.tokens.len() != 2 || pair.market_b.tokens.len() != 2 {
            return None;
        }

        let yes_a = &pair.market_a.tokens[0];
        let no_a = &pair.market_a.tokens[1];
        let yes_b = &pair.market_b.tokens[0];
        let no_b = &pair.market_b.tokens[1];

        let yes_a_book = (self.get_orderbook)(yes_a.token_id)?;
        let no_a_book = (self.get_orderbook)(no_a.token_id)?;
        let yes_b_book = (self.get_orderbook)(yes_b.token_id)?;
        let no_b_book = (self.get_orderbook)(no_b.token_id)?;

        match pair.correlation {
            CrossMarketCorrelation::ComplementaryYes => {
                // YES_a + YES_b should = expected_sum
                self.detect_complementary_yes(
                    pair, &yes_a_book, &no_a_book, &yes_b_book, &no_b_book,
                )
            }
            CrossMarketCorrelation::InverseYesNo => {
                // YES_a + NO_b should = expected_sum
                self.detect_inverse_yes_no(
                    pair, &yes_a_book, &no_a_book, &yes_b_book, &no_b_book,
                )
            }
        }
    }

    /// ComplementaryYes: market_a.YES + market_b.YES should = expected_sum.
    fn detect_complementary_yes(
        &self,
        pair: &CrossMarketPair,
        yes_a_book: &OrderBook,
        no_a_book: &OrderBook,
        yes_b_book: &OrderBook,
        no_b_book: &OrderBook,
    ) -> Option<ArbitrageOpportunity> {
        // Try buy direction: if YES_a_ask + YES_b_ask < expected_sum, buy both YES
        // Each market we do BuyAndMerge (buy YES + NO, merge)
        // Actually for cross-market, we buy the correlated token from each market.
        // The simplest execution: on each market do BuyAndMerge or SplitAndSell independently.
        // But the cross-market signal is from the correlated token prices.

        // Direction 1: Buy underpriced
        let yes_a_ask = yes_a_book.best_ask()?;
        let yes_b_ask = yes_b_book.best_ask()?;
        let sum_asks = yes_a_ask.price + yes_b_ask.price;

        if sum_asks < pair.expected_sum {
            let spread = pair.expected_sum - sum_asks;
            let spread_bps = (spread * dec!(10000)).to_u32_saturating();

            if spread_bps >= self.min_spread_bps {
                // Also need the NO asks for the execution plan (BuyAndMerge on each market)
                let no_a_ask = no_a_book.best_ask()?;
                let no_b_ask = no_b_book.best_ask()?;

                let available = (self.get_available_capital)();
                let size = yes_a_ask
                    .size
                    .min(yes_b_ask.size)
                    .min(no_a_ask.size)
                    .min(no_b_ask.size)
                    .min(self.max_trade_size)
                    .min(available);

                let estimate = self.profit_calc.cross_market_profit(
                    yes_a_ask.price,
                    pair.market_a.fee_rate_bps,
                    yes_b_ask.price,
                    pair.market_b.fee_rate_bps,
                    size,
                    pair.expected_sum,
                    true,
                );

                if estimate.net_profit >= self.min_profit {
                    return Some(ArbitrageOpportunity {
                        id: Uuid::now_v7(),
                        strategy_type: StrategyType::CrossMarket,
                        condition_id: pair.pair_id,
                        question: format!(
                            "Cross: {} / {}",
                            pair.market_a.question, pair.market_b.question
                        ),
                        spread,
                        estimated_profit: estimate.net_profit,
                        size,
                        detected_at: Utc::now(),
                        execution_plan: ExecutionPlan::CrossMarket {
                            pair_id: pair.pair_id,
                            leg_a: CrossMarketLeg {
                                condition_id: pair.market_a.condition_id,
                                yes_token_id: pair.market_a.tokens[0].token_id,
                                no_token_id: pair.market_a.tokens[1].token_id,
                                operation: CrossMarketOp::BuyAndMerge,
                                yes_price: yes_a_ask.price,
                                no_price: no_a_ask.price,
                                size,
                            },
                            leg_b: CrossMarketLeg {
                                condition_id: pair.market_b.condition_id,
                                yes_token_id: pair.market_b.tokens[0].token_id,
                                no_token_id: pair.market_b.tokens[1].token_id,
                                operation: CrossMarketOp::BuyAndMerge,
                                yes_price: yes_b_ask.price,
                                no_price: no_b_ask.price,
                                size,
                            },
                            amount: size,
                        },
                    });
                }
            }
        }

        // Direction 2: Sell overpriced
        let yes_a_bid = yes_a_book.best_bid()?;
        let yes_b_bid = yes_b_book.best_bid()?;
        let sum_bids = yes_a_bid.price + yes_b_bid.price;

        if sum_bids > pair.expected_sum {
            let spread = sum_bids - pair.expected_sum;
            let spread_bps = (spread * dec!(10000)).to_u32_saturating();

            if spread_bps >= self.min_spread_bps {
                let no_a_bid = no_a_book.best_bid()?;
                let no_b_bid = no_b_book.best_bid()?;

                let available = (self.get_available_capital)();
                let size = yes_a_bid
                    .size
                    .min(yes_b_bid.size)
                    .min(no_a_bid.size)
                    .min(no_b_bid.size)
                    .min(self.max_trade_size)
                    .min(available);

                let estimate = self.profit_calc.cross_market_profit(
                    yes_a_bid.price,
                    pair.market_a.fee_rate_bps,
                    yes_b_bid.price,
                    pair.market_b.fee_rate_bps,
                    size,
                    pair.expected_sum,
                    false,
                );

                if estimate.net_profit >= self.min_profit {
                    return Some(ArbitrageOpportunity {
                        id: Uuid::now_v7(),
                        strategy_type: StrategyType::CrossMarket,
                        condition_id: pair.pair_id,
                        question: format!(
                            "Cross: {} / {}",
                            pair.market_a.question, pair.market_b.question
                        ),
                        spread,
                        estimated_profit: estimate.net_profit,
                        size,
                        detected_at: Utc::now(),
                        execution_plan: ExecutionPlan::CrossMarket {
                            pair_id: pair.pair_id,
                            leg_a: CrossMarketLeg {
                                condition_id: pair.market_a.condition_id,
                                yes_token_id: pair.market_a.tokens[0].token_id,
                                no_token_id: pair.market_a.tokens[1].token_id,
                                operation: CrossMarketOp::SplitAndSell,
                                yes_price: yes_a_bid.price,
                                no_price: no_a_bid.price,
                                size,
                            },
                            leg_b: CrossMarketLeg {
                                condition_id: pair.market_b.condition_id,
                                yes_token_id: pair.market_b.tokens[0].token_id,
                                no_token_id: pair.market_b.tokens[1].token_id,
                                operation: CrossMarketOp::SplitAndSell,
                                yes_price: yes_b_bid.price,
                                no_price: no_b_bid.price,
                                size,
                            },
                            amount: size,
                        },
                    });
                }
            }
        }

        None
    }

    /// InverseYesNo: market_a.YES + market_b.NO should = expected_sum.
    fn detect_inverse_yes_no(
        &self,
        pair: &CrossMarketPair,
        yes_a_book: &OrderBook,
        _no_a_book: &OrderBook,
        _yes_b_book: &OrderBook,
        no_b_book: &OrderBook,
    ) -> Option<ArbitrageOpportunity> {
        // Buy direction: if YES_a_ask + NO_b_ask < expected_sum
        let yes_a_ask = yes_a_book.best_ask()?;
        let no_b_ask = no_b_book.best_ask()?;
        let sum_asks = yes_a_ask.price + no_b_ask.price;

        if sum_asks < pair.expected_sum {
            let spread = pair.expected_sum - sum_asks;
            let spread_bps = (spread * dec!(10000)).to_u32_saturating();

            if spread_bps >= self.min_spread_bps {
                let available = (self.get_available_capital)();
                let size = yes_a_ask
                    .size
                    .min(no_b_ask.size)
                    .min(self.max_trade_size)
                    .min(available);

                let estimate = self.profit_calc.cross_market_profit(
                    yes_a_ask.price,
                    pair.market_a.fee_rate_bps,
                    no_b_ask.price,
                    pair.market_b.fee_rate_bps,
                    size,
                    pair.expected_sum,
                    true,
                );

                if estimate.net_profit >= self.min_profit {
                    return Some(ArbitrageOpportunity {
                        id: Uuid::now_v7(),
                        strategy_type: StrategyType::CrossMarket,
                        condition_id: pair.pair_id,
                        question: format!(
                            "Cross(InvYN): {} / {}",
                            pair.market_a.question, pair.market_b.question
                        ),
                        spread,
                        estimated_profit: estimate.net_profit,
                        size,
                        detected_at: Utc::now(),
                        execution_plan: ExecutionPlan::CrossMarket {
                            pair_id: pair.pair_id,
                            leg_a: CrossMarketLeg {
                                condition_id: pair.market_a.condition_id,
                                yes_token_id: pair.market_a.tokens[0].token_id,
                                no_token_id: pair.market_a.tokens[1].token_id,
                                operation: CrossMarketOp::BuyAndMerge,
                                yes_price: yes_a_ask.price,
                                no_price: no_b_ask.price,
                                size,
                            },
                            leg_b: CrossMarketLeg {
                                condition_id: pair.market_b.condition_id,
                                yes_token_id: pair.market_b.tokens[0].token_id,
                                no_token_id: pair.market_b.tokens[1].token_id,
                                operation: CrossMarketOp::BuyAndMerge,
                                yes_price: no_b_ask.price,
                                no_price: yes_a_ask.price,
                                size,
                            },
                            amount: size,
                        },
                    });
                }
            }
        }

        None
    }
}

#[async_trait]
impl Strategy for CrossMarketArbitrage {
    fn name(&self) -> &str {
        "CrossMarket Arbitrage"
    }

    fn strategy_type(&self) -> StrategyType {
        StrategyType::CrossMarket
    }

    async fn scan(&self, _markets: &[MarketInfo]) -> Result<Vec<ArbitrageOpportunity>> {
        let opportunities: Vec<ArbitrageOpportunity> =
            self.pairs.iter().filter_map(|p| self.detect_pair(p)).collect();

        tracing::debug!(count = opportunities.len(), "CrossMarket scan complete");
        Ok(opportunities)
    }
}

/// Detect potential cross-market pairs using question text similarity.
///
/// Conservative heuristic: groups non-NegRisk markets whose question text
/// shares >60% words. Only creates ComplementaryYes pairs.
pub fn detect_cross_market_pairs(markets: &[MarketInfo]) -> Vec<CrossMarketPair> {
    let non_neg_risk: Vec<&MarketInfo> = markets
        .iter()
        .filter(|m| !m.neg_risk && m.active && m.tokens.len() == 2)
        .collect();

    let mut pairs = Vec::new();
    let mut used: HashSet<usize> = HashSet::new();

    for (i, a) in non_neg_risk.iter().enumerate() {
        if used.contains(&i) {
            continue;
        }
        for (j, b) in non_neg_risk.iter().enumerate().skip(i + 1) {
            if used.contains(&j) {
                continue;
            }
            if a.condition_id == b.condition_id {
                continue;
            }

            let sim = word_similarity(&a.question, &b.question);
            if sim > dec!(0.6) {
                let pair_id = compute_pair_id(a.condition_id, b.condition_id);
                pairs.push(CrossMarketPair {
                    pair_id,
                    market_a: (*a).clone(),
                    market_b: (*b).clone(),
                    expected_sum: Decimal::ONE,
                    correlation: CrossMarketCorrelation::ComplementaryYes,
                });
                used.insert(i);
                used.insert(j);
                break;
            }
        }
    }

    tracing::info!(pairs = pairs.len(), "Cross-market pairs detected");
    pairs
}

/// Compute word-level Jaccard similarity between two strings.
fn word_similarity(a: &str, b: &str) -> Decimal {
    let words_a: HashSet<&str> = a.split_whitespace().map(|w| w.trim_matches(|c: char| !c.is_alphanumeric())).collect();
    let words_b: HashSet<&str> = b.split_whitespace().map(|w| w.trim_matches(|c: char| !c.is_alphanumeric())).collect();

    let intersection = words_a.intersection(&words_b).count();
    let union = words_a.union(&words_b).count();

    if union == 0 {
        return Decimal::ZERO;
    }

    Decimal::from(intersection as u64) / Decimal::from(union as u64)
}

/// Deterministic pair ID from two condition IDs (order-independent).
fn compute_pair_id(a: B256, b: B256) -> B256 {
    use alloy::primitives::keccak256;
    let (first, second) = if a < b { (a, b) } else { (b, a) };
    let mut data = Vec::with_capacity(64);
    data.extend_from_slice(first.as_slice());
    data.extend_from_slice(second.as_slice());
    keccak256(&data)
}

/// Helper trait for saturating conversion.
trait ToU32Saturating {
    fn to_u32_saturating(&self) -> u32;
}

impl ToU32Saturating for Decimal {
    fn to_u32_saturating(&self) -> u32 {
        use rust_decimal::prelude::ToPrimitive;
        ToPrimitive::to_u32(self).unwrap_or(u32::MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pa_core::types::{Outcome, PriceLevel, TokenInfo};
    use rust_decimal_macros::dec;
    use std::collections::HashMap as StdHashMap;

    fn make_market(cond: B256, q: &str, yes_id: u64, no_id: u64) -> MarketInfo {
        MarketInfo {
            condition_id: cond,
            question_id: B256::ZERO,
            question: q.to_string(),
            neg_risk: false,
            neg_risk_market_id: None,
            tokens: vec![
                TokenInfo {
                    token_id: U256::from(yes_id),
                    outcome: Outcome::Yes,
                    complement_id: U256::from(no_id),
                },
                TokenInfo {
                    token_id: U256::from(no_id),
                    outcome: Outcome::No,
                    complement_id: U256::from(yes_id),
                },
            ],
            tick_size: dec!(0.01),
            fee_rate_bps: 200,
            active: true,
            liquidity: dec!(1000),
            event_title: None,
        }
    }

    fn make_book(token_id: U256, best_bid: Decimal, best_ask: Decimal) -> OrderBook {
        OrderBook {
            token_id,
            bids: vec![PriceLevel {
                price: best_bid,
                size: dec!(500),
            }],
            asks: vec![PriceLevel {
                price: best_ask,
                size: dec!(500),
            }],
            timestamp: Utc::now(),
        }
    }

    #[test]
    fn test_word_similarity() {
        let sim = word_similarity("Will Biden win the election?", "Will Biden win the primary?");
        assert!(sim > dec!(0.6));

        let sim = word_similarity("Will it rain?", "BTC price above 100k?");
        assert!(sim < dec!(0.3));
    }

    #[test]
    fn test_detect_pair_buy_underpriced() {
        let cond_a = B256::left_padding_from(&[1]);
        let cond_b = B256::left_padding_from(&[2]);
        let market_a = make_market(cond_a, "Market A", 10, 11);
        let market_b = make_market(cond_b, "Market B", 20, 21);

        let pair = CrossMarketPair {
            pair_id: compute_pair_id(cond_a, cond_b),
            market_a,
            market_b,
            expected_sum: Decimal::ONE,
            correlation: CrossMarketCorrelation::ComplementaryYes,
        };

        // YES_a_ask=0.40, YES_b_ask=0.45 → sum=0.85 < 1.00, spread=0.15
        let mut books = StdHashMap::new();
        books.insert(U256::from(10), make_book(U256::from(10), dec!(0.38), dec!(0.40)));
        books.insert(U256::from(11), make_book(U256::from(11), dec!(0.58), dec!(0.60)));
        books.insert(U256::from(20), make_book(U256::from(20), dec!(0.43), dec!(0.45)));
        books.insert(U256::from(21), make_book(U256::from(21), dec!(0.53), dec!(0.55)));

        let strategy = CrossMarketArbitrage::new(
            100,
            dec!(1000),
            dec!(0.01),
            dec!(0.01),
            vec![pair],
            Box::new(move |id| books.get(&id).cloned()),
            Box::new(|| Decimal::MAX),
        );

        let opps = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(strategy.scan(&[]));
        let opps = opps.unwrap();
        assert_eq!(opps.len(), 1);
        assert_eq!(opps[0].strategy_type, StrategyType::CrossMarket);
        assert!(opps[0].estimated_profit > Decimal::ZERO);
    }

    #[test]
    fn test_detect_no_opportunity() {
        let cond_a = B256::left_padding_from(&[1]);
        let cond_b = B256::left_padding_from(&[2]);
        let market_a = make_market(cond_a, "Market A", 10, 11);
        let market_b = make_market(cond_b, "Market B", 20, 21);

        let pair = CrossMarketPair {
            pair_id: compute_pair_id(cond_a, cond_b),
            market_a,
            market_b,
            expected_sum: Decimal::ONE,
            correlation: CrossMarketCorrelation::ComplementaryYes,
        };

        // YES_a_ask=0.50, YES_b_ask=0.52 → sum=1.02 > 1.00 (no buy opportunity)
        // YES_a_bid=0.48, YES_b_bid=0.48 → sum=0.96 < 1.00 (no sell opportunity)
        let mut books = StdHashMap::new();
        books.insert(U256::from(10), make_book(U256::from(10), dec!(0.48), dec!(0.50)));
        books.insert(U256::from(11), make_book(U256::from(11), dec!(0.48), dec!(0.50)));
        books.insert(U256::from(20), make_book(U256::from(20), dec!(0.48), dec!(0.52)));
        books.insert(U256::from(21), make_book(U256::from(21), dec!(0.48), dec!(0.52)));

        let strategy = CrossMarketArbitrage::new(
            100,
            dec!(1000),
            dec!(0.01),
            dec!(0.01),
            vec![pair],
            Box::new(move |id| books.get(&id).cloned()),
            Box::new(|| Decimal::MAX),
        );

        let opps = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(strategy.scan(&[]));
        assert!(opps.unwrap().is_empty());
    }

    #[test]
    fn test_detect_cross_market_pairs_heuristic() {
        let markets = vec![
            make_market(B256::left_padding_from(&[1]), "Will Biden win the election in November?", 10, 11),
            make_market(B256::left_padding_from(&[2]), "Will Biden win the election in December?", 20, 21),
            make_market(B256::left_padding_from(&[3]), "BTC price above 100k?", 30, 31),
        ];

        let pairs = detect_cross_market_pairs(&markets);
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].correlation, CrossMarketCorrelation::ComplementaryYes);
    }
}
