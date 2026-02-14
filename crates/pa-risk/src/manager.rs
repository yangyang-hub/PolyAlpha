use async_trait::async_trait;
use rust_decimal::Decimal;
use pa_core::config::RiskConfig;
use pa_core::traits::RiskManager;
use pa_core::types::{ArbitrageOpportunity, ExecutionResult, RiskDecision};

use crate::circuit_breaker::CircuitBreaker;
use crate::limits::LimitsChecker;
use crate::pnl::PnlTracker;
use crate::position::PositionTracker;

/// Composite risk manager combining position tracking, limits, and circuit breaker.
pub struct RiskManagerImpl {
    positions: PositionTracker,
    limits: LimitsChecker,
    circuit_breaker: CircuitBreaker,
    pnl: PnlTracker,
}

impl RiskManagerImpl {
    pub fn new(config: RiskConfig) -> Self {
        Self {
            positions: PositionTracker::new(),
            limits: LimitsChecker::new(config.clone()),
            circuit_breaker: CircuitBreaker::new(&config),
            pnl: PnlTracker::new(),
        }
    }
}

#[async_trait]
impl RiskManager for RiskManagerImpl {
    fn check_pre_trade(&self, opp: &ArbitrageOpportunity) -> RiskDecision {
        if self.circuit_breaker.is_broken() {
            return RiskDecision::Reject(pa_core::types::RiskRejectReason::CircuitBroken);
        }

        self.limits.check(opp, self.positions.total_exposure())
    }

    fn update_position(&self, result: &ExecutionResult) {
        // Update PnL
        self.pnl.record(result.realized_profit);
        self.circuit_breaker.record_trade(result.realized_profit);

        // Update positions from trades
        for trade in &result.trades {
            let is_buy = matches!(trade.side, pa_core::types::TradeSide::Buy);
            self.positions
                .update(trade.token_id, trade.filled_size, trade.price, is_buy);
        }
    }

    fn is_circuit_broken(&self) -> bool {
        self.circuit_breaker.is_broken()
    }

    fn total_exposure(&self) -> Decimal {
        self.positions.total_exposure()
    }

    fn reset_daily(&self) {
        self.circuit_breaker.reset_daily();
    }
}
