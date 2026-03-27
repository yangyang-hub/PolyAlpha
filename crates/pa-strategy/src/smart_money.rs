use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use alloy::primitives::{B256, U256};
use arc_swap::ArcSwap;
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
    SmartMoneyDecision, SmartMoneyExitDecision, SmartMoneyLeaderAttributionSlice,
    SmartMoneyLeaderPnlAttributionEntry, record_smart_money_decision,
    record_smart_money_exit_decision, record_smart_money_leader_pnl_attribution,
    record_smart_money_opportunity_attribution,
};

use crate::profitability::ProfitCalculator;
use crate::utils::floor_price_to_tick;

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
    leader_addresses: Vec<String>,
    leader_labels: Vec<String>,
    leader_contributions: Vec<LeaderContribution>,
}

#[derive(Debug, Clone)]
struct LeaderContribution {
    address: String,
    label: String,
    weighted_size: Decimal,
}

#[derive(Debug, Clone)]
struct LeaderExposureLot {
    address: String,
    label: String,
    size: Decimal,
}

#[derive(Debug, Clone, Default)]
struct LeaderAttributionTotals {
    label: String,
    estimated_realized_pnl: Decimal,
    estimated_exited_size: Decimal,
    estimated_exit_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SmartMoneyRejectReason {
    RouteMismatch,
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
            Self::RouteMismatch => "route_mismatch",
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
    config: Arc<ArcSwap<SmartMoneyConfig>>,
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
    /// Estimated outstanding copied size by token and source leader.
    leader_exposure_by_token: Arc<RwLock<HashMap<U256, Vec<LeaderExposureLot>>>>,
    /// Estimated realized PnL attribution by leader from generated smart-money exits.
    leader_realized_totals: Arc<RwLock<HashMap<String, LeaderAttributionTotals>>>,
}

impl SmartMoneyStrategy {
    pub fn new(
        config: Arc<ArcSwap<SmartMoneyConfig>>,
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
            leader_exposure_by_token: Arc::new(RwLock::new(HashMap::new())),
            leader_realized_totals: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    fn now(&self) -> DateTime<Utc> {
        (self.now)()
    }

    fn config(&self) -> Arc<SmartMoneyConfig> {
        self.config.load_full()
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
        let config = self.config();
        let follow_ratio = config.follow_ratio;
        let blocked_wallets: std::collections::HashSet<String> = config
            .blocked_wallets
            .iter()
            .map(|address| address.to_lowercase())
            .collect();

        for sig in signals {
            if blocked_wallets.contains(&sig.wallet_address.to_lowercase()) {
                continue;
            }
            if matches!(sig.signal_type, SignalType::Entry | SignalType::Increase)
                && !self.signal_matches_leader_route(sig, &config)
            {
                self.record_route_mismatch(sig);
                continue;
            }
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
                leader_addresses: Vec::new(),
                leader_labels: Vec::new(),
                leader_contributions: Vec::new(),
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
            if !entry
                .leader_addresses
                .iter()
                .any(|address| address == &sig.wallet_address)
            {
                entry.leader_addresses.push(sig.wallet_address.clone());
            }
            if let Some(label) = sig.wallet_label.as_ref()
                && !label.is_empty()
                && !entry.leader_labels.iter().any(|existing| existing == label)
            {
                entry.leader_labels.push(label.clone());
            }
            let weighted_size = match sig.signal_type {
                SignalType::Entry | SignalType::Increase => {
                    sig.wallet_size * follow_ratio * sig.wallet_weight
                }
                SignalType::Decrease | SignalType::Exit => {
                    sig.delta * follow_ratio * sig.wallet_weight
                }
            };
            if let Some(existing) = entry
                .leader_contributions
                .iter_mut()
                .find(|existing| existing.address == sig.wallet_address)
            {
                existing.weighted_size += weighted_size;
                if existing.label.is_empty() {
                    existing.label = sig.wallet_label.clone().unwrap_or_default();
                }
            } else {
                entry.leader_contributions.push(LeaderContribution {
                    address: sig.wallet_address.clone(),
                    label: sig.wallet_label.clone().unwrap_or_default(),
                    weighted_size,
                });
            }
        }

        for entry in map.values_mut() {
            if entry.wallet_count > 0 {
                entry.average_delta_ratio /= Decimal::from(entry.wallet_count as u64);
            }
        }

        map
    }

    fn signal_matches_leader_route(
        &self,
        signal: &SmartMoneySignal,
        config: &SmartMoneyConfig,
    ) -> bool {
        let Some(route) = config
            .leader_routes
            .iter()
            .find(|route| route.address.eq_ignore_ascii_case(&signal.wallet_address))
        else {
            return true;
        };

        let markets = self.markets.read().unwrap();
        let Some(market) = markets.get(&signal.condition_id) else {
            return false;
        };

        let category_match = route.categories.is_empty()
            || market.category.as_deref().is_some_and(|category| {
                route
                    .categories
                    .iter()
                    .any(|allowed| category.eq_ignore_ascii_case(allowed.trim()))
            });
        let question_match = route.question_keywords.is_empty()
            || contains_any_keyword(&market.question, &route.question_keywords);
        let event_title_match = route.event_title_keywords.is_empty()
            || market
                .event_title
                .as_deref()
                .is_some_and(|title| contains_any_keyword(title, &route.event_title_keywords));

        category_match && question_match && event_title_match
    }

    fn record_route_mismatch(&self, signal: &SmartMoneySignal) {
        record_smart_money_decision(SmartMoneyDecision {
            recorded_at: self.now(),
            token_id: signal.token_id.to_string(),
            condition_id: format!("{:#x}", signal.condition_id),
            signal_type: match signal.signal_type {
                SignalType::Entry => "entry",
                SignalType::Increase => "increase",
                SignalType::Decrease => "decrease",
                SignalType::Exit => "exit",
            }
            .into(),
            accepted: false,
            reject_reason: Some(SmartMoneyRejectReason::RouteMismatch.as_str().to_string()),
            wallet_count: 1,
            max_wallet_weight: signal.wallet_weight,
            source_data_api: matches!(signal.source, SmartMoneySignalSource::DataApi),
            source_onchain: matches!(signal.source, SmartMoneySignalSource::Onchain),
            leader_addresses: vec![signal.wallet_address.clone()],
            leader_labels: signal.wallet_label.clone().into_iter().collect(),
        });
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
        let config = self.config();
        let half_life = config.freshness_half_life_secs.max(1) as f64;
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
        let config = self.config();
        let bonus = (Decimal::from((consensus_wallets - 1) as u64)
            * config.consensus_bonus_per_wallet)
            .min(config.consensus_bonus_cap);
        Decimal::ONE + bonus
    }

    fn delta_ratio_multiplier(&self, average_delta_ratio: Decimal) -> Decimal {
        let config = self.config();
        average_delta_ratio
            .max(config.leader_delta_ratio_floor)
            .min(Decimal::ONE)
    }

    fn concentration_multiplier(&self, existing_size: Decimal, best_ask: Decimal) -> Decimal {
        let existing_notional = existing_size * best_ask;
        let config = self.config();
        let soft_cap = config.position_concentration_soft_cap_usdc;
        if existing_notional <= Decimal::ZERO
            || soft_cap <= Decimal::ZERO
            || existing_notional <= soft_cap
        {
            return Decimal::ONE;
        }
        (soft_cap / existing_notional)
            .max(config.position_concentration_min_multiplier)
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
            leader_addresses: agg.leader_addresses.clone(),
            leader_labels: agg.leader_labels.clone(),
        });
    }

    fn evaluate_entry_gate(
        &self,
        agg: &AggregatedSignal,
    ) -> Result<EntryGateContext, SmartMoneyRejectReason> {
        let config = self.config();
        if self.signal_age_secs(agg) > config.max_signal_age_secs as i64 {
            return Err(SmartMoneyRejectReason::SignalTooOld);
        }
        if agg.max_wallet_weight < config.min_wallet_weight {
            return Err(SmartMoneyRejectReason::WalletWeightTooLow);
        }
        if agg.consensus_wallets < config.min_consensus_wallets {
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
        if best_ask > config.max_entry_price {
            return Err(SmartMoneyRejectReason::EntryPriceTooHigh);
        }
        let spread_bps = Self::compute_spread_bps(best_bid, best_ask);
        if spread_bps > Decimal::from(config.max_spread_bps) {
            return Err(SmartMoneyRejectReason::SpreadTooWide);
        }
        let top_level_depth_usdc =
            Self::top_level_ask_depth_usdc(&book).ok_or(SmartMoneyRejectReason::DepthTooThin)?;
        if top_level_depth_usdc < config.min_top_level_depth_usdc {
            return Err(SmartMoneyRejectReason::DepthTooThin);
        }
        let markets = self.markets.read().unwrap();
        let market = markets
            .get(&agg.condition_id)
            .cloned()
            .ok_or(SmartMoneyRejectReason::MissingOrderbook)?;
        if market.liquidity < config.min_market_liquidity {
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
        let max_shares = config.max_position_usdc / best_ask;
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

        let opportunity_id = Uuid::now_v7();
        let opp = TradingOpportunity {
            id: opportunity_id,
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
        record_smart_money_opportunity_attribution(
            opportunity_id,
            self.opportunity_attribution_slices(agg, gate.final_size),
        );
        Ok(opp)
    }

    /// Process an aggregated exit signal → TradingOpportunity.
    fn process_exit_signal(&self, agg: &AggregatedSignal) -> Option<TradingOpportunity> {
        if matches!(agg.signal_type, SignalType::Decrease)
            && agg.average_delta_ratio < self.config().leader_exit_min_delta_ratio
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
        let avg_cost = (self.get_held_positions)()
            .into_iter()
            .find(|(token_id, _, _)| *token_id == agg.token_id)
            .map(|(_, _, avg_cost)| avg_cost)
            .unwrap_or(best_bid);

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
        let executable_bid = floor_price_to_tick(best_bid, market.tick_size);
        let est = self.profit_calc.directional_sell_profit(
            executable_bid,
            avg_cost,
            sell_size,
            market.fee_rate_bps,
        );
        let attributed_leaders =
            self.attribute_exit_to_leaders(agg.token_id, sell_size, est.net_profit);
        record_smart_money_exit_decision(SmartMoneyExitDecision {
            recorded_at: self.now(),
            token_id: agg.token_id.to_string(),
            condition_id: format!("{:#x}", agg.condition_id),
            reason: "leader_exit".to_string(),
            question: market.question.clone(),
            best_bid: executable_bid,
            avg_cost,
            size: sell_size,
            estimated_profit: est.net_profit,
            attributed_leaders: attributed_leaders.clone(),
        });

        let opportunity_id = Uuid::now_v7();
        let opp = TradingOpportunity {
            id: opportunity_id,
            strategy_type: StrategyType::SmartMoney,
            condition_id: agg.condition_id,
            question: format!("[EXIT] {}", market.question),
            spread: Decimal::ZERO,
            estimated_profit: est.net_profit,
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
                price: executable_bid,
                size: sell_size,
                condition_id: agg.condition_id,
            },
        };
        record_smart_money_opportunity_attribution(opportunity_id, attributed_leaders.clone());
        Some(opp)
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

    fn record_entry_attribution(
        &self,
        token_id: U256,
        agg: &AggregatedSignal,
        filled_size: Decimal,
    ) {
        if filled_size <= Decimal::ZERO || agg.leader_contributions.is_empty() {
            return;
        }
        let total_weight: Decimal = agg
            .leader_contributions
            .iter()
            .map(|contribution| contribution.weighted_size.max(Decimal::ZERO))
            .sum();
        if total_weight <= Decimal::ZERO {
            return;
        }
        let mut exposures = self.leader_exposure_by_token.write().unwrap();
        let token_lots = exposures.entry(token_id).or_default();
        for contribution in &agg.leader_contributions {
            let ratio = contribution.weighted_size / total_weight;
            let attributed_size = filled_size * ratio;
            if attributed_size <= Decimal::ZERO {
                continue;
            }
            if let Some(existing) = token_lots
                .iter_mut()
                .find(|existing| existing.address == contribution.address)
            {
                existing.size += attributed_size;
                if existing.label.is_empty() {
                    existing.label = contribution.label.clone();
                }
            } else {
                token_lots.push(LeaderExposureLot {
                    address: contribution.address.clone(),
                    label: contribution.label.clone(),
                    size: attributed_size,
                });
            }
        }
        drop(exposures);
        self.publish_leader_attribution_snapshot();
    }

    fn opportunity_attribution_slices(
        &self,
        agg: &AggregatedSignal,
        size: Decimal,
    ) -> Vec<SmartMoneyLeaderAttributionSlice> {
        if size <= Decimal::ZERO || agg.leader_contributions.is_empty() {
            return Vec::new();
        }
        let total_weight: Decimal = agg
            .leader_contributions
            .iter()
            .map(|contribution| contribution.weighted_size.max(Decimal::ZERO))
            .sum();
        if total_weight <= Decimal::ZERO {
            return Vec::new();
        }
        agg.leader_contributions
            .iter()
            .filter_map(|contribution| {
                let ratio = contribution.weighted_size / total_weight;
                let attributed_size = size * ratio;
                if attributed_size <= Decimal::ZERO {
                    None
                } else {
                    Some(SmartMoneyLeaderAttributionSlice {
                        leader: if !contribution.label.is_empty() {
                            contribution.label.clone()
                        } else {
                            contribution.address.clone()
                        },
                        estimated_size: attributed_size,
                        estimated_profit: Decimal::ZERO,
                    })
                }
            })
            .collect()
    }

    fn attribute_exit_to_leaders(
        &self,
        token_id: U256,
        exit_size: Decimal,
        estimated_profit: Decimal,
    ) -> Vec<SmartMoneyLeaderAttributionSlice> {
        if exit_size <= Decimal::ZERO {
            return Vec::new();
        }
        let mut exposures = self.leader_exposure_by_token.write().unwrap();
        let Some(token_lots) = exposures.get_mut(&token_id) else {
            return Vec::new();
        };
        let total_open: Decimal = token_lots
            .iter()
            .map(|lot| lot.size.max(Decimal::ZERO))
            .sum();
        if total_open <= Decimal::ZERO {
            return Vec::new();
        }

        let attributed_exit_size = exit_size.min(total_open);
        let mut slices = Vec::new();
        let mut realized = self.leader_realized_totals.write().unwrap();

        for lot in token_lots.iter_mut() {
            if lot.size <= Decimal::ZERO {
                continue;
            }
            let ratio = lot.size / total_open;
            let leader_exit_size = attributed_exit_size * ratio;
            let leader_profit = estimated_profit * ratio;
            if leader_exit_size <= Decimal::ZERO {
                continue;
            }
            lot.size = (lot.size - leader_exit_size).max(Decimal::ZERO);
            let leader_key = if !lot.label.is_empty() {
                lot.label.clone()
            } else {
                lot.address.clone()
            };
            let entry = realized.entry(leader_key.clone()).or_default();
            if entry.label.is_empty() {
                entry.label = leader_key.clone();
            }
            entry.estimated_realized_pnl += leader_profit;
            entry.estimated_exited_size += leader_exit_size;
            entry.estimated_exit_count += 1;
            slices.push(SmartMoneyLeaderAttributionSlice {
                leader: leader_key,
                estimated_size: leader_exit_size,
                estimated_profit: leader_profit,
            });
        }

        token_lots.retain(|lot| lot.size > Decimal::ZERO);
        if token_lots.is_empty() {
            exposures.remove(&token_id);
        }
        drop(realized);
        drop(exposures);
        self.publish_leader_attribution_snapshot();
        slices
    }

    fn publish_leader_attribution_snapshot(&self) {
        let exposures = self.leader_exposure_by_token.read().unwrap();
        let realized = self.leader_realized_totals.read().unwrap();
        let mut open_sizes: HashMap<String, Decimal> = HashMap::new();
        for lots in exposures.values() {
            for lot in lots {
                let leader = if !lot.label.is_empty() {
                    lot.label.clone()
                } else {
                    lot.address.clone()
                };
                *open_sizes.entry(leader).or_insert(Decimal::ZERO) += lot.size;
            }
        }
        let mut rows: Vec<_> = realized
            .iter()
            .map(|(leader, totals)| SmartMoneyLeaderPnlAttributionEntry {
                leader: leader.clone(),
                estimated_open_size: open_sizes.remove(leader).unwrap_or(Decimal::ZERO),
                estimated_exited_size: totals.estimated_exited_size,
                estimated_realized_pnl: totals.estimated_realized_pnl,
                estimated_exit_count: totals.estimated_exit_count,
            })
            .collect();
        rows.extend(open_sizes.into_iter().map(|(leader, open_size)| {
            SmartMoneyLeaderPnlAttributionEntry {
                leader,
                estimated_open_size: open_size,
                estimated_exited_size: Decimal::ZERO,
                estimated_realized_pnl: Decimal::ZERO,
                estimated_exit_count: 0,
            }
        }));
        rows.sort_by(|a, b| {
            b.estimated_realized_pnl
                .cmp(&a.estimated_realized_pnl)
                .then_with(|| b.estimated_open_size.cmp(&a.estimated_open_size))
                .then_with(|| a.leader.cmp(&b.leader))
        });
        record_smart_money_leader_pnl_attribution(rows);
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
        tick_size: Decimal,
        reason: &str,
    ) -> TradingOpportunity {
        let executable_bid = floor_price_to_tick(best_bid, tick_size);
        let est = self
            .profit_calc
            .directional_sell_profit(executable_bid, avg_cost, size, fee_rate_bps);
        let attributed_leaders = self.attribute_exit_to_leaders(token_id, size, est.net_profit);
        tracing::info!(
            token_id = %token_id,
            best_bid = %best_bid,
            executable_bid = %executable_bid,
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
            best_bid: executable_bid,
            avg_cost,
            size,
            estimated_profit: est.net_profit,
            attributed_leaders: attributed_leaders.clone(),
        });
        let opportunity_id = Uuid::now_v7();
        let opp = TradingOpportunity {
            id: opportunity_id,
            strategy_type: StrategyType::SmartMoney,
            condition_id,
            question: format!("[EXIT:{reason}] {question}"),
            spread: executable_bid - avg_cost,
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
                price: executable_bid,
                size,
                condition_id,
            },
        };
        record_smart_money_opportunity_attribution(opportunity_id, attributed_leaders.clone());
        opp
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

            let market = markets
                .values()
                .find(|m| m.tokens.iter().any(|t| t.token_id == *token_id));
            let condition_id = market.map(|m| m.condition_id).unwrap_or_default();
            let question = market.map(|m| m.question.clone()).unwrap_or_default();
            let fee_rate_bps = market.map(|m| m.fee_rate_bps).unwrap_or(200);
            let tick_size = market.map(|m| m.tick_size).unwrap_or(Decimal::new(1, 2));
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
                * (Decimal::ONE + Self::bps_to_ratio(self.config().profit_protect_min_gain_bps));
            let profit_protect_floor = peak_bid
                * (Decimal::ONE - Self::bps_to_ratio(self.config().profit_protect_drawdown_bps));
            let drawdown_floor =
                *avg_cost * (Decimal::ONE - Self::bps_to_ratio(self.config().max_drawdown_bps));

            let exit_reason = if best_bid <= drawdown_floor {
                Some("drawdown")
            } else if peak_bid >= profit_trigger_price && best_bid <= profit_protect_floor {
                Some("profit_protect")
            } else if held_secs >= self.config().max_hold_secs as i64 {
                Some("stale_follow")
            } else if best_bid >= self.config().capital_efficiency_threshold {
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
                    tick_size,
                    reason,
                ));
            }
        }

        self.publish_leader_attribution_snapshot();

        exits
    }
}

fn contains_any_keyword(haystack: &str, keywords: &[String]) -> bool {
    let haystack = haystack.to_lowercase();
    keywords
        .iter()
        .map(|keyword| keyword.trim().to_lowercase())
        .filter(|keyword| !keyword.is_empty())
        .any(|keyword| haystack.contains(&keyword))
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
                        self.record_entry_attribution(agg.token_id, agg, opp.size);
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
    use arc_swap::ArcSwap;
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
            Arc::new(ArcSwap::from_pointee(config)),
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
            config: Arc::new(ArcSwap::from_pointee(config)),
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
            leader_exposure_by_token: Arc::new(RwLock::new(HashMap::new())),
            leader_realized_totals: Arc::new(RwLock::new(HashMap::new())),
        };

        let aggregated = strategy.aggregate_signals(&signals);
        let agg = aggregated.get(&U256::from(42u64)).unwrap();

        assert_eq!(agg.wallet_count, 2);
        // 1000 * 0.10 * 1.0 + 2000 * 0.10 * 0.5 = 100 + 100 = 200
        assert_eq!(agg.target_size, dec!(200));
    }

    #[test]
    fn test_leader_route_filters_mismatched_entry_signal() {
        let token_id = U256::from(42u64);
        let cid = B256::ZERO;
        let market = MarketInfo {
            category: Some("politics".into()),
            ..make_market(cid, token_id)
        };
        let config = SmartMoneyConfig {
            leader_routes: vec![pa_core::config::SmartMoneyLeaderRouteConfig {
                address: "0xaaa".into(),
                categories: vec!["crypto".into()],
                question_keywords: vec![],
                event_title_keywords: vec![],
            }],
            ..make_config()
        };
        let strategy = make_strategy_with_config(
            make_book(vec![(dec!(0.59), dec!(500))], vec![(dec!(0.60), dec!(500))]),
            Decimal::ZERO,
            dec!(1000),
            vec![],
            market,
            config,
        );
        let signals = vec![make_signal(
            SignalType::Entry,
            "0xaaa",
            Decimal::ONE,
            token_id,
            cid,
            dec!(500),
            dec!(500),
        )];

        let aggregated = strategy.aggregate_signals(&signals);
        assert!(aggregated.is_empty());
    }

    #[test]
    fn test_leader_route_allows_matching_entry_signal() {
        let token_id = U256::from(42u64);
        let cid = B256::ZERO;
        let market = MarketInfo {
            category: Some("crypto".into()),
            ..make_market(cid, token_id)
        };
        let config = SmartMoneyConfig {
            leader_routes: vec![pa_core::config::SmartMoneyLeaderRouteConfig {
                address: "0xaaa".into(),
                categories: vec!["crypto".into()],
                question_keywords: vec![],
                event_title_keywords: vec![],
            }],
            ..make_config()
        };
        let strategy = make_strategy_with_config(
            make_book(vec![(dec!(0.59), dec!(500))], vec![(dec!(0.60), dec!(500))]),
            Decimal::ZERO,
            dec!(1000),
            vec![],
            market,
            config,
        );
        let signals = vec![make_signal(
            SignalType::Entry,
            "0xaaa",
            Decimal::ONE,
            token_id,
            cid,
            dec!(500),
            dec!(500),
        )];

        let aggregated = strategy.aggregate_signals(&signals);
        assert!(aggregated.contains_key(&token_id));
    }

    #[test]
    fn test_leader_pnl_attribution_tracks_entry_and_exit() {
        pa_monitor::diagnostics::clear_smart_money_leader_pnl_attribution();
        let token_id = U256::from(42u64);
        let cid = B256::ZERO;
        let market = make_market(cid, token_id);
        let strategy = make_strategy_with_config(
            make_book(vec![(dec!(0.59), dec!(500))], vec![(dec!(0.60), dec!(500))]),
            Decimal::ZERO,
            dec!(1000),
            vec![],
            market.clone(),
            make_config(),
        );
        let signals = vec![make_signal(
            SignalType::Entry,
            "0xaaa",
            Decimal::ONE,
            token_id,
            cid,
            dec!(500),
            dec!(500),
        )];
        let aggregated = strategy.aggregate_signals(&signals);
        let agg = aggregated.get(&token_id).unwrap();
        let opp = strategy.process_entry_signal(agg).unwrap();
        strategy.record_entry_attribution(token_id, agg, opp.size);

        let snapshot = pa_monitor::diagnostics::smart_money_leader_pnl_attribution();
        assert_eq!(snapshot.len(), 1);
        assert!(snapshot[0].estimated_open_size > Decimal::ZERO);

        let exit_opp = strategy.build_exit_opportunity(
            token_id,
            dec!(20),
            dec!(0.50),
            dec!(0.70),
            cid,
            market.question,
            market.fee_rate_bps,
            market.tick_size,
            "capital_efficiency",
        );
        assert!(exit_opp.estimated_profit > Decimal::ZERO);
        let snapshot = pa_monitor::diagnostics::smart_money_leader_pnl_attribution();
        assert_eq!(snapshot.len(), 1);
        assert!(snapshot[0].estimated_realized_pnl > Decimal::ZERO);
        assert!(snapshot[0].estimated_open_size < opp.size);
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
            Arc::new(ArcSwap::from_pointee(config)),
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
            Arc::new(ArcSwap::from_pointee(SmartMoneyConfig {
                max_signal_age_secs: 90,
                ..make_config()
            })),
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
