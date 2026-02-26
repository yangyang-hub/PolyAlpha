use std::sync::Arc;

use alloy::primitives::U256;
use async_trait::async_trait;
use chrono::Utc;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use uuid::Uuid;

use pa_core::config::ConvergenceConfig;
use pa_core::traits::Strategy;
use pa_core::types::{
    ArbitrageOpportunity, ExecutionPlan, MarketInfo, OrderBook, StrategyType, TradeSide,
};

use crate::profitability::ProfitCalculator;

/// Resolution Convergence Strategy
///
/// Buys tokens priced near 0 or 1 in markets approaching their resolution date.
/// As markets near resolution, outcomes become increasingly certain and token
/// prices converge to their payout value (0.00 or 1.00). Buying a token at 0.95
/// that resolves to 1.00 yields ~5 cents per token minus fees.
///
/// Uses `DirectionalBuy` execution plan — CLOB FOK only, no on-chain operations.
pub struct ResolutionConvergenceStrategy {
    config: ConvergenceConfig,
    profit_calc: ProfitCalculator,
    get_orderbook: Box<dyn Fn(U256) -> Option<OrderBook> + Send + Sync>,
    get_available_capital: Box<dyn Fn() -> Decimal + Send + Sync>,
    get_position: Box<dyn Fn(U256) -> Decimal + Send + Sync>,
    /// Returns all held positions for this strategy: (token_id, size, avg_cost).
    get_held_positions: Box<dyn Fn() -> Vec<(U256, Decimal, Decimal)> + Send + Sync>,
    /// Returns current wallet USDC balance for dynamic position sizing.
    get_balance: Box<dyn Fn() -> Decimal + Send + Sync>,
    scan_count: Arc<std::sync::atomic::AtomicU64>,
}

impl ResolutionConvergenceStrategy {
    pub fn new(
        config: ConvergenceConfig,
        gas_cost_usd: Decimal,
        get_orderbook: Box<dyn Fn(U256) -> Option<OrderBook> + Send + Sync>,
        get_available_capital: Box<dyn Fn() -> Decimal + Send + Sync>,
        get_position: Box<dyn Fn(U256) -> Decimal + Send + Sync>,
        get_held_positions: Box<dyn Fn() -> Vec<(U256, Decimal, Decimal)> + Send + Sync>,
        get_balance: Box<dyn Fn() -> Decimal + Send + Sync>,
    ) -> Self {
        Self {
            config,
            profit_calc: ProfitCalculator::new(gas_cost_usd),
            get_orderbook,
            get_available_capital,
            get_position,
            get_held_positions,
            get_balance,
            scan_count: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        }
    }

    /// Detect a convergence opportunity on a single market.
    fn detect_convergence(&self, market: &MarketInfo) -> Option<ArbitrageOpportunity> {
        // Filter: must have end_date
        let end_date = market.end_date?;

        // Filter: must not be past
        let now = Utc::now();
        if end_date <= now {
            return None;
        }

        // Filter: must be within max_days_to_resolution
        let duration = end_date.signed_duration_since(now);
        let days_remaining = duration.num_hours() as f64 / 24.0;
        if days_remaining > self.config.max_days_to_resolution as f64 {
            return None;
        }

        // Filter: binary market (2 tokens), non-NegRisk, active
        if market.neg_risk || market.tokens.len() != 2 || !market.active {
            return None;
        }

        let yes_token = &market.tokens[0];
        let no_token = &market.tokens[1];

        let yes_book = (self.get_orderbook)(yes_token.token_id)?;
        let no_book = (self.get_orderbook)(no_token.token_id)?;

        let yes_ask = yes_book.best_ask()?.price;
        let no_ask = no_book.best_ask()?.price;

        // Pick the side above min_price_threshold (if both qualify, pick higher price)
        let yes_qualifies = yes_ask >= self.config.min_price_threshold;
        let no_qualifies = no_ask >= self.config.min_price_threshold;

        let (token_id, ask_price) = match (yes_qualifies, no_qualifies) {
            (true, true) => {
                if yes_ask >= no_ask {
                    (yes_token.token_id, yes_ask)
                } else {
                    (no_token.token_id, no_ask)
                }
            }
            (true, false) => (yes_token.token_id, yes_ask),
            (false, true) => (no_token.token_id, no_ask),
            (false, false) => return None,
        };

        // Model probability: how likely this token resolves to 1.00
        let max_days = self.config.max_days_to_resolution as f64;
        let model_prob_f64 = if self.config.time_decay_boost {
            // Closer to resolution → higher confidence
            // Range: (1-rate) at max_days to ~1.0 at 0 days
            1.0 - (days_remaining / max_days) * self.config.time_decay_rate
        } else {
            0.99
        };
        let model_prob = Decimal::from_f64_retain(model_prob_f64)?;

        // Edge = model_prob - ask_price
        if model_prob <= ask_price {
            return None;
        }
        let edge = model_prob - ask_price;

        // Dynamic position cap: fraction of current wallet balance
        let effective_max = (self.get_balance)() * self.config.max_position_pct;

        // Kelly sizing: f* = edge / (1 - price)
        let kelly_raw = if ask_price > Decimal::ZERO && ask_price < dec!(0.99) {
            (edge / (Decimal::ONE - ask_price)).min(Decimal::TWO)
        } else {
            Decimal::ZERO
        };
        let kelly_size = kelly_raw * self.config.kelly_fraction * effective_max;
        let available = (self.get_available_capital)();

        // Position-aware sizing: subtract existing position from max
        let existing = (self.get_position)(token_id);
        let remaining = (effective_max - existing).max(Decimal::ZERO);
        let size = kelly_size.min(remaining).min(available);

        // Skip if Kelly-sized order below CLOB minimum cost ($1.00)
        // Do NOT bump up — Kelly says "edge too small to bet" and we should respect that
        let size = if size > Decimal::ZERO && ask_price > Decimal::ZERO {
            let min_cost_size = (Decimal::ONE / ask_price).ceil();
            if size < min_cost_size {
                return None;
            }
            size
        } else {
            size
        };

        if size <= Decimal::ZERO {
            return None;
        }

        // Profitability check
        let est = self.profit_calc.directional_buy_profit(
            ask_price,
            model_prob,
            size,
            market.fee_rate_bps,
        );

        if est.net_profit <= Decimal::ZERO {
            return None;
        }

        tracing::debug!(
            question = %market.question,
            days_remaining = format!("{:.1}", days_remaining),
            ask_price = %ask_price,
            model_prob = %model_prob,
            edge = %edge,
            size = %size,
            est_profit = %est.net_profit,
            "Resolution convergence opportunity detected"
        );

        Some(ArbitrageOpportunity {
            id: Uuid::now_v7(),
            strategy_type: StrategyType::ResolutionConvergence,
            condition_id: market.condition_id,
            question: market.question.clone(),
            spread: edge,
            estimated_profit: est.net_profit,
            size,
            detected_at: Utc::now(),
            execution_plan: ExecutionPlan::DirectionalBuy {
                token_id,
                side: TradeSide::Buy,
                price: ask_price,
                size,
                condition_id: market.condition_id,
            },
        })
    }

    /// Scan held positions for exit conditions (model reversal or capital efficiency).
    fn scan_exits(&self, markets: &[MarketInfo]) -> Vec<ArbitrageOpportunity> {
        let held = (self.get_held_positions)();
        if held.is_empty() {
            return vec![];
        }

        tracing::debug!(
            held_positions = held.len(),
            "[Convergence] scanning exits"
        );

        // Build reverse map: token_id → market
        let token_to_market: std::collections::HashMap<U256, &MarketInfo> = markets
            .iter()
            .flat_map(|m| m.tokens.iter().map(move |t| (t.token_id, m)))
            .collect();

        let exit_buffer = Decimal::from(self.config.exit_buffer_bps) / dec!(10000);
        let mut exits = Vec::new();

        for (token_id, size, avg_cost) in &held {
            let book = match (self.get_orderbook)(*token_id) {
                Some(b) => b,
                None => {
                    tracing::debug!(token_id = %token_id, "[Convergence EXIT] no orderbook — token not subscribed?");
                    continue;
                }
            };
            let best_bid = match book.best_bid() {
                Some(b) => b.price,
                None => {
                    tracing::debug!(token_id = %token_id, "[Convergence EXIT] no bids in orderbook");
                    continue;
                }
            };

            // Capital efficiency exit: bid >= threshold
            if best_bid >= self.config.capital_efficiency_threshold {
                tracing::info!(
                    token_id = %token_id,
                    best_bid = %best_bid,
                    threshold = %self.config.capital_efficiency_threshold,
                    "[EXIT] Capital efficiency — convergence"
                );
                exits.push(self.build_exit_opportunity(*token_id, *size, *avg_cost, best_bid, &token_to_market));
                pa_monitor::metrics::EXIT_TRADES.inc();
                continue;
            }

            // Model reversal: recompute model_prob for this market
            let market = match token_to_market.get(token_id) {
                Some(m) => *m,
                None => {
                    tracing::debug!(
                        token_id = %token_id,
                        best_bid = %best_bid,
                        "[Convergence EXIT] token not in scanned markets"
                    );
                    continue;
                }
            };

            let end_date = match market.end_date {
                Some(d) if d > Utc::now() => d,
                _ => continue,
            };
            let days_remaining = (end_date.signed_duration_since(Utc::now()).num_hours() as f64) / 24.0;
            let max_days = self.config.max_days_to_resolution as f64;

            let model_prob_f64 = if self.config.time_decay_boost {
                1.0 - (days_remaining / max_days).min(1.0) * self.config.time_decay_rate
            } else {
                0.99
            };
            let model_prob = match Decimal::from_f64_retain(model_prob_f64) {
                Some(d) => d,
                None => continue,
            };

            if model_prob < best_bid - exit_buffer {
                tracing::info!(
                    token_id = %token_id,
                    model_prob = %model_prob,
                    best_bid = %best_bid,
                    exit_buffer = %exit_buffer,
                    "[EXIT] Model reversal — convergence"
                );
                exits.push(self.build_exit_opportunity(*token_id, *size, *avg_cost, best_bid, &token_to_market));
                pa_monitor::metrics::EXIT_TRADES.inc();
            } else {
                tracing::debug!(
                    token_id = %token_id,
                    model_prob = %model_prob,
                    best_bid = %best_bid,
                    exit_buffer = %exit_buffer,
                    threshold = %(best_bid - exit_buffer),
                    "[Convergence EXIT] No reversal: model_prob >= best_bid - buffer"
                );
            }
        }

        exits
    }

    /// Build an exit opportunity (sell via CLOB FOK).
    fn build_exit_opportunity(
        &self,
        token_id: U256,
        size: Decimal,
        avg_cost: Decimal,
        best_bid: Decimal,
        token_to_market: &std::collections::HashMap<U256, &MarketInfo>,
    ) -> ArbitrageOpportunity {
        let market = token_to_market.get(&token_id);
        let condition_id = market.map(|m| m.condition_id).unwrap_or_default();
        let question = market.map(|m| m.question.clone()).unwrap_or_default();
        let fee_rate_bps = market.map(|m| m.fee_rate_bps).unwrap_or(200);

        let est = self.profit_calc.directional_sell_profit(best_bid, avg_cost, size, fee_rate_bps);

        ArbitrageOpportunity {
            id: Uuid::now_v7(),
            strategy_type: StrategyType::ResolutionConvergence,
            condition_id,
            question: format!("[EXIT] {}", question),
            spread: best_bid - avg_cost,
            estimated_profit: est.net_profit,
            size,
            detected_at: Utc::now(),
            execution_plan: ExecutionPlan::DirectionalBuy {
                token_id,
                side: TradeSide::Sell,
                price: best_bid,
                size,
                condition_id,
            },
        }
    }
}

#[async_trait]
impl Strategy for ResolutionConvergenceStrategy {
    fn name(&self) -> &str {
        "ResolutionConvergence"
    }

    fn strategy_type(&self) -> StrategyType {
        StrategyType::ResolutionConvergence
    }

    async fn scan(
        &self,
        markets: &[MarketInfo],
    ) -> pa_core::Result<Vec<ArbitrageOpportunity>> {
        let mut opportunities = Vec::new();
        let count = self.scan_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let log_diag = count.is_multiple_of(600);

        let now = Utc::now();
        let max_days = self.config.max_days_to_resolution as f64;
        let mut with_end_date = 0u32;
        let mut within_days = 0u32;
        let mut above_threshold = 0u32;
        let mut best_edge_bps: i32 = 0;
        let mut best_edge_market: Option<String> = None;
        // Fee-adjusted profitability tracking
        let mut best_net_edge_bps: i32 = i32::MIN;
        let mut best_net_edge_info: Option<String> = None;

        for market in markets {
            if !market.active || market.neg_risk {
                continue;
            }

            // Track diagnostic stats
            if let Some(end_date) = market.end_date {
                if end_date > now {
                    with_end_date += 1;
                    let days = (end_date.signed_duration_since(now).num_hours() as f64) / 24.0;
                    if days <= max_days {
                        within_days += 1;

                        // Check if any side is above price threshold
                        if market.tokens.len() == 2 {
                            let yes_ask = (self.get_orderbook)(market.tokens[0].token_id)
                                .and_then(|b| b.best_ask().map(|a| a.price));
                            let no_ask = (self.get_orderbook)(market.tokens[1].token_id)
                                .and_then(|b| b.best_ask().map(|a| a.price));

                            let high_ask = match (yes_ask, no_ask) {
                                (Some(y), Some(n)) => Some(y.max(n)),
                                (Some(y), None) => Some(y),
                                (None, Some(n)) => Some(n),
                                _ => None,
                            };

                            if let Some(ask) = high_ask {
                                if ask >= self.config.min_price_threshold {
                                    above_threshold += 1;
                                }
                                // Track best edge even for markets below threshold
                                let model_prob_f64 = if self.config.time_decay_boost {
                                    1.0 - (days / max_days) * self.config.time_decay_rate
                                } else {
                                    0.99
                                };
                                if let Some(mp) = Decimal::from_f64_retain(model_prob_f64) {
                                    use rust_decimal::prelude::ToPrimitive;
                                    let edge = mp - ask;
                                    let ebps = (edge * dec!(10000)).to_i32().unwrap_or(0);
                                    if ebps > best_edge_bps {
                                        best_edge_bps = ebps;
                                        best_edge_market =
                                            Some(market.question.chars().take(50).collect());
                                    }
                                    // Fee-adjusted net edge: edge - fee
                                    let fee = self.profit_calc.capped_fee(ask, market.fee_rate_bps);
                                    let net_edge = edge - fee;
                                    let net_ebps = (net_edge * dec!(10000)).to_i32().unwrap_or(0);
                                    if net_ebps > best_net_edge_bps {
                                        best_net_edge_bps = net_ebps;
                                        let fee_bps = (fee * dec!(10000)).to_i32().unwrap_or(0);
                                        best_net_edge_info = Some(format!(
                                            "{} (ask:{}, edge:{}bps, fee:{}bps, net:{}bps, days:{:.0})",
                                            &market.question[..market.question.len().min(40)],
                                            ask, ebps, fee_bps, net_ebps, days,
                                        ));
                                    }
                                }
                            }
                        }
                    }
                }
            }

            if let Some(opp) = self.detect_convergence(market) {
                opportunities.push(opp);
            }
        }

        if log_diag {
            tracing::debug!(
                with_end_date,
                within_days,
                above_threshold,
                best_edge_bps,
                best_edge_market = best_edge_market.as_deref().unwrap_or("none"),
                best_net_edge_bps,
                min_price_threshold = %self.config.min_price_threshold,
                max_days = self.config.max_days_to_resolution,
                time_decay_rate = self.config.time_decay_rate,
                opportunities = opportunities.len(),
                "[Convergence] scan diagnostics"
            );
            if let Some(ref info) = best_net_edge_info {
                tracing::debug!(info = %info, "[Convergence] best net-edge opportunity");
            }
        }

        // Exit scanning: check held positions for model reversal / capital efficiency
        let exit_opps = self.scan_exits(markets);
        opportunities.extend(exit_opps);

        Ok(opportunities)
    }
}

// ──── Tests ────

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::B256;
    use chrono::{Duration, Utc};
    use pa_core::types::{Outcome, PriceLevel, TokenInfo};
    use rust_decimal_macros::dec;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    fn default_config() -> ConvergenceConfig {
        ConvergenceConfig {
            min_price_threshold: dec!(0.93),
            max_days_to_resolution: 7,
            max_position_pct: dec!(0.50),
            kelly_fraction: dec!(0.25),
            time_decay_boost: true,
            time_decay_rate: 0.03,
            exit_buffer_bps: 50,
            capital_efficiency_threshold: dec!(0.98),
        }
    }

    fn make_market(
        end_date: Option<chrono::DateTime<Utc>>,
        neg_risk: bool,
    ) -> MarketInfo {
        MarketInfo {
            condition_id: B256::ZERO,
            question_id: B256::ZERO,
            question: "Will event X happen?".to_string(),
            neg_risk,
            neg_risk_market_id: None,
            tokens: vec![
                TokenInfo {
                    token_id: U256::from(1u64),
                    outcome: Outcome::Yes,
                    complement_id: U256::from(2u64),
                },
                TokenInfo {
                    token_id: U256::from(2u64),
                    outcome: Outcome::No,
                    complement_id: U256::from(1u64),
                },
            ],
            tick_size: dec!(0.01),
            fee_rate_bps: 200,
            active: true,
            liquidity: dec!(1000),
            event_title: None,
            end_date,
            category: None,
            outcome_prices: None,
            gamma_best_bid: None,
            gamma_best_ask: None,
        }
    }

    fn make_book(token_id: U256, best_ask_price: Decimal) -> OrderBook {
        OrderBook {
            token_id,
            bids: vec![PriceLevel {
                price: best_ask_price - dec!(0.02),
                size: dec!(500),
            }],
            asks: vec![PriceLevel {
                price: best_ask_price,
                size: dec!(500),
            }],
            timestamp: Utc::now(),
        }
    }

    fn make_strategy(
        config: ConvergenceConfig,
        books: HashMap<U256, OrderBook>,
        position: Decimal,
    ) -> ResolutionConvergenceStrategy {
        let books = Arc::new(books);
        let pos = Arc::new(Mutex::new(position));
        ResolutionConvergenceStrategy::new(
            config,
            dec!(0.00), // no gas for CLOB-only
            Box::new(move |tid| books.get(&tid).cloned()),
            Box::new(|| Decimal::MAX),
            Box::new(move |_| *pos.lock().unwrap()),
            Box::new(|| vec![]), // no held positions in tests
            Box::new(|| dec!(200)), // test balance $200
        )
    }

    #[test]
    fn test_filters_market_without_end_date() {
        let market = make_market(None, false);
        let mut books = HashMap::new();
        books.insert(U256::from(1u64), make_book(U256::from(1u64), dec!(0.95)));
        books.insert(U256::from(2u64), make_book(U256::from(2u64), dec!(0.05)));
        let strategy = make_strategy(default_config(), books, Decimal::ZERO);

        assert!(strategy.detect_convergence(&market).is_none());
    }

    #[test]
    fn test_filters_market_too_far() {
        let market = make_market(Some(Utc::now() + Duration::days(30)), false);
        let mut books = HashMap::new();
        books.insert(U256::from(1u64), make_book(U256::from(1u64), dec!(0.95)));
        books.insert(U256::from(2u64), make_book(U256::from(2u64), dec!(0.05)));
        let strategy = make_strategy(default_config(), books, Decimal::ZERO);

        assert!(strategy.detect_convergence(&market).is_none());
    }

    #[test]
    fn test_filters_market_past_end_date() {
        let market = make_market(Some(Utc::now() - Duration::hours(1)), false);
        let mut books = HashMap::new();
        books.insert(U256::from(1u64), make_book(U256::from(1u64), dec!(0.95)));
        books.insert(U256::from(2u64), make_book(U256::from(2u64), dec!(0.05)));
        let strategy = make_strategy(default_config(), books, Decimal::ZERO);

        assert!(strategy.detect_convergence(&market).is_none());
    }

    #[test]
    fn test_filters_price_below_threshold() {
        // YES@0.80, NO@0.20 — both below 0.93 threshold
        let market = make_market(Some(Utc::now() + Duration::days(3)), false);
        let mut books = HashMap::new();
        books.insert(U256::from(1u64), make_book(U256::from(1u64), dec!(0.80)));
        books.insert(U256::from(2u64), make_book(U256::from(2u64), dec!(0.20)));
        let strategy = make_strategy(default_config(), books, Decimal::ZERO);

        assert!(strategy.detect_convergence(&market).is_none());
    }

    #[test]
    fn test_detects_yes_convergence() {
        // YES@0.95, 3 days out → should detect opportunity on YES side
        let market = make_market(Some(Utc::now() + Duration::days(3)), false);
        let mut books = HashMap::new();
        books.insert(U256::from(1u64), make_book(U256::from(1u64), dec!(0.95)));
        books.insert(U256::from(2u64), make_book(U256::from(2u64), dec!(0.05)));
        let strategy = make_strategy(default_config(), books, Decimal::ZERO);

        let opp = strategy.detect_convergence(&market).unwrap();
        assert_eq!(opp.strategy_type, StrategyType::ResolutionConvergence);
        // Should buy the YES token
        match &opp.execution_plan {
            ExecutionPlan::DirectionalBuy { token_id, price, .. } => {
                assert_eq!(*token_id, U256::from(1u64));
                assert_eq!(*price, dec!(0.95));
            }
            _ => panic!("Expected DirectionalBuy"),
        }
    }

    #[test]
    fn test_detects_no_convergence() {
        // NO@0.96, 2 days out → should detect opportunity on NO side
        let market = make_market(Some(Utc::now() + Duration::days(2)), false);
        let mut books = HashMap::new();
        books.insert(U256::from(1u64), make_book(U256::from(1u64), dec!(0.04)));
        books.insert(U256::from(2u64), make_book(U256::from(2u64), dec!(0.96)));
        let strategy = make_strategy(default_config(), books, Decimal::ZERO);

        let opp = strategy.detect_convergence(&market).unwrap();
        match &opp.execution_plan {
            ExecutionPlan::DirectionalBuy { token_id, price, .. } => {
                assert_eq!(*token_id, U256::from(2u64));
                assert_eq!(*price, dec!(0.96));
            }
            _ => panic!("Expected DirectionalBuy"),
        }
    }

    #[test]
    fn test_picks_higher_conviction() {
        // YES@0.97 vs NO@0.94 → should pick YES (higher price, stronger conviction)
        let market = make_market(Some(Utc::now() + Duration::days(2)), false);
        let mut books = HashMap::new();
        books.insert(U256::from(1u64), make_book(U256::from(1u64), dec!(0.97)));
        books.insert(U256::from(2u64), make_book(U256::from(2u64), dec!(0.94)));
        let strategy = make_strategy(default_config(), books, Decimal::ZERO);

        let opp = strategy.detect_convergence(&market).unwrap();
        match &opp.execution_plan {
            ExecutionPlan::DirectionalBuy { token_id, .. } => {
                assert_eq!(*token_id, U256::from(1u64), "Should pick YES (higher price)");
            }
            _ => panic!("Expected DirectionalBuy"),
        }
    }

    #[test]
    fn test_time_decay_boost() {
        // 1 day remaining → higher model_prob than 7 days
        let config = default_config();
        let max_days = config.max_days_to_resolution as f64;
        let rate = config.time_decay_rate;

        let days_1 = 1.0_f64;
        let days_7 = 7.0_f64;
        let prob_1 = 1.0 - (days_1 / max_days) * rate;
        let prob_7 = 1.0 - (days_7 / max_days) * rate;

        assert!(
            prob_1 > prob_7,
            "1 day ({}) should have higher prob than 7 days ({})",
            prob_1,
            prob_7
        );
        // 1 day: ~0.9957, 7 days: ~0.97
        assert!(prob_1 > 0.99);
        assert!(prob_7 < 0.98);
    }

    #[test]
    fn test_position_aware_sizing() {
        // Existing $80, max $100 → only $20 remaining
        let market = make_market(Some(Utc::now() + Duration::days(3)), false);
        let mut books = HashMap::new();
        books.insert(U256::from(1u64), make_book(U256::from(1u64), dec!(0.95)));
        books.insert(U256::from(2u64), make_book(U256::from(2u64), dec!(0.05)));
        let strategy = make_strategy(default_config(), books, dec!(80));

        let opp = strategy.detect_convergence(&market).unwrap();
        match &opp.execution_plan {
            ExecutionPlan::DirectionalBuy { size, .. } => {
                assert!(
                    *size <= dec!(20),
                    "Size {} should be <= $20 (max $100 - existing $80)",
                    size
                );
            }
            _ => panic!("Expected DirectionalBuy"),
        }
    }

    #[test]
    fn test_skips_neg_risk() {
        let market = make_market(Some(Utc::now() + Duration::days(3)), true);
        let mut books = HashMap::new();
        books.insert(U256::from(1u64), make_book(U256::from(1u64), dec!(0.95)));
        books.insert(U256::from(2u64), make_book(U256::from(2u64), dec!(0.05)));
        let strategy = make_strategy(default_config(), books, Decimal::ZERO);

        assert!(strategy.detect_convergence(&market).is_none());
    }

    // ──── Exit Tests ────

    fn make_strategy_with_exits(
        config: ConvergenceConfig,
        books: HashMap<U256, OrderBook>,
        held: Vec<(U256, Decimal, Decimal)>,
    ) -> ResolutionConvergenceStrategy {
        let books = Arc::new(books);
        ResolutionConvergenceStrategy::new(
            config,
            dec!(0.00),
            Box::new(move |tid| books.get(&tid).cloned()),
            Box::new(|| Decimal::MAX),
            Box::new(|_| Decimal::ZERO),
            Box::new(move || held.clone()),
            Box::new(|| dec!(200)), // test balance $200
        )
    }

    fn make_book_with_bid(token_id: U256, best_bid: Decimal) -> OrderBook {
        OrderBook {
            token_id,
            bids: vec![PriceLevel {
                price: best_bid,
                size: dec!(500),
            }],
            asks: vec![PriceLevel {
                price: best_bid + dec!(0.02),
                size: dec!(500),
            }],
            timestamp: Utc::now(),
        }
    }

    #[test]
    fn test_exit_capital_efficiency() {
        // Held token with bid=0.99 → above threshold 0.98 → should emit exit
        let token_id = U256::from(1u64);
        let market = make_market(Some(Utc::now() + Duration::days(3)), false);
        let mut books = HashMap::new();
        books.insert(token_id, make_book_with_bid(token_id, dec!(0.99)));
        books.insert(U256::from(2u64), make_book_with_bid(U256::from(2u64), dec!(0.01)));

        let held = vec![(token_id, dec!(50), dec!(0.95))];
        let strategy = make_strategy_with_exits(default_config(), books, held);

        let exits = strategy.scan_exits(&[market]);
        assert_eq!(exits.len(), 1);
        assert!(exits[0].question.starts_with("[EXIT]"));
        match &exits[0].execution_plan {
            ExecutionPlan::DirectionalBuy { side, price, size, .. } => {
                assert_eq!(*side, TradeSide::Sell);
                assert_eq!(*price, dec!(0.99));
                assert_eq!(*size, dec!(50));
            }
            _ => panic!("Expected DirectionalBuy with Sell side"),
        }
    }

    #[test]
    fn test_exit_model_reversal() {
        // Market far from resolution (days_remaining=6.9 out of 7) → model_prob ~0.97
        // bid=0.985 → model_prob(0.97) < bid(0.985) - buffer(0.005) = 0.98 → should exit
        let token_id = U256::from(1u64);
        let market = make_market(
            Some(Utc::now() + Duration::hours(24 * 7 - 2)), // ~6.9 days
            false,
        );
        let mut books = HashMap::new();
        books.insert(token_id, make_book_with_bid(token_id, dec!(0.985)));
        books.insert(U256::from(2u64), make_book_with_bid(U256::from(2u64), dec!(0.015)));

        let held = vec![(token_id, dec!(30), dec!(0.94))];
        let strategy = make_strategy_with_exits(default_config(), books, held);

        let exits = strategy.scan_exits(&[market]);
        assert_eq!(exits.len(), 1, "Should detect model reversal exit");
        assert!(exits[0].question.starts_with("[EXIT]"));
    }

    #[test]
    fn test_exit_no_positions() {
        // No held positions → no exits
        let market = make_market(Some(Utc::now() + Duration::days(3)), false);
        let mut books = HashMap::new();
        books.insert(U256::from(1u64), make_book_with_bid(U256::from(1u64), dec!(0.95)));
        books.insert(U256::from(2u64), make_book_with_bid(U256::from(2u64), dec!(0.05)));

        let strategy = make_strategy_with_exits(default_config(), books, vec![]);

        let exits = strategy.scan_exits(&[market]);
        assert!(exits.is_empty());
    }

    #[test]
    fn test_exit_edge_still_positive() {
        // Market close to resolution (1 day) → model_prob ~0.9957
        // bid=0.96 → model_prob(0.9957) > bid(0.96) - buffer(0.005) = 0.955 → no exit
        let token_id = U256::from(1u64);
        let market = make_market(
            Some(Utc::now() + Duration::days(1)),
            false,
        );
        let mut books = HashMap::new();
        books.insert(token_id, make_book_with_bid(token_id, dec!(0.96)));
        books.insert(U256::from(2u64), make_book_with_bid(U256::from(2u64), dec!(0.04)));

        let held = vec![(token_id, dec!(50), dec!(0.94))];
        let strategy = make_strategy_with_exits(default_config(), books, held);

        let exits = strategy.scan_exits(&[market]);
        assert!(exits.is_empty(), "Edge still positive, should not exit");
    }
}
