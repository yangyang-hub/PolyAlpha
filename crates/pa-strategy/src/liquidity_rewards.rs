use pa_core::config::LiquidityRewardsConfig;
use pa_core::types::MarketInfo;
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;

// ──── Market Selection ────

/// A reward market candidate with its computed score.
#[derive(Debug, Clone)]
pub struct RewardMarketCandidate {
    pub market: MarketInfo,
    /// reward_density = daily_rate / (liquidity + 1)
    pub density: Decimal,
    /// CLOB API rewards_max_spread (authoritative source for quoting).
    pub clob_rewards_max_spread: Decimal,
    /// CLOB API rewards_min_size.
    pub clob_rewards_min_size: Decimal,
}

/// Select and rank markets eligible for liquidity rewards.
///
/// Filters: active, binary (2 tokens), not neg_risk, has reward params,
/// daily rate >= min_daily_rate. Sorted by reward density descending.
pub fn select_reward_markets(
    markets: &[MarketInfo],
    config: &LiquidityRewardsConfig,
) -> Vec<RewardMarketCandidate> {
    let mut candidates: Vec<RewardMarketCandidate> = markets
        .iter()
        .filter(|m| {
            m.active
                && m.tokens.len() == 2
                && !m.neg_risk
                && m.rewards_min_size.is_some()
                && m.rewards_max_spread.is_some()
                && m.rewards_daily_rate
                    .map_or(false, |r| r >= config.min_daily_rate)
        })
        .map(|m| {
            let daily_rate = m.rewards_daily_rate.unwrap_or(Decimal::ZERO);
            let density = daily_rate / (m.liquidity + Decimal::ONE);
            RewardMarketCandidate {
                market: m.clone(),
                density,
                clob_rewards_max_spread: m.rewards_max_spread.unwrap_or(Decimal::ZERO),
                clob_rewards_min_size: m.rewards_min_size.unwrap_or(Decimal::ZERO),
            }
        })
        .collect();

    candidates.sort_by(|a, b| b.density.partial_cmp(&a.density).unwrap_or(std::cmp::Ordering::Equal));
    candidates.truncate(config.max_markets);
    candidates
}



/// Reward market data from the CLOB API.
#[derive(Debug, Clone)]
pub struct ClobRewardData {
    pub condition_id: alloy::primitives::B256,
    pub rewards_max_spread: Decimal,
    pub rewards_min_size: Decimal,
    pub total_daily_rate: Decimal,
}

/// Select and rank markets eligible for liquidity rewards using CLOB API data.
///
/// This function uses the CLOB API rewards endpoint data instead of relying on
/// Gamma API fields which are no longer populated.
///
/// Filters: active, binary (2 tokens), not neg_risk, in clob_rewards list,
/// daily rate >= min_daily_rate. Sorted by reward density descending.
pub fn select_reward_markets_with_clob_data(
    markets: &[MarketInfo],
    clob_rewards: &[ClobRewardData],
    config: &LiquidityRewardsConfig,
) -> Vec<RewardMarketCandidate> {
    use std::collections::HashSet;
    
    // Build a HashSet of condition IDs with active rewards for O(1) lookup
    let reward_conditions: HashSet<alloy::primitives::B256> = clob_rewards
        .iter()
        .map(|r| r.condition_id)
        .collect();
    
    // Build a HashMap for reward data lookup
    let reward_map: std::collections::HashMap<alloy::primitives::B256, &ClobRewardData> = clob_rewards
        .iter()
        .map(|r| (r.condition_id, r))
        .collect();

    let mut candidates: Vec<RewardMarketCandidate> = markets
        .iter()
        .filter(|m| {
            m.active
                && m.tokens.len() == 2
                && !m.neg_risk
                && reward_conditions.contains(&m.condition_id)
        })
        .filter_map(|m| {
            let reward_data = reward_map.get(&m.condition_id)?;
            let daily_rate = reward_data.total_daily_rate;
            
            // Filter by min_daily_rate
            if daily_rate < config.min_daily_rate {
                return None;
            }
            
            let density = daily_rate / (m.liquidity + Decimal::ONE);
            Some(RewardMarketCandidate {
                market: m.clone(),
                density,
                clob_rewards_max_spread: reward_data.rewards_max_spread,
                clob_rewards_min_size: reward_data.rewards_min_size,
            })
        })
        .collect();

    candidates.sort_by(|a, b| b.density.partial_cmp(&a.density).unwrap_or(std::cmp::Ordering::Equal));
    candidates.truncate(config.max_markets);
    candidates
}

// ──── Quote Computation ────// ──── Quote Computation ────

/// Computed quotes for one side (YES or NO) of a market.
#[derive(Debug, Clone)]
pub struct TokenQuote {
    pub bid_price: Decimal,
    pub ask_price: Decimal,
    pub size: Decimal,
}

/// Compute bid/ask quotes for a single token side within the rewards spread band.
///
/// Returns `None` if the quotes would be invalid (crossed, out of bounds, or
/// outside the rewards spread).
pub fn compute_quotes(
    midpoint: Decimal,
    rewards_max_spread: Decimal,
    position: Decimal,
    config: &LiquidityRewardsConfig,
    rewards_min_size: Decimal,
    tick_size: Decimal,
) -> Option<TokenQuote> {
    let half_spread = rewards_max_spread * config.spread_fraction / Decimal::TWO;

    // Inventory skew: positive position → widen bid, tighten ask
    let max_pos = config.max_position_per_market;
    let inventory_ratio = if max_pos > Decimal::ZERO {
        (position / max_pos).min(Decimal::ONE).max(-Decimal::ONE)
    } else {
        Decimal::ZERO
    };
    let skew = inventory_ratio * config.inventory_skew_factor * half_spread;

    let bid_half = half_spread + skew;
    let ask_half = half_spread - skew;

    let bid_price = floor_to_tick(midpoint - bid_half, tick_size);
    let ask_price = ceil_to_tick(midpoint + ask_half, tick_size);

    // Clamp to valid price range [0.01, 0.99].
    // For LR, bidding at the floor price is valid — low risk, earns rewards.
    let min_price = Decimal::new(1, 2);
    let max_price = Decimal::new(99, 2);

    let bid_price = bid_price.max(min_price);
    let ask_price = ask_price.min(max_price);

    if bid_price >= ask_price {
        return None;
    }

    // Verify both quotes are within rewards_max_spread of midpoint
    if (midpoint - bid_price).abs() > rewards_max_spread
        || (ask_price - midpoint).abs() > rewards_max_spread
    {
        return None;
    }

    // Size: target max_position_per_market for maximum reward scoring,
    // floored to at least the greater of config min and market min.
    // The caller caps this to remaining position/exposure limits.
    let floor_size = config.min_order_size.max(rewards_min_size);
    let size = config.max_position_per_market.max(floor_size);

    Some(TokenQuote {
        bid_price,
        ask_price,
        size,
    })
}

/// Floor a price to the nearest tick size.
fn floor_to_tick(price: Decimal, tick: Decimal) -> Decimal {
    if tick <= Decimal::ZERO {
        return price.round_dp(2);
    }
    (price / tick).floor() * tick
}

/// Ceil a price to the nearest tick size.
fn ceil_to_tick(price: Decimal, tick: Decimal) -> Decimal {
    if tick <= Decimal::ZERO {
        return price.round_dp(2);
    }
    (price / tick).ceil() * tick
}

/// Compute bid/ask quotes by taking the Nth price level from the orderbook.
///
/// Unlike `compute_quotes()` which computes from midpoint ± spread, this reads
/// prices directly from the orderbook at the specified depth level.
///
/// Returns `None` if the orderbook lacks sufficient depth, quotes are crossed,
/// out of bounds, or outside the rewards spread band.
pub fn compute_depth_quotes(
    book: &pa_core::types::OrderBook,
    depth_level: usize,
    rewards_max_spread: Decimal,
    _position: Decimal,
    config: &LiquidityRewardsConfig,
    rewards_min_size: Decimal,
) -> Option<TokenQuote> {
    if depth_level < 1 {
        return None;
    }

    // Need at least depth_level entries on each side
    if book.bids.len() < depth_level || book.asks.len() < depth_level {
        return None;
    }

    let bid_price = book.bids[depth_level - 1].price;
    let ask_price = book.asks[depth_level - 1].price;

    // Quotes must not cross
    if bid_price >= ask_price {
        return None;
    }

    // Valid price range [0.01, 0.99]
    let min_price = Decimal::new(1, 2);
    let max_price = Decimal::new(99, 2);
    if bid_price < min_price || bid_price > max_price {
        return None;
    }
    if ask_price < min_price || ask_price > max_price {
        return None;
    }

    // Verify within rewards_max_spread of midpoint (using best bid/ask for midpoint)
    let best_bid = book.bids[0].price;
    let best_ask = book.asks[0].price;
    let midpoint = (best_bid + best_ask) / Decimal::TWO;

    if (midpoint - bid_price).abs() > rewards_max_spread
        || (ask_price - midpoint).abs() > rewards_max_spread
    {
        return None;
    }

    // Size: same as compute_quotes — target max_position_per_market
    let floor_size = config.min_order_size.max(rewards_min_size);
    let size = config.max_position_per_market.max(floor_size);

    Some(TokenQuote {
        bid_price,
        ask_price,
        size,
    })
}

/// Check if an outstanding order should be cancelled because it has reached
/// or passed the cancel depth level (i.e., it's now too close to best price).
///
/// For a buy order: counts how many bid levels have a strictly higher price.
/// If that count + 1 <= cancel_depth_level, the order is at or above the cancel
/// threshold and should be re-quoted further back.
///
/// For a sell order: counts how many ask levels have a strictly lower price.
/// Same logic applies.
pub fn should_cancel_depth_order(
    book: &pa_core::types::OrderBook,
    order_price: Decimal,
    is_buy: bool,
    cancel_depth_level: usize,
) -> bool {
    if cancel_depth_level == 0 {
        return false;
    }

    if is_buy {
        // Position = number of bids with price strictly above ours + 1
        let above = book.bids.iter().take_while(|l| l.price > order_price).count();
        let position = above + 1;
        position <= cancel_depth_level
    } else {
        // Position = number of asks with price strictly below ours + 1
        let below = book.asks.iter().take_while(|l| l.price < order_price).count();
        let position = below + 1;
        position <= cancel_depth_level
    }
}

// ──── Balance-Aware Sizing ────

/// Compute the affordable order size given current balance and quoting context.
///
/// `price` is the quote price for this order (bid price for buys, 1-bid for sells).
/// `sides_quoted` is the number of sides being quoted across all markets (for splitting budget).
/// Returns the affordable size floored to 2dp, or zero if below `min_order_size`.
pub fn balance_aware_size(
    price: Decimal,
    balance: Decimal,
    sides_quoted: u32,
    max_position_per_market: Decimal,
    remaining_exposure: Decimal,
    min_order_size: Decimal,
) -> Decimal {
    if price <= Decimal::ZERO || sides_quoted == 0 || balance <= Decimal::ZERO {
        return Decimal::ZERO;
    }

    let unit_cost = price;
    let per_side_budget = balance / Decimal::from(sides_quoted);
    // Truncate to 2 decimal places (floor)
    let raw = per_side_budget / unit_cost;
    let affordable = (raw * Decimal::from(100)).floor() / Decimal::from(100);

    let size = affordable
        .min(max_position_per_market)
        .min(remaining_exposure);

    if size < min_order_size {
        Decimal::ZERO
    } else {
        size
    }
}

// ──── Cooldown ────

/// Check if a failed order is still on cooldown.
///
/// Returns `true` if `(now - last_failure_time) < cooldown_duration`, meaning
/// the order should not be retried yet.
pub fn is_order_on_cooldown(
    last_failure: std::time::Instant,
    now: std::time::Instant,
    cooldown: std::time::Duration,
) -> bool {
    now.duration_since(last_failure) < cooldown
}

// ──── Duplicate Order Detection ────

/// A desired order computed by the quoting engine.
#[derive(Debug, Clone)]
pub struct DesiredOrder {
    pub token_id: alloy::primitives::U256,
    pub is_buy: bool,
    pub price: Decimal,
    pub size: Decimal,
}

/// Result of reconciling desired orders against existing outstanding orders.
#[derive(Debug, Default)]
pub struct ReconcileResult {
    /// Order IDs that should be cancelled (no longer desired or price changed).
    pub to_cancel: Vec<String>,
    /// New orders that need to be placed.
    pub to_place: Vec<DesiredOrder>,
    /// Count of existing orders that are still valid (no action needed).
    pub kept: usize,
}

/// Compare desired orders against existing outstanding orders.
///
/// An existing order is "kept" if there is a desired order with the same
/// `(token_id, is_buy)` and the price hasn't drifted more than `tolerance_bps`
/// from the desired price.
///
/// Orders that don't match are cancelled; desired orders without a match are placed.
pub fn reconcile_desired_vs_existing(
    desired: &[DesiredOrder],
    existing: &[(String, alloy::primitives::U256, bool, Decimal)], // (order_id, token_id, is_buy, price)
    tolerance_bps: u32,
) -> ReconcileResult {
    let mut result = ReconcileResult::default();
    let mut matched_existing: std::collections::HashSet<usize> = std::collections::HashSet::new();
    let mut matched_desired: std::collections::HashSet<usize> = std::collections::HashSet::new();

    for (di, d) in desired.iter().enumerate() {
        // Find best matching existing order: same token_id + side, within tolerance
        let best = existing.iter().enumerate()
            .filter(|(ei, _)| !matched_existing.contains(ei))
            .find(|(_, (_, tid, is_buy, price))| {
                *tid == d.token_id && *is_buy == d.is_buy && {
                    if d.price <= Decimal::ZERO { return false; }
                    let drift = ((d.price - *price).abs() / d.price * Decimal::from(10_000))
                        .to_u32()
                        .unwrap_or(u32::MAX);
                    drift <= tolerance_bps
                }
            });

        if let Some((ei, _)) = best {
            matched_existing.insert(ei);
            matched_desired.insert(di);
            result.kept += 1;
        }
    }

    // Cancel unmatched existing orders
    for (ei, (oid, _, _, _)) in existing.iter().enumerate() {
        if !matched_existing.contains(&ei) {
            result.to_cancel.push(oid.clone());
        }
    }

    // Place unmatched desired orders
    for (di, d) in desired.iter().enumerate() {
        if !matched_desired.contains(&di) {
            result.to_place.push(d.clone());
        }
    }

    result
}

// ──── Market Selection Hybrid ────

/// Select markets based on market_mode: auto, manual, or hybrid.
///
/// - `auto`: Standard density-ranked selection (backward compatible).
/// - `manual`: Only markets listed in `manual_markets`.
/// - `hybrid`: Auto-discovered markets + always include manual markets.
pub fn select_reward_markets_hybrid(
    markets: &[MarketInfo],
    clob_rewards: &[ClobRewardData],
    config: &LiquidityRewardsConfig,
) -> Vec<RewardMarketCandidate> {
    let mode = config.market_mode.as_str();

    match mode {
        "manual" => {
            // Only include manually specified markets
            let manual_cids: std::collections::HashSet<alloy::primitives::B256> = config
                .manual_markets
                .iter()
                .filter_map(|o| {
                    o.condition_id
                        .parse::<alloy::primitives::B256>()
                        .ok()
                })
                .collect();

            let reward_map: std::collections::HashMap<alloy::primitives::B256, &ClobRewardData> =
                clob_rewards.iter().map(|r| (r.condition_id, r)).collect();

            markets
                .iter()
                .filter(|m| {
                    m.active
                        && manual_cids.contains(&m.condition_id)
                        && (m.tokens.len() == 2 || config.allow_neg_risk)
                        && (!m.neg_risk || config.allow_neg_risk)
                })
                .filter_map(|m| {
                    let reward_data = reward_map.get(&m.condition_id);
                    let daily_rate = reward_data.map_or(Decimal::ZERO, |r| r.total_daily_rate);
                    let density = daily_rate / (m.liquidity + Decimal::ONE);
                    let (max_spread, min_size) = match reward_data {
                        Some(r) => (r.rewards_max_spread, r.rewards_min_size),
                        None => (Decimal::ZERO, Decimal::ZERO),
                    };
                    Some(RewardMarketCandidate {
                        market: m.clone(),
                        density,
                        clob_rewards_max_spread: max_spread,
                        clob_rewards_min_size: min_size,
                    })
                })
                .collect()
        }
        "hybrid" => {
            // Auto-discovered + always include manual markets
            let mut auto = select_reward_markets_with_clob_data(markets, clob_rewards, config);

            let manual_cids: std::collections::HashSet<alloy::primitives::B256> = config
                .manual_markets
                .iter()
                .filter_map(|o| {
                    o.condition_id
                        .parse::<alloy::primitives::B256>()
                        .ok()
                })
                .collect();

            let auto_cids: std::collections::HashSet<alloy::primitives::B256> =
                auto.iter().map(|c| c.market.condition_id).collect();

            let reward_map: std::collections::HashMap<alloy::primitives::B256, &ClobRewardData> =
                clob_rewards.iter().map(|r| (r.condition_id, r)).collect();

            // Add manual markets not already in auto list
            for m in markets {
                if manual_cids.contains(&m.condition_id)
                    && !auto_cids.contains(&m.condition_id)
                    && m.active
                    && (m.tokens.len() == 2 || config.allow_neg_risk)
                    && (!m.neg_risk || config.allow_neg_risk)
                {
                    let reward_data = reward_map.get(&m.condition_id);
                    let daily_rate = reward_data.map_or(Decimal::ZERO, |r| r.total_daily_rate);
                    let density = daily_rate / (m.liquidity + Decimal::ONE);
                    let (max_spread, min_size) = match reward_data {
                        Some(r) => (r.rewards_max_spread, r.rewards_min_size),
                        None => (Decimal::ZERO, Decimal::ZERO),
                    };
                    auto.push(RewardMarketCandidate {
                        market: m.clone(),
                        density,
                        clob_rewards_max_spread: max_spread,
                        clob_rewards_min_size: min_size,
                    });
                }
            }

            auto
        }
        _ => {
            // "auto" or any other value — backward compatible
            select_reward_markets_with_clob_data(markets, clob_rewards, config)
        }
    }
}

/// Get the effective config value for a specific market, merging global and per-market overrides.
pub fn effective_market_config(
    global: &LiquidityRewardsConfig,
    condition_id: &alloy::primitives::B256,
) -> EffectiveMarketConfig {
    let cid_hex = format!("{:#x}", condition_id);

    let override_entry = global.manual_markets.iter().find(|o| o.condition_id == cid_hex);

    EffectiveMarketConfig {
        max_position_per_market: override_entry
            .and_then(|o| o.max_position_per_market)
            .unwrap_or(global.max_position_per_market),
        spread_fraction: override_entry
            .and_then(|o| o.spread_fraction)
            .unwrap_or(global.spread_fraction),
        quote_yes: override_entry
            .and_then(|o| o.quote_yes)
            .unwrap_or(global.quote_yes),
        quote_no: override_entry
            .and_then(|o| o.quote_no)
            .unwrap_or(global.quote_no),
        order_depth_level: override_entry
            .and_then(|o| o.order_depth_level)
            .unwrap_or(global.order_depth_level),
    }
}

/// Resolved per-market configuration after merging global + override.
#[derive(Debug, Clone)]
pub struct EffectiveMarketConfig {
    pub max_position_per_market: Decimal,
    pub spread_fraction: Decimal,
    pub quote_yes: bool,
    pub quote_no: bool,
    pub order_depth_level: usize,
}

// ──── Tests ────

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::{B256, U256};
    use pa_core::types::{MarketInfo, Outcome, TokenInfo};
    use rust_decimal_macros::dec;

    fn make_reward_market(
        condition_id: u64,
        daily_rate: Option<Decimal>,
        liquidity: Decimal,
        min_size: Option<Decimal>,
        max_spread: Option<Decimal>,
    ) -> MarketInfo {
        MarketInfo {
            condition_id: B256::from(U256::from(condition_id)),
            question_id: B256::ZERO,
            question: format!("Market {}", condition_id),
            neg_risk: false,
            neg_risk_market_id: None,
            tokens: vec![
                TokenInfo {
                    token_id: U256::from(condition_id * 10 + 1),
                    outcome: Outcome::Yes,
                    complement_id: U256::from(condition_id * 10 + 2),
                },
                TokenInfo {
                    token_id: U256::from(condition_id * 10 + 2),
                    outcome: Outcome::No,
                    complement_id: U256::from(condition_id * 10 + 1),
                },
            ],
            tick_size: dec!(0.01),
            fee_rate_bps: 200,
            active: true,
            liquidity,
            event_title: None,
            end_date: None,
            category: None,
            outcome_prices: None,
            gamma_best_bid: None,
            gamma_best_ask: None,
            rewards_min_size: min_size,
            rewards_max_spread: max_spread,
            rewards_daily_rate: daily_rate,
            holding_rewards_enabled: false,
            fees_enabled: false,
        }
    }

    fn default_config() -> LiquidityRewardsConfig {
        LiquidityRewardsConfig::default()
    }

    #[test]
    fn test_select_markets_filters_no_rewards() {
        let config = default_config();
        let markets = vec![
            // No reward fields
            make_reward_market(1, None, dec!(1000), None, None),
            // Has rate but no min_size / max_spread
            make_reward_market(2, Some(dec!(5)), dec!(1000), None, None),
        ];
        let result = select_reward_markets(&markets, &config);
        assert!(result.is_empty());
    }

    #[test]
    fn test_select_markets_filters_below_min_rate() {
        let mut config = default_config();
        config.min_daily_rate = dec!(2);
        let markets = vec![
            make_reward_market(1, Some(dec!(1)), dec!(100), Some(dec!(5)), Some(dec!(0.04))),
        ];
        let result = select_reward_markets(&markets, &config);
        assert!(result.is_empty(), "Daily rate 1 < min_daily_rate 2 should be filtered");
    }

    #[test]
    fn test_select_markets_ranks_by_density() {
        let config = default_config();
        let markets = vec![
            // Market A: rate=10, liquidity=1000 → density=10/1001 ≈ 0.00999
            make_reward_market(1, Some(dec!(10)), dec!(1000), Some(dec!(5)), Some(dec!(0.04))),
            // Market B: rate=5, liquidity=100 → density=5/101 ≈ 0.0495 (higher)
            make_reward_market(2, Some(dec!(5)), dec!(100), Some(dec!(5)), Some(dec!(0.04))),
            // Market C: rate=20, liquidity=5000 → density=20/5001 ≈ 0.00399
            make_reward_market(3, Some(dec!(20)), dec!(5000), Some(dec!(5)), Some(dec!(0.04))),
        ];
        let result = select_reward_markets(&markets, &config);
        assert_eq!(result.len(), 3);
        // B (highest density) should be first
        assert_eq!(result[0].market.condition_id, B256::from(U256::from(2u64)));
        // A second
        assert_eq!(result[1].market.condition_id, B256::from(U256::from(1u64)));
        // C last
        assert_eq!(result[2].market.condition_id, B256::from(U256::from(3u64)));
    }

    #[test]
    fn test_select_markets_respects_max() {
        let mut config = default_config();
        config.max_markets = 2;
        let markets = vec![
            make_reward_market(1, Some(dec!(10)), dec!(100), Some(dec!(5)), Some(dec!(0.04))),
            make_reward_market(2, Some(dec!(10)), dec!(200), Some(dec!(5)), Some(dec!(0.04))),
            make_reward_market(3, Some(dec!(10)), dec!(300), Some(dec!(5)), Some(dec!(0.04))),
        ];
        let result = select_reward_markets(&markets, &config);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_compute_quotes_within_spread() {
        let config = default_config();
        let mid = dec!(0.50);
        let max_spread = dec!(0.04);
        let result = compute_quotes(mid, max_spread, Decimal::ZERO, &config, dec!(5), dec!(0.01));
        assert!(result.is_some());
        let q = result.unwrap();
        // half_spread = 0.04 * 0.80 / 2 = 0.016
        // bid = floor(0.50 - 0.016, 0.01) = floor(0.484, 0.01) = 0.48
        // ask = ceil(0.50 + 0.016, 0.01) = ceil(0.516, 0.01) = 0.52
        assert_eq!(q.bid_price, dec!(0.48));
        assert_eq!(q.ask_price, dec!(0.52));
        // Both within max_spread of midpoint
        assert!((mid - q.bid_price).abs() <= max_spread);
        assert!((q.ask_price - mid).abs() <= max_spread);
    }

    #[test]
    fn test_compute_quotes_inventory_skew() {
        let mut config = default_config();
        config.max_position_per_market = dec!(100);
        config.inventory_skew_factor = dec!(0.50);

        let mid = dec!(0.50);
        let max_spread = dec!(0.10); // wider spread so skew effect survives tick rounding

        // Zero position → symmetric
        let q0 = compute_quotes(mid, max_spread, Decimal::ZERO, &config, dec!(5), dec!(0.01)).unwrap();

        // Full position → maximum skew
        let q_pos = compute_quotes(mid, max_spread, dec!(100), &config, dec!(5), dec!(0.01)).unwrap();

        // half_spread = 0.10 * 0.80 / 2 = 0.04
        // skew = 1.0 * 0.50 * 0.04 = 0.02
        // bid_half = 0.04 + 0.02 = 0.06 → bid = floor(0.44) = 0.44
        // ask_half = 0.04 - 0.02 = 0.02 → ask = ceil(0.52) = 0.52
        assert!(q_pos.bid_price < q0.bid_price, "With positive inventory, bid should be lower (wider)");
        assert!(q_pos.ask_price < q0.ask_price, "With positive inventory, ask should be lower (closer to mid)");
    }

    #[test]
    fn test_compute_quotes_boundary_rejection() {
        let config = default_config();
        // Very tight spread that would result in quotes outside rewards band
        let mid = dec!(0.50);
        let max_spread = dec!(0.001); // Too tight for 0.01 tick
        let result = compute_quotes(mid, max_spread, Decimal::ZERO, &config, dec!(5), dec!(0.01));
        // half_spread = 0.001 * 0.80 / 2 = 0.0004
        // bid = floor(0.4996, 0.01) = 0.49, ask = ceil(0.5004, 0.01) = 0.51
        // Check if 0.49 is within 0.001 of 0.50 → |0.50 - 0.49| = 0.01 > 0.001 → rejected
        assert!(result.is_none(), "Quotes outside rewards_max_spread should be rejected");
    }

    #[test]
    fn test_order_size_targets_max_position() {
        let mut config = default_config();
        config.min_order_size = dec!(5);
        config.max_position_per_market = dec!(100);

        let mid = dec!(0.50);
        let max_spread = dec!(0.10);

        // Size should target max_position (100), not just the floor
        let q = compute_quotes(mid, max_spread, Decimal::ZERO, &config, dec!(10), dec!(0.01)).unwrap();
        assert_eq!(q.size, dec!(100), "Size should target max_position_per_market for maximum rewards");

        // Even with small market min_size, still targets max_position
        let q = compute_quotes(mid, max_spread, Decimal::ZERO, &config, dec!(3), dec!(0.01)).unwrap();
        assert_eq!(q.size, dec!(100), "Size should target max_position_per_market regardless of floor");
    }

    #[test]
    fn test_total_exposure_cap() {
        // compute_quotes returns max_position_per_market as target size.
        // The actual exposure capping occurs in the run loop (main.rs).
        let config = default_config();
        let mid = dec!(0.50);
        let max_spread = dec!(0.10);
        let q = compute_quotes(mid, max_spread, Decimal::ZERO, &config, dec!(5), dec!(0.01)).unwrap();
        assert_eq!(q.size, config.max_position_per_market);
        // In the run loop, remaining_exposure caps bid_size:
        // e.g. if remaining_exposure=3 USDC, bid_size = min(100, 3) = 3 < min_order_size → skipped
    }

    #[test]
    fn test_compute_quotes_clamps_to_floor() {
        // When midpoint is low and spread is wide, bid gets clamped to 0.01 floor.
        // This is valid for LR — low risk, earns rewards.
        let config = default_config();
        // midpoint=0.02, spread=0.10 → half_spread = 0.10*0.80/2 = 0.04
        // bid = 0.02 - 0.04 = -0.02 → clamped to 0.01
        // ask = 0.02 + 0.04 = 0.06
        let result = compute_quotes(dec!(0.02), dec!(0.10), Decimal::ZERO, &config, dec!(5), dec!(0.01));
        assert!(result.is_some(), "Clamped floor bid should be accepted for LR");
        let q = result.unwrap();
        assert_eq!(q.bid_price, dec!(0.01));
        assert_eq!(q.ask_price, dec!(0.06));

        // midpoint=0.05, spread=0.10 → half_spread=0.04
        // bid = 0.05 - 0.04 = 0.01 → exactly 0.01, valid
        let result = compute_quotes(dec!(0.05), dec!(0.10), Decimal::ZERO, &config, dec!(5), dec!(0.01));
        assert!(result.is_some(), "Bid exactly at 0.01 should be accepted");
    }

    fn make_orderbook(bids: Vec<(Decimal, Decimal)>, asks: Vec<(Decimal, Decimal)>) -> pa_core::types::OrderBook {
        pa_core::types::OrderBook {
            token_id: U256::from(1u64),
            bids: bids.into_iter().map(|(price, size)| pa_core::types::PriceLevel { price, size }).collect(),
            asks: asks.into_iter().map(|(price, size)| pa_core::types::PriceLevel { price, size }).collect(),
            timestamp: chrono::Utc::now(),
        }
    }

    #[test]
    fn test_compute_depth_quotes_basic() {
        let config = default_config();
        // 5 levels on each side, take 3rd level
        let book = make_orderbook(
            vec![
                (dec!(0.50), dec!(100)), // best bid
                (dec!(0.49), dec!(100)),
                (dec!(0.48), dec!(100)), // 3rd level
                (dec!(0.47), dec!(100)),
                (dec!(0.46), dec!(100)),
            ],
            vec![
                (dec!(0.51), dec!(100)), // best ask
                (dec!(0.52), dec!(100)),
                (dec!(0.53), dec!(100)), // 3rd level
                (dec!(0.54), dec!(100)),
                (dec!(0.55), dec!(100)),
            ],
        );
        let result = compute_depth_quotes(&book, 3, dec!(0.10), Decimal::ZERO, &config, dec!(5));
        assert!(result.is_some());
        let q = result.unwrap();
        assert_eq!(q.bid_price, dec!(0.48));
        assert_eq!(q.ask_price, dec!(0.53));
    }

    #[test]
    fn test_compute_depth_quotes_insufficient_depth() {
        let config = default_config();
        // Only 2 levels, request 3rd
        let book = make_orderbook(
            vec![(dec!(0.50), dec!(100)), (dec!(0.49), dec!(100))],
            vec![(dec!(0.51), dec!(100)), (dec!(0.52), dec!(100))],
        );
        let result = compute_depth_quotes(&book, 3, dec!(0.10), Decimal::ZERO, &config, dec!(5));
        assert!(result.is_none(), "Should return None when depth is insufficient");
    }

    #[test]
    fn test_compute_depth_quotes_outside_reward_spread() {
        let config = default_config();
        // 3rd level prices are far from midpoint
        let book = make_orderbook(
            vec![
                (dec!(0.50), dec!(100)),
                (dec!(0.49), dec!(100)),
                (dec!(0.40), dec!(100)), // 3rd bid far from mid ~0.505
            ],
            vec![
                (dec!(0.51), dec!(100)),
                (dec!(0.52), dec!(100)),
                (dec!(0.60), dec!(100)), // 3rd ask far from mid ~0.505
            ],
        );
        // rewards_max_spread = 0.04, midpoint = (0.50+0.51)/2 = 0.505
        // |0.505 - 0.40| = 0.105 > 0.04 → rejected
        let result = compute_depth_quotes(&book, 3, dec!(0.04), Decimal::ZERO, &config, dec!(5));
        assert!(result.is_none(), "Should reject quotes outside rewards_max_spread");
    }

    #[test]
    fn test_should_cancel_buy_at_depth() {
        // Buy order at 0.48 (was at level 3). After book changes, it's now at level 2.
        let book = make_orderbook(
            vec![
                (dec!(0.49), dec!(100)), // level 1 (was 0.50, now gone)
                (dec!(0.48), dec!(100)), // level 2 — our order moved up!
                (dec!(0.47), dec!(100)),
            ],
            vec![(dec!(0.51), dec!(100))],
        );
        // cancel_depth_level = 2: cancel when position <= 2
        assert!(should_cancel_depth_order(&book, dec!(0.48), true, 2),
            "Buy at level 2 should trigger cancel when cancel_depth=2");

        // Same order but book still has 0.50 above → position = 3
        let book2 = make_orderbook(
            vec![
                (dec!(0.50), dec!(100)),
                (dec!(0.49), dec!(100)),
                (dec!(0.48), dec!(100)), // level 3 — safe
                (dec!(0.47), dec!(100)),
            ],
            vec![(dec!(0.51), dec!(100))],
        );
        assert!(!should_cancel_depth_order(&book2, dec!(0.48), true, 2),
            "Buy at level 3 should NOT trigger cancel when cancel_depth=2");
    }

    #[test]
    fn test_should_cancel_sell_at_depth() {
        // Sell order at 0.53. Book has only 0.52 below → position = 2.
        let book = make_orderbook(
            vec![(dec!(0.50), dec!(100))],
            vec![
                (dec!(0.52), dec!(100)), // level 1
                (dec!(0.53), dec!(100)), // level 2 — our order
                (dec!(0.54), dec!(100)),
            ],
        );
        assert!(should_cancel_depth_order(&book, dec!(0.53), false, 2),
            "Sell at level 2 should trigger cancel when cancel_depth=2");

        // Add one more level below → position = 3
        let book2 = make_orderbook(
            vec![(dec!(0.50), dec!(100))],
            vec![
                (dec!(0.51), dec!(100)), // level 1
                (dec!(0.52), dec!(100)), // level 2
                (dec!(0.53), dec!(100)), // level 3 — safe
                (dec!(0.54), dec!(100)),
            ],
        );
        assert!(!should_cancel_depth_order(&book2, dec!(0.53), false, 2),
            "Sell at level 3 should NOT trigger cancel when cancel_depth=2");
    }

    // ──── Balance-Aware Sizing tests ────

    #[test]
    fn test_balance_aware_size_buy() {
        // balance=100 USDC, 4 sides being quoted, bid price=0.40
        // per_side_budget = 100/4 = 25
        // affordable = floor_2dp(25/0.40) = floor_2dp(62.50) = 62.50
        let size = balance_aware_size(
            dec!(0.40), dec!(100), 4, dec!(100), dec!(500), dec!(5),
        );
        assert_eq!(size, dec!(62.50));
    }

    #[test]
    fn test_balance_aware_size_sell() {
        // Sell: unit_cost = 1 - bid_price for the complement. Here price = 0.60
        // balance=50, 2 sides, max_pos=200, remaining_exp=1000
        // per_side_budget = 50/2 = 25
        // affordable = floor(25/0.60) = floor(41.666..) = 41.66
        let size = balance_aware_size(
            dec!(0.60), dec!(50), 2, dec!(200), dec!(1000), dec!(5),
        );
        assert_eq!(size, dec!(41.66));
    }

    #[test]
    fn test_balance_aware_size_respects_limits() {
        // affordable=100 but max_position=30 → 30
        let size = balance_aware_size(
            dec!(0.50), dec!(100), 2, dec!(30), dec!(500), dec!(5),
        );
        assert_eq!(size, dec!(30));

        // affordable=62 but remaining_exposure=10 → 10
        let size = balance_aware_size(
            dec!(0.40), dec!(100), 4, dec!(100), dec!(10), dec!(5),
        );
        assert_eq!(size, dec!(10));

        // Result below min_order_size → 0
        let size = balance_aware_size(
            dec!(0.40), dec!(100), 4, dec!(100), dec!(3), dec!(5),
        );
        assert_eq!(size, Decimal::ZERO);
    }

    // ──── Cooldown tests ────

    #[test]
    fn test_cooldown_blocks() {
        let now = std::time::Instant::now();
        let failure = now; // Just failed
        let cooldown = std::time::Duration::from_secs(60);
        assert!(is_order_on_cooldown(failure, now, cooldown));
    }

    #[test]
    fn test_cooldown_allows_after_expiry() {
        let failure = std::time::Instant::now();
        // Simulate time passing by using duration arithmetic
        let cooldown = std::time::Duration::from_secs(0); // Zero cooldown = always allowed
        assert!(!is_order_on_cooldown(failure, failure, cooldown));
    }

    // ──── Reconcile tests ────

    #[test]
    fn test_reconcile_no_change() {
        let desired = vec![DesiredOrder {
            token_id: U256::from(1u64),
            is_buy: true,
            price: dec!(0.50),
            size: dec!(100),
        }];
        let existing = vec![
            ("order1".to_string(), U256::from(1u64), true, dec!(0.50)),
        ];
        let result = reconcile_desired_vs_existing(&desired, &existing, 50);
        assert_eq!(result.kept, 1);
        assert!(result.to_cancel.is_empty());
        assert!(result.to_place.is_empty());
    }

    #[test]
    fn test_reconcile_price_drift() {
        let desired = vec![DesiredOrder {
            token_id: U256::from(1u64),
            is_buy: true,
            price: dec!(0.50),
            size: dec!(100),
        }];
        // Existing order drifted to 0.48 — 400 bps drift > 50 bps tolerance
        let existing = vec![
            ("order1".to_string(), U256::from(1u64), true, dec!(0.48)),
        ];
        let result = reconcile_desired_vs_existing(&desired, &existing, 50);
        assert_eq!(result.kept, 0);
        assert_eq!(result.to_cancel.len(), 1);
        assert_eq!(result.to_place.len(), 1);
    }

    #[test]
    fn test_reconcile_new_side() {
        // No existing orders, want to place a new one
        let desired = vec![DesiredOrder {
            token_id: U256::from(1u64),
            is_buy: false,
            price: dec!(0.55),
            size: dec!(50),
        }];
        let existing: Vec<(String, alloy::primitives::U256, bool, Decimal)> = vec![];
        let result = reconcile_desired_vs_existing(&desired, &existing, 50);
        assert_eq!(result.kept, 0);
        assert!(result.to_cancel.is_empty());
        assert_eq!(result.to_place.len(), 1);
    }

    // ──── Hybrid market selection tests ────

    #[test]
    fn test_hybrid_mode_auto_backward_compatible() {
        let config = default_config(); // market_mode = "auto"
        let clob_rewards = vec![ClobRewardData {
            condition_id: B256::from(U256::from(1u64)),
            rewards_max_spread: dec!(0.04),
            rewards_min_size: dec!(5),
            total_daily_rate: dec!(10),
        }];
        let markets = vec![
            make_reward_market(1, Some(dec!(10)), dec!(100), Some(dec!(5)), Some(dec!(0.04))),
        ];
        let result = select_reward_markets_hybrid(&markets, &clob_rewards, &config);
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn test_hybrid_mode_manual_only() {
        let mut config = default_config();
        config.market_mode = "manual".to_string();
        config.manual_markets = vec![pa_core::config::LrMarketOverride {
            condition_id: format!("{:#x}", B256::from(U256::from(2u64))),
            max_position_per_market: None,
            spread_fraction: None,
            quote_yes: None,
            quote_no: None,
            order_depth_level: None,
        }];
        let clob_rewards = vec![
            ClobRewardData {
                condition_id: B256::from(U256::from(1u64)),
                rewards_max_spread: dec!(0.04),
                rewards_min_size: dec!(5),
                total_daily_rate: dec!(10),
            },
            ClobRewardData {
                condition_id: B256::from(U256::from(2u64)),
                rewards_max_spread: dec!(0.04),
                rewards_min_size: dec!(5),
                total_daily_rate: dec!(5),
            },
        ];
        let markets = vec![
            make_reward_market(1, Some(dec!(10)), dec!(100), Some(dec!(5)), Some(dec!(0.04))),
            make_reward_market(2, Some(dec!(5)), dec!(100), Some(dec!(5)), Some(dec!(0.04))),
        ];
        let result = select_reward_markets_hybrid(&markets, &clob_rewards, &config);
        // Only market 2 should be selected (manual)
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].market.condition_id, B256::from(U256::from(2u64)));
    }

    #[test]
    fn test_hybrid_mode_merges() {
        let mut config = default_config();
        config.market_mode = "hybrid".to_string();
        config.manual_markets = vec![pa_core::config::LrMarketOverride {
            condition_id: format!("{:#x}", B256::from(U256::from(3u64))),
            max_position_per_market: Some(dec!(200)),
            spread_fraction: None,
            quote_yes: None,
            quote_no: None,
            order_depth_level: None,
        }];
        let clob_rewards = vec![
            ClobRewardData {
                condition_id: B256::from(U256::from(1u64)),
                rewards_max_spread: dec!(0.04),
                rewards_min_size: dec!(5),
                total_daily_rate: dec!(10),
            },
        ];
        let markets = vec![
            make_reward_market(1, Some(dec!(10)), dec!(100), Some(dec!(5)), Some(dec!(0.04))),
            make_reward_market(3, Some(dec!(3)), dec!(50), Some(dec!(5)), Some(dec!(0.04))),
        ];
        let result = select_reward_markets_hybrid(&markets, &clob_rewards, &config);
        // Market 1 from auto + market 3 from manual
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_effective_market_config_override() {
        let mut config = default_config();
        let cid = B256::from(U256::from(42u64));
        config.manual_markets = vec![pa_core::config::LrMarketOverride {
            condition_id: format!("{:#x}", cid),
            max_position_per_market: Some(dec!(500)),
            spread_fraction: Some(dec!(0.90)),
            quote_yes: Some(true),
            quote_no: Some(false),
            order_depth_level: Some(5),
        }];
        let eff = effective_market_config(&config, &cid);
        assert_eq!(eff.max_position_per_market, dec!(500));
        assert_eq!(eff.spread_fraction, dec!(0.90));
        assert!(eff.quote_yes);
        assert!(!eff.quote_no);
        assert_eq!(eff.order_depth_level, 5);
    }

    // ──── NegRisk filter tests ────

    #[test]
    fn test_neg_risk_filtered_by_default() {
        let config = default_config(); // allow_neg_risk = false
        let mut m = make_reward_market(1, Some(dec!(10)), dec!(100), Some(dec!(5)), Some(dec!(0.04)));
        m.neg_risk = true;
        let markets = vec![m];
        let result = select_reward_markets(&markets, &config);
        assert!(result.is_empty(), "NegRisk should be filtered by default");
    }

    #[test]
    fn test_neg_risk_allowed_with_flag() {
        let mut config = default_config();
        config.allow_neg_risk = true;
        config.market_mode = "manual".to_string();

        let mut m = make_reward_market(1, Some(dec!(10)), dec!(100), Some(dec!(5)), Some(dec!(0.04)));
        m.neg_risk = true;
        // NegRisk markets can have >2 tokens — add a 3rd
        m.tokens.push(TokenInfo {
            token_id: U256::from(13u64),
            outcome: Outcome::Yes,
            complement_id: U256::ZERO,
        });
        config.manual_markets = vec![pa_core::config::LrMarketOverride {
            condition_id: format!("{:#x}", m.condition_id),
            max_position_per_market: None,
            spread_fraction: None,
            quote_yes: None,
            quote_no: None,
            order_depth_level: None,
        }];

        let clob_rewards = vec![ClobRewardData {
            condition_id: m.condition_id,
            rewards_max_spread: dec!(0.04),
            rewards_min_size: dec!(5),
            total_daily_rate: dec!(10),
        }];

        let markets = vec![m];
        let result = select_reward_markets_hybrid(&markets, &clob_rewards, &config);
        assert_eq!(result.len(), 1, "NegRisk should be allowed with allow_neg_risk=true");
    }
}
