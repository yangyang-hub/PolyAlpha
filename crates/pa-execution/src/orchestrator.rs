use alloy::primitives::U256;
use alloy::providers::Provider;
use async_trait::async_trait;
use chrono::Utc;
use rust_decimal::Decimal;
use uuid::Uuid;

use pa_core::traits::Executor;
use pa_core::types::{
    ArbitrageOpportunity, ExecutionPlan, ExecutionResult, ExecutionStatus,
    TradeRecord, TradeSide, TxType,
};
use pa_core::Result;

use crate::clob_executor::ClobExecutor;
use crate::ctf_executor::CtfExecutor;

/// Orchestrates hybrid execution: CLOB orders + on-chain CTF operations.
///
/// Generic over the alloy Provider used for on-chain CTF interactions.
pub struct HybridOrchestrator<P: Provider + Clone> {
    clob: ClobExecutor,
    ctf: CtfExecutor<P>,
}

impl<P: Provider + Clone> HybridOrchestrator<P> {
    pub fn new(clob: ClobExecutor, ctf: CtfExecutor<P>) -> Self {
        Self { clob, ctf }
    }

    /// Execute a BuyAndMerge strategy:
    /// 1. Concurrently buy YES + NO via CLOB (FOK)
    /// 2. Merge matched amounts on-chain
    async fn execute_buy_and_merge(
        &self,
        yes_token_id: U256,
        no_token_id: U256,
        yes_price: Decimal,
        no_price: Decimal,
        size: Decimal,
        condition_id: alloy::primitives::B256,
        opportunity_id: Uuid,
    ) -> Result<ExecutionResult> {
        tracing::info!(
            id = %opportunity_id,
            yes_price = %yes_price,
            no_price = %no_price,
            size = %size,
            "Executing BuyAndMerge"
        );

        // Step 1: Concurrent FOK buy orders
        let (yes_result, no_result) = tokio::join!(
            self.clob.buy_fok(yes_token_id, yes_price, size),
            self.clob.buy_fok(no_token_id, no_price, size),
        );

        let yes_order = yes_result.map_err(|e| pa_core::Error::OrderFailed(e.to_string()))?;
        let no_order = no_result.map_err(|e| pa_core::Error::OrderFailed(e.to_string()))?;

        let mut trades = vec![];

        trades.push(TradeRecord {
            id: Uuid::now_v7(),
            token_id: yes_token_id,
            side: TradeSide::Buy,
            price: yes_price,
            size,
            filled_size: yes_order.filled_size,
            fee: Decimal::ZERO, // TODO: calculate from fill
            tx_type: TxType::ClobOrder,
            tx_hash: None,
        });

        trades.push(TradeRecord {
            id: Uuid::now_v7(),
            token_id: no_token_id,
            side: TradeSide::Buy,
            price: no_price,
            size,
            filled_size: no_order.filled_size,
            fee: Decimal::ZERO,
            tx_type: TxType::ClobOrder,
            tx_hash: None,
        });

        // Step 2: Determine merge amount
        let merge_amount = yes_order.filled_size.min(no_order.filled_size);

        if merge_amount <= Decimal::ZERO {
            return Ok(ExecutionResult {
                opportunity_id,
                status: ExecutionStatus::NoFill,
                trades,
                realized_profit: Decimal::ZERO,
                total_fees: Decimal::ZERO,
                total_gas: Decimal::ZERO,
                executed_at: Utc::now(),
            });
        }

        // Step 3: On-chain merge
        // Convert Decimal to U256 (USDC has 6 decimals)
        let merge_u256 = decimal_to_usdc_u256(merge_amount);

        let merge_result = self
            .ctf
            .merge(condition_id, merge_u256)
            .await
            .map_err(|e| pa_core::Error::OnChainTxFailed(e.to_string()))?;

        trades.push(TradeRecord {
            id: Uuid::now_v7(),
            token_id: U256::ZERO, // merge is not token-specific
            side: TradeSide::Buy,
            price: Decimal::ONE,
            size: merge_amount,
            filled_size: merge_amount,
            fee: Decimal::ZERO,
            tx_type: TxType::CtfMerge,
            tx_hash: Some(merge_result.tx_hash),
        });

        // Calculate profit
        let cost = (yes_price + no_price) * merge_amount;
        let revenue = merge_amount; // $1.00 per merged unit
        let realized_profit = revenue - cost; // TODO: subtract fees

        let status = if merge_amount == size {
            ExecutionStatus::Success
        } else {
            ExecutionStatus::PartialFill
        };

        Ok(ExecutionResult {
            opportunity_id,
            status,
            trades,
            realized_profit,
            total_fees: Decimal::ZERO, // TODO
            total_gas: Decimal::ZERO,  // TODO
            executed_at: Utc::now(),
        })
    }

    /// Execute a SplitAndSell strategy:
    /// 1. Split USDC into YES + NO tokens on-chain
    /// 2. Concurrently sell YES + NO via CLOB (FOK)
    async fn execute_split_and_sell(
        &self,
        yes_token_id: U256,
        no_token_id: U256,
        yes_price: Decimal,
        no_price: Decimal,
        size: Decimal,
        condition_id: alloy::primitives::B256,
        opportunity_id: Uuid,
    ) -> Result<ExecutionResult> {
        tracing::info!(
            id = %opportunity_id,
            yes_price = %yes_price,
            no_price = %no_price,
            size = %size,
            "Executing SplitAndSell"
        );

        // Step 1: On-chain split
        let split_u256 = decimal_to_usdc_u256(size);

        let split_result = self
            .ctf
            .split(condition_id, split_u256)
            .await
            .map_err(|e| pa_core::Error::OnChainTxFailed(e.to_string()))?;

        let mut trades = vec![];

        trades.push(TradeRecord {
            id: Uuid::now_v7(),
            token_id: U256::ZERO,
            side: TradeSide::Buy,
            price: Decimal::ONE,
            size,
            filled_size: size,
            fee: Decimal::ZERO,
            tx_type: TxType::CtfSplit,
            tx_hash: Some(split_result.tx_hash),
        });

        // Step 2: Concurrent FOK sell orders
        let (yes_result, no_result) = tokio::join!(
            self.clob.sell_fok(yes_token_id, yes_price, size),
            self.clob.sell_fok(no_token_id, no_price, size),
        );

        let yes_order = yes_result.map_err(|e| pa_core::Error::OrderFailed(e.to_string()))?;
        let no_order = no_result.map_err(|e| pa_core::Error::OrderFailed(e.to_string()))?;

        trades.push(TradeRecord {
            id: Uuid::now_v7(),
            token_id: yes_token_id,
            side: TradeSide::Sell,
            price: yes_price,
            size,
            filled_size: yes_order.filled_size,
            fee: Decimal::ZERO,
            tx_type: TxType::ClobOrder,
            tx_hash: None,
        });

        trades.push(TradeRecord {
            id: Uuid::now_v7(),
            token_id: no_token_id,
            side: TradeSide::Sell,
            price: no_price,
            size,
            filled_size: no_order.filled_size,
            fee: Decimal::ZERO,
            tx_type: TxType::ClobOrder,
            tx_hash: None,
        });

        // Calculate profit
        let sold_amount = yes_order.filled_size.min(no_order.filled_size);
        let revenue = (yes_price + no_price) * sold_amount;
        let cost = size; // $1.00 per split unit
        let realized_profit = revenue - cost; // TODO: subtract fees

        let status = if sold_amount == size {
            ExecutionStatus::Success
        } else if sold_amount > Decimal::ZERO {
            ExecutionStatus::PartialFill
        } else {
            ExecutionStatus::NoFill
        };

        Ok(ExecutionResult {
            opportunity_id,
            status,
            trades,
            realized_profit,
            total_fees: Decimal::ZERO,
            total_gas: Decimal::ZERO,
            executed_at: Utc::now(),
        })
    }
}

#[async_trait]
impl<P: Provider + Clone + Send + Sync> Executor for HybridOrchestrator<P> {
    async fn execute(&self, opp: &ArbitrageOpportunity) -> Result<ExecutionResult> {
        match &opp.execution_plan {
            ExecutionPlan::BuyAndMerge {
                yes_token_id,
                no_token_id,
                yes_price,
                no_price,
                merge_amount,
                condition_id,
            } => {
                self.execute_buy_and_merge(
                    *yes_token_id,
                    *no_token_id,
                    *yes_price,
                    *no_price,
                    *merge_amount,
                    *condition_id,
                    opp.id,
                )
                .await
            }
            ExecutionPlan::SplitAndSell {
                yes_token_id,
                no_token_id,
                yes_price,
                no_price,
                split_amount,
                condition_id,
            } => {
                self.execute_split_and_sell(
                    *yes_token_id,
                    *no_token_id,
                    *yes_price,
                    *no_price,
                    *split_amount,
                    *condition_id,
                    opp.id,
                )
                .await
            }
            ExecutionPlan::NegRiskArbitrage { .. } => {
                // TODO: Phase 2
                Err(pa_core::Error::Execution("NegRisk not yet implemented".into()))
            }
        }
    }

    async fn cancel_all(&self) -> Result<()> {
        self.clob
            .cancel_all()
            .await
            .map_err(|e| pa_core::Error::Execution(e.to_string()))
    }
}

/// Convert a Decimal amount to U256 with 6 decimal places (USDC).
fn decimal_to_usdc_u256(amount: Decimal) -> U256 {
    use rust_decimal::prelude::ToPrimitive;
    let scaled = amount * Decimal::from(1_000_000u64);
    let int_val = scaled.trunc().to_u64().unwrap_or(0);
    U256::from(int_val)
}
