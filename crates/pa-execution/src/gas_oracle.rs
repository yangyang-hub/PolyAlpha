use rust_decimal::Decimal;
use rust_decimal_macros::dec;

/// Estimates gas costs for on-chain operations on Polygon.
pub struct GasOracle {
    /// Base gas price in Gwei.
    gas_price_gwei: Decimal,
    /// MATIC/USD price.
    matic_usd: Decimal,
}

impl GasOracle {
    pub fn new(gas_price_gwei: Decimal, matic_usd: Decimal) -> Self {
        Self {
            gas_price_gwei,
            matic_usd,
        }
    }

    /// Default oracle with conservative estimates for Polygon.
    pub fn default_polygon() -> Self {
        Self {
            gas_price_gwei: dec!(50), // 50 Gwei
            matic_usd: dec!(0.50),    // ~$0.50 per MATIC
        }
    }

    /// Estimate gas cost in USD for a given gas limit.
    pub fn estimate_usd(&self, gas_limit: u64) -> Decimal {
        let gas_cost_matic = Decimal::from(gas_limit) * self.gas_price_gwei / dec!(1_000_000_000); // Gwei to MATIC
        gas_cost_matic * self.matic_usd
    }

    /// Estimate gas cost for a merge operation (~200k gas).
    pub fn estimate_merge(&self) -> Decimal {
        self.estimate_usd(200_000)
    }

    /// Estimate gas cost for a split operation (~200k gas).
    pub fn estimate_split(&self) -> Decimal {
        self.estimate_usd(200_000)
    }

    /// Estimate gas cost for an ERC-20 approval (~50k gas).
    pub fn estimate_approve(&self) -> Decimal {
        self.estimate_usd(50_000)
    }

    /// Update gas price.
    pub fn set_gas_price(&mut self, gas_price_gwei: Decimal) {
        self.gas_price_gwei = gas_price_gwei;
    }

    /// Update MATIC price.
    pub fn set_matic_price(&mut self, matic_usd: Decimal) {
        self.matic_usd = matic_usd;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gas_estimation() {
        let oracle = GasOracle::default_polygon();
        let merge_cost = oracle.estimate_merge();
        // 200k gas * 50 Gwei / 1e9 = 0.01 MATIC * $0.50 = $0.005
        assert_eq!(merge_cost, dec!(0.005));
        assert!(merge_cost < dec!(0.10), "Gas cost should be under $0.10");
    }
}
