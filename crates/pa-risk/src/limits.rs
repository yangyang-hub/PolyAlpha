use pa_core::config::RiskConfig;
use pa_core::types::{RiskDecision, RiskRejectReason, TradingOpportunity};
use rust_decimal::Decimal;

/// Checks trade-level risk limits.
pub struct LimitsChecker {
    config: RiskConfig,
}

impl LimitsChecker {
    pub fn new(config: RiskConfig) -> Self {
        Self { config }
    }

    /// Check if an opportunity passes all risk limits.
    pub fn check(&self, opp: &TradingOpportunity, total_exposure: Decimal) -> RiskDecision {
        // Check minimum order size
        if opp.size < self.config.min_order_usdc {
            return RiskDecision::Reject(RiskRejectReason::BelowMinOrder);
        }

        // Check single trade size
        let trade_value = opp.size;
        if trade_value > self.config.max_position_per_market {
            return RiskDecision::Reject(RiskRejectReason::ExceedsTradeLimit);
        }

        // Check total exposure
        if total_exposure + trade_value > self.config.max_total_exposure {
            return RiskDecision::Reject(RiskRejectReason::ExceedsTotalExposure);
        }

        // Check minimum profit threshold (from config, not hard-coded)
        if opp.estimated_profit < self.config.min_profit_usdc {
            return RiskDecision::Reject(RiskRejectReason::BelowMinProfit);
        }

        RiskDecision::Approve
    }
}
