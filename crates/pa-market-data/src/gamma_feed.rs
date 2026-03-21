use alloy::primitives::{B256, U256};
use pa_core::config::Settings;
use pa_core::crypto::{crypto_search_terms, is_crypto_price_market_text};
use pa_core::types::{BinaryEventGroup, MarketInfo, NegRiskEvent, Outcome, TokenInfo};
use polymarket_client_sdk::gamma::types::response::{Event, SearchResults};
use rust_decimal::Decimal;
use std::collections::{BTreeMap, HashMap, HashSet};

/// Discovers markets from the Polymarket Gamma API and filters candidates.
///
/// Uses a custom `reqwest::Client` with HTTP/1.1 forced to avoid HTTP/2 stream
/// reset issues with Gamma API's CDN. The SDK's built-in client negotiates HTTP/2
/// via ALPN but the CDN doesn't handle it correctly, causing PROTOCOL_ERROR resets.
pub struct GammaFeed {
    http_client: reqwest::Client,
    gamma_host: String,
    min_liquidity: Decimal,
    min_volume_24h: Decimal,
    max_markets: usize,
    enabled_strategies: Vec<String>,
    crypto_search_terms: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SearchTermSource {
    Default,
    Custom,
}

impl GammaFeed {
    pub fn new(settings: &Settings) -> anyhow::Result<Self> {
        let http_client = reqwest::Client::builder()
            .http1_only()
            .timeout(std::time::Duration::from_secs(90))
            .connect_timeout(std::time::Duration::from_secs(15))
            .pool_max_idle_per_host(0)
            .user_agent("polyalpha/0.1")
            .build()?;
        Ok(Self {
            http_client,
            gamma_host: settings.gamma.host.clone(),
            min_liquidity: settings.market_filter.min_liquidity,
            min_volume_24h: settings.market_filter.min_volume_24h,
            max_markets: settings.market_filter.max_markets,
            enabled_strategies: settings.strategy.enabled.clone(),
            crypto_search_terms: settings.crypto_alpha.discovery_search_terms.clone(),
        })
    }

    /// Create with default Gamma API endpoint.
    pub fn with_defaults(settings: &Settings) -> Self {
        let http_client = reqwest::Client::builder()
            .http1_only()
            .timeout(std::time::Duration::from_secs(90))
            .connect_timeout(std::time::Duration::from_secs(15))
            .pool_max_idle_per_host(0)
            .user_agent("polyalpha/0.1")
            .build()
            .expect("Failed to build HTTP client");
        Self {
            http_client,
            gamma_host: "https://gamma-api.polymarket.com".to_string(),
            min_liquidity: settings.market_filter.min_liquidity,
            min_volume_24h: settings.market_filter.min_volume_24h,
            max_markets: settings.market_filter.max_markets,
            enabled_strategies: settings.strategy.enabled.clone(),
            crypto_search_terms: settings.crypto_alpha.discovery_search_terms.clone(),
        }
    }

    /// Discover active markets from Gamma API.
    ///
    /// Two discovery modes based on enabled strategies:
    /// - **Directional-only** (weather, crypto): Uses `/public-search` API for targeted keyword
    ///   search — returns relevant markets in 2-5 requests instead of paginating all ~500 events.
    /// - **General strategies** (liquidity_rewards): Falls back to
    ///   full `/events` pagination since these need broad market coverage.
    pub async fn discover_markets(&self) -> anyhow::Result<Vec<MarketInfo>> {
        tracing::info!("Discovering markets from Gamma API");

        // Check which discovery mode to use
        let general_strategies = ["liquidity_rewards"];
        let needs_full_scan = self.enabled_strategies.is_empty()
            || self
                .enabled_strategies
                .iter()
                .any(|s| general_strategies.contains(&s.as_str()));

        let all_markets = if needs_full_scan {
            self.discover_via_pagination().await?
        } else {
            self.discover_via_search().await?
        };

        tracing::info!(
            total_discovered = all_markets.len(),
            "Raw market discovery complete"
        );

        // Partition into strategy-relevant vs general markets.
        let needs_general_markets = needs_full_scan;
        let mut strategy_markets = Vec::new();
        let mut general_markets = Vec::new();
        let crypto_enabled = self.enabled_strategies.iter().any(|s| s == "crypto");
        let mut crypto_relevance_counts: BTreeMap<&'static str, usize> = BTreeMap::new();

        for m in all_markets {
            if !m.active {
                continue;
            }
            let relevance_reasons = Self::market_relevance_reasons(&m, &self.enabled_strategies);
            if !relevance_reasons.is_empty() {
                if crypto_enabled {
                    for reason in &relevance_reasons {
                        *crypto_relevance_counts.entry(*reason).or_default() += 1;
                    }
                }
                if crypto_enabled
                    && relevance_reasons
                        .iter()
                        .any(|reason| matches!(*reason, "event_title" | "category+crypto_text"))
                {
                    tracing::debug!(
                        question = %m.question,
                        event_title = ?m.event_title,
                        category = ?m.category,
                        reasons = ?relevance_reasons,
                        "Crypto market discovery relevance matched"
                    );
                }
                strategy_markets.push(m);
            } else if needs_general_markets {
                general_markets.push(m);
            }
        }

        // Sort general markets by liquidity descending, fill remaining slots.
        // When liquidity_rewards is enabled, skip truncation — LR needs broad
        // market coverage to match against CLOB reward condition_ids.
        // WS subscription is independently limited by ws_max_instruments.
        general_markets.sort_by(|a, b| b.liquidity.cmp(&a.liquidity));
        let lr_enabled = self
            .enabled_strategies
            .iter()
            .any(|s| s == "liquidity_rewards");
        if !lr_enabled {
            let remaining_slots = self.max_markets.saturating_sub(strategy_markets.len());
            general_markets.truncate(remaining_slots);
        }

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

        if crypto_enabled {
            tracing::info!(
                crypto_relevance_counts = ?crypto_relevance_counts,
                "Crypto market discovery relevance summary"
            );
        }

        Ok(filtered)
    }

    /// Discover markets via targeted `/public-search` queries.
    ///
    /// Much faster than full pagination — fetches all search terms in parallel,
    /// then deduplicates by condition_id.
    async fn discover_via_search(&self) -> anyhow::Result<Vec<MarketInfo>> {
        let check_weather = self.enabled_strategies.iter().any(|s| s == "weather");
        let check_crypto = self.enabled_strategies.iter().any(|s| s == "crypto");

        let mut search_terms: Vec<String> = Vec::new();
        let mut crypto_term_sources: BTreeMap<String, SearchTermSource> = BTreeMap::new();
        if check_weather {
            // "weather" catches most weather markets; specific terms catch the rest
            search_terms.extend(
                ["weather", "temperature", "inches of snow", "inches of rain"]
                    .into_iter()
                    .map(str::to_string),
            );
        }
        if check_crypto {
            let merged_terms = self.merged_crypto_search_terms_with_source();
            search_terms.extend(merged_terms.iter().map(|(term, _)| term.clone()));
            crypto_term_sources.extend(merged_terms);
        }

        tracing::info!(
            terms = ?search_terms,
            "Discovering markets via search API (parallel)"
        );

        // Fire all search queries in parallel
        let futures: Vec<_> = search_terms
            .iter()
            .map(|term| {
                let term = term.to_string();
                async move {
                    let result = self.search_events(&term).await;
                    (term, result)
                }
            })
            .collect();

        let results = futures::future::join_all(futures).await;

        // Merge results with deduplication
        let mut all_markets = Vec::new();
        let mut seen_condition_ids: HashSet<B256> = HashSet::new();
        let mut custom_term_new_market_counts: BTreeMap<String, usize> = BTreeMap::new();

        for (term, result) in results {
            match result {
                Ok(events) => {
                    let mut term_count = 0;
                    for event in events {
                        let event_neg_risk = event.neg_risk.unwrap_or(false);
                        let event_neg_risk_market_id = event.neg_risk_market_id;
                        let event_title = event.title.clone();

                        if let Some(markets) = event.markets {
                            for market in markets {
                                if let Some(info) = self.convert_market(
                                    &market,
                                    event_neg_risk,
                                    event_neg_risk_market_id,
                                    event_title.clone(),
                                ) {
                                    if seen_condition_ids.insert(info.condition_id) {
                                        all_markets.push(info);
                                        term_count += 1;
                                    }
                                }
                            }
                        }
                    }
                    tracing::debug!(
                        term,
                        source = ?crypto_term_sources.get(&term),
                        new_markets = term_count,
                        total = all_markets.len(),
                        "Search term completed"
                    );
                    if matches!(
                        crypto_term_sources.get(&term),
                        Some(SearchTermSource::Custom)
                    ) {
                        custom_term_new_market_counts.insert(term.clone(), term_count);
                    }
                }
                Err(e) => {
                    tracing::warn!(term, error = %e, "Search query failed, skipping");
                }
            }
        }

        if check_crypto && !custom_term_new_market_counts.is_empty() {
            tracing::info!(
                custom_crypto_search_term_hits = ?custom_term_new_market_counts,
                "Crypto custom search term discovery summary"
            );
        }

        Ok(all_markets)
    }

    fn merged_crypto_search_terms_with_source(&self) -> BTreeMap<String, SearchTermSource> {
        let mut merged_terms: BTreeMap<String, SearchTermSource> = BTreeMap::new();
        for term in crypto_search_terms() {
            merged_terms.insert(term.to_string(), SearchTermSource::Default);
        }
        for term in &self.crypto_search_terms {
            let trimmed = term.trim();
            if !trimmed.is_empty() {
                merged_terms.insert(trimmed.to_string(), SearchTermSource::Custom);
            }
        }
        merged_terms
    }

    /// Search events by keyword using the `/public-search` endpoint.
    ///
    /// Returns matching events with their embedded markets.
    /// Retries up to 3 times with exponential backoff.
    async fn search_events(&self, query: &str) -> anyhow::Result<Vec<Event>> {
        let url = format!(
            "{}/public-search?q={}&limit_per_type=100&events_status=active",
            self.gamma_host.trim_end_matches('/'),
            urlencoding::encode(query),
        );

        for attempt in 0..3u32 {
            if attempt > 0 {
                let delay = std::time::Duration::from_secs(2u64.pow(attempt));
                tokio::time::sleep(delay).await;
            }

            match self.fetch_search_results(&url).await {
                Ok(results) => {
                    return Ok(results.events.unwrap_or_default());
                }
                Err(e) => {
                    tracing::warn!(
                        query,
                        attempt = attempt + 1,
                        error = %e,
                        "Search request failed"
                    );
                    if attempt == 2 {
                        return Err(e);
                    }
                }
            }
        }

        Ok(Vec::new())
    }

    /// Fetch and parse a search results page.
    async fn fetch_search_results(&self, url: &str) -> anyhow::Result<SearchResults> {
        let resp = self.http_client.get(url).send().await.map_err(|e| {
            anyhow::anyhow!(
                "search request failed: {} (timeout={}, connect={})",
                e,
                e.is_timeout(),
                e.is_connect()
            )
        })?;

        let status = resp.status();
        if !status.is_success() {
            return Err(anyhow::anyhow!("HTTP {}", status));
        }

        let body_text = resp
            .text()
            .await
            .map_err(|e| anyhow::anyhow!("body read failed: {}", e))?;

        serde_json::from_str(&body_text).map_err(|e| {
            let preview = &body_text[..body_text.len().min(200)];
            anyhow::anyhow!(
                "JSON parse failed: {} (len={}, preview={})",
                e,
                body_text.len(),
                preview
            )
        })
    }

    /// Discover markets via full pagination of `/events` endpoint.
    ///
    /// Used when general strategies (LR, etc.) need broad market coverage.
    async fn discover_via_pagination(&self) -> anyhow::Result<Vec<MarketInfo>> {
        tracing::info!("Discovering markets via full events pagination");

        let mut all_markets = Vec::new();
        let mut last_error = None;

        // Retry the entire pagination up to 5 times with exponential backoff
        for attempt in 0..5u32 {
            if attempt > 0 {
                let delay = std::time::Duration::from_secs(2u64.pow(attempt).min(30));
                tracing::warn!(
                    attempt = attempt + 1,
                    delay_secs = delay.as_secs(),
                    "Retrying Gamma API discovery"
                );
                tokio::time::sleep(delay).await;
            }

            all_markets.clear();
            let mut offset = 0i32;
            let limit = 100i32;
            let mut consecutive_page_failures = 0u32;

            loop {
                let url = format!(
                    "{}/events?limit={}&offset={}&active=true&closed=false",
                    self.gamma_host.trim_end_matches('/'),
                    limit,
                    offset,
                );

                // Per-page retry: try each page up to 3 times with short backoff
                let mut page_events: Option<Vec<Event>> = None;
                for page_attempt in 0..3u32 {
                    if page_attempt > 0 {
                        let delay = std::time::Duration::from_secs(2u64.pow(page_attempt));
                        tracing::debug!(offset, page_attempt = page_attempt + 1, "Retrying page");
                        tokio::time::sleep(delay).await;
                    }

                    match self.fetch_page(&url).await {
                        Ok(events) => {
                            page_events = Some(events);
                            break;
                        }
                        Err(e) => {
                            tracing::warn!(
                                attempt = attempt + 1,
                                page_attempt = page_attempt + 1,
                                offset,
                                error = %e,
                                "Gamma API page fetch failed"
                            );
                            last_error = Some(e.to_string());
                        }
                    }
                }

                let events = match page_events {
                    Some(e) => {
                        consecutive_page_failures = 0;
                        e
                    }
                    None => {
                        consecutive_page_failures += 1;
                        if consecutive_page_failures >= 3 {
                            tracing::warn!(
                                offset,
                                "3 consecutive page failures, stopping pagination"
                            );
                            break;
                        }
                        tracing::warn!(offset, "Skipping failed page, trying next offset");
                        offset += limit;
                        continue;
                    }
                };

                let page_count = events.len() as i32;

                for event in events {
                    let event_neg_risk = event.neg_risk.unwrap_or(false);
                    let event_neg_risk_market_id = event.neg_risk_market_id;
                    let event_title = event.title.clone();

                    let markets = match event.markets {
                        Some(m) => m,
                        None => continue,
                    };

                    for market in markets {
                        if let Some(info) = self.convert_market(
                            &market,
                            event_neg_risk,
                            event_neg_risk_market_id,
                            event_title.clone(),
                        ) {
                            all_markets.push(info);
                        }
                    }
                }

                tracing::debug!(
                    offset,
                    page_events = page_count,
                    total_markets = all_markets.len(),
                    "Gamma API page fetched"
                );

                if page_count < limit {
                    break;
                }
                offset += page_count;

                // Delay between pages to avoid CDN rate limiting
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }

            if !all_markets.is_empty() {
                tracing::info!(
                    total_discovered = all_markets.len(),
                    pages_fetched = offset / limit + 1,
                    "Gamma API pagination complete"
                );
                break;
            }

            tracing::warn!(attempt = attempt + 1, "Gamma API returned 0 usable markets");
            last_error = last_error.or_else(|| Some("No markets found".to_string()));
        }

        if all_markets.is_empty() {
            if let Some(err) = &last_error {
                tracing::error!(error = %err, "All Gamma API retry attempts failed");
            }
        }

        Ok(all_markets)
    }

    /// Fetch a single page from the Gamma API events endpoint.
    ///
    /// Reads response as text first for better error diagnostics, then parses JSON.
    async fn fetch_page(&self, url: &str) -> anyhow::Result<Vec<Event>> {
        let resp = self.http_client.get(url).send().await.map_err(|e| {
            anyhow::anyhow!(
                "request failed: {} (timeout={}, connect={})",
                e,
                e.is_timeout(),
                e.is_connect()
            )
        })?;

        let status = resp.status();
        if !status.is_success() {
            return Err(anyhow::anyhow!("HTTP {}", status));
        }

        let body_text = resp
            .text()
            .await
            .map_err(|e| anyhow::anyhow!("body read failed: {}", e))?;

        serde_json::from_str(&body_text).map_err(|e| {
            let preview = &body_text[..body_text.len().min(200)];
            anyhow::anyhow!(
                "JSON parse failed: {} (body_len={}, preview={})",
                e,
                body_text.len(),
                preview
            )
        })
    }

    /// Check if a market question is relevant to active strategies.
    /// These markets are always included regardless of liquidity ranking.
    pub fn is_strategy_relevant(question: &str) -> bool {
        // When called without strategy filter, check all strategies
        Self::is_relevant_for_strategies(question, &[])
    }

    pub fn is_market_relevant_for_strategies(market: &MarketInfo, enabled: &[String]) -> bool {
        !Self::market_relevance_reasons(market, enabled).is_empty()
    }

    pub fn market_relevance_reasons(market: &MarketInfo, enabled: &[String]) -> Vec<&'static str> {
        let mut reasons = Vec::new();

        if Self::is_relevant_for_strategies(&market.question, enabled) {
            reasons.push("question");
        }

        if let Some(title) = market.event_title.as_deref()
            && Self::is_relevant_for_strategies(title, enabled)
        {
            reasons.push("event_title");
        }

        let check_crypto = enabled.is_empty() || enabled.iter().any(|s| s == "crypto");
        if check_crypto
            && matches!(market.category.as_deref(), Some(category) if category.eq_ignore_ascii_case("crypto"))
        {
            let title = market.event_title.as_deref().unwrap_or_default();
            let combined = format!("{} {}", market.question, title);
            if is_crypto_price_market_text(&combined) {
                reasons.push("category+crypto_text");
            }
        }

        reasons
    }

    /// Check if a market question is relevant to the given enabled strategies.
    /// Empty slice means check all strategies (backwards compatibility).
    pub fn is_relevant_for_strategies(question: &str, enabled: &[String]) -> bool {
        let check_weather = enabled.is_empty() || enabled.iter().any(|s| s == "weather");
        let check_crypto = enabled.is_empty() || enabled.iter().any(|s| s == "crypto");
        let lower = question.to_lowercase();

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
            if is_crypto_price_market_text(&lower) {
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
        let tick_size = market
            .order_price_min_tick_size
            .unwrap_or(Decimal::new(1, 2)); // default 0.01
        let fee_rate_bps = market.taker_base_fee.unwrap_or(0) as u32;

        // Determine neg_risk status
        let neg_risk = event_neg_risk || market.neg_risk.unwrap_or(false);
        let neg_risk_market_id = event_neg_risk_market_id.or(market.neg_risk_market_id);

        // Apply liquidity/volume filters (relaxed for strategy-relevant markets
        // and when liquidity_rewards is enabled, since reward markets are
        // intentionally low-liquidity and need providers).
        let liquidity = market.liquidity.unwrap_or(Decimal::ZERO);
        let volume_24h = market.volume_24hr.unwrap_or(Decimal::ZERO);
        let strategy_relevant_preview = MarketInfo {
            condition_id,
            question_id,
            question: question.clone(),
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
            event_title: event_title.clone(),
            end_date: market.end_date,
            category: market.category.clone(),
            outcome_prices: market.outcome_prices.clone(),
            gamma_best_bid: market.best_bid,
            gamma_best_ask: market.best_ask,
            rewards_min_size: market.rewards_min_size,
            rewards_max_spread: market.rewards_max_spread,
            rewards_daily_rate: None,
            holding_rewards_enabled: market.holding_rewards_enabled.unwrap_or(false),
            fees_enabled: market.fees_enabled.unwrap_or(false),
        };
        let strategy_relevant =
            !Self::market_relevance_reasons(&strategy_relevant_preview, &self.enabled_strategies)
                .is_empty();
        let lr_enabled = self
            .enabled_strategies
            .iter()
            .any(|s| s == "liquidity_rewards");

        if !strategy_relevant && !lr_enabled {
            if liquidity < self.min_liquidity {
                return None;
            }
            if volume_24h < self.min_volume_24h {
                return None;
            }
        }

        // Extract CLOB liquidity rewards fields
        let rewards_min_size = market.rewards_min_size;
        let rewards_max_spread = market.rewards_max_spread;
        let today = chrono::Utc::now().date_naive();
        let rewards_daily_rate = market.clob_rewards.as_ref().and_then(|rewards| {
            let rate: Decimal = rewards
                .iter()
                .filter(|r| r.end_date.map_or(true, |ed| ed >= today))
                .filter_map(|r| r.rewards_daily_rate)
                .sum();
            if rate > Decimal::ZERO {
                Some(rate)
            } else {
                None
            }
        });
        let holding_rewards_enabled = market.holding_rewards_enabled.unwrap_or(false);
        let fees_enabled = market.fees_enabled.unwrap_or(false);

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
            rewards_min_size,
            rewards_max_spread,
            rewards_daily_rate,
            holding_rewards_enabled,
            fees_enabled,
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
                groups
                    .entry(title.clone())
                    .or_default()
                    .push(market.clone());
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
        let url = format!(
            "{}/markets?condition_ids={}",
            self.gamma_host.trim_end_matches('/'),
            condition_id,
        );

        let markets: Vec<polymarket_client_sdk::gamma::types::response::Market> =
            self.http_client.get(&url).send().await?.json().await?;

        let market = match markets.into_iter().next() {
            Some(m) => m,
            None => return Ok(None),
        };

        Ok(self.convert_market(&market, false, None, None))
    }

    /// Fetch markets for held positions, bypassing active/closed filters.
    ///
    /// Used to ensure exit scanning can find markets for positions even after
    /// the market has expired or been resolved.
    pub async fn fetch_position_markets(&self, condition_ids: &[B256]) -> Vec<MarketInfo> {
        let mut results = Vec::new();

        for condition_id in condition_ids {
            let url = format!(
                "{}/markets?condition_ids={}",
                self.gamma_host.trim_end_matches('/'),
                condition_id,
            );

            match self.http_client.get(&url).send().await {
                Ok(resp) => {
                    match resp
                        .json::<Vec<polymarket_client_sdk::gamma::types::response::Market>>()
                        .await
                    {
                        Ok(markets) => {
                            if let Some(market) = markets.into_iter().next() {
                                if let Some(info) = self.convert_market_for_exit(&market) {
                                    results.push(info);
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!(
                                condition_id = %condition_id,
                                error = %e,
                                "Failed to parse position market from Gamma API"
                            );
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        condition_id = %condition_id,
                        error = %e,
                        "Failed to fetch position market from Gamma API"
                    );
                }
            }
        }

        tracing::info!(
            requested = condition_ids.len(),
            found = results.len(),
            "Fetched position markets (bypassing active/closed filter)"
        );

        results
    }

    /// Convert a market response to MarketInfo without active/closed checks.
    ///
    /// Used for held position markets that may be expired/closed but still
    /// need to be in the scan list for exit logic.
    fn convert_market_for_exit(
        &self,
        market: &polymarket_client_sdk::gamma::types::response::Market,
    ) -> Option<MarketInfo> {
        let condition_id = market.condition_id?;
        let question_id = market.question_id.unwrap_or(B256::ZERO);
        let question = market.question.clone().unwrap_or_default();
        let active = market.active.unwrap_or(false);

        // No active/closed check here — we need these for exit scanning

        let clob_token_ids = market.clob_token_ids.as_ref()?;
        if clob_token_ids.len() != 2 {
            return None;
        }

        let yes_token_id = clob_token_ids[0];
        let no_token_id = clob_token_ids[1];

        let tick_size = market
            .order_price_min_tick_size
            .unwrap_or(Decimal::new(1, 2));
        let fee_rate_bps = market.taker_base_fee.unwrap_or(0) as u32;

        let neg_risk = market.neg_risk.unwrap_or(false);
        let neg_risk_market_id = market.neg_risk_market_id;

        let liquidity = market.liquidity.unwrap_or(Decimal::ZERO);

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
            event_title: None,
            end_date: market.end_date,
            category: market.category.clone(),
            outcome_prices: market.outcome_prices.clone(),
            gamma_best_bid: market.best_bid,
            gamma_best_ask: market.best_ask,
            rewards_min_size: None,
            rewards_max_spread: None,
            rewards_daily_rate: None,
            holding_rewards_enabled: false,
            fees_enabled: false,
        })
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
    use pa_core::crypto::crypto_search_terms;
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
            rewards_min_size: None,
            rewards_max_spread: None,
            rewards_daily_rate: None,
            holding_rewards_enabled: false,
            fees_enabled: false,
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
        assert!(
            groups.is_empty(),
            "Markets without event_title should be excluded"
        );
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

    #[test]
    fn test_crypto_search_terms_include_alt_assets() {
        let terms = crypto_search_terms();
        assert!(terms.contains(&"bitcoin price"));
        assert!(terms.contains(&"ethereum price"));
        assert!(terms.contains(&"solana price"));
        assert!(terms.contains(&"doge price"));
    }

    #[test]
    fn test_market_relevance_can_use_event_title_for_crypto() {
        let market = make_market(
            "Will it happen by June 30?",
            Some("Solana price targets"),
            false,
        );
        let enabled = vec!["crypto".to_string()];
        assert!(GammaFeed::is_market_relevant_for_strategies(
            &market, &enabled
        ));
        assert_eq!(
            GammaFeed::market_relevance_reasons(&market, &enabled),
            vec!["event_title"]
        );
    }

    #[test]
    fn test_market_relevance_reasons_can_include_multiple_sources() {
        let market = make_market(
            "Will Bitcoin reach $150k?",
            Some("Bitcoin price targets"),
            false,
        );
        let enabled = vec!["crypto".to_string()];
        assert_eq!(
            GammaFeed::market_relevance_reasons(&market, &enabled),
            vec!["question", "event_title"]
        );
    }

    #[test]
    fn test_gamma_feed_merges_custom_crypto_search_terms() {
        let feed = GammaFeed {
            http_client: reqwest::Client::new(),
            gamma_host: "https://gamma-api.polymarket.com".to_string(),
            min_liquidity: dec!(0),
            min_volume_24h: dec!(0),
            max_markets: 100,
            enabled_strategies: vec!["crypto".to_string()],
            crypto_search_terms: vec!["litecoin price".to_string(), "doge price".to_string()],
        };

        let terms = feed.merged_crypto_search_terms_with_source();
        assert_eq!(terms.get("litecoin price"), Some(&SearchTermSource::Custom));
        assert_eq!(terms.get("doge price"), Some(&SearchTermSource::Custom));
        assert_eq!(terms.get("bitcoin price"), Some(&SearchTermSource::Default));
    }
}
