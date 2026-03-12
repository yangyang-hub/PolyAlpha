use alloy::primitives::{B256, U256};
use chrono::{TimeDelta, Utc};
use pa_core::traits::{Executor, RiskManager, Strategy};
use pa_core::types::{
    ExecutionPlan, MarketInfo, OrderBook, RiskDecision, StrategyType, TradeSide, TradingOpportunity,
};
use pa_market_data::event_calendar::EventCalendarService;
use crate::profitability::ProfitCalculator;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::sync::{RwLock, broadcast};
use tokio::time::{Duration, interval};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use pa_market_data::ws_feed::OrderBookUpdate;

/// Event-driven strategy engine that orchestrates scanning, risk checks, and execution.
///
/// Operates in two modes simultaneously:
/// 1. **Event-driven**: reacts to `OrderBookUpdate` events from the WebSocket feed
/// 2. **Periodic fallback**: full market scan on a timer to catch any missed events
///
/// Position snapshot entry for the universal stop-loss scanner.
pub struct StopLossPosition {
    pub token_id: U256,
    pub size: Decimal,
    pub avg_cost: Decimal,
    pub strategy_type: Option<StrategyType>,
    pub condition_id: Option<B256>,
}

pub struct StrategyEngineDeps {
    pub get_orderbook: Box<dyn Fn(U256) -> Option<OrderBook> + Send + Sync>,
    pub get_available_capital: Box<dyn Fn() -> Decimal + Send + Sync>,
    pub get_all_positions: Box<dyn Fn() -> Vec<StopLossPosition> + Send + Sync>,
}

pub struct StrategyEngineOptions {
    pub scan_interval_ms: u64,
    pub event_calendar: Option<Arc<EventCalendarService>>,
    pub min_order_usdc: Decimal,
    pub max_market_end_days: Option<u64>,
}

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
    /// Get ALL positions for universal stop-loss scanning.
    get_all_positions: Box<dyn Fn() -> Vec<StopLossPosition> + Send + Sync>,
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
    fn cost_precision_step(price: Decimal) -> (u64, u64, u64) {
        use rust_decimal::prelude::ToPrimitive;

        let scale = price.scale();
        let denom = 10u64.pow(scale);
        let numer = (price * Decimal::from(denom)).round().to_u64().unwrap_or(1);
        if numer == 0 {
            return (1, 0, denom);
        }
        let g = gcd_u64(numer, denom);
        let s_step = denom / g;
        (s_step, numer, denom)
    }

    fn adjust_size_for_cost_precision(price: Decimal, size: Decimal) -> Decimal {
        use rust_decimal::prelude::ToPrimitive;

        let cost = price * size;
        if cost == cost.round_dp(2) {
            return size;
        }

        let (s_step, numer, _) = Self::cost_precision_step(price);
        if numer == 0 {
            return size;
        }

        let s_val = (size * Decimal::from(100u64)).round().to_u64().unwrap_or(0);
        if s_step == 0 || s_val < s_step {
            return Decimal::ZERO;
        }

        let s_rounded = (s_val / s_step) * s_step;
        Decimal::new(s_rounded as i64, 2)
    }

    fn min_cost_adjusted_size(price: Decimal) -> Decimal {
        if price <= Decimal::ZERO {
            return Decimal::ZERO;
        }

        let (s_step, numer, denom) = Self::cost_precision_step(price);
        if numer == 0 || s_step == 0 {
            return Decimal::ZERO;
        }

        let target = denom * 100;
        let min_s = (target + numer - 1) / numer;
        let s = ((min_s + s_step - 1) / s_step) * s_step;
        if s > 5000 {
            return Decimal::ZERO;
        }

        Decimal::new(s as i64, 2)
    }

    fn is_executable_directional_order(side: TradeSide, price: Decimal, size: Decimal) -> bool {
        if price <= Decimal::ZERO || size <= Decimal::ZERO {
            return false;
        }

        match side {
            TradeSide::Buy => {
                let requested_size = size.round_dp(2);
                let mut adjusted = Self::adjust_size_for_cost_precision(price, requested_size);
                if adjusted <= Decimal::ZERO {
                    let min_size = Self::min_cost_adjusted_size(price);
                    let max_cost = requested_size * price * Decimal::from(5u32);
                    if min_size > Decimal::ZERO && min_size * price <= max_cost {
                        adjusted = min_size;
                    } else {
                        return false;
                    }
                }

                if price * adjusted < Decimal::ONE {
                    let bumped = Self::min_cost_adjusted_size(price);
                    if bumped > Decimal::ZERO && bumped <= requested_size.max(adjusted) {
                        adjusted = bumped;
                    }
                }

                price * adjusted >= Decimal::ONE
            }
            TradeSide::Sell => {
                let requested_size = size.round_dp(2);
                let adjusted = Self::adjust_size_for_cost_precision(price, requested_size);
                adjusted > Decimal::ZERO
            }
        }
    }

    fn strategy_metric_label(strategy: StrategyType) -> &'static str {
        match strategy {
            StrategyType::Weather => "weather",
            StrategyType::CryptoAlpha => "crypto_alpha",
            StrategyType::LiquidityRewards => "liquidity_rewards",
            StrategyType::SmartMoney => "smart_money",
        }
    }

    fn refresh_estimated_profit(
        original: &TradingOpportunity,
        adjusted: &mut TradingOpportunity,
        fee_rate_bps: u32,
    ) -> bool {
        let calc = ProfitCalculator::new(Decimal::ZERO);

        let (
            ExecutionPlan::DirectionalBuy {
                side: original_side,
                price: original_price,
                size: original_size,
                ..
            },
            ExecutionPlan::DirectionalBuy {
                side: adjusted_side,
                price: adjusted_price,
                size: adjusted_size,
                ..
            },
        ) = (&original.execution_plan, &adjusted.execution_plan);

        if original_size <= &Decimal::ZERO || adjusted_size <= &Decimal::ZERO || original_side != adjusted_side
        {
            return false;
        }

        match adjusted_side {
            TradeSide::Buy => {
                let original_fee = calc.capped_fee(*original_price, fee_rate_bps);
                let implied_prob =
                    *original_price + original_fee + (original.estimated_profit / *original_size);
                let est = calc.directional_buy_profit(
                    *adjusted_price,
                    implied_prob,
                    *adjusted_size,
                    fee_rate_bps,
                );
                adjusted.estimated_profit = est.net_profit;
                adjusted.spread = implied_prob - *adjusted_price;
                est.net_profit > Decimal::ZERO
            }
            TradeSide::Sell => {
                let original_fee = calc.capped_fee(*original_price, fee_rate_bps);
                let implied_avg_cost =
                    *original_price - original_fee - (original.estimated_profit / *original_size);
                let est = calc.directional_sell_profit(
                    *adjusted_price,
                    implied_avg_cost,
                    *adjusted_size,
                    fee_rate_bps,
                );
                adjusted.estimated_profit = est.net_profit;
                adjusted.spread = *adjusted_price - implied_avg_cost;
                true
            }
        }
    }

    fn validate_execution_freshness(
        &self,
        opp: &TradingOpportunity,
        markets: &[MarketInfo],
    ) -> Option<TradingOpportunity> {
        let mut adjusted = opp.clone();
        let strategy_label = Self::strategy_metric_label(opp.strategy_type);

        match &mut adjusted.execution_plan {
            ExecutionPlan::DirectionalBuy {
                token_id,
                side,
                price,
                size,
                ..
            } => {
                let book = match (self.get_orderbook)(*token_id) {
                    Some(book) => book,
                    None => {
                        tracing::debug!(id = %opp.id, token_id = %token_id, "Freshness rejected: no order book");
                        pa_monitor::metrics::DEPTH_VALIDATION_REJECTED.inc();
                        pa_monitor::metrics::EXECUTION_FRESHNESS_REJECTIONS
                            .with_label_values(&[strategy_label, "missing_orderbook"])
                            .inc();
                        return None;
                    }
                };

                match side {
                    TradeSide::Buy => {
                        let best_ask = match book.best_ask() {
                            Some(level) => level.price,
                            None => {
                                tracing::debug!(id = %opp.id, token_id = %token_id, "Freshness rejected: no ask liquidity");
                                pa_monitor::metrics::DEPTH_VALIDATION_REJECTED.inc();
                                pa_monitor::metrics::EXECUTION_FRESHNESS_REJECTIONS
                                    .with_label_values(&[strategy_label, "missing_ask"])
                                    .inc();
                                return None;
                            }
                        };

                        if best_ask > *price {
                            tracing::debug!(
                                id = %opp.id,
                                token_id = %token_id,
                                original_limit = %price,
                                current_best_ask = %best_ask,
                                "Freshness rejected: ask moved above limit"
                            );
                            pa_monitor::metrics::DEPTH_VALIDATION_REJECTED.inc();
                            pa_monitor::metrics::EXECUTION_FRESHNESS_REJECTIONS
                                .with_label_values(&[strategy_label, "ask_above_limit"])
                                .inc();
                            return None;
                        }

                        let walk = match book.walk_book(TradeSide::Buy, *size) {
                            Some(walk) if walk.filled >= *size && walk.worst_price <= *price => walk,
                            _ => {
                                tracing::debug!(
                                    id = %opp.id,
                                    token_id = %token_id,
                                    size = %size,
                                    limit = %price,
                                    "Freshness rejected: insufficient executable ask depth"
                                );
                                pa_monitor::metrics::DEPTH_VALIDATION_REJECTED.inc();
                                pa_monitor::metrics::EXECUTION_FRESHNESS_REJECTIONS
                                    .with_label_values(&[strategy_label, "insufficient_ask_depth"])
                                    .inc();
                                return None;
                            }
                        };
                        let _ = walk;
                        if !Self::is_executable_directional_order(TradeSide::Buy, *price, *size) {
                            tracing::debug!(
                                id = %opp.id,
                                token_id = %token_id,
                                price = %price,
                                size = %size,
                                "Freshness rejected: buy order not executable after lot-size normalization"
                            );
                            pa_monitor::metrics::DEPTH_VALIDATION_REJECTED.inc();
                            pa_monitor::metrics::EXECUTION_FRESHNESS_REJECTIONS
                                .with_label_values(&[strategy_label, "buy_lot_size_invalid"])
                                .inc();
                            return None;
                        }
                    }
                    TradeSide::Sell => {
                        let best_bid = match book.best_bid() {
                            Some(level) => level.price,
                            None => {
                                tracing::debug!(id = %opp.id, token_id = %token_id, "Freshness rejected: no bid liquidity");
                                pa_monitor::metrics::DEPTH_VALIDATION_REJECTED.inc();
                                pa_monitor::metrics::EXECUTION_FRESHNESS_REJECTIONS
                                    .with_label_values(&[strategy_label, "missing_bid"])
                                    .inc();
                                return None;
                            }
                        };

                        let bid_depth = book.available_depth(TradeSide::Sell, best_bid);
                        let capped_size = (*size).min(bid_depth).round_dp(2);
                        if capped_size < dec!(0.01) {
                            tracing::debug!(
                                id = %opp.id,
                                token_id = %token_id,
                                best_bid = %best_bid,
                                bid_depth = %bid_depth,
                                "Freshness rejected: no sellable bid depth"
                            );
                            pa_monitor::metrics::DEPTH_VALIDATION_REJECTED.inc();
                            pa_monitor::metrics::EXECUTION_FRESHNESS_REJECTIONS
                                .with_label_values(&[strategy_label, "insufficient_bid_depth"])
                                .inc();
                            return None;
                        }

                        if capped_size < *size {
                            tracing::debug!(
                                id = %opp.id,
                                token_id = %token_id,
                                original_size = %size,
                                capped_size = %capped_size,
                                "Freshness scaling exit to current bid depth"
                            );
                            pa_monitor::metrics::EXECUTION_FRESHNESS_SCALED
                                .with_label_values(&[strategy_label, "sell"])
                                .inc();
                            *size = capped_size;
                            adjusted.size = capped_size;
                        }

                        *price = best_bid;
                        if !Self::is_executable_directional_order(TradeSide::Sell, *price, *size) {
                            tracing::debug!(
                                id = %opp.id,
                                token_id = %token_id,
                                price = %price,
                                size = %size,
                                "Freshness rejected: sell order not executable after lot-size normalization"
                            );
                            pa_monitor::metrics::DEPTH_VALIDATION_REJECTED.inc();
                            pa_monitor::metrics::EXECUTION_FRESHNESS_REJECTIONS
                                .with_label_values(&[strategy_label, "sell_lot_size_invalid"])
                                .inc();
                            return None;
                        }
                    }
                }
            }
        }

        let fee_rate_bps = markets
            .iter()
            .find(|market| market.condition_id == adjusted.condition_id)
            .map(|market| market.fee_rate_bps)
            .unwrap_or(200);

        if !Self::refresh_estimated_profit(opp, &mut adjusted, fee_rate_bps) {
            tracing::debug!(
                id = %opp.id,
                condition_id = %opp.condition_id,
                "Freshness rejected: estimated profit no longer positive"
            );
            pa_monitor::metrics::DEPTH_VALIDATION_REJECTED.inc();
            pa_monitor::metrics::EXECUTION_FRESHNESS_REJECTIONS
                .with_label_values(&[strategy_label, "non_positive_profit"])
                .inc();
            return None;
        }

        Some(adjusted)
    }

    pub fn new(
        strategies: Vec<Box<dyn Strategy>>,
        executor: Arc<dyn Executor>,
        risk_manager: Arc<dyn RiskManager>,
        deps: StrategyEngineDeps,
        options: StrategyEngineOptions,
    ) -> Self {
        let StrategyEngineDeps {
            get_orderbook,
            get_available_capital,
            get_all_positions,
        } = deps;

        let StrategyEngineOptions {
            scan_interval_ms,
            event_calendar,
            min_order_usdc,
            max_market_end_days,
        } = options;

        Self {
            strategies,
            executor,
            risk_manager,
            scan_interval: Duration::from_millis(scan_interval_ms),
            event_calendar,
            get_orderbook,
            get_available_capital,
            get_all_positions,
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
                    m.end_date.map(|ed| ed <= cutoff).unwrap_or(true) // no end_date → include (weather/crypto markets)
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
                            tracing::debug!(
                                "Execution paused mid-batch, skipping remaining opportunities"
                            );
                            break;
                        }

                        // Skip cooled-down opportunities (prevents retry flooding)
                        if self.is_cooled_down(opp.condition_id, opp.strategy_type) {
                            continue;
                        }

                        // Apply event calendar position filter
                        let opp = if let Some(ref ec) = self.event_calendar {
                            let multiplier =
                                ec.position_multiplier(&opp.question, Utc::now()).await;
                            if multiplier < Decimal::ONE {
                                tracing::debug!(
                                    id = %opp.id, multiplier = %multiplier,
                                    "Event calendar reducing position"
                                );
                                let mut scaled = opp;
                                scaled.size = (scaled.size * multiplier).round_dp(2);
                                scaled.estimated_profit =
                                    (scaled.estimated_profit * multiplier).round_dp(4);
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
                                self.set_cooldown(opp.condition_id, opp.strategy_type, 60);
                                continue;
                            }
                        };

                        let opp = match self.validate_execution_freshness(&opp, markets) {
                            Some(validated) => validated,
                            None => {
                                self.set_cooldown(opp.condition_id, opp.strategy_type, 30);
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
                                self.set_cooldown(opp.condition_id, opp.strategy_type, 120);
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

        // ── Universal stop-loss safety net ──
        // Scans ALL held positions (regardless of strategy_type) for deep losses.
        // This catches positions that strategies can't exit due to:
        //   - strategy_type = None (inference failed)
        //   - parse failure in scan_exits (e.g. NegRisk question format)
        //   - market not in shared_markets
        // Uses FULL (unfiltered) market list so safety checks work for all positions,
        // including markets beyond max_market_end_days.
        self.scan_stop_loss(markets).await;

        timer.observe_duration();
    }

    async fn process_opportunity(&self, opp: &TradingOpportunity) {
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
        let is_exit = opp.execution_plan.is_exit();
        let timer = pa_monitor::metrics::EXECUTION_LATENCY.start_timer();
        match self.executor.execute(opp).await {
            Ok(result) => {
                timer.observe_duration();
                tracing::info!(
                    id = %opp.id,
                    status = ?result.status,
                    profit = %result.realized_profit,
                    is_exit,
                    "Execution complete"
                );
                pa_monitor::metrics::EXECUTIONS_TOTAL.inc();
                if is_exit {
                    pa_monitor::metrics::EXIT_TRADES.inc();
                }
                // Update realized PnL gauge
                use rust_decimal::prelude::ToPrimitive;
                if let Some(pnl) = result.realized_profit.to_f64() {
                    pa_monitor::metrics::REALIZED_PNL.add(pnl);
                }
                self.risk_manager.update_position(&result);
                // Longer cooldown for NoFill/Failed (market conditions unlikely to change soon)
                if result.status == pa_core::types::ExecutionStatus::NoFill
                    || result.status == pa_core::types::ExecutionStatus::Failed
                {
                    self.set_cooldown(opp.condition_id, opp.strategy_type, 120);
                } else {
                    self.set_cooldown(opp.condition_id, opp.strategy_type, 10);
                }
            }
            Err(e) => {
                timer.observe_duration();
                let err_msg = e.to_string();
                tracing::error!(id = %opp.id, error = %err_msg, "Execution failed");
                pa_monitor::metrics::EXECUTION_ERRORS.inc();
                // Pause all execution for 5 minutes on balance/allowance failures
                if err_msg.contains("balance") || err_msg.contains("allowance") {
                    tracing::warn!(
                        "Balance/allowance error detected — pausing execution for 5 minutes"
                    );
                    *self.execution_paused_until.lock().unwrap() =
                        Instant::now() + Duration::from_secs(300);
                }
                // Lot-size errors won't resolve until price or Kelly sizing changes
                // significantly — use a longer cooldown to avoid retry spam.
                if err_msg.contains("lot size") {
                    self.set_cooldown(opp.condition_id, opp.strategy_type, 600);
                } else if err_msg.contains("does not exist") {
                    // Orderbook removed — market is closed/resolved permanently.
                    self.set_cooldown(opp.condition_id, opp.strategy_type, 86400);
                } else {
                    self.set_cooldown(opp.condition_id, opp.strategy_type, 60);
                }
            }
        }
    }

    /// Universal stop-loss safety net: exit any position that has lost >= 50% of cost basis.
    ///
    /// This runs AFTER strategy-specific exit scanning. It catches positions that
    /// strategies can't handle (untagged, unparseable, missing from market list).
    ///
    /// Safety checks to avoid selling winning positions at low prices:
    /// 1. Skip expired markets (let auto-redeem handle them — winning tokens redeem at $1.00)
    /// 2. Cross-validate with counter-token: if the OTHER side also has low bids,
    ///    the market is likely stale/illiquid — skip to avoid acting on bad data.
    ///    If the OTHER side has high bids (>0.80), our side is genuinely losing.
    /// 3. Skip positions too small to sell on CLOB.
    async fn scan_stop_loss(&self, markets: &[MarketInfo]) {
        let positions = (self.get_all_positions)();
        if positions.is_empty() {
            return;
        }

        // Log position diagnostics (debug level; enable with RUST_LOG=pa_strategy::engine=debug)
        for pos in &positions {
            if pos.size <= Decimal::ZERO {
                continue;
            }
            let bid =
                (self.get_orderbook)(pos.token_id).and_then(|b| b.best_bid().map(|l| l.price));
            let ask =
                (self.get_orderbook)(pos.token_id).and_then(|b| b.best_ask().map(|l| l.price));
            let stop_threshold = pos.avg_cost * dec!(0.50);
            let triggered = bid.map(|b| b < stop_threshold).unwrap_or(false);
            tracing::debug!(
                token_id = %pos.token_id,
                size = %pos.size,
                avg_cost = %pos.avg_cost,
                best_bid = ?bid,
                best_ask = ?ask,
                stop_threshold = %stop_threshold,
                triggered = triggered,
                strategy = ?pos.strategy_type,
                "[STOP-LOSS] Position scan"
            );
        }

        // Build lookups from markets
        let token_to_market: HashMap<U256, &MarketInfo> = markets
            .iter()
            .flat_map(|m| m.tokens.iter().map(move |t| (t.token_id, m)))
            .collect();

        for pos in &positions {
            if pos.size <= Decimal::ZERO || pos.avg_cost <= Decimal::ZERO {
                continue;
            }

            // Early cooldown check: skip all processing for cooled-down positions.
            // This prevents redundant orderbook lookups, safety checks, and log spam.
            let early_condition_id = pos.condition_id.unwrap_or(B256::ZERO);
            let early_st = pos.strategy_type.unwrap_or(StrategyType::CryptoAlpha);
            if self.is_cooled_down(early_condition_id, early_st) {
                continue;
            }

            let book = match (self.get_orderbook)(pos.token_id) {
                Some(b) => b,
                None => {
                    tracing::debug!(
                        token_id = %pos.token_id,
                        "[STOP-LOSS] No orderbook for position, skipping"
                    );
                    continue;
                }
            };
            let best_bid = match book.best_bid() {
                Some(b) => b.price,
                None => {
                    tracing::debug!(
                        token_id = %pos.token_id,
                        "[STOP-LOSS] No bids in orderbook, skipping"
                    );
                    continue;
                }
            };

            // Stop-loss: exit when best_bid < 50% of avg_cost (lost >= 50%)
            let stop_threshold = pos.avg_cost * dec!(0.50);
            if best_bid >= stop_threshold {
                continue;
            }

            // Safety check 0: If best_ask >= avg_cost AND the spread is reasonable,
            // the market still values this token above our cost basis. The low best_bid
            // is just thin buy-side liquidity (common in illiquid weather/niche markets).
            // e.g. best_bid=$0.01 (bot), best_ask=$0.77 (real price), avg_cost=$0.63
            //
            // BUT: only trust ask when bid is at least 10% of ask. If the spread is
            // extremely wide (bid < 10% of ask), the ask is likely stale while the bid
            // reflects real demand. Cross-validation (Safety check 2) provides a second
            // layer of protection for genuinely profitable wide-spread positions.
            let best_ask = book.best_ask().map(|a| a.price);
            if let Some(ask) = best_ask {
                if ask >= pos.avg_cost && best_bid >= ask * dec!(0.10) {
                    tracing::debug!(
                        token_id = %pos.token_id,
                        best_bid = %best_bid,
                        best_ask = %ask,
                        avg_cost = %pos.avg_cost,
                        "[STOP-LOSS] Skipping — best_ask >= avg_cost with reasonable spread"
                    );
                    continue;
                }
                if ask >= pos.avg_cost && best_bid < ask * dec!(0.10) {
                    tracing::debug!(
                        token_id = %pos.token_id,
                        best_bid = %best_bid,
                        best_ask = %ask,
                        avg_cost = %pos.avg_cost,
                        "[STOP-LOSS] Wide spread — ask >= avg_cost but bid < 10% of ask, not trusting ask"
                    );
                    // Fall through to further safety checks
                }
            }

            // Look up the market for this token
            let market = token_to_market.get(&pos.token_id).copied();

            // Safety check 1: Skip expired markets — auto-redeem handles them.
            // Winning tokens in resolved markets have low bids because nobody trades them,
            // but they redeem at $1.00. Selling at $0.01 would be catastrophic.
            //
            // EXCEPTION: If best_bid is very low (< $0.10), the token is likely the LOSING
            // side. Auto-redeem won't help (losing tokens redeem at $0.00). In this case,
            // selling at the current bid salvages some value.
            if let Some(m) = market
                && let Some(end_date) = m.end_date
                && end_date <= chrono::Utc::now()
            {
                // If price is very low, this is likely a losing position.
                // Auto-redeem won't help — try to sell to salvage whatever we can.
                if best_bid >= dec!(0.10) {
                    let condition_id = pos.condition_id.unwrap_or(m.condition_id);
                    let st = pos.strategy_type.unwrap_or(StrategyType::CryptoAlpha);
                    if !self.is_cooled_down(condition_id, st) {
                        tracing::info!(
                            token_id = %pos.token_id,
                            end_date = %end_date,
                            best_bid = %best_bid,
                            "[STOP-LOSS] Skipping expired market (bid >= $0.10) — auto-redeem will handle"
                        );
                        self.set_cooldown(condition_id, st, 3600); // 1 hour
                    }
                    continue;
                }
                // Low-priced expired position — likely losing side, try to sell
                tracing::info!(
                    token_id = %pos.token_id,
                    end_date = %end_date,
                    best_bid = %best_bid,
                    size = %pos.size,
                    "[STOP-LOSS] Expired market with low price — likely losing side, attempting sell"
                );
                // Fall through to sell logic below
            }

            // Safety check 2: Cross-validate with counter-token.
            // In a binary market (YES/NO), if our token has low bids, check the OTHER token.
            // If the other side has high bids (>0.50), our side is genuinely losing.
            // If the other side ALSO has low bids, data is likely stale — don't act.
            //
            // IMPORTANT: "counter-token not subscribed" (no orderbook at all) is DIFFERENT
            // from "counter-token has orderbook with low bids". When a token is not subscribed,
            // it's typically because WS filtering excluded it as extreme price (>0.95),
            // which actually CONFIRMS our side is losing. Only block when both sides
            // have actual orderbook data with low bids.
            if let Some(m) = market
                && m.tokens.len() == 2
            {
                let other_token = m
                    .tokens
                    .iter()
                    .find(|t| t.token_id != pos.token_id)
                    .map(|t| t.token_id);
                if let Some(other_id) = other_token {
                    let other_book = (self.get_orderbook)(other_id);
                    match other_book {
                        Some(book) => {
                            let other_bid =
                                book.best_bid().map(|l| l.price).unwrap_or(Decimal::ZERO);
                            if other_bid < dec!(0.50) {
                                // Both sides have orderbook data but low bids — likely stale.
                                let condition_id = pos.condition_id.unwrap_or(m.condition_id);
                                let st = pos.strategy_type.unwrap_or(StrategyType::CryptoAlpha);
                                let sell_value = best_bid * pos.size;
                                tracing::info!(
                                    token_id = %pos.token_id,
                                    our_bid = %best_bid,
                                    other_bid = %other_bid,
                                    avg_cost = %pos.avg_cost,
                                    size = %pos.size,
                                    sell_value = %sell_value,
                                    "[STOP-LOSS] Both sides low bids — market illiquid/stale, cannot sell (will retry in 10m)"
                                );
                                self.set_cooldown(condition_id, st, 600); // 10 min
                                continue;
                            }
                            // Counter-token has high bids → our side is genuinely losing.
                            // Fall through to sell.
                        }
                        None => {
                            // Counter-token not in orderbook cache (not subscribed).
                            // This typically means WS filtering excluded it as extreme price
                            // (>0.95 or <0.05), which confirms our side is on the losing end.
                            tracing::info!(
                                token_id = %pos.token_id,
                                our_bid = %best_bid,
                                avg_cost = %pos.avg_cost,
                                size = %pos.size,
                                "[STOP-LOSS] Counter-token not subscribed (likely extreme price) — proceeding"
                            );
                            // Fall through to sell.
                        }
                    }
                }
            }

            // Determine condition_id
            let condition_id = pos
                .condition_id
                .or_else(|| market.map(|m| m.condition_id))
                .unwrap_or(B256::ZERO);

            // Use a synthetic strategy_type for cooldown tracking
            let st = pos.strategy_type.unwrap_or(StrategyType::CryptoAlpha);

            // Check cooldown to avoid retry flooding
            if self.is_cooled_down(condition_id, st) {
                continue;
            }

            // Skip positions that are too small to sell on CLOB.
            let sell_value = best_bid * pos.size;
            if sell_value < dec!(0.05) {
                tracing::info!(
                    token_id = %pos.token_id,
                    size = %pos.size,
                    best_bid = %best_bid,
                    sell_value = %sell_value,
                    "[STOP-LOSS] Position too small to sell on CLOB — waiting for market resolution"
                );
                self.set_cooldown(condition_id, st, 3600); // 1 hour
                continue;
            }

            // Cap sell size to available bid depth.
            // FOK requires the full order to be filled. If our position exceeds available
            // bid depth, the FOK will be killed. Instead, sell what the book can absorb
            // and come back for the rest on the next scan.
            let bid_depth = book.available_depth(TradeSide::Sell, best_bid);
            let sell_size = pos.size.min(bid_depth).round_dp(2);
            if sell_size < dec!(0.01) {
                tracing::debug!(
                    token_id = %pos.token_id,
                    best_bid = %best_bid,
                    bid_depth = %bid_depth,
                    pos_size = %pos.size,
                    "[STOP-LOSS] No bid liquidity — cannot sell, waiting"
                );
                self.set_cooldown(condition_id, st, 3600); // 1 hour
                continue;
            }

            tracing::warn!(
                token_id = %pos.token_id,
                size = %pos.size,
                sell_size = %sell_size,
                avg_cost = %pos.avg_cost,
                best_bid = %best_bid,
                loss_pct = %(((pos.avg_cost - best_bid) / pos.avg_cost * dec!(100)).round_dp(1)),
                strategy = ?pos.strategy_type,
                "[STOP-LOSS] Forced exit — position lost >= 50%"
            );

            let opp = TradingOpportunity {
                id: Uuid::new_v4(),
                condition_id,
                question: format!("[STOP-LOSS] Force exit token {}", pos.token_id),
                strategy_type: st,
                spread: pos.avg_cost - best_bid,
                size: sell_size,
                estimated_profit: (best_bid - pos.avg_cost) * sell_size, // negative = loss
                detected_at: chrono::Utc::now(),
                execution_plan: ExecutionPlan::DirectionalBuy {
                    token_id: pos.token_id,
                    side: TradeSide::Sell,
                    price: best_bid,
                    size: sell_size,
                    condition_id,
                },
            };

            pa_monitor::metrics::EXIT_TRADES.inc();
            self.process_opportunity(&opp).await;
            // Long cooldown for stop-loss to avoid spam (5 minutes)
            self.set_cooldown(condition_id, st, 300);
        }
    }

    /// Validate order book depth for an opportunity.
    ///
    /// - Exit orders bypass validation (selling held positions).
    /// - Extracts liquidity requirements from the execution plan.
    /// - Checks available depth and slippage on each leg.
    /// - Scales down the opportunity if depth is insufficient.
    /// - Returns `None` if zero depth or scaled below `min_order_usdc`.
    fn validate_depth(&self, opp: &TradingOpportunity) -> Option<TradingOpportunity> {
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
        // Prune expired entries periodically to prevent unbounded growth.
        // Only prune when the map exceeds 500 entries (amortized O(1)).
        if cooldowns.len() > 500 {
            let now = Instant::now();
            cooldowns.retain(|_, until| *until > now);
        }
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
        ExecutionPlan::DirectionalBuy { size, .. } => {
            *size = (*size * multiplier).round_dp(2);
        }
    }
}

fn gcd_u64(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        let t = b;
        b = a % b;
        a = t;
    }
    a
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::B256;
    use async_trait::async_trait;
    use pa_core::types::{ExecutionResult, ExecutionStatus, OrderBook, PriceLevel, TradeRecord, TxType};
    use rust_decimal_macros::dec;
    use std::collections::HashMap;

    struct NoopStrategy;

    #[async_trait]
    impl Strategy for NoopStrategy {
        fn name(&self) -> &str {
            "noop"
        }

        fn strategy_type(&self) -> StrategyType {
            StrategyType::Weather
        }

        async fn scan(&self, _markets: &[MarketInfo]) -> pa_core::Result<Vec<TradingOpportunity>> {
            Ok(vec![])
        }
    }

    struct NoopExecutor;

    #[async_trait]
    impl Executor for NoopExecutor {
        async fn execute(&self, _opportunity: &TradingOpportunity) -> pa_core::Result<ExecutionResult> {
            Ok(ExecutionResult {
                opportunity_id: Uuid::now_v7(),
                strategy_type: StrategyType::Weather,
                status: ExecutionStatus::Success,
                trades: vec![TradeRecord {
                    id: Uuid::now_v7(),
                    token_id: U256::from(1u64),
                    condition_id: B256::ZERO,
                    side: TradeSide::Buy,
                    price: dec!(0.10),
                    size: dec!(1),
                    filled_size: dec!(1),
                    fee: Decimal::ZERO,
                    tx_type: TxType::ClobOrder,
                    tx_hash: None,
                }],
                realized_profit: Decimal::ZERO,
                total_fees: Decimal::ZERO,
                total_gas: Decimal::ZERO,
                executed_at: Utc::now(),
            })
        }

        async fn cancel_all(&self) -> pa_core::Result<()> {
            Ok(())
        }
    }

    struct NoopRiskManager;

    impl RiskManager for NoopRiskManager {
        fn check_pre_trade(&self, _opportunity: &TradingOpportunity) -> RiskDecision {
            RiskDecision::Approve
        }

        fn update_position(&self, _result: &ExecutionResult) {}

        fn is_circuit_broken(&self) -> bool {
            false
        }

        fn total_exposure(&self) -> Decimal {
            Decimal::ZERO
        }

        fn reset_daily(&self) {}
    }

    fn make_engine(books: HashMap<U256, OrderBook>) -> StrategyEngine {
        let books = Arc::new(books);
        StrategyEngine::new(
            vec![Box::new(NoopStrategy)],
            Arc::new(NoopExecutor),
            Arc::new(NoopRiskManager),
            StrategyEngineDeps {
                get_orderbook: Box::new(move |token_id| books.get(&token_id).cloned()),
                get_available_capital: Box::new(|| dec!(1000)),
                get_all_positions: Box::new(Vec::new),
            },
            StrategyEngineOptions {
                scan_interval_ms: 1000,
                event_calendar: None,
                min_order_usdc: dec!(1),
                max_market_end_days: None,
            },
        )
    }

    fn make_book(token_id: U256, bids: &[(Decimal, Decimal)], asks: &[(Decimal, Decimal)]) -> OrderBook {
        OrderBook {
            token_id,
            bids: bids
                .iter()
                .map(|(price, size)| PriceLevel {
                    price: *price,
                    size: *size,
                })
                .collect(),
            asks: asks
                .iter()
                .map(|(price, size)| PriceLevel {
                    price: *price,
                    size: *size,
                })
                .collect(),
            timestamp: Utc::now(),
        }
    }

    fn make_opp(side: TradeSide, price: Decimal, size: Decimal) -> TradingOpportunity {
        TradingOpportunity {
            id: Uuid::now_v7(),
            strategy_type: StrategyType::Weather,
            condition_id: B256::ZERO,
            question: "test".into(),
            spread: dec!(0.10),
            estimated_profit: dec!(1),
            size,
            detected_at: Utc::now(),
            execution_plan: ExecutionPlan::DirectionalBuy {
                token_id: U256::from(1u64),
                side,
                price,
                size,
                condition_id: B256::ZERO,
            },
        }
    }

    #[test]
    fn test_validate_execution_freshness_rejects_buy_if_ask_moved_above_limit() {
        let engine = make_engine(HashMap::from([(
            U256::from(1u64),
            make_book(U256::from(1u64), &[(dec!(0.09), dec!(10))], &[(dec!(0.12), dec!(10))]),
        )]));

        let opp = make_opp(TradeSide::Buy, dec!(0.10), dec!(5));
        let validated = engine.validate_execution_freshness(&opp, &[]);
        assert!(validated.is_none());
    }

    #[test]
    fn test_validate_execution_freshness_scales_exit_to_bid_depth() {
        let engine = make_engine(HashMap::from([(
            U256::from(1u64),
            make_book(U256::from(1u64), &[(dec!(0.40), dec!(3.25))], &[(dec!(0.45), dec!(10))]),
        )]));

        let opp = make_opp(TradeSide::Sell, dec!(0.40), dec!(5));
        let validated = engine.validate_execution_freshness(&opp, &[]).unwrap();
        assert_eq!(validated.size, dec!(3.25));
        match validated.execution_plan {
            ExecutionPlan::DirectionalBuy { side, price, size, .. } => {
                assert_eq!(side, TradeSide::Sell);
                assert_eq!(price, dec!(0.40));
                assert_eq!(size, dec!(3.25));
            }
        }
    }

    #[test]
    fn test_validate_execution_freshness_rejects_tiny_buy_before_executor() {
        let engine = make_engine(HashMap::from([(
            U256::from(1u64),
            make_book(U256::from(1u64), &[], &[(dec!(0.001), dec!(1000))]),
        )]));

        let opp = make_opp(TradeSide::Buy, dec!(0.001), dec!(5));
        assert!(engine.validate_execution_freshness(&opp, &[]).is_none());
    }

    #[test]
    fn test_validate_execution_freshness_rejects_tiny_sell_before_executor() {
        let engine = make_engine(HashMap::from([(
            U256::from(1u64),
            make_book(U256::from(1u64), &[(dec!(0.999), dec!(10))], &[]),
        )]));

        let opp = make_opp(TradeSide::Sell, dec!(0.999), dec!(0.00966));
        assert!(engine.validate_execution_freshness(&opp, &[]).is_none());
    }
}
