use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use alloy::primitives::U256;

use pa_core::config::{RiskConfig, StrategyConfig};
use pa_core::traits::{Executor, RiskManager, Strategy};
use pa_core::types::{NegRiskEvent, OrderBook, RiskDecision};
use pa_market_data::gamma_feed::GammaFeed;
use pa_risk::manager::RiskManagerImpl;
use pa_strategy::cross_market::{CrossMarketArbitrage, detect_cross_market_pairs};
use pa_strategy::neg_risk::NegRiskArbitrage;
use pa_strategy::yes_no::YesNoArbitrage;

use crate::data_loader::DataLoader;
use crate::report::{BacktestConfig, BacktestResult};
use crate::simulator::{SimulatorConfig, TradeSimulator};

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
        strategy_config: StrategyConfig,
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

        // Group NegRisk events
        let neg_risk_events = GammaFeed::group_neg_risk_events(&markets);
        tracing::info!(
            markets = markets.len(),
            neg_risk_events = neg_risk_events.len(),
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

        // Step 4: Build strategies with closures over shared books
        let strategies = Self::build_strategies(
            &strategy_config,
            &config.simulator,
            &neg_risk_events,
            &markets,
            shared_books.clone(),
        );

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
            // Merge: keep previous books for tokens not in this frame
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

    /// Build strategy instances with closures reading from the shared order book state.
    fn build_strategies(
        strategy_config: &StrategyConfig,
        sim_config: &SimulatorConfig,
        neg_risk_events: &[NegRiskEvent],
        markets: &[pa_core::types::MarketInfo],
        shared_books: Arc<RwLock<HashMap<U256, OrderBook>>>,
    ) -> Vec<Box<dyn Strategy>> {
        let mut strategies: Vec<Box<dyn Strategy>> = Vec::new();

        // In backtest, available capital is unlimited (simulation mode)

        // YesNo strategy
        let books_ref = shared_books.clone();
        let yes_no = YesNoArbitrage::new(
            strategy_config.min_spread_bps,
            strategy_config.max_trade_size_usdc,
            strategy_config.min_profit_usdc,
            sim_config.gas_cost_usd,
            Box::new(move |token_id| {
                let books = books_ref.read().ok()?;
                books.get(&token_id).cloned()
            }),
            Box::new(|| rust_decimal::Decimal::MAX),
        );
        strategies.push(Box::new(yes_no));

        // NegRisk strategy (if events exist)
        if !neg_risk_events.is_empty() {
            let books_ref = shared_books.clone();
            let neg_risk = NegRiskArbitrage::new(
                strategy_config.min_spread_bps,
                strategy_config.max_trade_size_usdc,
                strategy_config.min_profit_usdc,
                sim_config.gas_cost_usd,
                neg_risk_events.to_vec(),
                Box::new(move |token_id| {
                    let books = books_ref.read().ok()?;
                    books.get(&token_id).cloned()
                }),
                Box::new(|| rust_decimal::Decimal::MAX),
            );
            strategies.push(Box::new(neg_risk));
        }

        // CrossMarket strategy (if pairs are detected)
        let cross_market_pairs = detect_cross_market_pairs(markets);
        if !cross_market_pairs.is_empty() {
            let books_ref = shared_books.clone();
            let cross_market = CrossMarketArbitrage::new(
                strategy_config.min_spread_bps,
                strategy_config.max_trade_size_usdc,
                strategy_config.min_profit_usdc,
                sim_config.gas_cost_usd * rust_decimal::Decimal::TWO, // 2x gas
                cross_market_pairs,
                Box::new(move |token_id| {
                    let books = books_ref.read().ok()?;
                    books.get(&token_id).cloned()
                }),
                Box::new(|| rust_decimal::Decimal::MAX),
            );
            strategies.push(Box::new(cross_market));
        }

        // Note: WeatherAlphaStrategy requires live Open-Meteo API calls and cannot
        // meaningfully replay historical forecasts from DB snapshots alone.
        // Weather strategy (both binary and NegRisk modes) is excluded from backtest.

        strategies
    }
}
