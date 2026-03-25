use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use alloy::primitives::{B256, U256};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal::prelude::FromPrimitive;
use uuid::Uuid;

use pa_core::config::SmartMoneyConfig;
use pa_core::traits::Strategy;
use pa_core::types::{
    ExecutionPlan, MarketInfo, OrderBook, StrategyType, TradeSide, TradingOpportunity,
};
use pa_market_data::wallet_tracker::{SignalType, SmartMoneySignal, SmartMoneySignalSource};
use pa_monitor::diagnostics::{
    SmartMoneyDecision, SmartMoneyExitDecision, record_smart_money_decision,
    record_smart_money_exit_decision,
};

use crate::profitability::ProfitCalculator;

// ──── Aggregated Signal ────

#[derive(Debug, Clone)]
struct AggregatedSignal {
    signal_type: SignalType,
    token_id: U256,
    condition_id: B256,
    /// Weighted target size = sum(wallet_size * follow_ratio * weight) for entries,
    /// or sum(delta * follow_ratio * weight) for exits.
    target_size: Decimal,
    wallet_count: usize,
    total_notional_usdc: Decimal,
    consensus_wallets: usize,
    max_wallet_weight: Decimal,
    average_delta_ratio: Decimal,
    latest_detected_at: DateTime<Utc>,
    has_onchain_source: bool,
    has_data_api_source: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SmartMoneyRejectReason {
    SignalTooOld,
    WalletWeightTooLow,
    ConsensusTooWeak,
    MissingOrderbook,
    InvalidPrice,
    EntryPriceTooHigh,
    SpreadTooWide,
    DepthTooThin,
    MarketLiquidityTooLow,
    PositionCapReached,
    CapitalInsufficient,
    BelowMinOrderSize,
    NonProfitableAfterFees,
}

impl SmartMoneyRejectReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::SignalTooOld => "signal_too_old",
            Self::WalletWeightTooLow => "wallet_weight_too_low",
            Self::ConsensusTooWeak => "consensus_too_weak",
            Self::MissingOrderbook => "missing_orderbook",
            Self::InvalidPrice => "invalid_price",
            Self::EntryPriceTooHigh => "entry_price_too_high",
            Self::SpreadTooWide => "spread_too_wide",
            Self::DepthTooThin => "depth_too_thin",
            Self::MarketLiquidityTooLow => "market_liquidity_too_low",
            Self::PositionCapReached => "position_cap_reached",
            Self::CapitalInsufficient => "capital_insufficient",
            Self::BelowMinOrderSize => "below_min_order_size",
            Self::NonProfitableAfterFees => "non_profitable_after_fees",
        }
    }
}

#[derive(Debug, Clone)]
struct EntryGateContext {
    market: MarketInfo,
    best_bid: Decimal,
    best_ask: Decimal,
    spread_bps: Decimal,
    top_level_depth_usdc: Decimal,
    base_target_size: Decimal,
    consensus_multiplier: Decimal,
    freshness_multiplier: Decimal,
    delta_ratio_multiplier: Decimal,
    concentration_multiplier: Decimal,
    raw_size: Decimal,
    final_size: Decimal,
}

// ──── SmartMoneyStrategy ────

pub struct SmartMoneyStrategyDeps {
    pub get_orderbook: Box<dyn Fn(U256) -> Option<OrderBook> + Send + Sync>,
    pub get_available_capital: Box<dyn Fn() -> Decimal + Send + Sync>,
    pub get_position: Box<dyn Fn(U256) -> Decimal + Send + Sync>,
    pub get_held_positions: Box<dyn Fn() -> Vec<(U256, Decimal, Decimal)> + Send + Sync>,
    pub now: Box<dyn Fn() -> DateTime<Utc> + Send + Sync>,
    pub signals: Arc<RwLock<Vec<SmartMoneySignal>>>,
    pub markets: Arc<RwLock<HashMap<B256, MarketInfo>>>,
}

pub struct SmartMoneyStrategy {
    config: SmartMoneyConfig,
    profit_calc: ProfitCalculator,
    get_orderbook: Box<dyn Fn(U256) -> Option<OrderBook> + Send + Sync>,
    get_available_capital: Box<dyn Fn() -> Decimal + Send + Sync>,
    get_position: Box<dyn Fn(U256) -> Decimal + Send + Sync>,
    get_held_positions: Box<dyn Fn() -> Vec<(U256, Decimal, Decimal)> + Send + Sync>,
    now: Box<dyn Fn() -> DateTime<Utc> + Send + Sync>,
    /// Shared signal queue from WalletTracker.
    signals: Arc<RwLock<Vec<SmartMoneySignal>>>,
    /// Markets lookup: condition_id → MarketInfo (for fee_rate_bps etc.)
    markets: Arc<RwLock<HashMap<B256, MarketInfo>>>,
    /// First time we observed each currently held smart-money position.
    position_first_seen_at: Arc<RwLock<HashMap<U256, DateTime<Utc>>>>,
    /// Highest best bid seen since the position was first observed.
    peak_bid_by_token: Arc<RwLock<HashMap<U256, Decimal>>>,
}

impl SmartMoneyStrategy {
    pub fn new(
        config: SmartMoneyConfig,
        gas_cost_usd: Decimal,
        deps: SmartMoneyStrategyDeps,
    ) -> Self {
        let SmartMoneyStrategyDeps {
            get_orderbook,
            get_available_capital,
            get_position,
            get_held_positions,
            now,
            signals,
            markets,
        } = deps;

        Self {
            config,
            profit_calc: ProfitCalculator::new(gas_cost_usd),
            get_orderbook,
            get_available_capital,
            get_position,
            get_held_positions,
            now,
            signals,
            markets,
            position_first_seen_at: Arc::new(RwLock::new(HashMap::new())),
            peak_bid_by_token: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    fn now(&self) -> DateTime<Utc> {
        (self.now)()
    }

    /// Update the internal markets lookup from the latest scan data.
    fn update_markets(&self, markets: &[MarketInfo]) {
        let mut map = self.markets.write().unwrap();
        for m in markets {
            map.insert(m.condition_id, m.clone());
        }
    }

    /// Consume and clear pending signals from the WalletTracker.
    fn consume_signals(&self) -> Vec<SmartMoneySignal> {
        let mut signals = self.signals.write().unwrap();
        std::mem::take(&mut *signals)
    }

    /// Aggregate signals by token_id. Multiple wallets entering the same market
    /// produce a single aggregated signal with combined target size.
    fn aggregate_signals(&self, signals: &[SmartMoneySignal]) -> HashMap<U256, AggregatedSignal> {
        let mut map: HashMap<U256, AggregatedSignal> = HashMap::new();
        let follow_ratio = self.config.follow_ratio;

        for sig in signals {
            let entry = map.entry(sig.token_id).or_insert_with(|| AggregatedSignal {
                signal_type: sig.signal_type,
                token_id: sig.token_id,
                condition_id: sig.condition_id,
                target_size: Decimal::ZERO,
                wallet_count: 0,
                total_notional_usdc: Decimal::ZERO,
                consensus_wallets: 0,
                max_wallet_weight: Decimal::ZERO,
                average_delta_ratio: Decimal::ZERO,
                latest_detected_at: sig.detected_at,
                has_onchain_source: false,
                has_data_api_source: false,
            });

            match sig.signal_type {
                SignalType::Entry | SignalType::Increase => {
                    // For entries: target = wallet_size * follow_ratio * weight
                    entry.target_size += sig.wallet_size * follow_ratio * sig.wallet_weight;
                    // Keep as Entry/Increase
                    if entry.signal_type == SignalType::Exit
                        || entry.signal_type == SignalType::Decrease
                    {
                        entry.signal_type = sig.signal_type;
                    }
                }
                SignalType::Decrease | SignalType::Exit => {
                    // For exits: target = delta * follow_ratio * weight
                    entry.target_size += sig.delta * follow_ratio * sig.wallet_weight;
                    // Only override to exit if all signals are exit-like
                    if entry.wallet_count == 0 {
                        entry.signal_type = sig.signal_type;
                    }
                }
            }
            entry.wallet_count += 1;
            entry.total_notional_usdc += sig.signal_notional_usdc;
            entry.consensus_wallets += 1;
            entry.max_wallet_weight = entry.max_wallet_weight.max(sig.wallet_weight);
            let delta_ratio = if sig.wallet_size > Decimal::ZERO {
                (sig.delta / sig.wallet_size).min(Decimal::ONE)
            } else {
                Decimal::ONE
            };
            entry.average_delta_ratio += delta_ratio;
            entry.latest_detected_at = entry.latest_detected_at.max(sig.detected_at);
            match sig.source {
                SmartMoneySignalSource::DataApi => entry.has_data_api_source = true,
                SmartMoneySignalSource::Onchain => entry.has_onchain_source = true,
            }
        }

        for entry in map.values_mut() {
            if entry.wallet_count > 0 {
                entry.average_delta_ratio /= Decimal::from(entry.wallet_count as u64);
            }
        }

        map
    }

    fn signal_age_secs(&self, agg: &AggregatedSignal) -> i64 {
        (self.now() - agg.latest_detected_at).num_seconds()
    }

    fn compute_spread_bps(best_bid: Decimal, best_ask: Decimal) -> Decimal {
        if best_bid <= Decimal::ZERO || best_ask <= Decimal::ZERO || best_ask <= best_bid {
            return Decimal::ZERO;
        }
        ((best_ask - best_bid) / best_ask) * Decimal::from(10_000u32)
    }

    fn top_level_ask_depth_usdc(book: &OrderBook) -> Option<Decimal> {
        let best_ask = book.best_ask()?;
        Some(best_ask.price * best_ask.size)
    }

    fn freshness_multiplier(&self, age_secs: i64) -> Decimal {
        if age_secs <= 0 {
            return Decimal::ONE;
        }
        let half_life = self.config.freshness_half_life_secs.max(1) as f64;
        let multiplier = 0.5_f64.powf(age_secs as f64 / half_life);
        Decimal::from_f64(multiplier)
            .unwrap_or(Decimal::ONE)
            .max(Decimal::ZERO)
            .min(Decimal::ONE)
    }

    fn consensus_multiplier(&self, consensus_wallets: usize) -> Decimal {
        if consensus_wallets <= 1 {
            return Decimal::ONE;
        }
        let bonus = (Decimal::from((consensus_wallets - 1) as u64)
            * self.config.consensus_bonus_per_wallet)
            .min(self.config.consensus_bonus_cap);
        Decimal::ONE + bonus
    }

    fn delta_ratio_multiplier(&self, average_delta_ratio: Decimal) -> Decimal {
        average_delta_ratio
            .max(self.config.leader_delta_ratio_floor)
            .min(Decimal::ONE)
    }

    fn concentration_multiplier(&self, existing_size: Decimal, best_ask: Decimal) -> Decimal {
        let existing_notional = existing_size * best_ask;
        let soft_cap = self.config.position_concentration_soft_cap_usdc;
        if existing_notional <= Decimal::ZERO
            || soft_cap <= Decimal::ZERO
            || existing_notional <= soft_cap
        {
            return Decimal::ONE;
        }
        (soft_cap / existing_notional)
            .max(self.config.position_concentration_min_multiplier)
            .min(Decimal::ONE)
    }

    fn record_decision(
        &self,
        agg: &AggregatedSignal,
        accepted: bool,
        reject_reason: Option<SmartMoneyRejectReason>,
    ) {
        record_smart_money_decision(SmartMoneyDecision {
            recorded_at: self.now(),
            token_id: agg.token_id.to_string(),
            condition_id: format!("{:#x}", agg.condition_id),
            signal_type: match agg.signal_type {
                SignalType::Entry => "entry",
                SignalType::Increase => "increase",
                SignalType::Decrease => "decrease",
                SignalType::Exit => "exit",
            }
            .into(),
            accepted,
            reject_reason: reject_reason.map(|reason| reason.as_str().to_string()),
            wallet_count: agg.wallet_count,
            max_wallet_weight: agg.max_wallet_weight,
            source_data_api: agg.has_data_api_source,
            source_onchain: agg.has_onchain_source,
        });
    }

    fn evaluate_entry_gate(
        &self,
        agg: &AggregatedSignal,
    ) -> Result<EntryGateContext, SmartMoneyRejectReason> {
        if self.signal_age_secs(agg) > self.config.max_signal_age_secs as i64 {
            return Err(SmartMoneyRejectReason::SignalTooOld);
        }
        if agg.max_wallet_weight < self.config.min_wallet_weight {
            return Err(SmartMoneyRejectReason::WalletWeightTooLow);
        }
        if agg.consensus_wallets < self.config.min_consensus_wallets {
            return Err(SmartMoneyRejectReason::ConsensusTooWeak);
        }
        let book =
            (self.get_orderbook)(agg.token_id).ok_or(SmartMoneyRejectReason::MissingOrderbook)?;
        let best_bid = book
            .best_bid()
            .map(|level| level.price)
            .ok_or(SmartMoneyRejectReason::MissingOrderbook)?;
        let best_ask = book
            .best_ask()
            .map(|level| level.price)
            .ok_or(SmartMoneyRejectReason::MissingOrderbook)?;
        if best_ask <= Decimal::ZERO || best_ask >= Decimal::ONE || best_bid < Decimal::ZERO {
            return Err(SmartMoneyRejectReason::InvalidPrice);
        }
        if best_ask > self.config.max_entry_price {
            return Err(SmartMoneyRejectReason::EntryPriceTooHigh);
        }
        let spread_bps = Self::compute_spread_bps(best_bid, best_ask);
        if spread_bps > Decimal::from(self.config.max_spread_bps) {
            return Err(SmartMoneyRejectReason::SpreadTooWide);
        }
        let top_level_depth_usdc =
            Self::top_level_ask_depth_usdc(&book).ok_or(SmartMoneyRejectReason::DepthTooThin)?;
        if top_level_depth_usdc < self.config.min_top_level_depth_usdc {
            return Err(SmartMoneyRejectReason::DepthTooThin);
        }
        let markets = self.markets.read().unwrap();
        let market = markets
            .get(&agg.condition_id)
            .cloned()
            .ok_or(SmartMoneyRejectReason::MissingOrderbook)?;
        if market.liquidity < self.config.min_market_liquidity {
            return Err(SmartMoneyRejectReason::MarketLiquidityTooLow);
        }

        // Dynamic sizing: base copy size modulated by consensus, freshness,
        // leader conviction (delta ratio), and concentration.
        let existing = (self.get_position)(agg.token_id);
        let consensus_multiplier = self.consensus_multiplier(agg.consensus_wallets);
        let freshness_multiplier = self.freshness_multiplier(self.signal_age_secs(agg));
        let delta_ratio_multiplier = self.delta_ratio_multiplier(agg.average_delta_ratio);
        let concentration_multiplier = self.concentration_multiplier(existing, best_ask);
        let raw_size = agg.target_size
            * consensus_multiplier
            * freshness_multiplier
            * delta_ratio_multiplier
            * concentration_multiplier;
        let max_shares = self.config.max_position_usdc / best_ask;
        let remaining = (max_shares - existing).max(Decimal::ZERO);
        if remaining <= Decimal::ZERO {
            return Err(SmartMoneyRejectReason::PositionCapReached);
        }
        let available_capital = (self.get_available_capital)();
        if available_capital <= Decimal::ZERO {
            return Err(SmartMoneyRejectReason::CapitalInsufficient);
        }
        let max_from_capital = available_capital / best_ask;
        let size = raw_size.min(remaining).min(max_from_capital);
        if size <= Decimal::ZERO {
            return Err(SmartMoneyRejectReason::CapitalInsufficient);
        }

        // Below CLOB minimum ($1.00 cost)
        if size <= Decimal::ZERO || size * best_ask < Decimal::ONE {
            return Err(SmartMoneyRejectReason::BelowMinOrderSize);
        }

        // Profitability check (use 1.0 as model_prob since we're following smart money)
        let est = self.profit_calc.directional_buy_profit(
            best_ask,
            Decimal::ONE, // assume token resolves to 1.0
            size,
            market.fee_rate_bps,
        );

        if est.net_profit <= Decimal::ZERO {
            return Err(SmartMoneyRejectReason::NonProfitableAfterFees);
        }

        Ok(EntryGateContext {
            market,
            best_bid,
            best_ask,
            spread_bps,
            top_level_depth_usdc,
            base_target_size: agg.target_size,
            consensus_multiplier,
            freshness_multiplier,
            delta_ratio_multiplier,
            concentration_multiplier,
            raw_size,
            final_size: size,
        })
    }

    /// Process an aggregated entry signal → TradingOpportunity.
    fn process_entry_signal(
        &self,
        agg: &AggregatedSignal,
    ) -> Result<TradingOpportunity, SmartMoneyRejectReason> {
        let gate = self.evaluate_entry_gate(agg)?;
        let est = self.profit_calc.directional_buy_profit(
            gate.best_ask,
            Decimal::ONE,
            gate.final_size,
            gate.market.fee_rate_bps,
        );

        tracing::info!(
            question = %gate.market.question,
            wallets = agg.wallet_count,
            base_target_size = %gate.base_target_size,
            target_size = %gate.raw_size,
            actual_size = %gate.final_size,
            price = %gate.best_ask,
            spread_bps = %gate.spread_bps,
            depth_usdc = %gate.top_level_depth_usdc,
            consensus_multiplier = %gate.consensus_multiplier,
            freshness_multiplier = %gate.freshness_multiplier,
            delta_ratio_multiplier = %gate.delta_ratio_multiplier,
            concentration_multiplier = %gate.concentration_multiplier,
            "SmartMoney: following entry signal"
        );

        let opp = TradingOpportunity {
            id: Uuid::now_v7(),
            strategy_type: StrategyType::SmartMoney,
            condition_id: agg.condition_id,
            question: gate.market.question.clone(),
            spread: gate.best_ask - gate.best_bid,
            estimated_profit: est.net_profit,
            size: gate.final_size,
            min_profit_retention_ratio_multiplier: None,
            max_slippage_bps_multiplier: None,
            min_size_retention_ratio_multiplier: None,
            execution_quality_profit_weight_multiplier: None,
            execution_quality_size_weight_multiplier: None,
            execution_quality_slippage_weight_multiplier: None,
            detected_at: self.now(),
            execution_plan: ExecutionPlan::DirectionalBuy {
                token_id: agg.token_id,
                side: TradeSide::Buy,
                price: gate.best_ask,
                size: gate.final_size,
                condition_id: agg.condition_id,
            },
        };
        Ok(opp)
    }

    /// Process an aggregated exit signal → TradingOpportunity.
    fn process_exit_signal(&self, agg: &AggregatedSignal) -> Option<TradingOpportunity> {
        if matches!(agg.signal_type, SignalType::Decrease)
            && agg.average_delta_ratio < self.config.leader_exit_min_delta_ratio
        {
            return None;
        }
        let our_position = (self.get_position)(agg.token_id);
        if our_position <= Decimal::ZERO {
            return None;
        }

        let book = (self.get_orderbook)(agg.token_id)?;
        let best_bid = book.best_bid()?.price;

        if best_bid <= Decimal::ZERO {
            return None;
        }

        let markets = self.markets.read().unwrap();
        let market = markets.get(&agg.condition_id)?;

        // Sell min(our position, proportional to wallet exit)
        let sell_size = our_position.min(agg.target_size);
        if sell_size <= Decimal::ZERO || sell_size * best_bid < Decimal::ONE {
            return None;
        }

        // Cap to available bid depth
        let bid_depth = book.available_depth(TradeSide::Sell, best_bid);
        let sell_size = sell_size.min(bid_depth);
        if sell_size <= Decimal::ZERO {
            return None;
        }

        tracing::info!(
            question = %market.question,
            wallets = agg.wallet_count,
            sell_size = %sell_size,
            price = %best_bid,
            "SmartMoney: following exit signal"
        );

        Some(TradingOpportunity {
            id: Uuid::now_v7(),
            strategy_type: StrategyType::SmartMoney,
            condition_id: agg.condition_id,
            question: format!("[EXIT] {}", market.question),
            spread: Decimal::ZERO,
            estimated_profit: Decimal::ZERO,
            size: sell_size,
            min_profit_retention_ratio_multiplier: None,
            max_slippage_bps_multiplier: None,
            min_size_retention_ratio_multiplier: None,
            execution_quality_profit_weight_multiplier: None,
            execution_quality_size_weight_multiplier: None,
            execution_quality_slippage_weight_multiplier: None,
            detected_at: self.now(),
            execution_plan: ExecutionPlan::DirectionalBuy {
                token_id: agg.token_id,
                side: TradeSide::Sell,
                price: best_bid,
                size: sell_size,
                condition_id: agg.condition_id,
            },
        })
    }

    fn bps_to_ratio(bps: u32) -> Decimal {
        Decimal::from(bps) / Decimal::from(10_000u32)
    }

    fn update_position_tracking(&self, held: &[(U256, Decimal, Decimal)]) {
        let now = self.now();
        let held_tokens: std::collections::HashSet<_> =
            held.iter().map(|(token_id, _, _)| *token_id).collect();
        {
            let mut first_seen = self.position_first_seen_at.write().unwrap();
            for token_id in &held_tokens {
                first_seen.entry(*token_id).or_insert(now);
            }
            first_seen.retain(|token_id, _| held_tokens.contains(token_id));
        }
        {
            let mut peaks = self.peak_bid_by_token.write().unwrap();
            peaks.retain(|token_id, _| held_tokens.contains(token_id));
        }
    }

    fn build_exit_opportunity(
        &self,
        token_id: U256,
        size: Decimal,
        avg_cost: Decimal,
        best_bid: Decimal,
        condition_id: B256,
        question: String,
        fee_rate_bps: u32,
        reason: &str,
    ) -> TradingOpportunity {
        let est = self
            .profit_calc
            .directional_sell_profit(best_bid, avg_cost, size, fee_rate_bps);
        tracing::info!(
            token_id = %token_id,
            best_bid = %best_bid,
            avg_cost = %avg_cost,
            reason,
            "SmartMoney: strategy exit trigger"
        );
        record_smart_money_exit_decision(SmartMoneyExitDecision {
            recorded_at: self.now(),
            token_id: token_id.to_string(),
            condition_id: format!("{:#x}", condition_id),
            reason: reason.to_string(),
            question: question.clone(),
            best_bid,
            avg_cost,
            size,
        });
        TradingOpportunity {
            id: Uuid::now_v7(),
            strategy_type: StrategyType::SmartMoney,
            condition_id,
            question: format!("[EXIT:{reason}] {question}"),
            spread: best_bid - avg_cost,
            estimated_profit: est.net_profit,
            size,
            min_profit_retention_ratio_multiplier: None,
            max_slippage_bps_multiplier: None,
            min_size_retention_ratio_multiplier: None,
            execution_quality_profit_weight_multiplier: None,
            execution_quality_size_weight_multiplier: None,
            execution_quality_slippage_weight_multiplier: None,
            detected_at: self.now(),
            execution_plan: ExecutionPlan::DirectionalBuy {
                token_id,
                side: TradeSide::Sell,
                price: best_bid,
                size,
                condition_id,
            },
        }
    }

    /// Scan held positions for stale/profit-protect/drawdown/capital-efficiency exits.
    fn scan_exits(&self) -> Vec<TradingOpportunity> {
        let held = (self.get_held_positions)();
        if held.is_empty() {
            return vec![];
        }
        self.update_position_tracking(&held);

        let mut exits = Vec::new();
        let markets = self.markets.read().unwrap();

        for (token_id, size, avg_cost) in &held {
            let book = match (self.get_orderbook)(*token_id) {
                Some(b) => b,
                None => continue,
            };
            let best_bid = match book.best_bid() {
                Some(b) => b.price,
                None => continue,
            };

            let condition_id = markets
                .values()
                .find(|m| m.tokens.iter().any(|t| t.token_id == *token_id))
                .map(|m| m.condition_id)
                .unwrap_or_default();
            let question = markets
                .values()
                .find(|m| m.tokens.iter().any(|t| t.token_id == *token_id))
                .map(|m| m.question.clone())
                .unwrap_or_default();
            let fee_rate_bps = markets
                .values()
                .find(|m| m.tokens.iter().any(|t| t.token_id == *token_id))
                .map(|m| m.fee_rate_bps)
                .unwrap_or(200);
            let held_secs = self
                .position_first_seen_at
                .read()
                .unwrap()
                .get(token_id)
                .map(|first_seen| (self.now() - *first_seen).num_seconds())
                .unwrap_or(0);
            let peak_bid = {
                let mut peaks = self.peak_bid_by_token.write().unwrap();
                let entry = peaks.entry(*token_id).or_insert(best_bid);
                if best_bid > *entry {
                    *entry = best_bid;
                }
                *entry
            };
            let profit_trigger_price = *avg_cost
                * (Decimal::ONE + Self::bps_to_ratio(self.config.profit_protect_min_gain_bps));
            let profit_protect_floor = peak_bid
                * (Decimal::ONE - Self::bps_to_ratio(self.config.profit_protect_drawdown_bps));
            let drawdown_floor =
                *avg_cost * (Decimal::ONE - Self::bps_to_ratio(self.config.max_drawdown_bps));

            let exit_reason = if best_bid <= drawdown_floor {
                Some("drawdown")
            } else if peak_bid >= profit_trigger_price && best_bid <= profit_protect_floor {
                Some("profit_protect")
            } else if held_secs >= self.config.max_hold_secs as i64 {
                Some("stale_follow")
            } else if best_bid >= self.config.capital_efficiency_threshold {
                Some("capital_efficiency")
            } else {
                None
            };

            if let Some(reason) = exit_reason {
                exits.push(self.build_exit_opportunity(
                    *token_id,
                    *size,
                    *avg_cost,
                    best_bid,
                    condition_id,
                    question,
                    fee_rate_bps,
                    reason,
                ));
            }
        }

        exits
    }
}

#[async_trait]
impl Strategy for SmartMoneyStrategy {
    fn name(&self) -> &str {
        "SmartMoney"
    }

    fn strategy_type(&self) -> StrategyType {
        StrategyType::SmartMoney
    }

    async fn scan(&self, markets: &[MarketInfo]) -> pa_core::Result<Vec<TradingOpportunity>> {
        // Update markets lookup
        self.update_markets(markets);

        // Consume pending signals
        let signals = self.consume_signals();

        if signals.is_empty() {
            // No new signals — just run exit scan
            return Ok(self.scan_exits());
        }

        // Aggregate by token_id
        let aggregated = self.aggregate_signals(&signals);

        // Generate opportunities
        let mut opps = Vec::new();
        for agg in aggregated.values() {
            match agg.signal_type {
                SignalType::Entry | SignalType::Increase => match self.process_entry_signal(agg) {
                    Ok(opp) => {
                        self.record_decision(agg, true, None);
                        opps.push(opp);
                    }
                    Err(reason) => {
                        self.record_decision(agg, false, Some(reason));
                    }
                },
                SignalType::Decrease | SignalType::Exit => {
                    if let Some(opp) = self.process_exit_signal(agg) {
                        opps.push(opp);
                    }
                }
            }
        }

        // Append capital efficiency exits
        opps.extend(self.scan_exits());

        Ok(opps)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pa_core::types::{OrderBook, Outcome, PriceLevel, TokenInfo};
    use pa_market_data::wallet_tracker::SmartMoneySignalSource;
    use rust_decimal_macros::dec;

    fn make_book(bids: Vec<(Decimal, Decimal)>, asks: Vec<(Decimal, Decimal)>) -> OrderBook {
        OrderBook {
            token_id: U256::from(1u64),
            bids: bids
                .into_iter()
                .map(|(price, size)| PriceLevel { price, size })
                .collect(),
            asks: asks
                .into_iter()
                .map(|(price, size)| PriceLevel { price, size })
                .collect(),
            timestamp: Utc::now(),
        }
    }

    fn make_market(condition_id: B256, token_id: U256) -> MarketInfo {
        MarketInfo {
            condition_id,
            question_id: B256::ZERO,
            question: "Test market".to_string(),
            neg_risk: false,
            neg_risk_market_id: None,
            tokens: vec![TokenInfo {
                token_id,
                outcome: Outcome::Yes,
                complement_id: U256::from(999u64),
            }],
            tick_size: dec!(0.01),
            fee_rate_bps: 200,
            active: true,
            liquidity: dec!(1000),
            event_title: None,
            end_date: None,
            category: None,
            outcome_prices: None,
            gamma_best_bid: None,
            gamma_best_ask: None,
            rewards_min_size: None,
            rewards_max_spread: None,
            rewards_daily_rate: None,
            holding_rewards_enabled: false,
            fees_enabled: true,
        }
    }

    fn make_signal(
        signal_type: SignalType,
        wallet_address: &str,
        wallet_weight: Decimal,
        token_id: U256,
        condition_id: B256,
        wallet_size: Decimal,
        delta: Decimal,
    ) -> SmartMoneySignal {
        SmartMoneySignal {
            signal_type,
            wallet_address: wallet_address.to_string(),
            wallet_label: None,
            wallet_weight,
            token_id,
            condition_id,
            wallet_size,
            delta,
            signal_notional_usdc: delta,
            source: SmartMoneySignalSource::DataApi,
            detected_at: Utc::now(),
        }
    }

    fn make_strategy(
        book: OrderBook,
        position: Decimal,
        balance: Decimal,
        signals: Vec<SmartMoneySignal>,
        market: MarketInfo,
    ) -> SmartMoneyStrategy {
        let config = make_config();
        make_strategy_with_config(book, position, balance, signals, market, config)
    }

    fn make_config() -> SmartMoneyConfig {
        SmartMoneyConfig {
            follow_ratio: dec!(0.10),
            max_position_usdc: dec!(100),
            capital_efficiency_threshold: dec!(0.98),
            max_signal_age_secs: 300,
            freshness_half_life_secs: 60,
            ..Default::default()
        }
    }

    fn make_strategy_with_config(
        book: OrderBook,
        position: Decimal,
        balance: Decimal,
        signals: Vec<SmartMoneySignal>,
        market: MarketInfo,
        config: SmartMoneyConfig,
    ) -> SmartMoneyStrategy {
        let book = Arc::new(book);
        let signals_arc = Arc::new(RwLock::new(signals));
        let markets_arc = Arc::new(RwLock::new(HashMap::from([(market.condition_id, market)])));

        SmartMoneyStrategy::new(
            config,
            dec!(0.00),
            SmartMoneyStrategyDeps {
                get_orderbook: Box::new(move |_| Some((*book).clone())),
                get_available_capital: Box::new(move || balance),
                get_position: Box::new(move |_| position),
                get_held_positions: Box::new(Vec::new),
                now: Box::new(Utc::now),
                signals: signals_arc,
                markets: markets_arc,
            },
        )
    }

    #[test]
    fn test_aggregate_multi_wallet() {
        let config = SmartMoneyConfig {
            follow_ratio: dec!(0.10),
            ..Default::default()
        };

        let signals = vec![
            make_signal(
                SignalType::Entry,
                "0xaaa",
                Decimal::ONE,
                U256::from(42u64),
                B256::ZERO,
                dec!(1000),
                dec!(1000),
            ),
            make_signal(
                SignalType::Entry,
                "0xbbb",
                dec!(0.5),
                U256::from(42u64),
                B256::ZERO,
                dec!(2000),
                dec!(2000),
            ),
        ];

        let strategy = SmartMoneyStrategy {
            config,
            profit_calc: ProfitCalculator::new(dec!(0)),
            get_orderbook: Box::new(|_| None),
            get_available_capital: Box::new(|| dec!(1000)),
            get_position: Box::new(|_| Decimal::ZERO),
            get_held_positions: Box::new(Vec::new),
            now: Box::new(Utc::now),
            signals: Arc::new(RwLock::new(vec![])),
            markets: Arc::new(RwLock::new(HashMap::new())),
            position_first_seen_at: Arc::new(RwLock::new(HashMap::new())),
            peak_bid_by_token: Arc::new(RwLock::new(HashMap::new())),
        };

        let aggregated = strategy.aggregate_signals(&signals);
        let agg = aggregated.get(&U256::from(42u64)).unwrap();

        assert_eq!(agg.wallet_count, 2);
        // 1000 * 0.10 * 1.0 + 2000 * 0.10 * 0.5 = 100 + 100 = 200
        assert_eq!(agg.target_size, dec!(200));
    }

    #[test]
    fn test_entry_proportional_sizing() {
        let token_id = U256::from(42u64);
        let cid = B256::ZERO;
        let book = make_book(vec![(dec!(0.59), dec!(500))], vec![(dec!(0.60), dec!(500))]);
        let market = make_market(cid, token_id);

        let signals = vec![make_signal(
            SignalType::Entry,
            "0xaaa",
            Decimal::ONE,
            token_id,
            cid,
            dec!(500),
            dec!(500),
        )];

        let strategy = make_strategy(book, Decimal::ZERO, dec!(1000), signals, market);
        let consumed = strategy.consume_signals();
        let aggregated = strategy.aggregate_signals(&consumed);
        let agg = aggregated.get(&token_id).unwrap();

        // target_size = 500 * 0.10 * 1.0 = 50
        assert_eq!(agg.target_size, dec!(50));

        let opp = strategy.process_entry_signal(agg);
        assert!(opp.is_ok());
        let opp = opp.unwrap();
        assert_eq!(opp.size, dec!(50)); // 50 shares at 0.60 = $30 cost, fits in $1000 balance
    }

    #[test]
    fn test_exit_follows_wallet() {
        let token_id = U256::from(42u64);
        let cid = B256::ZERO;
        let book = make_book(vec![(dec!(0.55), dec!(500))], vec![(dec!(0.60), dec!(500))]);
        let market = make_market(cid, token_id);

        let signals = vec![make_signal(
            SignalType::Exit,
            "0xaaa",
            Decimal::ONE,
            token_id,
            cid,
            Decimal::ZERO,
            dec!(200),
        )];

        // We hold 30 shares
        let strategy = make_strategy(book, dec!(30), dec!(1000), signals, market);
        let consumed = strategy.consume_signals();
        let aggregated = strategy.aggregate_signals(&consumed);
        let agg = aggregated.get(&token_id).unwrap();

        // target_size = 200 * 0.10 * 1.0 = 20
        assert_eq!(agg.target_size, dec!(20));

        let opp = strategy.process_exit_signal(agg);
        assert!(opp.is_some());
        let opp = opp.unwrap();
        // sell min(our 30, target 20) = 20, capped by bid depth 500
        assert_eq!(opp.size, dec!(20));
        match opp.execution_plan {
            ExecutionPlan::DirectionalBuy { side, .. } => assert_eq!(side, TradeSide::Sell),
        }
    }

    #[test]
    fn test_position_cap() {
        let token_id = U256::from(42u64);
        let cid = B256::ZERO;
        let book = make_book(
            vec![(dec!(0.59), dec!(500))],
            vec![(dec!(0.60), dec!(5000))],
        );
        let market = make_market(cid, token_id);

        let signals = vec![make_signal(
            SignalType::Entry,
            "0xaaa",
            Decimal::ONE,
            token_id,
            cid,
            dec!(10000),
            dec!(10000),
        )];

        // Already have 100 shares at 0.60 → max_position_usdc=100, so 100/0.60=166 shares max
        // existing=100, remaining=166-100=66
        let strategy = make_strategy(book, dec!(100), dec!(1000), signals, market);
        let consumed = strategy.consume_signals();
        let aggregated = strategy.aggregate_signals(&consumed);
        let agg = aggregated.get(&token_id).unwrap();

        // target_size = 10000 * 0.10 * 1.0 = 1000 (way above cap)
        assert_eq!(agg.target_size, dec!(1000));

        let opp = strategy.process_entry_signal(agg);
        assert!(opp.is_ok());
        let opp = opp.unwrap();
        // Should be capped: min(1000, 66.666, 1666.666) ≈ 66
        // max_shares = 100/0.60 = 166.666, remaining = 166.666-100 = 66.666
        assert!(opp.size <= dec!(67)); // approximately 66.666
        assert!(opp.size > Decimal::ZERO);
    }

    #[test]
    fn test_capital_efficiency_exit() {
        let token_id = U256::from(42u64);
        let cid = B256::ZERO;
        // best_bid = 0.99 ≥ threshold 0.98
        let book = make_book(vec![(dec!(0.99), dec!(500))], vec![(dec!(1.00), dec!(500))]);
        let market = make_market(cid, token_id);

        let config = SmartMoneyConfig {
            capital_efficiency_threshold: dec!(0.98),
            ..Default::default()
        };

        let book_arc = Arc::new(book);
        let markets_arc = Arc::new(RwLock::new(HashMap::from([(cid, market)])));

        let strategy = SmartMoneyStrategy::new(
            config,
            dec!(0.00),
            SmartMoneyStrategyDeps {
                get_orderbook: Box::new(move |_| Some((*book_arc).clone())),
                get_available_capital: Box::new(|| dec!(1000)),
                get_position: Box::new(|_| Decimal::ZERO),
                // We hold 50 shares at avg_cost 0.60
                get_held_positions: Box::new(move || vec![(token_id, dec!(50), dec!(0.60))]),
                now: Box::new(Utc::now),
                signals: Arc::new(RwLock::new(vec![])),
                markets: markets_arc,
            },
        );

        let exits = strategy.scan_exits();
        assert_eq!(exits.len(), 1);
        assert_eq!(exits[0].size, dec!(50));
        match &exits[0].execution_plan {
            ExecutionPlan::DirectionalBuy { side, price, .. } => {
                assert_eq!(*side, TradeSide::Sell);
                assert_eq!(*price, dec!(0.99));
            }
        }
    }

    #[test]
    fn test_consensus_bonus_sizes_up_entry() {
        let token_id = U256::from(42u64);
        let cid = B256::ZERO;
        let book = make_book(
            vec![(dec!(0.59), dec!(1000))],
            vec![(dec!(0.60), dec!(1000))],
        );
        let market = make_market(cid, token_id);
        let config = SmartMoneyConfig {
            consensus_bonus_per_wallet: dec!(0.10),
            consensus_bonus_cap: dec!(0.30),
            ..make_config()
        };
        let signals = vec![
            make_signal(
                SignalType::Entry,
                "0xaaa",
                Decimal::ONE,
                token_id,
                cid,
                dec!(500),
                dec!(500),
            ),
            make_signal(
                SignalType::Entry,
                "0xbbb",
                Decimal::ONE,
                token_id,
                cid,
                dec!(500),
                dec!(500),
            ),
        ];
        let strategy =
            make_strategy_with_config(book, Decimal::ZERO, dec!(1000), signals, market, config);
        let consumed = strategy.consume_signals();
        let aggregated = strategy.aggregate_signals(&consumed);
        let agg = aggregated.get(&token_id).unwrap();
        let opp = strategy.process_entry_signal(agg).unwrap();

        // Base target size: 500*0.1 + 500*0.1 = 100, plus one extra-wallet bonus => 110.
        assert_eq!(opp.size.round_dp(2), dec!(110.00));
    }

    #[test]
    fn test_stale_signal_decays_entry_size() {
        let token_id = U256::from(42u64);
        let cid = B256::ZERO;
        let book = make_book(
            vec![(dec!(0.59), dec!(1000))],
            vec![(dec!(0.60), dec!(1000))],
        );
        let market = make_market(cid, token_id);
        let config = SmartMoneyConfig {
            freshness_half_life_secs: 30,
            max_signal_age_secs: 300,
            ..make_config()
        };
        let mut signal = make_signal(
            SignalType::Entry,
            "0xaaa",
            Decimal::ONE,
            token_id,
            cid,
            dec!(500),
            dec!(500),
        );
        signal.detected_at = Utc::now() - chrono::Duration::seconds(30);
        let strategy = make_strategy_with_config(
            book,
            Decimal::ZERO,
            dec!(1000),
            vec![signal],
            market,
            config,
        );
        let consumed = strategy.consume_signals();
        let aggregated = strategy.aggregate_signals(&consumed);
        let agg = aggregated.get(&token_id).unwrap();
        let opp = strategy.process_entry_signal(agg).unwrap();

        // Base target size is 50 and one half-life old => ~25.
        assert_eq!(opp.size.round_dp(2), dec!(25.00));
    }

    #[test]
    fn test_concentration_penalty_scales_new_entry_down() {
        let token_id = U256::from(42u64);
        let cid = B256::ZERO;
        let book = make_book(
            vec![(dec!(0.59), dec!(1000))],
            vec![(dec!(0.60), dec!(1000))],
        );
        let market = make_market(cid, token_id);
        let config = SmartMoneyConfig {
            position_concentration_soft_cap_usdc: dec!(60),
            position_concentration_min_multiplier: dec!(0.25),
            max_position_usdc: dec!(500),
            ..make_config()
        };
        let signals = vec![make_signal(
            SignalType::Entry,
            "0xaaa",
            Decimal::ONE,
            token_id,
            cid,
            dec!(500),
            dec!(500),
        )];
        // Existing 200 shares at 0.60 => $120 notional, so multiplier should halve target size.
        let strategy =
            make_strategy_with_config(book, dec!(200), dec!(1000), signals, market, config);
        let consumed = strategy.consume_signals();
        let aggregated = strategy.aggregate_signals(&consumed);
        let agg = aggregated.get(&token_id).unwrap();
        let opp = strategy.process_entry_signal(agg).unwrap();

        assert_eq!(opp.size.round_dp(2), dec!(25.00));
    }

    #[test]
    fn test_small_leader_decrease_does_not_trigger_exit() {
        let token_id = U256::from(42u64);
        let cid = B256::ZERO;
        let book = make_book(vec![(dec!(0.59), dec!(500))], vec![(dec!(0.60), dec!(500))]);
        let market = make_market(cid, token_id);
        let config = SmartMoneyConfig {
            leader_exit_min_delta_ratio: dec!(0.25),
            ..make_config()
        };
        let signals = vec![make_signal(
            SignalType::Decrease,
            "0xaaa",
            Decimal::ONE,
            token_id,
            cid,
            dec!(500),
            dec!(50),
        )];
        let strategy =
            make_strategy_with_config(book, dec!(30), dec!(1000), signals, market, config);
        let consumed = strategy.consume_signals();
        let aggregated = strategy.aggregate_signals(&consumed);
        let agg = aggregated.get(&token_id).unwrap();

        assert!(strategy.process_exit_signal(agg).is_none());
    }

    #[test]
    fn test_stale_follow_exit_triggers() {
        let token_id = U256::from(42u64);
        let cid = B256::ZERO;
        let book = make_book(vec![(dec!(0.59), dec!(500))], vec![(dec!(0.60), dec!(500))]);
        let market = make_market(cid, token_id);
        let config = SmartMoneyConfig {
            max_hold_secs: 60,
            capital_efficiency_threshold: dec!(0.98),
            ..make_config()
        };
        let strategy =
            make_strategy_with_config(book, Decimal::ZERO, dec!(1000), vec![], market, config);
        strategy
            .position_first_seen_at
            .write()
            .unwrap()
            .insert(token_id, Utc::now() - chrono::Duration::seconds(120));
        let exits = SmartMoneyStrategy {
            get_held_positions: Box::new(move || vec![(token_id, dec!(20), dec!(0.50))]),
            ..strategy
        }
        .scan_exits();

        assert_eq!(exits.len(), 1);
        assert!(exits[0].question.contains("stale_follow"));
    }

    #[test]
    fn test_profit_protect_exit_triggers_after_peak_drawdown() {
        let token_id = U256::from(42u64);
        let cid = B256::ZERO;
        let book = make_book(vec![(dec!(0.74), dec!(500))], vec![(dec!(0.75), dec!(500))]);
        let market = make_market(cid, token_id);
        let config = SmartMoneyConfig {
            profit_protect_min_gain_bps: 1000,
            profit_protect_drawdown_bps: 500,
            max_drawdown_bps: 2000,
            capital_efficiency_threshold: dec!(0.98),
            ..make_config()
        };
        let strategy =
            make_strategy_with_config(book, Decimal::ZERO, dec!(1000), vec![], market, config);
        strategy
            .position_first_seen_at
            .write()
            .unwrap()
            .insert(token_id, Utc::now() - chrono::Duration::seconds(30));
        strategy
            .peak_bid_by_token
            .write()
            .unwrap()
            .insert(token_id, dec!(0.85));
        let exits = SmartMoneyStrategy {
            get_held_positions: Box::new(move || vec![(token_id, dec!(20), dec!(0.60))]),
            ..strategy
        }
        .scan_exits();

        assert_eq!(exits.len(), 1);
        assert!(exits[0].question.contains("profit_protect"));
    }

    #[test]
    fn test_entry_rejects_wide_spread() {
        let token_id = U256::from(42u64);
        let cid = B256::ZERO;
        let book = make_book(vec![(dec!(0.10), dec!(500))], vec![(dec!(0.60), dec!(500))]);
        let market = make_market(cid, token_id);
        let signals = vec![make_signal(
            SignalType::Entry,
            "0xaaa",
            Decimal::ONE,
            token_id,
            cid,
            dec!(500),
            dec!(500),
        )];
        let strategy = make_strategy(book, Decimal::ZERO, dec!(1000), signals, market);
        let consumed = strategy.consume_signals();
        let aggregated = strategy.aggregate_signals(&consumed);
        let agg = aggregated.get(&token_id).unwrap();

        assert_eq!(
            strategy.process_entry_signal(agg).unwrap_err(),
            SmartMoneyRejectReason::SpreadTooWide
        );
    }

    #[test]
    fn test_entry_rejects_old_signal() {
        let token_id = U256::from(42u64);
        let cid = B256::ZERO;
        let book = make_book(vec![(dec!(0.59), dec!(500))], vec![(dec!(0.60), dec!(500))]);
        let market = make_market(cid, token_id);
        let mut signal = make_signal(
            SignalType::Entry,
            "0xaaa",
            Decimal::ONE,
            token_id,
            cid,
            dec!(500),
            dec!(500),
        );
        signal.detected_at = Utc::now() - chrono::Duration::seconds(120);
        let strategy = SmartMoneyStrategy::new(
            SmartMoneyConfig {
                max_signal_age_secs: 90,
                ..make_config()
            },
            dec!(0.00),
            SmartMoneyStrategyDeps {
                get_orderbook: Box::new({
                    let book = book;
                    move |_| Some(book.clone())
                }),
                get_available_capital: Box::new(|| dec!(1000)),
                get_position: Box::new(|_| Decimal::ZERO),
                get_held_positions: Box::new(Vec::new),
                now: Box::new(Utc::now),
                signals: Arc::new(RwLock::new(vec![signal])),
                markets: Arc::new(RwLock::new(HashMap::from([(cid, market)]))),
            },
        );
        let consumed = strategy.consume_signals();
        let aggregated = strategy.aggregate_signals(&consumed);
        let agg = aggregated.get(&token_id).unwrap();

        assert_eq!(
            strategy.process_entry_signal(agg).unwrap_err(),
            SmartMoneyRejectReason::SignalTooOld
        );
    }
}
