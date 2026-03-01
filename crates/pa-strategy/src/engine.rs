use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use alloy::primitives::{B256, U256};
use chrono::{TimeDelta, Utc};
use rust_decimal::Decimal;
use pa_core::traits::{Executor, RiskManager, Strategy};
use pa_core::types::{ArbitrageOpportunity, ExecutionPlan, MarketInfo, OrderBook, RiskDecision, StrategyType};
use pa_market_data::event_calendar::EventCalendarService;
use tokio::sync::{RwLock, broadcast};
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
    event_calendar: Option<Arc<EventCalendarService>>,
    /// Order book lookup for depth validation.
    get_orderbook: Box<dyn Fn(U256) -> Option<OrderBook> + Send + Sync>,
    /// Available capital query (balance - exposure). Used for per-cycle budget tracking.
    get_available_capital: Box<dyn Fn() -> Decimal + Send + Sync>,
    /// Minimum order size in USDC; opportunities below this after scaling are rejected.
    min_order_usdc: Decimal,
    /// Only trade markets ending within this many days. None = no filter.
    max_market_end_days: Option<u64>,
    /// Cooldown map: (condition_id, strategy_type) → expiry time.
    /// Prevents retry flooding when the same opportunity is detected repeatedly.
    cooldowns: Mutex<HashMap<(B256, StrategyType), Instant>>,
    /// Global execution pause until this time (e.g. after balance/allowance failure).
    execution_paused_until: Mutex<Instant>,
}

impl StrategyEngine {
    pub fn new(
        strategies: Vec<Box<dyn Strategy>>,
        executor: Arc<dyn Executor>,
        risk_manager: Arc<dyn RiskManager>,
        scan_interval_ms: u64,
        event_calendar: Option<Arc<EventCalendarService>>,
        get_orderbook: Box<dyn Fn(U256) -> Option<OrderBook> + Send + Sync>,
        get_available_capital: Box<dyn Fn() -> Decimal + Send + Sync>,
        min_order_usdc: Decimal,
        max_market_end_days: Option<u64>,
    ) -> Self {
        Self {
            strategies,
            executor,
            risk_manager,
            scan_interval: Duration::from_millis(scan_interval_ms),
            event_calendar,
            get_orderbook,
            get_available_capital,
            min_order_usdc,
            max_market_end_days,
            cooldowns: Mutex::new(HashMap::new()),
            execution_paused_until: Mutex::new(Instant::now()),
        }
    }

    /// Run the trading loop.
    ///
    /// Combines event-driven updates (from WS broadcast) with periodic full scans.
    /// Markets are shared via `Arc<RwLock<...>>` so the periodic market refresh task
    /// can append new markets while the engine is running.
    /// Shuts down gracefully when the `cancel` token is cancelled.
    pub async fn run(
        &self,
        shared_markets: Arc<RwLock<Vec<MarketInfo>>>,
        mut update_rx: broadcast::Receiver<OrderBookUpdate>,
        cancel: CancellationToken,
    ) {
        // Take an initial snapshot of the market list.
        let mut current_markets = shared_markets.read().await.clone();

        tracing::info!(
            strategies = self.strategies.len(),
            markets = current_markets.len(),
            scan_interval_ms = self.scan_interval.as_millis() as u64,
            "Strategy engine starting"
        );

        // Build a lookup: token_id → index into `current_markets`
        let mut token_to_market = build_token_to_market(&current_markets);

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
                                let affected = &current_markets[market_idx..=market_idx];
                                self.scan_and_execute(affected).await;
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!(missed = n, "Strategy engine lagged, doing full scan");
                            self.scan_and_execute(&current_markets).await;
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
                    // Refresh event calendar if needed
                    if let Some(ref ec) = self.event_calendar {
                        ec.refresh_if_needed().await;
                    }
                    // Check if shared market list has been updated by the refresh task
                    {
                        let latest = shared_markets.read().await;
                        if latest.len() != current_markets.len() {
                            current_markets = latest.clone();
                            token_to_market = build_token_to_market(&current_markets);
                            tracing::info!(markets = current_markets.len(), "Engine markets refreshed");
                        }
                    }
                    self.scan_and_execute(&current_markets).await;
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

        // Global execution pause (e.g. after balance/allowance failure)
        if Instant::now() < *self.execution_paused_until.lock().unwrap() {
            tracing::debug!("Execution paused due to recent balance/allowance failure");
            return;
        }

        let timer = pa_monitor::metrics::SCAN_LATENCY.start_timer();

        // Per-cycle budget tracking: query available capital once, then deduct
        // as we commit orders. Prevents over-spending when multiple strategies
        // generate opportunities against the same balance in a single cycle.
        let mut budget_remaining = (self.get_available_capital)();

        // Filter markets by end_date if max_market_end_days is configured.
        // Markets without end_date are always included (weather/crypto don't have end_date).
        // NOTE: expired markets (end_date <= now) are still included so that exit scanning
        // can find held positions and trigger model reversal / capital efficiency exits.
        let filtered: Vec<MarketInfo>;
        let scan_markets: &[MarketInfo] = if let Some(max_days) = self.max_market_end_days {
            let now = Utc::now();
            let cutoff = now + TimeDelta::days(max_days as i64);
            filtered = markets
                .iter()
                .filter(|m| {
                    m.end_date
                        .map(|ed| ed <= cutoff)
                        .unwrap_or(true) // no end_date → include (weather/crypto markets)
                })
                .cloned()
                .collect();
            &filtered
        } else {
            markets
        };

        for strategy in &self.strategies {
            match strategy.scan(scan_markets).await {
                Ok(opportunities) => {
                    if !opportunities.is_empty() {
                        tracing::debug!(
                            strategy = strategy.name(),
                            found = opportunities.len(),
                            "Strategy scan found opportunities"
                        );
                    }
                    for opp in opportunities {
                        // Check execution pause inside opportunity loop too —
                        // a balance/allowance failure on opp N should stop opp N+1
                        if Instant::now() < *self.execution_paused_until.lock().unwrap() {
                            tracing::debug!("Execution paused mid-batch, skipping remaining opportunities");
                            break;
                        }

                        // Skip cooled-down opportunities (prevents retry flooding)
                        if self.is_cooled_down(opp.condition_id, opp.strategy_type) {
                            continue;
                        }

                        // Apply event calendar position filter
                        let opp = if let Some(ref ec) = self.event_calendar {
                            let multiplier = ec.position_multiplier(&opp.question, Utc::now()).await;
                            if multiplier < Decimal::ONE {
                                tracing::debug!(
                                    id = %opp.id, multiplier = %multiplier,
                                    "Event calendar reducing position"
                                );
                                let mut scaled = opp;
                                scaled.size = (scaled.size * multiplier).round_dp(2);
                                scaled.estimated_profit = (scaled.estimated_profit * multiplier).round_dp(4);
                                scale_execution_plan_size(&mut scaled.execution_plan, multiplier);
                                pa_monitor::metrics::EVENT_FILTER_APPLIED.inc();
                                scaled
                            } else {
                                opp
                            }
                        } else {
                            opp
                        };

                        // Validate order book depth (may scale or reject)
                        let opp = match self.validate_depth(&opp) {
                            Some(validated) => validated,
                            None => {
                                self.set_cooldown(opp.condition_id, opp.strategy_type, 10);
                                continue;
                            }
                        };

                        // Budget guard: skip if estimated cost exceeds remaining budget.
                        // Exit orders bypass this — selling held positions doesn't cost USDC.
                        let cost = opp.execution_plan.estimated_cost();
                        if !opp.execution_plan.is_exit() && cost > Decimal::ZERO {
                            if cost > budget_remaining {
                                tracing::debug!(
                                    id = %opp.id,
                                    cost = %cost,
                                    budget_remaining = %budget_remaining,
                                    "Skipping — budget exhausted for this cycle"
                                );
                                self.set_cooldown(opp.condition_id, opp.strategy_type, 10);
                                continue;
                            }
                            // Deduct budget before execution (conservative: assume it will fill).
                            budget_remaining -= cost;
                        }

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
        tracing::debug!(
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
                tracing::debug!(id = %opp.id, reason = ?reason, "Opportunity rejected by risk manager");
                pa_monitor::metrics::OPPORTUNITIES_REJECTED.inc();
                self.set_cooldown(opp.condition_id, opp.strategy_type, 10);
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
                self.set_cooldown(opp.condition_id, opp.strategy_type, 10);
            }
            Err(e) => {
                timer.observe_duration();
                let err_msg = e.to_string();
                tracing::error!(id = %opp.id, error = %err_msg, "Execution failed");
                pa_monitor::metrics::EXECUTION_ERRORS.inc();
                // Pause all execution for 5 minutes on balance/allowance failures
                if err_msg.contains("balance") || err_msg.contains("allowance") {
                    tracing::warn!("Balance/allowance error detected — pausing execution for 5 minutes");
                    *self.execution_paused_until.lock().unwrap() =
                        Instant::now() + Duration::from_secs(300);
                }
                self.set_cooldown(opp.condition_id, opp.strategy_type, 60);
            }
        }
    }

    /// Validate order book depth for an opportunity.
    ///
    /// - Exit orders bypass validation (selling held positions).
    /// - Extracts liquidity requirements from the execution plan.
    /// - Checks available depth and slippage on each leg.
    /// - Scales down the opportunity if depth is insufficient.
    /// - Returns `None` if zero depth or scaled below `min_order_usdc`.
    fn validate_depth(&self, opp: &ArbitrageOpportunity) -> Option<ArbitrageOpportunity> {
        // Exit orders bypass depth validation
        if opp.execution_plan.is_exit() {
            return Some(opp.clone());
        }

        let reqs = opp.execution_plan.liquidity_requirements();
        if reqs.is_empty() {
            return Some(opp.clone());
        }

        let mut min_fill_ratio = Decimal::ONE;

        for req in &reqs {
            let book = match (self.get_orderbook)(req.token_id) {
                Some(b) => b,
                None => {
                    tracing::debug!(
                        id = %opp.id,
                        token_id = %req.token_id,
                        "Depth rejected: no order book"
                    );
                    pa_monitor::metrics::DEPTH_VALIDATION_REJECTED.inc();
                    return None;
                }
            };

            // FOK limit orders can only fill at or better than limit_price,
            // so available_depth at the limit is the exact fillable quantity.
            let depth = book.available_depth(req.side, req.limit_price);
            if depth.is_zero() {
                tracing::debug!(
                    id = %opp.id,
                    token_id = %req.token_id,
                    side = ?req.side,
                    limit_price = %req.limit_price,
                    "Depth rejected: zero depth at limit"
                );
                pa_monitor::metrics::DEPTH_VALIDATION_REJECTED.inc();
                return None;
            }

            let ratio = if req.size > Decimal::ZERO {
                depth / req.size
            } else {
                Decimal::ONE
            };
            if ratio < min_fill_ratio {
                min_fill_ratio = ratio;
            }
        }

        if min_fill_ratio >= Decimal::ONE {
            return Some(opp.clone());
        }

        // Scale down
        let mut scaled = opp.clone();
        scaled.size = (scaled.size * min_fill_ratio).round_dp(2);
        scaled.estimated_profit = (scaled.estimated_profit * min_fill_ratio).round_dp(4);
        scale_execution_plan_size(&mut scaled.execution_plan, min_fill_ratio);

        if scaled.size < self.min_order_usdc {
            tracing::debug!(
                id = %opp.id,
                fill_ratio = %min_fill_ratio,
                scaled_size = %scaled.size,
                "Depth rejected: scaled below min order"
            );
            pa_monitor::metrics::DEPTH_VALIDATION_REJECTED.inc();
            return None;
        }

        tracing::debug!(
            id = %opp.id,
            fill_ratio = %min_fill_ratio,
            original_size = %opp.size,
            scaled_size = %scaled.size,
            "Depth scaling opportunity"
        );
        pa_monitor::metrics::DEPTH_VALIDATION_SCALED.inc();
        Some(scaled)
    }

    /// Check if an opportunity is in cooldown (recently attempted).
    fn is_cooled_down(&self, condition_id: B256, strategy_type: StrategyType) -> bool {
        let cooldowns = self.cooldowns.lock().unwrap();
        cooldowns
            .get(&(condition_id, strategy_type))
            .is_some_and(|until| Instant::now() < *until)
    }

    /// Set a cooldown for an opportunity to prevent retry flooding.
    fn set_cooldown(&self, condition_id: B256, strategy_type: StrategyType, secs: u64) {
        let mut cooldowns = self.cooldowns.lock().unwrap();
        cooldowns.insert(
            (condition_id, strategy_type),
            Instant::now() + Duration::from_secs(secs),
        );
    }
}

/// Build a lookup table mapping each token_id to the index of its parent market.
fn build_token_to_market(markets: &[MarketInfo]) -> HashMap<U256, usize> {
    markets
        .iter()
        .enumerate()
        .flat_map(|(idx, m)| m.tokens.iter().map(move |t| (t.token_id, idx)))
        .collect()
}

/// Scale all size fields within an execution plan by the given multiplier.
fn scale_execution_plan_size(plan: &mut ExecutionPlan, multiplier: Decimal) {
    match plan {
        ExecutionPlan::BuyAndMerge { merge_amount, .. } => {
            *merge_amount = (*merge_amount * multiplier).round_dp(2);
        }
        ExecutionPlan::SplitAndSell { split_amount, .. } => {
            *split_amount = (*split_amount * multiplier).round_dp(2);
        }
        ExecutionPlan::NegRiskArbitrage { amount, legs, .. } => {
            *amount = (*amount * multiplier).round_dp(2);
            for leg in legs {
                leg.size = (leg.size * multiplier).round_dp(2);
            }
        }
        ExecutionPlan::CrossMarket { amount, leg_a, leg_b, .. } => {
            *amount = (*amount * multiplier).round_dp(2);
            leg_a.size = (leg_a.size * multiplier).round_dp(2);
            leg_b.size = (leg_b.size * multiplier).round_dp(2);
        }
        ExecutionPlan::DirectionalBuy { size, .. } => {
            *size = (*size * multiplier).round_dp(2);
        }
    }
}
