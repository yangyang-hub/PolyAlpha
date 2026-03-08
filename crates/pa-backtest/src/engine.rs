use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use alloy::primitives::U256;

use pa_core::config::{RiskConfig, StrategyConfig};
use pa_core::traits::{Executor, RiskManager, Strategy};
use pa_core::types::{OrderBook, RiskDecision};

use pa_risk::manager::RiskManagerImpl;

use crate::data_loader::DataLoader;
use crate::report::{BacktestConfig, BacktestResult};
use crate::simulator::TradeSimulator;

/// Backtest engine that replays historical order book data through strategies.
pub struct BacktestEngine {
    loader: DataLoader,
}

impl BacktestEngine {
    pub fn new(loader: DataLoader) -> Self {
        Self { loader }
    }

    /// Run a full backtest over the given configuration.
    ///
    /// 1. Loads markets and snapshots from the database
    /// 2. Builds strategies, risk manager, and simulated executor
    /// 3. Replays each snapshot frame through the strategy pipeline
    /// 4. Returns a comprehensive backtest result with PnL statistics
    pub async fn run(
        &self,
        config: BacktestConfig,
        risk_config: RiskConfig,
        _strategy_config: StrategyConfig,
    ) -> anyhow::Result<BacktestResult> {
        tracing::info!(
            from = %config.from,
            to = %config.to,
            initial_balance = %config.initial_balance,
            "Starting backtest"
        );

        // Step 1: Load markets
        let markets = self.loader.load_all_markets().await?;
        if markets.is_empty() {
            tracing::warn!("No markets found in database");
            return Ok(BacktestResult::build(config, 0, 0, 0, vec![], &[]));
        }

        tracing::info!(
            markets = markets.len(),
            "Markets loaded"
        );

        // Step 2: Load snapshots
        let token_ids = self.loader.snapshot_token_ids(config.from, config.to).await?;
        if token_ids.is_empty() {
            tracing::warn!("No snapshots found in time range");
            return Ok(BacktestResult::build(config, 0, 0, 0, vec![], &[]));
        }

        let frames = self
            .loader
            .load_snapshots(&token_ids, config.from, config.to)
            .await?;
        let total_frames = frames.len();
        tracing::info!(frames = total_frames, "Snapshots loaded, starting replay");

        // Step 3: Build shared order book state
        let shared_books: Arc<RwLock<HashMap<U256, OrderBook>>> =
            Arc::new(RwLock::new(HashMap::new()));

        // Step 4: Build strategies
        // Note: All directional strategies (weather, crypto, convergence) require live API
        // calls and cannot meaningfully replay from DB snapshots.
        // Arbitrage strategies have been removed from the codebase.
        let strategies: Vec<Box<dyn Strategy>> = Vec::new();

        // Step 5: Build risk manager and simulator
        let risk_manager = RiskManagerImpl::new(risk_config);
        let simulator = TradeSimulator::new(config.simulator.clone(), shared_books.clone());

        // Step 6: Replay loop
        let mut all_executions = Vec::new();
        let mut all_strategy_types = Vec::new();
        let mut opportunities_detected: usize = 0;
        let mut opportunities_rejected: usize = 0;

        for (frame_idx, frame) in frames.iter().enumerate() {
            // Update shared order book state with this frame's data
            {
                let mut books = shared_books.write().unwrap();
                for (token_id, book) in &frame.books {
                    books.insert(*token_id, book.clone());
                }
            }

            // Run all strategies
            for strategy in &strategies {
                let opps = match strategy.scan(&markets).await {
                    Ok(opps) => opps,
                    Err(e) => {
                        tracing::debug!(
                            frame = frame_idx,
                            strategy = strategy.name(),
                            error = %e,
                            "Strategy scan error"
                        );
                        continue;
                    }
                };

                for opp in opps {
                    opportunities_detected += 1;

                    // Risk check
                    match risk_manager.check_pre_trade(&opp) {
                        RiskDecision::Approve => {}
                        RiskDecision::Reject(reason) => {
                            tracing::debug!(
                                frame = frame_idx,
                                reason = ?reason,
                                "Opportunity rejected"
                            );
                            opportunities_rejected += 1;
                            continue;
                        }
                    }

                    // Simulate execution
                    match simulator.execute(&opp).await {
                        Ok(result) => {
                            risk_manager.update_position(&result);
                            all_strategy_types.push(opp.strategy_type);
                            all_executions.push(result);
                        }
                        Err(e) => {
                            tracing::debug!(
                                frame = frame_idx,
                                error = %e,
                                "Simulated execution error"
                            );
                        }
                    }
                }
            }

            // Log progress every 1000 frames
            if frame_idx > 0 && frame_idx % 1000 == 0 {
                tracing::info!(
                    frame = frame_idx,
                    total = total_frames,
                    detected = opportunities_detected,
                    executed = all_executions.len(),
                    "Backtest progress"
                );
            }
        }

        tracing::info!(
            frames = total_frames,
            detected = opportunities_detected,
            rejected = opportunities_rejected,
            executed = all_executions.len(),
            "Backtest replay complete"
        );

        Ok(BacktestResult::build(
            config,
            total_frames,
            opportunities_detected,
            opportunities_rejected,
            all_executions,
            &all_strategy_types,
        ))
    }
}
