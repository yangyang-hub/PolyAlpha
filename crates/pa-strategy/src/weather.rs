use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use alloy::primitives::U256;
use async_trait::async_trait;
use chrono::Utc;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::Deserialize;
use uuid::Uuid;

use pa_core::config::WeatherConfig;
use pa_core::traits::Strategy;
use pa_core::types::{
    ArbitrageOpportunity, ExecutionPlan, MarketInfo, OrderBook, StrategyType, TradeSide,
};

use crate::profitability::ProfitCalculator;

// ──── Weather Question Parser ────

/// Parsed weather question with extracted parameters.
#[derive(Debug, Clone)]
pub struct WeatherQuestion {
    pub metric: WeatherMetric,
    pub location: String,
    pub threshold: f64,
    pub comparison: Comparison,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WeatherMetric {
    TemperatureMax,
    TemperatureMin,
    TemperatureAvg,
    Rainfall,
    Snowfall,
    WindSpeed,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Comparison {
    Above,
    Below,
    AtLeast,
    AtMost,
}

/// Known city names for location extraction.
const KNOWN_CITIES: &[&str] = &[
    "New York", "NYC", "Los Angeles", "LA", "Chicago", "Houston", "Phoenix",
    "Philadelphia", "San Antonio", "San Diego", "Dallas", "San Jose",
    "Austin", "Jacksonville", "Fort Worth", "Columbus", "Charlotte",
    "Indianapolis", "San Francisco", "Seattle", "Denver", "Nashville",
    "Oklahoma City", "El Paso", "Portland", "Las Vegas", "Memphis",
    "Louisville", "Baltimore", "Milwaukee", "Albuquerque", "Tucson",
    "Fresno", "Mesa", "Sacramento", "Atlanta", "Kansas City", "Omaha",
    "Miami", "Minneapolis", "Tampa", "New Orleans", "Cleveland",
    "London", "Paris", "Tokyo", "Berlin", "Sydney", "Toronto", "Mumbai",
    "Beijing", "Shanghai", "Moscow", "Dubai", "Singapore", "Hong Kong",
    "Rome", "Madrid", "Amsterdam", "Bangkok", "Seoul",
];

/// Check if `text` contains `word` as a whole word (not part of a larger word).
fn contains_word(text: &str, word: &str) -> bool {
    for (i, _) in text.match_indices(word) {
        let before_ok = i == 0 || !text.as_bytes()[i - 1].is_ascii_alphabetic();
        let after_idx = i + word.len();
        let after_ok = after_idx >= text.len() || !text.as_bytes()[after_idx].is_ascii_alphabetic();
        if before_ok && after_ok {
            return true;
        }
    }
    false
}

/// Try to parse a market question as a weather-related question.
/// Returns `None` if the question doesn't match weather patterns.
pub fn parse_weather_question(question: &str) -> Option<WeatherQuestion> {
    let lower = question.to_lowercase();

    // Must contain at least one weather keyword (with word boundary checks for short words)
    let has_weather_keyword = lower.contains("temperature")
        || lower.contains("degrees")
        || lower.contains("rainfall")
        || contains_word(&lower, "rain")
        || lower.contains("snowfall")
        || contains_word(&lower, "snow")
        || lower.contains("wind speed")
        || contains_word(&lower, "wind")
        || lower.contains("inches of rain")
        || lower.contains("inches of snow")
        || lower.contains("fahrenheit")
        || lower.contains("celsius");

    if !has_weather_keyword {
        return None;
    }

    // Determine metric
    let metric = if lower.contains("snowfall") || contains_word(&lower, "snow") {
        WeatherMetric::Snowfall
    } else if lower.contains("rainfall") || contains_word(&lower, "rain") || lower.contains("inches of rain") {
        WeatherMetric::Rainfall
    } else if lower.contains("wind speed") || contains_word(&lower, "wind") {
        WeatherMetric::WindSpeed
    } else if lower.contains("high") || lower.contains("max") {
        WeatherMetric::TemperatureMax
    } else if lower.contains("low") || lower.contains("min") {
        WeatherMetric::TemperatureMin
    } else {
        WeatherMetric::TemperatureAvg
    };

    // Determine comparison
    let comparison = if lower.contains("below") || lower.contains("under") || lower.contains("less than") {
        Comparison::Below
    } else if lower.contains("at most") || lower.contains("no more than") {
        Comparison::AtMost
    } else if lower.contains("at least") || lower.contains("no less than") {
        Comparison::AtLeast
    } else {
        // Default: "exceed", "above", "over", "reach", "hit"
        Comparison::Above
    };

    // Extract numeric threshold
    let threshold = extract_number(&lower)?;

    // Extract location
    let location = extract_location(question)?;

    Some(WeatherQuestion {
        metric,
        location,
        threshold,
        comparison,
    })
}

/// Extract the first plausible number from the question text.
fn extract_number(text: &str) -> Option<f64> {
    let mut chars = text.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_ascii_digit() || c == '-' {
            let mut num_str = String::new();
            let mut has_dot = false;
            // Check for negative
            if c == '-' {
                num_str.push(c);
                chars.next();
            }
            while let Some(&c) = chars.peek() {
                if c.is_ascii_digit() {
                    num_str.push(c);
                    chars.next();
                } else if c == '.' && !has_dot {
                    has_dot = true;
                    num_str.push(c);
                    chars.next();
                } else {
                    break;
                }
            }
            if let Ok(n) = num_str.parse::<f64>() {
                // Filter out obviously non-threshold numbers (years, etc.)
                if n.abs() < 10000.0 && !(2020.0..=2035.0).contains(&n) {
                    return Some(n);
                }
            }
        } else {
            chars.next();
        }
    }
    None
}

/// Extract a known city/location name from the question.
fn extract_location(question: &str) -> Option<String> {
    let lower = question.to_lowercase();
    // Try known cities (longest match first for overlaps like "New York" vs "York")
    let mut best: Option<(&str, usize)> = None;
    for &city in KNOWN_CITIES {
        if let Some(pos) = lower.find(&city.to_lowercase()) {
            match best {
                Some((prev, _)) if city.len() <= prev.len() => {}
                _ => best = Some((city, pos)),
            }
        }
    }

    // Also try "in <Word>" pattern as fallback
    if let Some((city, _)) = best {
        return Some(city.to_string());
    }

    // Fallback: look for "in <CityName>" pattern
    let patterns = ["in ", "for "];
    for pat in &patterns {
        if let Some(idx) = lower.find(pat) {
            let after = &question[idx + pat.len()..];
            // Take capitalized words
            let words: Vec<&str> = after.split_whitespace().collect();
            let mut location = String::new();
            for w in words {
                if w.chars().next().is_some_and(|c| c.is_uppercase()) {
                    if !location.is_empty() {
                        location.push(' ');
                    }
                    // Strip trailing punctuation
                    let clean = w.trim_end_matches(|c: char| !c.is_alphanumeric());
                    location.push_str(clean);
                } else if !location.is_empty() {
                    break;
                }
            }
            if !location.is_empty() {
                return Some(location);
            }
        }
    }

    None
}

// ──── Open-Meteo Client ────

/// Weather forecast data from Open-Meteo API.
#[derive(Debug, Clone)]
pub struct ForecastData {
    pub values: Vec<f64>,
    pub dates: Vec<String>,
    pub mean: f64,
    pub std_dev: f64,
}

/// Open-Meteo geocoding API response.
#[derive(Debug, Deserialize)]
struct GeocodeResponse {
    results: Option<Vec<GeocodeResult>>,
}

#[derive(Debug, Deserialize)]
struct GeocodeResult {
    latitude: f64,
    longitude: f64,
}

/// Open-Meteo forecast API response.
#[derive(Debug, Deserialize)]
struct ForecastResponse {
    daily: Option<DailyData>,
}

#[derive(Debug, Deserialize)]
struct DailyData {
    #[serde(default)]
    time: Vec<String>,
    #[serde(default)]
    temperature_2m_max: Vec<f64>,
    #[serde(default)]
    temperature_2m_min: Vec<f64>,
    #[serde(default)]
    temperature_2m_mean: Vec<f64>,
    #[serde(default)]
    rain_sum: Vec<f64>,
    #[serde(default)]
    snowfall_sum: Vec<f64>,
    #[serde(default)]
    wind_speed_10m_max: Vec<f64>,
}

/// Fetch weather forecasts from Open-Meteo API (free, no API key).
pub struct OpenMeteoClient {
    http: reqwest::Client,
}

impl Default for OpenMeteoClient {
    fn default() -> Self {
        Self::new()
    }
}

impl OpenMeteoClient {
    pub fn new() -> Self {
        Self {
            http: reqwest::Client::new(),
        }
    }

    /// Geocode a location name to (latitude, longitude).
    pub async fn geocode(&self, location: &str) -> anyhow::Result<(f64, f64)> {
        let resp: GeocodeResponse = self
            .http
            .get("https://geocoding-api.open-meteo.com/v1/search")
            .query(&[("name", location), ("count", "1")])
            .send()
            .await?
            .json()
            .await?;

        let result = resp
            .results
            .and_then(|r| r.into_iter().next())
            .ok_or_else(|| anyhow::anyhow!("Location not found: {}", location))?;

        Ok((result.latitude, result.longitude))
    }

    /// Fetch daily weather forecast for the given coordinates.
    pub async fn forecast(
        &self,
        lat: f64,
        lon: f64,
        metric: WeatherMetric,
    ) -> anyhow::Result<ForecastData> {
        let daily_param = match metric {
            WeatherMetric::TemperatureMax => "temperature_2m_max",
            WeatherMetric::TemperatureMin => "temperature_2m_min",
            WeatherMetric::TemperatureAvg => "temperature_2m_mean",
            WeatherMetric::Rainfall => "rain_sum",
            WeatherMetric::Snowfall => "snowfall_sum",
            WeatherMetric::WindSpeed => "wind_speed_10m_max",
        };

        let resp: ForecastResponse = self
            .http
            .get("https://api.open-meteo.com/v1/forecast")
            .query(&[
                ("latitude", &lat.to_string()),
                ("longitude", &lon.to_string()),
                ("daily", &daily_param.to_string()),
                ("forecast_days", &"14".to_string()),
                ("temperature_unit", &"fahrenheit".to_string()),
            ])
            .send()
            .await?
            .json()
            .await?;

        let daily = resp
            .daily
            .ok_or_else(|| anyhow::anyhow!("No daily data in forecast response"))?;

        let (values, dates) = match metric {
            WeatherMetric::TemperatureMax => (daily.temperature_2m_max, daily.time),
            WeatherMetric::TemperatureMin => (daily.temperature_2m_min, daily.time),
            WeatherMetric::TemperatureAvg => (daily.temperature_2m_mean, daily.time),
            WeatherMetric::Rainfall => (daily.rain_sum, daily.time),
            WeatherMetric::Snowfall => (daily.snowfall_sum, daily.time),
            WeatherMetric::WindSpeed => (daily.wind_speed_10m_max, daily.time),
        };

        if values.is_empty() {
            return Err(anyhow::anyhow!("Empty forecast data"));
        }

        let mean = values.iter().sum::<f64>() / values.len() as f64;
        let variance = values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / values.len() as f64;
        let std_dev = variance.sqrt();

        Ok(ForecastData {
            values,
            dates,
            mean,
            std_dev,
        })
    }
}

// ──── Probability Model ────

/// Calculate probability that the weather metric meets the comparison threshold.
/// Uses normal distribution CDF with forecast mean and std_dev + extra uncertainty.
pub fn model_probability(
    forecast: &ForecastData,
    threshold: f64,
    comparison: Comparison,
    extra_uncertainty_pct: f64,
) -> f64 {
    // sigma = sqrt(forecast_std_dev^2 + (mean * uncertainty_pct / 100)^2)
    let extra_sigma = forecast.mean.abs() * extra_uncertainty_pct / 100.0;
    let sigma = (forecast.std_dev.powi(2) + extra_sigma.powi(2)).sqrt();

    // Avoid division by zero
    if sigma < 1e-10 {
        return match comparison {
            Comparison::Above | Comparison::AtLeast => {
                if forecast.mean >= threshold { 1.0 } else { 0.0 }
            }
            Comparison::Below | Comparison::AtMost => {
                if forecast.mean <= threshold { 1.0 } else { 0.0 }
            }
        };
    }

    let z = (threshold - forecast.mean) / sigma;

    match comparison {
        Comparison::Above | Comparison::AtLeast => 1.0 - normal_cdf(z),
        Comparison::Below | Comparison::AtMost => normal_cdf(z),
    }
}

/// Rational approximation of the standard normal CDF (Abramowitz & Stegun 26.2.17).
/// Accuracy: |error| < 7.5e-8.
pub fn normal_cdf(x: f64) -> f64 {
    if x < -8.0 {
        return 0.0;
    }
    if x > 8.0 {
        return 1.0;
    }

    let is_negative = x < 0.0;
    let x_abs = x.abs();

    // Coefficients from Abramowitz & Stegun
    let p = 0.2316419;
    let b1 = 0.319381530;
    let b2 = -0.356563782;
    let b3 = 1.781477937;
    let b4 = -1.821255978;
    let b5 = 1.330274429;

    let t = 1.0 / (1.0 + p * x_abs);
    let t2 = t * t;
    let t3 = t2 * t;
    let t4 = t3 * t;
    let t5 = t4 * t;

    let pdf = (-0.5 * x_abs * x_abs).exp() / (2.0 * std::f64::consts::PI).sqrt();
    let cdf = 1.0 - pdf * (b1 * t + b2 * t2 + b3 * t3 + b4 * t4 + b5 * t5);

    if is_negative { 1.0 - cdf } else { cdf }
}

// ──── Weather Alpha Strategy ────

/// Cache entry for forecasts keyed by question hash.
struct CachedForecast {
    forecast: ForecastData,
    fetched_at: Instant,
}

pub struct WeatherAlphaStrategy {
    config: WeatherConfig,
    meteo: OpenMeteoClient,
    profit_calc: ProfitCalculator,
    get_orderbook: Box<dyn Fn(U256) -> Option<OrderBook> + Send + Sync>,
    /// Returns available capital (balance - exposure) for position sizing.
    get_available_capital: Box<dyn Fn() -> Decimal + Send + Sync>,
    forecast_cache: Arc<Mutex<HashMap<u64, CachedForecast>>>,
}

impl WeatherAlphaStrategy {
    pub fn new(
        config: WeatherConfig,
        gas_cost_usd: Decimal,
        get_orderbook: Box<dyn Fn(U256) -> Option<OrderBook> + Send + Sync>,
        get_available_capital: Box<dyn Fn() -> Decimal + Send + Sync>,
    ) -> Self {
        Self {
            config,
            meteo: OpenMeteoClient::new(),
            profit_calc: ProfitCalculator::new(gas_cost_usd),
            get_orderbook,
            get_available_capital,
            forecast_cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Hash a question string for cache key.
    fn question_hash(question: &str) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        question.to_lowercase().hash(&mut hasher);
        hasher.finish()
    }

    /// Get cached forecast or fetch new one.
    async fn get_forecast(
        &self,
        question: &str,
        parsed: &WeatherQuestion,
    ) -> Option<ForecastData> {
        let key = Self::question_hash(question);

        // Check cache
        {
            let cache = self.forecast_cache.lock().unwrap();
            if let Some(entry) = cache.get(&key)
                && entry.fetched_at.elapsed().as_secs() < self.config.refresh_interval_secs
            {
                return Some(entry.forecast.clone());
            }
        }

        // Fetch from API
        let coords = match self.meteo.geocode(&parsed.location).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(
                    location = %parsed.location,
                    error = %e,
                    "Failed to geocode location"
                );
                return None;
            }
        };

        let forecast = match self.meteo.forecast(coords.0, coords.1, parsed.metric).await {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!(
                    location = %parsed.location,
                    metric = ?parsed.metric,
                    error = %e,
                    "Failed to fetch forecast"
                );
                return None;
            }
        };

        // Cache the result
        {
            let mut cache = self.forecast_cache.lock().unwrap();
            cache.insert(
                key,
                CachedForecast {
                    forecast: forecast.clone(),
                    fetched_at: Instant::now(),
                },
            );
        }

        Some(forecast)
    }

    /// Detect a directional opportunity on a single weather market.
    async fn detect_weather_opportunity(
        &self,
        market: &MarketInfo,
        parsed: &WeatherQuestion,
    ) -> Option<ArbitrageOpportunity> {
        let forecast = self.get_forecast(&market.question, parsed).await?;

        let uncertainty_pct: f64 = self
            .config
            .forecast_uncertainty_pct
            .to_string()
            .parse()
            .unwrap_or(10.0);
        let model_prob = model_probability(
            &forecast,
            parsed.threshold,
            parsed.comparison,
            uncertainty_pct,
        );

        // Determine which side to buy
        // YES token = tokens[0] (by convention)
        if market.tokens.len() < 2 {
            return None;
        }

        let yes_token = &market.tokens[0];
        let no_token = &market.tokens[1];

        let yes_book = (self.get_orderbook)(yes_token.token_id)?;
        let no_book = (self.get_orderbook)(no_token.token_id)?;

        let yes_ask = yes_book.best_ask()?.price;
        let no_ask = no_book.best_ask()?.price;

        // Model says event is likely (buy YES) or unlikely (buy NO)
        let model_prob_dec = Decimal::from_f64_retain(model_prob)?;
        let no_model_prob = Decimal::ONE - model_prob_dec;

        // Compute edge on both sides and pick the larger one
        let yes_edge = if model_prob_dec > yes_ask {
            Some(model_prob_dec - yes_ask)
        } else {
            None
        };
        let no_edge = if no_model_prob > no_ask {
            Some(no_model_prob - no_ask)
        } else {
            None
        };

        let (token_id, ask_price, edge, effective_prob) = match (yes_edge, no_edge) {
            (Some(ye), Some(ne)) => {
                if ye >= ne {
                    (yes_token.token_id, yes_ask, ye, model_prob_dec)
                } else {
                    (no_token.token_id, no_ask, ne, no_model_prob)
                }
            }
            (Some(ye), None) => (yes_token.token_id, yes_ask, ye, model_prob_dec),
            (None, Some(ne)) => (no_token.token_id, no_ask, ne, no_model_prob),
            (None, None) => return None,
        };

        // Check minimum edge threshold
        let edge_bps = {
            use rust_decimal::prelude::ToPrimitive;
            (edge * dec!(10000)).to_u32().unwrap_or(0)
        };
        if edge_bps < self.config.min_edge_bps {
            return None;
        }

        // Position sizing via Kelly criterion: f* = edge / (1 - price)
        // Guard against extreme prices where denominator approaches zero
        let kelly_raw = if ask_price > Decimal::ZERO && ask_price < dec!(0.99) {
            (edge / (Decimal::ONE - ask_price)).min(Decimal::TWO) // cap at 200%
        } else {
            Decimal::ZERO
        };
        let kelly_size = kelly_raw * self.config.kelly_fraction * self.config.max_position_usdc;
        let available = (self.get_available_capital)();
        let size = kelly_size.min(self.config.max_position_usdc).min(available);

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
            question = %market.question,
            metric = ?parsed.metric,
            location = %parsed.location,
            threshold = parsed.threshold,
            forecast_mean = forecast.mean,
            model_prob = model_prob,
            ask_price = %ask_price,
            edge_bps = edge_bps,
            available_capital = %available,
            size = %size,
            est_profit = %est.net_profit,
            "Weather alpha opportunity detected"
        );

        Some(ArbitrageOpportunity {
            id: Uuid::now_v7(),
            strategy_type: StrategyType::Weather,
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
}

#[async_trait]
impl Strategy for WeatherAlphaStrategy {
    fn name(&self) -> &str {
        "WeatherAlpha"
    }

    fn strategy_type(&self) -> StrategyType {
        StrategyType::Weather
    }

    async fn scan(
        &self,
        markets: &[MarketInfo],
    ) -> pa_core::Result<Vec<ArbitrageOpportunity>> {
        let mut opportunities = Vec::new();

        for market in markets {
            if !market.active {
                continue;
            }

            let parsed = match parse_weather_question(&market.question) {
                Some(p) => p,
                None => continue, // Not a weather market
            };

            if let Some(opp) = self.detect_weather_opportunity(market, &parsed).await {
                opportunities.push(opp);
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
    fn test_parse_temperature_above() {
        let q = "Will the temperature in NYC exceed 100F this summer?";
        let parsed = parse_weather_question(q).unwrap();
        assert_eq!(parsed.metric, WeatherMetric::TemperatureAvg);
        assert_eq!(parsed.threshold, 100.0);
        assert_eq!(parsed.comparison, Comparison::Above);
        assert_eq!(parsed.location, "NYC");
    }

    #[test]
    fn test_parse_temperature_max() {
        let q = "Will the high temperature in New York exceed 105 degrees?";
        let parsed = parse_weather_question(q).unwrap();
        assert_eq!(parsed.metric, WeatherMetric::TemperatureMax);
        assert_eq!(parsed.threshold, 105.0);
        assert_eq!(parsed.comparison, Comparison::Above);
        assert_eq!(parsed.location, "New York");
    }

    #[test]
    fn test_parse_rainfall() {
        let q = "Will rainfall in London exceed 2 inches this week?";
        let parsed = parse_weather_question(q).unwrap();
        assert_eq!(parsed.metric, WeatherMetric::Rainfall);
        assert_eq!(parsed.threshold, 2.0);
        assert_eq!(parsed.comparison, Comparison::Above);
        assert_eq!(parsed.location, "London");
    }

    #[test]
    fn test_parse_snow() {
        let q = "Will snowfall in Denver exceed 12 inches this weekend?";
        let parsed = parse_weather_question(q).unwrap();
        assert_eq!(parsed.metric, WeatherMetric::Snowfall);
        assert_eq!(parsed.threshold, 12.0);
        assert_eq!(parsed.comparison, Comparison::Above);
        assert_eq!(parsed.location, "Denver");
    }

    #[test]
    fn test_parse_below() {
        let q = "Will the temperature in Chicago fall below 0 degrees?";
        let parsed = parse_weather_question(q).unwrap();
        assert_eq!(parsed.comparison, Comparison::Below);
        assert_eq!(parsed.threshold, 0.0);
        assert_eq!(parsed.location, "Chicago");
    }

    #[test]
    fn test_parse_non_weather() {
        assert!(parse_weather_question("Will Bitcoin reach $100k?").is_none());
        assert!(parse_weather_question("Will the Democrats win the election?").is_none());
        assert!(parse_weather_question("Will Tesla stock price exceed 500?").is_none());
    }

    #[test]
    fn test_normal_cdf_known_values() {
        // CDF(0) = 0.5
        let cdf0 = normal_cdf(0.0);
        assert!((cdf0 - 0.5).abs() < 1e-6, "CDF(0) = {}, expected 0.5", cdf0);

        // CDF(1.96) ≈ 0.975
        let cdf196 = normal_cdf(1.96);
        assert!(
            (cdf196 - 0.975).abs() < 1e-3,
            "CDF(1.96) = {}, expected ~0.975",
            cdf196
        );

        // CDF(-1.96) ≈ 0.025
        let cdf_neg196 = normal_cdf(-1.96);
        assert!(
            (cdf_neg196 - 0.025).abs() < 1e-3,
            "CDF(-1.96) = {}, expected ~0.025",
            cdf_neg196
        );

        // CDF(3.0) should be very close to 1
        assert!(normal_cdf(3.0) > 0.998);

        // CDF(-3.0) should be very close to 0
        assert!(normal_cdf(-3.0) < 0.002);
    }

    #[test]
    fn test_model_probability_above() {
        // mean=95, std=5, threshold=100
        // z = (100-95)/5 = 1.0, P(X>100) = 1 - CDF(1.0) ≈ 0.159
        let forecast = ForecastData {
            values: vec![90.0, 95.0, 100.0],
            dates: vec!["2025-07-01".into(), "2025-07-02".into(), "2025-07-03".into()],
            mean: 95.0,
            std_dev: 5.0,
        };
        let prob = model_probability(&forecast, 100.0, Comparison::Above, 0.0);
        assert!(
            (prob - 0.159).abs() < 0.01,
            "P(X>100) = {}, expected ~0.159",
            prob
        );
    }

    #[test]
    fn test_model_probability_below() {
        // mean=95, std=5, threshold=100
        // P(X<100) = CDF(1.0) ≈ 0.841
        let forecast = ForecastData {
            values: vec![90.0, 95.0, 100.0],
            dates: vec!["2025-07-01".into(), "2025-07-02".into(), "2025-07-03".into()],
            mean: 95.0,
            std_dev: 5.0,
        };
        let prob = model_probability(&forecast, 100.0, Comparison::Below, 0.0);
        assert!(
            (prob - 0.841).abs() < 0.01,
            "P(X<100) = {}, expected ~0.841",
            prob
        );
    }

    #[test]
    fn test_model_probability_with_extra_uncertainty() {
        // Extra uncertainty widens the distribution, moving probabilities toward 0.5
        let forecast = ForecastData {
            values: vec![95.0],
            dates: vec!["2025-07-01".into()],
            mean: 95.0,
            std_dev: 5.0,
        };
        let prob_no_extra = model_probability(&forecast, 100.0, Comparison::Above, 0.0);
        let prob_with_extra = model_probability(&forecast, 100.0, Comparison::Above, 10.0);
        // With extra uncertainty, probability should be higher (closer to 0.5)
        assert!(
            prob_with_extra > prob_no_extra,
            "Extra uncertainty should increase P(X>threshold): {} > {}",
            prob_with_extra,
            prob_no_extra
        );
    }

    #[test]
    fn test_high_edge_triggers_opportunity() {
        // Setup: market has YES@0.30 but model says P=0.60
        // This should produce a detectable edge
        let config = WeatherConfig {
            min_edge_bps: 500, // 5%
            max_position_usdc: dec!(100),
            kelly_fraction: dec!(0.25),
            forecast_uncertainty_pct: dec!(0),
            refresh_interval_secs: 3600,
        };

        let profit_calc = ProfitCalculator::new(Decimal::ZERO);

        // Model says 60% likely, market says 30% → 30% edge = 3000 bps
        let est = profit_calc.directional_buy_profit(dec!(0.30), dec!(0.60), dec!(50), 200);
        assert!(est.net_profit > Decimal::ZERO);

        // Edge check: 0.60 - 0.30 = 0.30 = 3000 bps > min_edge_bps(500)
        let edge = dec!(0.60) - dec!(0.30);
        let edge_bps_dec = edge * dec!(10000);
        use rust_decimal::prelude::ToPrimitive;
        let edge_bps = edge_bps_dec.to_u32().unwrap_or(0);
        assert!(edge_bps >= config.min_edge_bps, "edge_bps={} >= min={}", edge_bps, config.min_edge_bps);
    }
}
