use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use pa_core::types::ProfitEstimate;

/// Calculates profitability for arbitrage opportunities.
pub struct ProfitCalculator {
    /// Estimated gas cost per on-chain transaction in USD.
    pub gas_cost_usd: Decimal,
}

impl ProfitCalculator {
    pub fn new(gas_cost_usd: Decimal) -> Self {
        Self { gas_cost_usd }
    }

    /// Calculate net profit for a BuyAndMerge strategy.
    ///
    /// Buy YES at `yes_ask` + Buy NO at `no_ask` → merge → receive $1.00 USDC.
    pub fn buy_and_merge_profit(
        &self,
        yes_ask: Decimal,
        no_ask: Decimal,
        size: Decimal,
        fee_rate_bps: u32,
    ) -> ProfitEstimate {
        let cost_per_unit = yes_ask + no_ask;
        let revenue_per_unit = Decimal::ONE;

        // Taker fees on both legs
        let yes_fee = self.capped_fee(yes_ask, fee_rate_bps);
        let no_fee = self.capped_fee(no_ask, fee_rate_bps);

        let gross_profit = (revenue_per_unit - cost_per_unit) * size;
        let total_fees = (yes_fee + no_fee) * size;
        let total_gas = self.gas_cost_usd; // single merge tx

        let net_profit = gross_profit - total_fees - total_gas;
        let total_cost = cost_per_unit * size + total_fees + total_gas;
        let roi = if total_cost > Decimal::ZERO {
            net_profit / total_cost
        } else {
            Decimal::ZERO
        };

        ProfitEstimate {
            gross_profit,
            fees: total_fees,
            gas: total_gas,
            net_profit,
            roi,
        }
    }

    /// Calculate net profit for a SplitAndSell strategy.
    ///
    /// Split $1.00 USDC → YES+NO tokens, sell YES at `yes_bid` + NO at `no_bid`.
    pub fn split_and_sell_profit(
        &self,
        yes_bid: Decimal,
        no_bid: Decimal,
        size: Decimal,
        fee_rate_bps: u32,
    ) -> ProfitEstimate {
        let cost_per_unit = Decimal::ONE; // split costs $1.00
        let revenue_per_unit = yes_bid + no_bid;

        let yes_fee = self.capped_fee(yes_bid, fee_rate_bps);
        let no_fee = self.capped_fee(no_bid, fee_rate_bps);

        let gross_profit = (revenue_per_unit - cost_per_unit) * size;
        let total_fees = (yes_fee + no_fee) * size;
        let total_gas = self.gas_cost_usd; // single split tx

        let net_profit = gross_profit - total_fees - total_gas;
        let total_cost = cost_per_unit * size + total_fees + total_gas;
        let roi = if total_cost > Decimal::ZERO {
            net_profit / total_cost
        } else {
            Decimal::ZERO
        };

        ProfitEstimate {
            gross_profit,
            fees: total_fees,
            gas: total_gas,
            net_profit,
            roi,
        }
    }

    /// Polymarket fee cap: `min(fee_rate * price, price * (1 - price))`
    ///
    /// The fee can never exceed the value of the losing side.
    pub fn capped_fee(&self, price: Decimal, fee_rate_bps: u32) -> Decimal {
        let fee_rate = Decimal::from(fee_rate_bps) / dec!(10000);
        let raw_fee = price * fee_rate;
        let max_fee = price * (Decimal::ONE - price);
        raw_fee.min(max_fee)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn calc() -> ProfitCalculator {
        ProfitCalculator::new(dec!(0.10))
    }

    #[test]
    fn test_capped_fee_normal() {
        // price=0.50, fee_rate=200bps (2%)
        // raw_fee = 0.50 * 0.02 = 0.01
        // max_fee = 0.50 * 0.50 = 0.25
        // capped = 0.01
        assert_eq!(calc().capped_fee(dec!(0.50), 200), dec!(0.01));
    }

    #[test]
    fn test_capped_fee_extreme_price() {
        // price=0.99, fee_rate=200bps
        // raw_fee = 0.99 * 0.02 = 0.0198
        // max_fee = 0.99 * 0.01 = 0.0099
        // capped = 0.0099
        assert_eq!(calc().capped_fee(dec!(0.99), 200), dec!(0.0099));
    }

    #[test]
    fn test_buy_and_merge_profitable() {
        let est = calc().buy_and_merge_profit(dec!(0.47), dec!(0.50), dec!(100), 200);
        // gross = (1.00 - 0.97) * 100 = 3.00
        assert_eq!(est.gross_profit, dec!(3.00));
        assert!(est.net_profit > Decimal::ZERO, "Should be profitable: {}", est.net_profit);
    }

    #[test]
    fn test_buy_and_merge_unprofitable_tight_spread() {
        let est = calc().buy_and_merge_profit(dec!(0.49), dec!(0.50), dec!(100), 200);
        // gross = (1.00 - 0.99) * 100 = 1.00
        // fees eat into this
        assert!(est.net_profit < dec!(1.00));
    }

    #[test]
    fn test_split_and_sell_profitable() {
        let est = calc().split_and_sell_profit(dec!(0.53), dec!(0.50), dec!(100), 200);
        // gross = (1.03 - 1.00) * 100 = 3.00
        assert_eq!(est.gross_profit, dec!(3.00));
        assert!(est.net_profit > Decimal::ZERO);
    }
}
