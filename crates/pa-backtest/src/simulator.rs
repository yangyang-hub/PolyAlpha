use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use chrono::Utc;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use pa_core::Result;
use pa_core::traits::Executor;
use pa_core::types::{
    ExecutionPlan, ExecutionResult, ExecutionStatus, OrderBook, TradeRecord, TradeSide,
    TradingOpportunity, TxType,
};
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
                strategy_type: pa_core::types::StrategyType::Weather, // placeholder, overwritten by caller
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

        let fee = self
            .profit_calc
            .capped_fee(actual_price, self.config.fee_rate_bps);
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
            strategy_type: pa_core::types::StrategyType::Weather, // placeholder, overwritten by caller
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
    async fn execute(&self, opp: &TradingOpportunity) -> Result<ExecutionResult> {
        let mut result = match &opp.execution_plan {
            ExecutionPlan::DirectionalBuy {
                token_id,
                side,
                price,
                size,
                condition_id,
            } => self.simulate_directional_buy(
                *token_id,
                *side,
                *price,
                *size,
                *condition_id,
                opp.id,
            ),
        };
        result.strategy_type = opp.strategy_type;
        Ok(result)
    }

    async fn cancel_all(&self) -> Result<()> {
        Ok(()) // No-op in simulation
    }
}
