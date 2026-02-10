use alloy::primitives::{B256, U256};
use alloy::providers::Provider;
use async_trait::async_trait;
use chrono::Utc;
use rust_decimal::Decimal;
use uuid::Uuid;

use pa_core::traits::Executor;
use pa_core::types::{
    ArbitrageOpportunity, CrossMarketLeg, CrossMarketOp, ExecutionPlan, ExecutionResult,
    ExecutionStatus, NegRiskLeg, TradeRecord, TradeSide, TxType,
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

    /// Execute a cross-market arbitrage by running both legs concurrently.
    /// Each leg delegates to either BuyAndMerge or SplitAndSell on its respective market.
    async fn execute_cross_market(
        &self,
        leg_a: &CrossMarketLeg,
        leg_b: &CrossMarketLeg,
        amount: Decimal,
        opportunity_id: Uuid,
    ) -> Result<ExecutionResult> {
        tracing::info!(
            id = %opportunity_id,
            op_a = ?leg_a.operation,
            op_b = ?leg_b.operation,
            amount = %amount,
            "Executing CrossMarket arbitrage"
        );

        let (result_a, result_b) = tokio::join!(
            self.execute_single_leg(leg_a, amount, opportunity_id),
            self.execute_single_leg(leg_b, amount, opportunity_id),
        );

        let exec_a = result_a?;
        let exec_b = result_b?;

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

        Ok(ExecutionResult {
            opportunity_id,
            status,
            trades,
            realized_profit,
            total_fees,
            total_gas,
            executed_at: Utc::now(),
        })
    }

    /// Execute a single cross-market leg.
    async fn execute_single_leg(
        &self,
        leg: &CrossMarketLeg,
        amount: Decimal,
        opportunity_id: Uuid,
    ) -> Result<ExecutionResult> {
        match leg.operation {
            CrossMarketOp::BuyAndMerge => {
                self.execute_buy_and_merge(
                    leg.yes_token_id,
                    leg.no_token_id,
                    leg.yes_price,
                    leg.no_price,
                    amount,
                    leg.condition_id,
                    opportunity_id,
                )
                .await
            }
            CrossMarketOp::SplitAndSell => {
                self.execute_split_and_sell(
                    leg.yes_token_id,
                    leg.no_token_id,
                    leg.yes_price,
                    leg.no_price,
                    amount,
                    leg.condition_id,
                    opportunity_id,
                )
                .await
            }
        }
    }

    /// Execute a NegRisk multi-outcome arbitrage:
    /// 1. Concurrently buy YES tokens for all outcomes via CLOB (FOK)
    /// 2. The "complete set" of all YES tokens = $1.00
    ///
    /// Note: NegRisk markets use the NegRiskExchange for CLOB order signing,
    /// which the SDK handles automatically via the `neg_risk` flag per token.
    /// On-chain merge is not needed because the CLOB settlement handles it.
    async fn execute_neg_risk(
        &self,
        legs: &[NegRiskLeg],
        amount: Decimal,
        _neg_risk_market_id: B256,
        opportunity_id: Uuid,
    ) -> Result<ExecutionResult> {
        tracing::info!(
            id = %opportunity_id,
            legs = legs.len(),
            amount = %amount,
            "Executing NegRisk arbitrage"
        );

        // Step 1: Concurrently buy all YES tokens via CLOB FOK
        let buy_futures: Vec<_> = legs
            .iter()
            .map(|leg| self.clob.buy_fok(leg.token_id, leg.price, amount))
            .collect();

        let results = futures::future::join_all(buy_futures).await;

        let mut trades = Vec::with_capacity(legs.len());
        let mut min_filled = amount;
        let mut all_filled = true;

        for (i, result) in results.into_iter().enumerate() {
            let leg = &legs[i];
            match result {
                Ok(order) => {
                    min_filled = min_filled.min(order.filled_size);
                    if order.filled_size < amount {
                        all_filled = false;
                    }
                    trades.push(TradeRecord {
                        id: Uuid::now_v7(),
                        token_id: leg.token_id,
                        side: TradeSide::Buy,
                        price: leg.price,
                        size: amount,
                        filled_size: order.filled_size,
                        fee: Decimal::ZERO,
                        tx_type: TxType::ClobOrder,
                        tx_hash: None,
                    });
                }
                Err(e) => {
                    tracing::error!(
                        leg = i,
                        token_id = %leg.token_id,
                        error = %e,
                        "NegRisk leg failed"
                    );
                    min_filled = Decimal::ZERO;
                    all_filled = false;
                }
            }
        }

        // Calculate profit based on minimum filled across all legs
        let total_cost_per_unit: Decimal = legs.iter().map(|l| l.price).sum();
        let realized_profit = if min_filled > Decimal::ZERO {
            (Decimal::ONE - total_cost_per_unit) * min_filled
        } else {
            Decimal::ZERO
        };

        let status = if min_filled == amount && all_filled {
            ExecutionStatus::Success
        } else if min_filled > Decimal::ZERO {
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
            ExecutionPlan::NegRiskArbitrage {
                neg_risk_market_id,
                legs,
                amount,
            } => {
                self.execute_neg_risk(legs, *amount, *neg_risk_market_id, opp.id)
                    .await
            }
            ExecutionPlan::CrossMarket {
                leg_a,
                leg_b,
                amount,
                ..
            } => {
                self.execute_cross_market(leg_a, leg_b, *amount, opp.id)
                    .await
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
