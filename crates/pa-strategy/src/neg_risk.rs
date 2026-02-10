use alloy::primitives::U256;
use async_trait::async_trait;
use chrono::Utc;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use uuid::Uuid;

use pa_core::traits::Strategy;
use pa_core::types::{
    ArbitrageOpportunity, ExecutionPlan, MarketInfo, NegRiskEvent, NegRiskLeg,
    OrderBook, Outcome, StrategyType, TradeSide,
};
use pa_core::Result;

use crate::profitability::ProfitCalculator;

/// NegRisk multi-outcome arbitrage strategy.
///
/// In a NegRisk event with N outcomes, each outcome has a binary market (YES/NO).
/// The invariant enforced by the NegRiskAdapter: sum of all YES prices = $1.00.
///
/// **Arbitrage opportunity**: if `sum(YES_ask[i]) < $1.00` across all outcomes,
/// buy all YES tokens and the "complete set" is worth $1.00.
///
/// This is analogous to BuyAndMerge but across multiple outcomes within a single event.
pub struct NegRiskArbitrage {
    /// Minimum spread in basis points to consider.
    min_spread_bps: u32,
    /// Maximum trade size in USDC per event.
    max_trade_size: Decimal,
    /// Minimum net profit in USDC to execute.
    min_profit: Decimal,
    /// Profit calculator with fee model.
    profit_calc: ProfitCalculator,
    /// NegRisk events to scan.
    events: Vec<NegRiskEvent>,
    /// Callback to get current order book for a token.
    get_orderbook: Box<dyn Fn(U256) -> Option<OrderBook> + Send + Sync>,
}

impl NegRiskArbitrage {
    pub fn new(
        min_spread_bps: u32,
        max_trade_size: Decimal,
        min_profit: Decimal,
        gas_cost_usd: Decimal,
        events: Vec<NegRiskEvent>,
        get_orderbook: Box<dyn Fn(U256) -> Option<OrderBook> + Send + Sync>,
    ) -> Self {
        Self {
            min_spread_bps,
            max_trade_size,
            min_profit,
            profit_calc: ProfitCalculator::new(gas_cost_usd),
            events,
            get_orderbook,
        }
    }

    /// Detect arbitrage in a single NegRisk event.
    ///
    /// Checks if sum(YES_ask) < $1.00 across all outcomes.
    /// If so, constructs legs to buy all YES tokens and merge.
    fn detect_event(&self, event: &NegRiskEvent) -> Option<ArbitrageOpportunity> {
        if event.markets.len() < 2 {
            return None;
        }

        // Collect YES ask prices and order book data for each outcome
        let mut legs = Vec::with_capacity(event.markets.len());
        let mut ask_prices = Vec::with_capacity(event.markets.len());
        let mut min_ask_size = Decimal::MAX;

        for market in &event.markets {
            if market.tokens.is_empty() || !market.active {
                return None;
            }

            // YES token is always tokens[0]
            let yes_token = &market.tokens[0];
            let yes_book = (self.get_orderbook)(yes_token.token_id)?;
            let best_ask = yes_book.best_ask()?;

            ask_prices.push(best_ask.price);
            min_ask_size = min_ask_size.min(best_ask.size);

            legs.push(NegRiskLeg {
                token_id: yes_token.token_id,
                condition_id: market.condition_id,
                outcome: Outcome::Yes,
                side: TradeSide::Buy,
                price: best_ask.price,
                size: best_ask.size,
            });
        }

        // Check if sum of YES asks < $1.00
        let total_ask: Decimal = ask_prices.iter().copied().sum();

        if total_ask >= Decimal::ONE {
            return None;
        }

        let spread = Decimal::ONE - total_ask;
        let spread_bps = (spread * dec!(10000)).to_string().parse::<u32>().unwrap_or(0);

        if spread_bps < self.min_spread_bps {
            return None;
        }

        // Executable size: min of all ask sizes, capped by max trade size / total_ask
        let max_size_by_price = self.max_trade_size / total_ask;
        let size = min_ask_size.min(max_size_by_price);

        if size <= Decimal::ZERO {
            return None;
        }

        // Profitability check
        let estimate = self.profit_calc.neg_risk_buy_all_yes_profit(
            &ask_prices,
            size,
            event.fee_rate_bps,
        );

        if estimate.net_profit < self.min_profit {
            return None;
        }

        // Update legs with capped size
        let legs: Vec<NegRiskLeg> = legs
            .into_iter()
            .map(|mut leg| {
                leg.size = size;
                leg
            })
            .collect();

        Some(ArbitrageOpportunity {
            id: Uuid::now_v7(),
            strategy_type: StrategyType::NegRiskConvert,
            condition_id: event.neg_risk_market_id, // use event-level ID
            question: format!("NegRisk event ({} outcomes)", event.markets.len()),
            spread,
            estimated_profit: estimate.net_profit,
            size,
            detected_at: Utc::now(),
            execution_plan: ExecutionPlan::NegRiskArbitrage {
                neg_risk_market_id: event.neg_risk_market_id,
                legs,
                amount: size,
            },
        })
    }
}

#[async_trait]
impl Strategy for NegRiskArbitrage {
    fn name(&self) -> &str {
        "NegRisk Arbitrage"
    }

    fn strategy_type(&self) -> StrategyType {
        StrategyType::NegRiskConvert
    }

    /// Scans NegRisk events (not individual markets) for arbitrage.
    ///
    /// The `markets` parameter from the trait is unused here since NegRisk
    /// operates on events (groups of markets). We scan our stored events instead.
    async fn scan(&self, _markets: &[MarketInfo]) -> Result<Vec<ArbitrageOpportunity>> {
        let opportunities: Vec<ArbitrageOpportunity> = self
            .events
            .iter()
            .filter_map(|e| self.detect_event(e))
            .collect();

        tracing::debug!(count = opportunities.len(), "NegRisk scan complete");
        Ok(opportunities)
    }
}
