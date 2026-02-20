use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use alloy::primitives::U256;
use async_trait::async_trait;
use chrono::{NaiveDate, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use uuid::Uuid;

use pa_core::config::CryptoAlphaConfig;
use pa_core::traits::Strategy;
use pa_core::types::{
    ArbitrageOpportunity, BinaryEventGroup, ExecutionPlan, MarketInfo, NegRiskEvent, OrderBook,
    StrategyType, TradeSide,
};

use crate::profitability::ProfitCalculator;
use crate::weather::{contains_word, normal_cdf, parse_target_date, with_retry};

// ──── Asset Mapping ────

#[derive(Debug)]
pub struct CryptoAsset {
    pub name: &'static str,
    pub keywords: &'static [&'static str],
    pub binance_symbol: &'static str,
    pub coingecko_id: &'static str,
}

pub static CRYPTO_ASSETS: &[CryptoAsset] = &[
    CryptoAsset {
        name: "Bitcoin",
        keywords: &["bitcoin", "btc"],
        binance_symbol: "BTCUSDT",
        coingecko_id: "bitcoin",
    },
    CryptoAsset {
        name: "Ethereum",
        keywords: &["ethereum", "eth"],
        binance_symbol: "ETHUSDT",
        coingecko_id: "ethereum",
    },
    CryptoAsset {
        name: "Solana",
        keywords: &["solana"],
        binance_symbol: "SOLUSDT",
        coingecko_id: "solana",
    },
    CryptoAsset {
        name: "BNB",
        keywords: &["bnb", "binance coin"],
        binance_symbol: "BNBUSDT",
        coingecko_id: "binancecoin",
    },
    CryptoAsset {
        name: "XRP",
        keywords: &["xrp", "ripple"],
        binance_symbol: "XRPUSDT",
        coingecko_id: "ripple",
    },
    CryptoAsset {
        name: "Dogecoin",
        keywords: &["dogecoin"],
        binance_symbol: "DOGEUSDT",
        coingecko_id: "dogecoin",
    },
    CryptoAsset {
        name: "Cardano",
        keywords: &["cardano"],
        binance_symbol: "ADAUSDT",
        coingecko_id: "cardano",
    },
    CryptoAsset {
        name: "Avalanche",
        keywords: &["avax"],
        binance_symbol: "AVAXUSDT",
        coingecko_id: "avalanche-2",
    },
    CryptoAsset {
        name: "Polkadot",
        keywords: &["polkadot"],
        binance_symbol: "DOTUSDT",
        coingecko_id: "polkadot",
    },
    CryptoAsset {
        name: "Polygon",
        keywords: &["polygon", "matic"],
        binance_symbol: "POLUSDT",
        coingecko_id: "polygon-ecosystem-token",
    },
];

/// Find a matching crypto asset from question text.
pub fn find_asset(question: &str) -> Option<&'static CryptoAsset> {
    let lower = question.to_lowercase();

    // Exclude non-price markets that happen to contain crypto asset names
    if lower.contains("gas price") || lower.contains("gas fee")
        || lower.contains("volatility index")
        || lower.contains("dominance")
        || lower.contains("kimchi premium")
    {
        return None;
    }

    CRYPTO_ASSETS
        .iter()
        .find(|asset| asset.keywords.iter().any(|kw| contains_word(&lower, kw)))
}

// ──── Question Parser ────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PriceDirection {
    Above,
    Below,
}

#[derive(Debug)]
pub struct CryptoQuestion {
    pub asset: &'static CryptoAsset,
    pub threshold: f64,
    pub direction: PriceDirection,
    pub target_date: Option<NaiveDate>,
}

/// Extract a dollar price from text.
/// Handles: $100,000 / $100000 / 100k / 1.5m / plain numbers after $
pub fn extract_price(text: &str) -> Option<f64> {
    let lower = text.to_lowercase();

    // Try patterns with $ prefix first
    for (i, _) in lower.match_indices('$') {
        let rest = &lower[i + 1..];
        if let Some(price) = parse_price_token(rest) {
            return Some(price);
        }
    }

    // Try "k" suffix numbers without $ (e.g. "100k")
    for word in lower.split_whitespace() {
        let word = word.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '.' && c != ',');
        if let Some(price) = parse_suffixed_number(word) {
            // Filter out years (1900-2099)
            if (1900.0..=2099.0).contains(&price) && price == price.floor() {
                continue;
            }
            return Some(price);
        }
    }

    None
}

/// Parse a number token that may have commas and/or k/m suffix.
fn parse_price_token(rest: &str) -> Option<f64> {
    // Collect digits, dots, commas, and possible k/m suffix
    let mut end = 0;
    for c in rest.chars() {
        if c.is_ascii_digit() || c == '.' || c == ',' {
            end += c.len_utf8();
        } else if c == 'k' || c == 'm' {
            end += 1;
            break;
        } else {
            break;
        }
    }
    if end == 0 {
        return None;
    }
    let token = &rest[..end];
    parse_suffixed_number(token)
}

/// Parse number with optional k/m suffix and commas.
fn parse_suffixed_number(token: &str) -> Option<f64> {
    if token.is_empty() {
        return None;
    }

    let (num_part, multiplier) = if let Some(stripped) = token.strip_suffix('k') {
        (stripped, 1_000.0)
    } else if let Some(stripped) = token.strip_suffix('m') {
        (stripped, 1_000_000.0)
    } else {
        (token, 1.0)
    };

    // Remove commas
    let cleaned: String = num_part.chars().filter(|c| *c != ',').collect();
    let val: f64 = cleaned.parse().ok()?;
    Some(val * multiplier)
}

/// Parse a crypto prediction market question.
pub fn parse_crypto_question(question: &str) -> Option<CryptoQuestion> {
    let asset = find_asset(question)?;

    let lower = question.to_lowercase();

    // Must contain a price indicator
    let has_price_indicator = lower.contains('$')
        || contains_word(&lower, "price")
        || contains_word(&lower, "reach")
        || contains_word(&lower, "hit")
        || contains_word(&lower, "exceed")
        || contains_word(&lower, "dip");

    if !has_price_indicator {
        return None;
    }

    let threshold = extract_price(question)?;

    // Direction detection: scan for below-keywords
    let direction = if lower.contains("fall below")
        || lower.contains("drop below")
        || lower.contains("drop under")
        || lower.contains("fall under")
        || (contains_word(&lower, "below") && !lower.contains("or below"))
        || contains_word(&lower, "under")
    {
        PriceDirection::Below
    } else {
        PriceDirection::Above
    };

    let target_date = parse_target_date(question);

    Some(CryptoQuestion {
        asset,
        threshold,
        direction,
        target_date,
    })
}

// ──── NegRisk Event Title Parser ────

/// Check if a NegRisk event title is crypto-related and extract the asset + optional date.
///
/// Example titles:
///   "Bitcoin price at 12pm ET on March 1"
///   "What will ETH be on February 28?"
pub fn parse_crypto_event_title(title: &str) -> Option<(&'static CryptoAsset, Option<NaiveDate>)> {
    let asset = find_asset(title)?;
    let lower = title.to_lowercase();
    let has_price_indicator = lower.contains("price")
        || lower.contains('$')
        || lower.contains("value")
        || contains_word(&lower, "worth")
        || contains_word(&lower, "hit")
        || contains_word(&lower, "dip");
    if !has_price_indicator {
        return None;
    }
    let target_date = parse_target_date(title);
    Some((asset, target_date))
}

// ──── NegRisk Outcome Range Parser ────

/// A price range parsed from a NegRisk crypto outcome market question.
#[derive(Debug, Clone)]
pub enum CryptoPriceRange {
    /// "$89,999 or below"
    AtOrBelow(f64),
    /// "$90,000 - $94,999"
    Range(f64, f64),
    /// "$100,000 or above"
    AtOrAbove(f64),
}

/// Extract all dollar prices from text.
fn extract_all_prices(text: &str) -> Vec<f64> {
    let lower = text.to_lowercase();
    let mut prices = Vec::new();

    // Find prices with $ prefix
    for (i, _) in lower.match_indices('$') {
        let rest = &lower[i + 1..];
        if let Some(price) = parse_price_token(rest) {
            prices.push(price);
        }
    }

    // If no $ prices found, try suffixed numbers (100k, 1.5m)
    if prices.is_empty() {
        for word in lower.split_whitespace() {
            let word =
                word.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '.' && c != ',');
            if let Some(price) = parse_suffixed_number(word) {
                if (1900.0..=2099.0).contains(&price) && price == price.floor() {
                    continue;
                }
                prices.push(price);
            }
        }
    }

    prices
}

/// Parse a NegRisk crypto outcome market question into a price range.
///
/// Supports formats:
/// - `"$89,999 or below"` / `"Under $90,000"` → AtOrBelow
/// - `"$100,000 or above"` / `"$100,000+"` → AtOrAbove
/// - `"$90,000 - $94,999"` → Range
pub fn parse_crypto_outcome_range(question: &str) -> Option<CryptoPriceRange> {
    let lower = question.to_lowercase();
    let prices = extract_all_prices(question);

    if lower.contains("or below")
        || lower.contains("or less")
        || lower.contains("or lower")
        || lower.contains("under ")
    {
        let price = prices.first()?;
        Some(CryptoPriceRange::AtOrBelow(*price))
    } else if lower.contains("or above")
        || lower.contains("or more")
        || lower.contains("or higher")
        || lower.contains('+')
    {
        let price = prices.first()?;
        Some(CryptoPriceRange::AtOrAbove(*price))
    } else if prices.len() >= 2 {
        Some(CryptoPriceRange::Range(prices[0], prices[1]))
    } else {
        None
    }
}

// ──── GBM Range Probability ────

/// Calculate the probability of price falling within a range under GBM.
///
/// Uses `gbm_probability()` (P(S_T > K)) to compute interval probabilities:
/// - AtOrBelow(bound): P(S_T <= bound) = 1 - P(S_T > bound)
/// - Range(lo, hi): P(lo < S_T <= hi) = P(S_T > lo) - P(S_T > hi)
/// - AtOrAbove(bound): P(S_T > bound)
pub fn gbm_range_probability(
    current_price: f64,
    range: &CryptoPriceRange,
    mu: f64,
    sigma: f64,
    days: f64,
) -> f64 {
    match range {
        CryptoPriceRange::AtOrBelow(bound) => {
            1.0 - gbm_probability(current_price, *bound, mu, sigma, days)
        }
        CryptoPriceRange::Range(lo, hi) => {
            let p_above_lo = gbm_probability(current_price, *lo, mu, sigma, days);
            let p_above_hi = gbm_probability(current_price, *hi, mu, sigma, days);
            (p_above_lo - p_above_hi).max(0.0)
        }
        CryptoPriceRange::AtOrAbove(bound) => {
            gbm_probability(current_price, *bound, mu, sigma, days)
        }
    }
}

// ──── Price Client ────

#[derive(Debug, Clone)]
pub struct CryptoPriceData {
    pub current_price: f64,
    /// 30 daily close prices, oldest first.
    pub daily_closes: Vec<f64>,
}

pub struct CryptoPriceClient {
    http: reqwest::Client,
    coingecko_api_key: Option<String>,
}

impl CryptoPriceClient {
    pub fn new(coingecko_api_key: Option<String>) -> Self {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap_or_default();
        Self {
            http,
            coingecko_api_key,
        }
    }

    /// Fetch current price + 30-day daily closes from Binance. Falls back to CoinGecko.
    pub async fn get_price_data(
        &self,
        asset: &CryptoAsset,
    ) -> anyhow::Result<CryptoPriceData> {
        // Try Binance first
        match self.fetch_binance(asset.binance_symbol).await {
            Ok(data) => return Ok(data),
            Err(e) => {
                tracing::warn!(
                    asset = asset.name,
                    error = %e,
                    "Binance fetch failed, trying CoinGecko fallback"
                );
            }
        }

        // Fallback to CoinGecko
        if self.coingecko_api_key.as_ref().is_some_and(|k| !k.is_empty()) {
            self.fetch_coingecko(asset.coingecko_id).await
        } else {
            anyhow::bail!(
                "Binance failed and CoinGecko API key not configured for {}",
                asset.name
            )
        }
    }

    async fn fetch_binance(&self, symbol: &str) -> anyhow::Result<CryptoPriceData> {
        // Current price
        let price_url = format!(
            "https://api.binance.com/api/v3/ticker/price?symbol={}",
            symbol
        );
        let http = self.http.clone();
        let price_url_clone = price_url.clone();
        let current_price: f64 = with_retry(2, || {
            let http = http.clone();
            let url = price_url_clone.clone();
            async move {
                let resp: serde_json::Value = http.get(&url).send().await?.json().await?;
                let price_str = resp["price"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("Missing price field"))?;
                price_str
                    .parse::<f64>()
                    .map_err(|e| anyhow::anyhow!("Invalid price: {}", e))
            }
        })
        .await?;

        // 30-day klines
        let kline_url = format!(
            "https://api.binance.com/api/v3/klines?symbol={}&interval=1d&limit=30",
            symbol
        );
        let http = self.http.clone();
        let kline_url_clone = kline_url.clone();
        let daily_closes: Vec<f64> = with_retry(2, || {
            let http = http.clone();
            let url = kline_url_clone.clone();
            async move {
                let resp: serde_json::Value = http.get(&url).send().await?.json().await?;
                let arr = resp
                    .as_array()
                    .ok_or_else(|| anyhow::anyhow!("Expected array from klines"))?;
                let mut closes = Vec::with_capacity(arr.len());
                for kline in arr {
                    // Index [4] is the close price
                    let close_str = kline[4]
                        .as_str()
                        .ok_or_else(|| anyhow::anyhow!("Missing close in kline"))?;
                    closes.push(
                        close_str
                            .parse::<f64>()
                            .map_err(|e| anyhow::anyhow!("Invalid close: {}", e))?,
                    );
                }
                Ok(closes)
            }
        })
        .await?;

        Ok(CryptoPriceData {
            current_price,
            daily_closes,
        })
    }

    async fn fetch_coingecko(&self, coin_id: &str) -> anyhow::Result<CryptoPriceData> {
        let api_key = self
            .coingecko_api_key
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("CoinGecko API key not set"))?;

        // Current price
        let price_url = format!(
            "https://api.coingecko.com/api/v3/simple/price?ids={}&vs_currencies=usd",
            coin_id
        );
        let http = self.http.clone();
        let key = api_key.clone();
        let price_url_clone = price_url.clone();
        let coin_id_owned = coin_id.to_string();
        let current_price: f64 = with_retry(2, || {
            let http = http.clone();
            let url = price_url_clone.clone();
            let key = key.clone();
            let coin_id = coin_id_owned.clone();
            async move {
                let resp: serde_json::Value = http
                    .get(&url)
                    .header("x-cg-demo-api-key", &key)
                    .send()
                    .await?
                    .json()
                    .await?;
                resp[&coin_id]["usd"]
                    .as_f64()
                    .ok_or_else(|| anyhow::anyhow!("Missing price from CoinGecko"))
            }
        })
        .await?;

        // 30-day history
        let chart_url = format!(
            "https://api.coingecko.com/api/v3/coins/{}/market_chart?vs_currency=usd&days=30&interval=daily",
            coin_id
        );
        let http = self.http.clone();
        let key = api_key.clone();
        let chart_url_clone = chart_url.clone();
        let daily_closes: Vec<f64> = with_retry(2, || {
            let http = http.clone();
            let url = chart_url_clone.clone();
            let key = key.clone();
            async move {
                let resp: serde_json::Value = http
                    .get(&url)
                    .header("x-cg-demo-api-key", &key)
                    .send()
                    .await?
                    .json()
                    .await?;
                let prices = resp["prices"]
                    .as_array()
                    .ok_or_else(|| anyhow::anyhow!("Missing prices from CoinGecko"))?;
                let mut closes = Vec::with_capacity(prices.len());
                for point in prices {
                    let price = point[1]
                        .as_f64()
                        .ok_or_else(|| anyhow::anyhow!("Invalid price point"))?;
                    closes.push(price);
                }
                Ok(closes)
            }
        })
        .await?;

        Ok(CryptoPriceData {
            current_price,
            daily_closes,
        })
    }
}

// ──── GBM Model ────

/// Calculate annualized drift (mu) and volatility (sigma) from daily close prices.
/// Returns None if fewer than 2 data points.
pub fn calculate_volatility(daily_closes: &[f64]) -> Option<(f64, f64)> {
    if daily_closes.len() < 2 {
        return None;
    }

    // Calculate daily log-returns
    let mut log_returns = Vec::with_capacity(daily_closes.len() - 1);
    for i in 1..daily_closes.len() {
        if daily_closes[i - 1] <= 0.0 || daily_closes[i] <= 0.0 {
            continue;
        }
        log_returns.push((daily_closes[i] / daily_closes[i - 1]).ln());
    }

    if log_returns.is_empty() {
        return None;
    }

    let n = log_returns.len() as f64;
    let mean = log_returns.iter().sum::<f64>() / n;

    let variance = if log_returns.len() > 1 {
        log_returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / (n - 1.0)
    } else {
        0.0
    };

    let daily_sigma = variance.sqrt();

    // Annualize: mu = mean * 365, sigma = daily_sigma * sqrt(365)
    let mu = mean * 365.0;
    let sigma = daily_sigma * 365.0_f64.sqrt();

    Some((mu, sigma))
}

/// Probability that price exceeds threshold under GBM.
/// P(S_T > K) = Φ(d) where d = (ln(S/K) + (μ - σ²/2) * t) / (σ * √t)
pub fn gbm_probability(current_price: f64, threshold: f64, mu: f64, sigma: f64, days: f64) -> f64 {
    if current_price <= 0.0 || threshold <= 0.0 || sigma <= 0.0 || days <= 0.0 {
        return 0.5;
    }

    let t = days / 365.0;
    let d = ((current_price / threshold).ln() + (mu - sigma * sigma / 2.0) * t)
        / (sigma * t.sqrt());

    normal_cdf(d)
}

// ──── Cached Price ────

struct CachedPrice {
    data: CryptoPriceData,
    fetched_at: chrono::DateTime<Utc>,
}

// ──── Strategy ────

pub struct CryptoAlphaStrategy {
    config: CryptoAlphaConfig,
    price_client: CryptoPriceClient,
    profit_calc: ProfitCalculator,
    get_orderbook: Box<dyn Fn(U256) -> Option<OrderBook> + Send + Sync>,
    get_available_capital: Box<dyn Fn() -> Decimal + Send + Sync>,
    get_position: Box<dyn Fn(U256) -> Decimal + Send + Sync>,
    price_cache: Arc<Mutex<HashMap<String, CachedPrice>>>,
    neg_risk_events: Vec<NegRiskEvent>,
    binary_event_groups: Vec<BinaryEventGroup>,
    /// Scan counter for periodic diagnostics (every ~600 scans ≈ 1 min at 100ms interval).
    scan_count: Arc<std::sync::atomic::AtomicU64>,
    /// Best near-miss edge (bps) seen since last diagnostic — shows how close to threshold.
    near_miss_edge_bps: std::sync::atomic::AtomicU32,
}

impl CryptoAlphaStrategy {
    pub fn new(
        config: CryptoAlphaConfig,
        gas_cost_usd: Decimal,
        get_orderbook: Box<dyn Fn(U256) -> Option<OrderBook> + Send + Sync>,
        get_available_capital: Box<dyn Fn() -> Decimal + Send + Sync>,
        get_position: Box<dyn Fn(U256) -> Decimal + Send + Sync>,
        neg_risk_events: Vec<NegRiskEvent>,
        binary_event_groups: Vec<BinaryEventGroup>,
    ) -> Self {
        let coingecko_key = if config.coingecko_api_key.is_empty() {
            None
        } else {
            Some(config.coingecko_api_key.clone())
        };
        Self {
            config,
            price_client: CryptoPriceClient::new(coingecko_key),
            profit_calc: ProfitCalculator::new(gas_cost_usd),
            get_orderbook,
            get_available_capital,
            get_position,
            price_cache: Arc::new(Mutex::new(HashMap::new())),
            neg_risk_events,
            binary_event_groups,
            scan_count: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            near_miss_edge_bps: std::sync::atomic::AtomicU32::new(0),
        }
    }

    /// Get price data, using cache if fresh enough.
    async fn get_price_data(&self, asset: &CryptoAsset) -> anyhow::Result<CryptoPriceData> {
        let cache_key = asset.binance_symbol.to_string();
        let refresh_secs = self.config.refresh_interval_secs;

        // Check cache
        {
            let cache = self.price_cache.lock().unwrap();
            if let Some(cached) = cache.get(&cache_key) {
                let age = Utc::now()
                    .signed_duration_since(cached.fetched_at)
                    .num_seconds();
                if age < refresh_secs as i64 {
                    return Ok(cached.data.clone());
                }
            }
        }

        // Fetch fresh data
        let data = self.price_client.get_price_data(asset).await?;

        // Update cache
        {
            let mut cache = self.price_cache.lock().unwrap();
            cache.insert(
                cache_key,
                CachedPrice {
                    data: data.clone(),
                    fetched_at: Utc::now(),
                },
            );
        }

        Ok(data)
    }

    /// Detect a crypto alpha opportunity on a single market.
    pub async fn detect_crypto_opportunity(
        &self,
        market: &MarketInfo,
    ) -> Option<ArbitrageOpportunity> {
        let question = parse_crypto_question(&market.question)?;

        // Require a target date
        let target_date = question.target_date?;
        let now_date = Utc::now().date_naive();
        let days = (target_date - now_date).num_days();
        if days <= 0 {
            return None;
        }

        // Fetch price data
        let price_data = match self.get_price_data(question.asset).await {
            Ok(d) => d,
            Err(e) => {
                tracing::debug!(
                    asset = question.asset.name,
                    error = %e,
                    "Failed to fetch crypto price data"
                );
                return None;
            }
        };

        // Calculate volatility
        let (mu, sigma) = calculate_volatility(&price_data.daily_closes)?;

        // GBM probability P(S_T > K)
        let prob_above = gbm_probability(
            price_data.current_price,
            question.threshold,
            mu,
            sigma,
            days as f64,
        );

        let model_prob_f64 = match question.direction {
            PriceDirection::Above => prob_above,
            PriceDirection::Below => 1.0 - prob_above,
        };

        // Need 2 tokens (binary market)
        if market.tokens.len() != 2 {
            return None;
        }

        let yes_token = &market.tokens[0];
        let no_token = &market.tokens[1];

        let yes_book = (self.get_orderbook)(yes_token.token_id)?;
        let no_book = (self.get_orderbook)(no_token.token_id)?;

        let yes_ask = yes_book.best_ask()?.price;
        let no_ask = no_book.best_ask()?.price;

        // Check both YES and NO sides, pick larger edge
        let model_prob = Decimal::from_f64_retain(model_prob_f64)?;
        let model_prob_no = Decimal::ONE - model_prob;

        let yes_edge = model_prob - yes_ask;
        let no_edge = model_prob_no - no_ask;

        let (token_id, ask_price, edge, prob_for_sizing) = if yes_edge > no_edge && yes_edge > Decimal::ZERO
        {
            (yes_token.token_id, yes_ask, yes_edge, model_prob)
        } else if no_edge > Decimal::ZERO {
            (no_token.token_id, no_ask, no_edge, model_prob_no)
        } else {
            return None;
        };

        // Check min edge
        let edge_bps = {
            use rust_decimal::prelude::ToPrimitive;
            (edge * dec!(10000)).to_u32().unwrap_or(0)
        };
        if edge_bps < self.config.min_edge_bps {
            self.near_miss_edge_bps
                .fetch_max(edge_bps, std::sync::atomic::Ordering::Relaxed);
            tracing::debug!(
                question = %market.question,
                asset = question.asset.name,
                current_price = price_data.current_price,
                model_prob = %prob_for_sizing,
                ask = %ask_price,
                edge_bps,
                min_edge_bps = self.config.min_edge_bps,
                "[CryptoAlpha] near-miss: edge below threshold"
            );
            return None;
        }

        // Kelly sizing: f* = edge / (1 - price)
        let kelly_raw = if ask_price > Decimal::ZERO && ask_price < dec!(0.99) {
            (edge / (Decimal::ONE - ask_price)).min(Decimal::TWO)
        } else {
            Decimal::ZERO
        };
        let kelly_size = kelly_raw * self.config.kelly_fraction * self.config.max_position_usdc;
        let available = (self.get_available_capital)();

        // Position-aware sizing
        let existing = (self.get_position)(token_id);
        let remaining = (self.config.max_position_usdc - existing).max(Decimal::ZERO);
        let size = kelly_size.min(remaining).min(available);

        if size <= Decimal::ZERO {
            return None;
        }

        // Profitability check
        let est = self.profit_calc.directional_buy_profit(
            ask_price,
            prob_for_sizing,
            size,
            market.fee_rate_bps,
        );

        if est.net_profit <= Decimal::ZERO {
            return None;
        }

        tracing::info!(
            question = %market.question,
            asset = question.asset.name,
            current_price = price_data.current_price,
            threshold = question.threshold,
            direction = ?question.direction,
            days = days,
            model_prob = %prob_for_sizing,
            ask = %ask_price,
            edge_bps = edge_bps,
            size = %size,
            est_profit = %est.net_profit,
            "Crypto alpha opportunity detected"
        );

        Some(ArbitrageOpportunity {
            id: Uuid::now_v7(),
            strategy_type: StrategyType::CryptoAlpha,
            condition_id: market.condition_id,
            question: market.question.clone(),
            spread: edge,
            estimated_profit: est.net_profit,
            size,
            detected_at: Utc::now(),
            execution_plan: ExecutionPlan::DirectionalBuy {
                token_id,
                side: TradeSide::Buy,
                price: ask_price,
                size,
                condition_id: market.condition_id,
            },
        })
    }

    /// Detect the best undervalued outcome in a NegRisk crypto price event.
    ///
    /// Checks both YES and NO sides for each outcome market, picks the best edge.
    async fn detect_crypto_neg_risk(
        &self,
        event: &NegRiskEvent,
        asset: &'static CryptoAsset,
        days: f64,
    ) -> Option<ArbitrageOpportunity> {
        // Fetch price data
        let price_data = match self.get_price_data(asset).await {
            Ok(d) => d,
            Err(e) => {
                tracing::debug!(
                    asset = asset.name,
                    error = %e,
                    "Failed to fetch crypto price data for NegRisk"
                );
                return None;
            }
        };

        let (mu, sigma) = calculate_volatility(&price_data.daily_closes)?;

        // Evaluate each outcome market
        let mut best_edge = Decimal::ZERO;
        let mut best_candidate: Option<(
            &MarketInfo,
            U256,    // token_id
            Decimal, // ask_price
            Decimal, // effective_prob
            Decimal, // edge
        )> = None;

        for market in &event.markets {
            if !market.active || market.tokens.len() < 2 {
                continue;
            }

            let range = match parse_crypto_outcome_range(&market.question) {
                Some(r) => r,
                None => continue,
            };

            let model_prob_f64 =
                gbm_range_probability(price_data.current_price, &range, mu, sigma, days);
            let model_prob = Decimal::from_f64_retain(model_prob_f64)?;

            let yes_token = &market.tokens[0];
            let no_token = &market.tokens[1];

            // YES side check
            if let Some(yes_book) = (self.get_orderbook)(yes_token.token_id)
                && let Some(yes_ask_level) = yes_book.best_ask()
            {
                let yes_ask = yes_ask_level.price;
                if model_prob > yes_ask {
                    let edge = model_prob - yes_ask;
                    if edge > best_edge {
                        best_edge = edge;
                        best_candidate =
                            Some((market, yes_token.token_id, yes_ask, model_prob, edge));
                    }
                }
            }

            // NO side check
            let no_model_prob = Decimal::ONE - model_prob;
            if let Some(no_book) = (self.get_orderbook)(no_token.token_id)
                && let Some(no_ask_level) = no_book.best_ask()
            {
                let no_ask = no_ask_level.price;
                if no_model_prob > no_ask {
                    let edge = no_model_prob - no_ask;
                    if edge > best_edge {
                        best_edge = edge;
                        best_candidate =
                            Some((market, no_token.token_id, no_ask, no_model_prob, edge));
                    }
                }
            }
        }

        let (market, token_id, ask_price, effective_prob, edge) = best_candidate?;

        // Check minimum edge threshold
        let edge_bps = {
            use rust_decimal::prelude::ToPrimitive;
            (edge * dec!(10000)).to_u32().unwrap_or(0)
        };
        if edge_bps < self.config.min_edge_bps {
            self.near_miss_edge_bps
                .fetch_max(edge_bps, std::sync::atomic::Ordering::Relaxed);
            tracing::debug!(
                event_title = %event.title,
                asset = asset.name,
                model_prob = %effective_prob,
                ask = %ask_price,
                edge_bps,
                min_edge_bps = self.config.min_edge_bps,
                "[CryptoAlpha] NegRisk near-miss: edge below threshold"
            );
            return None;
        }

        // Kelly sizing
        let kelly_raw = if ask_price > Decimal::ZERO && ask_price < dec!(0.99) {
            (edge / (Decimal::ONE - ask_price)).min(Decimal::TWO)
        } else {
            Decimal::ZERO
        };
        let kelly_size = kelly_raw * self.config.kelly_fraction * self.config.max_position_usdc;
        let available = (self.get_available_capital)();

        // Position-aware sizing
        let existing = (self.get_position)(token_id);
        let remaining = (self.config.max_position_usdc - existing).max(Decimal::ZERO);
        let size = kelly_size.min(remaining).min(available);

        if size <= Decimal::ZERO {
            return None;
        }

        // Profitability check
        let est = self.profit_calc.directional_buy_profit(
            ask_price,
            effective_prob,
            size,
            event.fee_rate_bps,
        );

        if est.net_profit <= Decimal::ZERO {
            return None;
        }

        tracing::info!(
            event_title = %event.title,
            outcome = %market.question,
            asset = asset.name,
            current_price = price_data.current_price,
            model_prob = %effective_prob,
            ask = %ask_price,
            edge_bps = edge_bps,
            size = %size,
            est_profit = %est.net_profit,
            "NegRisk crypto alpha opportunity detected"
        );

        Some(ArbitrageOpportunity {
            id: Uuid::now_v7(),
            strategy_type: StrategyType::CryptoAlpha,
            condition_id: market.condition_id,
            question: format!("{} → {}", event.title, market.question),
            spread: edge,
            estimated_profit: est.net_profit,
            size,
            detected_at: Utc::now(),
            execution_plan: ExecutionPlan::DirectionalBuy {
                token_id,
                side: TradeSide::Buy,
                price: ask_price,
                size,
                condition_id: market.condition_id,
            },
        })
    }

    /// Detect the best crypto alpha opportunity within a binary event group.
    ///
    /// A binary event group is a set of independent binary markets sharing
    /// the same event title (e.g. "What price will Bitcoin hit in 2026?").
    /// Fetches price data once for the shared asset, then evaluates all
    /// markets to find the best edge.
    async fn detect_crypto_group(
        &self,
        group: &BinaryEventGroup,
    ) -> Option<ArbitrageOpportunity> {
        // Try to identify crypto asset from the group title first, then from individual markets
        let asset = find_asset(&group.title).or_else(|| {
            group.markets.iter().find_map(|m| {
                parse_crypto_question(&m.question).map(|q| q.asset)
            })
        })?;

        // Fetch price data once for the entire group
        let price_data = match self.get_price_data(asset).await {
            Ok(d) => d,
            Err(e) => {
                tracing::debug!(
                    asset = asset.name,
                    group_title = %group.title,
                    error = %e,
                    "Failed to fetch crypto price data for binary group"
                );
                return None;
            }
        };

        let (mu, sigma) = calculate_volatility(&price_data.daily_closes)?;

        // Evaluate each market in the group, track the best edge
        let mut best_edge = Decimal::ZERO;
        let mut best_candidate: Option<(
            &MarketInfo,
            U256,    // token_id
            Decimal, // ask_price
            Decimal, // effective_prob (for sizing)
            Decimal, // edge
            u32,     // edge_bps
        )> = None;

        for market in &group.markets {
            if !market.active || market.tokens.len() != 2 {
                continue;
            }

            let question = match parse_crypto_question(&market.question) {
                Some(q) => q,
                None => continue,
            };

            // Require a target date
            let target_date = match question.target_date {
                Some(d) => d,
                None => continue,
            };
            let now_date = Utc::now().date_naive();
            let days = (target_date - now_date).num_days();
            if days <= 0 {
                continue;
            }

            // GBM probability
            let prob_above = gbm_probability(
                price_data.current_price,
                question.threshold,
                mu,
                sigma,
                days as f64,
            );
            let model_prob_f64 = match question.direction {
                PriceDirection::Above => prob_above,
                PriceDirection::Below => 1.0 - prob_above,
            };
            let model_prob = Decimal::from_f64_retain(model_prob_f64)?;
            let model_prob_no = Decimal::ONE - model_prob;

            let yes_token = &market.tokens[0];
            let no_token = &market.tokens[1];

            // Check YES side
            if let Some(yes_book) = (self.get_orderbook)(yes_token.token_id)
                && let Some(yes_ask_level) = yes_book.best_ask()
            {
                let yes_ask = yes_ask_level.price;
                if model_prob > yes_ask {
                    let edge = model_prob - yes_ask;
                    if edge > best_edge {
                        let edge_bps = {
                            use rust_decimal::prelude::ToPrimitive;
                            (edge * dec!(10000)).to_u32().unwrap_or(0)
                        };
                        best_edge = edge;
                        best_candidate = Some((
                            market,
                            yes_token.token_id,
                            yes_ask,
                            model_prob,
                            edge,
                            edge_bps,
                        ));
                    }
                }
            }

            // Check NO side
            if let Some(no_book) = (self.get_orderbook)(no_token.token_id)
                && let Some(no_ask_level) = no_book.best_ask()
            {
                let no_ask = no_ask_level.price;
                if model_prob_no > no_ask {
                    let edge = model_prob_no - no_ask;
                    if edge > best_edge {
                        let edge_bps = {
                            use rust_decimal::prelude::ToPrimitive;
                            (edge * dec!(10000)).to_u32().unwrap_or(0)
                        };
                        best_edge = edge;
                        best_candidate = Some((
                            market,
                            no_token.token_id,
                            no_ask,
                            model_prob_no,
                            edge,
                            edge_bps,
                        ));
                    }
                }
            }
        }

        let (market, token_id, ask_price, effective_prob, edge, edge_bps) = best_candidate?;

        // Check minimum edge threshold
        if edge_bps < self.config.min_edge_bps {
            self.near_miss_edge_bps
                .fetch_max(edge_bps, std::sync::atomic::Ordering::Relaxed);
            tracing::debug!(
                group_title = %group.title,
                question = %market.question,
                model_prob = %effective_prob,
                ask = %ask_price,
                edge_bps,
                min_edge_bps = self.config.min_edge_bps,
                "[CryptoAlpha] group near-miss: edge below threshold"
            );
            return None;
        }

        // Kelly sizing
        let kelly_raw = if ask_price > Decimal::ZERO && ask_price < dec!(0.99) {
            (edge / (Decimal::ONE - ask_price)).min(Decimal::TWO)
        } else {
            Decimal::ZERO
        };
        let kelly_size = kelly_raw * self.config.kelly_fraction * self.config.max_position_usdc;
        let available = (self.get_available_capital)();

        // Position-aware sizing
        let existing = (self.get_position)(token_id);
        let remaining = (self.config.max_position_usdc - existing).max(Decimal::ZERO);
        let size = kelly_size.min(remaining).min(available);

        if size <= Decimal::ZERO {
            return None;
        }

        // Profitability check
        let est = self.profit_calc.directional_buy_profit(
            ask_price,
            effective_prob,
            size,
            market.fee_rate_bps,
        );

        if est.net_profit <= Decimal::ZERO {
            return None;
        }

        tracing::info!(
            group_title = %group.title,
            question = %market.question,
            asset = asset.name,
            current_price = price_data.current_price,
            model_prob = %effective_prob,
            ask = %ask_price,
            edge_bps = edge_bps,
            group_size = group.markets.len(),
            size = %size,
            est_profit = %est.net_profit,
            "Binary group crypto alpha opportunity detected"
        );

        Some(ArbitrageOpportunity {
            id: Uuid::now_v7(),
            strategy_type: StrategyType::CryptoAlpha,
            condition_id: market.condition_id,
            question: format!("[Group] {} → {}", group.title, market.question),
            spread: edge,
            estimated_profit: est.net_profit,
            size,
            detected_at: Utc::now(),
            execution_plan: ExecutionPlan::DirectionalBuy {
                token_id,
                side: TradeSide::Buy,
                price: ask_price,
                size,
                condition_id: market.condition_id,
            },
        })
    }
}

#[async_trait]
impl Strategy for CryptoAlphaStrategy {
    fn name(&self) -> &str {
        "CryptoAlpha"
    }

    fn strategy_type(&self) -> StrategyType {
        StrategyType::CryptoAlpha
    }

    async fn scan(
        &self,
        markets: &[MarketInfo],
    ) -> pa_core::Result<Vec<ArbitrageOpportunity>> {
        let mut opportunities = Vec::new();
        let count = self.scan_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let log_diag = count.is_multiple_of(600); // ~every 60s at 100ms interval

        let mut binary_crypto = 0u32;
        let mut binary_group_crypto = 0u32;
        let mut neg_risk_matched = 0u32;
        let mut neg_risk_expired = 0u32;
        let mut neg_risk_no_date = 0u32;

        // Build a set of condition_ids that belong to binary event groups,
        // so we skip them in the individual market loop (avoid double-processing).
        let grouped_condition_ids: std::collections::HashSet<alloy::primitives::B256> = self
            .binary_event_groups
            .iter()
            .flat_map(|g| g.markets.iter().map(|m| m.condition_id))
            .collect();

        // 1. Binary event groups — process each group as a unit
        for group in &self.binary_event_groups {
            // Quick check: does this group contain any crypto market?
            let has_crypto = find_asset(&group.title).is_some()
                || group.markets.iter().any(|m| parse_crypto_question(&m.question).is_some());
            if !has_crypto {
                continue;
            }
            binary_group_crypto += 1;
            if let Some(opp) = self.detect_crypto_group(group).await {
                opportunities.push(opp);
            }
        }

        // 2. Individual binary markets (skip those already in groups)
        let mut binary_crypto_samples: Vec<String> = Vec::new();
        for market in markets {
            if !market.active || market.neg_risk {
                continue;
            }

            // Skip markets that are part of a binary event group
            if grouped_condition_ids.contains(&market.condition_id) {
                continue;
            }

            if parse_crypto_question(&market.question).is_some() {
                binary_crypto += 1;
                if binary_crypto_samples.len() < 3 {
                    binary_crypto_samples.push(market.question.chars().take(60).collect());
                }
            }

            if let Some(opp) = self.detect_crypto_opportunity(market).await {
                opportunities.push(opp);
            }
        }

        // 3. NegRisk events
        for event in &self.neg_risk_events {
            if let Some((asset, target_date)) = parse_crypto_event_title(&event.title) {
                let now_date = Utc::now().date_naive();
                let days = target_date
                    .map(|d| (d - now_date).num_days())
                    .unwrap_or(0);
                if days <= 0 {
                    if target_date.is_some() {
                        neg_risk_expired += 1;
                    } else {
                        neg_risk_no_date += 1;
                    }
                    continue;
                }
                neg_risk_matched += 1;
                if let Some(opp) = self.detect_crypto_neg_risk(event, asset, days as f64).await {
                    opportunities.push(opp);
                }
            }
        }

        if log_diag {
            let best_near_miss = self
                .near_miss_edge_bps
                .swap(0, std::sync::atomic::Ordering::Relaxed);
            tracing::info!(
                binary_groups = self.binary_event_groups.len(),
                binary_group_crypto,
                binary_ungrouped = binary_crypto,
                neg_risk_events = self.neg_risk_events.len(),
                neg_risk_matched,
                neg_risk_expired,
                neg_risk_no_date,
                opportunities = opportunities.len(),
                best_near_miss_bps = best_near_miss,
                min_edge_bps = self.config.min_edge_bps,
                "[CryptoAlpha] scan diagnostics"
            );
            // Log sample binary event group titles
            for (i, group) in self.binary_event_groups.iter().enumerate() {
                if i >= 5 { break; }
                let has_crypto = find_asset(&group.title).is_some();
                tracing::info!(
                    idx = i,
                    title = %group.title,
                    markets = group.markets.len(),
                    is_crypto = has_crypto,
                    "[CryptoAlpha] binary event group"
                );
            }
            // Log sample ungrouped binary crypto questions
            for (i, q) in binary_crypto_samples.iter().enumerate() {
                tracing::info!(idx = i, question = %q, "[CryptoAlpha] ungrouped binary crypto");
            }
        }

        Ok(opportunities)
    }
}

// ──── Tests ────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_bitcoin_above() {
        let q = parse_crypto_question("Will Bitcoin exceed $100,000 by end of March?").unwrap();
        assert_eq!(q.asset.name, "Bitcoin");
        assert!((q.threshold - 100_000.0).abs() < 0.01);
        assert_eq!(q.direction, PriceDirection::Above);
    }

    #[test]
    fn test_parse_ethereum_below() {
        let q = parse_crypto_question("Will ETH fall below $2000 on Feb 20?").unwrap();
        assert_eq!(q.asset.name, "Ethereum");
        assert!((q.threshold - 2000.0).abs() < 0.01);
        assert_eq!(q.direction, PriceDirection::Below);
    }

    #[test]
    fn test_parse_with_comma_price() {
        let price = extract_price("Will BTC reach $100,000?").unwrap();
        assert!((price - 100_000.0).abs() < 0.01);
    }

    #[test]
    fn test_parse_with_k_suffix() {
        let price = extract_price("Will Bitcoin hit $100k?").unwrap();
        assert!((price - 100_000.0).abs() < 0.01);
    }

    #[test]
    fn test_parse_non_crypto() {
        // No crypto asset mentioned
        let result = parse_crypto_question("Will inflation exceed 5% this year?");
        assert!(result.is_none());
    }

    #[test]
    fn test_unknown_crypto() {
        // "FooToken" is not in our asset list
        let result = parse_crypto_question("Will FooToken reach $10?");
        assert!(result.is_none());
    }

    #[test]
    fn test_volatility_calculation() {
        // Simulated 30 daily closes with upward trend
        let closes: Vec<f64> = (0..30)
            .map(|i| 100.0 * (1.0 + 0.01 * i as f64))
            .collect();

        let (mu, sigma) = calculate_volatility(&closes).unwrap();
        // Positive drift (prices going up)
        assert!(mu > 0.0, "mu should be positive, got {}", mu);
        // Non-zero volatility
        assert!(sigma > 0.0, "sigma should be positive, got {}", sigma);
    }

    #[test]
    fn test_gbm_probability_atm() {
        // At-the-money: S = K, mu = 0
        let prob = gbm_probability(100.0, 100.0, 0.0, 0.5, 30.0);
        // With mu=0, sigma>0, P(S_T > K) is slightly below 0.5 due to the -σ²/2 drift
        assert!(
            (prob - 0.5).abs() < 0.05,
            "ATM with mu=0 should be near 0.5, got {}",
            prob
        );
    }

    #[test]
    fn test_gbm_probability_deep_itm() {
        // Deep in-the-money: S >> K
        let prob = gbm_probability(200.0, 50.0, 0.0, 0.3, 30.0);
        assert!(
            prob > 0.95,
            "Deep ITM should have prob > 0.95, got {}",
            prob
        );
    }

    #[test]
    fn test_gbm_probability_deep_otm() {
        // Deep out-of-the-money: S << K
        let prob = gbm_probability(50.0, 200.0, 0.0, 0.3, 30.0);
        assert!(
            prob < 0.05,
            "Deep OTM should have prob < 0.05, got {}",
            prob
        );
    }

    #[test]
    fn test_gbm_momentum_bullish() {
        // Bullish momentum (positive μ) should increase probability vs μ=0
        let prob_neutral = gbm_probability(100.0, 110.0, 0.0, 0.5, 30.0);
        let prob_bullish = gbm_probability(100.0, 110.0, 1.0, 0.5, 30.0);
        assert!(
            prob_bullish > prob_neutral,
            "Bullish mu should increase prob: bullish={} vs neutral={}",
            prob_bullish,
            prob_neutral
        );
    }

    #[test]
    fn test_asset_mapping_completeness() {
        assert_eq!(CRYPTO_ASSETS.len(), 10, "Should have 10 crypto assets");
        for asset in CRYPTO_ASSETS {
            assert!(!asset.name.is_empty(), "Name should not be empty");
            assert!(
                !asset.keywords.is_empty(),
                "Keywords should not be empty for {}",
                asset.name
            );
            assert!(
                !asset.binance_symbol.is_empty(),
                "Binance symbol should not be empty for {}",
                asset.name
            );
            assert!(
                !asset.coingecko_id.is_empty(),
                "CoinGecko ID should not be empty for {}",
                asset.name
            );
        }
    }

    // ──── NegRisk Event Title Parser Tests ────

    #[test]
    fn test_parse_crypto_event_title_bitcoin() {
        use chrono::Datelike;
        let (asset, date) =
            parse_crypto_event_title("Bitcoin price at 12pm ET on March 1").unwrap();
        assert_eq!(asset.name, "Bitcoin");
        let d = date.unwrap();
        assert_eq!(d.month(), 3);
        assert_eq!(d.day(), 1);
    }

    #[test]
    fn test_parse_crypto_event_title_eth() {
        let (asset, _date) =
            parse_crypto_event_title("Ethereum price on February 28?").unwrap();
        assert_eq!(asset.name, "Ethereum");
    }

    #[test]
    fn test_parse_crypto_event_title_non_crypto() {
        assert!(parse_crypto_event_title("US GDP Q1 2026").is_none());
    }

    // ──── NegRisk Outcome Range Parser Tests ────

    #[test]
    fn test_parse_crypto_outcome_range_above() {
        let range = parse_crypto_outcome_range("$100,000 or above").unwrap();
        match range {
            CryptoPriceRange::AtOrAbove(p) => {
                assert!((p - 100_000.0).abs() < 0.01, "got {}", p);
            }
            other => panic!("Expected AtOrAbove, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_crypto_outcome_range_below() {
        let range = parse_crypto_outcome_range("$89,999 or below").unwrap();
        match range {
            CryptoPriceRange::AtOrBelow(p) => {
                assert!((p - 89_999.0).abs() < 0.01, "got {}", p);
            }
            other => panic!("Expected AtOrBelow, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_crypto_outcome_range_interval() {
        let range = parse_crypto_outcome_range("$90,000 - $94,999").unwrap();
        match range {
            CryptoPriceRange::Range(lo, hi) => {
                assert!((lo - 90_000.0).abs() < 0.01, "lo={}", lo);
                assert!((hi - 94_999.0).abs() < 0.01, "hi={}", hi);
            }
            other => panic!("Expected Range, got {:?}", other),
        }
    }

    // ──── GBM Range Probability Tests ────

    #[test]
    fn test_gbm_range_probability_monotonicity() {
        // Higher range should have lower probability when price is at 95k
        let p1 = gbm_range_probability(
            95_000.0,
            &CryptoPriceRange::Range(90_000.0, 95_000.0),
            0.0,
            0.5,
            30.0,
        );
        let p2 = gbm_range_probability(
            95_000.0,
            &CryptoPriceRange::Range(95_000.0, 100_000.0),
            0.0,
            0.5,
            30.0,
        );
        let p3 = gbm_range_probability(
            95_000.0,
            &CryptoPriceRange::Range(150_000.0, 200_000.0),
            0.0,
            0.5,
            30.0,
        );
        // Range containing current price should have highest probability
        assert!(
            p1 > p3,
            "Near-price range should have higher prob than far range: {} > {}",
            p1,
            p3
        );
        assert!(
            p2 > p3,
            "Adjacent range should have higher prob than far range: {} > {}",
            p2,
            p3
        );
    }

    #[test]
    fn test_gbm_range_probability_partition() {
        // AtOrBelow + ranges + AtOrAbove should sum to ~1.0
        let price = 95_000.0;
        let mu = 0.0;
        let sigma = 0.5;
        let days = 30.0;

        let p_below = gbm_range_probability(
            price,
            &CryptoPriceRange::AtOrBelow(85_000.0),
            mu,
            sigma,
            days,
        );
        let p1 = gbm_range_probability(
            price,
            &CryptoPriceRange::Range(85_000.0, 90_000.0),
            mu,
            sigma,
            days,
        );
        let p2 = gbm_range_probability(
            price,
            &CryptoPriceRange::Range(90_000.0, 95_000.0),
            mu,
            sigma,
            days,
        );
        let p3 = gbm_range_probability(
            price,
            &CryptoPriceRange::Range(95_000.0, 100_000.0),
            mu,
            sigma,
            days,
        );
        let p4 = gbm_range_probability(
            price,
            &CryptoPriceRange::Range(100_000.0, 105_000.0),
            mu,
            sigma,
            days,
        );
        let p_above = gbm_range_probability(
            price,
            &CryptoPriceRange::AtOrAbove(105_000.0),
            mu,
            sigma,
            days,
        );

        let total = p_below + p1 + p2 + p3 + p4 + p_above;
        assert!(
            (total - 1.0).abs() < 0.01,
            "Partition should sum to ~1.0, got {}",
            total
        );
    }

    // ──── Binary Event Group Tests ────

    #[test]
    fn test_find_asset_from_event_title() {
        // Group title contains crypto asset
        assert!(find_asset("What price will Bitcoin hit in 2026?").is_some());
        assert_eq!(
            find_asset("What price will Bitcoin hit in 2026?").unwrap().name,
            "Bitcoin"
        );
        assert!(find_asset("Ethereum price predictions 2026").is_some());
        // Non-crypto event title
        assert!(find_asset("Who will win the Super Bowl?").is_none());
    }

    #[test]
    fn test_find_asset_from_reach_question() {
        // Real Polymarket format: "Will Bitcoin reach $200,000 by December 31, 2026?"
        let q = parse_crypto_question("Will Bitcoin reach $200,000 by December 31, 2026?");
        assert!(q.is_some());
        let q = q.unwrap();
        assert_eq!(q.asset.name, "Bitcoin");
        assert!((q.threshold - 200_000.0).abs() < 0.01);
        assert_eq!(q.direction, PriceDirection::Above);
    }

    #[test]
    fn test_find_asset_from_dip_question() {
        // "Will Bitcoin dip to $85,000 by December 31, 2026?"
        let q = parse_crypto_question("Will Bitcoin dip to $85,000 by December 31, 2026?");
        assert!(q.is_some());
        let q = q.unwrap();
        assert_eq!(q.asset.name, "Bitcoin");
        assert!((q.threshold - 85_000.0).abs() < 0.01);
        // "dip to" is not explicitly "fall below" — direction depends on keyword matching
    }

    #[test]
    fn test_binary_event_group_type() {
        use pa_core::types::BinaryEventGroup;
        use alloy::primitives::B256;

        let market1 = MarketInfo {
            condition_id: B256::ZERO,
            question_id: B256::ZERO,
            question: "Will Bitcoin reach $200,000?".into(),
            neg_risk: false,
            neg_risk_market_id: None,
            tokens: vec![],
            tick_size: dec!(0.01),
            fee_rate_bps: 200,
            active: true,
            liquidity: dec!(1000),
            event_title: Some("What price will Bitcoin hit in 2026?".into()),
            end_date: None,
            category: Some("crypto".into()),
            outcome_prices: None,
        };
        let market2 = MarketInfo {
            question: "Will Bitcoin reach $150,000?".into(),
            ..market1.clone()
        };

        let group = BinaryEventGroup {
            title: "What price will Bitcoin hit in 2026?".into(),
            markets: vec![market1, market2],
        };

        assert_eq!(group.markets.len(), 2);
        assert!(find_asset(&group.title).is_some());
        assert_eq!(find_asset(&group.title).unwrap().name, "Bitcoin");
    }
}
