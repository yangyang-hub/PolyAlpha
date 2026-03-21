//! Shared helper functions used across runtime modules.
//!
//! These are intentionally narrow utility functions that are reused by account,
//! market, and liquidity-rewards runtime code.

use alloy::primitives::{B256, U256};
use chrono::Utc;
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal_macros::dec;
use std::collections::HashSet;

use pa_core::weather::{SettlementValidationStatus, settlement_validation_status};
use pa_execution::clob_executor::ClobExecutor;
use pa_market_data::cache::OrderBookCache;
use pa_market_data::gamma_feed::GammaFeed;

use crate::app::types::AccountContext;

fn infer_crypto_asset(question: &str, event_title: Option<&str>) -> Option<String> {
    pa_strategy::crypto_alpha::parse_crypto_question(question)
        .map(|parsed| parsed.asset.name.to_string())
        .or_else(|| {
            event_title
                .and_then(pa_strategy::crypto_alpha::parse_crypto_event_title)
                .map(|(asset, _)| asset.name.to_string())
        })
}

fn infer_crypto_direction(question: &str, outcome: &str) -> Option<String> {
    pa_strategy::crypto_alpha::infer_crypto_direction_label(question, Some(outcome))
        .map(str::to_string)
}

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

    let mut token_map: HashMap<U256, (&str, &str, B256, Option<&str>)> = HashMap::new();
    for m in markets {
        for t in &m.tokens {
            let outcome = match t.outcome {
                pa_core::types::Outcome::Yes => "YES",
                pa_core::types::Outcome::No => "NO",
            };
            token_map.insert(
                t.token_id,
                (
                    &m.question,
                    outcome,
                    m.condition_id,
                    m.event_title.as_deref(),
                ),
            );
        }
    }

    let mut entries = Vec::new();
    for ctx in account_contexts {
        for (token_id, pe) in ctx.risk_manager_impl.snapshot_positions() {
            if pe.size < dec!(0.1) {
                continue;
            }
            let (question, outcome, _cid, event_title) = token_map
                .get(&token_id)
                .copied()
                .unwrap_or(("", "", B256::ZERO, None));
            let asset = infer_crypto_asset(question, event_title);
            let direction = if outcome.is_empty() {
                None
            } else {
                infer_crypto_direction(question, outcome)
            };

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
                asset,
                direction,
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
    const NEG_RISK_WS_CAP_MAX: usize = 80;

    #[derive(Clone, Copy)]
    struct CandidatePair {
        yes_tid: U256,
        no_tid: U256,
        priority: (u8, u8, u8, i64, i64),
    }

    fn price_priority(price: Option<f64>) -> (u8, i64) {
        match price {
            Some(p) => {
                let dist = ((p - 0.50_f64).abs() * 1_000_000.0) as i64;
                (0, dist)
            }
            None => (1, i64::MAX),
        }
    }

    fn weather_priority(
        m: &pa_core::types::MarketInfo,
        price: Option<f64>,
    ) -> Option<(u8, u8, u8, i64, i64)> {
        let parsed = pa_strategy::weather::parse_weather_question(&m.question)?;
        let validation_rank = match settlement_validation_status(&parsed.location) {
            SettlementValidationStatus::Validated => 0,
            SettlementValidationStatus::DefaultProtected => 1,
        };
        let chicago_penalty = if parsed.location == "Chicago" { 1 } else { 0 };
        let neg_risk_penalty = if m.neg_risk { 1 } else { 0 };
        let (price_rank, price_dist) = price_priority(price);
        let liquidity_rank = -(m.liquidity * dec!(100)).round().to_i64().unwrap_or(0);
        Some((
            validation_rank,
            chicago_penalty + neg_risk_penalty,
            price_rank,
            price_dist,
            liquidity_rank,
        ))
    }

    fn general_priority(
        m: &pa_core::types::MarketInfo,
        price: Option<f64>,
    ) -> (u8, u8, u8, i64, i64) {
        let neg_risk_penalty = if m.neg_risk { 1 } else { 0 };
        let (price_rank, price_dist) = price_priority(price);
        let liquidity_rank = -(m.liquidity * dec!(100)).round().to_i64().unwrap_or(0);
        (2, neg_risk_penalty, price_rank, price_dist, liquidity_rank)
    }

    let mut strategy_mid: Vec<CandidatePair> = Vec::new();
    let mut general_mid: Vec<CandidatePair> = Vec::new();
    let mut neg_risk_pairs: Vec<CandidatePair> = Vec::new();

    for m in markets {
        if m.tokens.len() != 2 || !m.active {
            continue;
        }

        let yes_price = m.gamma_best_ask.and_then(|p| p.to_f64()).or_else(|| {
            m.outcome_prices
                .as_ref()
                .and_then(|p| p.first().copied())
                .and_then(|p| p.to_f64())
        });

        if let Some(yp) = yes_price
            && !(0.05..=0.95).contains(&yp)
        {
            continue;
        }

        let pair = CandidatePair {
            yes_tid: m.tokens[0].token_id,
            no_tid: m.tokens[1].token_id,
            priority: if let Some(priority) = weather_priority(m, yes_price) {
                priority
            } else {
                general_priority(m, yes_price)
            },
        };

        if m.neg_risk {
            neg_risk_pairs.push(pair);
        } else if GammaFeed::is_relevant_for_strategies(&m.question, enabled_strategies) {
            strategy_mid.push(pair);
        } else {
            general_mid.push(pair);
        }
    }

    strategy_mid.sort_by(|a, b| a.priority.cmp(&b.priority));
    general_mid.sort_by(|a, b| a.priority.cmp(&b.priority));
    neg_risk_pairs.sort_by(|a, b| a.priority.cmp(&b.priority));

    let mut token_ids: Vec<U256> = Vec::new();
    let mut seen = HashSet::new();

    for tid in held_position_token_ids {
        if seen.insert(*tid) {
            token_ids.push(*tid);
        }
    }

    for pair in &strategy_mid {
        if seen.insert(pair.yes_tid) {
            token_ids.push(pair.yes_tid);
        }
        if seen.insert(pair.no_tid) {
            token_ids.push(pair.no_tid);
        }
    }
    for pair in &general_mid {
        if seen.insert(pair.yes_tid) {
            token_ids.push(pair.yes_tid);
        }
        if seen.insert(pair.no_tid) {
            token_ids.push(pair.no_tid);
        }
    }

    let neg_risk_budget = ws_max.min(NEG_RISK_WS_CAP_MAX).min(ws_max / 4);
    let mut neg_risk_added = 0usize;
    for pair in &neg_risk_pairs {
        if neg_risk_added >= neg_risk_budget {
            break;
        }
        if seen.insert(pair.yes_tid) {
            token_ids.push(pair.yes_tid);
            neg_risk_added += 1;
            if neg_risk_added >= neg_risk_budget {
                break;
            }
        }
        if seen.insert(pair.no_tid) {
            token_ids.push(pair.no_tid);
            neg_risk_added += 1;
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

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::B256;
    use pa_core::types::{MarketInfo, Outcome, TokenInfo};

    fn make_market(
        question: &str,
        yes_tid: u64,
        no_tid: u64,
        yes_price: Decimal,
        liquidity: Decimal,
    ) -> MarketInfo {
        MarketInfo {
            condition_id: B256::from([yes_tid as u8; 32]),
            question_id: B256::from([no_tid as u8; 32]),
            question: question.to_string(),
            neg_risk: false,
            neg_risk_market_id: None,
            tokens: vec![
                TokenInfo {
                    token_id: U256::from(yes_tid),
                    outcome: Outcome::Yes,
                    complement_id: U256::from(no_tid),
                },
                TokenInfo {
                    token_id: U256::from(no_tid),
                    outcome: Outcome::No,
                    complement_id: U256::from(yes_tid),
                },
            ],
            tick_size: dec!(0.01),
            fee_rate_bps: 200,
            active: true,
            liquidity,
            event_title: None,
            end_date: None,
            category: None,
            outcome_prices: Some(vec![yes_price, Decimal::ONE - yes_price]),
            gamma_best_bid: Some((yes_price - dec!(0.01)).max(dec!(0.01))),
            gamma_best_ask: Some(yes_price),
            rewards_min_size: None,
            rewards_max_spread: None,
            rewards_daily_rate: None,
            holding_rewards_enabled: false,
            fees_enabled: true,
        }
    }

    #[test]
    fn test_ws_token_list_prioritizes_validated_weather_city_over_default_protected() {
        let validated = make_market(
            "Will the highest temperature in Atlanta be between 70-71°F on March 19?",
            11,
            12,
            dec!(0.50),
            dec!(1000),
        );
        let protected = make_market(
            "Will the highest temperature in San Francisco be between 60-61°F on March 19?",
            21,
            22,
            dec!(0.50),
            dec!(1000),
        );

        let tokens = build_ws_token_list(&[protected, validated], &[], &["weather".into()], 2);
        assert_eq!(tokens, vec![U256::from(11u64), U256::from(12u64)]);
    }

    #[test]
    fn test_ws_token_list_caps_neg_risk_tokens_to_small_budget() {
        let mut markets = Vec::new();
        for i in 0..60u64 {
            let mut market = make_market(
                &format!(
                    "Will the highest temperature in Atlanta be between {i}-{i}°F on March 19?"
                ),
                100 + i * 2,
                101 + i * 2,
                dec!(0.50),
                dec!(1000),
            );
            market.neg_risk = true;
            markets.push(market);
        }

        let tokens = build_ws_token_list(&markets, &[], &["weather".into()], 200);
        assert_eq!(tokens.len(), 50);
    }
}
