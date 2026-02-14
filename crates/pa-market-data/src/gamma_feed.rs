use alloy::primitives::{B256, U256};
use futures::{StreamExt, pin_mut};
use pa_core::config::Settings;
use pa_core::types::{MarketInfo, NegRiskEvent, Outcome, TokenInfo};
use rust_decimal::Decimal;
use std::collections::HashMap;

/// Discovers markets from the Polymarket Gamma API and filters candidates.
pub struct GammaFeed {
    client: polymarket_client_sdk::gamma::Client,
    min_liquidity: Decimal,
    min_volume_24h: Decimal,
    max_markets: usize,
}

impl GammaFeed {
    pub fn new(settings: &Settings) -> anyhow::Result<Self> {
        let client = polymarket_client_sdk::gamma::Client::new(&settings.gamma.host)?;
        Ok(Self {
            client,
            min_liquidity: settings.market_filter.min_liquidity,
            min_volume_24h: settings.market_filter.min_volume_24h,
            max_markets: settings.market_filter.max_markets,
        })
    }

    /// Create with default Gamma API endpoint.
    pub fn with_defaults(settings: &Settings) -> Self {
        Self {
            client: polymarket_client_sdk::gamma::Client::default(),
            min_liquidity: settings.market_filter.min_liquidity,
            min_volume_24h: settings.market_filter.min_volume_24h,
            max_markets: settings.market_filter.max_markets,
        }
    }

    /// Discover all active binary markets from Gamma API.
    ///
    /// Uses pagination via `stream_data()` to enumerate all active events,
    /// then extracts binary markets with CLOB token IDs.
    pub async fn discover_markets(&self) -> anyhow::Result<Vec<MarketInfo>> {
        tracing::info!("Discovering markets from Gamma API");

        let mut all_markets = Vec::new();

        // Stream all active events with pagination
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
            500, // max page size
        );

        pin_mut!(stream);

        while let Some(event_result) = stream.next().await {
            let event = match event_result {
                Ok(e) => e,
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to fetch event, skipping");
                    continue;
                }
            };

            let event_neg_risk = event.neg_risk.unwrap_or(false);
            let event_neg_risk_market_id = event.neg_risk_market_id;
            let event_title = event.title.clone();

            // Extract markets from this event
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

        tracing::info!(
            total_discovered = all_markets.len(),
            "Raw market discovery complete"
        );

        // Filter and sort by liquidity descending (keep best markets when truncating)
        let mut filtered: Vec<MarketInfo> = all_markets
            .into_iter()
            .filter(|m| m.active)
            .collect();

        filtered.sort_by(|a, b| b.liquidity.cmp(&a.liquidity));
        filtered.truncate(self.max_markets);

        tracing::info!(
            filtered_count = filtered.len(),
            "Market discovery complete after filtering"
        );

        Ok(filtered)
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

        // Apply liquidity/volume filters
        let liquidity = market.liquidity.unwrap_or(Decimal::ZERO);
        let volume_24h = market.volume_24hr.unwrap_or(Decimal::ZERO);

        if liquidity < self.min_liquidity {
            return None;
        }
        if volume_24h < self.min_volume_24h {
            return None;
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
