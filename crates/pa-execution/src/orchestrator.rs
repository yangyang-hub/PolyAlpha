use alloy::primitives::{B256, U256};
use async_trait::async_trait;
use chrono::Utc;
use rust_decimal::Decimal;
use uuid::Uuid;

use pa_core::Result;
use pa_core::traits::Executor;
use pa_core::types::{
    ExecutionPlan, ExecutionResult, ExecutionStatus, TradeRecord, TradeSide, TradingOpportunity,
    TxType,
};

use crate::clob_executor::ClobExecutor;
use crate::ctf_executor::CtfExecutor;

/// Orchestrates execution: CLOB orders + on-chain CTF operations (redeem).
///
/// Generic over the alloy Provider used for on-chain CTF interactions.
pub struct HybridOrchestrator<P: alloy::providers::Provider + Clone> {
    clob: ClobExecutor,
    #[allow(dead_code)]
    ctf: CtfExecutor<P>,
}

impl<P: alloy::providers::Provider + Clone> HybridOrchestrator<P> {
    pub fn new(clob: ClobExecutor, ctf: CtfExecutor<P>) -> Self {
        Self { clob, ctf }
    }

    /// Execute a directional buy: single CLOB FOK order, no on-chain tx.
    async fn execute_directional_buy(
        &self,
        token_id: U256,
        side: TradeSide,
        price: Decimal,
        size: Decimal,
        condition_id: B256,
        opportunity_id: Uuid,
    ) -> Result<ExecutionResult> {
        tracing::info!(
            id = %opportunity_id,
            token_id = %token_id,
            side = ?side,
            price = %price,
            size = %size,
            "Executing DirectionalBuy"
        );

        let order_result = match side {
            TradeSide::Buy => self.clob.buy_fok(token_id, price, size).await,
            TradeSide::Sell => self.clob.sell_fok(token_id, price, size).await,
        };

        let order = order_result.map_err(|e| pa_core::Error::OrderFailed(e.to_string()))?;

        let trades = vec![TradeRecord {
            id: Uuid::now_v7(),
            order_id: Some(order.order_id.clone()),
            token_id,
            condition_id,
            side,
            price: order.avg_price,
            size: order.posted_size,
            filled_size: order.filled_size,
            fee: Decimal::ZERO,
            tx_type: TxType::ClobOrder,
            tx_hash: None,
        }];

        let status = match order.status {
            crate::clob_executor::OrderFillStatus::Filled => ExecutionStatus::Success,
            crate::clob_executor::OrderFillStatus::PartialFill => ExecutionStatus::PartialFill,
            crate::clob_executor::OrderFillStatus::NoFill
            | crate::clob_executor::OrderFillStatus::Rejected => ExecutionStatus::NoFill,
        };

        Ok(ExecutionResult {
            opportunity_id,
            strategy_type: opp_strategy_type_placeholder(),
            status,
            trades,
            realized_profit: Decimal::ZERO, // Unknown until market resolution
            total_fees: Decimal::ZERO,
            total_gas: Decimal::ZERO, // CLOB-only
            executed_at: Utc::now(),
        })
    }
}

/// Placeholder — the caller overwrites strategy_type after execute().
fn opp_strategy_type_placeholder() -> pa_core::types::StrategyType {
    pa_core::types::StrategyType::Weather
}

#[async_trait]
impl<P: alloy::providers::Provider + Clone + Send + Sync> Executor for HybridOrchestrator<P> {
    async fn execute(&self, opp: &TradingOpportunity) -> Result<ExecutionResult> {
        let mut result = match &opp.execution_plan {
            ExecutionPlan::DirectionalBuy {
                token_id,
                side,
                price,
                size,
                condition_id,
            } => {
                self.execute_directional_buy(*token_id, *side, *price, *size, *condition_id, opp.id)
                    .await
            }
        }?;
        result.strategy_type = opp.strategy_type;
        Ok(result)
    }

    async fn cancel_all(&self) -> Result<()> {
        self.clob
            .cancel_all()
            .await
            .map_err(|e| pa_core::Error::Execution(e.to_string()))
    }

    async fn get_balance(&self) -> Result<rust_decimal::Decimal> {
        self.clob
            .get_balance()
            .await
            .map_err(|e| pa_core::Error::Execution(e.to_string()))
    }
}
