use rust_decimal::Decimal;
use pa_core::config::RiskConfig;
use pa_core::types::{ArbitrageOpportunity, RiskDecision, RiskRejectReason};

/// Checks trade-level risk limits.
pub struct LimitsChecker {
    config: RiskConfig,
}

impl LimitsChecker {
    pub fn new(config: RiskConfig) -> Self {
        Self { config }
    }

    /// Check if an opportunity passes all risk limits.
    pub fn check(
        &self,
        opp: &ArbitrageOpportunity,
        total_exposure: Decimal,
    ) -> RiskDecision {
        // Check single trade size
        let trade_value = opp.size;
        if trade_value > self.config.max_position_per_market {
            return RiskDecision::Reject(RiskRejectReason::ExceedsTradeLimit);
        }

        // Check total exposure
        if total_exposure + trade_value > self.config.max_total_exposure {
            return RiskDecision::Reject(RiskRejectReason::ExceedsTotalExposure);
        }

        // Check minimum profit threshold
        let min_profit = Decimal::from(50) / Decimal::from(100); // $0.50
        if opp.estimated_profit < min_profit {
            return RiskDecision::Reject(RiskRejectReason::BelowMinProfit);
        }

        RiskDecision::Approve
    }
}
