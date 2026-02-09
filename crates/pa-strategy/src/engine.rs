use std::sync::Arc;
use pa_core::traits::{Executor, RiskManager, Strategy};
use pa_core::types::{ArbitrageOpportunity, MarketInfo, RiskDecision};
use tokio::time::{Duration, interval};
use tracing;

/// The main strategy engine that orchestrates scanning, risk checks, and execution.
pub struct StrategyEngine {
    strategies: Vec<Box<dyn Strategy>>,
    executor: Arc<dyn Executor>,
    risk_manager: Arc<dyn RiskManager>,
    scan_interval: Duration,
}

impl StrategyEngine {
    pub fn new(
        strategies: Vec<Box<dyn Strategy>>,
        executor: Arc<dyn Executor>,
        risk_manager: Arc<dyn RiskManager>,
        scan_interval_ms: u64,
    ) -> Self {
        Self {
            strategies,
            executor,
            risk_manager,
            scan_interval: Duration::from_millis(scan_interval_ms),
        }
    }

    /// Run the main trading loop.
    pub async fn run(&self, markets: &[MarketInfo]) -> anyhow::Result<()> {
        tracing::info!(
            strategies = self.strategies.len(),
            markets = markets.len(),
            "Strategy engine starting"
        );

        let mut ticker = interval(self.scan_interval);

        loop {
            ticker.tick().await;

            if self.risk_manager.is_circuit_broken() {
                tracing::warn!("Circuit breaker active, skipping scan");
                continue;
            }

            for strategy in &self.strategies {
                match strategy.scan(markets).await {
                    Ok(opportunities) => {
                        for opp in opportunities {
                            self.process_opportunity(&opp).await;
                        }
                    }
                    Err(e) => {
                        tracing::error!(strategy = strategy.name(), error = %e, "Scan failed");
                    }
                }
            }
        }
    }

    async fn process_opportunity(&self, opp: &ArbitrageOpportunity) {
        tracing::info!(
            id = %opp.id,
            strategy = ?opp.strategy_type,
            spread = %opp.spread,
            profit = %opp.estimated_profit,
            size = %opp.size,
            "Opportunity detected"
        );

        // Pre-trade risk check
        match self.risk_manager.check_pre_trade(opp) {
            RiskDecision::Approve => {}
            RiskDecision::Reject(reason) => {
                tracing::warn!(id = %opp.id, reason = ?reason, "Opportunity rejected by risk manager");
                return;
            }
        }

        // Execute
        match self.executor.execute(opp).await {
            Ok(result) => {
                tracing::info!(
                    id = %opp.id,
                    status = ?result.status,
                    profit = %result.realized_profit,
                    "Execution complete"
                );
                self.risk_manager.update_position(&result);
            }
            Err(e) => {
                tracing::error!(id = %opp.id, error = %e, "Execution failed");
            }
        }
    }
}
