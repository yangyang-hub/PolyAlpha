use pa_core::types::ProfitEstimate;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

/// Calculates profitability for trading opportunities.
pub struct ProfitCalculator {
    /// Estimated gas cost per on-chain transaction in USD.
    pub gas_cost_usd: Decimal,
}

impl ProfitCalculator {
    pub fn new(gas_cost_usd: Decimal) -> Self {
        Self { gas_cost_usd }
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

    /// Calculate expected profit for a directional buy.
    ///
    /// Directional buys are probabilistic:
    /// - Expected value = model_prob * payout - cost
    /// - EV per unit = model_prob * $1.00 - ask_price
    /// - Net = EV * size - fee - gas(0, CLOB-only)
    pub fn directional_buy_profit(
        &self,
        ask_price: Decimal,
        model_prob: Decimal,
        size: Decimal,
        fee_rate_bps: u32,
    ) -> ProfitEstimate {
        let ev_per_unit = model_prob - ask_price;
        let gross_profit = ev_per_unit * size;
        let fee = self.capped_fee(ask_price, fee_rate_bps);
        let total_fees = fee * size;
        let total_gas = Decimal::ZERO; // CLOB-only, no on-chain tx

        let net_profit = gross_profit - total_fees;
        let total_cost = ask_price * size + total_fees;
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

    /// Calculate profit for a directional sell (exit).
    ///
    /// Sells existing position at the best bid price.
    /// Revenue = sell_price × size, Cost = avg_cost × size.
    /// Note: net_profit can be negative (stop-loss). This is intentional —
    /// exiting a losing position early is better than waiting for resolution to zero.
    pub fn directional_sell_profit(
        &self,
        sell_price: Decimal,
        avg_cost: Decimal,
        size: Decimal,
        fee_rate_bps: u32,
    ) -> ProfitEstimate {
        let revenue = sell_price * size;
        let cost = avg_cost * size;
        let gross_profit = revenue - cost;
        let fee = self.capped_fee(sell_price, fee_rate_bps);
        let total_fees = fee * size;
        let total_gas = Decimal::ZERO; // CLOB-only

        let net_profit = gross_profit - total_fees;
        let roi = if cost > Decimal::ZERO {
            net_profit / cost
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
    fn test_directional_buy_positive_edge() {
        // model_prob=0.70, ask=0.50 → strong positive edge
        let est = calc().directional_buy_profit(dec!(0.50), dec!(0.70), dec!(100), 200);
        // gross = (0.70 - 0.50) * 100 = 20.00
        assert_eq!(est.gross_profit, dec!(20.00));
        assert!(
            est.net_profit > Decimal::ZERO,
            "Expected positive profit, got {}",
            est.net_profit
        );
        assert_eq!(est.gas, Decimal::ZERO); // CLOB-only
    }

    #[test]
    fn test_directional_buy_no_edge() {
        // model_prob=0.45, ask=0.50 → negative edge (market overestimates)
        let est = calc().directional_buy_profit(dec!(0.50), dec!(0.45), dec!(100), 200);
        // gross = (0.45 - 0.50) * 100 = -5.00
        assert!(est.gross_profit < Decimal::ZERO);
        assert!(est.net_profit < Decimal::ZERO);
    }

    #[test]
    fn test_directional_sell_profit_positive() {
        // Bought at 0.50, sell at 0.80 → gross profit = (0.80 - 0.50) * 100 = 30.00
        let est = calc().directional_sell_profit(dec!(0.80), dec!(0.50), dec!(100), 200);
        assert_eq!(est.gross_profit, dec!(30.00));
        assert!(
            est.net_profit > Decimal::ZERO,
            "Expected positive net profit, got {}",
            est.net_profit
        );
        assert_eq!(est.gas, Decimal::ZERO); // CLOB-only
        assert!(est.roi > Decimal::ZERO);
    }

    #[test]
    fn test_directional_sell_profit_at_loss() {
        // Bought at 0.70, sell at 0.40 → gross profit = (0.40 - 0.70) * 100 = -30.00
        let est = calc().directional_sell_profit(dec!(0.40), dec!(0.70), dec!(100), 200);
        assert_eq!(est.gross_profit, dec!(-30.00));
        assert!(
            est.net_profit < Decimal::ZERO,
            "Expected negative net profit (stop-loss), got {}",
            est.net_profit
        );
        assert_eq!(est.gas, Decimal::ZERO); // CLOB-only
        assert!(est.roi < Decimal::ZERO);
    }
}
