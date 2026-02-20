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

    /// Calculate net profit for a NegRisk multi-outcome arbitrage.
    ///
    /// Buy YES tokens at `ask_prices[i]` for all N outcomes.
    /// The constraint: sum(YES_price) should = $1.00 for the event.
    /// If sum(ask) < $1.00, profit = ($1.00 - sum(ask)) * size - fees - gas.
    ///
    /// We pay taker fees on each leg. Gas covers a single merge tx.
    pub fn neg_risk_buy_all_yes_profit(
        &self,
        ask_prices: &[Decimal],
        size: Decimal,
        fee_rate_bps: u32,
    ) -> ProfitEstimate {
        let total_ask: Decimal = ask_prices.iter().copied().sum();
        let revenue_per_unit = Decimal::ONE;

        let total_fee_per_unit: Decimal = ask_prices
            .iter()
            .map(|&p| self.capped_fee(p, fee_rate_bps))
            .sum();

        let gross_profit = (revenue_per_unit - total_ask) * size;
        let total_fees = total_fee_per_unit * size;
        let total_gas = self.gas_cost_usd;

        let net_profit = gross_profit - total_fees - total_gas;
        let total_cost = total_ask * size + total_fees + total_gas;
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

    /// Calculate net profit for a cross-market arbitrage.
    ///
    /// Two independent markets with correlated outcomes that should sum to `expected_sum`.
    /// If sum(ask) < expected_sum, buy both (underpriced).
    /// If sum(bid) > expected_sum, sell both (overpriced).
    /// Gas is doubled: two independent on-chain transactions (one per market).
    pub fn cross_market_profit(
        &self,
        price_a: Decimal,
        fee_bps_a: u32,
        price_b: Decimal,
        fee_bps_b: u32,
        size: Decimal,
        expected_sum: Decimal,
        is_buy: bool,
    ) -> ProfitEstimate {
        let fee_a = self.capped_fee(price_a, fee_bps_a);
        let fee_b = self.capped_fee(price_b, fee_bps_b);

        let gross_profit = if is_buy {
            // Buy underpriced: revenue = expected_sum, cost = sum(asks)
            (expected_sum - price_a - price_b) * size
        } else {
            // Sell overpriced: revenue = sum(bids), cost = expected_sum
            (price_a + price_b - expected_sum) * size
        };

        let total_fees = (fee_a + fee_b) * size;
        // Two on-chain txs (merge/split on each market)
        let total_gas = self.gas_cost_usd * Decimal::TWO;

        let net_profit = gross_profit - total_fees - total_gas;
        let total_cost = if is_buy {
            (price_a + price_b) * size + total_fees + total_gas
        } else {
            expected_sum * size + total_fees + total_gas
        };
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

    /// Calculate expected profit for a directional buy.
    ///
    /// Unlike arbitrage (risk-free), directional buys are probabilistic:
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

    #[test]
    fn test_neg_risk_buy_all_yes_profitable() {
        // 4 outcomes: ask prices sum to 0.94 (< 1.00, spread = 0.06)
        let asks = vec![dec!(0.30), dec!(0.25), dec!(0.20), dec!(0.19)];
        let est = calc().neg_risk_buy_all_yes_profit(&asks, dec!(100), 200);
        // gross = (1.00 - 0.94) * 100 = 6.00
        assert_eq!(est.gross_profit, dec!(6.00));
        assert!(est.net_profit > Decimal::ZERO, "Should be profitable: {}", est.net_profit);
    }

    #[test]
    fn test_neg_risk_no_opportunity() {
        // 3 outcomes: ask prices sum to 1.02 (> 1.00, no opportunity)
        let asks = vec![dec!(0.40), dec!(0.32), dec!(0.30)];
        let est = calc().neg_risk_buy_all_yes_profit(&asks, dec!(100), 200);
        assert!(est.gross_profit < Decimal::ZERO);
    }

    #[test]
    fn test_cross_market_buy_profitable() {
        // Two correlated markets: ask_a=0.45, ask_b=0.48 → sum=0.93 < 1.00
        let est = calc().cross_market_profit(
            dec!(0.45), 200, dec!(0.48), 200, dec!(100), Decimal::ONE, true,
        );
        // gross = (1.00 - 0.93) * 100 = 7.00
        assert_eq!(est.gross_profit, dec!(7.00));
        assert!(est.net_profit > Decimal::ZERO, "Should be profitable: {}", est.net_profit);
        // Gas is doubled: 0.10 * 2 = 0.20
        assert_eq!(est.gas, dec!(0.20));
    }

    #[test]
    fn test_cross_market_sell_profitable() {
        // Two correlated markets: bid_a=0.55, bid_b=0.52 → sum=1.07 > 1.00
        let est = calc().cross_market_profit(
            dec!(0.55), 200, dec!(0.52), 200, dec!(100), Decimal::ONE, false,
        );
        // gross = (1.07 - 1.00) * 100 = 7.00
        assert_eq!(est.gross_profit, dec!(7.00));
        assert!(est.net_profit > Decimal::ZERO, "Should be profitable: {}", est.net_profit);
    }

    #[test]
    fn test_cross_market_no_opportunity() {
        // Sum equals expected_sum, no spread
        let est = calc().cross_market_profit(
            dec!(0.50), 200, dec!(0.50), 200, dec!(100), Decimal::ONE, true,
        );
        assert_eq!(est.gross_profit, Decimal::ZERO);
        assert!(est.net_profit < Decimal::ZERO); // fees + gas make it negative
    }

    #[test]
    fn test_directional_buy_positive_edge() {
        // model_prob=0.70, ask=0.50 → strong positive edge
        let est = calc().directional_buy_profit(dec!(0.50), dec!(0.70), dec!(100), 200);
        // gross = (0.70 - 0.50) * 100 = 20.00
        assert_eq!(est.gross_profit, dec!(20.00));
        assert!(est.net_profit > Decimal::ZERO, "Expected positive profit, got {}", est.net_profit);
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
        assert!(est.net_profit > Decimal::ZERO, "Expected positive net profit, got {}", est.net_profit);
        assert_eq!(est.gas, Decimal::ZERO); // CLOB-only
        assert!(est.roi > Decimal::ZERO);
    }

    #[test]
    fn test_directional_sell_profit_at_loss() {
        // Bought at 0.70, sell at 0.40 → gross profit = (0.40 - 0.70) * 100 = -30.00
        let est = calc().directional_sell_profit(dec!(0.40), dec!(0.70), dec!(100), 200);
        assert_eq!(est.gross_profit, dec!(-30.00));
        assert!(est.net_profit < Decimal::ZERO, "Expected negative net profit (stop-loss), got {}", est.net_profit);
        assert_eq!(est.gas, Decimal::ZERO); // CLOB-only
        assert!(est.roi < Decimal::ZERO);
    }
}
