use async_trait::async_trait;
use alloy::primitives::{B256, U256};
use rust_decimal::Decimal;
use pa_core::config::RiskConfig;
use pa_core::traits::RiskManager;
use pa_core::types::{ArbitrageOpportunity, ExecutionResult, RiskDecision, RiskRejectReason, StrategyType};

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
    config: RiskConfig,
}

impl RiskManagerImpl {
    pub fn new(config: RiskConfig) -> Self {
        Self {
            positions: PositionTracker::new(),
            limits: LimitsChecker::new(config.clone()),
            circuit_breaker: CircuitBreaker::new(&config),
            pnl: PnlTracker::new(),
            config,
        }
    }

    /// Get the current position size for a specific token.
    /// Returns `Decimal::ZERO` if no position exists.
    pub fn get_position_size(&self, token_id: &U256) -> Decimal {
        self.positions.get_size(token_id)
    }

    /// Load initial positions from external data (DB rows).
    pub fn load_initial_positions(&self, entries: Vec<(U256, Decimal, Decimal, Option<StrategyType>, Option<B256>)>) {
        self.positions.load_initial(entries);
    }

    /// Snapshot all current positions for persistence.
    pub fn snapshot_positions(&self) -> Vec<(U256, crate::position::PositionEntry)> {
        self.positions.snapshot_all()
    }

    /// Get all positions held by a specific strategy.
    /// Returns (token_id, size, avg_cost) for each position.
    pub fn positions_by_strategy(&self, st: StrategyType) -> Vec<(U256, Decimal, Decimal)> {
        self.positions.positions_by_strategy(st)
    }
}

#[async_trait]
impl RiskManager for RiskManagerImpl {
    fn check_pre_trade(&self, opp: &ArbitrageOpportunity) -> RiskDecision {
        if self.circuit_breaker.is_broken() {
            return RiskDecision::Reject(RiskRejectReason::CircuitBroken);
        }

        // Per-market accumulated position check
        let market_size = self.positions.size_by_market(&opp.condition_id);
        if market_size + opp.size > self.config.max_position_per_market {
            return RiskDecision::Reject(RiskRejectReason::ExceedsMarketPositionLimit);
        }

        // Per-strategy total exposure check
        let strategy_exposure = self.positions.exposure_by_strategy(opp.strategy_type);
        let new_cost = opp.size * opp.execution_plan.entry_price().unwrap_or(Decimal::ONE);
        if strategy_exposure + new_cost > self.config.max_exposure_per_strategy {
            return RiskDecision::Reject(RiskRejectReason::ExceedsStrategyExposure);
        }

        // Per-strategy market count check (only for new markets)
        let already_in_market = market_size > Decimal::ZERO;
        if !already_in_market {
            let market_count = self.positions.market_count_by_strategy(opp.strategy_type);
            if market_count >= self.config.max_markets_per_strategy {
                return RiskDecision::Reject(RiskRejectReason::ExceedsStrategyMarketCount);
            }
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
                .update(trade.token_id, trade.filled_size, trade.price, is_buy, Some(result.strategy_type), Some(trade.condition_id));
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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use rust_decimal_macros::dec;
    use uuid::Uuid;
    use pa_core::types::{ExecutionPlan, TradeSide};

    fn test_config() -> RiskConfig {
        RiskConfig {
            max_position_per_market: dec!(100),
            max_total_exposure: dec!(10000),
            max_daily_loss: dec!(500),
            circuit_breaker_loss: dec!(1000),
            circuit_breaker_consecutive_losses: 5,
            max_slippage_bps: 50,
            min_order_usdc: dec!(1),
            min_profit_usdc: dec!(0.01),
            max_exposure_per_strategy: dec!(200),
            max_markets_per_strategy: 3,
        }
    }

    fn cid(n: u8) -> B256 {
        let mut bytes = [0u8; 32];
        bytes[31] = n;
        B256::from(bytes)
    }

    fn make_opp(condition_id: B256, strategy_type: StrategyType, size: Decimal, price: Decimal) -> ArbitrageOpportunity {
        ArbitrageOpportunity {
            id: Uuid::now_v7(),
            strategy_type,
            condition_id,
            question: "Test".into(),
            spread: dec!(0.05),
            estimated_profit: dec!(1.0),
            size,
            detected_at: Utc::now(),
            execution_plan: ExecutionPlan::DirectionalBuy {
                token_id: U256::from(1),
                side: TradeSide::Buy,
                price,
                size,
                condition_id,
            },
        }
    }

    #[test]
    fn test_approve_under_limits() {
        let rm = RiskManagerImpl::new(test_config());
        let opp = make_opp(cid(1), StrategyType::Weather, dec!(10), dec!(0.50));
        assert_eq!(rm.check_pre_trade(&opp), RiskDecision::Approve);
    }

    #[test]
    fn test_reject_per_market_accumulated() {
        let rm = RiskManagerImpl::new(test_config());

        // Pre-load 90 shares in market cid(1) via position tracker
        rm.positions.update(U256::from(10), dec!(90), dec!(0.50), true, Some(StrategyType::Weather), Some(cid(1)));

        // Now try to add 20 more → 90+20=110 > max_position_per_market=100
        let opp = make_opp(cid(1), StrategyType::Weather, dec!(20), dec!(0.50));
        assert_eq!(
            rm.check_pre_trade(&opp),
            RiskDecision::Reject(RiskRejectReason::ExceedsMarketPositionLimit),
        );
    }

    #[test]
    fn test_reject_per_strategy_exposure() {
        let rm = RiskManagerImpl::new(test_config());

        // Pre-load: 100 shares @ $0.90 = $90, and another 100 @ $0.80 = $80 → total $170
        rm.positions.update(U256::from(10), dec!(100), dec!(0.90), true, Some(StrategyType::Weather), Some(cid(1)));
        rm.positions.update(U256::from(20), dec!(100), dec!(0.80), true, Some(StrategyType::Weather), Some(cid(2)));

        // New trade: 50 shares @ $0.80 = $40 → total exposure $210 > max_exposure_per_strategy=$200
        let opp = make_opp(cid(3), StrategyType::Weather, dec!(50), dec!(0.80));
        assert_eq!(
            rm.check_pre_trade(&opp),
            RiskDecision::Reject(RiskRejectReason::ExceedsStrategyExposure),
        );
    }

    #[test]
    fn test_reject_per_strategy_market_count() {
        let rm = RiskManagerImpl::new(test_config());

        // Fill 3 markets (max_markets_per_strategy=3)
        rm.positions.update(U256::from(10), dec!(5), dec!(0.50), true, Some(StrategyType::Weather), Some(cid(1)));
        rm.positions.update(U256::from(20), dec!(5), dec!(0.50), true, Some(StrategyType::Weather), Some(cid(2)));
        rm.positions.update(U256::from(30), dec!(5), dec!(0.50), true, Some(StrategyType::Weather), Some(cid(3)));

        // Try a 4th market → rejected
        let opp = make_opp(cid(4), StrategyType::Weather, dec!(5), dec!(0.50));
        assert_eq!(
            rm.check_pre_trade(&opp),
            RiskDecision::Reject(RiskRejectReason::ExceedsStrategyMarketCount),
        );
    }

    #[test]
    fn test_same_market_no_count_trigger() {
        let rm = RiskManagerImpl::new(test_config());

        // Fill 3 markets
        rm.positions.update(U256::from(10), dec!(5), dec!(0.50), true, Some(StrategyType::Weather), Some(cid(1)));
        rm.positions.update(U256::from(20), dec!(5), dec!(0.50), true, Some(StrategyType::Weather), Some(cid(2)));
        rm.positions.update(U256::from(30), dec!(5), dec!(0.50), true, Some(StrategyType::Weather), Some(cid(3)));

        // Adding more to existing market cid(1) should NOT trigger market count check
        let opp = make_opp(cid(1), StrategyType::Weather, dec!(5), dec!(0.50));
        // Should pass market count check (already in market), may pass all checks
        assert_eq!(rm.check_pre_trade(&opp), RiskDecision::Approve);
    }
}
