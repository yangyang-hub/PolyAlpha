//! Liquidity Rewards task runtime.
//!
//! This module owns LR market selection, quoting, refresh/requote behavior,
//! fill handling, and runtime status publication for the monitor API.

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use alloy::signers::Signer as _;
use alloy::signers::local::PrivateKeySigner;
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal_macros::dec;
use tokio::sync::{RwLock, broadcast};
use tokio_util::sync::CancellationToken;

use pa_execution::clob_executor::ClobExecutor;
use pa_market_data::cache::OrderBookCache;
use pa_market_data::ws_feed::OrderBookUpdate;
use pa_monitor::api::LrRuntimeStatus;
use pa_risk::manager::RiskManagerImpl;
use pa_strategy::liquidity_rewards::RewardMarketCandidate;

use crate::app::helpers::fetch_clob_rewards;
use crate::app::types::LrOrderMeta;

type LrQuoteResult = (
    Vec<(String, LrOrderMeta)>,
    Decimal,
    Option<Decimal>,
    Option<Decimal>,
);

type LrCooldownMap =
    std::collections::HashMap<(alloy::primitives::U256, bool, Decimal), std::time::Instant>;

pub struct LrRuntimePrep {
    pub clob: ClobExecutor,
    pub effective_max_exposure: Decimal,
    pub cached_balance: Decimal,
    pub outstanding_orders: HashMap<alloy::primitives::B256, HashMap<String, LrOrderMeta>>,
    pub last_quoted_mid: HashMap<alloy::primitives::U256, Decimal>,
    pub token_to_condition: HashMap<alloy::primitives::U256, alloy::primitives::B256>,
    pub last_quote_time: HashMap<alloy::primitives::B256, std::time::Instant>,
    pub cooldown_map: HashMap<(alloy::primitives::U256, bool, Decimal), std::time::Instant>,
    pub cooldown_duration: Duration,
    pub active_candidates: Vec<RewardMarketCandidate>,
    pub cid_to_candidate_idx: HashMap<alloy::primitives::B256, usize>,
}

pub async fn prepare_liquidity_rewards_runtime(
    account_name: &str,
    config: &pa_core::config::LiquidityRewardsConfig,
    cache: &Arc<OrderBookCache>,
    risk_manager: &Arc<RiskManagerImpl>,
    shared_markets: &Arc<RwLock<Vec<pa_core::types::MarketInfo>>>,
    private_key: &str,
    clob_host: &str,
    signature_type: u8,
    chain_id: u64,
) -> Option<LrRuntimePrep> {
    let lr_signer = match PrivateKeySigner::from_str(private_key) {
        Ok(s) => s.with_chain_id(Some(chain_id)),
        Err(e) => {
            tracing::error!(account = %account_name, error = %e, "LR: failed to parse signer");
            return None;
        }
    };
    let lr_clob = match ClobExecutor::connect(clob_host, lr_signer, signature_type).await {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(account = %account_name, error = %e, "LR: CLOB auth failed");
            return None;
        }
    };
    tracing::info!(account = %account_name, "LR: CLOB authenticated, starting liquidity rewards");

    let effective_max_exposure = match lr_clob.get_balance().await {
        Ok(bal) => {
            let cap = bal.min(config.max_total_exposure);
            tracing::info!(
                account = %account_name,
                balance_usdc = %bal,
                config_max = %config.max_total_exposure,
                effective_cap = %cap,
                "LR: exposure cap set from balance"
            );
            cap
        }
        Err(e) => {
            tracing::warn!(account = %account_name, error = %e, "LR: failed to query balance, using config max");
            config.max_total_exposure
        }
    };

    let cooldown_duration = Duration::from_secs(config.failed_cooldown_secs);
    let cached_balance = effective_max_exposure;
    let mut outstanding_orders: HashMap<alloy::primitives::B256, HashMap<String, LrOrderMeta>> =
        HashMap::new();
    let mut last_quoted_mid: HashMap<alloy::primitives::U256, Decimal> = HashMap::new();
    let mut token_to_condition: HashMap<alloy::primitives::U256, alloy::primitives::B256> =
        HashMap::new();
    let mut last_quote_time: HashMap<alloy::primitives::B256, std::time::Instant> = HashMap::new();
    let cooldown_map: HashMap<(alloy::primitives::U256, bool, Decimal), std::time::Instant> =
        HashMap::new();

    let clob_rewards = match fetch_clob_rewards(&lr_clob).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "LR: Failed to fetch initial rewards");
            Vec::new()
        }
    };

    let markets_init = shared_markets.read().await;
    let active_candidates = pa_strategy::liquidity_rewards::select_reward_markets_hybrid(
        &markets_init,
        &clob_rewards,
        config,
    );
    drop(markets_init);

    let mut cid_to_candidate_idx: HashMap<alloy::primitives::B256, usize> = HashMap::new();
    for (idx, c) in active_candidates.iter().enumerate() {
        let cid = c.market.condition_id;
        cid_to_candidate_idx.insert(cid, idx);
        for t in &c.market.tokens {
            token_to_condition.insert(t.token_id, cid);
        }
    }

    let sides_being_quoted: u32 = active_candidates
        .iter()
        .map(|c| {
            let eff = pa_strategy::liquidity_rewards::effective_market_config(
                config,
                &c.market.condition_id,
            );
            let mut sides = 0u32;
            if c.market.tokens.len() == 2 {
                if eff.quote_yes {
                    sides += 1;
                }
                if eff.quote_no {
                    sides += 1;
                }
            } else {
                sides += c.market.tokens.len() as u32;
            }
            sides
        })
        .sum::<u32>()
        .max(1);

    pa_monitor::metrics::LR_ACTIVE_MARKETS.set(active_candidates.len() as f64);

    let mut total_exposure = Decimal::ZERO;
    for candidate in &active_candidates {
        let (metas, exp, yes_mid, no_mid) = lr_quote_one_market(
            &candidate.market,
            config,
            cache.as_ref(),
            risk_manager,
            &lr_clob,
            total_exposure,
            candidate.clob_rewards_max_spread,
            candidate.clob_rewards_min_size,
            effective_max_exposure,
            cached_balance,
            sides_being_quoted,
            &cooldown_map,
            cooldown_duration,
        )
        .await;
        total_exposure += exp;
        let cid = candidate.market.condition_id;
        if !metas.is_empty() {
            outstanding_orders.insert(cid, metas.into_iter().collect());
        }
        if candidate.market.tokens.len() >= 2 {
            if let Some(m) = yes_mid {
                last_quoted_mid.insert(candidate.market.tokens[0].token_id, m);
            }
            if let Some(m) = no_mid {
                last_quoted_mid.insert(candidate.market.tokens[1].token_id, m);
            }
        }
        last_quote_time.insert(cid, std::time::Instant::now());
    }

    tracing::info!(
        account = %account_name,
        active_markets = outstanding_orders.len(),
        total_candidates = active_candidates.len(),
        "LR: initial market selection and quote complete"
    );

    Some(LrRuntimePrep {
        clob: lr_clob,
        effective_max_exposure,
        cached_balance,
        outstanding_orders,
        last_quoted_mid,
        token_to_condition,
        last_quote_time,
        cooldown_map,
        cooldown_duration,
        active_candidates,
        cid_to_candidate_idx,
    })
}

/// Quote a single market for LR. Returns (order_id+meta pairs, exposure_added, yes_mid, no_mid).
pub async fn lr_quote_one_market(
    market: &pa_core::types::MarketInfo,
    config: &pa_core::config::LiquidityRewardsConfig,
    cache: &pa_market_data::cache::OrderBookCache,
    rm: &RiskManagerImpl,
    clob: &ClobExecutor,
    current_exposure: Decimal,
    rewards_max_spread: Decimal,
    rewards_min_size: Decimal,
    effective_max_exposure: Decimal,
    cached_balance: Decimal,
    sides_being_quoted: u32,
    cooldown_map: &LrCooldownMap,
    cooldown_duration: Duration,
) -> LrQuoteResult {
    let cid = market.condition_id;
    let eff = pa_strategy::liquidity_rewards::effective_market_config(config, &cid);

    let mut order_metas: Vec<(String, LrOrderMeta)> = Vec::new();
    let mut exposure_added = Decimal::ZERO;
    let mut first_mid_out: Option<Decimal> = None;
    let mut second_mid_out: Option<Decimal> = None;

    let token_pairs: Vec<(alloy::primitives::U256, bool, bool)> = if market.tokens.len() == 2 {
        let mut pairs = Vec::new();
        if eff.quote_yes {
            pairs.push((market.tokens[0].token_id, true, true));
        }
        if eff.quote_no {
            pairs.push((market.tokens[1].token_id, false, true));
        }
        pairs
    } else {
        market
            .tokens
            .iter()
            .map(|t| (t.token_id, true, true))
            .collect()
    };

    for (idx, &(tid, is_yes_side, _do_ask)) in token_pairs.iter().enumerate() {
        let position = rm.get_position_size(&tid);
        let Some(book) = cache.get(&tid) else {
            continue;
        };
        let Some(mid) = book.midpoint() else { continue };

        if idx == 0 {
            first_mid_out = Some(mid);
        }
        if idx == 1 {
            second_mid_out = Some(mid);
        }

        let quote_opt = if eff.order_depth_level > 0 {
            pa_strategy::liquidity_rewards::compute_depth_quotes(
                &book,
                eff.order_depth_level,
                rewards_max_spread,
                position,
                config,
                rewards_min_size,
            )
        } else {
            pa_strategy::liquidity_rewards::compute_quotes(
                mid,
                rewards_max_spread,
                position,
                config,
                rewards_min_size,
                market.tick_size,
            )
        };

        let Some(quote) = quote_opt else { continue };

        tracing::info!(
            market = %cid, side = if is_yes_side { "YES" } else { "NO" },
            token = %tid, midpoint = %mid,
            rewards_max_spread = %rewards_max_spread,
            bid = %quote.bid_price, ask = %quote.ask_price,
            "LR: computed quotes"
        );

        let remaining_pos = (eff.max_position_per_market - position).max(Decimal::ZERO);
        let remaining_exp =
            (effective_max_exposure - current_exposure - exposure_added).max(Decimal::ZERO);
        let max_from_exp = if quote.bid_price > Decimal::ZERO {
            remaining_exp / quote.bid_price
        } else {
            Decimal::ZERO
        };

        let balance_size = pa_strategy::liquidity_rewards::balance_aware_size(
            quote.bid_price,
            cached_balance,
            sides_being_quoted,
            eff.max_position_per_market,
            remaining_exp,
            config.min_order_size,
        );

        let bid_size = quote
            .size
            .min(remaining_pos)
            .min(max_from_exp)
            .min(balance_size.max(quote.size));
        let bid_size = if cached_balance > Decimal::ZERO {
            bid_size.min(balance_size)
        } else {
            bid_size
        };

        let bid_cooldown_key = (tid, true, quote.bid_price);
        let bid_on_cooldown = cooldown_map.get(&bid_cooldown_key).is_some_and(|&t| {
            pa_strategy::liquidity_rewards::is_order_on_cooldown(
                t,
                std::time::Instant::now(),
                cooldown_duration,
            )
        });

        if bid_size >= config.min_order_size && !bid_on_cooldown {
            match clob
                .buy_limit_post_only(tid, quote.bid_price, bid_size)
                .await
            {
                Ok(r) if !r.order_id.is_empty() => {
                    pa_monitor::metrics::LR_ORDERS_PLACED.inc();
                    exposure_added += bid_size * quote.bid_price;
                    order_metas.push((
                        r.order_id,
                        LrOrderMeta {
                            token_id: tid,
                            is_buy: true,
                            price: quote.bid_price,
                            size: bid_size,
                            last_synced_matched: Decimal::ZERO,
                        },
                    ));
                }
                Ok(_) => {}
                Err(e) => tracing::debug!(error = %e, market = %cid, "LR: bid failed for {}", tid),
            }
        }

        if position > Decimal::ZERO {
            let sell_size = quote.size.min(position);
            let ask_cooldown_key = (tid, false, quote.ask_price);
            let ask_on_cooldown = cooldown_map.get(&ask_cooldown_key).is_some_and(|&t| {
                pa_strategy::liquidity_rewards::is_order_on_cooldown(
                    t,
                    std::time::Instant::now(),
                    cooldown_duration,
                )
            });
            if sell_size >= config.min_order_size && !ask_on_cooldown {
                match clob
                    .sell_limit_post_only(tid, quote.ask_price, sell_size)
                    .await
                {
                    Ok(r) if !r.order_id.is_empty() => {
                        pa_monitor::metrics::LR_ORDERS_PLACED.inc();
                        order_metas.push((
                            r.order_id,
                            LrOrderMeta {
                                token_id: tid,
                                is_buy: false,
                                price: quote.ask_price,
                                size: sell_size,
                                last_synced_matched: Decimal::ZERO,
                            },
                        ));
                    }
                    Ok(_) => {}
                    Err(e) => {
                        tracing::debug!(error = %e, market = %cid, "LR: ask failed for {}", tid)
                    }
                }
            }
        }
    }

    (order_metas, exposure_added, first_mid_out, second_mid_out)
}

fn quoted_sides_count(
    candidates: &[RewardMarketCandidate],
    config: &pa_core::config::LiquidityRewardsConfig,
) -> u32 {
    candidates
        .iter()
        .map(|c| {
            let eff = pa_strategy::liquidity_rewards::effective_market_config(
                config,
                &c.market.condition_id,
            );
            if c.market.tokens.len() == 2 {
                (if eff.quote_yes { 1u32 } else { 0 }) + (if eff.quote_no { 1 } else { 0 })
            } else {
                c.market.tokens.len() as u32
            }
        })
        .sum::<u32>()
        .max(1)
}

async fn requote_active_candidates(
    candidates: &[RewardMarketCandidate],
    config: &pa_core::config::LiquidityRewardsConfig,
    cache: &Arc<OrderBookCache>,
    risk_manager: &Arc<RiskManagerImpl>,
    clob: &ClobExecutor,
    effective_max_exposure: Decimal,
    cached_balance: Decimal,
    outstanding_orders: &mut HashMap<alloy::primitives::B256, HashMap<String, LrOrderMeta>>,
    last_quoted_mid: &mut HashMap<alloy::primitives::U256, Decimal>,
    last_quote_time: &mut HashMap<alloy::primitives::B256, std::time::Instant>,
    cooldown_map: &LrCooldownMap,
    cooldown_duration: Duration,
) -> Decimal {
    let sides_being_quoted = quoted_sides_count(candidates, config);
    let mut total_exposure = Decimal::ZERO;
    for candidate in candidates {
        let cid = candidate.market.condition_id;
        let (metas, exp, yes_mid, no_mid) = lr_quote_one_market(
            &candidate.market,
            config,
            cache.as_ref(),
            risk_manager,
            clob,
            total_exposure,
            candidate.clob_rewards_max_spread,
            candidate.clob_rewards_min_size,
            effective_max_exposure,
            cached_balance,
            sides_being_quoted,
            cooldown_map,
            cooldown_duration,
        )
        .await;
        total_exposure += exp;
        if !metas.is_empty() {
            outstanding_orders.insert(cid, metas.into_iter().collect());
        }
        if candidate.market.tokens.len() >= 2 {
            if let Some(m) = yes_mid {
                last_quoted_mid.insert(candidate.market.tokens[0].token_id, m);
            }
            if let Some(m) = no_mid {
                last_quoted_mid.insert(candidate.market.tokens[1].token_id, m);
            }
        }
        last_quote_time.insert(cid, std::time::Instant::now());
    }
    total_exposure
}

async fn update_lr_runtime_status(
    status_ref: &Arc<RwLock<LrRuntimeStatus>>,
    candidates: &[RewardMarketCandidate],
    outstanding_orders: &HashMap<alloy::primitives::B256, HashMap<String, LrOrderMeta>>,
    total_exposure: Decimal,
    cached_balance: Decimal,
    market_mode: String,
) {
    let mut status = status_ref.write().await;
    status.active_markets = candidates
        .iter()
        .map(|c| {
            let cid = c.market.condition_id;
            let order_count = outstanding_orders.get(&cid).map_or(0, |m| m.len());
            pa_monitor::api::LrMarketStatus {
                condition_id: format!("{:#x}", cid),
                question: c.market.question.clone(),
                daily_rate: c.density * (c.market.liquidity + Decimal::ONE),
                outstanding_orders: order_count,
                yes_bid: None,
                yes_ask: None,
                no_bid: None,
                no_ask: None,
            }
        })
        .collect();
    status.total_exposure = total_exposure;
    status.cached_balance = cached_balance;
    status.market_mode = market_mode;
    status.last_refresh = Some(chrono::Utc::now());
}

pub async fn fallback_quote_refresh(
    account_name: &str,
    config: &pa_core::config::LiquidityRewardsConfig,
    cache: &Arc<OrderBookCache>,
    risk_manager: &Arc<RiskManagerImpl>,
    clob: &ClobExecutor,
    effective_max_exposure: Decimal,
    cached_balance: &mut Decimal,
    active_candidates: &[RewardMarketCandidate],
    outstanding_orders: &mut HashMap<alloy::primitives::B256, HashMap<String, LrOrderMeta>>,
    last_quoted_mid: &mut HashMap<alloy::primitives::U256, Decimal>,
    last_quote_time: &mut HashMap<alloy::primitives::B256, std::time::Instant>,
    cooldown_map: &mut LrCooldownMap,
    cooldown_duration: Duration,
    status_ref: &Arc<RwLock<LrRuntimeStatus>>,
) {
    let prev_ids: Vec<String> = outstanding_orders
        .drain()
        .flat_map(|(_, m)| m.into_keys())
        .collect();
    if !prev_ids.is_empty() {
        let refs: Vec<&str> = prev_ids.iter().map(|s| s.as_str()).collect();
        if let Err(e) = clob.cancel_orders(&refs).await {
            tracing::warn!(error = %e, "LR: batch cancel failed");
        }
        pa_monitor::metrics::LR_ORDERS_CANCELLED.inc_by(prev_ids.len() as u64);
    }

    last_quoted_mid.clear();

    if active_candidates.is_empty() {
        tracing::debug!(account = %account_name, "LR: no eligible reward markets");
        return;
    }

    if let Ok(bal) = clob.get_balance().await {
        *cached_balance = bal.min(config.max_total_exposure);
    }
    let now_cd = std::time::Instant::now();
    cooldown_map.retain(|_, t| now_cd.duration_since(*t) < cooldown_duration);

    let total_exposure = requote_active_candidates(
        active_candidates,
        config,
        cache,
        risk_manager,
        clob,
        effective_max_exposure,
        *cached_balance,
        outstanding_orders,
        last_quoted_mid,
        last_quote_time,
        cooldown_map,
        cooldown_duration,
    )
    .await;

    tracing::debug!(
        account = %account_name,
        active_markets = outstanding_orders.len(),
        "LR: fallback quote refresh complete"
    );

    update_lr_runtime_status(
        status_ref,
        active_candidates,
        outstanding_orders,
        total_exposure,
        *cached_balance,
        config.market_mode.clone(),
    )
    .await;

    if config.verify_scoring {
        let order_ids: Vec<String> = outstanding_orders
            .values()
            .flat_map(|m| m.keys().cloned())
            .collect();
        if !order_ids.is_empty() {
            tokio::time::sleep(Duration::from_secs(2)).await;
            let refs: Vec<&str> = order_ids.iter().map(|s| s.as_str()).collect();
            match clob.are_orders_scoring(&refs).await {
                Ok(scoring_map) => {
                    for (oid, scoring) in &scoring_map {
                        if !scoring {
                            pa_monitor::metrics::LR_ORDERS_NOT_SCORING.inc();
                            tracing::warn!(
                                account = %account_name,
                                order_id = %oid,
                                "LR: order NOT scoring"
                            );
                        }
                    }
                }
                Err(e) => tracing::debug!(error = %e, "LR: scoring check failed"),
            }
        }
    }
}

pub async fn market_reselection_refresh(
    account_name: &str,
    config: &pa_core::config::LiquidityRewardsConfig,
    cache: &Arc<OrderBookCache>,
    risk_manager: &Arc<RiskManagerImpl>,
    clob: &ClobExecutor,
    effective_max_exposure: Decimal,
    cached_balance: &mut Decimal,
    shared_markets: &Arc<RwLock<Vec<pa_core::types::MarketInfo>>>,
    active_candidates: &mut Vec<RewardMarketCandidate>,
    cid_to_candidate_idx: &mut HashMap<alloy::primitives::B256, usize>,
    token_to_condition: &mut HashMap<alloy::primitives::U256, alloy::primitives::B256>,
    outstanding_orders: &mut HashMap<alloy::primitives::B256, HashMap<String, LrOrderMeta>>,
    last_quoted_mid: &mut HashMap<alloy::primitives::U256, Decimal>,
    last_quote_time: &mut HashMap<alloy::primitives::B256, std::time::Instant>,
    cooldown_map: &LrCooldownMap,
    cooldown_duration: Duration,
) {
    let prev_ids: Vec<String> = outstanding_orders
        .drain()
        .flat_map(|(_, m)| m.into_keys())
        .collect();
    if !prev_ids.is_empty() {
        let refs: Vec<&str> = prev_ids.iter().map(|s| s.as_str()).collect();
        if let Err(e) = clob.cancel_orders(&refs).await {
            tracing::warn!(error = %e, "LR: market refresh cancel failed");
        }
        pa_monitor::metrics::LR_ORDERS_CANCELLED.inc_by(prev_ids.len() as u64);
    }

    let clob_rewards = match fetch_clob_rewards(clob).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "LR: Failed to refresh rewards");
            Vec::new()
        }
    };

    let markets_snapshot = shared_markets.read().await;
    *active_candidates = pa_strategy::liquidity_rewards::select_reward_markets_hybrid(
        &markets_snapshot,
        &clob_rewards,
        config,
    );
    drop(markets_snapshot);

    token_to_condition.clear();
    cid_to_candidate_idx.clear();
    last_quoted_mid.clear();
    last_quote_time.clear();
    for (idx, c) in active_candidates.iter().enumerate() {
        let cid = c.market.condition_id;
        cid_to_candidate_idx.insert(cid, idx);
        for t in &c.market.tokens {
            token_to_condition.insert(t.token_id, cid);
        }
    }

    pa_monitor::metrics::LR_ACTIVE_MARKETS.set(active_candidates.len() as f64);

    if let Ok(bal) = clob.get_balance().await {
        *cached_balance = bal.min(config.max_total_exposure);
    }

    let _total_exposure = requote_active_candidates(
        active_candidates,
        config,
        cache,
        risk_manager,
        clob,
        effective_max_exposure,
        *cached_balance,
        outstanding_orders,
        last_quoted_mid,
        last_quote_time,
        cooldown_map,
        cooldown_duration,
    )
    .await;

    tracing::info!(
        account = %account_name,
        active_markets = outstanding_orders.len(),
        "LR: market re-selection complete"
    );
}

pub async fn ws_requote_if_needed(
    account_name: &str,
    config: &pa_core::config::LiquidityRewardsConfig,
    cache: &Arc<OrderBookCache>,
    risk_manager: &Arc<RiskManagerImpl>,
    clob: &ClobExecutor,
    effective_max_exposure: Decimal,
    cached_balance: Decimal,
    token_id: alloy::primitives::U256,
    token_to_condition: &HashMap<alloy::primitives::U256, alloy::primitives::B256>,
    cid_to_candidate_idx: &HashMap<alloy::primitives::B256, usize>,
    active_candidates: &[RewardMarketCandidate],
    outstanding_orders: &mut HashMap<alloy::primitives::B256, HashMap<String, LrOrderMeta>>,
    last_quoted_mid: &mut HashMap<alloy::primitives::U256, Decimal>,
    last_quote_time: &mut HashMap<alloy::primitives::B256, std::time::Instant>,
    cooldown_map: &LrCooldownMap,
    cooldown_duration: Duration,
    requote_cooldown: Duration,
) {
    let Some(&cid) = token_to_condition.get(&token_id) else {
        return;
    };

    if config.order_depth_level > 0 {
        let now = std::time::Instant::now();
        if let Some(&last_t) = last_quote_time.get(&cid) {
            if now.duration_since(last_t) < requote_cooldown {
                return;
            }
        }

        let need_requote = outstanding_orders.get(&cid).is_some_and(|order_map| {
            order_map.values().any(|meta| {
                cache.get(&meta.token_id).is_some_and(|book| {
                    pa_strategy::liquidity_rewards::should_cancel_depth_order(
                        &book,
                        meta.price,
                        meta.is_buy,
                        config.cancel_depth_level,
                    )
                })
            })
        });

        if !need_requote {
            return;
        }

        tracing::info!(
            account = %account_name,
            token = %token_id,
            market = %cid,
            cancel_depth = config.cancel_depth_level,
            "LR: depth cancel triggered"
        );

        if let Some(order_map) = outstanding_orders.remove(&cid) {
            if !order_map.is_empty() {
                let id_strs: Vec<String> = order_map.into_keys().collect();
                let refs: Vec<&str> = id_strs.iter().map(|s| s.as_str()).collect();
                if let Err(e) = clob.cancel_orders(&refs).await {
                    tracing::warn!(error = %e, "LR: depth re-quote cancel failed");
                }
                pa_monitor::metrics::LR_ORDERS_CANCELLED.inc_by(id_strs.len() as u64);
            }
        }

        if let Some(&idx) = cid_to_candidate_idx.get(&cid) {
            if let Some(candidate) = active_candidates.get(idx) {
                let (metas, _exp, yes_mid, no_mid) = lr_quote_one_market(
                    &candidate.market,
                    config,
                    cache.as_ref(),
                    risk_manager,
                    clob,
                    Decimal::ZERO,
                    candidate.clob_rewards_max_spread,
                    candidate.clob_rewards_min_size,
                    effective_max_exposure,
                    cached_balance,
                    2,
                    cooldown_map,
                    cooldown_duration,
                )
                .await;
                if !metas.is_empty() {
                    outstanding_orders.insert(cid, metas.into_iter().collect());
                }
                if candidate.market.tokens.len() >= 2 {
                    if let Some(m) = yes_mid {
                        last_quoted_mid.insert(candidate.market.tokens[0].token_id, m);
                    }
                    if let Some(m) = no_mid {
                        last_quoted_mid.insert(candidate.market.tokens[1].token_id, m);
                    }
                }
                last_quote_time.insert(cid, now);
            }
        }
    } else {
        let Some(new_mid) = cache.get(&token_id).and_then(|b| b.midpoint()) else {
            return;
        };
        let Some(&old_mid) = last_quoted_mid.get(&token_id) else {
            return;
        };
        if old_mid <= Decimal::ZERO {
            return;
        }

        let drift_bps = ((new_mid - old_mid).abs() / old_mid * dec!(10000))
            .to_u32()
            .unwrap_or(0);
        if drift_bps < config.requote_trigger_bps {
            return;
        }

        let now = std::time::Instant::now();
        if let Some(&last_t) = last_quote_time.get(&cid) {
            if now.duration_since(last_t) < requote_cooldown {
                return;
            }
        }

        tracing::info!(
            account = %account_name,
            token = %token_id,
            market = %cid,
            old_mid = %old_mid,
            new_mid = %new_mid,
            drift_bps = drift_bps,
            "LR: WS re-quote triggered"
        );

        if let Some(order_map) = outstanding_orders.remove(&cid) {
            if !order_map.is_empty() {
                let id_strs: Vec<String> = order_map.into_keys().collect();
                let refs: Vec<&str> = id_strs.iter().map(|s| s.as_str()).collect();
                if let Err(e) = clob.cancel_orders(&refs).await {
                    tracing::warn!(error = %e, "LR: WS re-quote cancel failed");
                }
                pa_monitor::metrics::LR_ORDERS_CANCELLED.inc_by(id_strs.len() as u64);
            }
        }

        last_quoted_mid.remove(&token_id);

        if let Some(&idx) = cid_to_candidate_idx.get(&cid) {
            if let Some(candidate) = active_candidates.get(idx) {
                let (metas, _exp, yes_mid, no_mid) = lr_quote_one_market(
                    &candidate.market,
                    config,
                    cache.as_ref(),
                    risk_manager,
                    clob,
                    Decimal::ZERO,
                    candidate.clob_rewards_max_spread,
                    candidate.clob_rewards_min_size,
                    effective_max_exposure,
                    cached_balance,
                    2,
                    cooldown_map,
                    cooldown_duration,
                )
                .await;
                if !metas.is_empty() {
                    outstanding_orders.insert(cid, metas.into_iter().collect());
                }
                if candidate.market.tokens.len() >= 2 {
                    if let Some(m) = yes_mid {
                        last_quoted_mid.insert(candidate.market.tokens[0].token_id, m);
                    }
                    if let Some(m) = no_mid {
                        last_quoted_mid.insert(candidate.market.tokens[1].token_id, m);
                    }
                }
                last_quote_time.insert(cid, now);
            }
        }
    }
}

pub async fn fill_check_refresh(
    cache: &Arc<OrderBookCache>,
    risk_manager: &Arc<RiskManagerImpl>,
    clob: &ClobExecutor,
    config: &pa_core::config::LiquidityRewardsConfig,
    effective_max_exposure: Decimal,
    cached_balance: Decimal,
    active_candidates: &[RewardMarketCandidate],
    cid_to_candidate_idx: &HashMap<alloy::primitives::B256, usize>,
    outstanding_orders: &mut HashMap<alloy::primitives::B256, HashMap<String, LrOrderMeta>>,
    last_quoted_mid: &mut HashMap<alloy::primitives::U256, Decimal>,
    last_quote_time: &mut HashMap<alloy::primitives::B256, std::time::Instant>,
    cooldown_map: &LrCooldownMap,
    cooldown_duration: Duration,
) {
    let cids: Vec<alloy::primitives::B256> = outstanding_orders.keys().cloned().collect();
    for cid in cids {
        let api_orders = match clob.get_orders_by_market(cid).await {
            Ok(o) => o,
            Err(e) => {
                tracing::debug!(error = %e, market = %cid, "LR: fill check query failed");
                continue;
            }
        };

        let tracked = match outstanding_orders.get_mut(&cid) {
            Some(m) => m,
            None => continue,
        };

        let mut any_full_fill = false;
        let mut fully_filled_ids: Vec<String> = Vec::new();

        for (oid, meta) in tracked.iter_mut() {
            let api_match = api_orders.iter().find(|o| o.order_id == *oid);

            let (is_fully_done, api_matched_size) = match api_match {
                Some(o) => (o.is_matched, o.size_matched),
                None => (true, meta.size),
            };

            let delta = api_matched_size - meta.last_synced_matched;
            if delta > Decimal::ZERO {
                let current_pos = risk_manager.get_position_size(&meta.token_id);
                let new_pos = if meta.is_buy {
                    current_pos + delta
                } else {
                    (current_pos - delta).max(Decimal::ZERO)
                };
                risk_manager.sync_position(
                    meta.token_id,
                    new_pos,
                    meta.price,
                    Some(pa_core::types::StrategyType::LiquidityRewards),
                    Some(cid),
                );
                meta.last_synced_matched = api_matched_size;
                pa_monitor::metrics::LR_FILLS_DETECTED.inc();
                tracing::info!(
                    market = %cid,
                    token = %meta.token_id,
                    side = if meta.is_buy { "buy" } else { "sell" },
                    delta = %delta,
                    total_matched = %api_matched_size,
                    new_pos = %new_pos,
                    fully_done = is_fully_done,
                    "LR: fill detected, position synced"
                );
            }

            if is_fully_done {
                fully_filled_ids.push(oid.clone());
                any_full_fill = true;
            }
        }

        for id in &fully_filled_ids {
            tracked.remove(id);
        }

        if any_full_fill {
            let remaining_ids: Vec<String> = tracked.keys().cloned().collect();
            if !remaining_ids.is_empty() {
                let refs: Vec<&str> = remaining_ids.iter().map(|s| s.as_str()).collect();
                if let Err(e) = clob.cancel_orders(&refs).await {
                    tracing::debug!(error = %e, "LR: fill re-quote cancel failed");
                }
                pa_monitor::metrics::LR_ORDERS_CANCELLED.inc_by(remaining_ids.len() as u64);
            }
            outstanding_orders.remove(&cid);

            if let Some(&idx) = cid_to_candidate_idx.get(&cid) {
                if let Some(candidate) = active_candidates.get(idx) {
                    let (metas, _exp, yes_mid, no_mid) = lr_quote_one_market(
                        &candidate.market,
                        config,
                        cache.as_ref(),
                        risk_manager,
                        clob,
                        Decimal::ZERO,
                        candidate.clob_rewards_max_spread,
                        candidate.clob_rewards_min_size,
                        effective_max_exposure,
                        cached_balance,
                        2,
                        cooldown_map,
                        cooldown_duration,
                    )
                    .await;
                    if !metas.is_empty() {
                        outstanding_orders.insert(cid, metas.into_iter().collect());
                    }
                    if candidate.market.tokens.len() >= 2 {
                        if let Some(m) = yes_mid {
                            last_quoted_mid.insert(candidate.market.tokens[0].token_id, m);
                        }
                        if let Some(m) = no_mid {
                            last_quoted_mid.insert(candidate.market.tokens[1].token_id, m);
                        }
                    }
                    last_quote_time.insert(cid, std::time::Instant::now());
                    pa_monitor::metrics::LR_FILL_REQUOTES.inc();
                }
            }
        }
    }
}

pub fn spawn_liquidity_rewards_task(
    account_name: String,
    config: pa_core::config::LiquidityRewardsConfig,
    cache: Arc<OrderBookCache>,
    cancel: CancellationToken,
    risk_manager: Arc<RiskManagerImpl>,
    shared_markets: Arc<RwLock<Vec<pa_core::types::MarketInfo>>>,
    private_key: String,
    clob_host: String,
    signature_type: u8,
    chain_id: u64,
    update_rx: broadcast::Receiver<OrderBookUpdate>,
    status_ref: Arc<RwLock<LrRuntimeStatus>>,
) {
    tokio::spawn(async move {
        let Some(prep) = prepare_liquidity_rewards_runtime(
            &account_name,
            &config,
            &cache,
            &risk_manager,
            &shared_markets,
            &private_key,
            &clob_host,
            signature_type,
            chain_id,
        )
        .await
        else {
            return;
        };

        let clob = prep.clob;
        let effective_max_exposure = prep.effective_max_exposure;
        let mut cached_balance = prep.cached_balance;
        let mut outstanding_orders = prep.outstanding_orders;
        let mut last_quoted_mid = prep.last_quoted_mid;
        let mut token_to_condition = prep.token_to_condition;
        let mut last_quote_time = prep.last_quote_time;
        let mut cooldown_map = prep.cooldown_map;
        let cooldown_duration = prep.cooldown_duration;
        let mut active_candidates = prep.active_candidates;
        let mut cid_to_candidate_idx = prep.cid_to_candidate_idx;
        let mut update_rx = update_rx;

        let mut fallback_interval =
            tokio::time::interval(Duration::from_secs(config.quote_refresh_secs));
        let mut market_interval =
            tokio::time::interval(Duration::from_secs(config.market_refresh_secs));
        let requote_cooldown = Duration::from_secs(config.requote_cooldown_secs);
        let fill_check_enabled = config.fill_check_secs > 0;
        let mut fill_check_interval =
            tokio::time::interval(Duration::from_secs(if fill_check_enabled {
                config.fill_check_secs
            } else {
                86400
            }));

        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    let all_ids: Vec<String> = outstanding_orders.values()
                        .flat_map(|m| m.keys().cloned())
                        .collect();
                    if !all_ids.is_empty() {
                        let refs: Vec<&str> = all_ids.iter().map(|s| s.as_str()).collect();
                        if let Err(e) = clob.cancel_orders(&refs).await {
                            tracing::warn!(account = %account_name, error = %e, "LR: cancel on shutdown failed");
                        }
                        pa_monitor::metrics::LR_ORDERS_CANCELLED.inc_by(all_ids.len() as u64);
                    }
                    tracing::info!(account = %account_name, "LR: shutdown, cancelled {} orders", all_ids.len());
                    break;
                }
                result = update_rx.recv() => {
                    if let Ok(update) = result {
                        ws_requote_if_needed(
                            &account_name,
                            &config,
                            &cache,
                            &risk_manager,
                            &clob,
                            effective_max_exposure,
                            cached_balance,
                            update.token_id,
                            &token_to_condition,
                            &cid_to_candidate_idx,
                            &active_candidates,
                            &mut outstanding_orders,
                            &mut last_quoted_mid,
                            &mut last_quote_time,
                            &cooldown_map,
                            cooldown_duration,
                            requote_cooldown,
                        ).await;
                    }
                }
                _ = fallback_interval.tick() => {
                    fallback_quote_refresh(
                        &account_name,
                        &config,
                        &cache,
                        &risk_manager,
                        &clob,
                        effective_max_exposure,
                        &mut cached_balance,
                        &active_candidates,
                        &mut outstanding_orders,
                        &mut last_quoted_mid,
                        &mut last_quote_time,
                        &mut cooldown_map,
                        cooldown_duration,
                        &status_ref,
                    ).await;
                }
                _ = market_interval.tick() => {
                    market_reselection_refresh(
                        &account_name,
                        &config,
                        &cache,
                        &risk_manager,
                        &clob,
                        effective_max_exposure,
                        &mut cached_balance,
                        &shared_markets,
                        &mut active_candidates,
                        &mut cid_to_candidate_idx,
                        &mut token_to_condition,
                        &mut outstanding_orders,
                        &mut last_quoted_mid,
                        &mut last_quote_time,
                        &cooldown_map,
                        cooldown_duration,
                    ).await;
                }
                _ = fill_check_interval.tick(), if fill_check_enabled => {
                    fill_check_refresh(
                        &cache,
                        &risk_manager,
                        &clob,
                        &config,
                        effective_max_exposure,
                        cached_balance,
                        &active_candidates,
                        &cid_to_candidate_idx,
                        &mut outstanding_orders,
                        &mut last_quoted_mid,
                        &mut last_quote_time,
                        &cooldown_map,
                        cooldown_duration,
                    ).await;
                }
            }
        }
    });
}
