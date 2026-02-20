use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use chrono::Utc;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use pa_core::traits::Executor;
use pa_core::types::{
    ArbitrageOpportunity, CrossMarketLeg, CrossMarketOp, ExecutionPlan, ExecutionResult,
    ExecutionStatus, OrderBook, StrategyType, TradeRecord, TradeSide, TxType,
};
use pa_core::Result;
use pa_strategy::profitability::ProfitCalculator;

use alloy::primitives::{B256, U256};

/// Configuration for simulated trade execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimulatorConfig {
    /// Slippage in basis points applied to execution price (default: 10 = 0.1%).
    pub slippage_bps: u32,
    /// Fill ratio for FOK orders (default: 1.0 = 100%).
    pub fill_ratio: Decimal,
    /// Simulated gas cost per on-chain transaction in USD.
    pub gas_cost_usd: Decimal,
    /// Default taker fee rate in basis points.
    pub fee_rate_bps: u32,
}

impl Default for SimulatorConfig {
    fn default() -> Self {
        Self {
            slippage_bps: 10,
            fill_ratio: Decimal::ONE,
            gas_cost_usd: dec!(0.01),
            fee_rate_bps: 200,
        }
    }
}

/// Simulated executor for backtesting. Implements the `Executor` trait
/// without sending real orders — it models fills based on the current order book.
pub struct TradeSimulator {
    config: SimulatorConfig,
    books: Arc<RwLock<HashMap<U256, OrderBook>>>,
    profit_calc: ProfitCalculator,
}

impl TradeSimulator {
    pub fn new(config: SimulatorConfig, books: Arc<RwLock<HashMap<U256, OrderBook>>>) -> Self {
        let profit_calc = ProfitCalculator::new(config.gas_cost_usd);
        Self {
            config,
            books,
            profit_calc,
        }
    }

    /// Apply slippage to a buy price (price increases).
    fn slipped_buy_price(&self, price: Decimal) -> Decimal {
        let slip = Decimal::from(self.config.slippage_bps) / dec!(10000);
        price * (Decimal::ONE + slip)
    }

    /// Apply slippage to a sell price (price decreases).
    fn slipped_sell_price(&self, price: Decimal) -> Decimal {
        let slip = Decimal::from(self.config.slippage_bps) / dec!(10000);
        price * (Decimal::ONE - slip)
    }

    /// Simulate a BuyAndMerge execution.
    fn simulate_buy_and_merge(
        &self,
        yes_token_id: U256,
        no_token_id: U256,
        yes_price: Decimal,
        no_price: Decimal,
        merge_amount: Decimal,
        condition_id: B256,
        opportunity_id: Uuid,
    ) -> ExecutionResult {
        let books = self.books.read().unwrap();

        // Check available liquidity from order books
        let yes_available = books
            .get(&yes_token_id)
            .and_then(|b| b.best_ask())
            .map(|a| a.size)
            .unwrap_or(Decimal::ZERO);

        let no_available = books
            .get(&no_token_id)
            .and_then(|b| b.best_ask())
            .map(|a| a.size)
            .unwrap_or(Decimal::ZERO);

        let filled = merge_amount
            .min(yes_available)
            .min(no_available)
            * self.config.fill_ratio;

        if filled <= Decimal::ZERO {
            return ExecutionResult {
                opportunity_id,
                strategy_type: StrategyType::YesNoMerge,
                status: ExecutionStatus::NoFill,
                trades: vec![],
                realized_profit: Decimal::ZERO,
                total_fees: Decimal::ZERO,
                total_gas: Decimal::ZERO,
                executed_at: Utc::now(),
            };
        }

        let actual_yes = self.slipped_buy_price(yes_price);
        let actual_no = self.slipped_buy_price(no_price);

        let yes_fee = self.profit_calc.capped_fee(actual_yes, self.config.fee_rate_bps);
        let no_fee = self.profit_calc.capped_fee(actual_no, self.config.fee_rate_bps);
        let total_fees = (yes_fee + no_fee) * filled;
        let total_gas = self.config.gas_cost_usd;

        let cost = (actual_yes + actual_no) * filled;
        let revenue = filled; // $1.00 per merged unit
        let realized_profit = revenue - cost - total_fees - total_gas;

        let trades = vec![
            TradeRecord {
                id: Uuid::now_v7(),
                token_id: yes_token_id,
                condition_id,
                side: TradeSide::Buy,
                price: actual_yes,
                size: merge_amount,
                filled_size: filled,
                fee: yes_fee * filled,
                tx_type: TxType::ClobOrder,
                tx_hash: None,
            },
            TradeRecord {
                id: Uuid::now_v7(),
                token_id: no_token_id,
                condition_id,
                side: TradeSide::Buy,
                price: actual_no,
                size: merge_amount,
                filled_size: filled,
                fee: no_fee * filled,
                tx_type: TxType::ClobOrder,
                tx_hash: None,
            },
        ];

        let status = if filled == merge_amount {
            ExecutionStatus::Success
        } else {
            ExecutionStatus::PartialFill
        };

        ExecutionResult {
            opportunity_id,
            strategy_type: StrategyType::YesNoMerge,
            status,
            trades,
            realized_profit,
            total_fees,
            total_gas,
            executed_at: Utc::now(),
        }
    }

    /// Simulate a SplitAndSell execution.
    fn simulate_split_and_sell(
        &self,
        yes_token_id: U256,
        no_token_id: U256,
        yes_price: Decimal,
        no_price: Decimal,
        split_amount: Decimal,
        condition_id: B256,
        opportunity_id: Uuid,
    ) -> ExecutionResult {
        let books = self.books.read().unwrap();

        // Check available bid liquidity
        let yes_available = books
            .get(&yes_token_id)
            .and_then(|b| b.best_bid())
            .map(|b| b.size)
            .unwrap_or(Decimal::ZERO);

        let no_available = books
            .get(&no_token_id)
            .and_then(|b| b.best_bid())
            .map(|b| b.size)
            .unwrap_or(Decimal::ZERO);

        let filled = split_amount
            .min(yes_available)
            .min(no_available)
            * self.config.fill_ratio;

        if filled <= Decimal::ZERO {
            return ExecutionResult {
                opportunity_id,
                strategy_type: StrategyType::YesNoMerge,
                status: ExecutionStatus::NoFill,
                trades: vec![],
                realized_profit: Decimal::ZERO,
                total_fees: Decimal::ZERO,
                total_gas: Decimal::ZERO,
                executed_at: Utc::now(),
            };
        }

        let actual_yes = self.slipped_sell_price(yes_price);
        let actual_no = self.slipped_sell_price(no_price);

        let yes_fee = self.profit_calc.capped_fee(actual_yes, self.config.fee_rate_bps);
        let no_fee = self.profit_calc.capped_fee(actual_no, self.config.fee_rate_bps);
        let total_fees = (yes_fee + no_fee) * filled;
        let total_gas = self.config.gas_cost_usd;

        let revenue = (actual_yes + actual_no) * filled;
        let cost = split_amount; // $1.00 per split unit
        let realized_profit = revenue - cost - total_fees - total_gas;

        let trades = vec![
            TradeRecord {
                id: Uuid::now_v7(),
                token_id: yes_token_id,
                condition_id,
                side: TradeSide::Sell,
                price: actual_yes,
                size: split_amount,
                filled_size: filled,
                fee: yes_fee * filled,
                tx_type: TxType::ClobOrder,
                tx_hash: None,
            },
            TradeRecord {
                id: Uuid::now_v7(),
                token_id: no_token_id,
                condition_id,
                side: TradeSide::Sell,
                price: actual_no,
                size: split_amount,
                filled_size: filled,
                fee: no_fee * filled,
                tx_type: TxType::ClobOrder,
                tx_hash: None,
            },
        ];

        let status = if filled == split_amount {
            ExecutionStatus::Success
        } else if filled > Decimal::ZERO {
            ExecutionStatus::PartialFill
        } else {
            ExecutionStatus::NoFill
        };

        ExecutionResult {
            opportunity_id,
            strategy_type: StrategyType::YesNoMerge,
            status,
            trades,
            realized_profit,
            total_fees,
            total_gas,
            executed_at: Utc::now(),
        }
    }

    /// Simulate a NegRisk multi-outcome buy execution.
    fn simulate_neg_risk(
        &self,
        legs: &[pa_core::types::NegRiskLeg],
        amount: Decimal,
        opportunity_id: Uuid,
    ) -> ExecutionResult {
        let books = self.books.read().unwrap();

        // Check available ask liquidity for each leg
        let mut min_filled = amount;
        for leg in legs {
            let available = books
                .get(&leg.token_id)
                .and_then(|b| b.best_ask())
                .map(|a| a.size)
                .unwrap_or(Decimal::ZERO);
            min_filled = min_filled.min(available);
        }
        min_filled *= self.config.fill_ratio;

        if min_filled <= Decimal::ZERO {
            return ExecutionResult {
                opportunity_id,
                strategy_type: StrategyType::YesNoMerge,
                status: ExecutionStatus::NoFill,
                trades: vec![],
                realized_profit: Decimal::ZERO,
                total_fees: Decimal::ZERO,
                total_gas: Decimal::ZERO,
                executed_at: Utc::now(),
            };
        }

        let mut trades = Vec::with_capacity(legs.len());
        let mut total_cost = Decimal::ZERO;
        let mut total_fees = Decimal::ZERO;

        for leg in legs {
            let actual_price = self.slipped_buy_price(leg.price);
            let fee = self.profit_calc.capped_fee(actual_price, self.config.fee_rate_bps);
            total_cost += actual_price * min_filled;
            total_fees += fee * min_filled;

            trades.push(TradeRecord {
                id: Uuid::now_v7(),
                token_id: leg.token_id,
                condition_id: leg.condition_id,
                side: TradeSide::Buy,
                price: actual_price,
                size: amount,
                filled_size: min_filled,
                fee: fee * min_filled,
                tx_type: TxType::ClobOrder,
                tx_hash: None,
            });
        }

        let total_gas = self.config.gas_cost_usd;
        let revenue = min_filled; // $1.00 per complete set
        let realized_profit = revenue - total_cost - total_fees - total_gas;

        let status = if min_filled == amount {
            ExecutionStatus::Success
        } else {
            ExecutionStatus::PartialFill
        };

        ExecutionResult {
            opportunity_id,
            strategy_type: StrategyType::YesNoMerge,
            status,
            trades,
            realized_profit,
            total_fees,
            total_gas,
            executed_at: Utc::now(),
        }
    }

    /// Simulate a cross-market arbitrage by executing both legs.
    fn simulate_cross_market(
        &self,
        leg_a: &CrossMarketLeg,
        leg_b: &CrossMarketLeg,
        amount: Decimal,
        opportunity_id: Uuid,
    ) -> ExecutionResult {
        let exec_a = match leg_a.operation {
            CrossMarketOp::BuyAndMerge => self.simulate_buy_and_merge(
                leg_a.yes_token_id,
                leg_a.no_token_id,
                leg_a.yes_price,
                leg_a.no_price,
                amount,
                leg_a.condition_id,
                opportunity_id,
            ),
            CrossMarketOp::SplitAndSell => self.simulate_split_and_sell(
                leg_a.yes_token_id,
                leg_a.no_token_id,
                leg_a.yes_price,
                leg_a.no_price,
                amount,
                leg_a.condition_id,
                opportunity_id,
            ),
        };

        let exec_b = match leg_b.operation {
            CrossMarketOp::BuyAndMerge => self.simulate_buy_and_merge(
                leg_b.yes_token_id,
                leg_b.no_token_id,
                leg_b.yes_price,
                leg_b.no_price,
                amount,
                leg_b.condition_id,
                opportunity_id,
            ),
            CrossMarketOp::SplitAndSell => self.simulate_split_and_sell(
                leg_b.yes_token_id,
                leg_b.no_token_id,
                leg_b.yes_price,
                leg_b.no_price,
                amount,
                leg_b.condition_id,
                opportunity_id,
            ),
        };

        let mut trades = exec_a.trades;
        trades.extend(exec_b.trades);

        let realized_profit = exec_a.realized_profit + exec_b.realized_profit;
        let total_fees = exec_a.total_fees + exec_b.total_fees;
        let total_gas = exec_a.total_gas + exec_b.total_gas;

        let status = match (exec_a.status, exec_b.status) {
            (ExecutionStatus::Success, ExecutionStatus::Success) => ExecutionStatus::Success,
            (ExecutionStatus::NoFill, _) | (_, ExecutionStatus::NoFill) => {
                ExecutionStatus::NoFill
            }
            _ => ExecutionStatus::PartialFill,
        };

        ExecutionResult {
            opportunity_id,
            strategy_type: StrategyType::YesNoMerge,
            status,
            trades,
            realized_profit,
            total_fees,
            total_gas,
            executed_at: Utc::now(),
        }
    }

    /// Simulate a directional buy: single CLOB order, no on-chain tx.
    fn simulate_directional_buy(
        &self,
        token_id: U256,
        side: TradeSide,
        price: Decimal,
        size: Decimal,
        condition_id: B256,
        opportunity_id: Uuid,
    ) -> ExecutionResult {
        let books = self.books.read().unwrap();

        // Check available liquidity
        let available = match side {
            TradeSide::Buy => books
                .get(&token_id)
                .and_then(|b| b.best_ask())
                .map(|a| a.size)
                .unwrap_or(Decimal::ZERO),
            TradeSide::Sell => books
                .get(&token_id)
                .and_then(|b| b.best_bid())
                .map(|b| b.size)
                .unwrap_or(Decimal::ZERO),
        };

        let filled = size.min(available) * self.config.fill_ratio;

        if filled <= Decimal::ZERO {
            return ExecutionResult {
                opportunity_id,
                strategy_type: StrategyType::YesNoMerge,
                status: ExecutionStatus::NoFill,
                trades: vec![],
                realized_profit: Decimal::ZERO,
                total_fees: Decimal::ZERO,
                total_gas: Decimal::ZERO,
                executed_at: Utc::now(),
            };
        }

        let actual_price = match side {
            TradeSide::Buy => self.slipped_buy_price(price),
            TradeSide::Sell => self.slipped_sell_price(price),
        };

        let fee = self.profit_calc.capped_fee(actual_price, self.config.fee_rate_bps);
        let total_fees = fee * filled;

        // Directional profit is based on model edge, not actual resolution.
        // We record cost basis only; true P&L depends on market outcome.
        let realized_profit = Decimal::ZERO;

        let trades = vec![TradeRecord {
            id: Uuid::now_v7(),
            token_id,
            condition_id,
            side,
            price: actual_price,
            size,
            filled_size: filled,
            fee: total_fees,
            tx_type: TxType::ClobOrder,
            tx_hash: None,
        }];

        let status = if filled == size {
            ExecutionStatus::Success
        } else {
            ExecutionStatus::PartialFill
        };

        ExecutionResult {
            opportunity_id,
            strategy_type: StrategyType::YesNoMerge,
            status,
            trades,
            realized_profit,
            total_fees,
            total_gas: Decimal::ZERO, // CLOB-only
            executed_at: Utc::now(),
        }
    }
}

#[async_trait]
impl Executor for TradeSimulator {
    async fn execute(&self, opp: &ArbitrageOpportunity) -> Result<ExecutionResult> {
        let mut result = match &opp.execution_plan {
            ExecutionPlan::BuyAndMerge {
                yes_token_id,
                no_token_id,
                yes_price,
                no_price,
                merge_amount,
                condition_id,
            } => self.simulate_buy_and_merge(
                *yes_token_id,
                *no_token_id,
                *yes_price,
                *no_price,
                *merge_amount,
                *condition_id,
                opp.id,
            ),
            ExecutionPlan::SplitAndSell {
                yes_token_id,
                no_token_id,
                yes_price,
                no_price,
                split_amount,
                condition_id,
            } => self.simulate_split_and_sell(
                *yes_token_id,
                *no_token_id,
                *yes_price,
                *no_price,
                *split_amount,
                *condition_id,
                opp.id,
            ),
            ExecutionPlan::NegRiskArbitrage { legs, amount, .. } => {
                self.simulate_neg_risk(legs, *amount, opp.id)
            }
            ExecutionPlan::CrossMarket {
                leg_a,
                leg_b,
                amount,
                ..
            } => self.simulate_cross_market(leg_a, leg_b, *amount, opp.id),
            ExecutionPlan::DirectionalBuy {
                token_id,
                side,
                price,
                size,
                condition_id,
            } => self.simulate_directional_buy(*token_id, *side, *price, *size, *condition_id, opp.id),
        };
        result.strategy_type = opp.strategy_type;
        Ok(result)
    }

    async fn cancel_all(&self) -> Result<()> {
        Ok(()) // No-op in simulation
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::B256;
    use pa_core::types::{Outcome, PriceLevel, StrategyType};
    use rust_decimal_macros::dec;

    fn make_orderbook(token_id: U256, best_bid: Decimal, best_ask: Decimal) -> OrderBook {
        OrderBook {
            token_id,
            bids: vec![PriceLevel {
                price: best_bid,
                size: dec!(1000),
            }],
            asks: vec![PriceLevel {
                price: best_ask,
                size: dec!(1000),
            }],
            timestamp: Utc::now(),
        }
    }

    fn make_simulator(books: HashMap<U256, OrderBook>) -> TradeSimulator {
        TradeSimulator::new(
            SimulatorConfig {
                slippage_bps: 0, // no slippage for deterministic tests
                fill_ratio: Decimal::ONE,
                gas_cost_usd: dec!(0.01),
                fee_rate_bps: 200,
            },
            Arc::new(RwLock::new(books)),
        )
    }

    #[tokio::test]
    async fn test_simulate_buy_and_merge_profitable() {
        let yes_id = U256::from(1);
        let no_id = U256::from(2);
        let mut books = HashMap::new();
        books.insert(yes_id, make_orderbook(yes_id, dec!(0.46), dec!(0.47)));
        books.insert(no_id, make_orderbook(no_id, dec!(0.49), dec!(0.50)));

        let sim = make_simulator(books);

        let opp = ArbitrageOpportunity {
            id: Uuid::now_v7(),
            strategy_type: StrategyType::YesNoMerge,
            condition_id: B256::ZERO,
            question: "Test".into(),
            spread: dec!(0.03),
            estimated_profit: dec!(2.0),
            size: dec!(100),
            detected_at: Utc::now(),
            execution_plan: ExecutionPlan::BuyAndMerge {
                yes_token_id: yes_id,
                no_token_id: no_id,
                yes_price: dec!(0.47),
                no_price: dec!(0.50),
                merge_amount: dec!(100),
                condition_id: B256::ZERO,
            },
        };

        let result = sim.execute(&opp).await.unwrap();
        assert_eq!(result.status, ExecutionStatus::Success);
        // Revenue = 100, Cost = (0.47+0.50)*100 = 97 => gross = 3.00
        // Fees + gas eat into profit, but should still be positive
        assert!(
            result.realized_profit > Decimal::ZERO,
            "Expected positive profit, got {}",
            result.realized_profit
        );
        assert_eq!(result.trades.len(), 2);
    }

    #[tokio::test]
    async fn test_simulate_no_liquidity() {
        let yes_id = U256::from(1);
        let no_id = U256::from(2);
        let mut books = HashMap::new();
        // YES has liquidity, NO has empty book
        books.insert(yes_id, make_orderbook(yes_id, dec!(0.46), dec!(0.47)));
        books.insert(
            no_id,
            OrderBook {
                token_id: no_id,
                bids: vec![],
                asks: vec![],
                timestamp: Utc::now(),
            },
        );

        let sim = make_simulator(books);

        let opp = ArbitrageOpportunity {
            id: Uuid::now_v7(),
            strategy_type: StrategyType::YesNoMerge,
            condition_id: B256::ZERO,
            question: "Test".into(),
            spread: dec!(0.03),
            estimated_profit: dec!(2.0),
            size: dec!(100),
            detected_at: Utc::now(),
            execution_plan: ExecutionPlan::BuyAndMerge {
                yes_token_id: yes_id,
                no_token_id: no_id,
                yes_price: dec!(0.47),
                no_price: dec!(0.50),
                merge_amount: dec!(100),
                condition_id: B256::ZERO,
            },
        };

        let result = sim.execute(&opp).await.unwrap();
        assert_eq!(result.status, ExecutionStatus::NoFill);
    }

    #[tokio::test]
    async fn test_simulate_neg_risk() {
        let t1 = U256::from(10);
        let t2 = U256::from(20);
        let t3 = U256::from(30);

        let mut books = HashMap::new();
        books.insert(t1, make_orderbook(t1, dec!(0.28), dec!(0.30)));
        books.insert(t2, make_orderbook(t2, dec!(0.23), dec!(0.25)));
        books.insert(t3, make_orderbook(t3, dec!(0.18), dec!(0.20)));

        let sim = make_simulator(books);

        let legs = vec![
            pa_core::types::NegRiskLeg {
                token_id: t1,
                condition_id: B256::ZERO,
                outcome: Outcome::Yes,
                side: TradeSide::Buy,
                price: dec!(0.30),
                size: dec!(100),
            },
            pa_core::types::NegRiskLeg {
                token_id: t2,
                condition_id: B256::ZERO,
                outcome: Outcome::Yes,
                side: TradeSide::Buy,
                price: dec!(0.25),
                size: dec!(100),
            },
            pa_core::types::NegRiskLeg {
                token_id: t3,
                condition_id: B256::ZERO,
                outcome: Outcome::Yes,
                side: TradeSide::Buy,
                price: dec!(0.20),
                size: dec!(100),
            },
        ];

        let opp = ArbitrageOpportunity {
            id: Uuid::now_v7(),
            strategy_type: StrategyType::NegRiskConvert,
            condition_id: B256::ZERO,
            question: "NegRisk test".into(),
            spread: dec!(0.25),
            estimated_profit: dec!(20.0),
            size: dec!(100),
            detected_at: Utc::now(),
            execution_plan: ExecutionPlan::NegRiskArbitrage {
                neg_risk_market_id: B256::ZERO,
                legs,
                amount: dec!(100),
            },
        };

        let result = sim.execute(&opp).await.unwrap();
        assert_eq!(result.status, ExecutionStatus::Success);
        // sum(asks) = 0.75, revenue per unit = 1.00 => gross spread = 0.25
        assert!(
            result.realized_profit > Decimal::ZERO,
            "Expected positive profit, got {}",
            result.realized_profit
        );
        assert_eq!(result.trades.len(), 3);
    }
}
