use std::collections::HashMap;
use std::sync::Arc;
use alloy::primitives::U256;
use pa_core::traits::{Executor, RiskManager, Strategy};
use pa_core::types::{ArbitrageOpportunity, MarketInfo, RiskDecision};
use tokio::sync::broadcast;
use tokio::time::{Duration, interval};
use tokio_util::sync::CancellationToken;

use pa_market_data::ws_feed::OrderBookUpdate;

/// Event-driven strategy engine that orchestrates scanning, risk checks, and execution.
///
/// Operates in two modes simultaneously:
/// 1. **Event-driven**: reacts to `OrderBookUpdate` events from the WebSocket feed
/// 2. **Periodic fallback**: full market scan on a timer to catch any missed events
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

    /// Run the trading loop.
    ///
    /// Combines event-driven updates (from WS broadcast) with periodic full scans.
    /// Shuts down gracefully when the `cancel` token is cancelled.
    pub async fn run(
        &self,
        markets: &[MarketInfo],
        mut update_rx: broadcast::Receiver<OrderBookUpdate>,
        cancel: CancellationToken,
    ) {
        tracing::info!(
            strategies = self.strategies.len(),
            markets = markets.len(),
            scan_interval_ms = self.scan_interval.as_millis() as u64,
            "Strategy engine starting"
        );

        // Build a lookup: token_id → index into `markets`
        let token_to_market: HashMap<U256, usize> = markets
            .iter()
            .enumerate()
            .flat_map(|(idx, m)| m.tokens.iter().map(move |t| (t.token_id, idx)))
            .collect();

        let mut ticker = interval(self.scan_interval);

        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    tracing::info!("Strategy engine shutting down");
                    break;
                }
                // Event-driven: order book updated
                update = update_rx.recv() => {
                    match update {
                        Ok(event) => {
                            if let Some(&market_idx) = token_to_market.get(&event.token_id) {
                                // Scan only the affected market
                                let affected = &markets[market_idx..=market_idx];
                                self.scan_and_execute(affected).await;
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!(missed = n, "Strategy engine lagged, doing full scan");
                            self.scan_and_execute(markets).await;
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            tracing::info!("WebSocket update channel closed, stopping");
                            break;
                        }
                    }
                }
                // Periodic fallback: full scan
                _ = ticker.tick() => {
                    if self.risk_manager.is_circuit_broken() {
                        tracing::warn!("Circuit breaker active, skipping periodic scan");
                        continue;
                    }
                    self.scan_and_execute(markets).await;
                }
            }
        }
    }

    /// Scan the given markets with all strategies, then risk-check and execute.
    async fn scan_and_execute(&self, markets: &[MarketInfo]) {
        if self.risk_manager.is_circuit_broken() {
            pa_monitor::metrics::CIRCUIT_BREAKER_ACTIVE.set(1.0);
            return;
        }
        pa_monitor::metrics::CIRCUIT_BREAKER_ACTIVE.set(0.0);

        let timer = pa_monitor::metrics::SCAN_LATENCY.start_timer();

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

        timer.observe_duration();
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

        pa_monitor::metrics::OPPORTUNITIES_DETECTED.inc();

        // Pre-trade risk check
        match self.risk_manager.check_pre_trade(opp) {
            RiskDecision::Approve => {}
            RiskDecision::Reject(reason) => {
                tracing::warn!(id = %opp.id, reason = ?reason, "Opportunity rejected by risk manager");
                pa_monitor::metrics::OPPORTUNITIES_REJECTED.inc();
                return;
            }
        }

        // Execute
        let timer = pa_monitor::metrics::EXECUTION_LATENCY.start_timer();
        match self.executor.execute(opp).await {
            Ok(result) => {
                timer.observe_duration();
                tracing::info!(
                    id = %opp.id,
                    status = ?result.status,
                    profit = %result.realized_profit,
                    "Execution complete"
                );
                pa_monitor::metrics::EXECUTIONS_TOTAL.inc();
                // Update realized PnL gauge
                use rust_decimal::prelude::ToPrimitive;
                if let Some(pnl) = result.realized_profit.to_f64() {
                    pa_monitor::metrics::REALIZED_PNL.add(pnl);
                }
                self.risk_manager.update_position(&result);
            }
            Err(e) => {
                timer.observe_duration();
                tracing::error!(id = %opp.id, error = %e, "Execution failed");
                pa_monitor::metrics::EXECUTION_ERRORS.inc();
            }
        }
    }
}
