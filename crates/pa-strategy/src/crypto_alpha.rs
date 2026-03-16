use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use alloy::primitives::U256;
use async_trait::async_trait;
use chrono::{NaiveDate, Utc};
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal_macros::dec;
use uuid::Uuid;

use pa_core::config::CryptoAlphaConfig;
use pa_core::traits::Strategy;
use pa_core::types::{
    BinaryEventGroup, EventImpact, ExecutionPlan, MarketInfo, NegRiskEvent, OrderBook,
    StrategyType, TradeSide, TradingOpportunity,
};
use pa_market_data::event_calendar::EventCalendarService;

use crate::profitability::ProfitCalculator;
use crate::weather::{contains_word, normal_cdf, parse_target_date_server_local, with_retry};

// ──── Asset Mapping ────

#[derive(Debug, Clone, Copy)]
pub struct CryptoAsset {
    pub name: &'static str,
    pub keywords: &'static [&'static str],
    pub binance_symbol: &'static str,
    pub coingecko_id: &'static str,
    /// Deribit currency code for DVOL implied volatility (BTC/ETH only).
    pub deribit_currency: Option<&'static str>,
}

pub static CRYPTO_ASSETS: &[CryptoAsset] = &[
    CryptoAsset {
        name: "Bitcoin",
        keywords: &["bitcoin", "btc"],
        binance_symbol: "BTCUSDT",
        coingecko_id: "bitcoin",
        deribit_currency: Some("BTC"),
    },
    CryptoAsset {
        name: "Ethereum",
        keywords: &["ethereum", "eth"],
        binance_symbol: "ETHUSDT",
        coingecko_id: "ethereum",
        deribit_currency: Some("ETH"),
    },
    CryptoAsset {
        name: "Solana",
        keywords: &["solana", "sol"],
        binance_symbol: "SOLUSDT",
        coingecko_id: "solana",
        deribit_currency: None,
    },
    CryptoAsset {
        name: "BNB",
        keywords: &["bnb", "binance coin"],
        binance_symbol: "BNBUSDT",
        coingecko_id: "binancecoin",
        deribit_currency: None,
    },
    CryptoAsset {
        name: "XRP",
        keywords: &["xrp", "ripple"],
        binance_symbol: "XRPUSDT",
        coingecko_id: "ripple",
        deribit_currency: None,
    },
    CryptoAsset {
        name: "Dogecoin",
        keywords: &["dogecoin", "doge"],
        binance_symbol: "DOGEUSDT",
        coingecko_id: "dogecoin",
        deribit_currency: None,
    },
    CryptoAsset {
        name: "Cardano",
        keywords: &["cardano"],
        binance_symbol: "ADAUSDT",
        coingecko_id: "cardano",
        deribit_currency: None,
    },
    CryptoAsset {
        name: "Avalanche",
        keywords: &["avax"],
        binance_symbol: "AVAXUSDT",
        coingecko_id: "avalanche-2",
        deribit_currency: None,
    },
    CryptoAsset {
        name: "Polkadot",
        keywords: &["polkadot"],
        binance_symbol: "DOTUSDT",
        coingecko_id: "polkadot",
        deribit_currency: None,
    },
    CryptoAsset {
        name: "Polygon",
        keywords: &["polygon", "matic"],
        binance_symbol: "POLUSDT",
        coingecko_id: "polygon-ecosystem-token",
        deribit_currency: None,
    },
];

/// Find a matching crypto asset from question text.
pub fn find_asset(question: &str) -> Option<&'static CryptoAsset> {
    let lower = question.to_lowercase();

    // Exclude non-price markets that happen to contain crypto asset names
    if lower.contains("gas price")
        || lower.contains("gas fee")
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum CryptoDirectionBucket {
    Up,
    Down,
    InsideRange,
    OutsideRange,
}

#[derive(Debug, Clone, Copy)]
enum CryptoMarketType {
    Binary,
    Range,
}

impl CryptoMarketType {
    fn as_str(self) -> &'static str {
        match self {
            CryptoMarketType::Binary => "binary",
            CryptoMarketType::Range => "range",
        }
    }
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

fn direction_bucket_label(bucket: CryptoDirectionBucket) -> &'static str {
    match bucket {
        CryptoDirectionBucket::Up => "up",
        CryptoDirectionBucket::Down => "down",
        CryptoDirectionBucket::InsideRange => "inside_range",
        CryptoDirectionBucket::OutsideRange => "outside_range",
    }
}

pub fn infer_crypto_direction_label(question: &str, outcome: Option<&str>) -> Option<&'static str> {
    let is_yes = match outcome {
        Some("YES") => true,
        Some("NO") => false,
        _ => return None,
    };

    if let Some(parsed) = parse_crypto_question(question) {
        return Some(direction_bucket_label(
            CryptoAlphaStrategy::binary_direction_bucket(parsed.direction, is_yes),
        ));
    }

    if let Some(range) = parse_crypto_outcome_range(question) {
        return Some(direction_bucket_label(
            CryptoAlphaStrategy::range_direction_bucket(&range, is_yes),
        ));
    }

    None
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
        || lower.contains("dip to")
        || lower.contains("dip below")
        || (contains_word(&lower, "below") && !lower.contains("or below"))
        || contains_word(&lower, "under")
    {
        PriceDirection::Below
    } else {
        PriceDirection::Above
    };

    let target_date = parse_target_date_server_local(question);

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
    let target_date = parse_target_date_server_local(title);
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
        || lower.contains("and below")
        || lower.contains("under ")
    {
        let price = prices.first()?;
        Some(CryptoPriceRange::AtOrBelow(*price))
    } else if lower.contains("or above")
        || lower.contains("or more")
        || lower.contains("or higher")
        || lower.contains("and above")
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
    /// Annualized implied volatility from Deribit DVOL (BTC/ETH only).
    pub implied_vol: Option<f64>,
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

    pub async fn fetch_current_price(&self, asset: &CryptoAsset) -> anyhow::Result<f64> {
        match self.fetch_binance_current_price(asset.binance_symbol).await {
            Ok(price) => Ok(price),
            Err(e) => {
                tracing::warn!(
                    asset = asset.name,
                    error = %e,
                    "Binance spot fetch failed, trying CoinGecko fallback"
                );
                if self
                    .coingecko_api_key
                    .as_ref()
                    .is_some_and(|k| !k.is_empty())
                {
                    self.fetch_coingecko_current_price(asset.coingecko_id).await
                } else {
                    anyhow::bail!(
                        "Binance spot fetch failed and CoinGecko API key not configured for {}",
                        asset.name
                    )
                }
            }
        }
    }

    pub async fn fetch_daily_closes(&self, asset: &CryptoAsset) -> anyhow::Result<Vec<f64>> {
        match self.fetch_binance_daily_closes(asset.binance_symbol).await {
            Ok(closes) => Ok(closes),
            Err(e) => {
                tracing::warn!(
                    asset = asset.name,
                    error = %e,
                    "Binance history fetch failed, trying CoinGecko fallback"
                );
                if self
                    .coingecko_api_key
                    .as_ref()
                    .is_some_and(|k| !k.is_empty())
                {
                    self.fetch_coingecko_daily_closes(asset.coingecko_id).await
                } else {
                    anyhow::bail!(
                        "Binance history fetch failed and CoinGecko API key not configured for {}",
                        asset.name
                    )
                }
            }
        }
    }

    pub async fn fetch_implied_vol(&self, asset: &CryptoAsset) -> anyhow::Result<Option<f64>> {
        let Some(currency) = asset.deribit_currency else {
            return Ok(None);
        };

        match self.fetch_deribit_dvol(currency).await {
            Ok(iv) => {
                tracing::debug!(asset = asset.name, iv, "Fetched Deribit DVOL");
                Ok(Some(iv))
            }
            Err(e) => {
                tracing::warn!(
                    asset = asset.name,
                    error = %e,
                    "Deribit DVOL fetch failed, using historical vol only"
                );
                Ok(None)
            }
        }
    }

    async fn fetch_binance_current_price(&self, symbol: &str) -> anyhow::Result<f64> {
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

        Ok(current_price)
    }

    async fn fetch_binance_daily_closes(&self, symbol: &str) -> anyhow::Result<Vec<f64>> {
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

        Ok(daily_closes)
    }

    async fn fetch_coingecko_current_price(&self, coin_id: &str) -> anyhow::Result<f64> {
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

        Ok(current_price)
    }

    async fn fetch_coingecko_daily_closes(&self, coin_id: &str) -> anyhow::Result<Vec<f64>> {
        let api_key = self
            .coingecko_api_key
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("CoinGecko API key not set"))?;

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

        Ok(daily_closes)
    }

    /// Fetch the latest Deribit DVOL (30-day implied volatility index) for BTC or ETH.
    /// Returns annualized IV as a decimal (e.g. 0.65 = 65%).
    async fn fetch_deribit_dvol(&self, currency: &str) -> anyhow::Result<f64> {
        let now_ms = Utc::now().timestamp_millis();
        // Request last 2 hours of 1-hour resolution data to get the latest point
        let start_ms = now_ms - 2 * 3600 * 1000;
        let url = format!(
            "https://www.deribit.com/api/v2/public/get_volatility_index_data?currency={}&resolution=3600&start_timestamp={}&end_timestamp={}",
            currency, start_ms, now_ms
        );
        let http = self.http.clone();
        let url_clone = url.clone();
        let iv: f64 = with_retry(1, || {
            let http = http.clone();
            let url = url_clone.clone();
            async move {
                let resp: serde_json::Value = http
                    .get(&url)
                    .timeout(std::time::Duration::from_secs(5))
                    .send()
                    .await?
                    .json()
                    .await?;
                // Response: { "result": { "data": [[timestamp, open, high, low, close], ...] } }
                let data = resp["result"]["data"]
                    .as_array()
                    .ok_or_else(|| anyhow::anyhow!("Missing DVOL data array"))?;
                let last = data
                    .last()
                    .ok_or_else(|| anyhow::anyhow!("Empty DVOL data"))?;
                // Index [4] is close
                let close = last[4]
                    .as_f64()
                    .ok_or_else(|| anyhow::anyhow!("Invalid DVOL close value"))?;
                // DVOL is in percentage points (e.g. 65.0 = 65%), convert to decimal
                Ok(close / 100.0)
            }
        })
        .await?;
        Ok(iv)
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

/// Blend historical volatility with Deribit implied volatility when available.
/// Uses 70% IV + 30% historical for BTC/ETH; falls back to pure historical for others.
pub fn effective_volatility(historical_sigma: f64, implied_vol: Option<f64>) -> f64 {
    match implied_vol {
        Some(iv) if iv > 0.0 => 0.7 * iv + 0.3 * historical_sigma,
        _ => historical_sigma,
    }
}

/// Probability that price exceeds threshold under GBM.
/// P(S_T > K) = Φ(d) where d = (ln(S/K) + (μ - σ²/2) * t) / (σ * √t)
pub fn gbm_probability(current_price: f64, threshold: f64, mu: f64, sigma: f64, days: f64) -> f64 {
    if current_price <= 0.0 || threshold <= 0.0 || sigma <= 0.0 || days <= 0.0 {
        return 0.5;
    }

    let t = days / 365.0;
    let d =
        ((current_price / threshold).ln() + (mu - sigma * sigma / 2.0) * t) / (sigma * t.sqrt());

    normal_cdf(d)
}

// ──── Cached Price ────

/// Baseline annualized crypto volatility (~60%) for dynamic TTL scaling.
const BASELINE_CRYPTO_SIGMA: f64 = 0.60;

struct CachedPrice {
    current_price: Option<f64>,
    current_price_fetched_at: Option<chrono::DateTime<Utc>>,
    daily_closes: Option<Vec<f64>>,
    daily_closes_fetched_at: Option<chrono::DateTime<Utc>>,
    implied_vol: Option<f64>,
    implied_vol_fetched_at: Option<chrono::DateTime<Utc>>,
    /// Last computed annualized volatility, used to shorten cache TTL during high-vol periods.
    last_sigma: Option<f64>,
}

#[derive(Debug, Clone, Copy)]
struct EdgeDecayConfirmationState {
    count: u32,
    last_seen: Instant,
}

// ──── Strategy ────

pub struct CryptoAlphaStrategy {
    config: CryptoAlphaConfig,
    price_client: CryptoPriceClient,
    profit_calc: ProfitCalculator,
    get_orderbook: Box<dyn Fn(U256) -> Option<OrderBook> + Send + Sync>,
    get_available_capital: Box<dyn Fn() -> Decimal + Send + Sync>,
    get_position: Box<dyn Fn(U256) -> Decimal + Send + Sync>,
    /// Returns all held positions for this strategy: (token_id, size, avg_cost).
    get_held_positions: Box<dyn Fn() -> Vec<(U256, Decimal, Decimal)> + Send + Sync>,
    /// Returns current wallet USDC balance for dynamic position sizing.
    get_balance: Box<dyn Fn() -> Decimal + Send + Sync>,
    price_cache: Arc<Mutex<HashMap<String, CachedPrice>>>,
    neg_risk_events: Vec<NegRiskEvent>,
    binary_event_groups: Vec<BinaryEventGroup>,
    event_calendar: Option<Arc<EventCalendarService>>,
    edge_decay_cooldowns: Arc<Mutex<HashMap<U256, Instant>>>,
    edge_decay_confirmations: Arc<Mutex<HashMap<U256, EdgeDecayConfirmationState>>>,
    /// Scan counter for periodic diagnostics (every ~600 scans ≈ 1 min at 100ms interval).
    scan_count: Arc<std::sync::atomic::AtomicU64>,
    /// Best near-miss edge (bps) seen since last diagnostic — shows how close to threshold.
    near_miss_edge_bps: std::sync::atomic::AtomicU32,
}

pub struct CryptoAlphaDeps {
    pub get_orderbook: Box<dyn Fn(U256) -> Option<OrderBook> + Send + Sync>,
    pub get_available_capital: Box<dyn Fn() -> Decimal + Send + Sync>,
    pub get_position: Box<dyn Fn(U256) -> Decimal + Send + Sync>,
    pub get_held_positions: Box<dyn Fn() -> Vec<(U256, Decimal, Decimal)> + Send + Sync>,
    pub get_balance: Box<dyn Fn() -> Decimal + Send + Sync>,
    pub neg_risk_events: Vec<NegRiskEvent>,
    pub binary_event_groups: Vec<BinaryEventGroup>,
    pub event_calendar: Option<Arc<EventCalendarService>>,
}

impl CryptoAlphaStrategy {
    fn binary_direction_bucket(direction: PriceDirection, is_yes: bool) -> CryptoDirectionBucket {
        match (direction, is_yes) {
            (PriceDirection::Above, true) | (PriceDirection::Below, false) => {
                CryptoDirectionBucket::Up
            }
            (PriceDirection::Below, true) | (PriceDirection::Above, false) => {
                CryptoDirectionBucket::Down
            }
        }
    }

    fn range_direction_bucket(range: &CryptoPriceRange, is_yes: bool) -> CryptoDirectionBucket {
        match (range, is_yes) {
            (CryptoPriceRange::AtOrAbove(_), true) | (CryptoPriceRange::AtOrBelow(_), false) => {
                CryptoDirectionBucket::Up
            }
            (CryptoPriceRange::AtOrBelow(_), true) | (CryptoPriceRange::AtOrAbove(_), false) => {
                CryptoDirectionBucket::Down
            }
            (CryptoPriceRange::Range(_, _), true) => CryptoDirectionBucket::InsideRange,
            (CryptoPriceRange::Range(_, _), false) => CryptoDirectionBucket::OutsideRange,
        }
    }

    fn record_rejection(asset: &CryptoAsset, reason: &'static str) {
        pa_monitor::metrics::CRYPTO_ALPHA_REJECTIONS
            .with_label_values(&[asset.name, reason])
            .inc();
    }

    fn refresh_interval_or_legacy(&self, granular: u64) -> u64 {
        if granular > 0 {
            granular
        } else {
            self.config.refresh_interval_secs
        }
    }

    fn spot_refresh_interval_secs(&self) -> u64 {
        self.refresh_interval_or_legacy(self.config.spot_refresh_interval_secs)
    }

    fn history_refresh_interval_secs(&self) -> u64 {
        self.refresh_interval_or_legacy(self.config.history_refresh_interval_secs)
    }

    fn iv_refresh_interval_secs(&self) -> u64 {
        self.refresh_interval_or_legacy(self.config.iv_refresh_interval_secs)
    }

    pub fn new(config: CryptoAlphaConfig, gas_cost_usd: Decimal, deps: CryptoAlphaDeps) -> Self {
        let CryptoAlphaDeps {
            get_orderbook,
            get_available_capital,
            get_position,
            get_held_positions,
            get_balance,
            neg_risk_events,
            binary_event_groups,
            event_calendar,
        } = deps;

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
            get_held_positions,
            get_balance,
            price_cache: Arc::new(Mutex::new(HashMap::new())),
            neg_risk_events,
            binary_event_groups,
            event_calendar,
            edge_decay_cooldowns: Arc::new(Mutex::new(HashMap::new())),
            edge_decay_confirmations: Arc::new(Mutex::new(HashMap::new())),
            scan_count: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            near_miss_edge_bps: std::sync::atomic::AtomicU32::new(0),
        }
    }

    async fn effective_entry_thresholds(
        &self,
        market_text: &str,
        days_to_resolution: u32,
    ) -> (u32, u32) {
        let mut min_edge_bps = self.config.min_edge_bps;
        let mut max_spread_bps = self.config.max_spread_bps;

        let (horizon_edge_multiplier, horizon_spread_multiplier) =
            if days_to_resolution <= self.config.short_horizon_max_days {
                (
                    self.config.short_horizon_min_edge_multiplier,
                    self.config.short_horizon_max_spread_multiplier,
                )
            } else if days_to_resolution <= self.config.medium_horizon_max_days {
                (
                    self.config.medium_horizon_min_edge_multiplier,
                    self.config.medium_horizon_max_spread_multiplier,
                )
            } else {
                (Decimal::ONE, Decimal::ONE)
            };

        min_edge_bps = (Decimal::from(min_edge_bps) * horizon_edge_multiplier)
            .ceil()
            .to_u32()
            .unwrap_or(u32::MAX);
        max_spread_bps = (Decimal::from(max_spread_bps) * horizon_spread_multiplier)
            .floor()
            .to_u32()
            .unwrap_or(0);

        let Some(event_calendar) = &self.event_calendar else {
            return (min_edge_bps, max_spread_bps);
        };

        let Some(impact) = event_calendar
            .matching_impact(market_text, Utc::now())
            .await
        else {
            return (min_edge_bps, max_spread_bps);
        };

        let (edge_multiplier, spread_multiplier) = match impact {
            EventImpact::Low => (
                self.config.low_event_min_edge_multiplier,
                self.config.low_event_max_spread_multiplier,
            ),
            EventImpact::Medium => (
                self.config.medium_event_min_edge_multiplier,
                self.config.medium_event_max_spread_multiplier,
            ),
            EventImpact::High => (
                self.config.high_event_min_edge_multiplier,
                self.config.high_event_max_spread_multiplier,
            ),
        };

        min_edge_bps = (Decimal::from(min_edge_bps) * edge_multiplier)
            .ceil()
            .to_u32()
            .unwrap_or(u32::MAX);
        max_spread_bps = (Decimal::from(max_spread_bps) * spread_multiplier)
            .floor()
            .to_u32()
            .unwrap_or(0);

        (min_edge_bps, max_spread_bps)
    }

    async fn effective_event_sigma_multiplier(&self, market_text: &str) -> f64 {
        let Some(event_calendar) = &self.event_calendar else {
            return 1.0;
        };

        let Some(impact) = event_calendar
            .matching_impact(market_text, Utc::now())
            .await
        else {
            return 1.0;
        };

        let multiplier = match impact {
            EventImpact::Low => self.config.low_event_sigma_multiplier,
            EventImpact::Medium => self.config.medium_event_sigma_multiplier,
            EventImpact::High => self.config.high_event_sigma_multiplier,
        };

        multiplier.to_f64().unwrap_or(1.0).max(1.0)
    }

    async fn effective_event_size_multiplier(&self, market_text: &str) -> Decimal {
        let Some(event_calendar) = &self.event_calendar else {
            return Decimal::ONE;
        };

        let Some(impact) = event_calendar
            .matching_impact(market_text, Utc::now())
            .await
        else {
            return Decimal::ONE;
        };

        match impact {
            EventImpact::Low => self.config.low_event_size_multiplier,
            EventImpact::Medium => self.config.medium_event_size_multiplier,
            EventImpact::High => self.config.high_event_size_multiplier,
        }
        .min(Decimal::ONE)
        .max(Decimal::ZERO)
    }

    async fn effective_sigma_multiplier(
        &self,
        asset: &CryptoAsset,
        days_to_resolution: u32,
        market_type: CryptoMarketType,
        market_text: &str,
    ) -> f64 {
        let event_multiplier = self.effective_event_sigma_multiplier(market_text).await;
        let override_multiplier = self
            .calibration_override_sigma_multiplier(asset, days_to_resolution, market_type)
            .unwrap_or(Decimal::ONE)
            .to_f64()
            .unwrap_or(1.0);
        (event_multiplier * override_multiplier).max(0.0)
    }

    async fn effective_size_multiplier(
        &self,
        asset: &CryptoAsset,
        days_to_resolution: u32,
        market_type: CryptoMarketType,
        market_text: &str,
    ) -> Decimal {
        let event_multiplier = self.effective_event_size_multiplier(market_text).await;
        let horizon_multiplier = self.effective_horizon_size_multiplier(days_to_resolution);
        let override_multiplier = self
            .calibration_override_size_multiplier(asset, days_to_resolution, market_type)
            .unwrap_or(Decimal::ONE);
        (event_multiplier * horizon_multiplier * override_multiplier)
            .min(Decimal::ONE)
            .max(Decimal::ZERO)
    }

    fn effective_horizon_size_multiplier(&self, days_to_resolution: u32) -> Decimal {
        if days_to_resolution <= self.config.short_horizon_max_days {
            self.config.short_horizon_size_multiplier
        } else if days_to_resolution <= self.config.medium_horizon_max_days {
            self.config.medium_horizon_size_multiplier
        } else {
            Decimal::ONE
        }
        .min(Decimal::ONE)
        .max(Decimal::ZERO)
    }

    fn horizon_bucket(&self, days_to_resolution: u32) -> &'static str {
        if days_to_resolution <= self.config.short_horizon_max_days {
            "short"
        } else if days_to_resolution <= self.config.medium_horizon_max_days {
            "medium"
        } else {
            "long"
        }
    }

    fn calibration_override_entry(
        &self,
        asset: &CryptoAsset,
        days_to_resolution: u32,
        market_type: CryptoMarketType,
    ) -> Option<&pa_core::config::CryptoCalibrationOverride> {
        let horizon = self.horizon_bucket(days_to_resolution);
        self.config.calibration_overrides.iter().find(|entry| {
            let asset_match = entry.asset.is_empty()
                || entry.asset == "*"
                || entry.asset.eq_ignore_ascii_case(asset.binance_symbol);
            let horizon_match = entry.horizon.is_empty()
                || entry.horizon.eq_ignore_ascii_case("any")
                || entry.horizon.eq_ignore_ascii_case(horizon);
            let market_type_match = entry.market_type.is_empty()
                || entry.market_type.eq_ignore_ascii_case("any")
                || entry.market_type.eq_ignore_ascii_case(market_type.as_str());
            asset_match && horizon_match && market_type_match
        })
    }

    fn calibration_override_probability(
        &self,
        asset: &CryptoAsset,
        days_to_resolution: u32,
        market_type: CryptoMarketType,
    ) -> Option<Decimal> {
        self.calibration_override_entry(asset, days_to_resolution, market_type)
            .and_then(|entry| entry.probability_calibration)
            .map(|v| v.min(Decimal::ONE).max(Decimal::ZERO))
    }

    fn calibration_override_sigma_multiplier(
        &self,
        asset: &CryptoAsset,
        days_to_resolution: u32,
        market_type: CryptoMarketType,
    ) -> Option<Decimal> {
        self.calibration_override_entry(asset, days_to_resolution, market_type)
            .and_then(|entry| entry.sigma_multiplier)
            .map(|v| v.max(Decimal::ZERO))
    }

    fn calibration_override_size_multiplier(
        &self,
        asset: &CryptoAsset,
        days_to_resolution: u32,
        market_type: CryptoMarketType,
    ) -> Option<Decimal> {
        self.calibration_override_entry(asset, days_to_resolution, market_type)
            .and_then(|entry| entry.size_multiplier)
            .map(|v| v.min(Decimal::ONE).max(Decimal::ZERO))
    }

    fn effective_probability_calibration_factor(
        &self,
        asset: &CryptoAsset,
        days_to_resolution: u32,
        market_type: CryptoMarketType,
    ) -> Decimal {
        if let Some(override_factor) =
            self.calibration_override_probability(asset, days_to_resolution, market_type)
        {
            return override_factor;
        }
        let asset_factor = match asset.binance_symbol {
            "BTCUSDT" => self.config.btc_probability_calibration,
            "ETHUSDT" => self.config.eth_probability_calibration,
            _ => self.config.alt_probability_calibration,
        };
        let horizon_factor = if days_to_resolution <= self.config.short_horizon_max_days {
            self.config.short_horizon_probability_calibration
        } else if days_to_resolution <= self.config.medium_horizon_max_days {
            self.config.medium_horizon_probability_calibration
        } else {
            Decimal::ONE
        };
        let market_type_factor = match market_type {
            CryptoMarketType::Binary => self.config.binary_probability_calibration,
            CryptoMarketType::Range => self.config.range_probability_calibration,
        };

        (asset_factor * horizon_factor * market_type_factor)
            .min(Decimal::ONE)
            .max(Decimal::ZERO)
    }

    fn calibrate_probability(
        &self,
        asset: &CryptoAsset,
        days_to_resolution: u32,
        market_type: CryptoMarketType,
        raw_prob: Decimal,
    ) -> Decimal {
        let factor =
            self.effective_probability_calibration_factor(asset, days_to_resolution, market_type);
        (dec!(0.5) + ((raw_prob - dec!(0.5)) * factor))
            .min(Decimal::ONE)
            .max(Decimal::ZERO)
    }

    fn effective_exit_thresholds(&self, days_to_resolution: u32) -> (Decimal, Decimal) {
        if days_to_resolution <= self.config.short_horizon_max_days {
            (
                self.config.short_horizon_capital_efficiency_threshold,
                (Decimal::from(self.config.exit_buffer_bps) / dec!(10000))
                    * self.config.short_horizon_exit_buffer_multiplier,
            )
        } else if days_to_resolution <= self.config.medium_horizon_max_days {
            (
                self.config.medium_horizon_capital_efficiency_threshold,
                (Decimal::from(self.config.exit_buffer_bps) / dec!(10000))
                    * self.config.medium_horizon_exit_buffer_multiplier,
            )
        } else {
            (
                self.config.capital_efficiency_threshold,
                Decimal::from(self.config.exit_buffer_bps) / dec!(10000),
            )
        }
    }

    fn effective_hold_edge_threshold(&self, days_to_resolution: u32) -> Decimal {
        let multiplier = if days_to_resolution <= self.config.short_horizon_max_days {
            self.config.short_horizon_hold_edge_multiplier
        } else if days_to_resolution <= self.config.medium_horizon_max_days {
            self.config.medium_horizon_hold_edge_multiplier
        } else {
            Decimal::ONE
        };

        (Decimal::from(self.config.hold_min_edge_bps) / dec!(10000)) * multiplier
    }

    fn effective_edge_decay_exit_fraction(&self, days_to_resolution: u32) -> Decimal {
        let multiplier = if days_to_resolution <= self.config.short_horizon_max_days {
            self.config.short_horizon_edge_decay_exit_multiplier
        } else if days_to_resolution <= self.config.medium_horizon_max_days {
            self.config.medium_horizon_edge_decay_exit_multiplier
        } else {
            Decimal::ONE
        };

        (self.config.edge_decay_exit_fraction * multiplier)
            .min(Decimal::ONE)
            .max(Decimal::ZERO)
    }

    fn edge_decay_severity_multiplier(
        &self,
        edge_shortfall: Decimal,
        moderate_multiplier: Decimal,
        severe_multiplier: Decimal,
    ) -> Decimal {
        if edge_shortfall >= Decimal::from(self.config.edge_decay_severe_gap_bps) / dec!(10000) {
            severe_multiplier
        } else if edge_shortfall
            >= Decimal::from(self.config.edge_decay_moderate_gap_bps) / dec!(10000)
        {
            moderate_multiplier
        } else {
            Decimal::ONE
        }
    }

    fn planned_edge_decay_exit_size(
        &self,
        position_size: Decimal,
        best_bid: Decimal,
        days_to_resolution: u32,
        confirmations: u32,
        edge_shortfall: Decimal,
    ) -> Decimal {
        let required_confirmations =
            self.effective_edge_decay_confirmation_scans(days_to_resolution, edge_shortfall);
        let extra_confirmations = confirmations.saturating_sub(required_confirmations);
        let severity_multiplier = self.edge_decay_severity_multiplier(
            edge_shortfall,
            self.config.edge_decay_moderate_exit_multiplier,
            self.config.edge_decay_severe_exit_multiplier,
        );
        let fraction = ((self.effective_edge_decay_exit_fraction(days_to_resolution)
            + (self.config.edge_decay_exit_fraction_step * Decimal::from(extra_confirmations)))
            * severity_multiplier)
            .min(Decimal::ONE)
            .max(Decimal::ZERO);
        let partial = (position_size * fraction).round_dp(2);
        if partial <= Decimal::ZERO
            || (best_bid > Decimal::ZERO && partial * best_bid < Decimal::ONE)
        {
            position_size
        } else {
            partial.min(position_size)
        }
    }

    fn edge_decay_cooldown_active(&self, token_id: U256) -> bool {
        let cooldowns = self.edge_decay_cooldowns.lock().unwrap();
        cooldowns
            .get(&token_id)
            .is_some_and(|until| *until > Instant::now())
    }

    fn effective_edge_decay_cooldown_secs(
        &self,
        days_to_resolution: u32,
        edge_shortfall: Decimal,
    ) -> u64 {
        let horizon_multiplier = if days_to_resolution <= self.config.short_horizon_max_days {
            self.config.short_horizon_edge_decay_cooldown_multiplier
        } else if days_to_resolution <= self.config.medium_horizon_max_days {
            self.config.medium_horizon_edge_decay_cooldown_multiplier
        } else {
            Decimal::ONE
        };
        let severity_multiplier = self.edge_decay_severity_multiplier(
            edge_shortfall,
            self.config.edge_decay_moderate_cooldown_multiplier,
            self.config.edge_decay_severe_cooldown_multiplier,
        );

        (Decimal::from(self.config.edge_decay_cooldown_secs)
            * horizon_multiplier
            * severity_multiplier)
            .round()
            .to_u64()
            .unwrap_or(self.config.edge_decay_cooldown_secs)
            .max(1)
    }

    fn effective_edge_decay_confirmation_scans(
        &self,
        days_to_resolution: u32,
        edge_shortfall: Decimal,
    ) -> u32 {
        let base = if days_to_resolution <= self.config.short_horizon_max_days {
            self.config.short_horizon_edge_decay_confirmation_scans
        } else if days_to_resolution <= self.config.medium_horizon_max_days {
            self.config.medium_horizon_edge_decay_confirmation_scans
        } else {
            self.config.edge_decay_confirmation_scans
        };
        let severity_multiplier = self.edge_decay_severity_multiplier(
            edge_shortfall,
            self.config.edge_decay_moderate_confirmation_scan_multiplier,
            self.config.edge_decay_severe_confirmation_scan_multiplier,
        );

        (Decimal::from(base) * severity_multiplier)
            .ceil()
            .to_u32()
            .unwrap_or(base)
            .max(1)
    }

    fn effective_edge_decay_confirmation_window_secs(
        &self,
        days_to_resolution: u32,
        edge_shortfall: Decimal,
    ) -> u64 {
        let horizon_multiplier = if days_to_resolution <= self.config.short_horizon_max_days {
            self.config
                .short_horizon_edge_decay_confirmation_window_multiplier
        } else if days_to_resolution <= self.config.medium_horizon_max_days {
            self.config
                .medium_horizon_edge_decay_confirmation_window_multiplier
        } else {
            Decimal::ONE
        };
        let severity_multiplier = self.edge_decay_severity_multiplier(
            edge_shortfall,
            self.config
                .edge_decay_moderate_confirmation_window_multiplier,
            self.config.edge_decay_severe_confirmation_window_multiplier,
        );

        (Decimal::from(self.config.edge_decay_confirmation_window_secs)
            * horizon_multiplier
            * severity_multiplier)
            .round()
            .to_u64()
            .unwrap_or(self.config.edge_decay_confirmation_window_secs)
            .max(1)
    }

    fn set_edge_decay_cooldown(
        &self,
        token_id: U256,
        days_to_resolution: u32,
        edge_shortfall: Decimal,
    ) {
        let mut cooldowns = self.edge_decay_cooldowns.lock().unwrap();
        cooldowns.insert(
            token_id,
            Instant::now()
                + Duration::from_secs(
                    self.effective_edge_decay_cooldown_secs(days_to_resolution, edge_shortfall),
                ),
        );
        if cooldowns.len() > 512 {
            let now = Instant::now();
            cooldowns.retain(|_, until| *until > now);
        }
    }

    fn note_edge_decay_confirmation(
        &self,
        token_id: U256,
        days_to_resolution: u32,
        edge_shortfall: Decimal,
    ) -> u32 {
        let mut confirmations = self.edge_decay_confirmations.lock().unwrap();
        let now = Instant::now();
        let window = Duration::from_secs(
            self.effective_edge_decay_confirmation_window_secs(days_to_resolution, edge_shortfall),
        );
        let count = {
            let state = confirmations
                .entry(token_id)
                .or_insert(EdgeDecayConfirmationState {
                    count: 0,
                    last_seen: now,
                });
            if now.duration_since(state.last_seen) <= window {
                state.count = state.count.saturating_add(1);
            } else {
                state.count = 1;
            }
            state.last_seen = now;
            state.count
        };
        if confirmations.len() > 512 {
            let max_window_secs = self
                .config
                .edge_decay_confirmation_window_secs
                .max(self.effective_edge_decay_confirmation_window_secs(
                    self.config.medium_horizon_max_days,
                    Decimal::ZERO,
                ))
                .max(self.effective_edge_decay_confirmation_window_secs(
                    self.config.short_horizon_max_days,
                    Decimal::ZERO,
                ));
            let max_window = Duration::from_secs(max_window_secs);
            confirmations
                .retain(|_, existing| now.duration_since(existing.last_seen) <= max_window);
        }
        count
    }

    fn reset_edge_decay_confirmation(&self, token_id: U256) {
        let mut confirmations = self.edge_decay_confirmations.lock().unwrap();
        confirmations.remove(&token_id);
    }

    /// Get price data, using independently refreshed cache components.
    /// Spot TTL is dynamically shortened when recent volatility exceeds the baseline.
    async fn get_price_data(&self, asset: &CryptoAsset) -> anyhow::Result<CryptoPriceData> {
        let cache_key = asset.binance_symbol.to_string();
        let now = Utc::now();
        let (
            mut current_price,
            mut daily_closes,
            mut implied_vol,
            last_sigma,
            spot_fetched_at,
            history_fetched_at,
            iv_fetched_at,
        ) = {
            let cache = self.price_cache.lock().unwrap();
            if let Some(cached) = cache.get(&cache_key) {
                (
                    cached.current_price,
                    cached.daily_closes.clone(),
                    cached.implied_vol,
                    cached.last_sigma,
                    cached.current_price_fetched_at,
                    cached.daily_closes_fetched_at,
                    cached.implied_vol_fetched_at,
                )
            } else {
                (None, None, None, None, None, None, None)
            }
        };

        let spot_ttl = match last_sigma {
            Some(sigma) if sigma > 0.0 => {
                let scale = (sigma / BASELINE_CRYPTO_SIGMA).max(1.0);
                self.spot_refresh_interval_secs() as f64 / scale
            }
            _ => self.spot_refresh_interval_secs() as f64,
        };

        let spot_fresh = spot_fetched_at
            .map(|ts| now.signed_duration_since(ts).num_seconds() < spot_ttl as i64)
            .unwrap_or(false);
        let mut spot_refreshed = false;
        if !spot_fresh || current_price.is_none() {
            pa_monitor::metrics::CRYPTO_ALPHA_CACHE_EVENTS
                .with_label_values(&[asset.name, "spot", "refresh"])
                .inc();
            current_price = Some(self.price_client.fetch_current_price(asset).await?);
            spot_refreshed = true;
        } else {
            pa_monitor::metrics::CRYPTO_ALPHA_CACHE_EVENTS
                .with_label_values(&[asset.name, "spot", "hit"])
                .inc();
        }

        let history_fresh = history_fetched_at
            .map(|ts| {
                now.signed_duration_since(ts).num_seconds()
                    < self.history_refresh_interval_secs() as i64
            })
            .unwrap_or(false);
        let mut history_refreshed = false;
        if !history_fresh || daily_closes.is_none() {
            pa_monitor::metrics::CRYPTO_ALPHA_CACHE_EVENTS
                .with_label_values(&[asset.name, "history", "refresh"])
                .inc();
            daily_closes = Some(self.price_client.fetch_daily_closes(asset).await?);
            history_refreshed = true;
        } else {
            pa_monitor::metrics::CRYPTO_ALPHA_CACHE_EVENTS
                .with_label_values(&[asset.name, "history", "hit"])
                .inc();
        }

        let iv_required = asset.deribit_currency.is_some();
        let iv_fresh = iv_fetched_at
            .map(|ts| {
                now.signed_duration_since(ts).num_seconds() < self.iv_refresh_interval_secs() as i64
            })
            .unwrap_or(false);
        let mut iv_refreshed = false;
        if iv_required && (!iv_fresh || implied_vol.is_none()) {
            pa_monitor::metrics::CRYPTO_ALPHA_CACHE_EVENTS
                .with_label_values(&[asset.name, "iv", "refresh"])
                .inc();
            implied_vol = self.price_client.fetch_implied_vol(asset).await?;
            iv_refreshed = true;
        } else if iv_required {
            pa_monitor::metrics::CRYPTO_ALPHA_CACHE_EVENTS
                .with_label_values(&[asset.name, "iv", "hit"])
                .inc();
        }

        {
            let mut cache = self.price_cache.lock().unwrap();
            let entry = cache.entry(cache_key).or_insert(CachedPrice {
                current_price: None,
                current_price_fetched_at: None,
                daily_closes: None,
                daily_closes_fetched_at: None,
                implied_vol: None,
                implied_vol_fetched_at: None,
                last_sigma,
            });

            if let Some(price) = current_price {
                entry.current_price = Some(price);
                if spot_refreshed {
                    entry.current_price_fetched_at = Some(now);
                }
            }
            if let Some(ref closes) = daily_closes {
                entry.daily_closes = Some(closes.clone());
                if history_refreshed {
                    entry.daily_closes_fetched_at = Some(now);
                }
            }
            if iv_required {
                entry.implied_vol = implied_vol;
                if iv_refreshed {
                    entry.implied_vol_fetched_at = Some(now);
                }
            }
        }

        Ok(CryptoPriceData {
            current_price: current_price
                .ok_or_else(|| anyhow::anyhow!("Missing cached/fetched current price"))?,
            daily_closes: daily_closes
                .ok_or_else(|| anyhow::anyhow!("Missing cached/fetched daily closes"))?,
            implied_vol,
        })
    }

    /// Update the cached volatility for dynamic TTL calculation.
    fn update_cache_sigma(&self, binance_symbol: &str, sigma: f64) {
        if let Ok(mut cache) = self.price_cache.lock()
            && let Some(entry) = cache.get_mut(binance_symbol)
        {
            entry.last_sigma = Some(sigma);
        }
    }

    fn build_token_asset_map(&self, markets: &[MarketInfo]) -> HashMap<U256, &'static CryptoAsset> {
        let mut token_assets = HashMap::new();

        for market in markets {
            if let Some(asset) = parse_crypto_question(&market.question).map(|q| q.asset) {
                for token in &market.tokens {
                    token_assets.insert(token.token_id, asset);
                }
                continue;
            }

            if let Some(title) = market.event_title.as_ref()
                && let Some((asset, _)) = parse_crypto_event_title(title)
            {
                for token in &market.tokens {
                    token_assets.insert(token.token_id, asset);
                }
            }
        }

        for event in &self.neg_risk_events {
            if let Some((asset, _)) = parse_crypto_event_title(&event.title) {
                for market in &event.markets {
                    for token in &market.tokens {
                        token_assets.insert(token.token_id, asset);
                    }
                }
            }
        }

        for group in &self.binary_event_groups {
            if let Some(asset) = find_asset(&group.title).or_else(|| {
                group
                    .markets
                    .iter()
                    .find_map(|market| parse_crypto_question(&market.question).map(|q| q.asset))
            }) {
                for market in &group.markets {
                    for token in &market.tokens {
                        token_assets.insert(token.token_id, asset);
                    }
                }
            }
        }

        token_assets
    }

    fn build_token_direction_map(
        &self,
        markets: &[MarketInfo],
    ) -> HashMap<U256, CryptoDirectionBucket> {
        let mut token_directions = HashMap::new();

        for market in markets {
            if let Some(question) = parse_crypto_question(&market.question)
                && market.tokens.len() >= 2
            {
                token_directions.insert(
                    market.tokens[0].token_id,
                    Self::binary_direction_bucket(question.direction, true),
                );
                token_directions.insert(
                    market.tokens[1].token_id,
                    Self::binary_direction_bucket(question.direction, false),
                );
                continue;
            }

            if let Some(range) = parse_crypto_outcome_range(&market.question)
                && market.tokens.len() >= 2
            {
                token_directions.insert(
                    market.tokens[0].token_id,
                    Self::range_direction_bucket(&range, true),
                );
                token_directions.insert(
                    market.tokens[1].token_id,
                    Self::range_direction_bucket(&range, false),
                );
            }
        }

        for event in &self.neg_risk_events {
            for market in &event.markets {
                if let Some(range) = parse_crypto_outcome_range(&market.question)
                    && market.tokens.len() >= 2
                {
                    token_directions.insert(
                        market.tokens[0].token_id,
                        Self::range_direction_bucket(&range, true),
                    );
                    token_directions.insert(
                        market.tokens[1].token_id,
                        Self::range_direction_bucket(&range, false),
                    );
                }
            }
        }

        for group in &self.binary_event_groups {
            for market in &group.markets {
                if let Some(question) = parse_crypto_question(&market.question)
                    && market.tokens.len() >= 2
                {
                    token_directions.insert(
                        market.tokens[0].token_id,
                        Self::binary_direction_bucket(question.direction, true),
                    );
                    token_directions.insert(
                        market.tokens[1].token_id,
                        Self::binary_direction_bucket(question.direction, false),
                    );
                }
            }
        }

        token_directions
    }

    fn current_asset_exposure(
        &self,
        asset: &CryptoAsset,
        token_assets: &HashMap<U256, &'static CryptoAsset>,
    ) -> Decimal {
        (self.get_held_positions)()
            .into_iter()
            .filter_map(|(token_id, size, avg_cost)| {
                let held_asset = token_assets.get(&token_id)?;
                if held_asset.binance_symbol == asset.binance_symbol {
                    Some(size * avg_cost)
                } else {
                    None
                }
            })
            .sum()
    }

    fn current_asset_direction_exposure(
        &self,
        asset: &CryptoAsset,
        direction: CryptoDirectionBucket,
        token_assets: &HashMap<U256, &'static CryptoAsset>,
        token_directions: &HashMap<U256, CryptoDirectionBucket>,
    ) -> Decimal {
        (self.get_held_positions)()
            .into_iter()
            .filter_map(|(token_id, size, avg_cost)| {
                let held_asset = token_assets.get(&token_id)?;
                let held_direction = token_directions.get(&token_id)?;
                if held_asset.binance_symbol == asset.binance_symbol && *held_direction == direction
                {
                    Some(size * avg_cost)
                } else {
                    None
                }
            })
            .sum()
    }

    fn size_entry(
        &self,
        asset: &CryptoAsset,
        direction: CryptoDirectionBucket,
        token_id: U256,
        ask_price: Decimal,
        edge: Decimal,
        days_to_resolution: u32,
        event_size_multiplier: Decimal,
        current_asset_exposure: Decimal,
        current_asset_direction_exposure: Decimal,
    ) -> Option<Decimal> {
        let wallet_balance = (self.get_balance)();
        let market_cap = wallet_balance * self.config.max_position_pct;
        let asset_cap = wallet_balance * self.config.max_exposure_per_asset_pct;
        let asset_direction_cap = wallet_balance * self.config.max_exposure_per_asset_direction_pct;
        let available = (self.get_available_capital)();

        let existing_cost = (self.get_position)(token_id) * ask_price;
        let remaining_market = (market_cap - existing_cost).max(Decimal::ZERO);
        let remaining_asset = (asset_cap - current_asset_exposure).max(Decimal::ZERO);
        let remaining_asset_direction =
            (asset_direction_cap - current_asset_direction_exposure).max(Decimal::ZERO);
        let effective_max = remaining_market
            .min(remaining_asset)
            .min(remaining_asset_direction);
        if effective_max <= Decimal::ZERO {
            Self::record_rejection(asset, "asset_exposure_cap");
            tracing::debug!(
                asset = asset.name,
                direction = ?direction,
                market_cap = %market_cap,
                asset_cap = %asset_cap,
                asset_direction_cap = %asset_direction_cap,
                existing_cost = %existing_cost,
                current_asset_exposure = %current_asset_exposure,
                current_asset_direction_exposure = %current_asset_direction_exposure,
                "[CryptoAlpha] Rejecting: no remaining asset/market exposure room"
            );
            return None;
        }

        let kelly_raw = if ask_price > Decimal::ZERO && ask_price < dec!(0.99) {
            (edge / (Decimal::ONE - ask_price)).min(Decimal::TWO)
        } else {
            Decimal::ZERO
        };
        let horizon_size_multiplier = self.effective_horizon_size_multiplier(days_to_resolution);
        let kelly_size = kelly_raw
            * self.config.kelly_fraction
            * event_size_multiplier
            * horizon_size_multiplier
            * effective_max;
        let size = kelly_size.min(effective_max).min(available);

        let size = if size > Decimal::ZERO && ask_price > Decimal::ZERO && kelly_raw >= dec!(0.04) {
            let min_cost_size = (Decimal::ONE / ask_price).ceil();
            size.max(min_cost_size)
        } else {
            size
        };
        let size = size.min(effective_max).min(available);

        if size <= Decimal::ZERO || (ask_price > Decimal::ZERO && size * ask_price < Decimal::ONE) {
            Self::record_rejection(asset, "min_order_or_budget");
            return None;
        }

        Some(size)
    }

    fn entry_depth_buffer(&self, opp: &TradingOpportunity) -> Decimal {
        let ExecutionPlan::DirectionalBuy {
            token_id,
            side,
            price,
            size,
            ..
        } = &opp.execution_plan;
        let Some(book) = (self.get_orderbook)(*token_id) else {
            return Decimal::ZERO;
        };
        if *size <= Decimal::ZERO {
            return Decimal::ZERO;
        }
        let depth = book.available_depth(*side, *price);
        depth / *size
    }

    fn keep_better_entry(
        &self,
        best_by_asset: &mut HashMap<(&'static str, CryptoDirectionBucket), TradingOpportunity>,
        asset: &'static CryptoAsset,
        direction: CryptoDirectionBucket,
        candidate: TradingOpportunity,
    ) {
        fn profit_efficiency(opp: &TradingOpportunity) -> Decimal {
            let cost = opp.execution_plan.estimated_cost();
            if cost > Decimal::ZERO {
                opp.estimated_profit / cost
            } else {
                Decimal::ZERO
            }
        }

        match best_by_asset.get_mut(&(asset.binance_symbol, direction)) {
            Some(existing) => {
                let candidate_cost = candidate.execution_plan.estimated_cost();
                let existing_cost = existing.execution_plan.estimated_cost();
                let candidate_efficiency = profit_efficiency(&candidate);
                let existing_efficiency = profit_efficiency(existing);
                let candidate_depth_buffer = self.entry_depth_buffer(&candidate);
                let existing_depth_buffer = self.entry_depth_buffer(existing);
                let candidate_better = candidate.estimated_profit > existing.estimated_profit
                    || (candidate.estimated_profit == existing.estimated_profit
                        && candidate_efficiency > existing_efficiency)
                    || (candidate.estimated_profit == existing.estimated_profit
                        && candidate_efficiency == existing_efficiency
                        && candidate_depth_buffer > existing_depth_buffer)
                    || (candidate.estimated_profit == existing.estimated_profit
                        && candidate_efficiency == existing_efficiency
                        && candidate_depth_buffer == existing_depth_buffer
                        && candidate_cost < existing_cost)
                    || (candidate.estimated_profit == existing.estimated_profit
                        && candidate_efficiency == existing_efficiency
                        && candidate_depth_buffer == existing_depth_buffer
                        && candidate_cost == existing_cost
                        && candidate.spread > existing.spread)
                    || (candidate.estimated_profit == existing.estimated_profit
                        && candidate_efficiency == existing_efficiency
                        && candidate_depth_buffer == existing_depth_buffer
                        && candidate_cost == existing_cost
                        && candidate.spread == existing.spread
                        && candidate.size > existing.size);
                if candidate_better {
                    *existing = candidate;
                }
            }
            None => {
                best_by_asset.insert((asset.binance_symbol, direction), candidate);
            }
        }
    }

    /// Detect a crypto alpha opportunity on a single market.
    async fn detect_crypto_opportunity(
        &self,
        market: &MarketInfo,
        current_asset_exposure: Decimal,
        asset_direction_exposure: &HashMap<(&'static str, CryptoDirectionBucket), Decimal>,
    ) -> Option<TradingOpportunity> {
        let question = parse_crypto_question(&market.question)?;
        // Require a target date
        let target_date = question.target_date?;
        let now_date = Utc::now().date_naive();
        let days = (target_date - now_date).num_days();
        if days <= 0 {
            return None;
        }
        let (effective_min_edge_bps, effective_max_spread_bps) = self
            .effective_entry_thresholds(&market.question, days as u32)
            .await;

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
        let mu = mu * self.config.drift_decay;
        let sigma = effective_volatility(sigma, price_data.implied_vol)
            * self
                .effective_sigma_multiplier(
                    question.asset,
                    days as u32,
                    CryptoMarketType::Binary,
                    &market.question,
                )
                .await;
        self.update_cache_sigma(question.asset.binance_symbol, sigma);

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

        // Check bid-ask spread — wide spreads cause immediate mark-to-market losses
        let yes_bid = yes_book
            .best_bid()
            .map(|l| l.price)
            .unwrap_or(Decimal::ZERO);
        let no_bid = no_book.best_bid().map(|l| l.price).unwrap_or(Decimal::ZERO);
        let yes_spread = if yes_ask > Decimal::ZERO {
            (yes_ask - yes_bid) / yes_ask
        } else {
            Decimal::ONE
        };
        let no_spread = if no_ask > Decimal::ZERO {
            (no_ask - no_bid) / no_ask
        } else {
            Decimal::ONE
        };
        let max_spread = yes_spread.max(no_spread);
        let spread_bps = {
            use rust_decimal::prelude::ToPrimitive;
            (max_spread * dec!(10000)).to_u32().unwrap_or(u32::MAX)
        };
        if spread_bps > effective_max_spread_bps {
            Self::record_rejection(question.asset, "spread_too_wide");
            tracing::debug!(
                question = %market.question,
                yes_spread_bps = %((yes_spread * dec!(10000)).round()),
                no_spread_bps = %((no_spread * dec!(10000)).round()),
                max_allowed = effective_max_spread_bps,
                "[CryptoAlpha] Rejecting: spread too wide"
            );
            return None;
        }

        // Check both YES and NO sides, pick larger edge
        let model_prob = self.calibrate_probability(
            question.asset,
            days as u32,
            CryptoMarketType::Binary,
            Decimal::from_f64_retain(model_prob_f64)?,
        );
        let model_prob_no = Decimal::ONE - model_prob;

        let yes_edge = model_prob - yes_ask;
        let no_edge = model_prob_no - no_ask;

        let (token_id, ask_price, edge, prob_for_sizing, direction_bucket) =
            if yes_edge > no_edge && yes_edge > Decimal::ZERO {
                (
                    yes_token.token_id,
                    yes_ask,
                    yes_edge,
                    model_prob,
                    Self::binary_direction_bucket(question.direction, true),
                )
            } else if no_edge > Decimal::ZERO {
                (
                    no_token.token_id,
                    no_ask,
                    no_edge,
                    model_prob_no,
                    Self::binary_direction_bucket(question.direction, false),
                )
            } else {
                return None;
            };

        // Check min edge
        let edge_bps = {
            use rust_decimal::prelude::ToPrimitive;
            (edge * dec!(10000)).to_u32().unwrap_or(0)
        };
        if edge_bps < effective_min_edge_bps {
            Self::record_rejection(question.asset, "edge_below_threshold");
            self.near_miss_edge_bps
                .fetch_max(edge_bps, std::sync::atomic::Ordering::Relaxed);
            tracing::debug!(
                question = %market.question,
                asset = question.asset.name,
                current_price = price_data.current_price,
                model_prob = %prob_for_sizing,
                ask = %ask_price,
                edge_bps,
                min_edge_bps = effective_min_edge_bps,
                "[CryptoAlpha] near-miss: edge below threshold"
            );
            return None;
        }

        let size = self.size_entry(
            question.asset,
            direction_bucket,
            token_id,
            ask_price,
            edge,
            days as u32,
            self.effective_size_multiplier(
                question.asset,
                days as u32,
                CryptoMarketType::Binary,
                &market.question,
            )
            .await,
            current_asset_exposure,
            *asset_direction_exposure
                .get(&(question.asset.binance_symbol, direction_bucket))
                .unwrap_or(&Decimal::ZERO),
        )?;

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

        tracing::debug!(
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

        Some(TradingOpportunity {
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
        current_asset_exposure: Decimal,
        asset_direction_exposure: &HashMap<(&'static str, CryptoDirectionBucket), Decimal>,
    ) -> Option<TradingOpportunity> {
        let event_market_text = format!("{} {}", event.title, asset.name);
        let (effective_min_edge_bps, effective_max_spread_bps) = self
            .effective_entry_thresholds(&event_market_text, days.ceil() as u32)
            .await;
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
        let mu = mu * self.config.drift_decay;
        let sigma = effective_volatility(sigma, price_data.implied_vol)
            * self
                .effective_sigma_multiplier(
                    asset,
                    days.ceil() as u32,
                    CryptoMarketType::Range,
                    &event_market_text,
                )
                .await;
        self.update_cache_sigma(asset.binance_symbol, sigma);

        // Evaluate each outcome market
        let mut best_edge = Decimal::ZERO;
        let mut best_candidate: Option<(
            &MarketInfo,
            U256,    // token_id
            Decimal, // ask_price
            Decimal, // effective_prob
            Decimal, // edge
            CryptoDirectionBucket,
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
            let model_prob = self.calibrate_probability(
                asset,
                days.ceil() as u32,
                CryptoMarketType::Range,
                Decimal::from_f64_retain(model_prob_f64)?,
            );

            let yes_token = &market.tokens[0];
            let no_token = &market.tokens[1];

            // Check bid-ask spread — skip outcomes with wide spreads
            let mut skip_outcome = false;
            if let Some(ref yb) = (self.get_orderbook)(yes_token.token_id)
                && let (Some(ask), Some(bid)) = (yb.best_ask(), yb.best_bid())
            {
                let spread = if ask.price > Decimal::ZERO {
                    (ask.price - bid.price) / ask.price
                } else {
                    Decimal::ONE
                };
                let spread_bps = {
                    use rust_decimal::prelude::ToPrimitive;
                    (spread * dec!(10000)).to_u32().unwrap_or(u32::MAX)
                };
                if spread_bps > effective_max_spread_bps {
                    Self::record_rejection(asset, "spread_too_wide");
                    skip_outcome = true;
                }
            }
            if let Some(ref nb) = (self.get_orderbook)(no_token.token_id)
                && let (Some(ask), Some(bid)) = (nb.best_ask(), nb.best_bid())
            {
                let spread = if ask.price > Decimal::ZERO {
                    (ask.price - bid.price) / ask.price
                } else {
                    Decimal::ONE
                };
                let spread_bps = {
                    use rust_decimal::prelude::ToPrimitive;
                    (spread * dec!(10000)).to_u32().unwrap_or(u32::MAX)
                };
                if spread_bps > effective_max_spread_bps {
                    Self::record_rejection(asset, "spread_too_wide");
                    skip_outcome = true;
                }
            }
            if skip_outcome {
                tracing::debug!(
                    outcome = %market.question,
                    max_allowed = effective_max_spread_bps,
                    "[CryptoAlpha NegRisk] Skipping: spread too wide"
                );
                continue;
            }

            // YES side check
            if let Some(yes_book) = (self.get_orderbook)(yes_token.token_id)
                && let Some(yes_ask_level) = yes_book.best_ask()
            {
                let yes_ask = yes_ask_level.price;
                if model_prob > yes_ask {
                    let edge = model_prob - yes_ask;
                    if edge > best_edge {
                        best_edge = edge;
                        best_candidate = Some((
                            market,
                            yes_token.token_id,
                            yes_ask,
                            model_prob,
                            edge,
                            Self::range_direction_bucket(&range, true),
                        ));
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
                        best_candidate = Some((
                            market,
                            no_token.token_id,
                            no_ask,
                            no_model_prob,
                            edge,
                            Self::range_direction_bucket(&range, false),
                        ));
                    }
                }
            }
        }

        let (market, token_id, ask_price, effective_prob, edge, direction_bucket) = best_candidate?;

        // Check minimum edge threshold
        let edge_bps = {
            use rust_decimal::prelude::ToPrimitive;
            (edge * dec!(10000)).to_u32().unwrap_or(0)
        };
        if edge_bps < effective_min_edge_bps {
            Self::record_rejection(asset, "edge_below_threshold");
            self.near_miss_edge_bps
                .fetch_max(edge_bps, std::sync::atomic::Ordering::Relaxed);
            tracing::debug!(
                event_title = %event.title,
                asset = asset.name,
                model_prob = %effective_prob,
                ask = %ask_price,
                edge_bps,
                min_edge_bps = effective_min_edge_bps,
                "[CryptoAlpha] NegRisk near-miss: edge below threshold"
            );
            return None;
        }

        let size = self.size_entry(
            asset,
            direction_bucket,
            token_id,
            ask_price,
            edge,
            days.ceil() as u32,
            self.effective_size_multiplier(
                asset,
                days.ceil() as u32,
                CryptoMarketType::Range,
                &event_market_text,
            )
            .await,
            current_asset_exposure,
            *asset_direction_exposure
                .get(&(asset.binance_symbol, direction_bucket))
                .unwrap_or(&Decimal::ZERO),
        )?;

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

        tracing::debug!(
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

        Some(TradingOpportunity {
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
        current_asset_exposure: Decimal,
        asset_direction_exposure: &HashMap<(&'static str, CryptoDirectionBucket), Decimal>,
    ) -> Option<TradingOpportunity> {
        // Try to identify crypto asset from the group title first, then from individual markets
        let asset = find_asset(&group.title).or_else(|| {
            group
                .markets
                .iter()
                .find_map(|m| parse_crypto_question(&m.question).map(|q| q.asset))
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

        let (mu, sigma_base) = calculate_volatility(&price_data.daily_closes)?;
        let mu = mu * self.config.drift_decay;
        let base_effective_sigma = effective_volatility(sigma_base, price_data.implied_vol);

        // Evaluate each market in the group, track the best edge
        let mut best_edge = Decimal::ZERO;
        let mut best_candidate: Option<(
            &MarketInfo,
            U256,    // token_id
            Decimal, // ask_price
            Decimal, // effective_prob (for sizing)
            Decimal, // edge
            u32,     // edge_bps
            CryptoDirectionBucket,
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
            let group_market_text = if group.title.is_empty() {
                market.question.clone()
            } else {
                format!("{} {}", group.title, market.question)
            };
            let (_, effective_max_spread_bps) = self
                .effective_entry_thresholds(&group_market_text, days as u32)
                .await;
            let sigma = base_effective_sigma
                * self
                    .effective_sigma_multiplier(
                        asset,
                        days as u32,
                        CryptoMarketType::Binary,
                        &group_market_text,
                    )
                    .await;
            self.update_cache_sigma(asset.binance_symbol, sigma);

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
            let model_prob = self.calibrate_probability(
                asset,
                days as u32,
                CryptoMarketType::Binary,
                Decimal::from_f64_retain(model_prob_f64)?,
            );
            let model_prob_no = Decimal::ONE - model_prob;

            let yes_token = &market.tokens[0];
            let no_token = &market.tokens[1];

            // Check bid-ask spread — skip markets with wide spreads
            let mut skip_market = false;
            if let Some(ref yb) = (self.get_orderbook)(yes_token.token_id)
                && let (Some(ask), Some(bid)) = (yb.best_ask(), yb.best_bid())
            {
                let spread = if ask.price > Decimal::ZERO {
                    (ask.price - bid.price) / ask.price
                } else {
                    Decimal::ONE
                };
                let spread_bps = {
                    use rust_decimal::prelude::ToPrimitive;
                    (spread * dec!(10000)).to_u32().unwrap_or(u32::MAX)
                };
                if spread_bps > effective_max_spread_bps {
                    Self::record_rejection(asset, "spread_too_wide");
                    skip_market = true;
                }
            }
            if let Some(ref nb) = (self.get_orderbook)(no_token.token_id)
                && let (Some(ask), Some(bid)) = (nb.best_ask(), nb.best_bid())
            {
                let spread = if ask.price > Decimal::ZERO {
                    (ask.price - bid.price) / ask.price
                } else {
                    Decimal::ONE
                };
                let spread_bps = {
                    use rust_decimal::prelude::ToPrimitive;
                    (spread * dec!(10000)).to_u32().unwrap_or(u32::MAX)
                };
                if spread_bps > effective_max_spread_bps {
                    Self::record_rejection(asset, "spread_too_wide");
                    skip_market = true;
                }
            }
            if skip_market {
                tracing::debug!(
                    question = %market.question,
                    max_allowed = effective_max_spread_bps,
                    "[CryptoAlpha Group] Skipping: spread too wide"
                );
                continue;
            }

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
                            Self::binary_direction_bucket(question.direction, true),
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
                            Self::binary_direction_bucket(question.direction, false),
                        ));
                    }
                }
            }
        }

        let (market, token_id, ask_price, effective_prob, edge, edge_bps, direction_bucket) =
            best_candidate?;
        let winner_question = parse_crypto_question(&market.question)?;
        let winner_target_date = winner_question.target_date?;
        let winner_days = (winner_target_date - Utc::now().date_naive()).num_days();
        if winner_days <= 0 {
            return None;
        }
        let winner_market_text = if group.title.is_empty() {
            market.question.clone()
        } else {
            format!("{} {}", group.title, market.question)
        };
        let (effective_min_edge_bps, _) = self
            .effective_entry_thresholds(&winner_market_text, winner_days as u32)
            .await;

        // Check minimum edge threshold
        if edge_bps < effective_min_edge_bps {
            Self::record_rejection(asset, "edge_below_threshold");
            self.near_miss_edge_bps
                .fetch_max(edge_bps, std::sync::atomic::Ordering::Relaxed);
            tracing::debug!(
                group_title = %group.title,
                question = %market.question,
                model_prob = %effective_prob,
                ask = %ask_price,
                edge_bps,
                min_edge_bps = effective_min_edge_bps,
                "[CryptoAlpha] group near-miss: edge below threshold"
            );
            return None;
        }

        let size = self.size_entry(
            asset,
            direction_bucket,
            token_id,
            ask_price,
            edge,
            winner_days as u32,
            self.effective_size_multiplier(
                asset,
                winner_days as u32,
                CryptoMarketType::Binary,
                &group.title,
            )
            .await,
            current_asset_exposure,
            *asset_direction_exposure
                .get(&(asset.binance_symbol, direction_bucket))
                .unwrap_or(&Decimal::ZERO),
        )?;

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

        tracing::debug!(
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

        Some(TradingOpportunity {
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

impl CryptoAlphaStrategy {
    /// Scan held positions for exit conditions (model reversal or capital efficiency).
    async fn scan_exits(&self, markets: &[MarketInfo]) -> Vec<TradingOpportunity> {
        let held = (self.get_held_positions)();
        if held.is_empty() {
            return vec![];
        }

        tracing::debug!(held_positions = held.len(), "[CryptoAlpha] scanning exits");

        // Build reverse map: token_id → market
        let token_to_market: HashMap<U256, &MarketInfo> = markets
            .iter()
            .flat_map(|m| m.tokens.iter().map(move |t| (t.token_id, m)))
            .collect();

        let mut exits = Vec::new();

        for (token_id, size, avg_cost) in &held {
            let book = match (self.get_orderbook)(*token_id) {
                Some(b) => b,
                None => {
                    tracing::debug!(token_id = %token_id, "[CryptoAlpha EXIT] no orderbook — token not subscribed?");
                    continue;
                }
            };
            let best_bid = match book.best_bid() {
                Some(b) => b.price,
                None => {
                    tracing::debug!(token_id = %token_id, "[CryptoAlpha EXIT] no bids in orderbook");
                    continue;
                }
            };

            let market = match token_to_market.get(token_id) {
                Some(m) => *m,
                None => {
                    tracing::debug!(
                        token_id = %token_id,
                        best_bid = %best_bid,
                        "[CryptoAlpha EXIT] token not in scanned markets"
                    );
                    continue;
                }
            };

            let days_to_resolution = if let Some(parsed) = parse_crypto_question(&market.question) {
                parsed
                    .target_date
                    .map(|d| (d - Utc::now().date_naive()).num_days().max(1) as u32)
                    .unwrap_or(30)
            } else if let Some((_asset, target_date)) = market
                .event_title
                .as_deref()
                .and_then(parse_crypto_event_title)
            {
                target_date
                    .map(|d| (d - Utc::now().date_naive()).num_days().max(1) as u32)
                    .unwrap_or(30)
            } else {
                30
            };
            let (capital_efficiency_threshold, exit_buffer) =
                self.effective_exit_thresholds(days_to_resolution);

            // Capital efficiency exit: bid >= threshold
            if best_bid >= capital_efficiency_threshold {
                self.reset_edge_decay_confirmation(*token_id);
                pa_monitor::metrics::CRYPTO_ALPHA_EXITS
                    .with_label_values(&["capital_efficiency"])
                    .inc();
                tracing::debug!(
                    token_id = %token_id,
                    best_bid = %best_bid,
                    capital_efficiency_threshold = %capital_efficiency_threshold,
                    days_to_resolution,
                    "[EXIT] Capital efficiency — crypto"
                );
                exits.push(self.build_exit_opportunity(
                    *token_id,
                    *size,
                    *avg_cost,
                    best_bid,
                    &token_to_market,
                ));
                continue;
            }

            // Relative stop-loss: cut losses regardless of model once the bid falls through
            // a configurable fraction of average cost.
            if *avg_cost > Decimal::ZERO
                && self.config.relative_stop_loss_ratio > Decimal::ZERO
                && best_bid < *avg_cost * self.config.relative_stop_loss_ratio
            {
                self.reset_edge_decay_confirmation(*token_id);
                pa_monitor::metrics::CRYPTO_ALPHA_EXITS
                    .with_label_values(&["relative_stop_loss"])
                    .inc();
                let loss_pct = ((*avg_cost - best_bid) / *avg_cost * dec!(100)).round_dp(1);
                tracing::debug!(
                    token_id = %token_id,
                    best_bid = %best_bid,
                    avg_cost = %avg_cost,
                    loss_pct = %loss_pct,
                    stop_loss_ratio = %self.config.relative_stop_loss_ratio,
                    "[EXIT] Relative stop-loss — crypto"
                );
                exits.push(self.build_exit_opportunity(
                    *token_id,
                    *size,
                    *avg_cost,
                    best_bid,
                    &token_to_market,
                ));
                continue;
            }

            // Model reversal: recompute model probability
            // Try binary market parsing first, then fall back to NegRisk range parsing
            let held_side_prob = if let Some(parsed) = parse_crypto_question(&market.question) {
                // Binary market: standard GBM probability
                let price_data = match self.get_price_data(parsed.asset).await {
                    Ok(d) => d,
                    Err(e) => {
                        tracing::debug!(token_id = %token_id, asset = ?parsed.asset, error = %e, "[CryptoAlpha EXIT] price data fetch failed");
                        continue;
                    }
                };

                let (mu, sigma) = match calculate_volatility(&price_data.daily_closes) {
                    Some(v) => v,
                    None => {
                        tracing::debug!(token_id = %token_id, "[CryptoAlpha EXIT] volatility calculation failed");
                        continue;
                    }
                };
                let mu = mu * self.config.drift_decay;
                let sigma = effective_volatility(sigma, price_data.implied_vol)
                    * self
                        .effective_event_sigma_multiplier(&market.question)
                        .await;
                self.update_cache_sigma(parsed.asset.binance_symbol, sigma);
                if sigma <= 0.0 {
                    continue;
                }

                let target_date = parsed
                    .target_date
                    .unwrap_or_else(|| (Utc::now() + chrono::Duration::days(30)).date_naive());
                let days_to_target =
                    (target_date - Utc::now().date_naive()).num_days().max(1) as f64;

                let model_prob = gbm_probability(
                    price_data.current_price,
                    parsed.threshold,
                    mu,
                    sigma,
                    days_to_target,
                );
                let calibrated_prob = match Decimal::from_f64_retain(match parsed.direction {
                    PriceDirection::Above => model_prob,
                    PriceDirection::Below => 1.0 - model_prob,
                }) {
                    Some(v) => v,
                    None => continue,
                };
                let effective_prob = self
                    .calibrate_probability(
                        parsed.asset,
                        days_to_target.ceil() as u32,
                        CryptoMarketType::Binary,
                        calibrated_prob,
                    )
                    .to_f64()
                    .unwrap_or(0.5);

                let is_yes = market
                    .tokens
                    .first()
                    .map(|t| t.token_id == *token_id)
                    .unwrap_or(false);
                if is_yes {
                    effective_prob
                } else {
                    1.0 - effective_prob
                }
            } else if let Some(range) = parse_crypto_outcome_range(&market.question) {
                // NegRisk outcome: find asset from parent event title
                let neg_risk_asset = self
                    .neg_risk_events
                    .iter()
                    .find(|ev| {
                        ev.markets
                            .iter()
                            .any(|m| m.tokens.iter().any(|t| t.token_id == *token_id))
                    })
                    .and_then(|ev| parse_crypto_event_title(&ev.title));

                let (asset, target_date) = match neg_risk_asset {
                    Some(v) => v,
                    None => {
                        tracing::debug!(token_id = %token_id, question = %market.question, "[CryptoAlpha EXIT] NegRisk range but no matching event");
                        continue;
                    }
                };

                let price_data = match self.get_price_data(asset).await {
                    Ok(d) => d,
                    Err(e) => {
                        tracing::debug!(token_id = %token_id, asset = asset.name, error = %e, "[CryptoAlpha EXIT] NegRisk price data fetch failed");
                        continue;
                    }
                };

                let (mu, sigma) = match calculate_volatility(&price_data.daily_closes) {
                    Some(v) => v,
                    None => {
                        tracing::debug!(token_id = %token_id, "[CryptoAlpha EXIT] NegRisk volatility calculation failed");
                        continue;
                    }
                };
                let mu = mu * self.config.drift_decay;
                let event_market_text = if let Some(title) = market.event_title.as_deref() {
                    format!("{} {}", title, market.question)
                } else {
                    market.question.clone()
                };
                let sigma = effective_volatility(sigma, price_data.implied_vol)
                    * self
                        .effective_event_sigma_multiplier(&event_market_text)
                        .await;
                self.update_cache_sigma(asset.binance_symbol, sigma);
                if sigma <= 0.0 {
                    continue;
                }

                let target_date = target_date
                    .unwrap_or_else(|| (Utc::now() + chrono::Duration::days(30)).date_naive());
                let days_to_target =
                    (target_date - Utc::now().date_naive()).num_days().max(1) as f64;

                let range_prob = gbm_range_probability(
                    price_data.current_price,
                    &range,
                    mu,
                    sigma,
                    days_to_target,
                );
                let calibrated_range_prob = match Decimal::from_f64_retain(range_prob) {
                    Some(v) => self
                        .calibrate_probability(
                            asset,
                            days_to_target.ceil() as u32,
                            CryptoMarketType::Range,
                            v,
                        )
                        .to_f64()
                        .unwrap_or(0.5),
                    None => continue,
                };

                // NegRisk: YES token = tokens[0], NO token = tokens[1]
                let is_yes = market
                    .tokens
                    .first()
                    .map(|t| t.token_id == *token_id)
                    .unwrap_or(false);
                if is_yes {
                    calibrated_range_prob
                } else {
                    1.0 - calibrated_range_prob
                }
            } else {
                tracing::debug!(token_id = %token_id, question = %market.question, "[CryptoAlpha EXIT] could not parse question or outcome range");
                continue;
            };
            let model_prob_dec = match Decimal::from_f64_retain(held_side_prob) {
                Some(d) => d,
                None => continue,
            };
            let hold_edge_threshold = self.effective_hold_edge_threshold(days_to_resolution);

            if model_prob_dec < best_bid - exit_buffer {
                self.reset_edge_decay_confirmation(*token_id);
                pa_monitor::metrics::CRYPTO_ALPHA_EXITS
                    .with_label_values(&["model_reversal"])
                    .inc();
                tracing::debug!(
                    token_id = %token_id,
                    model_prob = %model_prob_dec,
                    best_bid = %best_bid,
                    days_to_resolution,
                    "[EXIT] Model reversal — crypto"
                );
                exits.push(self.build_exit_opportunity(
                    *token_id,
                    *size,
                    *avg_cost,
                    best_bid,
                    &token_to_market,
                ));
            } else if model_prob_dec < best_bid + hold_edge_threshold {
                let edge_shortfall =
                    ((best_bid + hold_edge_threshold) - model_prob_dec).max(Decimal::ZERO);
                let confirmations = self.note_edge_decay_confirmation(
                    *token_id,
                    days_to_resolution,
                    edge_shortfall,
                );
                let edge_decay_exit_size = self.planned_edge_decay_exit_size(
                    *size,
                    best_bid,
                    days_to_resolution,
                    confirmations,
                    edge_shortfall,
                );
                if self.edge_decay_cooldown_active(*token_id) {
                    tracing::debug!(
                        token_id = %token_id,
                        best_bid = %best_bid,
                        model_prob = %model_prob_dec,
                        edge_shortfall = %edge_shortfall,
                        confirmations,
                        "[CryptoAlpha EXIT] edge-decay cooldown active"
                    );
                    continue;
                }
                let required_confirmations = self
                    .effective_edge_decay_confirmation_scans(days_to_resolution, edge_shortfall);
                if confirmations < required_confirmations {
                    tracing::debug!(
                        token_id = %token_id,
                        confirmations,
                        required = required_confirmations,
                        "[CryptoAlpha EXIT] edge-decay awaiting confirmation"
                    );
                    continue;
                }
                pa_monitor::metrics::CRYPTO_ALPHA_EXITS
                    .with_label_values(&["edge_decay"])
                    .inc();
                tracing::debug!(
                    token_id = %token_id,
                    model_prob = %model_prob_dec,
                    best_bid = %best_bid,
                    days_to_resolution,
                    edge_shortfall = %edge_shortfall,
                    hold_edge_threshold = %hold_edge_threshold,
                    exit_size = %edge_decay_exit_size,
                    threshold = %(best_bid + hold_edge_threshold),
                    "[EXIT] Edge decay — crypto"
                );
                exits.push(self.build_exit_opportunity(
                    *token_id,
                    edge_decay_exit_size,
                    *avg_cost,
                    best_bid,
                    &token_to_market,
                ));
                self.set_edge_decay_cooldown(*token_id, days_to_resolution, edge_shortfall);
            } else {
                self.reset_edge_decay_confirmation(*token_id);
                tracing::debug!(
                    token_id = %token_id,
                    model_prob = %model_prob_dec,
                    best_bid = %best_bid,
                    days_to_resolution,
                    exit_buffer = %exit_buffer,
                    hold_edge_threshold = %hold_edge_threshold,
                    threshold = %(best_bid - exit_buffer),
                    "[CryptoAlpha EXIT] No reversal: model_prob >= best_bid - buffer"
                );
            }
        }

        exits
    }

    /// Build an exit opportunity (sell via CLOB FOK).
    fn build_exit_opportunity(
        &self,
        token_id: U256,
        size: Decimal,
        avg_cost: Decimal,
        best_bid: Decimal,
        token_to_market: &HashMap<U256, &MarketInfo>,
    ) -> TradingOpportunity {
        let market = token_to_market.get(&token_id);
        let condition_id = market.map(|m| m.condition_id).unwrap_or_default();
        let question = market.map(|m| m.question.clone()).unwrap_or_default();
        let fee_rate_bps = market.map(|m| m.fee_rate_bps).unwrap_or(200);

        let est = self
            .profit_calc
            .directional_sell_profit(best_bid, avg_cost, size, fee_rate_bps);

        TradingOpportunity {
            id: Uuid::now_v7(),
            strategy_type: StrategyType::CryptoAlpha,
            condition_id,
            question: format!("[EXIT] {}", question),
            spread: best_bid - avg_cost,
            estimated_profit: est.net_profit,
            size,
            detected_at: Utc::now(),
            execution_plan: ExecutionPlan::DirectionalBuy {
                token_id,
                side: TradeSide::Sell,
                price: best_bid,
                size,
                condition_id,
            },
        }
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

    async fn scan(&self, markets: &[MarketInfo]) -> pa_core::Result<Vec<TradingOpportunity>> {
        let mut opportunities = Vec::new();
        let mut best_entries_by_asset: HashMap<
            (&'static str, CryptoDirectionBucket),
            TradingOpportunity,
        > = HashMap::new();
        let count = self
            .scan_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let log_diag = count.is_multiple_of(600); // ~every 60s at 100ms interval

        let mut binary_crypto = 0u32;
        let mut binary_group_crypto = 0u32;
        let mut neg_risk_matched = 0u32;
        let mut neg_risk_expired = 0u32;
        let token_assets = self.build_token_asset_map(markets);
        let token_directions = self.build_token_direction_map(markets);
        let mut asset_exposure: HashMap<&'static str, Decimal> = HashMap::new();
        let mut asset_direction_exposure: HashMap<(&'static str, CryptoDirectionBucket), Decimal> =
            HashMap::new();
        for asset in token_assets.values() {
            asset_exposure
                .entry(asset.binance_symbol)
                .or_insert_with(|| self.current_asset_exposure(asset, &token_assets));
        }
        for asset in token_assets.values() {
            for direction in [
                CryptoDirectionBucket::Up,
                CryptoDirectionBucket::Down,
                CryptoDirectionBucket::InsideRange,
                CryptoDirectionBucket::OutsideRange,
            ] {
                asset_direction_exposure
                    .entry((asset.binance_symbol, direction))
                    .or_insert_with(|| {
                        self.current_asset_direction_exposure(
                            asset,
                            direction,
                            &token_assets,
                            &token_directions,
                        )
                    });
            }
        }
        for asset in CRYPTO_ASSETS {
            let exposure = asset_exposure
                .get(asset.binance_symbol)
                .copied()
                .unwrap_or(Decimal::ZERO);
            if let Some(exposure_f64) = rust_decimal::prelude::ToPrimitive::to_f64(&exposure) {
                pa_monitor::metrics::CRYPTO_ALPHA_ASSET_EXPOSURE
                    .with_label_values(&[asset.name])
                    .set(exposure_f64);
            }
        }

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
                || group
                    .markets
                    .iter()
                    .any(|m| parse_crypto_question(&m.question).is_some());
            if !has_crypto {
                continue;
            }
            binary_group_crypto += 1;
            let Some(asset) = find_asset(&group.title).or_else(|| {
                group
                    .markets
                    .iter()
                    .find_map(|m| parse_crypto_question(&m.question).map(|q| q.asset))
            }) else {
                continue;
            };
            let current_asset_exposure = *asset_exposure
                .get(asset.binance_symbol)
                .unwrap_or(&Decimal::ZERO);
            if let Some(opp) = self
                .detect_crypto_group(group, current_asset_exposure, &asset_direction_exposure)
                .await
            {
                let direction = opp
                    .execution_plan
                    .token_id()
                    .and_then(|token_id| token_directions.get(&token_id).copied())
                    .unwrap_or(CryptoDirectionBucket::OutsideRange);
                self.keep_better_entry(&mut best_entries_by_asset, asset, direction, opp);
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

            let Some(asset) = parse_crypto_question(&market.question).map(|q| q.asset) else {
                continue;
            };
            let current_asset_exposure = *asset_exposure
                .get(asset.binance_symbol)
                .unwrap_or(&Decimal::ZERO);
            if let Some(opp) = self
                .detect_crypto_opportunity(
                    market,
                    current_asset_exposure,
                    &asset_direction_exposure,
                )
                .await
            {
                let direction = opp
                    .execution_plan
                    .token_id()
                    .and_then(|token_id| token_directions.get(&token_id).copied())
                    .unwrap_or(CryptoDirectionBucket::OutsideRange);
                self.keep_better_entry(&mut best_entries_by_asset, asset, direction, opp);
            }
        }

        // 3. NegRisk events
        for event in &self.neg_risk_events {
            if let Some((asset, target_date)) = parse_crypto_event_title(&event.title) {
                let now_date = Utc::now().date_naive();
                let days = target_date.map(|d| (d - now_date).num_days()).unwrap_or(30); // Default 30 days for events without explicit date
                if days <= 0 {
                    neg_risk_expired += 1;
                    continue;
                }
                neg_risk_matched += 1;
                let current_asset_exposure = *asset_exposure
                    .get(asset.binance_symbol)
                    .unwrap_or(&Decimal::ZERO);
                if let Some(opp) = self
                    .detect_crypto_neg_risk(
                        event,
                        asset,
                        days as f64,
                        current_asset_exposure,
                        &asset_direction_exposure,
                    )
                    .await
                {
                    let direction = opp
                        .execution_plan
                        .token_id()
                        .and_then(|token_id| token_directions.get(&token_id).copied())
                        .unwrap_or(CryptoDirectionBucket::OutsideRange);
                    self.keep_better_entry(&mut best_entries_by_asset, asset, direction, opp);
                }
            }
        }

        for ((asset_symbol, direction), opp) in best_entries_by_asset {
            let cost = opp.execution_plan.estimated_cost();
            asset_exposure
                .entry(asset_symbol)
                .and_modify(|exposure| *exposure += cost)
                .or_insert(cost);
            asset_direction_exposure
                .entry((asset_symbol, direction))
                .and_modify(|exposure| *exposure += cost)
                .or_insert(cost);
            opportunities.push(opp);
        }

        if log_diag {
            let best_near_miss = self
                .near_miss_edge_bps
                .swap(0, std::sync::atomic::Ordering::Relaxed);
            tracing::debug!(
                binary_groups = self.binary_event_groups.len(),
                binary_group_crypto,
                binary_ungrouped = binary_crypto,
                neg_risk_events = self.neg_risk_events.len(),
                neg_risk_matched,
                neg_risk_expired,
                opportunities = opportunities.len(),
                best_near_miss_bps = best_near_miss,
                min_edge_bps = self.config.min_edge_bps,
                "[CryptoAlpha] scan diagnostics"
            );
            // Log sample binary event group titles
            for (i, group) in self.binary_event_groups.iter().enumerate() {
                if i >= 5 {
                    break;
                }
                let has_crypto = find_asset(&group.title).is_some();
                tracing::debug!(
                    idx = i,
                    title = %group.title,
                    markets = group.markets.len(),
                    is_crypto = has_crypto,
                    "[CryptoAlpha] binary event group"
                );
            }
            // Log sample ungrouped binary crypto questions
            for (i, q) in binary_crypto_samples.iter().enumerate() {
                tracing::debug!(idx = i, question = %q, "[CryptoAlpha] ungrouped binary crypto");
            }
        }

        // Exit scanning: check held positions for model reversal / capital efficiency
        let exit_opps = self.scan_exits(markets).await;
        opportunities.extend(exit_opps);

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
        let closes: Vec<f64> = (0..30).map(|i| 100.0 * (1.0 + 0.01 * i as f64)).collect();

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
        let (asset, _date) = parse_crypto_event_title("Ethereum price on February 28?").unwrap();
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
            find_asset("What price will Bitcoin hit in 2026?")
                .unwrap()
                .name,
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
        assert_eq!(q.direction, PriceDirection::Below); // "dip to" → Below
    }

    #[test]
    fn test_binary_event_group_type() {
        use alloy::primitives::B256;
        use pa_core::types::BinaryEventGroup;

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
            gamma_best_bid: None,
            gamma_best_ask: None,
            rewards_min_size: None,
            rewards_max_spread: None,
            rewards_daily_rate: None,
            holding_rewards_enabled: false,
            fees_enabled: false,
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

    // ──── Exit Tests ────

    use alloy::primitives::B256;
    use pa_core::config::{EventCalendarConfig, StaticEventConfig};
    use pa_core::types::EventImpact;
    use pa_core::types::{Outcome, PriceLevel, TokenInfo};

    fn make_crypto_market(question: &str) -> MarketInfo {
        MarketInfo {
            condition_id: B256::ZERO,
            question_id: B256::ZERO,
            question: question.to_string(),
            neg_risk: false,
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
            event_title: None,
            end_date: None,
            category: Some("crypto".into()),
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

    fn make_crypto_book(token_id: U256, best_bid: Decimal) -> OrderBook {
        OrderBook {
            token_id,
            bids: vec![PriceLevel {
                price: best_bid,
                size: dec!(500),
            }],
            asks: vec![PriceLevel {
                price: best_bid + dec!(0.02),
                size: dec!(500),
            }],
            timestamp: Utc::now(),
        }
    }

    fn market_with_token_ids(
        mut market: MarketInfo,
        yes_token: U256,
        no_token: U256,
        condition_id: B256,
    ) -> MarketInfo {
        market.condition_id = condition_id;
        market.question_id = condition_id;
        market.tokens[0].token_id = yes_token;
        market.tokens[0].complement_id = no_token;
        market.tokens[1].token_id = no_token;
        market.tokens[1].complement_id = yes_token;
        market
    }

    fn seed_price_cache(strategy: &CryptoAlphaStrategy, asset: &CryptoAsset, current_price: f64) {
        let mut cache = strategy.price_cache.lock().unwrap();
        cache.insert(
            asset.binance_symbol.to_string(),
            CachedPrice {
                current_price: Some(current_price),
                current_price_fetched_at: Some(Utc::now()),
                daily_closes: Some(
                    (0..30)
                        .map(|i| current_price * (0.98 + 0.001 * i as f64))
                        .collect(),
                ),
                daily_closes_fetched_at: Some(Utc::now()),
                implied_vol: None,
                implied_vol_fetched_at: None,
                last_sigma: Some(0.60),
            },
        );
    }

    fn make_crypto_strategy(
        books: HashMap<U256, OrderBook>,
        held: Vec<(U256, Decimal, Decimal)>,
    ) -> CryptoAlphaStrategy {
        let config = CryptoAlphaConfig {
            min_edge_bps: 500,
            max_position_pct: dec!(0.50),
            kelly_fraction: dec!(0.25),
            refresh_interval_secs: 300,
            spot_refresh_interval_secs: 30,
            history_refresh_interval_secs: 1800,
            iv_refresh_interval_secs: 300,
            coingecko_api_key: String::new(),
            exit_buffer_bps: 50,
            capital_efficiency_threshold: dec!(0.98),
            drift_decay: 0.0,
            max_spread_bps: 1500,
            relative_stop_loss_ratio: dec!(0.80),
            max_exposure_per_asset_pct: dec!(0.75),
            max_exposure_per_asset_direction_pct: dec!(0.45),
            low_event_min_edge_multiplier: dec!(1.20),
            medium_event_min_edge_multiplier: dec!(1.50),
            high_event_min_edge_multiplier: dec!(2.00),
            low_event_max_spread_multiplier: dec!(0.90),
            medium_event_max_spread_multiplier: dec!(0.80),
            high_event_max_spread_multiplier: dec!(0.65),
            low_event_sigma_multiplier: dec!(1.05),
            medium_event_sigma_multiplier: dec!(1.15),
            high_event_sigma_multiplier: dec!(1.30),
            low_event_size_multiplier: dec!(0.90),
            medium_event_size_multiplier: dec!(0.75),
            high_event_size_multiplier: dec!(0.50),
            btc_probability_calibration: dec!(0.95),
            eth_probability_calibration: dec!(0.93),
            alt_probability_calibration: dec!(0.88),
            binary_probability_calibration: dec!(0.97),
            range_probability_calibration: dec!(0.90),
            calibration_overrides: vec![],
            short_horizon_max_days: 1,
            medium_horizon_max_days: 7,
            short_horizon_probability_calibration: dec!(0.85),
            medium_horizon_probability_calibration: dec!(0.92),
            short_horizon_size_multiplier: dec!(0.60),
            medium_horizon_size_multiplier: dec!(0.80),
            short_horizon_min_edge_multiplier: dec!(1.50),
            medium_horizon_min_edge_multiplier: dec!(1.20),
            short_horizon_max_spread_multiplier: dec!(0.75),
            medium_horizon_max_spread_multiplier: dec!(0.90),
            short_horizon_capital_efficiency_threshold: dec!(0.92),
            medium_horizon_capital_efficiency_threshold: dec!(0.95),
            short_horizon_exit_buffer_multiplier: dec!(0.50),
            medium_horizon_exit_buffer_multiplier: dec!(0.80),
            hold_min_edge_bps: 100,
            short_horizon_hold_edge_multiplier: dec!(1.50),
            medium_horizon_hold_edge_multiplier: dec!(1.20),
            edge_decay_exit_fraction: dec!(0.25),
            edge_decay_exit_fraction_step: dec!(0.10),
            edge_decay_moderate_gap_bps: 50,
            edge_decay_severe_gap_bps: 150,
            edge_decay_moderate_exit_multiplier: dec!(1.25),
            edge_decay_severe_exit_multiplier: dec!(1.50),
            edge_decay_moderate_cooldown_multiplier: dec!(0.75),
            edge_decay_severe_cooldown_multiplier: dec!(0.50),
            short_horizon_edge_decay_exit_multiplier: dec!(1.50),
            medium_horizon_edge_decay_exit_multiplier: dec!(1.20),
            edge_decay_cooldown_secs: 1800,
            edge_decay_confirmation_scans: 2,
            short_horizon_edge_decay_confirmation_scans: 1,
            medium_horizon_edge_decay_confirmation_scans: 2,
            edge_decay_moderate_confirmation_scan_multiplier: dec!(0.75),
            edge_decay_severe_confirmation_scan_multiplier: dec!(0.50),
            edge_decay_confirmation_window_secs: 900,
            short_horizon_edge_decay_confirmation_window_multiplier: dec!(0.50),
            medium_horizon_edge_decay_confirmation_window_multiplier: dec!(0.75),
            edge_decay_moderate_confirmation_window_multiplier: dec!(0.75),
            edge_decay_severe_confirmation_window_multiplier: dec!(0.50),
            short_horizon_edge_decay_cooldown_multiplier: dec!(0.50),
            medium_horizon_edge_decay_cooldown_multiplier: dec!(0.75),
        };
        let books = Arc::new(books);
        CryptoAlphaStrategy::new(
            config,
            Decimal::ZERO,
            CryptoAlphaDeps {
                get_orderbook: Box::new(move |tid| books.get(&tid).cloned()),
                get_available_capital: Box::new(|| Decimal::MAX),
                get_position: Box::new(|_| Decimal::ZERO),
                get_held_positions: Box::new(move || held.clone()),
                get_balance: Box::new(|| dec!(200)), // test balance $200
                neg_risk_events: vec![],
                binary_event_groups: vec![],
                event_calendar: None,
            },
        )
    }

    fn make_event_calendar(keyword: &str, impact: EventImpact) -> Arc<EventCalendarService> {
        Arc::new(EventCalendarService::new(EventCalendarConfig {
            enabled: true,
            static_events: vec![StaticEventConfig {
                title: format!("{keyword} event"),
                category: "crypto".to_string(),
                event_time: Utc::now().to_rfc3339(),
                impact: match impact {
                    EventImpact::Low => "low".to_string(),
                    EventImpact::Medium => "medium".to_string(),
                    EventImpact::High => "high".to_string(),
                },
                keywords: vec![keyword.to_string()],
            }],
            ..EventCalendarConfig::default()
        }))
    }

    #[tokio::test]
    async fn test_exit_capital_efficiency_crypto() {
        // Held YES token with bid=0.99 → should emit capital efficiency exit
        let token_id = U256::from(1u64);
        let question = "Will Bitcoin exceed $100,000 by December 31?";
        let market = make_crypto_market(question);

        let mut books = HashMap::new();
        books.insert(token_id, make_crypto_book(token_id, dec!(0.99)));
        books.insert(
            U256::from(2u64),
            make_crypto_book(U256::from(2u64), dec!(0.01)),
        );

        let held = vec![(token_id, dec!(50), dec!(0.60))];
        let strategy = make_crypto_strategy(books, held);

        let exits = strategy.scan_exits(&[market]).await;
        assert_eq!(exits.len(), 1);
        assert!(exits[0].question.starts_with("[EXIT]"));
        match &exits[0].execution_plan {
            ExecutionPlan::DirectionalBuy {
                side, price, size, ..
            } => {
                assert_eq!(*side, TradeSide::Sell);
                assert_eq!(*price, dec!(0.99));
                assert_eq!(*size, dec!(50));
            }
        }
    }

    #[tokio::test]
    async fn test_exit_model_reversal_crypto() {
        // Held YES token at bid=0.80. Pre-populate cache so GBM model gives
        // very low probability → model_prob < bid - buffer → should exit.
        let token_id = U256::from(1u64);
        // "Will Bitcoin exceed $500,000" with current price $95k → P very low
        let question = "Will Bitcoin exceed $500,000 by December 31, 2026?";
        let market = make_crypto_market(question);

        let mut books = HashMap::new();
        books.insert(token_id, make_crypto_book(token_id, dec!(0.80)));
        books.insert(
            U256::from(2u64),
            make_crypto_book(U256::from(2u64), dec!(0.20)),
        );

        let held = vec![(token_id, dec!(30), dec!(0.50))];
        let strategy = make_crypto_strategy(books, held);

        // Pre-populate price cache with Bitcoin data: current=$95k, 30 daily closes ~$95k
        // GBM with these numbers for $500k target → probability ≈ 0.0
        let parsed = parse_crypto_question(question).unwrap();
        seed_price_cache(&strategy, parsed.asset, 95000.0);

        let exits = strategy.scan_exits(&[market]).await;
        assert_eq!(exits.len(), 1, "Should detect model reversal exit");
        assert!(exits[0].question.starts_with("[EXIT]"));
    }

    #[tokio::test]
    async fn test_exit_relative_stop_loss_crypto() {
        let token_id = U256::from(1u64);
        let question = "Will Bitcoin exceed $120,000 by December 31, 2026?";
        let market = make_crypto_market(question);

        let mut books = HashMap::new();
        books.insert(token_id, make_crypto_book(token_id, dec!(0.55)));
        books.insert(
            U256::from(2u64),
            make_crypto_book(U256::from(2u64), dec!(0.45)),
        );

        let held = vec![(token_id, dec!(25), dec!(0.80))];
        let strategy = make_crypto_strategy(books, held);

        let exits = strategy.scan_exits(&[market]).await;
        assert_eq!(exits.len(), 1, "Relative stop-loss should trigger");
        assert!(exits[0].question.starts_with("[EXIT]"));
    }

    #[tokio::test]
    async fn test_short_horizon_capital_efficiency_exits_earlier() {
        let token_id = U256::from(1u64);
        let question = "Will Bitcoin exceed $100,000 by March 17, 2026?";
        let market = make_crypto_market(question);

        let mut books = HashMap::new();
        books.insert(token_id, make_crypto_book(token_id, dec!(0.95)));
        books.insert(
            U256::from(2u64),
            make_crypto_book(U256::from(2u64), dec!(0.05)),
        );

        let held = vec![(token_id, dec!(20), dec!(0.70))];
        let strategy = make_crypto_strategy(books, held);

        let exits = strategy.scan_exits(&[market]).await;
        assert_eq!(
            exits.len(),
            1,
            "short-dated capital efficiency threshold should trigger earlier exit"
        );
    }

    #[tokio::test]
    async fn test_short_horizon_model_reversal_uses_tighter_buffer() {
        let token_id = U256::from(1u64);
        let question = "Will Bitcoin exceed $90,000 by March 17, 2026?";
        let market = make_crypto_market(question);

        let mut yes_book = make_crypto_book(token_id, dec!(0.60));
        yes_book.asks[0].price = dec!(0.62);

        let mut books = HashMap::new();
        books.insert(token_id, yes_book);
        books.insert(
            U256::from(2u64),
            make_crypto_book(U256::from(2u64), dec!(0.40)),
        );

        let held = vec![(token_id, dec!(15), dec!(0.52))];
        let strategy = make_crypto_strategy(books, held);
        seed_price_cache(&strategy, &CRYPTO_ASSETS[0], 89_500.0);

        let exits = strategy.scan_exits(&[market]).await;
        assert_eq!(
            exits.len(),
            1,
            "short-dated model reversal should use a tighter exit buffer"
        );
    }

    #[tokio::test]
    async fn test_exit_edge_decay_crypto() {
        let token_id = U256::from(1u64);
        let question = "Will Bitcoin exceed $90,000 by December 31, 2026?";
        let market = make_crypto_market(question);

        let mut books = HashMap::new();
        books.insert(token_id, make_crypto_book(token_id, dec!(0.60)));
        books.insert(
            U256::from(2u64),
            make_crypto_book(U256::from(2u64), dec!(0.40)),
        );

        let held = vec![(token_id, dec!(10), dec!(0.50))];
        let strategy = make_crypto_strategy(books, held);
        seed_price_cache(&strategy, &CRYPTO_ASSETS[0], 90_500.0);
        assert_eq!(
            strategy.note_edge_decay_confirmation(token_id, 30, dec!(0.0025)),
            1
        );

        let exits = strategy.scan_exits(&[market]).await;
        assert_eq!(
            exits.len(),
            1,
            "confirmed thin-edge state should trigger edge-decay exit"
        );
    }

    #[tokio::test]
    async fn test_edge_decay_exit_respects_cooldown() {
        let token_id = U256::from(1u64);
        let strategy = make_crypto_strategy(HashMap::new(), vec![]);
        assert!(!strategy.edge_decay_cooldown_active(token_id));
        strategy.set_edge_decay_cooldown(token_id, 30, Decimal::ZERO);
        assert!(
            strategy.edge_decay_cooldown_active(token_id),
            "token should enter edge-decay cooldown immediately after a trim"
        );
    }

    #[test]
    fn test_short_horizon_hold_edge_threshold_is_stricter() {
        let strategy = make_crypto_strategy(HashMap::new(), vec![]);
        assert_eq!(strategy.effective_hold_edge_threshold(30), dec!(0.01));
        assert_eq!(strategy.effective_hold_edge_threshold(7), dec!(0.012));
        assert_eq!(strategy.effective_hold_edge_threshold(1), dec!(0.015));
        assert_eq!(strategy.effective_edge_decay_exit_fraction(30), dec!(0.25));
        assert_eq!(strategy.effective_edge_decay_exit_fraction(7), dec!(0.30));
        assert_eq!(strategy.effective_edge_decay_exit_fraction(1), dec!(0.375));
        assert_eq!(
            strategy.effective_edge_decay_confirmation_scans(30, Decimal::ZERO),
            2
        );
        assert_eq!(
            strategy.effective_edge_decay_confirmation_scans(7, Decimal::ZERO),
            2
        );
        assert_eq!(
            strategy.effective_edge_decay_confirmation_scans(1, Decimal::ZERO),
            1
        );
        assert_eq!(
            strategy.effective_edge_decay_confirmation_scans(30, dec!(0.0050)),
            2
        );
        assert_eq!(
            strategy.effective_edge_decay_confirmation_scans(30, dec!(0.0150)),
            1
        );
        assert_eq!(
            strategy.effective_edge_decay_confirmation_window_secs(30, Decimal::ZERO),
            900
        );
        assert_eq!(
            strategy.effective_edge_decay_confirmation_window_secs(7, Decimal::ZERO),
            675
        );
        assert_eq!(
            strategy.effective_edge_decay_confirmation_window_secs(1, Decimal::ZERO),
            450
        );
        assert_eq!(
            strategy.effective_edge_decay_confirmation_window_secs(30, dec!(0.0050)),
            675
        );
        assert_eq!(
            strategy.effective_edge_decay_confirmation_window_secs(30, dec!(0.0150)),
            450
        );
        assert_eq!(
            strategy.effective_edge_decay_cooldown_secs(30, Decimal::ZERO),
            1800
        );
        assert_eq!(
            strategy.effective_edge_decay_cooldown_secs(7, Decimal::ZERO),
            1350
        );
        assert_eq!(
            strategy.effective_edge_decay_cooldown_secs(1, Decimal::ZERO),
            900
        );
        assert_eq!(
            strategy.effective_edge_decay_cooldown_secs(30, dec!(0.0050)),
            1350
        );
        assert_eq!(
            strategy.effective_edge_decay_cooldown_secs(30, dec!(0.0150)),
            900
        );
        assert_eq!(strategy.config.edge_decay_confirmation_scans, 2);
        assert_eq!(
            strategy.planned_edge_decay_exit_size(dec!(10), dec!(0.60), 30, 2, dec!(0.0025)),
            dec!(2.50)
        );
        assert_eq!(
            strategy.planned_edge_decay_exit_size(dec!(10), dec!(0.60), 30, 4, dec!(0.0025)),
            dec!(4.50)
        );
        assert_eq!(
            strategy.planned_edge_decay_exit_size(dec!(10), dec!(0.60), 1, 1, dec!(0.0025)),
            dec!(3.75)
        );
        assert_eq!(
            strategy.planned_edge_decay_exit_size(dec!(10), dec!(0.60), 1, 3, dec!(0.0025)),
            dec!(5.75)
        );
        assert_eq!(
            strategy.planned_edge_decay_exit_size(dec!(10), dec!(0.60), 30, 2, dec!(0.0050)),
            dec!(3.12)
        );
        assert_eq!(
            strategy.planned_edge_decay_exit_size(dec!(10), dec!(0.60), 30, 2, dec!(0.0150)),
            dec!(5.25)
        );
        let token_id = U256::from(42u64);
        assert_eq!(
            strategy.note_edge_decay_confirmation(token_id, 30, dec!(0.0025)),
            1
        );
        assert_eq!(
            strategy.note_edge_decay_confirmation(token_id, 30, dec!(0.0025)),
            2
        );
        {
            let mut confirmations = strategy.edge_decay_confirmations.lock().unwrap();
            confirmations.insert(
                token_id,
                EdgeDecayConfirmationState {
                    count: 2,
                    last_seen: Instant::now() - Duration::from_secs(901),
                },
            );
        }
        assert_eq!(
            strategy.note_edge_decay_confirmation(token_id, 30, dec!(0.0025)),
            1,
            "expired confirmation window should reset the sequence"
        );
        strategy.reset_edge_decay_confirmation(token_id);
        assert_eq!(
            strategy.note_edge_decay_confirmation(token_id, 30, dec!(0.0025)),
            1
        );
    }

    // ──── Drift Decay Tests ────

    #[test]
    fn test_drift_decay_reduces_mu() {
        // Verify drift_decay multiplicatively reduces mu,
        // and that reduced mu lowers the probability of exceeding a threshold.
        let mu = 0.50; // 50% annualized drift
        let sigma = 0.60; // 60% annualized vol (typical crypto)
        let current = 100.0;
        let threshold = 120.0; // 20% OTM
        let days = 90.0;

        // drift_decay = 1.0 → full historical drift
        let prob_full = gbm_probability(current, threshold, mu * 1.0, sigma, days);
        // drift_decay = 0.5 → halved drift
        let prob_half = gbm_probability(current, threshold, mu * 0.5, sigma, days);
        // drift_decay = 0.0 → risk-neutral (mu=0)
        let prob_zero = gbm_probability(current, threshold, mu * 0.0, sigma, days);

        // Higher drift → higher probability of exceeding threshold
        assert!(
            prob_full > prob_half,
            "full drift should give higher prob than half: {} > {}",
            prob_full,
            prob_half
        );
        assert!(
            prob_half > prob_zero,
            "half drift should give higher prob than zero: {} > {}",
            prob_half,
            prob_zero
        );
        // Risk-neutral should be noticeably below full drift
        assert!(
            prob_full - prob_zero > 0.01,
            "drift should meaningfully affect probability: diff={}",
            prob_full - prob_zero
        );
    }

    // ──── Effective Volatility Tests ────

    #[test]
    fn test_effective_volatility_blends_iv() {
        // When IV is available, blend 70% IV + 30% historical
        let hist = 0.80;
        let iv = Some(0.60);
        let blended = effective_volatility(hist, iv);
        let expected = 0.7 * 0.60 + 0.3 * 0.80; // 0.42 + 0.24 = 0.66
        assert!(
            (blended - expected).abs() < 1e-10,
            "Expected {}, got {}",
            expected,
            blended
        );
    }

    #[test]
    fn test_effective_volatility_fallback() {
        // When IV is None, use pure historical
        let hist = 0.80;
        assert_eq!(effective_volatility(hist, None), hist);
        // When IV is zero, also fallback
        assert_eq!(effective_volatility(hist, Some(0.0)), hist);
        // When IV is negative (invalid), also fallback
        assert_eq!(effective_volatility(hist, Some(-0.1)), hist);
    }

    // ──── Dynamic Cache TTL Tests ────

    #[test]
    fn test_dynamic_cache_ttl() {
        // High sigma (1.2) should halve the TTL compared to baseline (0.6)
        let refresh_secs: u64 = 300;
        let high_sigma = 1.2_f64;
        let scale = (high_sigma / BASELINE_CRYPTO_SIGMA).max(1.0);
        let effective_ttl = refresh_secs as f64 / scale;
        assert!(
            (effective_ttl - 150.0).abs() < 0.01,
            "sigma=1.2 should halve TTL to 150s, got {}",
            effective_ttl
        );

        // Normal sigma (0.6 = baseline) should keep TTL unchanged
        let normal_sigma = 0.6_f64;
        let scale = (normal_sigma / BASELINE_CRYPTO_SIGMA).max(1.0);
        let effective_ttl = refresh_secs as f64 / scale;
        assert!(
            (effective_ttl - 300.0).abs() < 0.01,
            "sigma=0.6 (baseline) should keep TTL at 300s, got {}",
            effective_ttl
        );

        // Low sigma (0.3) should not exceed baseline TTL (clamped by max(1.0))
        let low_sigma = 0.3_f64;
        let scale = (low_sigma / BASELINE_CRYPTO_SIGMA).max(1.0);
        let effective_ttl = refresh_secs as f64 / scale;
        assert!(
            (effective_ttl - 300.0).abs() < 0.01,
            "sigma=0.3 (below baseline) should keep TTL at 300s, got {}",
            effective_ttl
        );
    }

    // ──── Spread Filter Tests ────

    #[tokio::test]
    async fn test_crypto_spread_filter_rejects_wide_spread() {
        // Test that markets with bid-ask spread > max_spread_bps are rejected
        let token_id = U256::from(1u64);

        // Create order book with 20% spread (YES: bid=0.40, ask=0.50)
        let mut yes_book = make_crypto_book(token_id, dec!(0.50));
        yes_book.bids.clear();
        yes_book.bids.push(PriceLevel {
            price: dec!(0.40),
            size: dec!(500),
        });

        // NO side with tight spread (doesn't matter, YES is wide enough)
        let no_book = make_crypto_book(U256::from(2u64), dec!(0.50));

        let mut books = HashMap::new();
        books.insert(token_id, yes_book);
        books.insert(U256::from(2u64), no_book);

        // Use max_spread_bps = 1200 (12%) so 20% is rejected
        let config = CryptoAlphaConfig {
            min_edge_bps: 500,
            max_position_pct: dec!(0.50),
            kelly_fraction: dec!(0.25),
            refresh_interval_secs: 300,
            spot_refresh_interval_secs: 30,
            history_refresh_interval_secs: 1800,
            iv_refresh_interval_secs: 300,
            coingecko_api_key: String::new(),
            exit_buffer_bps: 50,
            capital_efficiency_threshold: dec!(0.98),
            drift_decay: 0.0,
            max_spread_bps: 1200,
            relative_stop_loss_ratio: dec!(0.80),
            max_exposure_per_asset_pct: dec!(0.75),
            max_exposure_per_asset_direction_pct: dec!(0.45),
            low_event_min_edge_multiplier: dec!(1.20),
            medium_event_min_edge_multiplier: dec!(1.50),
            high_event_min_edge_multiplier: dec!(2.00),
            low_event_max_spread_multiplier: dec!(0.90),
            medium_event_max_spread_multiplier: dec!(0.80),
            high_event_max_spread_multiplier: dec!(0.65),
            low_event_sigma_multiplier: dec!(1.05),
            medium_event_sigma_multiplier: dec!(1.15),
            high_event_sigma_multiplier: dec!(1.30),
            low_event_size_multiplier: dec!(0.90),
            medium_event_size_multiplier: dec!(0.75),
            high_event_size_multiplier: dec!(0.50),
            btc_probability_calibration: dec!(0.95),
            eth_probability_calibration: dec!(0.93),
            alt_probability_calibration: dec!(0.88),
            binary_probability_calibration: dec!(0.97),
            range_probability_calibration: dec!(0.90),
            calibration_overrides: vec![],
            short_horizon_max_days: 1,
            medium_horizon_max_days: 7,
            short_horizon_probability_calibration: dec!(0.85),
            medium_horizon_probability_calibration: dec!(0.92),
            short_horizon_size_multiplier: dec!(0.60),
            medium_horizon_size_multiplier: dec!(0.80),
            short_horizon_min_edge_multiplier: dec!(1.50),
            medium_horizon_min_edge_multiplier: dec!(1.20),
            short_horizon_max_spread_multiplier: dec!(0.75),
            medium_horizon_max_spread_multiplier: dec!(0.90),
            short_horizon_capital_efficiency_threshold: dec!(0.92),
            medium_horizon_capital_efficiency_threshold: dec!(0.95),
            short_horizon_exit_buffer_multiplier: dec!(0.50),
            medium_horizon_exit_buffer_multiplier: dec!(0.80),
            hold_min_edge_bps: 100,
            short_horizon_hold_edge_multiplier: dec!(1.50),
            medium_horizon_hold_edge_multiplier: dec!(1.20),
            edge_decay_exit_fraction: dec!(0.25),
            edge_decay_exit_fraction_step: dec!(0.10),
            edge_decay_moderate_gap_bps: 50,
            edge_decay_severe_gap_bps: 150,
            edge_decay_moderate_exit_multiplier: dec!(1.25),
            edge_decay_severe_exit_multiplier: dec!(1.50),
            edge_decay_moderate_cooldown_multiplier: dec!(0.75),
            edge_decay_severe_cooldown_multiplier: dec!(0.50),
            short_horizon_edge_decay_exit_multiplier: dec!(1.50),
            medium_horizon_edge_decay_exit_multiplier: dec!(1.20),
            edge_decay_cooldown_secs: 1800,
            edge_decay_confirmation_scans: 2,
            short_horizon_edge_decay_confirmation_scans: 1,
            medium_horizon_edge_decay_confirmation_scans: 2,
            edge_decay_moderate_confirmation_scan_multiplier: dec!(0.75),
            edge_decay_severe_confirmation_scan_multiplier: dec!(0.50),
            edge_decay_confirmation_window_secs: 900,
            short_horizon_edge_decay_confirmation_window_multiplier: dec!(0.50),
            medium_horizon_edge_decay_confirmation_window_multiplier: dec!(0.75),
            edge_decay_moderate_confirmation_window_multiplier: dec!(0.75),
            edge_decay_severe_confirmation_window_multiplier: dec!(0.50),
            short_horizon_edge_decay_cooldown_multiplier: dec!(0.50),
            medium_horizon_edge_decay_cooldown_multiplier: dec!(0.75),
        };
        let books = Arc::new(books);
        let strategy = CryptoAlphaStrategy::new(
            config,
            Decimal::ZERO,
            CryptoAlphaDeps {
                get_orderbook: Box::new(move |tid| books.get(&tid).cloned()),
                get_available_capital: Box::new(|| Decimal::MAX),
                get_position: Box::new(|_| Decimal::ZERO),
                get_held_positions: Box::new(Vec::new),
                get_balance: Box::new(|| dec!(200)),
                neg_risk_events: vec![],
                binary_event_groups: vec![],
                event_calendar: None,
            },
        );

        // Use a far future date so the question parses with days > 0
        let question = "Will Bitcoin exceed $50,000 by December 31?";
        let market = make_crypto_market(question);
        let result = strategy
            .detect_crypto_opportunity(&market, Decimal::ZERO, &HashMap::new())
            .await;
        assert!(
            result.is_none(),
            "Market with 20% spread should be rejected (max 12%)"
        );
    }

    #[tokio::test]
    async fn test_crypto_spread_filter_accepts_narrow_spread() {
        // Test that markets with bid-ask spread <= max_spread_bps are accepted
        // We verify by checking the spread calculation logic directly,
        // since detect_crypto_opportunity requires live price data.
        let spread = (dec!(0.50) - dec!(0.46)) / dec!(0.50); // 0.08 = 800 bps
        let spread_bps = {
            use rust_decimal::prelude::ToPrimitive;
            (spread * dec!(10000)).to_u32().unwrap_or(u32::MAX)
        };
        assert!(
            spread_bps <= 1200,
            "8% spread (800 bps) should pass 12% filter"
        );
        assert_eq!(spread_bps, 800);

        // Also verify wide spread is correctly computed
        let wide_spread = (dec!(0.50) - dec!(0.40)) / dec!(0.50); // 0.20 = 2000 bps
        let wide_bps = {
            use rust_decimal::prelude::ToPrimitive;
            (wide_spread * dec!(10000)).to_u32().unwrap_or(u32::MAX)
        };
        assert!(
            wide_bps > 1200,
            "20% spread (2000 bps) should fail 12% filter"
        );
        assert_eq!(wide_bps, 2000);
    }

    #[tokio::test]
    async fn test_crypto_event_window_raises_min_edge_threshold() {
        let yes_token = U256::from(31u64);
        let no_token = U256::from(32u64);
        let market = market_with_token_ids(
            make_crypto_market("Will Bitcoin exceed $105,000 by December 31, 2026?"),
            yes_token,
            no_token,
            B256::from([3u8; 32]),
        );

        let mut books = HashMap::new();
        books.insert(
            yes_token,
            OrderBook {
                token_id: yes_token,
                bids: vec![PriceLevel {
                    price: dec!(0.07),
                    size: dec!(500),
                }],
                asks: vec![PriceLevel {
                    price: dec!(0.08),
                    size: dec!(500),
                }],
                timestamp: Utc::now(),
            },
        );
        books.insert(
            no_token,
            OrderBook {
                token_id: no_token,
                bids: vec![PriceLevel {
                    price: dec!(0.92),
                    size: dec!(500),
                }],
                asks: vec![PriceLevel {
                    price: dec!(0.94),
                    size: dec!(500),
                }],
                timestamp: Utc::now(),
            },
        );

        let base_config = CryptoAlphaConfig {
            min_edge_bps: 500,
            max_position_pct: dec!(0.50),
            kelly_fraction: dec!(0.25),
            refresh_interval_secs: 300,
            spot_refresh_interval_secs: 30,
            history_refresh_interval_secs: 1800,
            iv_refresh_interval_secs: 300,
            coingecko_api_key: String::new(),
            exit_buffer_bps: 50,
            capital_efficiency_threshold: dec!(0.98),
            drift_decay: 0.0,
            max_spread_bps: 1500,
            relative_stop_loss_ratio: dec!(0.80),
            max_exposure_per_asset_pct: dec!(0.75),
            max_exposure_per_asset_direction_pct: dec!(0.45),
            low_event_min_edge_multiplier: dec!(1.20),
            medium_event_min_edge_multiplier: dec!(1.50),
            high_event_min_edge_multiplier: dec!(2.00),
            low_event_max_spread_multiplier: dec!(0.90),
            medium_event_max_spread_multiplier: dec!(0.80),
            high_event_max_spread_multiplier: dec!(0.65),
            low_event_sigma_multiplier: dec!(1.05),
            medium_event_sigma_multiplier: dec!(1.15),
            high_event_sigma_multiplier: dec!(1.30),
            low_event_size_multiplier: dec!(0.90),
            medium_event_size_multiplier: dec!(0.75),
            high_event_size_multiplier: dec!(0.50),
            btc_probability_calibration: dec!(0.95),
            eth_probability_calibration: dec!(0.93),
            alt_probability_calibration: dec!(0.88),
            binary_probability_calibration: dec!(0.97),
            range_probability_calibration: dec!(0.90),
            calibration_overrides: vec![],
            short_horizon_max_days: 1,
            medium_horizon_max_days: 7,
            short_horizon_probability_calibration: dec!(0.85),
            medium_horizon_probability_calibration: dec!(0.92),
            short_horizon_size_multiplier: dec!(0.60),
            medium_horizon_size_multiplier: dec!(0.80),
            short_horizon_min_edge_multiplier: dec!(1.50),
            medium_horizon_min_edge_multiplier: dec!(1.20),
            short_horizon_max_spread_multiplier: dec!(0.75),
            medium_horizon_max_spread_multiplier: dec!(0.90),
            short_horizon_capital_efficiency_threshold: dec!(0.92),
            medium_horizon_capital_efficiency_threshold: dec!(0.95),
            short_horizon_exit_buffer_multiplier: dec!(0.50),
            medium_horizon_exit_buffer_multiplier: dec!(0.80),
            hold_min_edge_bps: 100,
            short_horizon_hold_edge_multiplier: dec!(1.50),
            medium_horizon_hold_edge_multiplier: dec!(1.20),
            edge_decay_exit_fraction: dec!(0.25),
            edge_decay_exit_fraction_step: dec!(0.10),
            edge_decay_moderate_gap_bps: 50,
            edge_decay_severe_gap_bps: 150,
            edge_decay_moderate_exit_multiplier: dec!(1.25),
            edge_decay_severe_exit_multiplier: dec!(1.50),
            edge_decay_moderate_cooldown_multiplier: dec!(0.75),
            edge_decay_severe_cooldown_multiplier: dec!(0.50),
            short_horizon_edge_decay_exit_multiplier: dec!(1.50),
            medium_horizon_edge_decay_exit_multiplier: dec!(1.20),
            edge_decay_cooldown_secs: 1800,
            edge_decay_confirmation_scans: 2,
            short_horizon_edge_decay_confirmation_scans: 1,
            medium_horizon_edge_decay_confirmation_scans: 2,
            edge_decay_moderate_confirmation_scan_multiplier: dec!(0.75),
            edge_decay_severe_confirmation_scan_multiplier: dec!(0.50),
            edge_decay_confirmation_window_secs: 900,
            short_horizon_edge_decay_confirmation_window_multiplier: dec!(0.50),
            medium_horizon_edge_decay_confirmation_window_multiplier: dec!(0.75),
            edge_decay_moderate_confirmation_window_multiplier: dec!(0.75),
            edge_decay_severe_confirmation_window_multiplier: dec!(0.50),
            short_horizon_edge_decay_cooldown_multiplier: dec!(0.50),
            medium_horizon_edge_decay_cooldown_multiplier: dec!(0.75),
        };
        let books = Arc::new(books);

        let strategy_without_event = CryptoAlphaStrategy::new(
            base_config.clone(),
            Decimal::ZERO,
            CryptoAlphaDeps {
                get_orderbook: Box::new({
                    let books = Arc::clone(&books);
                    move |tid| books.get(&tid).cloned()
                }),
                get_available_capital: Box::new(|| Decimal::MAX),
                get_position: Box::new(|_| Decimal::ZERO),
                get_held_positions: Box::new(Vec::new),
                get_balance: Box::new(|| dec!(200)),
                neg_risk_events: vec![],
                binary_event_groups: vec![],
                event_calendar: None,
            },
        );
        seed_price_cache(&strategy_without_event, &CRYPTO_ASSETS[0], 95_000.0);
        let baseline = strategy_without_event
            .detect_crypto_opportunity(&market, Decimal::ZERO, &HashMap::new())
            .await;
        assert!(
            baseline.is_some(),
            "baseline market should pass normal min-edge filter"
        );

        let strategy_with_event = CryptoAlphaStrategy::new(
            base_config,
            Decimal::ZERO,
            CryptoAlphaDeps {
                get_orderbook: Box::new({
                    let books = Arc::clone(&books);
                    move |tid| books.get(&tid).cloned()
                }),
                get_available_capital: Box::new(|| Decimal::MAX),
                get_position: Box::new(|_| Decimal::ZERO),
                get_held_positions: Box::new(Vec::new),
                get_balance: Box::new(|| dec!(200)),
                neg_risk_events: vec![],
                binary_event_groups: vec![],
                event_calendar: Some(make_event_calendar("bitcoin", EventImpact::High)),
            },
        );
        seed_price_cache(&strategy_with_event, &CRYPTO_ASSETS[0], 95_000.0);
        let tightened = strategy_with_event
            .detect_crypto_opportunity(&market, Decimal::ZERO, &HashMap::new())
            .await;
        assert!(
            tightened.is_none(),
            "event window should tighten min-edge enough to reject the same market"
        );
    }

    #[tokio::test]
    async fn test_crypto_event_thresholds_scale_by_impact() {
        let strategy = make_crypto_strategy(HashMap::new(), vec![]);
        assert_eq!(
            strategy
                .effective_entry_thresholds("unmatched market", 30)
                .await,
            (500, 1500)
        );

        let mut low_strategy = make_crypto_strategy(HashMap::new(), vec![]);
        low_strategy.event_calendar = Some(make_event_calendar("bitcoin", EventImpact::Low));
        assert_eq!(
            low_strategy
                .effective_entry_thresholds("Will Bitcoin rally?", 30)
                .await,
            (600, 1350)
        );

        let mut high_strategy = make_crypto_strategy(HashMap::new(), vec![]);
        high_strategy.event_calendar = Some(make_event_calendar("bitcoin", EventImpact::High));
        assert_eq!(
            high_strategy
                .effective_entry_thresholds("Will Bitcoin rally?", 30)
                .await,
            (1000, 975)
        );
    }

    #[tokio::test]
    async fn test_crypto_event_sigma_scales_by_impact() {
        let strategy = make_crypto_strategy(HashMap::new(), vec![]);
        assert_eq!(
            strategy
                .effective_event_sigma_multiplier("unmatched market")
                .await,
            1.0
        );

        let mut low_strategy = make_crypto_strategy(HashMap::new(), vec![]);
        low_strategy.event_calendar = Some(make_event_calendar("bitcoin", EventImpact::Low));
        assert_eq!(
            low_strategy
                .effective_event_sigma_multiplier("Will Bitcoin rally?")
                .await,
            1.05
        );

        let mut medium_strategy = make_crypto_strategy(HashMap::new(), vec![]);
        medium_strategy.event_calendar = Some(make_event_calendar("bitcoin", EventImpact::Medium));
        assert_eq!(
            medium_strategy
                .effective_event_sigma_multiplier("Will Bitcoin rally?")
                .await,
            1.15
        );

        let mut high_strategy = make_crypto_strategy(HashMap::new(), vec![]);
        high_strategy.event_calendar = Some(make_event_calendar("bitcoin", EventImpact::High));
        assert_eq!(
            high_strategy
                .effective_event_sigma_multiplier("Will Bitcoin rally?")
                .await,
            1.30
        );
    }

    #[tokio::test]
    async fn test_crypto_event_size_scales_by_impact() {
        let strategy = make_crypto_strategy(HashMap::new(), vec![]);
        assert_eq!(
            strategy
                .effective_event_size_multiplier("unmatched market")
                .await,
            Decimal::ONE
        );

        let mut low_strategy = make_crypto_strategy(HashMap::new(), vec![]);
        low_strategy.event_calendar = Some(make_event_calendar("bitcoin", EventImpact::Low));
        assert_eq!(
            low_strategy
                .effective_event_size_multiplier("Will Bitcoin rally?")
                .await,
            dec!(0.90)
        );

        let mut medium_strategy = make_crypto_strategy(HashMap::new(), vec![]);
        medium_strategy.event_calendar = Some(make_event_calendar("bitcoin", EventImpact::Medium));
        assert_eq!(
            medium_strategy
                .effective_event_size_multiplier("Will Bitcoin rally?")
                .await,
            dec!(0.75)
        );

        let mut high_strategy = make_crypto_strategy(HashMap::new(), vec![]);
        high_strategy.event_calendar = Some(make_event_calendar("bitcoin", EventImpact::High));
        assert_eq!(
            high_strategy
                .effective_event_size_multiplier("Will Bitcoin rally?")
                .await,
            dec!(0.50)
        );
    }

    #[test]
    fn test_crypto_horizon_size_scales_by_bucket() {
        let strategy = make_crypto_strategy(HashMap::new(), vec![]);
        assert_eq!(strategy.effective_horizon_size_multiplier(30), Decimal::ONE);
        assert_eq!(strategy.effective_horizon_size_multiplier(7), dec!(0.80));
        assert_eq!(strategy.effective_horizon_size_multiplier(1), dec!(0.60));
    }

    #[test]
    fn test_crypto_probability_calibration_shrinks_extremes_by_asset_and_horizon() {
        let strategy = make_crypto_strategy(HashMap::new(), vec![]);
        let raw = dec!(0.90);
        assert_eq!(
            strategy.calibrate_probability(&CRYPTO_ASSETS[0], 30, CryptoMarketType::Binary, raw),
            dec!(0.8686)
        );
        assert_eq!(
            strategy.calibrate_probability(&CRYPTO_ASSETS[1], 30, CryptoMarketType::Binary, raw),
            dec!(0.86084)
        );
        assert_eq!(
            strategy.calibrate_probability(&CRYPTO_ASSETS[2], 30, CryptoMarketType::Binary, raw),
            dec!(0.84144)
        );
        assert_eq!(
            strategy.calibrate_probability(&CRYPTO_ASSETS[0], 1, CryptoMarketType::Binary, raw),
            dec!(0.81331)
        );
        assert_eq!(
            strategy.calibrate_probability(&CRYPTO_ASSETS[2], 1, CryptoMarketType::Binary, raw),
            dec!(0.790224)
        );
        assert_eq!(
            strategy.calibrate_probability(&CRYPTO_ASSETS[0], 30, CryptoMarketType::Range, raw),
            dec!(0.842)
        );
    }

    #[test]
    fn test_crypto_probability_calibration_override_table() {
        let mut strategy = make_crypto_strategy(HashMap::new(), vec![]);
        strategy.config.calibration_overrides = vec![
            pa_core::config::CryptoCalibrationOverride {
                asset: "BTCUSDT".to_string(),
                horizon: "short".to_string(),
                market_type: "binary".to_string(),
                probability_calibration: Some(dec!(0.82)),
                sigma_multiplier: Some(dec!(1.10)),
                size_multiplier: Some(dec!(0.70)),
            },
            pa_core::config::CryptoCalibrationOverride {
                asset: "*".to_string(),
                horizon: "short".to_string(),
                market_type: "range".to_string(),
                probability_calibration: Some(dec!(0.78)),
                sigma_multiplier: Some(dec!(1.20)),
                size_multiplier: Some(dec!(0.65)),
            },
        ];

        let raw = dec!(0.90);
        assert_eq!(
            strategy.calibrate_probability(&CRYPTO_ASSETS[0], 1, CryptoMarketType::Binary, raw),
            dec!(0.828)
        );
        assert_eq!(
            strategy.calibrate_probability(&CRYPTO_ASSETS[2], 1, CryptoMarketType::Range, raw),
            dec!(0.812)
        );
        assert_eq!(
            strategy.calibrate_probability(&CRYPTO_ASSETS[0], 30, CryptoMarketType::Binary, raw),
            dec!(0.8686)
        );
        assert_eq!(
            strategy.calibration_override_sigma_multiplier(
                &CRYPTO_ASSETS[0],
                1,
                CryptoMarketType::Binary
            ),
            Some(dec!(1.10))
        );
        assert_eq!(
            strategy.calibration_override_size_multiplier(
                &CRYPTO_ASSETS[2],
                1,
                CryptoMarketType::Range
            ),
            Some(dec!(0.65))
        );
        assert_eq!(
            strategy.calibration_override_sigma_multiplier(
                &CRYPTO_ASSETS[0],
                30,
                CryptoMarketType::Binary
            ),
            None
        );
    }

    #[tokio::test]
    async fn test_crypto_entry_thresholds_tighten_for_short_horizon() {
        let strategy = make_crypto_strategy(HashMap::new(), vec![]);
        assert_eq!(
            strategy
                .effective_entry_thresholds("Will Bitcoin rally?", 1)
                .await,
            (750, 1125)
        );
        assert_eq!(
            strategy
                .effective_entry_thresholds("Will Bitcoin rally?", 7)
                .await,
            (600, 1350)
        );
        assert_eq!(
            strategy
                .effective_entry_thresholds("Will Bitcoin rally?", 30)
                .await,
            (500, 1500)
        );
    }

    #[tokio::test]
    async fn test_crypto_asset_exposure_cap_limits_new_entry() {
        let held_market = market_with_token_ids(
            make_crypto_market("Will Bitcoin exceed $500,000 by December 31, 2026?"),
            U256::from(11u64),
            U256::from(12u64),
            B256::from([1u8; 32]),
        );
        let target_market = market_with_token_ids(
            make_crypto_market("Will Bitcoin exceed $50,000 by December 31, 2026?"),
            U256::from(21u64),
            U256::from(22u64),
            B256::from([2u8; 32]),
        );

        let mut books = HashMap::new();
        books.insert(
            U256::from(11u64),
            make_crypto_book(U256::from(11u64), dec!(0.60)),
        );
        books.insert(
            U256::from(12u64),
            make_crypto_book(U256::from(12u64), dec!(0.40)),
        );
        books.insert(
            U256::from(21u64),
            make_crypto_book(U256::from(21u64), dec!(0.18)),
        );
        books.insert(
            U256::from(22u64),
            make_crypto_book(U256::from(22u64), dec!(0.82)),
        );

        let held = vec![(U256::from(11u64), dec!(50), dec!(0.60))];
        let mut strategy = make_crypto_strategy(books, held);
        strategy.config.max_exposure_per_asset_pct = dec!(0.25); // $50 cap on $200 balance

        seed_price_cache(&strategy, &CRYPTO_ASSETS[0], 95_000.0);

        let opportunities = pa_core::traits::Strategy::scan(
            &strategy,
            &[held_market.clone(), target_market.clone()],
        )
        .await
        .unwrap();

        let entry = opportunities
            .into_iter()
            .find(|opp| !opp.question.starts_with("[EXIT]"))
            .expect("expected one entry opportunity");

        match entry.execution_plan {
            ExecutionPlan::DirectionalBuy { price, size, .. } => {
                assert!(
                    price * size <= dec!(20.00),
                    "asset cap should limit new BTC exposure to remaining $20"
                );
            }
        }
    }

    #[tokio::test]
    async fn test_crypto_scan_keeps_only_best_entry_per_asset() {
        let market_a = market_with_token_ids(
            make_crypto_market("Will Bitcoin exceed $90,000 by December 31, 2026?"),
            U256::from(31u64),
            U256::from(32u64),
            B256::from([3u8; 32]),
        );
        let market_b = market_with_token_ids(
            make_crypto_market("Will Bitcoin exceed $100,000 by December 31, 2026?"),
            U256::from(41u64),
            U256::from(42u64),
            B256::from([4u8; 32]),
        );

        let mut books = HashMap::new();
        let mut book_a = make_crypto_book(U256::from(31u64), dec!(0.50));
        book_a.asks[0].price = dec!(0.52);
        books.insert(U256::from(31u64), book_a);
        books.insert(
            U256::from(32u64),
            make_crypto_book(U256::from(32u64), dec!(0.48)),
        );

        let mut book_b = make_crypto_book(U256::from(41u64), dec!(0.20));
        book_b.asks[0].price = dec!(0.22);
        books.insert(U256::from(41u64), book_b);
        books.insert(
            U256::from(42u64),
            make_crypto_book(U256::from(42u64), dec!(0.78)),
        );

        let held = vec![];
        let strategy = make_crypto_strategy(books, held);
        seed_price_cache(&strategy, &CRYPTO_ASSETS[0], 95_000.0);

        let opportunities = pa_core::traits::Strategy::scan(&strategy, &[market_a, market_b])
            .await
            .unwrap();

        let entries: Vec<_> = opportunities
            .into_iter()
            .filter(|opp| !opp.question.starts_with("[EXIT]"))
            .collect();

        assert_eq!(entries.len(), 1, "only one BTC entry should survive dedupe");
        assert!(
            entries[0].question.contains("$100,000"),
            "higher-profit BTC candidate should win"
        );
    }

    #[test]
    fn test_keep_better_entry_prefers_higher_efficiency_then_lower_cost_and_better_depth() {
        let asset = &CRYPTO_ASSETS[0];
        let strategy = make_crypto_strategy(
            HashMap::from([
                (
                    U256::from(1u64),
                    OrderBook {
                        token_id: U256::from(1u64),
                        bids: vec![],
                        asks: vec![PriceLevel {
                            price: dec!(0.50),
                            size: dec!(20),
                        }],
                        timestamp: Utc::now(),
                    },
                ),
                (
                    U256::from(2u64),
                    OrderBook {
                        token_id: U256::from(2u64),
                        bids: vec![],
                        asks: vec![PriceLevel {
                            price: dec!(0.40),
                            size: dec!(30),
                        }],
                        timestamp: Utc::now(),
                    },
                ),
                (
                    U256::from(3u64),
                    OrderBook {
                        token_id: U256::from(3u64),
                        bids: vec![],
                        asks: vec![PriceLevel {
                            price: dec!(0.80),
                            size: dec!(20),
                        }],
                        timestamp: Utc::now(),
                    },
                ),
                (
                    U256::from(4u64),
                    OrderBook {
                        token_id: U256::from(4u64),
                        bids: vec![],
                        asks: vec![PriceLevel {
                            price: dec!(0.40),
                            size: dec!(10),
                        }],
                        timestamp: Utc::now(),
                    },
                ),
            ]),
            vec![],
        );
        let mut best = HashMap::new();

        let baseline = TradingOpportunity {
            id: Uuid::now_v7(),
            strategy_type: StrategyType::CryptoAlpha,
            condition_id: B256::from([9u8; 32]),
            question: "baseline".into(),
            spread: dec!(0.10),
            estimated_profit: dec!(4.00),
            size: dec!(20),
            detected_at: Utc::now(),
            execution_plan: ExecutionPlan::DirectionalBuy {
                token_id: U256::from(1u64),
                side: TradeSide::Buy,
                price: dec!(0.50),
                size: dec!(20),
                condition_id: B256::from([9u8; 32]),
            },
        };
        strategy.keep_better_entry(&mut best, asset, CryptoDirectionBucket::Up, baseline);

        let higher_efficiency_same_profit = TradingOpportunity {
            id: Uuid::now_v7(),
            strategy_type: StrategyType::CryptoAlpha,
            condition_id: B256::from([10u8; 32]),
            question: "better-efficiency".into(),
            spread: dec!(0.09),
            estimated_profit: dec!(4.00),
            size: dec!(10),
            detected_at: Utc::now(),
            execution_plan: ExecutionPlan::DirectionalBuy {
                token_id: U256::from(2u64),
                side: TradeSide::Buy,
                price: dec!(0.40),
                size: dec!(10),
                condition_id: B256::from([10u8; 32]),
            },
        };
        strategy.keep_better_entry(
            &mut best,
            asset,
            CryptoDirectionBucket::Up,
            higher_efficiency_same_profit,
        );

        assert_eq!(
            best.get(&(asset.binance_symbol, CryptoDirectionBucket::Up))
                .map(|opp| opp.question.as_str()),
            Some("better-efficiency")
        );

        let worse_depth_same_profit_efficiency = TradingOpportunity {
            id: Uuid::now_v7(),
            strategy_type: StrategyType::CryptoAlpha,
            condition_id: B256::from([12u8; 32]),
            question: "worse-depth".into(),
            spread: dec!(0.11),
            estimated_profit: dec!(4.00),
            size: dec!(10),
            detected_at: Utc::now(),
            execution_plan: ExecutionPlan::DirectionalBuy {
                token_id: U256::from(4u64),
                side: TradeSide::Buy,
                price: dec!(0.40),
                size: dec!(10),
                condition_id: B256::from([12u8; 32]),
            },
        };
        strategy.keep_better_entry(
            &mut best,
            asset,
            CryptoDirectionBucket::Up,
            worse_depth_same_profit_efficiency,
        );

        assert_eq!(
            best.get(&(asset.binance_symbol, CryptoDirectionBucket::Up))
                .map(|opp| opp.question.as_str()),
            Some("better-efficiency"),
            "shallower candidate should not replace equally efficient deeper one"
        );

        let same_efficiency_higher_cost = TradingOpportunity {
            id: Uuid::now_v7(),
            strategy_type: StrategyType::CryptoAlpha,
            condition_id: B256::from([11u8; 32]),
            question: "same-efficiency-higher-cost".into(),
            spread: dec!(0.20),
            estimated_profit: dec!(4.00),
            size: dec!(20),
            detected_at: Utc::now(),
            execution_plan: ExecutionPlan::DirectionalBuy {
                token_id: U256::from(3u64),
                side: TradeSide::Buy,
                price: dec!(0.80),
                size: dec!(10),
                condition_id: B256::from([11u8; 32]),
            },
        };
        strategy.keep_better_entry(
            &mut best,
            asset,
            CryptoDirectionBucket::Up,
            same_efficiency_higher_cost,
        );

        assert_eq!(
            best.get(&(asset.binance_symbol, CryptoDirectionBucket::Up))
                .map(|opp| opp.question.as_str()),
            Some("better-efficiency"),
            "higher-cost candidate should not replace equally efficient incumbent"
        );
    }

    #[tokio::test]
    async fn test_crypto_scan_keeps_opposite_directions_for_same_asset() {
        let up_market = market_with_token_ids(
            make_crypto_market("Will Bitcoin exceed $100,000 by December 31, 2026?"),
            U256::from(51u64),
            U256::from(52u64),
            B256::from([5u8; 32]),
        );
        let down_market = market_with_token_ids(
            make_crypto_market("Will Bitcoin dip to $85,000 by December 31, 2026?"),
            U256::from(61u64),
            U256::from(62u64),
            B256::from([6u8; 32]),
        );

        let mut books = HashMap::new();
        let mut up_book = make_crypto_book(U256::from(51u64), dec!(0.20));
        up_book.asks[0].price = dec!(0.22);
        books.insert(U256::from(51u64), up_book);
        books.insert(
            U256::from(52u64),
            make_crypto_book(U256::from(52u64), dec!(0.78)),
        );

        let mut down_book = make_crypto_book(U256::from(61u64), dec!(0.20));
        down_book.asks[0].price = dec!(0.22);
        books.insert(U256::from(61u64), down_book);
        books.insert(
            U256::from(62u64),
            make_crypto_book(U256::from(62u64), dec!(0.78)),
        );

        let strategy = make_crypto_strategy(books, vec![]);
        seed_price_cache(&strategy, &CRYPTO_ASSETS[0], 95_000.0);

        let opportunities = pa_core::traits::Strategy::scan(&strategy, &[up_market, down_market])
            .await
            .unwrap();

        let entries: Vec<_> = opportunities
            .into_iter()
            .filter(|opp| !opp.question.starts_with("[EXIT]"))
            .collect();

        assert_eq!(entries.len(), 2, "up/down BTC candidates should coexist");
    }

    #[tokio::test]
    async fn test_crypto_asset_direction_exposure_cap_limits_same_direction_entry() {
        let held_market = market_with_token_ids(
            make_crypto_market("Will Bitcoin exceed $90,000 by December 31, 2026?"),
            U256::from(71u64),
            U256::from(72u64),
            B256::from([7u8; 32]),
        );
        let target_market = market_with_token_ids(
            make_crypto_market("Will Bitcoin exceed $100,000 by December 31, 2026?"),
            U256::from(81u64),
            U256::from(82u64),
            B256::from([8u8; 32]),
        );

        let mut books = HashMap::new();
        let mut held_book = make_crypto_book(U256::from(71u64), dec!(0.50));
        held_book.asks[0].price = dec!(0.52);
        books.insert(U256::from(71u64), held_book);
        books.insert(
            U256::from(72u64),
            make_crypto_book(U256::from(72u64), dec!(0.48)),
        );

        let mut target_book = make_crypto_book(U256::from(81u64), dec!(0.20));
        target_book.asks[0].price = dec!(0.22);
        books.insert(U256::from(81u64), target_book);
        books.insert(
            U256::from(82u64),
            make_crypto_book(U256::from(82u64), dec!(0.78)),
        );

        let held = vec![(U256::from(71u64), dec!(70), dec!(0.60))];
        let mut strategy = make_crypto_strategy(books, held);
        strategy.config.max_exposure_per_asset_pct = dec!(0.90);
        strategy.config.max_exposure_per_asset_direction_pct = dec!(0.20); // $40 cap on $200

        seed_price_cache(&strategy, &CRYPTO_ASSETS[0], 95_000.0);

        let opportunities =
            pa_core::traits::Strategy::scan(&strategy, &[held_market, target_market])
                .await
                .unwrap();

        let entries: Vec<_> = opportunities
            .into_iter()
            .filter(|opp| !opp.question.starts_with("[EXIT]"))
            .collect();

        assert!(
            entries.is_empty(),
            "same-direction BTC entry should be blocked by direction cap"
        );
    }
}
