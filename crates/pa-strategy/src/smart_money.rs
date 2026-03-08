use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use alloy::primitives::{B256, U256};
use async_trait::async_trait;
use chrono::Utc;
use rust_decimal::Decimal;
use uuid::Uuid;

use pa_core::config::SmartMoneyConfig;
use pa_core::traits::Strategy;
use pa_core::types::{
    TradingOpportunity, ExecutionPlan, MarketInfo, OrderBook, StrategyType, TradeSide,
};
use pa_market_data::wallet_tracker::{SignalType, SmartMoneySignal};

use crate::profitability::ProfitCalculator;

// ──── Aggregated Signal ────

struct AggregatedSignal {
    signal_type: SignalType,
    token_id: U256,
    condition_id: B256,
    /// Weighted target size = sum(wallet_size * follow_ratio * weight) for entries,
    /// or sum(delta * follow_ratio * weight) for exits.
    target_size: Decimal,
    wallet_count: usize,
}

// ──── SmartMoneyStrategy ────

pub struct SmartMoneyStrategy {
    config: SmartMoneyConfig,
    profit_calc: ProfitCalculator,
    get_orderbook: Box<dyn Fn(U256) -> Option<OrderBook> + Send + Sync>,
    get_available_capital: Box<dyn Fn() -> Decimal + Send + Sync>,
    get_position: Box<dyn Fn(U256) -> Decimal + Send + Sync>,
    get_held_positions: Box<dyn Fn() -> Vec<(U256, Decimal, Decimal)> + Send + Sync>,
    /// Returns current wallet USDC balance (reserved for future dynamic sizing).
    #[allow(dead_code)]
    get_balance: Box<dyn Fn() -> Decimal + Send + Sync>,
    /// Shared signal queue from WalletTracker.
    signals: Arc<RwLock<Vec<SmartMoneySignal>>>,
    /// Markets lookup: condition_id → MarketInfo (for fee_rate_bps etc.)
    markets: Arc<RwLock<HashMap<B256, MarketInfo>>>,
}

impl SmartMoneyStrategy {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: SmartMoneyConfig,
        gas_cost_usd: Decimal,
        get_orderbook: Box<dyn Fn(U256) -> Option<OrderBook> + Send + Sync>,
        get_available_capital: Box<dyn Fn() -> Decimal + Send + Sync>,
        get_position: Box<dyn Fn(U256) -> Decimal + Send + Sync>,
        get_held_positions: Box<dyn Fn() -> Vec<(U256, Decimal, Decimal)> + Send + Sync>,
        get_balance: Box<dyn Fn() -> Decimal + Send + Sync>,
        signals: Arc<RwLock<Vec<SmartMoneySignal>>>,
        markets: Arc<RwLock<HashMap<B256, MarketInfo>>>,
    ) -> Self {
        Self {
            config,
            profit_calc: ProfitCalculator::new(gas_cost_usd),
            get_orderbook,
            get_available_capital,
            get_position,
            get_held_positions,
            get_balance,
            signals,
            markets,
        }
    }

    /// Update the internal markets lookup from the latest scan data.
    fn update_markets(&self, markets: &[MarketInfo]) {
        let mut map = self.markets.write().unwrap();
        for m in markets {
            map.insert(m.condition_id, m.clone());
        }
    }

    /// Consume and clear pending signals from the WalletTracker.
    fn consume_signals(&self) -> Vec<SmartMoneySignal> {
        let mut signals = self.signals.write().unwrap();
        std::mem::take(&mut *signals)
    }

    /// Aggregate signals by token_id. Multiple wallets entering the same market
    /// produce a single aggregated signal with combined target size.
    fn aggregate_signals(&self, signals: &[SmartMoneySignal]) -> HashMap<U256, AggregatedSignal> {
        let mut map: HashMap<U256, AggregatedSignal> = HashMap::new();
        let follow_ratio = self.config.follow_ratio;

        for sig in signals {
            let entry = map.entry(sig.token_id).or_insert_with(|| AggregatedSignal {
                signal_type: sig.signal_type,
                token_id: sig.token_id,
                condition_id: sig.condition_id,
                target_size: Decimal::ZERO,
                wallet_count: 0,
            });

            match sig.signal_type {
                SignalType::Entry | SignalType::Increase => {
                    // For entries: target = wallet_size * follow_ratio * weight
                    entry.target_size += sig.wallet_size * follow_ratio * sig.wallet_weight;
                    // Keep as Entry/Increase
                    if entry.signal_type == SignalType::Exit || entry.signal_type == SignalType::Decrease {
                        entry.signal_type = sig.signal_type;
                    }
                }
                SignalType::Decrease | SignalType::Exit => {
                    // For exits: target = delta * follow_ratio * weight
                    entry.target_size += sig.delta * follow_ratio * sig.wallet_weight;
                    // Only override to exit if all signals are exit-like
                    if entry.wallet_count == 0 {
                        entry.signal_type = sig.signal_type;
                    }
                }
            }
            entry.wallet_count += 1;
        }

        map
    }

    /// Process an aggregated entry signal → TradingOpportunity.
    fn process_entry_signal(&self, agg: &AggregatedSignal) -> Option<TradingOpportunity> {
        let book = (self.get_orderbook)(agg.token_id)?;
        let best_ask = book.best_ask()?.price;

        if best_ask <= Decimal::ZERO || best_ask >= Decimal::ONE {
            return None;
        }

        let markets = self.markets.read().unwrap();
        let market = markets.get(&agg.condition_id)?;

        // Proportional sizing, capped by max_position_usdc and available capital
        let raw_size = agg.target_size;
        let existing = (self.get_position)(agg.token_id);
        let max_shares = self.config.max_position_usdc / best_ask;
        let remaining = (max_shares - existing).max(Decimal::ZERO);
        let available_capital = (self.get_available_capital)();
        let max_from_capital = available_capital / best_ask;
        let size = raw_size.min(remaining).min(max_from_capital);

        // Below CLOB minimum ($1.00 cost)
        if size <= Decimal::ZERO || size * best_ask < Decimal::ONE {
            return None;
        }

        // Profitability check (use 1.0 as model_prob since we're following smart money)
        let est = self.profit_calc.directional_buy_profit(
            best_ask,
            Decimal::ONE, // assume token resolves to 1.0
            size,
            market.fee_rate_bps,
        );

        if est.net_profit <= Decimal::ZERO {
            return None;
        }

        tracing::info!(
            question = %market.question,
            wallets = agg.wallet_count,
            target_size = %raw_size,
            actual_size = %size,
            price = %best_ask,
            "SmartMoney: following entry signal"
        );

        Some(TradingOpportunity {
            id: Uuid::now_v7(),
            strategy_type: StrategyType::SmartMoney,
            condition_id: agg.condition_id,
            question: market.question.clone(),
            spread: Decimal::ZERO, // No model edge — following smart money
            estimated_profit: est.net_profit,
            size,
            detected_at: Utc::now(),
            execution_plan: ExecutionPlan::DirectionalBuy {
                token_id: agg.token_id,
                side: TradeSide::Buy,
                price: best_ask,
                size,
                condition_id: agg.condition_id,
            },
        })
    }

    /// Process an aggregated exit signal → TradingOpportunity.
    fn process_exit_signal(&self, agg: &AggregatedSignal) -> Option<TradingOpportunity> {
        let our_position = (self.get_position)(agg.token_id);
        if our_position <= Decimal::ZERO {
            return None;
        }

        let book = (self.get_orderbook)(agg.token_id)?;
        let best_bid = book.best_bid()?.price;

        if best_bid <= Decimal::ZERO {
            return None;
        }

        let markets = self.markets.read().unwrap();
        let market = markets.get(&agg.condition_id)?;

        // Sell min(our position, proportional to wallet exit)
        let sell_size = our_position.min(agg.target_size);
        if sell_size <= Decimal::ZERO || sell_size * best_bid < Decimal::ONE {
            return None;
        }

        // Cap to available bid depth
        let bid_depth = book.available_depth(TradeSide::Sell, best_bid);
        let sell_size = sell_size.min(bid_depth);
        if sell_size <= Decimal::ZERO {
            return None;
        }

        tracing::info!(
            question = %market.question,
            wallets = agg.wallet_count,
            sell_size = %sell_size,
            price = %best_bid,
            "SmartMoney: following exit signal"
        );

        Some(TradingOpportunity {
            id: Uuid::now_v7(),
            strategy_type: StrategyType::SmartMoney,
            condition_id: agg.condition_id,
            question: format!("[EXIT] {}", market.question),
            spread: Decimal::ZERO,
            estimated_profit: Decimal::ZERO,
            size: sell_size,
            detected_at: Utc::now(),
            execution_plan: ExecutionPlan::DirectionalBuy {
                token_id: agg.token_id,
                side: TradeSide::Sell,
                price: best_bid,
                size: sell_size,
                condition_id: agg.condition_id,
            },
        })
    }

    /// Scan held positions for capital efficiency exits (best_bid >= threshold).
    fn scan_exits(&self) -> Vec<TradingOpportunity> {
        let held = (self.get_held_positions)();
        if held.is_empty() {
            return vec![];
        }

        let mut exits = Vec::new();
        let markets = self.markets.read().unwrap();

        for (token_id, size, avg_cost) in &held {
            let book = match (self.get_orderbook)(*token_id) {
                Some(b) => b,
                None => continue,
            };
            let best_bid = match book.best_bid() {
                Some(b) => b.price,
                None => continue,
            };

            // Capital efficiency exit
            if best_bid >= self.config.capital_efficiency_threshold {
                // Find condition_id for this token
                let condition_id = markets
                    .values()
                    .find(|m| m.tokens.iter().any(|t| t.token_id == *token_id))
                    .map(|m| m.condition_id)
                    .unwrap_or_default();
                let question = markets
                    .values()
                    .find(|m| m.tokens.iter().any(|t| t.token_id == *token_id))
                    .map(|m| m.question.clone())
                    .unwrap_or_default();
                let fee_rate_bps = markets
                    .values()
                    .find(|m| m.tokens.iter().any(|t| t.token_id == *token_id))
                    .map(|m| m.fee_rate_bps)
                    .unwrap_or(200);

                let est = self.profit_calc.directional_sell_profit(
                    best_bid, *avg_cost, *size, fee_rate_bps,
                );

                tracing::info!(
                    token_id = %token_id,
                    best_bid = %best_bid,
                    "[SmartMoney EXIT] Capital efficiency"
                );

                exits.push(TradingOpportunity {
                    id: Uuid::now_v7(),
                    strategy_type: StrategyType::SmartMoney,
                    condition_id,
                    question: format!("[EXIT] {}", question),
                    spread: best_bid - *avg_cost,
                    estimated_profit: est.net_profit,
                    size: *size,
                    detected_at: Utc::now(),
                    execution_plan: ExecutionPlan::DirectionalBuy {
                        token_id: *token_id,
                        side: TradeSide::Sell,
                        price: best_bid,
                        size: *size,
                        condition_id,
                    },
                });
            }
        }

        exits
    }
}

#[async_trait]
impl Strategy for SmartMoneyStrategy {
    fn name(&self) -> &str {
        "SmartMoney"
    }

    fn strategy_type(&self) -> StrategyType {
        StrategyType::SmartMoney
    }

    async fn scan(
        &self,
        markets: &[MarketInfo],
    ) -> pa_core::Result<Vec<TradingOpportunity>> {
        // Update markets lookup
        self.update_markets(markets);

        // Consume pending signals
        let signals = self.consume_signals();

        if signals.is_empty() {
            // No new signals — just run exit scan
            return Ok(self.scan_exits());
        }

        // Aggregate by token_id
        let aggregated = self.aggregate_signals(&signals);

        // Generate opportunities
        let mut opps = Vec::new();
        for (_token_id, agg) in &aggregated {
            let opp = match agg.signal_type {
                SignalType::Entry | SignalType::Increase => self.process_entry_signal(agg),
                SignalType::Decrease | SignalType::Exit => self.process_exit_signal(agg),
            };
            if let Some(o) = opp {
                opps.push(o);
            }
        }

        // Append capital efficiency exits
        opps.extend(self.scan_exits());

        Ok(opps)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pa_core::types::{OrderBook, PriceLevel, TokenInfo, Outcome};
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

    fn make_market(condition_id: B256, token_id: U256) -> MarketInfo {
        MarketInfo {
            condition_id,
            question_id: B256::ZERO,
            question: "Test market".to_string(),
            neg_risk: false,
            neg_risk_market_id: None,
            tokens: vec![TokenInfo {
                token_id,
                outcome: Outcome::Yes,
                complement_id: U256::from(999u64),
            }],
            tick_size: dec!(0.01),
            fee_rate_bps: 200,
            active: true,
            liquidity: dec!(1000),
            event_title: None,
            end_date: None,
            category: None,
            outcome_prices: None,
            gamma_best_bid: None,
            gamma_best_ask: None,
            rewards_min_size: None,
            rewards_max_spread: None,
            rewards_daily_rate: None,
            holding_rewards_enabled: false,
            fees_enabled: true,
        }
    }

    fn make_strategy(
        book: OrderBook,
        position: Decimal,
        balance: Decimal,
        signals: Vec<SmartMoneySignal>,
        market: MarketInfo,
    ) -> SmartMoneyStrategy {
        let config = SmartMoneyConfig {
            follow_ratio: dec!(0.10),
            max_position_usdc: dec!(100),
            capital_efficiency_threshold: dec!(0.98),
            ..Default::default()
        };

        let book = Arc::new(book);
        let signals_arc = Arc::new(RwLock::new(signals));
        let markets_arc = Arc::new(RwLock::new(
            HashMap::from([(market.condition_id, market)]),
        ));

        SmartMoneyStrategy::new(
            config,
            dec!(0.00),
            Box::new(move |_| Some((*book).clone())),
            Box::new(move || balance),
            Box::new(move |_| position),
            Box::new(|| vec![]),
            Box::new(move || balance),
            signals_arc,
            markets_arc,
        )
    }

    #[test]
    fn test_aggregate_multi_wallet() {
        let config = SmartMoneyConfig {
            follow_ratio: dec!(0.10),
            ..Default::default()
        };

        let signals = vec![
            SmartMoneySignal {
                signal_type: SignalType::Entry,
                wallet_address: "0xaaa".to_string(),
                wallet_weight: Decimal::ONE,
                token_id: U256::from(42u64),
                condition_id: B256::ZERO,
                wallet_size: dec!(1000),
                delta: dec!(1000),
                detected_at: Utc::now(),
            },
            SmartMoneySignal {
                signal_type: SignalType::Entry,
                wallet_address: "0xbbb".to_string(),
                wallet_weight: dec!(0.5),
                token_id: U256::from(42u64),
                condition_id: B256::ZERO,
                wallet_size: dec!(2000),
                delta: dec!(2000),
                detected_at: Utc::now(),
            },
        ];

        let strategy = SmartMoneyStrategy {
            config,
            profit_calc: ProfitCalculator::new(dec!(0)),
            get_orderbook: Box::new(|_| None),
            get_available_capital: Box::new(|| dec!(1000)),
            get_position: Box::new(|_| Decimal::ZERO),
            get_held_positions: Box::new(|| vec![]),
            get_balance: Box::new(|| dec!(1000)),
            signals: Arc::new(RwLock::new(vec![])),
            markets: Arc::new(RwLock::new(HashMap::new())),
        };

        let aggregated = strategy.aggregate_signals(&signals);
        let agg = aggregated.get(&U256::from(42u64)).unwrap();

        assert_eq!(agg.wallet_count, 2);
        // 1000 * 0.10 * 1.0 + 2000 * 0.10 * 0.5 = 100 + 100 = 200
        assert_eq!(agg.target_size, dec!(200));
    }

    #[test]
    fn test_entry_proportional_sizing() {
        let token_id = U256::from(42u64);
        let cid = B256::ZERO;
        let book = make_book(
            vec![(dec!(0.55), dec!(500))],
            vec![(dec!(0.60), dec!(500))],
        );
        let market = make_market(cid, token_id);

        let signals = vec![SmartMoneySignal {
            signal_type: SignalType::Entry,
            wallet_address: "0xaaa".to_string(),
            wallet_weight: Decimal::ONE,
            token_id,
            condition_id: cid,
            wallet_size: dec!(500), // wallet has 500 shares
            delta: dec!(500),
            detected_at: Utc::now(),
        }];

        let strategy = make_strategy(book, Decimal::ZERO, dec!(1000), signals, market);
        let consumed = strategy.consume_signals();
        let aggregated = strategy.aggregate_signals(&consumed);
        let agg = aggregated.get(&token_id).unwrap();

        // target_size = 500 * 0.10 * 1.0 = 50
        assert_eq!(agg.target_size, dec!(50));

        let opp = strategy.process_entry_signal(agg);
        assert!(opp.is_some());
        let opp = opp.unwrap();
        assert_eq!(opp.size, dec!(50)); // 50 shares at 0.60 = $30 cost, fits in $1000 balance
    }

    #[test]
    fn test_exit_follows_wallet() {
        let token_id = U256::from(42u64);
        let cid = B256::ZERO;
        let book = make_book(
            vec![(dec!(0.55), dec!(500))],
            vec![(dec!(0.60), dec!(500))],
        );
        let market = make_market(cid, token_id);

        let signals = vec![SmartMoneySignal {
            signal_type: SignalType::Exit,
            wallet_address: "0xaaa".to_string(),
            wallet_weight: Decimal::ONE,
            token_id,
            condition_id: cid,
            wallet_size: Decimal::ZERO,
            delta: dec!(200),
            detected_at: Utc::now(),
        }];

        // We hold 30 shares
        let strategy = make_strategy(book, dec!(30), dec!(1000), signals, market);
        let consumed = strategy.consume_signals();
        let aggregated = strategy.aggregate_signals(&consumed);
        let agg = aggregated.get(&token_id).unwrap();

        // target_size = 200 * 0.10 * 1.0 = 20
        assert_eq!(agg.target_size, dec!(20));

        let opp = strategy.process_exit_signal(agg);
        assert!(opp.is_some());
        let opp = opp.unwrap();
        // sell min(our 30, target 20) = 20, capped by bid depth 500
        assert_eq!(opp.size, dec!(20));
        match opp.execution_plan {
            ExecutionPlan::DirectionalBuy { side, .. } => assert_eq!(side, TradeSide::Sell),
            _ => panic!("expected DirectionalBuy"),
        }
    }

    #[test]
    fn test_position_cap() {
        let token_id = U256::from(42u64);
        let cid = B256::ZERO;
        let book = make_book(
            vec![(dec!(0.55), dec!(500))],
            vec![(dec!(0.60), dec!(5000))],
        );
        let market = make_market(cid, token_id);

        let signals = vec![SmartMoneySignal {
            signal_type: SignalType::Entry,
            wallet_address: "0xaaa".to_string(),
            wallet_weight: Decimal::ONE,
            token_id,
            condition_id: cid,
            wallet_size: dec!(10000), // huge wallet
            delta: dec!(10000),
            detected_at: Utc::now(),
        }];

        // Already have 100 shares at 0.60 → max_position_usdc=100, so 100/0.60=166 shares max
        // existing=100, remaining=166-100=66
        let strategy = make_strategy(book, dec!(100), dec!(1000), signals, market);
        let consumed = strategy.consume_signals();
        let aggregated = strategy.aggregate_signals(&consumed);
        let agg = aggregated.get(&token_id).unwrap();

        // target_size = 10000 * 0.10 * 1.0 = 1000 (way above cap)
        assert_eq!(agg.target_size, dec!(1000));

        let opp = strategy.process_entry_signal(agg);
        assert!(opp.is_some());
        let opp = opp.unwrap();
        // Should be capped: min(1000, 66.666, 1666.666) ≈ 66
        // max_shares = 100/0.60 = 166.666, remaining = 166.666-100 = 66.666
        assert!(opp.size <= dec!(67)); // approximately 66.666
        assert!(opp.size > Decimal::ZERO);
    }

    #[test]
    fn test_capital_efficiency_exit() {
        let token_id = U256::from(42u64);
        let cid = B256::ZERO;
        // best_bid = 0.99 ≥ threshold 0.98
        let book = make_book(
            vec![(dec!(0.99), dec!(500))],
            vec![(dec!(1.00), dec!(500))],
        );
        let market = make_market(cid, token_id);

        let config = SmartMoneyConfig {
            capital_efficiency_threshold: dec!(0.98),
            ..Default::default()
        };

        let book_arc = Arc::new(book);
        let markets_arc = Arc::new(RwLock::new(
            HashMap::from([(cid, market)]),
        ));

        let strategy = SmartMoneyStrategy::new(
            config,
            dec!(0.00),
            Box::new(move |_| Some((*book_arc).clone())),
            Box::new(|| dec!(1000)),
            Box::new(|_| Decimal::ZERO),
            // We hold 50 shares at avg_cost 0.60
            Box::new(move || vec![(token_id, dec!(50), dec!(0.60))]),
            Box::new(|| dec!(1000)),
            Arc::new(RwLock::new(vec![])),
            markets_arc,
        );

        let exits = strategy.scan_exits();
        assert_eq!(exits.len(), 1);
        assert_eq!(exits[0].size, dec!(50));
        match &exits[0].execution_plan {
            ExecutionPlan::DirectionalBuy { side, price, .. } => {
                assert_eq!(*side, TradeSide::Sell);
                assert_eq!(*price, dec!(0.99));
            }
            _ => panic!("expected DirectionalBuy"),
        }
    }
}
