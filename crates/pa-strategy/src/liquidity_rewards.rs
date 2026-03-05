use pa_core::config::LiquidityRewardsConfig;
use pa_core::types::MarketInfo;
use rust_decimal::Decimal;

// ──── Market Selection ────

/// A reward market candidate with its computed score.
#[derive(Debug, Clone)]
pub struct RewardMarketCandidate {
    pub market: MarketInfo,
    /// reward_density = daily_rate / (liquidity + 1)
    pub density: Decimal,
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

    // Validate: within rewards spread, not crossed, within [0.01, 0.99]
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
}
