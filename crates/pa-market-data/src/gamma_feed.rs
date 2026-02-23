use alloy::primitives::{B256, U256};
use futures::{StreamExt, pin_mut};
use pa_core::config::Settings;
use pa_core::types::{BinaryEventGroup, MarketInfo, NegRiskEvent, Outcome, TokenInfo};
use rust_decimal::Decimal;
use std::collections::HashMap;

/// Discovers markets from the Polymarket Gamma API and filters candidates.
pub struct GammaFeed {
    client: polymarket_client_sdk::gamma::Client,
    min_liquidity: Decimal,
    min_volume_24h: Decimal,
    max_markets: usize,
    enabled_strategies: Vec<String>,
}

impl GammaFeed {
    pub fn new(settings: &Settings) -> anyhow::Result<Self> {
        let client = polymarket_client_sdk::gamma::Client::new(&settings.gamma.host)?;
        Ok(Self {
            client,
            min_liquidity: settings.market_filter.min_liquidity,
            min_volume_24h: settings.market_filter.min_volume_24h,
            max_markets: settings.market_filter.max_markets,
            enabled_strategies: settings.strategy.enabled.clone(),
        })
    }

    /// Create with default Gamma API endpoint.
    pub fn with_defaults(settings: &Settings) -> Self {
        Self {
            client: polymarket_client_sdk::gamma::Client::default(),
            min_liquidity: settings.market_filter.min_liquidity,
            min_volume_24h: settings.market_filter.min_volume_24h,
            max_markets: settings.market_filter.max_markets,
            enabled_strategies: settings.strategy.enabled.clone(),
        }
    }

    /// Discover all active binary markets from Gamma API.
    ///
    /// Uses pagination via `stream_data()` to enumerate all active events,
    /// then extracts binary markets with CLOB token IDs.
    /// Retries up to 3 times with exponential backoff on failure.
    pub async fn discover_markets(&self) -> anyhow::Result<Vec<MarketInfo>> {
        tracing::info!("Discovering markets from Gamma API");

        let mut all_markets = Vec::new();
        let mut last_error = None;

        // Retry the entire stream up to 5 times with exponential backoff
        for attempt in 0..5u32 {
            if attempt > 0 {
                let delay = std::time::Duration::from_secs(2u64.pow(attempt).min(30)); // 2s, 4s, 8s, 16s
                tracing::warn!(
                    attempt = attempt + 1,
                    delay_secs = delay.as_secs(),
                    "Retrying Gamma API discovery"
                );
                tokio::time::sleep(delay).await;
            }

            all_markets.clear();
            let mut had_error = false;

            let stream = self.client.stream_data(
                |client, limit, offset| {
                    let request = polymarket_client_sdk::gamma::types::request::EventsRequest::builder()
                        .active(true)
                        .closed(false)
                        .limit(limit)
                        .offset(offset)
                        .build();
                    async move { client.events(&request).await }
                },
                100, // reduced from 500 to avoid HTTP/2 stream resets on large responses
            );

            pin_mut!(stream);

            loop {
                // Wrap each page fetch with a 60s timeout to avoid 4-minute hangs
                let event_result = match tokio::time::timeout(
                    std::time::Duration::from_secs(60),
                    stream.next(),
                ).await {
                    Ok(Some(result)) => result,
                    Ok(None) => break, // stream exhausted — all pages fetched
                    Err(_) => {
                        tracing::warn!(
                            attempt = attempt + 1,
                            "Gamma API page fetch timed out after 60s"
                        );
                        last_error = Some("Page fetch timed out".to_string());
                        had_error = true;
                        break;
                    }
                };

                let event = match event_result {
                    Ok(e) => e,
                    Err(e) => {
                        tracing::warn!(
                            attempt = attempt + 1,
                            error = %e,
                            error_debug = ?e,
                            "Failed to fetch event page"
                        );
                        last_error = Some(e.to_string());
                        had_error = true;
                        break; // Break inner loop to trigger retry
                    }
                };

                let event_neg_risk = event.neg_risk.unwrap_or(false);
                let event_neg_risk_market_id = event.neg_risk_market_id;
                let event_title = event.title.clone();

                let markets = match event.markets {
                    Some(m) => m,
                    None => continue,
                };

                for market in markets {
                    if let Some(info) = self.convert_market(&market, event_neg_risk, event_neg_risk_market_id, event_title.clone()) {
                        all_markets.push(info);
                    }
                }
            }

            if !had_error && !all_markets.is_empty() {
                break; // Full success — all pages fetched
            }

            // Partial success: got some markets before error — use what we have
            if had_error && !all_markets.is_empty() {
                tracing::warn!(
                    fetched = all_markets.len(),
                    "Gamma API partially succeeded, using fetched markets"
                );
                break;
            }

            if !had_error && all_markets.is_empty() {
                tracing::warn!(attempt = attempt + 1, "Gamma API returned 0 events");
                last_error = Some("Gamma API returned 0 events".to_string());
            }
        }

        if all_markets.is_empty() {
            if let Some(err) = &last_error {
                tracing::error!(error = %err, "All Gamma API retry attempts failed");
            }
        }

        tracing::info!(
            total_discovered = all_markets.len(),
            "Raw market discovery complete"
        );

        // Determine if any general (non-directional) strategies are enabled.
        // General strategies (yes_no, neg_risk, cross_market, convergence) need broad market data.
        // Directional-only strategies (weather, crypto) only need strategy-relevant markets.
        let general_strategies = ["yes_no", "neg_risk", "cross_market", "convergence"];
        let needs_general_markets = self.enabled_strategies.is_empty()
            || self.enabled_strategies.iter().any(|s| general_strategies.contains(&s.as_str()));

        // Partition into strategy-relevant vs general markets.
        let mut strategy_markets = Vec::new();
        let mut general_markets = Vec::new();

        for m in all_markets {
            if !m.active {
                continue;
            }
            if Self::is_relevant_for_strategies(&m.question, &self.enabled_strategies) {
                strategy_markets.push(m);
            } else if needs_general_markets {
                general_markets.push(m);
            }
            // If only directional strategies enabled, skip non-relevant markets entirely
        }

        // Sort general markets by liquidity descending, fill remaining slots
        general_markets.sort_by(|a, b| b.liquidity.cmp(&a.liquidity));
        let remaining_slots = self.max_markets.saturating_sub(strategy_markets.len());
        general_markets.truncate(remaining_slots);

        let mut filtered = strategy_markets;
        let strategy_count = filtered.len();
        filtered.extend(general_markets);

        tracing::info!(
            filtered_count = filtered.len(),
            strategy_relevant = strategy_count,
            general_markets = filtered.len() - strategy_count,
            needs_general = needs_general_markets,
            enabled = ?self.enabled_strategies,
            "Market discovery complete after filtering"
        );

        Ok(filtered)
    }

    /// Check if a market question is relevant to active strategies.
    /// These markets are always included regardless of liquidity ranking.
    pub fn is_strategy_relevant(question: &str) -> bool {
        // When called without strategy filter, check all strategies
        Self::is_relevant_for_strategies(question, &[])
    }

    /// Check if a market question is relevant to the given enabled strategies.
    /// Empty slice means check all strategies (backwards compatibility).
    pub fn is_relevant_for_strategies(question: &str, enabled: &[String]) -> bool {
        let lower = question.to_lowercase();

        let check_weather = enabled.is_empty() || enabled.iter().any(|s| s == "weather");
        let check_crypto = enabled.is_empty() || enabled.iter().any(|s| s == "crypto");

        // Weather: only strong unambiguous keywords
        if check_weather {
            let weather = lower.contains("temperature")
                || lower.contains("fahrenheit")
                || lower.contains("celsius")
                || lower.contains("rainfall")
                || lower.contains("snowfall")
                || lower.contains("wind speed")
                || lower.contains("inches of rain")
                || lower.contains("inches of snow");

            if weather {
                return true;
            }
        }

        // Crypto price markets: asset keyword + price indicator
        if check_crypto {
            let crypto_assets = [
                "bitcoin", "btc", "ethereum", "eth", "solana",
                "bnb", "xrp", "ripple", "dogecoin",
                "cardano", "avax", "polkadot", "polygon", "matic",
            ];
            let has_crypto_asset = crypto_assets.iter().any(|kw| {
                if let Some(pos) = lower.find(kw) {
                    let before_ok = pos == 0 || !lower.as_bytes()[pos - 1].is_ascii_alphabetic();
                    let after = pos + kw.len();
                    let after_ok =
                        after >= lower.len() || !lower.as_bytes()[after].is_ascii_alphabetic();
                    before_ok && after_ok
                } else {
                    false
                }
            });
            let gas_price = lower.contains("gas price") || lower.contains("gas fee");
            let has_price_indicator = lower.contains('$')
                || lower.contains("price")
                || lower.contains("reach")
                || lower.contains("hit")
                || lower.contains("exceed")
                || lower.contains("dip");

            if has_crypto_asset && has_price_indicator && !gas_price {
                return true;
            }
        }

        false
    }

    /// Convert a Gamma SDK `Market` into our internal `MarketInfo`.
    ///
    /// Returns `None` if the market is not a valid binary market
    /// (missing condition_id, question_id, or clob_token_ids).
    fn convert_market(
        &self,
        market: &polymarket_client_sdk::gamma::types::response::Market,
        event_neg_risk: bool,
        event_neg_risk_market_id: Option<B256>,
        event_title: Option<String>,
    ) -> Option<MarketInfo> {
        let condition_id = market.condition_id?;
        let question_id = market.question_id.unwrap_or(B256::ZERO);
        let question = market.question.clone().unwrap_or_default();
        let active = market.active.unwrap_or(false);
        let closed = market.closed.unwrap_or(false);

        if closed || !active {
            return None;
        }

        // Need exactly 2 CLOB token IDs for binary markets
        let clob_token_ids = market.clob_token_ids.as_ref()?;
        if clob_token_ids.len() != 2 {
            return None;
        }

        let yes_token_id = clob_token_ids[0];
        let no_token_id = clob_token_ids[1];

        // Extract tick size and fee rate
        let tick_size = market.order_price_min_tick_size.unwrap_or(Decimal::new(1, 2)); // default 0.01
        let fee_rate_bps = market.taker_base_fee.unwrap_or(0) as u32;

        // Determine neg_risk status
        let neg_risk = event_neg_risk || market.neg_risk.unwrap_or(false);
        let neg_risk_market_id = event_neg_risk_market_id.or(market.neg_risk_market_id);

        // Apply liquidity/volume filters (relaxed for strategy-relevant markets)
        let liquidity = market.liquidity.unwrap_or(Decimal::ZERO);
        let volume_24h = market.volume_24hr.unwrap_or(Decimal::ZERO);
        let strategy_relevant = Self::is_strategy_relevant(&question);

        if !strategy_relevant {
            if liquidity < self.min_liquidity {
                return None;
            }
            if volume_24h < self.min_volume_24h {
                return None;
            }
        }

        Some(MarketInfo {
            condition_id,
            question_id,
            question,
            neg_risk,
            neg_risk_market_id,
            tokens: vec![
                TokenInfo {
                    token_id: yes_token_id,
                    outcome: Outcome::Yes,
                    complement_id: no_token_id,
                },
                TokenInfo {
                    token_id: no_token_id,
                    outcome: Outcome::No,
                    complement_id: yes_token_id,
                },
            ],
            tick_size,
            fee_rate_bps,
            active,
            liquidity,
            event_title,
            end_date: market.end_date,
            category: market.category.clone(),
            outcome_prices: market.outcome_prices.clone(),
            gamma_best_bid: market.best_bid,
            gamma_best_ask: market.best_ask,
        })
    }

    /// Discover NegRisk multi-outcome events from Gamma API.
    ///
    /// Groups all markets sharing the same `neg_risk_market_id` into `NegRiskEvent`s.
    /// Only returns events with at least 2 outcome markets.
    pub fn group_neg_risk_events(markets: &[MarketInfo]) -> Vec<NegRiskEvent> {
        let mut groups: HashMap<B256, Vec<MarketInfo>> = HashMap::new();

        for market in markets {
            if !market.neg_risk {
                continue;
            }
            if let Some(nr_id) = market.neg_risk_market_id {
                groups.entry(nr_id).or_default().push(market.clone());
            }
        }

        let events: Vec<NegRiskEvent> = groups
            .into_iter()
            .filter(|(_, ms)| ms.len() >= 2)
            .map(|(nr_id, ms)| {
                let fee_rate_bps = ms.first().map(|m| m.fee_rate_bps).unwrap_or(0);
                let title = ms
                    .iter()
                    .find_map(|m| m.event_title.clone())
                    .unwrap_or_else(|| format!("NegRisk event {}", nr_id));
                NegRiskEvent {
                    neg_risk_market_id: nr_id,
                    title,
                    markets: ms,
                    fee_rate_bps,
                }
            })
            .collect();

        tracing::info!(
            neg_risk_events = events.len(),
            total_outcomes = events.iter().map(|e| e.markets.len()).sum::<usize>(),
            "NegRisk events grouped"
        );

        events
    }

    /// Group non-NegRisk binary markets by their event title.
    ///
    /// Returns groups with at least 2 markets that share the same `event_title`.
    /// This captures Polymarket's grouped binary market format (e.g. "What price
    /// will Bitcoin hit in 2026?" containing "Will Bitcoin reach $200,000?", etc.).
    pub fn group_binary_events(markets: &[MarketInfo]) -> Vec<BinaryEventGroup> {
        let mut groups: HashMap<String, Vec<MarketInfo>> = HashMap::new();

        for market in markets {
            if market.neg_risk || !market.active {
                continue;
            }
            if let Some(ref title) = market.event_title
                && !title.is_empty()
            {
                groups.entry(title.clone()).or_default().push(market.clone());
            }
        }

        let events: Vec<BinaryEventGroup> = groups
            .into_iter()
            .filter(|(_, ms)| ms.len() >= 2)
            .map(|(title, ms)| BinaryEventGroup { title, markets: ms })
            .collect();

        tracing::info!(
            binary_event_groups = events.len(),
            total_grouped_markets = events.iter().map(|e| e.markets.len()).sum::<usize>(),
            "Binary event groups formed"
        );

        events
    }

    /// Get all token IDs from NegRisk events (for WebSocket subscription).
    pub fn extract_neg_risk_token_ids(events: &[NegRiskEvent]) -> Vec<U256> {
        events
            .iter()
            .flat_map(|e| e.markets.iter())
            .flat_map(|m| m.tokens.iter().map(|t| t.token_id))
            .collect()
    }

    /// Fetch a single market by its condition ID.
    pub async fn get_market_by_condition_id(
        &self,
        condition_id: B256,
    ) -> anyhow::Result<Option<MarketInfo>> {
        let request = polymarket_client_sdk::gamma::types::request::MarketsRequest::builder()
            .condition_ids(vec![condition_id])
            .build();

        let markets = self.client.markets(&request).await?;

        let market = match markets.into_iter().next() {
            Some(m) => m,
            None => return Ok(None),
        };

        Ok(self.convert_market(&market, false, None, None))
    }

    /// Get all token IDs from discovered markets (for WebSocket subscription).
    pub fn extract_token_ids(markets: &[MarketInfo]) -> Vec<U256> {
        markets
            .iter()
            .flat_map(|m| m.tokens.iter().map(|t| t.token_id))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn make_market(question: &str, event_title: Option<&str>, neg_risk: bool) -> MarketInfo {
        MarketInfo {
            condition_id: B256::ZERO,
            question_id: B256::ZERO,
            question: question.into(),
            neg_risk,
            neg_risk_market_id: None,
            tokens: vec![
                TokenInfo {
                    token_id: U256::from(1u64),
                    outcome: Outcome::Yes,
                    complement_id: U256::from(2u64),
                },
                TokenInfo {
                    token_id: U256::from(2u64),
                    outcome: Outcome::No,
                    complement_id: U256::from(1u64),
                },
            ],
            tick_size: dec!(0.01),
            fee_rate_bps: 200,
            active: true,
            liquidity: dec!(1000),
            event_title: event_title.map(String::from),
            end_date: None,
            category: None,
            outcome_prices: None,
            gamma_best_bid: None,
            gamma_best_ask: None,
        }
    }

    #[test]
    fn test_group_binary_events_basic() {
        let markets = vec![
            make_market("Will BTC reach $200k?", Some("Bitcoin prices 2026"), false),
            make_market("Will BTC reach $150k?", Some("Bitcoin prices 2026"), false),
            make_market("Will BTC reach $100k?", Some("Bitcoin prices 2026"), false),
            make_market("Unrelated solo market", Some("Other event"), false),
        ];

        let groups = GammaFeed::group_binary_events(&markets);
        // "Bitcoin prices 2026" has 3 markets → 1 group
        // "Other event" has 1 market → not a group (< 2)
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].title, "Bitcoin prices 2026");
        assert_eq!(groups[0].markets.len(), 3);
    }

    #[test]
    fn test_group_binary_events_excludes_neg_risk() {
        let markets = vec![
            make_market("Will BTC reach $200k?", Some("BTC event"), true), // neg_risk=true
            make_market("Will BTC reach $150k?", Some("BTC event"), true), // neg_risk=true
        ];

        let groups = GammaFeed::group_binary_events(&markets);
        assert!(groups.is_empty(), "NegRisk markets should be excluded");
    }

    #[test]
    fn test_group_binary_events_no_title() {
        let markets = vec![
            make_market("Will BTC reach $200k?", None, false),
            make_market("Will BTC reach $150k?", None, false),
        ];

        let groups = GammaFeed::group_binary_events(&markets);
        assert!(groups.is_empty(), "Markets without event_title should be excluded");
    }

    #[test]
    fn test_group_binary_events_multiple_groups() {
        let markets = vec![
            make_market("Will BTC reach $200k?", Some("Bitcoin 2026"), false),
            make_market("Will BTC reach $150k?", Some("Bitcoin 2026"), false),
            make_market("Will ETH reach $10k?", Some("Ethereum 2026"), false),
            make_market("Will ETH reach $5k?", Some("Ethereum 2026"), false),
        ];

        let groups = GammaFeed::group_binary_events(&markets);
        assert_eq!(groups.len(), 2);
        let titles: Vec<&str> = groups.iter().map(|g| g.title.as_str()).collect();
        assert!(titles.contains(&"Bitcoin 2026"));
        assert!(titles.contains(&"Ethereum 2026"));
    }
}
