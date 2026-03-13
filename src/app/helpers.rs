//! Shared helper functions used across runtime modules.
//!
//! These are intentionally narrow utility functions that are reused by account,
//! market, and liquidity-rewards runtime code.

use alloy::primitives::{B256, U256};
use chrono::Utc;
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal_macros::dec;

use pa_execution::clob_executor::ClobExecutor;
use pa_market_data::cache::OrderBookCache;
use pa_market_data::gamma_feed::GammaFeed;

use crate::app::types::AccountContext;

/// Fetch current liquidity rewards from the CLOB API.
///
/// Returns a list of markets with active rewards, including their reward parameters
/// (max_spread, min_size, total_daily_rate).
pub async fn fetch_clob_rewards(
    clob: &ClobExecutor,
) -> anyhow::Result<Vec<pa_strategy::liquidity_rewards::ClobRewardData>> {
    let mut all_rewards = Vec::new();
    let mut next_cursor: Option<String> = None;

    tracing::debug!("Fetching CLOB rewards with next_cursor={:?}", next_cursor);

    loop {
        let page = match clob.current_rewards(next_cursor.clone()).await {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    next_cursor = ?next_cursor,
                    "LR: Failed to fetch CLOB rewards, API may have changed"
                );
                return Ok(all_rewards);
            }
        };

        for reward in page.data {
            let total_daily_rate: Decimal =
                reward.rewards_config.iter().map(|r| r.rate_per_day).sum();

            all_rewards.push(pa_strategy::liquidity_rewards::ClobRewardData {
                condition_id: reward.condition_id,
                rewards_max_spread: reward.rewards_max_spread / Decimal::from(100),
                rewards_min_size: reward.rewards_min_size,
                total_daily_rate,
            });
        }

        if page.next_cursor.is_empty() || page.next_cursor == "LTE=" {
            break;
        }
        next_cursor = Some(page.next_cursor.clone());
    }

    tracing::info!(
        count = all_rewards.len(),
        "LR: Fetched CLOB rewards markets from API"
    );

    Ok(all_rewards)
}

/// Build a snapshot of all positions across all accounts for the API.
pub fn build_position_snapshot(
    account_contexts: &[AccountContext],
    markets: &[pa_core::types::MarketInfo],
    cache: &OrderBookCache,
) -> Vec<pa_monitor::api::PositionApiEntry> {
    use std::collections::HashMap;

    let mut token_map: HashMap<U256, (&str, &str, B256)> = HashMap::new();
    for m in markets {
        for t in &m.tokens {
            let outcome = match t.outcome {
                pa_core::types::Outcome::Yes => "YES",
                pa_core::types::Outcome::No => "NO",
            };
            token_map.insert(t.token_id, (&m.question, outcome, m.condition_id));
        }
    }

    let mut entries = Vec::new();
    for ctx in account_contexts {
        for (token_id, pe) in ctx.risk_manager_impl.snapshot_positions() {
            if pe.size < dec!(0.1) {
                continue;
            }
            let (question, outcome, _cid) =
                token_map
                    .get(&token_id)
                    .copied()
                    .unwrap_or(("", "", B256::ZERO));

            let current_price = cache
                .get(&token_id)
                .and_then(|ob| ob.bids.first().map(|b| b.price));

            let unrealized_pnl = current_price.map(|p| pe.size * (p - pe.avg_cost));

            let strategy_name = pe.strategy_type.map(|st| match st {
                pa_core::types::StrategyType::Weather => "weather",
                pa_core::types::StrategyType::CryptoAlpha => "crypto_alpha",
                pa_core::types::StrategyType::LiquidityRewards => "liquidity_rewards",
                pa_core::types::StrategyType::SmartMoney => "smart_money",
            });

            entries.push(pa_monitor::api::PositionApiEntry {
                token_id: format!("{:#x}", token_id),
                size: pe.size,
                avg_cost: pe.avg_cost,
                cost_basis: pe.size * pe.avg_cost,
                strategy: strategy_name.map(|s| s.to_string()),
                condition_id: pe.condition_id.map(|c| format!("{:#x}", c)),
                question: if question.is_empty() {
                    None
                } else {
                    Some(question.to_string())
                },
                outcome: if outcome.is_empty() {
                    None
                } else {
                    Some(outcome.to_string())
                },
                current_price,
                unrealized_pnl,
            });
        }
    }
    entries
}

/// Seed the OrderBookCache for a single market using its gamma prices.
pub fn seed_market_cache(cache: &OrderBookCache, m: &pa_core::types::MarketInfo) -> bool {
    if m.tokens.len() < 2 {
        return false;
    }

    let (yes_bid, yes_ask) = if let (Some(bid), Some(ask)) = (m.gamma_best_bid, m.gamma_best_ask) {
        if ask > Decimal::ZERO && ask <= Decimal::ONE && bid > Decimal::ZERO {
            (bid, ask)
        } else {
            match m.outcome_prices.as_ref().and_then(|p| p.first().copied()) {
                Some(yp) if yp > Decimal::ZERO && yp < Decimal::ONE => {
                    ((yp - dec!(0.01)).max(dec!(0.01)), yp)
                }
                _ => return false,
            }
        }
    } else {
        match m.outcome_prices.as_ref().and_then(|p| p.first().copied()) {
            Some(yp) if yp > Decimal::ZERO && yp < Decimal::ONE => {
                ((yp - dec!(0.01)).max(dec!(0.01)), yp)
            }
            _ => return false,
        }
    };

    let no_ask = (Decimal::ONE - yes_bid).min(dec!(0.99));
    let no_bid = (Decimal::ONE - yes_ask).max(dec!(0.01));

    cache.update(
        m.tokens[0].token_id,
        pa_core::types::OrderBook {
            token_id: m.tokens[0].token_id,
            bids: vec![pa_core::types::PriceLevel {
                price: yes_bid,
                size: dec!(1000),
            }],
            asks: vec![pa_core::types::PriceLevel {
                price: yes_ask,
                size: dec!(1000),
            }],
            timestamp: Utc::now(),
        },
    );

    cache.update(
        m.tokens[1].token_id,
        pa_core::types::OrderBook {
            token_id: m.tokens[1].token_id,
            bids: vec![pa_core::types::PriceLevel {
                price: no_bid,
                size: dec!(1000),
            }],
            asks: vec![pa_core::types::PriceLevel {
                price: no_ask,
                size: dec!(1000),
            }],
            timestamp: Utc::now(),
        },
    );

    true
}

/// Build the smart-ordered WS token subscription list.
pub fn build_ws_token_list(
    markets: &[pa_core::types::MarketInfo],
    held_position_token_ids: &[U256],
    enabled_strategies: &[String],
    ws_max: usize,
) -> Vec<U256> {
    let mut strategy_mid: Vec<(U256, U256, f64)> = Vec::new();
    let mut general_mid: Vec<(U256, U256, f64)> = Vec::new();

    for m in markets {
        if m.neg_risk || m.tokens.len() != 2 || !m.active {
            continue;
        }

        let yes_price = m.gamma_best_ask.and_then(|p| p.to_f64()).or_else(|| {
            m.outcome_prices
                .as_ref()
                .and_then(|p| p.first().copied())
                .and_then(|p| p.to_f64())
        });

        if let Some(yp) = yes_price {
            if !(0.05..=0.95).contains(&yp) {
                continue;
            }
            let dist = (yp - 0.50_f64).abs();
            if GammaFeed::is_relevant_for_strategies(&m.question, enabled_strategies) {
                strategy_mid.push((m.tokens[0].token_id, m.tokens[1].token_id, dist));
            } else {
                general_mid.push((m.tokens[0].token_id, m.tokens[1].token_id, dist));
            }
        } else if GammaFeed::is_relevant_for_strategies(&m.question, enabled_strategies) {
            strategy_mid.push((m.tokens[0].token_id, m.tokens[1].token_id, 1.0));
        } else {
            general_mid.push((m.tokens[0].token_id, m.tokens[1].token_id, 1.0));
        }
    }

    strategy_mid.sort_by(|a, b| a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal));
    general_mid.sort_by(|a, b| a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal));

    let mut token_ids: Vec<U256> = Vec::new();

    for tid in held_position_token_ids {
        if !token_ids.contains(tid) {
            token_ids.push(*tid);
        }
    }

    for (yes_tid, no_tid, _) in &strategy_mid {
        if !token_ids.contains(yes_tid) {
            token_ids.push(*yes_tid);
        }
        if !token_ids.contains(no_tid) {
            token_ids.push(*no_tid);
        }
    }
    for (yes_tid, no_tid, _) in &general_mid {
        if !token_ids.contains(yes_tid) {
            token_ids.push(*yes_tid);
        }
        if !token_ids.contains(no_tid) {
            token_ids.push(*no_tid);
        }
    }

    let neg_risk_token_ids: Vec<_> = markets
        .iter()
        .filter(|m| m.neg_risk)
        .flat_map(|m| m.tokens.iter().map(|t| t.token_id))
        .collect();
    for tid in &neg_risk_token_ids {
        if !token_ids.contains(tid) {
            token_ids.push(*tid);
        }
    }

    token_ids.truncate(ws_max);
    token_ids
}

/// Infer strategy_type for a loaded position by matching its token_id against discovered markets.
pub fn infer_strategy_type(
    token_id: U256,
    markets: &[pa_core::types::MarketInfo],
    neg_risk_events: &[pa_core::types::NegRiskEvent],
) -> Option<pa_core::types::StrategyType> {
    use pa_core::types::StrategyType;

    for event in neg_risk_events {
        let has_token = event
            .markets
            .iter()
            .any(|m| m.tokens.iter().any(|t| t.token_id == token_id));
        if has_token {
            if pa_strategy::weather::parse_weather_event_title(&event.title).is_some() {
                return Some(StrategyType::Weather);
            }
            if pa_strategy::crypto_alpha::parse_crypto_event_title(&event.title).is_some() {
                return Some(StrategyType::CryptoAlpha);
            }
        }
    }

    for market in markets {
        let has_token = market.tokens.iter().any(|t| t.token_id == token_id);
        if !has_token {
            continue;
        }

        if pa_strategy::weather::parse_weather_question(&market.question).is_some() {
            return Some(StrategyType::Weather);
        }
        if pa_strategy::crypto_alpha::parse_crypto_question(&market.question).is_some() {
            return Some(StrategyType::CryptoAlpha);
        }
    }

    None
}
