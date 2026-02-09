use alloy::primitives::U256;
use async_trait::async_trait;
use chrono::Utc;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use uuid::Uuid;

use pa_core::traits::Strategy;
use pa_core::types::{
    ArbitrageOpportunity, ExecutionPlan, MarketInfo, OrderBook, StrategyType,
};
use pa_core::Result;

use crate::profitability::ProfitCalculator;

/// YES/NO single-market arbitrage strategy.
///
/// Detects when YES_ask + NO_ask < $1.00 (BuyAndMerge)
/// or YES_bid + NO_bid > $1.00 (SplitAndSell).
pub struct YesNoArbitrage {
    /// Minimum spread in basis points to consider an opportunity.
    min_spread_bps: u32,
    /// Maximum trade size in USDC.
    max_trade_size: Decimal,
    /// Minimum net profit in USDC to execute.
    min_profit: Decimal,
    /// Profit calculator with fee model.
    profit_calc: ProfitCalculator,
    /// Callback to get current order book for a token.
    get_orderbook: Box<dyn Fn(U256) -> Option<OrderBook> + Send + Sync>,
}

impl YesNoArbitrage {
    pub fn new(
        min_spread_bps: u32,
        max_trade_size: Decimal,
        min_profit: Decimal,
        gas_cost_usd: Decimal,
        get_orderbook: Box<dyn Fn(U256) -> Option<OrderBook> + Send + Sync>,
    ) -> Self {
        Self {
            min_spread_bps,
            max_trade_size,
            min_profit,
            profit_calc: ProfitCalculator::new(gas_cost_usd),
            get_orderbook,
        }
    }

    /// Detect arbitrage opportunity for a single binary market.
    fn detect_single(&self, market: &MarketInfo) -> Option<ArbitrageOpportunity> {
        if market.tokens.len() != 2 || !market.active {
            return None;
        }

        let yes_token = &market.tokens[0];
        let no_token = &market.tokens[1];

        let yes_book = (self.get_orderbook)(yes_token.token_id)?;
        let no_book = (self.get_orderbook)(no_token.token_id)?;

        // Try BuyAndMerge: buy at ask prices, merge for $1.00
        if let Some(opp) = self.detect_buy_and_merge(market, &yes_book, &no_book) {
            return Some(opp);
        }

        // Try SplitAndSell: split $1.00, sell at bid prices
        if let Some(opp) = self.detect_split_and_sell(market, &yes_book, &no_book) {
            return Some(opp);
        }

        None
    }

    fn detect_buy_and_merge(
        &self,
        market: &MarketInfo,
        yes_book: &OrderBook,
        no_book: &OrderBook,
    ) -> Option<ArbitrageOpportunity> {
        let yes_ask = yes_book.best_ask()?;
        let no_ask = no_book.best_ask()?;

        let total_cost = yes_ask.price + no_ask.price;

        // Must be < 1.00 for BuyAndMerge to work
        if total_cost >= Decimal::ONE {
            return None;
        }

        let spread = Decimal::ONE - total_cost;
        let spread_bps = (spread * dec!(10000)).to_u32()?;

        if spread_bps < self.min_spread_bps {
            return None;
        }

        // Executable size = min of both ask sizes, capped by max trade size
        let max_size_by_price = self.max_trade_size / total_cost;
        let size = yes_ask
            .size
            .min(no_ask.size)
            .min(max_size_by_price);

        let estimate = self.profit_calc.buy_and_merge_profit(
            yes_ask.price,
            no_ask.price,
            size,
            market.fee_rate_bps,
        );

        if estimate.net_profit < self.min_profit {
            return None;
        }

        Some(ArbitrageOpportunity {
            id: Uuid::now_v7(),
            strategy_type: StrategyType::YesNoMerge,
            condition_id: market.condition_id,
            question: market.question.clone(),
            spread,
            estimated_profit: estimate.net_profit,
            size,
            detected_at: Utc::now(),
            execution_plan: ExecutionPlan::BuyAndMerge {
                yes_token_id: market.tokens[0].token_id,
                no_token_id: market.tokens[1].token_id,
                yes_price: yes_ask.price,
                no_price: no_ask.price,
                merge_amount: size,
                condition_id: market.condition_id,
            },
        })
    }

    fn detect_split_and_sell(
        &self,
        market: &MarketInfo,
        yes_book: &OrderBook,
        no_book: &OrderBook,
    ) -> Option<ArbitrageOpportunity> {
        let yes_bid = yes_book.best_bid()?;
        let no_bid = no_book.best_bid()?;

        let total_revenue = yes_bid.price + no_bid.price;

        // Must be > 1.00 for SplitAndSell to work
        if total_revenue <= Decimal::ONE {
            return None;
        }

        let spread = total_revenue - Decimal::ONE;
        let spread_bps = (spread * dec!(10000)).to_u32()?;

        if spread_bps < self.min_spread_bps {
            return None;
        }

        let size = yes_bid
            .size
            .min(no_bid.size)
            .min(self.max_trade_size);

        let estimate = self.profit_calc.split_and_sell_profit(
            yes_bid.price,
            no_bid.price,
            size,
            market.fee_rate_bps,
        );

        if estimate.net_profit < self.min_profit {
            return None;
        }

        Some(ArbitrageOpportunity {
            id: Uuid::now_v7(),
            strategy_type: StrategyType::YesNoSplit,
            condition_id: market.condition_id,
            question: market.question.clone(),
            spread,
            estimated_profit: estimate.net_profit,
            size,
            detected_at: Utc::now(),
            execution_plan: ExecutionPlan::SplitAndSell {
                yes_token_id: market.tokens[0].token_id,
                no_token_id: market.tokens[1].token_id,
                yes_price: yes_bid.price,
                no_price: no_bid.price,
                split_amount: size,
                condition_id: market.condition_id,
            },
        })
    }
}

#[async_trait]
impl Strategy for YesNoArbitrage {
    fn name(&self) -> &str {
        "YesNo Arbitrage"
    }

    fn strategy_type(&self) -> StrategyType {
        StrategyType::YesNoMerge
    }

    async fn scan(&self, markets: &[MarketInfo]) -> Result<Vec<ArbitrageOpportunity>> {
        let opportunities: Vec<ArbitrageOpportunity> = markets
            .iter()
            .filter(|m| !m.neg_risk) // Only binary markets
            .filter_map(|m| self.detect_single(m))
            .collect();

        tracing::debug!(count = opportunities.len(), "YesNo scan complete");
        Ok(opportunities)
    }
}

// Helper to convert Decimal to u32
trait ToU32 {
    fn to_u32(&self) -> Option<u32>;
}

impl ToU32 for Decimal {
    fn to_u32(&self) -> Option<u32> {
        use rust_decimal::prelude::ToPrimitive;
        ToPrimitive::to_u32(self)
    }
}
