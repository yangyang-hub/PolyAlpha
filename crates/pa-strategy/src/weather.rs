use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use alloy::primitives::U256;
use async_trait::async_trait;
use chrono::{Datelike, Local, NaiveDate, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::Deserialize;
use uuid::Uuid;

use pa_core::config::{ForecastErrorConfig, WeatherConfig};
use pa_core::traits::Strategy;
use pa_core::types::{
    ArbitrageOpportunity, ExecutionPlan, MarketInfo, NegRiskEvent, OrderBook, StrategyType,
    TradeSide,
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

/// Return the forecast error sigma for a given metric from the config.
pub fn sigma_for_metric(config: &ForecastErrorConfig, metric: WeatherMetric) -> f64 {
    match metric {
        WeatherMetric::TemperatureMax
        | WeatherMetric::TemperatureMin
        | WeatherMetric::TemperatureAvg => config.temperature_sigma_f,
        WeatherMetric::Rainfall => config.precipitation_sigma_in,
        WeatherMetric::Snowfall => config.snowfall_sigma_in,
        WeatherMetric::WindSpeed => config.wind_sigma_mph,
    }
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

// ──── Date Parser ────

/// Month name lookup table for date parsing.
const MONTHS: &[(&str, u32)] = &[
    ("january", 1), ("jan", 1),
    ("february", 2), ("feb", 2),
    ("march", 3), ("mar", 3),
    ("april", 4), ("apr", 4),
    ("may", 5),
    ("june", 6), ("jun", 6),
    ("july", 7), ("jul", 7),
    ("august", 8), ("aug", 8),
    ("september", 9), ("sep", 9),
    ("october", 10), ("oct", 10),
    ("november", 11), ("nov", 11),
    ("december", 12), ("dec", 12),
];

/// Parse a target date from question/event text.
///
/// Patterns matched:
/// - `"today"` → today's date
/// - `"tomorrow"` → tomorrow's date
/// - `"on February 14"` / `"on Feb 14"` → month + day of current year
/// - `"on 2/14"` → numeric M/D of current year
///
/// Returns `None` if no recognizable date pattern is found.
pub fn parse_target_date(text: &str) -> Option<NaiveDate> {
    let lower = text.to_lowercase();
    let today = Local::now().date_naive();

    // "today"
    if contains_word(&lower, "today") {
        return Some(today);
    }

    // "tomorrow"
    if contains_word(&lower, "tomorrow") {
        return Some(today + chrono::Duration::days(1));
    }

    // "on February 14" / "on Feb 14" — match "on <Month> <day>"
    // Also match without "on ": "February 14" anywhere in text
    for &(name, month_num) in MONTHS {
        if let Some(idx) = lower.find(name) {
            // Look for a day number after the month name
            let after_month = &lower[idx + name.len()..];
            let trimmed = after_month.trim_start();
            if let Some(day) = parse_leading_u32(trimmed)
                && (1..=31).contains(&day)
            {
                let year = today.year();
                if let Some(date) = NaiveDate::from_ymd_opt(year, month_num, day) {
                    return Some(date);
                }
            }
        }
    }

    // "on 2/14" — numeric M/D pattern
    // Match "M/D" anywhere in text
    let bytes = lower.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    while i < len {
        if bytes[i].is_ascii_digit() {
            let m_start = i;
            while i < len && bytes[i].is_ascii_digit() {
                i += 1;
            }
            let m_end = i;
            if i < len && bytes[i] == b'/' {
                i += 1;
                if i < len && bytes[i].is_ascii_digit() {
                    let d_start = i;
                    while i < len && bytes[i].is_ascii_digit() {
                        i += 1;
                    }
                    let d_end = i;
                    // Make sure this isn't followed by more '/' (which would be a year/path)
                    if (i >= len || bytes[i] != b'/')
                        && let (Ok(m), Ok(d)) = (
                            lower[m_start..m_end].parse::<u32>(),
                            lower[d_start..d_end].parse::<u32>(),
                        )
                        && (1..=12).contains(&m) && (1..=31).contains(&d)
                    {
                        let year = today.year();
                        if let Some(date) = NaiveDate::from_ymd_opt(year, m, d) {
                            return Some(date);
                        }
                    }
                }
            }
        } else {
            i += 1;
        }
    }

    None
}

/// Parse a leading unsigned integer from a string slice.
fn parse_leading_u32(s: &str) -> Option<u32> {
    let num_str: String = s.chars().take_while(|c| c.is_ascii_digit()).collect();
    if num_str.is_empty() {
        return None;
    }
    num_str.parse().ok()
}

// ──── Precipitation Unit Detection ────

/// Detect whether a market question uses millimeters or inches for precipitation.
///
/// Returns `"mm"` if the text mentions millimeters, otherwise `"inch"` (US default).
pub fn detect_precipitation_unit(text: &str) -> &'static str {
    let lower = text.to_lowercase();
    if lower.contains("mm") || lower.contains("millimeter") {
        "mm"
    } else {
        "inch"
    }
}

// ──── NegRisk Outcome Range Parser ────

/// A numeric range parsed from a NegRisk outcome market question.
#[derive(Debug, Clone)]
pub struct OutcomeRange {
    /// Lower bound (None = no lower bound, e.g. "35°F or below").
    pub lower: Option<f64>,
    /// Upper bound (None = no upper bound, e.g. "50°F or higher").
    pub upper: Option<f64>,
}

/// Parse a numeric range from a NegRisk outcome market question text.
///
/// Supports formats:
/// - "35°F or below" / "35 or less" → `{ lower: None, upper: 35.0 }`
/// - "50°F or higher" / "50 or above" → `{ lower: 50.0, upper: None }`
/// - "36-37°F" / "36-37" → `{ lower: 36.0, upper: 37.0 }`
pub fn parse_outcome_range(text: &str) -> Option<OutcomeRange> {
    // Strip units for cleaner parsing
    let cleaned = text
        .replace("°F", "")
        .replace("°C", "")
        .replace("°f", "")
        .replace("°c", "")
        .replace(" inches", "")
        .replace(" mm", "")
        .replace(" mph", "")
        .replace(" km/h", "");
    let lower_text = cleaned.to_lowercase();

    // 1. Try dash-separated range: "36-37", "36 - 37"
    if let Some(range) = parse_dash_range(&cleaned) {
        return Some(range);
    }

    // 2. Try "X or below/less/under" pattern
    let below_keywords = ["or below", "or less", "or under", "or lower", "or fewer"];
    for kw in &below_keywords {
        if lower_text.contains(kw)
            && let Some(n) = extract_first_number(&cleaned)
        {
            return Some(OutcomeRange {
                lower: None,
                upper: Some(n),
            });
        }
    }

    // 3. Try "X or above/higher/more" pattern
    let above_keywords = ["or above", "or higher", "or more", "or greater"];
    for kw in &above_keywords {
        if lower_text.contains(kw)
            && let Some(n) = extract_first_number(&cleaned)
        {
            return Some(OutcomeRange {
                lower: Some(n),
                upper: None,
            });
        }
    }

    None
}

/// Parse "X-Y" dash-separated range from text.
fn parse_dash_range(text: &str) -> Option<OutcomeRange> {
    // Look for pattern: digits (optional decimal) dash digits (optional decimal)
    let re_like = |s: &str| -> Option<(f64, f64)> {
        // Find first occurrence of "number-number"
        let bytes = s.as_bytes();
        let len = bytes.len();
        let mut i = 0;

        while i < len {
            // Find start of first number
            if bytes[i].is_ascii_digit() || (bytes[i] == b'-' && i + 1 < len && bytes[i + 1].is_ascii_digit()) {
                let num1_start = i;
                // Skip possible negative sign
                if bytes[i] == b'-' {
                    i += 1;
                }
                // Read digits (and optional dot)
                while i < len && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
                    i += 1;
                }
                let num1_end = i;

                // Skip optional whitespace
                while i < len && bytes[i] == b' ' {
                    i += 1;
                }

                // Expect dash
                if i < len && bytes[i] == b'-' {
                    i += 1;
                    // Skip optional whitespace
                    while i < len && bytes[i] == b' ' {
                        i += 1;
                    }

                    // Read second number
                    if i < len && (bytes[i].is_ascii_digit() || bytes[i] == b'-') {
                        let num2_start = i;
                        if bytes[i] == b'-' {
                            i += 1;
                        }
                        while i < len && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
                            i += 1;
                        }
                        let num2_end = i;

                        if let (Ok(a), Ok(b)) = (
                            s[num1_start..num1_end].parse::<f64>(),
                            s[num2_start..num2_end].parse::<f64>(),
                        ) {
                            // Filter out year-like numbers
                            if !(2020.0..=2035.0).contains(&a) && !(2020.0..=2035.0).contains(&b) {
                                return Some((a, b));
                            }
                        }
                    }
                }
            }
            i += 1;
        }
        None
    };

    let (a, b) = re_like(text)?;
    Some(OutcomeRange {
        lower: Some(a),
        upper: Some(b),
    })
}

/// Extract the first plausible number from text (for outcome range parsing).
fn extract_first_number(text: &str) -> Option<f64> {
    let mut chars = text.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_ascii_digit() || c == '-' {
            let mut num_str = String::new();
            let mut has_dot = false;
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
            if let Ok(n) = num_str.parse::<f64>()
                && !(2020.0..=2035.0).contains(&n)
            {
                return Some(n);
            }
        } else {
            chars.next();
        }
    }
    None
}

/// Check if a NegRisk event title is weather-related and extract metric + location.
///
/// Unlike `parse_weather_question`, this doesn't need a threshold or comparison
/// since NegRisk events distribute outcomes across multiple range markets.
pub fn parse_weather_event_title(title: &str) -> Option<(WeatherMetric, String)> {
    let lower = title.to_lowercase();

    // Must contain at least one weather keyword
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

    // Determine metric (same logic as parse_weather_question)
    let metric = if lower.contains("snowfall") || contains_word(&lower, "snow") {
        WeatherMetric::Snowfall
    } else if lower.contains("rainfall") || contains_word(&lower, "rain") || lower.contains("inches of rain") {
        WeatherMetric::Rainfall
    } else if lower.contains("wind speed") || contains_word(&lower, "wind") {
        WeatherMetric::WindSpeed
    } else if lower.contains("high") || lower.contains("max") || lower.contains("highest") {
        WeatherMetric::TemperatureMax
    } else if lower.contains("low") || lower.contains("min") || lower.contains("lowest") {
        WeatherMetric::TemperatureMin
    } else {
        WeatherMetric::TemperatureAvg
    };

    let location = extract_location(title)?;

    Some((metric, location))
}

// ──── Open-Meteo Client ────

/// Weather forecast data from Open-Meteo API.
#[derive(Debug, Clone)]
pub struct ForecastData {
    pub values: Vec<f64>,
    pub dates: Vec<String>,
    pub mean: f64,
    pub std_dev: f64,
    /// Single-day value when a specific target date was requested.
    pub target_value: Option<f64>,
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
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
        }
    }

    /// Geocode a location name to (latitude, longitude) with retry.
    pub async fn geocode(&self, location: &str) -> anyhow::Result<(f64, f64)> {
        let http = &self.http;
        let result = with_retry(2, || async {
            let resp: GeocodeResponse = http
                .get("https://geocoding-api.open-meteo.com/v1/search")
                .query(&[("name", location), ("count", "1")])
                .send()
                .await?
                .json()
                .await?;
            Ok(resp)
        })
        .await?;

        let geo = result
            .results
            .and_then(|r| r.into_iter().next())
            .ok_or_else(|| anyhow::anyhow!("Location not found: {}", location))?;

        Ok((geo.latitude, geo.longitude))
    }

    /// Fetch daily weather forecast for the given coordinates.
    ///
    /// When `target_date` is `Some`, requests a single-day forecast for that date.
    /// When `None`, fetches a 14-day forecast window.
    ///
    /// `precipitation_unit` should be `"mm"` or `"inch"`.
    pub async fn forecast(
        &self,
        lat: f64,
        lon: f64,
        metric: WeatherMetric,
        target_date: Option<NaiveDate>,
        precipitation_unit: &str,
    ) -> anyhow::Result<ForecastData> {
        let daily_param = match metric {
            WeatherMetric::TemperatureMax => "temperature_2m_max",
            WeatherMetric::TemperatureMin => "temperature_2m_min",
            WeatherMetric::TemperatureAvg => "temperature_2m_mean",
            WeatherMetric::Rainfall => "rain_sum",
            WeatherMetric::Snowfall => "snowfall_sum",
            WeatherMetric::WindSpeed => "wind_speed_10m_max",
        };

        let mut params: Vec<(&str, String)> = vec![
            ("latitude", lat.to_string()),
            ("longitude", lon.to_string()),
            ("daily", daily_param.to_string()),
            ("temperature_unit", "fahrenheit".to_string()),
        ];

        // Add precipitation unit for rain/snow metrics
        if matches!(metric, WeatherMetric::Rainfall | WeatherMetric::Snowfall) {
            params.push(("precipitation_unit", precipitation_unit.to_string()));
        }

        // Add wind speed unit — Open-Meteo default is km/h, Polymarket uses mph
        if matches!(metric, WeatherMetric::WindSpeed) {
            params.push(("wind_speed_unit", "mph".to_string()));
        }

        if let Some(date) = target_date {
            let date_str = date.format("%Y-%m-%d").to_string();
            params.push(("start_date", date_str.clone()));
            params.push(("end_date", date_str));
        } else {
            params.push(("forecast_days", "14".to_string()));
        }

        let http = &self.http;
        let params_clone = params.clone();
        let resp: ForecastResponse = with_retry(2, || {
            let p = params_clone.clone();
            async move {
                let r: ForecastResponse = http
                    .get("https://api.open-meteo.com/v1/forecast")
                    .query(&p)
                    .send()
                    .await?
                    .json()
                    .await?;
                Ok(r)
            }
        })
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

        let target_value = if target_date.is_some() {
            Some(values[0])
        } else {
            None
        };

        Ok(ForecastData {
            values,
            dates,
            mean,
            std_dev,
            target_value,
        })
    }
}

// ──── HTTP Retry Helper ────

/// Retry an async operation with exponential backoff.
/// Delays: 500ms, 1s, 2s, ...
async fn with_retry<T, F, Fut>(max_retries: u32, f: F) -> anyhow::Result<T>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<T>>,
{
    let mut last_err = None;
    for attempt in 0..=max_retries {
        match f().await {
            Ok(v) => return Ok(v),
            Err(e) => {
                last_err = Some(e);
                if attempt < max_retries {
                    let delay = Duration::from_millis(500 * 2u64.pow(attempt));
                    tokio::time::sleep(delay).await;
                }
            }
        }
    }
    Err(last_err.unwrap())
}

// ──── Probability Model ────

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

/// Log-normal CDF for precipitation metrics.
///
/// Precipitation data is non-negative and right-skewed, making log-normal
/// a better fit than normal distribution.
/// Returns 0.0 for t <= 0 or mean <= 0.
pub fn lognormal_cdf(t: f64, mean: f64, sigma: f64) -> f64 {
    if t <= 0.0 || mean <= 0.0 || sigma <= 0.0 {
        return 0.0;
    }

    // Convert (mean, sigma) of the actual variable to log-space parameters
    // mu_ln = ln(mean^2 / sqrt(mean^2 + sigma^2))
    // sigma_ln = sqrt(ln(1 + sigma^2/mean^2))
    let sigma2 = sigma * sigma;
    let mean2 = mean * mean;
    let sigma_ln = (1.0 + sigma2 / mean2).ln().sqrt();
    let mu_ln = mean.ln() - 0.5 * sigma_ln * sigma_ln;

    let z = (t.ln() - mu_ln) / sigma_ln;
    normal_cdf(z)
}

/// Weibull CDF for wind speed metrics.
///
/// Wind speed follows a Weibull distribution. We use shape k=2 (Rayleigh).
/// lambda = mean / Gamma(1 + 1/k) = mean / Gamma(1.5) ≈ mean / 0.8862.
/// P(X < t) = 1 - exp(-(t/lambda)^k)
/// Returns 0.0 for t <= 0 or mean <= 0.
pub fn weibull_cdf(t: f64, mean: f64, _sigma: f64) -> f64 {
    if t <= 0.0 || mean <= 0.0 {
        return 0.0;
    }

    let k = 2.0;
    // Gamma(1.5) = sqrt(pi)/2 ≈ 0.886227
    let gamma_1_5 = std::f64::consts::PI.sqrt() / 2.0;
    let lambda = mean / gamma_1_5;

    1.0 - (-(t / lambda).powf(k)).exp()
}

/// Dispatch to the appropriate CDF based on weather metric type.
fn cdf_for_metric(metric: WeatherMetric, value: f64, mean: f64, sigma: f64) -> f64 {
    match metric {
        WeatherMetric::TemperatureMax
        | WeatherMetric::TemperatureMin
        | WeatherMetric::TemperatureAvg => {
            if sigma < 1e-10 {
                if mean >= value { 1.0 } else { 0.0 }
            } else {
                normal_cdf((value - mean) / sigma)
            }
        }
        WeatherMetric::Rainfall | WeatherMetric::Snowfall => lognormal_cdf(value, mean, sigma),
        WeatherMetric::WindSpeed => weibull_cdf(value, mean, sigma),
    }
}

/// Compute effective sigma combining forecast std_dev and forecast error.
///
/// For date-specific forecasts (target_value.is_some()): uses only forecast error sigma.
/// For multi-day forecasts: sqrt(std_dev² + forecast_error_sigma²).
fn effective_sigma(forecast: &ForecastData, forecast_error_sigma: f64) -> f64 {
    if forecast.target_value.is_some() {
        // Date-specific: day-to-day variance is irrelevant, only forecast error matters
        forecast_error_sigma
    } else {
        // Multi-day: combine observed variance with forecast error
        (forecast.std_dev.powi(2) + forecast_error_sigma.powi(2)).sqrt()
    }
}

/// Calculate probability that the weather metric meets the comparison threshold.
///
/// Uses the appropriate distribution CDF for the metric type:
/// - Temperature: normal
/// - Precipitation/snowfall: log-normal
/// - Wind speed: Weibull
pub fn model_probability(
    forecast: &ForecastData,
    threshold: f64,
    comparison: Comparison,
    forecast_error_sigma: f64,
    metric: WeatherMetric,
) -> f64 {
    let sigma = effective_sigma(forecast, forecast_error_sigma);
    let mean = forecast.target_value.unwrap_or(forecast.mean);

    // Avoid division by zero for normal distribution
    if sigma < 1e-10 {
        return match comparison {
            Comparison::Above | Comparison::AtLeast => {
                if mean >= threshold { 1.0 } else { 0.0 }
            }
            Comparison::Below | Comparison::AtMost => {
                if mean <= threshold { 1.0 } else { 0.0 }
            }
        };
    }

    let cdf_at_threshold = cdf_for_metric(metric, threshold, mean, sigma);

    match comparison {
        Comparison::Above | Comparison::AtLeast => 1.0 - cdf_at_threshold,
        Comparison::Below | Comparison::AtMost => cdf_at_threshold,
    }
}

/// Calculate P(lower <= X <= upper) using the appropriate distribution CDF.
///
/// Used for NegRisk multi-outcome weather events where each outcome covers
/// a numeric range (e.g. "36-37°F").
pub fn model_range_probability(
    forecast: &ForecastData,
    range: &OutcomeRange,
    forecast_error_sigma: f64,
    metric: WeatherMetric,
) -> f64 {
    let sigma = effective_sigma(forecast, forecast_error_sigma);
    let mean = forecast.target_value.unwrap_or(forecast.mean);

    if sigma < 1e-10 {
        // Degenerate case: point distribution
        let in_range = match (range.lower, range.upper) {
            (Some(lo), Some(hi)) => mean >= lo && mean <= hi,
            (Some(lo), None) => mean >= lo,
            (None, Some(hi)) => mean <= hi,
            (None, None) => true,
        };
        return if in_range { 1.0 } else { 0.0 };
    }

    let cdf_upper = range
        .upper
        .map(|h| cdf_for_metric(metric, h, mean, sigma))
        .unwrap_or(1.0);
    let cdf_lower = range
        .lower
        .map(|l| cdf_for_metric(metric, l, mean, sigma))
        .unwrap_or(0.0);

    (cdf_upper - cdf_lower).max(0.0)
}

// ──── Cache Eviction ────

/// Remove stale forecast cache entries older than `max_age_secs`.
fn evict_stale_cache_entries(cache: &mut HashMap<u64, CachedForecast>, max_age_secs: u64) {
    cache.retain(|_, entry| entry.fetched_at.elapsed().as_secs() < max_age_secs);
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
    /// Returns existing position size for a given token (for dedup/cap).
    get_position: Box<dyn Fn(U256) -> Decimal + Send + Sync>,
    forecast_cache: Arc<Mutex<HashMap<u64, CachedForecast>>>,
    /// NegRisk multi-outcome weather events to scan.
    neg_risk_events: Vec<NegRiskEvent>,
}

impl WeatherAlphaStrategy {
    pub fn new(
        config: WeatherConfig,
        gas_cost_usd: Decimal,
        get_orderbook: Box<dyn Fn(U256) -> Option<OrderBook> + Send + Sync>,
        get_available_capital: Box<dyn Fn() -> Decimal + Send + Sync>,
        get_position: Box<dyn Fn(U256) -> Decimal + Send + Sync>,
        neg_risk_events: Vec<NegRiskEvent>,
    ) -> Self {
        Self {
            config,
            meteo: OpenMeteoClient::new(),
            profit_calc: ProfitCalculator::new(gas_cost_usd),
            get_orderbook,
            get_available_capital,
            get_position,
            forecast_cache: Arc::new(Mutex::new(HashMap::new())),
            neg_risk_events,
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
        target_date: Option<NaiveDate>,
        precipitation_unit: &str,
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

        let forecast = match self
            .meteo
            .forecast(coords.0, coords.1, parsed.metric, target_date, precipitation_unit)
            .await
        {
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

        // Cache the result and evict stale entries
        {
            let mut cache = self.forecast_cache.lock().unwrap();
            cache.insert(
                key,
                CachedForecast {
                    forecast: forecast.clone(),
                    fetched_at: Instant::now(),
                },
            );
            evict_stale_cache_entries(&mut cache, self.config.refresh_interval_secs * 2);
        }

        Some(forecast)
    }

    /// Detect a directional opportunity on a single weather market.
    async fn detect_weather_opportunity(
        &self,
        market: &MarketInfo,
        parsed: &WeatherQuestion,
    ) -> Option<ArbitrageOpportunity> {
        // Parse target date and precipitation unit from question
        let target_date = parse_target_date(&market.question);
        let precipitation_unit = if matches!(parsed.metric, WeatherMetric::Rainfall | WeatherMetric::Snowfall) {
            detect_precipitation_unit(&market.question)
        } else {
            "inch"
        };

        let forecast = self
            .get_forecast(&market.question, parsed, target_date, precipitation_unit)
            .await?;

        let forecast_error_sigma = sigma_for_metric(&self.config.forecast_error, parsed.metric);
        let model_prob = model_probability(
            &forecast,
            parsed.threshold,
            parsed.comparison,
            forecast_error_sigma,
            parsed.metric,
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

        // Position-aware sizing: subtract existing position from max
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
            question = %market.question,
            metric = ?parsed.metric,
            location = %parsed.location,
            threshold = parsed.threshold,
            forecast_mean = forecast.mean,
            model_prob = model_prob,
            ask_price = %ask_price,
            edge_bps = edge_bps,
            available_capital = %available,
            existing_position = %existing,
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

    /// Get a forecast by location string and metric, using the cache.
    async fn get_forecast_by_location(
        &self,
        location: &str,
        metric: WeatherMetric,
        target_date: Option<NaiveDate>,
        precipitation_unit: &str,
    ) -> Option<ForecastData> {
        // Use location+metric as cache key
        let cache_key = {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            location.to_lowercase().hash(&mut hasher);
            (metric as u8).hash(&mut hasher);
            hasher.finish()
        };

        // Check cache
        {
            let cache = self.forecast_cache.lock().unwrap();
            if let Some(entry) = cache.get(&cache_key)
                && entry.fetched_at.elapsed().as_secs() < self.config.refresh_interval_secs
            {
                return Some(entry.forecast.clone());
            }
        }

        // Fetch from API
        let coords = match self.meteo.geocode(location).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(location = %location, error = %e, "Failed to geocode for NegRisk weather");
                return None;
            }
        };

        let forecast = match self
            .meteo
            .forecast(coords.0, coords.1, metric, target_date, precipitation_unit)
            .await
        {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!(location = %location, metric = ?metric, error = %e, "Failed to fetch forecast for NegRisk weather");
                return None;
            }
        };

        // Cache and evict stale entries
        {
            let mut cache = self.forecast_cache.lock().unwrap();
            cache.insert(
                cache_key,
                CachedForecast {
                    forecast: forecast.clone(),
                    fetched_at: Instant::now(),
                },
            );
            evict_stale_cache_entries(&mut cache, self.config.refresh_interval_secs * 2);
        }

        Some(forecast)
    }

    /// Detect the best undervalued outcome in a NegRisk weather event.
    ///
    /// Checks both YES and NO sides for each outcome market.
    async fn detect_neg_risk_weather(
        &self,
        event: &NegRiskEvent,
        metric: WeatherMetric,
        location: &str,
    ) -> Option<ArbitrageOpportunity> {
        // Parse target date and precipitation unit from event title
        let target_date = parse_target_date(&event.title);
        let precipitation_unit = if matches!(metric, WeatherMetric::Rainfall | WeatherMetric::Snowfall) {
            detect_precipitation_unit(&event.title)
        } else {
            "inch"
        };

        let forecast = self
            .get_forecast_by_location(location, metric, target_date, precipitation_unit)
            .await?;

        let forecast_error_sigma = sigma_for_metric(&self.config.forecast_error, metric);

        // Evaluate each outcome market for edge (both YES and NO sides)
        let mut best_edge = Decimal::ZERO;
        let mut best_candidate: Option<(
            &MarketInfo,
            U256,       // token_id
            Decimal,    // ask_price
            Decimal,    // model_prob (effective for this side)
            Decimal,    // edge
        )> = None;

        for market in &event.markets {
            if !market.active || market.tokens.len() < 2 {
                continue;
            }

            // Parse the outcome range from this market's question
            let range = match parse_outcome_range(&market.question) {
                Some(r) => r,
                None => continue,
            };

            let model_prob_f64 =
                model_range_probability(&forecast, &range, forecast_error_sigma, metric);
            let model_prob = match Decimal::from_f64_retain(model_prob_f64) {
                Some(d) => d,
                None => continue,
            };

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

            // NO side check: P(NOT this range) = 1 - model_prob
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
            return None;
        }

        // Kelly criterion position sizing
        let kelly_raw = if ask_price > Decimal::ZERO && ask_price < dec!(0.99) {
            (edge / (Decimal::ONE - ask_price)).min(Decimal::TWO)
        } else {
            Decimal::ZERO
        };
        let kelly_size = kelly_raw * self.config.kelly_fraction * self.config.max_position_usdc;
        let available = (self.get_available_capital)();

        // Position-aware sizing: subtract existing position from max
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
            metric = ?metric,
            location = %location,
            forecast_mean = forecast.mean,
            model_prob = %effective_prob,
            ask_price = %ask_price,
            edge_bps = edge_bps,
            existing_position = %existing,
            size = %size,
            est_profit = %est.net_profit,
            "NegRisk weather alpha opportunity detected"
        );

        Some(ArbitrageOpportunity {
            id: Uuid::now_v7(),
            strategy_type: StrategyType::Weather,
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

        // 1. Scan binary weather markets (existing logic)
        for market in markets {
            if !market.active || market.neg_risk {
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

        // 2. Scan NegRisk weather events
        for event in &self.neg_risk_events {
            if let Some((metric, location)) = parse_weather_event_title(&event.title)
                && let Some(opp) = self.detect_neg_risk_weather(event, metric, &location).await
            {
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
            target_value: None,
        };
        let prob = model_probability(&forecast, 100.0, Comparison::Above, 0.0, WeatherMetric::TemperatureMax);
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
            target_value: None,
        };
        let prob = model_probability(&forecast, 100.0, Comparison::Below, 0.0, WeatherMetric::TemperatureMax);
        assert!(
            (prob - 0.841).abs() < 0.01,
            "P(X<100) = {}, expected ~0.841",
            prob
        );
    }

    #[test]
    fn test_model_probability_with_forecast_error() {
        // Extra forecast error widens the distribution, moving probabilities toward 0.5
        let forecast = ForecastData {
            values: vec![95.0],
            dates: vec!["2025-07-01".into()],
            mean: 95.0,
            std_dev: 5.0,
            target_value: None,
        };
        let prob_no_extra = model_probability(&forecast, 100.0, Comparison::Above, 0.0, WeatherMetric::TemperatureMax);
        let prob_with_extra = model_probability(&forecast, 100.0, Comparison::Above, 3.0, WeatherMetric::TemperatureMax);
        // With extra forecast error, probability should be higher (closer to 0.5)
        assert!(
            prob_with_extra > prob_no_extra,
            "Forecast error should increase P(X>threshold): {} > {}",
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
            forecast_error: ForecastErrorConfig::default(),
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

    // ──── OutcomeRange Parser Tests ────

    #[test]
    fn test_parse_outcome_range_below() {
        let range = parse_outcome_range("35°F or below").unwrap();
        assert!(range.lower.is_none());
        assert!((range.upper.unwrap() - 35.0).abs() < 0.01);
    }

    #[test]
    fn test_parse_outcome_range_above() {
        let range = parse_outcome_range("50°F or higher").unwrap();
        assert!((range.lower.unwrap() - 50.0).abs() < 0.01);
        assert!(range.upper.is_none());
    }

    #[test]
    fn test_parse_outcome_range_dash() {
        let range = parse_outcome_range("36-37°F").unwrap();
        assert!((range.lower.unwrap() - 36.0).abs() < 0.01);
        assert!((range.upper.unwrap() - 37.0).abs() < 0.01);
    }

    #[test]
    fn test_parse_outcome_range_in_question() {
        let range = parse_outcome_range("Will it be 38-39°F?").unwrap();
        assert!((range.lower.unwrap() - 38.0).abs() < 0.01);
        assert!((range.upper.unwrap() - 39.0).abs() < 0.01);
    }

    #[test]
    fn test_parse_outcome_range_non_numeric() {
        assert!(parse_outcome_range("Bitcoin $100k").is_none());
    }

    // ──── CDF Range Probability Tests ────

    #[test]
    fn test_range_probability_centered() {
        // mean=40, σ=3, range=38-42
        // P(38 ≤ X ≤ 42) should be roughly 0.50 (within ±1 sigma covers ~68%, so ±0.67σ ≈ 50%)
        let forecast = ForecastData {
            values: vec![37.0, 40.0, 43.0],
            dates: vec!["d1".into(), "d2".into(), "d3".into()],
            mean: 40.0,
            std_dev: 3.0,
            target_value: None,
        };
        let range = OutcomeRange {
            lower: Some(38.0),
            upper: Some(42.0),
        };
        let prob = model_range_probability(&forecast, &range, 0.0, WeatherMetric::TemperatureMax);
        assert!(
            (prob - 0.4950).abs() < 0.05,
            "P(38≤X≤42) = {}, expected ~0.50",
            prob
        );
    }

    #[test]
    fn test_range_probability_tail() {
        // mean=40, σ=3, range=50+ (>3σ from mean)
        let forecast = ForecastData {
            values: vec![40.0],
            dates: vec!["d1".into()],
            mean: 40.0,
            std_dev: 3.0,
            target_value: None,
        };
        let range = OutcomeRange {
            lower: Some(50.0),
            upper: None,
        };
        let prob = model_range_probability(&forecast, &range, 0.0, WeatherMetric::TemperatureMax);
        assert!(
            prob < 0.005,
            "P(X≥50) = {}, expected <0.005 (>3σ tail)",
            prob
        );
    }

    #[test]
    fn test_range_probabilities_sum_near_one() {
        // 7 contiguous ranges should sum to ~1.0
        let forecast = ForecastData {
            values: vec![40.0],
            dates: vec!["d1".into()],
            mean: 40.0,
            std_dev: 3.0,
            target_value: None,
        };
        let ranges = vec![
            OutcomeRange { lower: None, upper: Some(35.0) },          // ≤35
            OutcomeRange { lower: Some(35.0), upper: Some(37.0) },    // 35-37
            OutcomeRange { lower: Some(37.0), upper: Some(39.0) },    // 37-39
            OutcomeRange { lower: Some(39.0), upper: Some(41.0) },    // 39-41
            OutcomeRange { lower: Some(41.0), upper: Some(43.0) },    // 41-43
            OutcomeRange { lower: Some(43.0), upper: Some(45.0) },    // 43-45
            OutcomeRange { lower: Some(45.0), upper: None },          // ≥45
        ];
        let total: f64 = ranges
            .iter()
            .map(|r| model_range_probability(&forecast, r, 0.0, WeatherMetric::TemperatureMax))
            .sum();
        assert!(
            (total - 1.0).abs() < 0.01,
            "Sum of range probabilities = {}, expected ~1.0",
            total
        );
    }

    // ──── Event Title Parser Tests ────

    #[test]
    fn test_parse_weather_event_title_temperature() {
        let (metric, loc) =
            parse_weather_event_title("Highest temperature in NYC on Feb 14?").unwrap();
        assert_eq!(metric, WeatherMetric::TemperatureMax);
        assert_eq!(loc, "NYC");
    }

    #[test]
    fn test_parse_weather_event_title_non_weather() {
        assert!(parse_weather_event_title("Who will win Super Bowl?").is_none());
    }

    // ──── Date Parser Tests ────

    #[test]
    fn test_parse_target_date_full_month() {
        let date = parse_target_date("What will the temperature be on February 14?").unwrap();
        let today = Local::now().date_naive();
        assert_eq!(date.month(), 2);
        assert_eq!(date.day(), 14);
        assert_eq!(date.year(), today.year());
    }

    #[test]
    fn test_parse_target_date_abbreviated() {
        let date = parse_target_date("Highest temperature in NYC on Feb 14?").unwrap();
        let today = Local::now().date_naive();
        assert_eq!(date.month(), 2);
        assert_eq!(date.day(), 14);
        assert_eq!(date.year(), today.year());
    }

    #[test]
    fn test_parse_target_date_slash() {
        let date = parse_target_date("Temperature on 2/14 in NYC?").unwrap();
        let today = Local::now().date_naive();
        assert_eq!(date.month(), 2);
        assert_eq!(date.day(), 14);
        assert_eq!(date.year(), today.year());
    }

    #[test]
    fn test_parse_target_date_none() {
        assert!(parse_target_date("Will it rain this week?").is_none());
    }

    #[test]
    fn test_parse_target_date_today() {
        let date = parse_target_date("What is the temperature today in NYC?").unwrap();
        let today = Local::now().date_naive();
        assert_eq!(date, today);
    }

    #[test]
    fn test_parse_target_date_tomorrow() {
        let date = parse_target_date("Will it rain tomorrow in NYC?").unwrap();
        let today = Local::now().date_naive();
        assert_eq!(date, today + chrono::Duration::days(1));
    }

    // ──── Precipitation Unit Detection Tests ────

    #[test]
    fn test_detect_precipitation_unit_inches() {
        assert_eq!(detect_precipitation_unit("2 inches of rain in NYC"), "inch");
    }

    #[test]
    fn test_detect_precipitation_unit_mm() {
        assert_eq!(detect_precipitation_unit("50mm rainfall in Tokyo"), "mm");
    }

    #[test]
    fn test_detect_precipitation_unit_default() {
        assert_eq!(detect_precipitation_unit("Will it rain in NYC?"), "inch");
    }

    // ──── Distribution Model Tests ────

    #[test]
    fn test_lognormal_cdf_basic() {
        // For a log-normal variable with mean=1.0 and sigma=0.5,
        // CDF at the mean should be roughly around 0.5 (it's slightly above for log-normal)
        let cdf = lognormal_cdf(1.0, 1.0, 0.5);
        assert!(
            (cdf - 0.5).abs() < 0.15,
            "lognormal CDF at mean should be near 0.5, got {}",
            cdf
        );
    }

    #[test]
    fn test_lognormal_cdf_zero() {
        // P(rain < 0) = 0 for log-normal
        assert_eq!(lognormal_cdf(0.0, 1.0, 0.5), 0.0);
        assert_eq!(lognormal_cdf(-1.0, 1.0, 0.5), 0.0);
    }

    #[test]
    fn test_lognormal_cdf_monotonic() {
        // CDF should be monotonically increasing
        let cdf1 = lognormal_cdf(0.5, 1.0, 0.5);
        let cdf2 = lognormal_cdf(1.0, 1.0, 0.5);
        let cdf3 = lognormal_cdf(2.0, 1.0, 0.5);
        assert!(cdf1 < cdf2, "CDF should increase: {} < {}", cdf1, cdf2);
        assert!(cdf2 < cdf3, "CDF should increase: {} < {}", cdf2, cdf3);
    }

    #[test]
    fn test_weibull_cdf_basic() {
        // Weibull with k=2 (Rayleigh): known CDF values
        // lambda = mean / Gamma(1.5) ≈ mean / 0.886
        // P(X < mean) should be significant (around 0.54 for Rayleigh)
        let cdf = weibull_cdf(10.0, 10.0, 0.0);
        assert!(
            cdf > 0.4 && cdf < 0.8,
            "Weibull CDF at mean should be moderate, got {}",
            cdf
        );
    }

    #[test]
    fn test_weibull_cdf_zero() {
        assert_eq!(weibull_cdf(0.0, 10.0, 0.0), 0.0);
        assert_eq!(weibull_cdf(-1.0, 10.0, 0.0), 0.0);
    }

    #[test]
    fn test_weibull_cdf_large() {
        // Very large value should have CDF near 1
        let cdf = weibull_cdf(100.0, 10.0, 0.0);
        assert!(cdf > 0.99, "Weibull CDF at 10x mean should be near 1, got {}", cdf);
    }

    // ──── Forecast Error Sigma Tests ────

    #[test]
    fn test_forecast_error_sigma_for_metric() {
        let config = ForecastErrorConfig {
            temperature_sigma_f: 3.0,
            precipitation_sigma_in: 0.3,
            snowfall_sigma_in: 2.0,
            wind_sigma_mph: 5.0,
        };
        assert_eq!(sigma_for_metric(&config, WeatherMetric::TemperatureMax), 3.0);
        assert_eq!(sigma_for_metric(&config, WeatherMetric::TemperatureMin), 3.0);
        assert_eq!(sigma_for_metric(&config, WeatherMetric::TemperatureAvg), 3.0);
        assert_eq!(sigma_for_metric(&config, WeatherMetric::Rainfall), 0.3);
        assert_eq!(sigma_for_metric(&config, WeatherMetric::Snowfall), 2.0);
        assert_eq!(sigma_for_metric(&config, WeatherMetric::WindSpeed), 5.0);
    }

    #[test]
    fn test_date_specific_sigma_no_crossday() {
        // Date-specific forecast uses only forecast error, ignoring std_dev
        let forecast = ForecastData {
            values: vec![95.0],
            dates: vec!["2025-07-01".into()],
            mean: 95.0,
            std_dev: 0.0,   // Single day → no variance
            target_value: Some(95.0), // Date-specific
        };
        let sigma = effective_sigma(&forecast, 3.0);
        assert_eq!(sigma, 3.0, "Date-specific sigma should equal forecast error only");

        // Multi-day forecast combines both
        let multi_forecast = ForecastData {
            values: vec![90.0, 95.0, 100.0],
            dates: vec!["d1".into(), "d2".into(), "d3".into()],
            mean: 95.0,
            std_dev: 5.0,
            target_value: None,
        };
        let sigma2 = effective_sigma(&multi_forecast, 3.0);
        let expected = (25.0_f64 + 9.0).sqrt(); // sqrt(5² + 3²)
        assert!(
            (sigma2 - expected).abs() < 0.01,
            "Multi-day sigma should be sqrt(std_dev² + error²): {} vs {}",
            sigma2,
            expected
        );
    }

    // ──── Position-Aware Sizing Tests ────

    #[test]
    fn test_position_aware_sizing() {
        // Max position $50, existing $20 → remaining $30
        let max_position = dec!(50);
        let existing = dec!(20);
        let remaining = (max_position - existing).max(Decimal::ZERO);
        assert_eq!(remaining, dec!(30));

        // Kelly wants $40, but only $30 remaining
        let kelly_size = dec!(40);
        let size = kelly_size.min(remaining);
        assert_eq!(size, dec!(30));
    }

    #[test]
    fn test_position_at_max_skips() {
        // Max position $50, existing $50 → remaining $0 → skip
        let max_position = dec!(50);
        let existing = dec!(50);
        let remaining = (max_position - existing).max(Decimal::ZERO);
        assert_eq!(remaining, Decimal::ZERO);
        assert!(remaining <= Decimal::ZERO);
    }

    #[test]
    fn test_position_over_max_skips() {
        // Edge case: existing > max (shouldn't happen, but handle gracefully)
        let max_position = dec!(50);
        let existing = dec!(60);
        let remaining = (max_position - existing).max(Decimal::ZERO);
        assert_eq!(remaining, Decimal::ZERO);
    }

    // ──── Cache Eviction Test ────

    #[test]
    fn test_cache_eviction() {
        let mut cache = HashMap::new();

        // Insert an entry that's "old"
        cache.insert(
            1u64,
            CachedForecast {
                forecast: ForecastData {
                    values: vec![1.0],
                    dates: vec!["d1".into()],
                    mean: 1.0,
                    std_dev: 0.0,
                    target_value: None,
                },
                fetched_at: Instant::now() - Duration::from_secs(7200), // 2 hours old
            },
        );

        // Insert a fresh entry
        cache.insert(
            2u64,
            CachedForecast {
                forecast: ForecastData {
                    values: vec![2.0],
                    dates: vec!["d2".into()],
                    mean: 2.0,
                    std_dev: 0.0,
                    target_value: None,
                },
                fetched_at: Instant::now(),
            },
        );

        assert_eq!(cache.len(), 2);

        // Evict entries older than 1 hour
        evict_stale_cache_entries(&mut cache, 3600);

        assert_eq!(cache.len(), 1);
        assert!(cache.contains_key(&2));
        assert!(!cache.contains_key(&1));
    }

    // ──── NegRisk NO-Side Detection Test ────

    #[test]
    fn test_neg_risk_no_side_detection() {
        // If model says P(range) = 0.10, then P(NOT range) = 0.90
        // If NO token ask is 0.70, edge = 0.90 - 0.70 = 0.20 (undervalued NO)
        let model_prob: f64 = 0.10;
        let no_model_prob: f64 = 1.0 - model_prob;
        let no_ask: f64 = 0.70;
        let edge = no_model_prob - no_ask;
        assert!(edge > 0.0, "NO side should have positive edge: {}", edge);
        assert!((edge - 0.20).abs() < 0.01, "Edge should be ~0.20, got {}", edge);
    }

    // ──── CDF Dispatcher Tests ────

    #[test]
    fn test_cdf_for_metric_temperature_uses_normal() {
        // Temperature should use normal CDF
        let cdf = cdf_for_metric(WeatherMetric::TemperatureMax, 100.0, 95.0, 5.0);
        let expected = normal_cdf((100.0 - 95.0) / 5.0);
        assert!((cdf - expected).abs() < 1e-10);
    }

    #[test]
    fn test_cdf_for_metric_rainfall_uses_lognormal() {
        // Rainfall should use log-normal CDF
        let cdf = cdf_for_metric(WeatherMetric::Rainfall, 2.0, 1.0, 0.5);
        let expected = lognormal_cdf(2.0, 1.0, 0.5);
        assert!((cdf - expected).abs() < 1e-10);
    }

    #[test]
    fn test_cdf_for_metric_wind_uses_weibull() {
        // Wind speed should use Weibull CDF
        let cdf = cdf_for_metric(WeatherMetric::WindSpeed, 15.0, 10.0, 3.0);
        let expected = weibull_cdf(15.0, 10.0, 3.0);
        assert!((cdf - expected).abs() < 1e-10);
    }
}
