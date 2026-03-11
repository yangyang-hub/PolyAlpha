use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use alloy::primitives::U256;
use rust_decimal::prelude::ToPrimitive;
use async_trait::async_trait;
use chrono::{Datelike, Local, NaiveDate, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::Deserialize;
use uuid::Uuid;

use pa_core::config::{ForecastErrorConfig, WeatherConfig};
use pa_core::traits::Strategy;
use pa_core::types::{
    TradingOpportunity, ExecutionPlan, MarketInfo, NegRiskEvent, OrderBook, StrategyType,
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
///
/// When `dynamic_sigma` is true, scales the base sigma by `sqrt(max(1, days_to_event))`
/// to account for increasing forecast uncertainty over longer horizons.
pub fn sigma_for_metric(
    config: &ForecastErrorConfig,
    metric: WeatherMetric,
    days_to_event: Option<i64>,
    dynamic_sigma: bool,
) -> f64 {
    let base = match metric {
        WeatherMetric::TemperatureMax
        | WeatherMetric::TemperatureMin
        | WeatherMetric::TemperatureAvg => config.temperature_sigma_f,
        WeatherMetric::Rainfall => config.precipitation_sigma_in,
        WeatherMetric::Snowfall => config.snowfall_sigma_in,
        WeatherMetric::WindSpeed => config.wind_sigma_mph,
    };

    if dynamic_sigma {
        let days = days_to_event.unwrap_or(1).max(1) as f64;
        base * days.sqrt()
    } else {
        base
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
pub(crate) fn contains_word(text: &str, word: &str) -> bool {
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

    // "in February" / "by end of February" — bare month name without day
    // Default to the last day of the month.
    // Require a preposition before the month name to avoid false matches
    // (e.g. "may" as modal verb in "temperatures may exceed").
    for &(name, month_num) in MONTHS {
        if has_month_with_context(&lower, name) {
            let year = today.year();
            // Last day of month: go to 1st of next month, subtract 1 day
            let next_month = if month_num == 12 {
                NaiveDate::from_ymd_opt(year + 1, 1, 1)
            } else {
                NaiveDate::from_ymd_opt(year, month_num + 1, 1)
            };
            if let Some(first_next) = next_month {
                return Some(first_next - chrono::Duration::days(1));
            }
        }
    }

    None
}

/// Check if a month name appears in text with a preposition context.
/// Avoids false matches like "may" (modal verb) in "temperatures may exceed".
fn has_month_with_context(lower: &str, month_name: &str) -> bool {
    for prefix in &["in ", "by ", "of ", "for ", "during ", "through ", "before ", "after ", "until "] {
        let pattern = format!("{}{}", prefix, month_name);
        if lower.contains(&pattern) {
            return true;
        }
    }
    false
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

/// Detect if a temperature question uses Celsius (°C).
///
/// Markets on Polymarket use °C for non-US cities (Wellington, Toronto, Paris, etc.)
/// and °F for US cities (Dallas, Miami, NYC, etc.).
pub fn is_celsius_market(text: &str) -> bool {
    text.contains("°C") || text.contains("°c") || text.to_lowercase().contains("celsius")
}

/// Convert Celsius to Fahrenheit.
fn celsius_to_fahrenheit(c: f64) -> f64 {
    c * 9.0 / 5.0 + 32.0
}

/// Returns true if the metric is a temperature type.
fn is_temperature_metric(metric: WeatherMetric) -> bool {
    matches!(
        metric,
        WeatherMetric::TemperatureMax | WeatherMetric::TemperatureMin | WeatherMetric::TemperatureAvg
    )
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

// ──── NOAA Client ────

/// Weather forecast data (API-agnostic interface).
#[derive(Debug, Clone)]
pub struct ForecastData {
    pub values: Vec<f64>,
    pub dates: Vec<String>,
    pub mean: f64,
    pub std_dev: f64,
    /// Single-day value when a specific target date was requested.
    pub target_value: Option<f64>,
    /// Cross-model standard deviation (always 0.0 for NOAA — single model).
    pub model_spread: f64,
}

/// Hardcoded US city coordinates for NOAA lookup (NOAA has no geocoding API).
const US_CITY_COORDS: &[(&str, f64, f64)] = &[
    ("New York", 40.7128, -74.0060),
    ("NYC", 40.7128, -74.0060),
    ("Chicago", 41.8781, -87.6298),
    ("Los Angeles", 34.0522, -118.2437),
    ("LA", 34.0522, -118.2437),
    ("Houston", 29.7604, -95.3698),
    ("Phoenix", 33.4484, -112.0740),
    ("Miami", 25.7617, -80.1918),
    ("Philadelphia", 39.9526, -75.1652),
    ("San Antonio", 29.4241, -98.4936),
    ("San Diego", 32.7157, -117.1611),
    ("Dallas", 32.7767, -96.7970),
    ("Austin", 30.2672, -97.7431),
    ("San Francisco", 37.7749, -122.4194),
    ("Seattle", 47.6062, -122.3321),
    ("Denver", 39.7392, -104.9903),
    ("Nashville", 36.1627, -86.7816),
    ("Portland", 45.5152, -122.6784),
    ("Las Vegas", 36.1699, -115.1398),
    ("Atlanta", 33.7490, -84.3880),
    ("Minneapolis", 44.9778, -93.2650),
    ("Tampa", 27.9506, -82.4572),
    ("New Orleans", 29.9511, -90.0715),
    ("Cleveland", 41.4993, -81.6944),
];

/// Cached NOAA grid point (office, gridX, gridY). Grid points never change.
#[derive(Debug, Clone)]
struct CachedGridPoint {
    office: String,
    grid_x: u32,
    grid_y: u32,
}

/// NOAA /points response.
#[derive(Debug, Deserialize)]
struct PointsResponse {
    properties: PointsProperties,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PointsProperties {
    grid_id: String,
    grid_x: u32,
    grid_y: u32,
}

/// NOAA /gridpoints response.
#[derive(Debug, Deserialize)]
struct GridpointsResponse {
    properties: GridpointsProperties,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GridpointsProperties {
    #[serde(default)]
    max_temperature: Option<NoaaTimeSeries>,
    #[serde(default)]
    min_temperature: Option<NoaaTimeSeries>,
    #[serde(default)]
    temperature: Option<NoaaTimeSeries>,
    #[serde(default)]
    quantitative_precipitation: Option<NoaaTimeSeries>,
    #[serde(default)]
    snowfall_amount: Option<NoaaTimeSeries>,
    #[serde(default)]
    wind_speed: Option<NoaaTimeSeries>,
}

/// NOAA time series container.
#[derive(Debug, Deserialize)]
struct NoaaTimeSeries {
    values: Vec<NoaaTimeValue>,
}

/// Single NOAA time series value.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NoaaTimeValue {
    valid_time: String,
    value: Option<f64>,
}

/// Parse ISO 8601 date from NOAA validTime format: "2026-03-10T06:00:00+00:00/PT6H"
fn parse_noaa_date(valid_time: &str) -> Option<NaiveDate> {
    // Take the date portion before 'T'
    let date_str = valid_time.split('T').next()?;
    NaiveDate::parse_from_str(date_str, "%Y-%m-%d").ok()
}

/// Convert km/h to mph.
fn kmh_to_mph(kmh: f64) -> f64 {
    kmh * 0.621371
}

/// Convert mm to inches.
fn mm_to_inches(mm: f64) -> f64 {
    mm / 25.4
}

/// Fetch weather forecasts from the NOAA API (US-only, free, no API key).
pub struct NoaaClient {
    http: reqwest::Client,
    grid_cache: Arc<Mutex<HashMap<String, CachedGridPoint>>>,
}

impl NoaaClient {
    pub fn new(user_agent: &str) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .user_agent(user_agent)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            http,
            grid_cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Look up US city coordinates from the hardcoded table.
    /// Returns `Err` for non-US cities (NOAA only covers the US).
    pub fn geocode(location: &str) -> anyhow::Result<(f64, f64)> {
        let loc_lower = location.to_lowercase();
        for &(name, lat, lon) in US_CITY_COORDS {
            if loc_lower == name.to_lowercase() {
                return Ok((lat, lon));
            }
        }
        Err(anyhow::anyhow!("City not in US lookup table: {}", location))
    }

    /// Resolve lat/lon to NOAA grid point (office, gridX, gridY).
    /// Results are cached permanently (grid points don't change).
    async fn resolve_grid(&self, lat: f64, lon: f64) -> anyhow::Result<(String, u32, u32)> {
        let cache_key = format!("{:.4},{:.4}", lat, lon);

        // Check cache
        {
            let cache = self.grid_cache.lock().unwrap();
            if let Some(entry) = cache.get(&cache_key) {
                return Ok((entry.office.clone(), entry.grid_x, entry.grid_y));
            }
        }

        // Fetch from NOAA
        let url = format!("https://api.weather.gov/points/{:.4},{:.4}", lat, lon);
        let http = &self.http;
        let url_clone = url.clone();
        let resp: PointsResponse = with_retry(2, || {
            let u = url_clone.clone();
            let h = http.clone();
            async move {
                let r: PointsResponse = h
                    .get(&u)
                    .header("Accept", "application/geo+json")
                    .send()
                    .await?
                    .json()
                    .await?;
                Ok(r)
            }
        })
        .await?;

        let office = resp.properties.grid_id;
        let grid_x = resp.properties.grid_x;
        let grid_y = resp.properties.grid_y;

        // Cache the result
        {
            let mut cache = self.grid_cache.lock().unwrap();
            cache.insert(cache_key, CachedGridPoint {
                office: office.clone(),
                grid_x,
                grid_y,
            });
        }

        Ok((office, grid_x, grid_y))
    }

    /// Fetch daily weather forecast from NOAA gridpoints API.
    ///
    /// NOAA returns SI units; this function converts to US units:
    /// - Temperature: °C → °F
    /// - Wind speed: km/h → mph
    /// - Precipitation/snow: mm → inches
    pub async fn forecast(
        &self,
        lat: f64,
        lon: f64,
        metric: WeatherMetric,
        target_date: Option<NaiveDate>,
        _precipitation_unit: &str,
    ) -> anyhow::Result<ForecastData> {
        let (office, grid_x, grid_y) = self.resolve_grid(lat, lon).await?;
        let url = format!(
            "https://api.weather.gov/gridpoints/{}/{},{}",
            office, grid_x, grid_y
        );

        let http = &self.http;
        let url_clone = url.clone();
        let resp: GridpointsResponse = with_retry(2, || {
            let u = url_clone.clone();
            let h = http.clone();
            async move {
                let r: GridpointsResponse = h
                    .get(&u)
                    .header("Accept", "application/geo+json")
                    .send()
                    .await?
                    .json()
                    .await?;
                Ok(r)
            }
        })
        .await?;

        // Select the appropriate time series
        let series = match metric {
            WeatherMetric::TemperatureMax => resp.properties.max_temperature,
            WeatherMetric::TemperatureMin => resp.properties.min_temperature,
            WeatherMetric::TemperatureAvg => resp.properties.temperature,
            WeatherMetric::Rainfall => resp.properties.quantitative_precipitation,
            WeatherMetric::Snowfall => resp.properties.snowfall_amount,
            WeatherMetric::WindSpeed => resp.properties.wind_speed,
        };

        let series = series.ok_or_else(|| {
            anyhow::anyhow!("NOAA response missing data for metric {:?}", metric)
        })?;

        // Group values by date, applying unit conversions
        let is_temp = is_temperature_metric(metric);
        let is_precip = matches!(metric, WeatherMetric::Rainfall | WeatherMetric::Snowfall);
        let is_wind = matches!(metric, WeatherMetric::WindSpeed);

        let mut daily_values: HashMap<NaiveDate, Vec<f64>> = HashMap::new();
        for tv in &series.values {
            let Some(raw_val) = tv.value else { continue };
            let Some(date) = parse_noaa_date(&tv.valid_time) else { continue };

            // Convert SI → US units
            let val = if is_temp {
                celsius_to_fahrenheit(raw_val)
            } else if is_wind {
                kmh_to_mph(raw_val)
            } else if is_precip {
                mm_to_inches(raw_val)
            } else {
                raw_val
            };

            daily_values.entry(date).or_default().push(val);
        }

        if daily_values.is_empty() {
            return Err(anyhow::anyhow!("No forecast data from NOAA"));
        }

        // Aggregate per day: temperature → max/min/mean, precip → sum, wind → max
        let mut sorted_dates: Vec<NaiveDate> = daily_values.keys().copied().collect();
        sorted_dates.sort();

        let mut values = Vec::new();
        let mut dates = Vec::new();
        for date in &sorted_dates {
            let day_vals = &daily_values[date];
            let agg = match metric {
                WeatherMetric::TemperatureMax => day_vals.iter().copied().fold(f64::NEG_INFINITY, f64::max),
                WeatherMetric::TemperatureMin => day_vals.iter().copied().fold(f64::INFINITY, f64::min),
                WeatherMetric::TemperatureAvg => day_vals.iter().sum::<f64>() / day_vals.len() as f64,
                WeatherMetric::Rainfall | WeatherMetric::Snowfall => day_vals.iter().sum::<f64>(),
                WeatherMetric::WindSpeed => day_vals.iter().copied().fold(f64::NEG_INFINITY, f64::max),
            };
            values.push(agg);
            dates.push(date.format("%Y-%m-%d").to_string());
        }

        // If a target date is requested, filter to that day
        let target_value = if let Some(td) = target_date {
            let idx = sorted_dates.iter().position(|d| *d == td);
            idx.map(|i| values[i])
        } else {
            None
        };

        let mean = values.iter().sum::<f64>() / values.len() as f64;
        let variance = values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / values.len() as f64;
        let std_dev = variance.sqrt();

        Ok(ForecastData {
            values,
            dates,
            mean,
            std_dev,
            target_value,
            model_spread: 0.0, // NOAA is a single model
        })
    }

    /// NOAA doesn't have a convenient historical API.
    /// Returns Err so stale liquidity detection gracefully skips.
    pub async fn fetch_historical(
        &self,
        _lat: f64,
        _lon: f64,
        _metric: WeatherMetric,
        _target_date: NaiveDate,
        _precipitation_unit: &str,
    ) -> anyhow::Result<f64> {
        Err(anyhow::anyhow!("NOAA historical API not available"))
    }
}

// ──── HTTP Retry Helper ────

/// Retry an async operation with exponential backoff.
/// Delays: 500ms, 1s, 2s, ...
pub(crate) async fn with_retry<T, F, Fut>(max_retries: u32, f: F) -> anyhow::Result<T>
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
/// When mean <= 0 (no precipitation expected), returns 1.0 for any t > 0
/// (almost certain the amount is below any positive threshold).
pub fn lognormal_cdf(t: f64, mean: f64, sigma: f64) -> f64 {
    if t <= 0.0 {
        return 0.0;
    }
    if mean <= 0.0 {
        // No precipitation expected: P(X ≤ t) ≈ 1 for any t > 0
        return 1.0;
    }
    if sigma <= 0.0 {
        // Degenerate: point mass at mean
        return if t >= mean { 1.0 } else { 0.0 };
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
/// When mean <= 0 (no wind expected), returns 1.0 for any t > 0.
pub fn weibull_cdf(t: f64, mean: f64, _sigma: f64) -> f64 {
    if t <= 0.0 {
        return 0.0;
    }
    if mean <= 0.0 {
        // No wind expected: P(X ≤ t) ≈ 1 for any t > 0
        return 1.0;
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
                if mean <= value { 1.0 } else { 0.0 }
            } else {
                normal_cdf((value - mean) / sigma)
            }
        }
        WeatherMetric::Rainfall | WeatherMetric::Snowfall => lognormal_cdf(value, mean, sigma),
        WeatherMetric::WindSpeed => weibull_cdf(value, mean, sigma),
    }
}

/// Compute effective sigma combining forecast std_dev, forecast error, and model spread.
///
/// For date-specific forecasts (target_value.is_some()): sqrt(forecast_error² + model_spread²).
/// For multi-day forecasts: sqrt(std_dev² + forecast_error² + model_spread²).
/// When model_spread is 0.0, this degrades to the original behavior.
fn effective_sigma(forecast: &ForecastData, forecast_error_sigma: f64) -> f64 {
    if forecast.target_value.is_some() {
        // Date-specific: day-to-day variance is irrelevant
        (forecast_error_sigma.powi(2) + forecast.model_spread.powi(2)).sqrt()
    } else {
        // Multi-day: combine observed variance with forecast error and model spread
        (forecast.std_dev.powi(2) + forecast_error_sigma.powi(2) + forecast.model_spread.powi(2)).sqrt()
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

// ──── Forecast Change Detection ────

/// Check whether the new forecast value represents a significant change from the previous one.
///
/// Returns `true` if:
/// - There is no previous value (first observation → always significant)
/// - `|new_value - previous| > threshold * sigma`
///
/// Returns `false` if the change is below the threshold.
fn is_significant_change(new_value: f64, previous: Option<f64>, sigma: f64, threshold: f64) -> bool {
    match previous {
        None => true, // First observation is always fresh
        Some(prev) => {
            if sigma <= 0.0 {
                // Avoid division issues; any non-zero change is significant
                (new_value - prev).abs() > 1e-10
            } else {
                (new_value - prev).abs() > threshold * sigma
            }
        }
    }
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
    /// Previous forecast mean (for change detection).
    previous_mean: Option<f64>,
    /// Whether this forecast represents a significant change from the previous one.
    is_fresh_signal: bool,
}

pub struct WeatherAlphaStrategy {
    config: WeatherConfig,
    noaa: NoaaClient,
    profit_calc: ProfitCalculator,
    get_orderbook: Box<dyn Fn(U256) -> Option<OrderBook> + Send + Sync>,
    /// Returns available capital (balance - exposure) for position sizing.
    get_available_capital: Box<dyn Fn() -> Decimal + Send + Sync>,
    /// Returns existing position size for a given token (for dedup/cap).
    get_position: Box<dyn Fn(U256) -> Decimal + Send + Sync>,
    /// Returns all held positions for this strategy: (token_id, size, avg_cost).
    get_held_positions: Box<dyn Fn() -> Vec<(U256, Decimal, Decimal)> + Send + Sync>,
    /// Returns current wallet USDC balance for dynamic position sizing.
    get_balance: Box<dyn Fn() -> Decimal + Send + Sync>,
    forecast_cache: Arc<Mutex<HashMap<u64, CachedForecast>>>,
    /// NegRisk multi-outcome weather events to scan.
    neg_risk_events: Vec<NegRiskEvent>,
    /// Scan counter for periodic diagnostics.
    scan_count: Arc<std::sync::atomic::AtomicU64>,
}

impl WeatherAlphaStrategy {
    pub fn new(
        config: WeatherConfig,
        gas_cost_usd: Decimal,
        get_orderbook: Box<dyn Fn(U256) -> Option<OrderBook> + Send + Sync>,
        get_available_capital: Box<dyn Fn() -> Decimal + Send + Sync>,
        get_position: Box<dyn Fn(U256) -> Decimal + Send + Sync>,
        neg_risk_events: Vec<NegRiskEvent>,
        get_held_positions: Box<dyn Fn() -> Vec<(U256, Decimal, Decimal)> + Send + Sync>,
        get_balance: Box<dyn Fn() -> Decimal + Send + Sync>,
    ) -> Self {
        let noaa = NoaaClient::new(&config.noaa_user_agent);
        Self {
            config,
            noaa,
            profit_calc: ProfitCalculator::new(gas_cost_usd),
            get_orderbook,
            get_available_capital,
            get_position,
            get_held_positions,
            get_balance,
            forecast_cache: Arc::new(Mutex::new(HashMap::new())),
            neg_risk_events,
            scan_count: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        }
    }

    /// Hash a question string for cache key.
    fn question_hash(question: &str) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        question.to_lowercase().hash(&mut hasher);
        hasher.finish()
    }

    /// Hash location + metric + date for cache key (used by get_forecast_by_location).
    #[cfg(test)]
    fn location_hash(location: &str, metric: WeatherMetric, target_date: Option<NaiveDate>) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        location.to_lowercase().hash(&mut hasher);
        (metric as u8).hash(&mut hasher);
        target_date.map(|d| d.num_days_from_ce()).hash(&mut hasher);
        hasher.finish()
    }

    /// Check if a location is in the target cities list.
    ///
    /// When target_cities is non-empty, only markets in these cities are scanned.
    /// Supports city name aliases (NYC → New York, LA → Los Angeles).
    fn is_target_city(&self, location: &str) -> bool {
        if self.config.target_cities.is_empty() {
            return true; // No filter, all cities pass
        }
        let loc_lower = location.to_lowercase();
        self.config.target_cities.iter().any(|city| {
            let city_lower = city.to_lowercase();
            loc_lower.contains(&city_lower) || {
                // Alias matching
                if city_lower == "new york" {
                    loc_lower.contains("nyc") || loc_lower.contains("new york")
                } else if city_lower == "los angeles" {
                    loc_lower.contains("la ") || loc_lower == "la" || loc_lower.contains("los angeles")
                } else {
                    false
                }
            }
        })
    }

    // ──── Stale Liquidity Detection ────

    /// Scan markets for stale liquidity opportunities.
    ///
    /// When the target date has passed and actual weather data confirms an outcome,
    /// but order book prices haven't updated yet (the "8-hour lag" phenomenon),
    /// we can aggressively buy the underpriced token.
    async fn scan_stale_liquidity(&self, markets: &[MarketInfo]) -> Vec<TradingOpportunity> {
        let mut opportunities = Vec::new();
        let today = Local::now().date_naive();

        for market in markets {
            if !market.active || market.neg_risk {
                continue;
            }

            let parsed = match parse_weather_question(&market.question) {
                Some(p) => p,
                None => continue,
            };

            // Only check markets where target date is today or in the past
            let target_date = match parse_target_date(&market.question) {
                Some(d) => d,
                None => continue, // Skip markets without a clear date
            };

            // Skip if target date is in the future
            if target_date > today {
                continue;
            }

            // Skip if too old (more than 7 days past)
            if (today - target_date).num_days() > 7 {
                continue;
            }

            // Geocode the location
            let coords = match NoaaClient::geocode(&parsed.location) {
                Ok(c) => c,
                Err(e) => {
                    tracing::debug!(location = %parsed.location, error = %e, "Stale check: geocode failed");
                    continue;
                }
            };

            // Detect precipitation unit
            let precipitation_unit = if matches!(parsed.metric, WeatherMetric::Rainfall | WeatherMetric::Snowfall) {
                detect_precipitation_unit(&market.question)
            } else {
                "inch"
            };

            // Fetch actual historical data
            let actual_value = match self.noaa.fetch_historical(
                coords.0,
                coords.1,
                parsed.metric,
                target_date,
                precipitation_unit,
            ).await {
                Ok(v) => v,
                Err(e) => {
                    tracing::debug!(
                        location = %parsed.location,
                        date = %target_date,
                        error = %e,
                        "Stale check: historical data unavailable"
                    );
                    continue;
                }
            };

            // Determine if outcome is confirmed
            let should_trigger_yes = match parsed.comparison {
                Comparison::Above | Comparison::AtLeast => actual_value > parsed.threshold,
                Comparison::Below | Comparison::AtMost => actual_value < parsed.threshold,
            };

            // For the opposite side, check if the threshold is definitively NOT met
            let should_trigger_no = match parsed.comparison {
                Comparison::Above | Comparison::AtLeast => actual_value <= parsed.threshold,
                Comparison::Below | Comparison::AtMost => actual_value >= parsed.threshold,
            };

            // Get order books for both sides
            if market.tokens.len() < 2 {
                continue;
            }

            let yes_token = &market.tokens[0];
            let no_token = &market.tokens[1];

            let yes_book = match (self.get_orderbook)(yes_token.token_id) {
                Some(b) => b,
                None => continue,
            };
            let no_book = match (self.get_orderbook)(no_token.token_id) {
                Some(b) => b,
                None => continue,
            };

            // Check if prices are stale (YES price should be near 1.0 when confirmed, NO near 0.0)
            let yes_ask = match yes_book.best_ask() {
                Some(l) => l.price,
                None => continue,
            };
            let no_ask = match no_book.best_ask() {
                Some(l) => l.price,
                None => continue,
            };

            // If YES is confirmed but price is still low (< 0.90), buy YES
            if should_trigger_yes && yes_ask < dec!(0.90) {
                let model_prob = Decimal::ONE; // Certain outcome
                let edge = model_prob - yes_ask;
                if edge > Decimal::ZERO {
                    let effective_max = (self.get_balance)() * self.config.max_position_pct;
                    let size = effective_max.min((self.get_available_capital)());
                    
                    if size > Decimal::ZERO {
                        let est = self.profit_calc.directional_buy_profit(
                            yes_ask,
                            model_prob,
                            size,
                            market.fee_rate_bps,
                        );
                        if est.net_profit > Decimal::ZERO {
                            opportunities.push(TradingOpportunity {
                                id: Uuid::now_v7(),
                                strategy_type: StrategyType::Weather,
                                condition_id: market.condition_id,
                                question: format!("[STALE] {}", market.question),
                                spread: edge,
                                estimated_profit: est.net_profit,
                                size,
                                detected_at: Utc::now(),
                                execution_plan: ExecutionPlan::DirectionalBuy {
                                    token_id: yes_token.token_id,
                                    side: TradeSide::Buy,
                                    price: yes_ask,
                                    size,
                                    condition_id: market.condition_id,
                                },
                            });
                        }
                    }
                }
            }

            // If NO is confirmed but price is still low (< 0.90), buy NO
            if should_trigger_no && no_ask < dec!(0.90) {
                let model_prob = Decimal::ONE; // Certain outcome
                let edge = model_prob - no_ask;
                if edge > Decimal::ZERO {
                    let effective_max = (self.get_balance)() * self.config.max_position_pct;
                    let size = effective_max.min((self.get_available_capital)());
                    
                    if size > Decimal::ZERO {
                        let est = self.profit_calc.directional_buy_profit(
                            no_ask,
                            model_prob,
                            size,
                            market.fee_rate_bps,
                        );
                        if est.net_profit > Decimal::ZERO {
                            opportunities.push(TradingOpportunity {
                                id: Uuid::now_v7(),
                                strategy_type: StrategyType::Weather,
                                condition_id: market.condition_id,
                                question: format!("[STALE] {}", market.question),
                                spread: edge,
                                estimated_profit: est.net_profit,
                                size,
                                detected_at: Utc::now(),
                                execution_plan: ExecutionPlan::DirectionalBuy {
                                    token_id: no_token.token_id,
                                    side: TradeSide::Buy,
                                    price: no_ask,
                                    size,
                                    condition_id: market.condition_id,
                                },
                            });
                        }
                    }
                }
            }
        }

        opportunities
    }

    // ──── NegRisk Surround Strategy ────

    /// Detect NegRisk surround opportunities (Early Game strategy).
    ///
    /// When the forecast distribution has a clear peak, buys the peak bin
    /// plus adjacent bins to "surround" the likely outcome. This hedges
    /// against forecast uncertainty while capturing the high-probability region.
    async fn detect_neg_risk_surround(
        &self,
        event: &NegRiskEvent,
        metric: WeatherMetric,
        location: &str,
    ) -> Vec<TradingOpportunity> {
        let mut opportunities = Vec::new();

        // Parse target date
        let target_date = match parse_target_date(&event.title) {
            Some(d) => d,
            None => return opportunities, // Skip if no clear date
        };

        // Only use surround strategy when event is more than 24 hours away
        let today = Local::now().date_naive();
        if (target_date - today).num_days() < 1 {
            return opportunities; // Too close to event, use regular detection
        }

        // Get forecast
        let precipitation_unit = if matches!(metric, WeatherMetric::Rainfall | WeatherMetric::Snowfall) {
            detect_precipitation_unit(&event.title)
        } else {
            "inch"
        };

        let forecast = match self.get_forecast_by_location(location, metric, Some(target_date), precipitation_unit).await {
            Some((f, _)) => f,
            None => return opportunities,
        };

        // Evaluate each outcome bin
        let mut bin_evals: Vec<(OutcomeRange, Decimal, &MarketInfo)> = Vec::new();
        for market in &event.markets {
            if !market.active || market.tokens.len() < 2 {
                continue;
            }

            let range = match parse_outcome_range(&market.question) {
                Some(r) => r,
                None => continue,
            };

            // Convert Celsius to Fahrenheit if needed
            let range = if is_temperature_metric(metric) && is_celsius_market(&market.question) {
                OutcomeRange {
                    lower: range.lower.map(celsius_to_fahrenheit),
                    upper: range.upper.map(celsius_to_fahrenheit),
                }
            } else {
                range
            };

            let sigma = sigma_for_metric(&self.config.forecast_error, metric, Some((target_date - today).num_days()), self.config.dynamic_sigma);
            let prob_f64 = model_range_probability(&forecast, &range, sigma, metric);
            let model_prob = match Decimal::from_f64_retain(prob_f64) {
                Some(d) => d,
                None => continue,
            };

            bin_evals.push((range, model_prob, market));
        }

        if bin_evals.is_empty() {
            return opportunities;
        }

        // Find peak probability bin
        let peak_prob = bin_evals.iter()
            .map(|(_, p, _)| *p)
            .fold(Decimal::ZERO, |a, b| a.max(b));

        if peak_prob < dec!(0.40) {
            return opportunities; // No clear peak, skip surround
        }

        // Calculate surround size: 50% of max position per bin
        let effective_max = (self.get_balance)() * self.config.max_position_pct;
        let surround_size_per_bin = effective_max / Decimal::from(2); // Use half of max for surround
        let min_prob = peak_prob * dec!(0.50); // Adjacent bins must have at least 50% of peak prob

        let mut bought_bins = 0u8;

        for (_range, model_prob, market) in &bin_evals {
            // Skip if probability is too low
            if *model_prob < min_prob {
                continue;
            }

            // Check if we have meaningful edge
            let yes_book = match (self.get_orderbook)(market.tokens[0].token_id) {
                Some(b) => b,
                None => continue,
            };
            let yes_ask = match yes_book.best_ask() {
                Some(l) => l.price,
                None => continue,
            };

            if *model_prob > yes_ask {
                let edge = *model_prob - yes_ask;
                let edge_bps = (edge * dec!(10000)).to_u32().unwrap_or(0);

                if edge_bps < self.config.min_edge_bps {
                    continue;
                }

                let size = surround_size_per_bin.min((self.get_available_capital)());
                if size <= Decimal::ZERO {
                    continue;
                }

                let existing_cost = (self.get_position)(market.tokens[0].token_id) * yes_ask;
                let remaining = (surround_size_per_bin - existing_cost).max(Decimal::ZERO);
                let final_size = size.min(remaining);

                if final_size <= Decimal::ZERO {
                    continue;
                }

                let est = self.profit_calc.directional_buy_profit(
                    yes_ask,
                    *model_prob,
                    final_size,
                    market.fee_rate_bps,
                );

                if est.net_profit <= Decimal::ZERO {
                    continue;
                }

                opportunities.push(TradingOpportunity {
                    id: Uuid::now_v7(),
                    strategy_type: StrategyType::Weather,
                    condition_id: market.condition_id,
                    question: format!("[SURROUND] {} → {}", event.title, market.question),
                    spread: edge,
                    estimated_profit: est.net_profit,
                    size: final_size,
                    detected_at: Utc::now(),
                    execution_plan: ExecutionPlan::DirectionalBuy {
                        token_id: market.tokens[0].token_id,
                        side: TradeSide::Buy,
                        price: yes_ask,
                        size: final_size,
                        condition_id: market.condition_id,
                    },
                });
                bought_bins += 1;
            }

            // Limit surround to at most 3 bins (peak + 2 adjacent)
            if bought_bins >= 3 {
                break;
            }
        }

        opportunities
    }

    // ──── Mid Game Dynamic Trimming ────
    // Note: Dynamic trimming functionality is integrated into scan_exits for NegRisk positions.
    // The exit logic already handles model reversal which effectively trims losing positions
    // when the forecast moves against them.


    /// Get cached forecast or fetch new one.    }

    /// Get cached forecast or fetch new one.
    /// Returns (forecast, is_fresh_signal) where is_fresh_signal indicates a significant change.
    async fn get_forecast(
        &self,
        question: &str,
        parsed: &WeatherQuestion,
        target_date: Option<NaiveDate>,
        precipitation_unit: &str,
    ) -> Option<(ForecastData, bool)> {
        let key = Self::question_hash(question);

        // Check cache
        {
            let cache = self.forecast_cache.lock().unwrap();
            if let Some(entry) = cache.get(&key)
                && entry.fetched_at.elapsed().as_secs() < self.config.refresh_interval_secs
            {
                return Some((entry.forecast.clone(), entry.is_fresh_signal));
            }
        }

        // Geocode via lookup table
        let coords = match NoaaClient::geocode(&parsed.location) {
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

        // Fetch from NOAA
        let forecast = match self
            .noaa
            .forecast(coords.0, coords.1, parsed.metric, target_date, precipitation_unit)
            .await
        {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!(
                    location = %parsed.location,
                    metric = ?parsed.metric,
                    error = %e,
                    "Failed to fetch forecast from NOAA"
                );
                return None;
            }
        };

        // Compute change detection signal
        let new_mean = forecast.target_value.unwrap_or(forecast.mean);
        let (previous_mean, is_fresh_signal) = {
            let cache = self.forecast_cache.lock().unwrap();
            let prev = cache.get(&key).and_then(|e| e.previous_mean);
            let base_sigma = sigma_for_metric(
                &self.config.forecast_error,
                parsed.metric,
                None,
                false,
            );
            let fresh = is_significant_change(
                new_mean,
                prev,
                base_sigma,
                self.config.forecast_change_threshold,
            );
            (Some(new_mean), fresh)
        };

        // Cache the result and evict stale entries
        {
            let mut cache = self.forecast_cache.lock().unwrap();
            cache.insert(
                key,
                CachedForecast {
                    forecast: forecast.clone(),
                    fetched_at: Instant::now(),
                    previous_mean,
                    is_fresh_signal,
                },
            );
            evict_stale_cache_entries(&mut cache, self.config.refresh_interval_secs * 2);
        }

        Some((forecast, is_fresh_signal))
    }

    /// Detect a directional opportunity on a single weather market.
    async fn detect_weather_opportunity(
        &self,
        market: &MarketInfo,
        parsed: &WeatherQuestion,
    ) -> Option<TradingOpportunity> {
        // Parse target date and precipitation unit from question
        let target_date = parse_target_date(&market.question);
        let precipitation_unit = if matches!(parsed.metric, WeatherMetric::Rainfall | WeatherMetric::Snowfall) {
            detect_precipitation_unit(&market.question)
        } else {
            "inch"
        };

        let (forecast, is_fresh) = self
            .get_forecast(&market.question, parsed, target_date, precipitation_unit)
            .await?;

        // Skip if forecast hasn't changed significantly (when change detection is enabled)
        if self.config.forecast_change_detection && !is_fresh {
            return None;
        }

        let days_to_event = target_date.map(|d| {
            (d - Local::now().date_naive()).num_days()
        });
        let forecast_error_sigma = sigma_for_metric(
            &self.config.forecast_error,
            parsed.metric,
            days_to_event,
            self.config.dynamic_sigma,
        );
        // Convert °C threshold to °F since forecast is always in Fahrenheit
        let threshold = if is_temperature_metric(parsed.metric) && is_celsius_market(&market.question) {
            celsius_to_fahrenheit(parsed.threshold)
        } else {
            parsed.threshold
        };
        let model_prob = model_probability(
            &forecast,
            threshold,
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
        let yes_bid = yes_book.best_bid()?.price;
        let no_bid = no_book.best_bid()?.price;

        // Check bid-ask spread on both sides
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

        if spread_bps > self.config.max_spread_bps {
            tracing::debug!(
                question = %market.question,
                yes_spread_bps = %((yes_spread * dec!(10000)).round()),
                no_spread_bps = %((no_spread * dec!(10000)).round()),
                max_allowed = self.config.max_spread_bps,
                "[Weather] Rejecting: spread too wide"
            );
            return None;
        }

        // Model says event is likely (buy YES) or unlikely (buy NO)
        let model_prob_dec = Decimal::from_f64_retain(model_prob)?;
        let no_model_prob = Decimal::ONE - model_prob_dec;

        // Compute edge on both sides and pick the larger one
        // Only consider sides where the ask price is below max_entry_price
        let yes_edge = if model_prob_dec > yes_ask && yes_ask <= self.config.max_entry_price {
            Some(model_prob_dec - yes_ask)
        } else {
            None
        };
        let no_edge = if no_model_prob > no_ask && no_ask <= self.config.max_entry_price {
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

        // Fixed position sizing: max_position_usdc per trade, position-aware
        let max_usdc = self.config.max_position_usdc;
        let available = (self.get_available_capital)();
        let existing_cost = (self.get_position)(token_id) * ask_price;
        let remaining = (max_usdc - existing_cost).max(Decimal::ZERO);
        let size = max_usdc.min(remaining).min(available);

        // After capping, verify we still meet CLOB minimum ($1.00 cost).
        // E.g. at price $0.019, min_cost_size=53 but available may only allow 2 shares.
        if size <= Decimal::ZERO || (ask_price > Decimal::ZERO && size * ask_price < Decimal::ONE) {
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

        tracing::debug!(
            question = %market.question,
            metric = ?parsed.metric,
            location = %parsed.location,
            threshold = parsed.threshold,
            forecast_mean = forecast.mean,
            model_prob = model_prob,
            ask_price = %ask_price,
            edge_bps = edge_bps,
            available_capital = %available,
            existing_position = %existing_cost,
            size = %size,
            est_profit = %est.net_profit,
            "Weather alpha opportunity detected"
        );

        Some(TradingOpportunity {
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
    /// Returns (forecast, is_fresh_signal) where is_fresh_signal indicates a significant change.
    async fn get_forecast_by_location(
        &self,
        location: &str,
        metric: WeatherMetric,
        target_date: Option<NaiveDate>,
        precipitation_unit: &str,
    ) -> Option<(ForecastData, bool)> {
        // Use location+metric as cache key
        let cache_key = {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            location.to_lowercase().hash(&mut hasher);
            (metric as u8).hash(&mut hasher);
            target_date.map(|d| d.num_days_from_ce()).hash(&mut hasher);
            hasher.finish()
        };

        // Check cache
        {
            let cache = self.forecast_cache.lock().unwrap();
            if let Some(entry) = cache.get(&cache_key)
                && entry.fetched_at.elapsed().as_secs() < self.config.refresh_interval_secs
            {
                return Some((entry.forecast.clone(), entry.is_fresh_signal));
            }
        }

        // Fetch from API
        let coords = match NoaaClient::geocode(location) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(location = %location, error = %e, "Failed to geocode for NegRisk weather");
                return None;
            }
        };

        // Fetch from NOAA
        let forecast = match self
            .noaa
            .forecast(coords.0, coords.1, metric, target_date, precipitation_unit)
            .await
        {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!(location = %location, metric = ?metric, error = %e, "Failed to fetch forecast from NOAA for NegRisk weather");
                return None;
            }
        };

        // Compute change detection signal
        let new_mean = forecast.target_value.unwrap_or(forecast.mean);
        let (previous_mean, is_fresh_signal) = {
            let cache = self.forecast_cache.lock().unwrap();
            let prev = cache.get(&cache_key).and_then(|e| e.previous_mean);
            let base_sigma = sigma_for_metric(
                &self.config.forecast_error,
                metric,
                None,
                false,
            );
            let fresh = is_significant_change(
                new_mean,
                prev,
                base_sigma,
                self.config.forecast_change_threshold,
            );
            (Some(new_mean), fresh)
        };

        // Cache and evict stale entries
        {
            let mut cache = self.forecast_cache.lock().unwrap();
            cache.insert(
                cache_key,
                CachedForecast {
                    forecast: forecast.clone(),
                    fetched_at: Instant::now(),
                    previous_mean,
                    is_fresh_signal,
                },
            );
            evict_stale_cache_entries(&mut cache, self.config.refresh_interval_secs * 2);
        }

        Some((forecast, is_fresh_signal))
    }

    /// Detect the best undervalued outcome in a NegRisk weather event.
    ///
    /// Checks both YES and NO sides for each outcome market.
    async fn detect_neg_risk_weather(
        &self,
        event: &NegRiskEvent,
        metric: WeatherMetric,
        location: &str,
    ) -> Option<TradingOpportunity> {
        // Parse target date and precipitation unit from event title
        let target_date = parse_target_date(&event.title);
        let precipitation_unit = if matches!(metric, WeatherMetric::Rainfall | WeatherMetric::Snowfall) {
            detect_precipitation_unit(&event.title)
        } else {
            "inch"
        };

        let (forecast, is_fresh) = self
            .get_forecast_by_location(location, metric, target_date, precipitation_unit)
            .await?;

        // Skip if forecast hasn't changed significantly (when change detection is enabled)
        if self.config.forecast_change_detection && !is_fresh {
            return None;
        }

        let days_to_event = target_date.map(|d| {
            (d - Local::now().date_naive()).num_days()
        });
        let forecast_error_sigma = sigma_for_metric(
            &self.config.forecast_error,
            metric,
            days_to_event,
            self.config.dynamic_sigma,
        );

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

            // Convert °C thresholds to °F since forecast is always in Fahrenheit
            let range = if is_temperature_metric(metric) && is_celsius_market(&market.question) {
                OutcomeRange {
                    lower: range.lower.map(celsius_to_fahrenheit),
                    upper: range.upper.map(celsius_to_fahrenheit),
                }
            } else {
                range
            };

            let model_prob_f64 =
                model_range_probability(&forecast, &range, forecast_error_sigma, metric);
            let model_prob = match Decimal::from_f64_retain(model_prob_f64) {
                Some(d) => d,
                None => continue,
            };

            tracing::debug!(
                outcome = %market.question,
                range_lower = ?range.lower,
                range_upper = ?range.upper,
                forecast_mean = forecast.mean,
                forecast_target = ?forecast.target_value,
                sigma = forecast_error_sigma,
                model_prob = model_prob_f64,
                "NegRisk weather outcome probability"
            );

            let yes_token = &market.tokens[0];
            let no_token = &market.tokens[1];

            // Get order books for spread check
            let yes_book = (self.get_orderbook)(yes_token.token_id);
            let no_book = (self.get_orderbook)(no_token.token_id);

            // Check bid-ask spread before evaluating edge
            let mut skip_market = false;
            if let Some(ref yb) = yes_book {
                if let (Some(ask), Some(bid)) = (yb.best_ask(), yb.best_bid()) {
                    let spread = if ask.price > Decimal::ZERO {
                        (ask.price - bid.price) / ask.price
                    } else {
                        Decimal::ONE
                    };
                    let spread_bps = {
                        use rust_decimal::prelude::ToPrimitive;
                        (spread * dec!(10000)).to_u32().unwrap_or(u32::MAX)
                    };
                    if spread_bps > self.config.max_spread_bps {
                        skip_market = true;
                    }
                }
            }
            if let Some(ref nb) = no_book {
                if let (Some(ask), Some(bid)) = (nb.best_ask(), nb.best_bid()) {
                    let spread = if ask.price > Decimal::ZERO {
                        (ask.price - bid.price) / ask.price
                    } else {
                        Decimal::ONE
                    };
                    let spread_bps = {
                        use rust_decimal::prelude::ToPrimitive;
                        (spread * dec!(10000)).to_u32().unwrap_or(u32::MAX)
                    };
                    if spread_bps > self.config.max_spread_bps {
                        skip_market = true;
                    }
                }
            }

            if skip_market {
                tracing::debug!(
                    outcome = %market.question,
                    max_allowed = self.config.max_spread_bps,
                    "[Weather NegRisk] Skipping: spread too wide"
                );
                continue;
            }

            // YES side check: only if ask price is below max_entry_price
            if let Some(yes_book) = yes_book
                && let Some(yes_ask_level) = yes_book.best_ask()
            {
                let yes_ask = yes_ask_level.price;
                if model_prob > yes_ask && yes_ask <= self.config.max_entry_price {
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
            if let Some(no_book) = no_book
                && let Some(no_ask_level) = no_book.best_ask()
            {
                let no_ask = no_ask_level.price;
                if no_model_prob > no_ask && no_ask <= self.config.max_entry_price {
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

        // Fixed position sizing: max_position_usdc per trade, position-aware
        let max_usdc = self.config.max_position_usdc;
        let available = (self.get_available_capital)();
        let existing_cost = (self.get_position)(token_id) * ask_price;
        let remaining = (max_usdc - existing_cost).max(Decimal::ZERO);
        let size = max_usdc.min(remaining).min(available);

        // After capping, verify we still meet CLOB minimum ($1.00 cost).
        // E.g. at price $0.019, min_cost_size=53 but available may only allow 2 shares.
        if size <= Decimal::ZERO || (ask_price > Decimal::ZERO && size * ask_price < Decimal::ONE) {
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

        tracing::debug!(
            event_title = %event.title,
            outcome = %market.question,
            metric = ?metric,
            location = %location,
            forecast_mean = forecast.mean,
            model_prob = %effective_prob,
            ask_price = %ask_price,
            edge_bps = edge_bps,
            existing_position = %existing_cost,
            size = %size,
            est_profit = %est.net_profit,
            "NegRisk weather alpha opportunity detected"
        );

        Some(TradingOpportunity {
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

    /// Scan held positions for exit conditions (model reversal or capital efficiency).
    async fn scan_exits(&self, markets: &[MarketInfo]) -> Vec<TradingOpportunity> {
        let held = (self.get_held_positions)();
        if held.is_empty() {
            return vec![];
        }

        tracing::debug!(
            held_positions = held.len(),
            "[Weather] scanning exits"
        );

        // Build reverse map: token_id → market
        let token_to_market: std::collections::HashMap<U256, &MarketInfo> = markets
            .iter()
            .flat_map(|m| m.tokens.iter().map(move |t| (t.token_id, m)))
            .collect();

        let exit_buffer = Decimal::from(self.config.exit_buffer_bps) / dec!(10000);
        let mut exits = Vec::new();

        for (token_id, size, avg_cost) in &held {
            let book = match (self.get_orderbook)(*token_id) {
                Some(b) => b,
                None => {
                    tracing::debug!(token_id = %token_id, "[Weather EXIT] no orderbook — token not subscribed?");
                    continue;
                }
            };
            let best_bid = match book.best_bid() {
                Some(b) => b.price,
                None => {
                    tracing::debug!(token_id = %token_id, "[Weather EXIT] no bids in orderbook");
                    continue;
                }
            };

            // Profit-take exit: sell when price rises above profit_take_threshold
            if best_bid >= self.config.profit_take_threshold {
                tracing::debug!(
                    token_id = %token_id,
                    best_bid = %best_bid,
                    threshold = %self.config.profit_take_threshold,
                    "[EXIT] Profit take — weather"
                );
                exits.push(self.build_exit_opportunity(*token_id, *size, *avg_cost, best_bid, &token_to_market));
                continue;
            }

            // Capital efficiency exit: bid >= threshold
            if best_bid >= self.config.capital_efficiency_threshold {
                tracing::debug!(
                    token_id = %token_id,
                    best_bid = %best_bid,
                    "[EXIT] Capital efficiency — weather"
                );
                exits.push(self.build_exit_opportunity(*token_id, *size, *avg_cost, best_bid, &token_to_market));
                continue;
            }

            // Deep loss exit: cut losses regardless of model when position has lost >= 50%.
            // The model reversal check below only triggers when model_prob < best_bid,
            // which is nearly impossible at low prices (e.g., model_prob would need to be
            // below 4.5% when best_bid is 0.05). This check provides a model-independent
            // exit when the loss is severe.
            if *avg_cost > Decimal::ZERO && best_bid < *avg_cost * dec!(0.50) {
                let loss_pct = ((*avg_cost - best_bid) / *avg_cost * dec!(100)).round_dp(1);
                tracing::debug!(
                    token_id = %token_id,
                    best_bid = %best_bid,
                    avg_cost = %avg_cost,
                    loss_pct = %loss_pct,
                    "[EXIT] Deep loss detected — weather position lost >= 50%"
                );
                exits.push(self.build_exit_opportunity(*token_id, *size, *avg_cost, best_bid, &token_to_market));
                continue;
            }

            // Model reversal: recompute model_prob using cached forecast
            let market = match token_to_market.get(token_id) {
                Some(m) => *m,
                None => {
                    tracing::debug!(
                        token_id = %token_id,
                        best_bid = %best_bid,
                        "[Weather EXIT] token not in scanned markets — market may be filtered by max_market_end_days"
                    );
                    continue;
                }
            };

            // For NegRisk outcomes (e.g. "36-37°F"), parse_weather_question fails because
            // there's no city name. Use parse_weather_event_title on the event title instead.
            // For binary markets, parse_weather_question gives threshold + comparison too.
            let (location, metric, target_date, precipitation_unit, binary_parsed) = if market.neg_risk {
                let event_title = match &market.event_title {
                    Some(t) => t.as_str(),
                    None => {
                        tracing::debug!(token_id = %token_id, "[Weather EXIT] NegRisk market has no event_title");
                        continue;
                    }
                };
                let (metric, location) = match parse_weather_event_title(event_title) {
                    Some(r) => r,
                    None => {
                        tracing::debug!(token_id = %token_id, event_title, "[Weather EXIT] parse_weather_event_title failed");
                        continue;
                    }
                };
                let target_date = parse_target_date(event_title);
                let precipitation_unit = if matches!(metric, WeatherMetric::Rainfall | WeatherMetric::Snowfall) {
                    detect_precipitation_unit(event_title)
                } else {
                    "inch"
                };
                (location, metric, target_date, precipitation_unit, None)
            } else {
                let parsed = match parse_weather_question(&market.question) {
                    Some(p) => p,
                    None => {
                        tracing::debug!(token_id = %token_id, question = %market.question, "[Weather EXIT] parse_weather_question failed — not a weather market?");
                        continue;
                    }
                };
                let target_date = parse_target_date(&market.question);
                let precipitation_unit = if matches!(parsed.metric, WeatherMetric::Rainfall | WeatherMetric::Snowfall) {
                    detect_precipitation_unit(&market.question)
                } else {
                    "inch"
                };
                let loc = parsed.location.clone();
                let met = parsed.metric;
                (loc, met, target_date, precipitation_unit, Some(parsed))
            };

            // For NegRisk positions, use get_forecast_by_location to share cache with entry path.
            // For binary positions, use get_forecast (keyed by question text).
            let (forecast, _is_fresh) = if market.neg_risk {
                match self
                    .get_forecast_by_location(&location, metric, target_date, precipitation_unit)
                    .await
                {
                    Some(f) => f,
                    None => {
                        tracing::debug!(token_id = %token_id, %location, "[Weather EXIT] forecast fetch failed (NegRisk)");
                        continue;
                    }
                }
            } else {
                let parsed = match &binary_parsed {
                    Some(p) => p,
                    None => continue,
                };
                match self
                    .get_forecast(&market.question, parsed, target_date, precipitation_unit)
                    .await
                {
                    Some(f) => f,
                    None => {
                        tracing::debug!(token_id = %token_id, question = %market.question, "[Weather EXIT] forecast fetch failed (binary)");
                        continue;
                    }
                }
            };

            let days_to_event = target_date.map(|d| {
                (d - Local::now().date_naive()).num_days()
            });
            let forecast_error_sigma = sigma_for_metric(
                &self.config.forecast_error,
                metric,
                days_to_event,
                self.config.dynamic_sigma,
            );
            // Convert °C threshold to °F since forecast is always in Fahrenheit
            // For NegRisk outcomes (ranges), use model_range_probability instead of model_probability
            let model_prob = if market.neg_risk {
                if let Some(range) = parse_outcome_range(&market.question) {
                    let range = if is_temperature_metric(metric) && is_celsius_market(&market.question) {
                        OutcomeRange {
                            lower: range.lower.map(celsius_to_fahrenheit),
                            upper: range.upper.map(celsius_to_fahrenheit),
                        }
                    } else {
                        range
                    };
                    model_range_probability(&forecast, &range, forecast_error_sigma, metric)
                } else {
                    // NegRisk outcome couldn't be parsed as range — skip
                    continue;
                }
            } else {
                let parsed = match &binary_parsed {
                    Some(p) => p,
                    None => continue,
                };
                let threshold = if is_temperature_metric(metric) && is_celsius_market(&market.question) {
                    celsius_to_fahrenheit(parsed.threshold)
                } else {
                    parsed.threshold
                };
                model_probability(
                    &forecast,
                    threshold,
                    parsed.comparison,
                    forecast_error_sigma,
                    metric,
                )
            };
            let model_prob_dec = match Decimal::from_f64_retain(model_prob) {
                Some(d) => d,
                None => continue,
            };

            // Determine which side we hold: check if this token is YES or NO
            let is_yes = market.tokens.first().map(|t| t.token_id == *token_id).unwrap_or(false);
            let effective_prob = if is_yes { model_prob_dec } else { Decimal::ONE - model_prob_dec };

            if effective_prob < best_bid - exit_buffer {
                tracing::debug!(
                    token_id = %token_id,
                    effective_prob = %effective_prob,
                    best_bid = %best_bid,
                    "[EXIT] Model reversal — weather"
                );
                exits.push(self.build_exit_opportunity(*token_id, *size, *avg_cost, best_bid, &token_to_market));
            } else {
                tracing::debug!(
                    token_id = %token_id,
                    effective_prob = %effective_prob,
                    best_bid = %best_bid,
                    exit_buffer = %exit_buffer,
                    threshold = %(best_bid - exit_buffer),
                    "[Weather EXIT] No reversal: effective_prob >= best_bid - buffer"
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
        token_to_market: &std::collections::HashMap<U256, &MarketInfo>,
    ) -> TradingOpportunity {
        let market = token_to_market.get(&token_id);
        let condition_id = market.map(|m| m.condition_id).unwrap_or_default();
        let question = market.map(|m| m.question.clone()).unwrap_or_default();
        let fee_rate_bps = market.map(|m| m.fee_rate_bps).unwrap_or(200);

        let est = self.profit_calc.directional_sell_profit(best_bid, avg_cost, size, fee_rate_bps);

        TradingOpportunity {
            id: Uuid::now_v7(),
            strategy_type: StrategyType::Weather,
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
    ) -> pa_core::Result<Vec<TradingOpportunity>> {
        let mut opportunities = Vec::new();
        let count = self.scan_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let log_diag = count % 600 == 0;

        let mut binary_weather = 0u32;
        let mut neg_risk_weather = 0u32;

        // 1. Collect binary weather markets, filtered by target cities
        let mut binary_candidates: Vec<(&MarketInfo, WeatherQuestion)> = Vec::new();
        for market in markets {
            if !market.active || market.neg_risk {
                continue;
            }

            let parsed = match parse_weather_question(&market.question) {
                Some(p) => p,
                None => continue, // Not a weather market
            };

            // Filter by target cities
            if !self.is_target_city(&parsed.location) {
                continue;
            }

            binary_weather += 1;
            binary_candidates.push((market, parsed));
        }

        // Scan binary weather markets
        for (market, parsed) in binary_candidates {
            if let Some(opp) = self.detect_weather_opportunity(market, &parsed).await {
                opportunities.push(opp);
            }
        }

        // 2. Scan NegRisk weather events (filtered by target cities)
        for event in &self.neg_risk_events {
            if let Some((metric, location)) = parse_weather_event_title(&event.title) {
                if !self.is_target_city(&location) {
                    continue;
                }
                neg_risk_weather += 1;
                if let Some(opp) = self.detect_neg_risk_weather(event, metric, &location).await {
                    opportunities.push(opp);
                }
            }
        }

        if log_diag {
            tracing::debug!(
                total_events = self.neg_risk_events.len(),
                binary_weather,
                neg_risk_weather,
                opportunities = opportunities.len(),
                "[Weather] scan diagnostics"
            );
        }

        // 3. Stale liquidity detection: scan past-date markets with confirmed outcomes
        let stale_opps = self.scan_stale_liquidity(markets).await;
        if !stale_opps.is_empty() {
            tracing::info!(
                count = stale_opps.len(),
                "[Weather] Stale liquidity opportunities detected"
            );
        }
        opportunities.extend(stale_opps);

        // 4. NegRisk surround strategy: buy peak + adjacent bins in early game
        let mut surround_count = 0u32;
        for event in &self.neg_risk_events {
            if let Some((metric, location)) = parse_weather_event_title(&event.title) {
                if !self.is_target_city(&location) {
                    continue;
                }
                let opps = self.detect_neg_risk_surround(event, metric, &location).await;
                if !opps.is_empty() {
                    surround_count += opps.len() as u32;
                    opportunities.extend(opps);
                }
            }
        }
        if log_diag && surround_count > 0 {
            tracing::debug!(surround_count, "[Weather] NegRisk surround opportunities");
        }

        // Exit scanning: check held positions for model reversal / capital efficiency
        let exit_opps = self.scan_exits(markets).await;
        opportunities.extend(exit_opps);

        // Note: Dynamic trimming is handled through the scan_exits function
        // which detects model reversals for NegRisk positions.


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
            model_spread: 0.0,
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
            model_spread: 0.0,
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
            model_spread: 0.0,
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
            max_spread_bps: 1200, // 12%
            max_position_pct: dec!(0.50),
            kelly_fraction: dec!(0.25),
            forecast_error: ForecastErrorConfig::default(),
            refresh_interval_secs: 3600,
            exit_buffer_bps: 50,
            capital_efficiency_threshold: dec!(0.98),
            dynamic_sigma: false,
            forecast_change_detection: false,
            forecast_change_threshold: 0.5,
            max_entry_price: dec!(0.15),
            profit_take_threshold: dec!(0.45),
            max_position_usdc: dec!(2),
            noaa_user_agent: "test".to_string(),
            target_cities: vec![],
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
            model_spread: 0.0,
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
            model_spread: 0.0,
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
            model_spread: 0.0,
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
    fn test_parse_target_date_bare_month() {
        // "in February" without a day → last day of February
        let date = parse_target_date("What price will Solana hit in February?").unwrap();
        assert_eq!(date.month(), 2);
        // Last day: Feb 28 or 29 depending on leap year
        assert!(date.day() == 28 || date.day() == 29);
    }

    #[test]
    fn test_parse_target_date_by_end_of_month() {
        // "by end of March" → last day of March
        let date = parse_target_date("Will BTC reach $200k by end of March?").unwrap();
        assert_eq!(date.month(), 3);
        assert_eq!(date.day(), 31);
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

    #[test]
    fn test_parse_target_date_may_modal_verb() {
        // "may" as a modal verb should NOT be parsed as the month May
        let date = parse_target_date("Will temperatures may exceed 100°F this summer?");
        assert!(date.is_none(), "Modal verb 'may' should not match as month May");
    }

    #[test]
    fn test_parse_target_date_in_may() {
        // "in May" with preposition should match
        let date = parse_target_date("What will the temperature be in May?").unwrap();
        assert_eq!(date.month(), 5);
        assert_eq!(date.day(), 31);
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

    // ──── CDF Zero-Mean Edge Case Tests ────

    #[test]
    fn test_lognormal_cdf_zero_mean_returns_one() {
        // No precipitation expected: P(X ≤ 13) should be ~1.0, not 0.0
        assert_eq!(lognormal_cdf(13.0, 0.0, 0.3), 1.0);
        assert_eq!(lognormal_cdf(0.1, 0.0, 1.0), 1.0);
        assert_eq!(lognormal_cdf(100.0, -1.0, 0.5), 1.0);
        // t <= 0 still returns 0.0
        assert_eq!(lognormal_cdf(0.0, 0.0, 0.3), 0.0);
        assert_eq!(lognormal_cdf(-1.0, 0.0, 0.3), 0.0);
    }

    #[test]
    fn test_lognormal_cdf_zero_sigma_point_mass() {
        // sigma=0 with positive mean: point mass at mean
        assert_eq!(lognormal_cdf(5.0, 3.0, 0.0), 1.0);  // t > mean
        assert_eq!(lognormal_cdf(3.0, 3.0, 0.0), 1.0);  // t == mean
        assert_eq!(lognormal_cdf(1.0, 3.0, 0.0), 0.0);  // t < mean
    }

    #[test]
    fn test_weibull_cdf_zero_mean_returns_one() {
        // No wind expected: P(X ≤ t) should be ~1.0 for any t > 0
        assert_eq!(weibull_cdf(5.0, 0.0, 0.0), 1.0);
        assert_eq!(weibull_cdf(0.1, 0.0, 0.0), 1.0);
        assert_eq!(weibull_cdf(50.0, -1.0, 0.0), 1.0);
        // t <= 0 still returns 0.0
        assert_eq!(weibull_cdf(0.0, 0.0, 0.0), 0.0);
    }

    #[test]
    fn test_snow_zero_forecast_range_probability() {
        // "13 or more inches of snow" with forecast_mean=0.0 → should be ~0, not 1.0
        let forecast = ForecastData {
            values: vec![0.0],
            dates: vec!["2026-02-21".to_string()],
            mean: 0.0,
            std_dev: 0.0,
            target_value: Some(0.0),
            model_spread: 0.0,
        };
        let range = OutcomeRange {
            lower: Some(13.0),
            upper: None,
        };
        let prob = model_range_probability(&forecast, &range, 2.0, WeatherMetric::Snowfall);
        // P(snow ≥ 13 | forecast=0) should be very small (near 0)
        assert!(
            prob < 0.01,
            "P(snow >= 13 | mean=0) should be ~0, got {}",
            prob
        );
    }

    // ──── Celsius/Fahrenheit Conversion Tests ────

    #[test]
    fn test_is_celsius_market() {
        assert!(is_celsius_market("Will the highest temperature in Wellington be 23°C or higher?"));
        assert!(is_celsius_market("between 10-12°C"));
        assert!(is_celsius_market("temperature will exceed 30 Celsius"));
        assert!(!is_celsius_market("Will the highest temperature be 84°F?"));
        assert!(!is_celsius_market("between 84-85°F"));
        assert!(!is_celsius_market("Will it snow 13 inches?"));
    }

    #[test]
    fn test_celsius_to_fahrenheit_conversion() {
        assert!((celsius_to_fahrenheit(0.0) - 32.0).abs() < 0.01);
        assert!((celsius_to_fahrenheit(100.0) - 212.0).abs() < 0.01);
        assert!((celsius_to_fahrenheit(23.0) - 73.4).abs() < 0.01);
        assert!((celsius_to_fahrenheit(-40.0) - (-40.0)).abs() < 0.01); // C and F meet at -40
    }

    #[test]
    fn test_cdf_for_metric_degenerate_temperature() {
        // CDF(value) = P(X <= value). For point mass at mean (sigma → 0):
        // P(X <= 80) = 1.0 when mean=70 (mass is below threshold)
        // P(X <= 60) = 0.0 when mean=70 (mass is above threshold)
        assert_eq!(cdf_for_metric(WeatherMetric::TemperatureMax, 80.0, 70.0, 0.0), 1.0);
        assert_eq!(cdf_for_metric(WeatherMetric::TemperatureMax, 60.0, 70.0, 0.0), 0.0);
        assert_eq!(cdf_for_metric(WeatherMetric::TemperatureMax, 70.0, 70.0, 0.0), 1.0); // at mean
    }

    #[test]
    fn test_celsius_market_probability_correction() {
        // Wellington: "23°C or higher" with forecast 71.5°F (=21.9°C)
        // Without fix: P(X >= 23 | mean=71.5, sigma=3) ≈ 1.0 (wrong!)
        // With fix: P(X >= 73.4 | mean=71.5, sigma=3) ≈ 0.26 (correct)
        let forecast = ForecastData {
            values: vec![71.5],
            dates: vec!["2026-02-22".to_string()],
            mean: 71.5,
            std_dev: 0.0,
            target_value: Some(71.5),
            model_spread: 0.0,
        };
        let sigma = 3.0;

        // Incorrect: using raw Celsius threshold
        let prob_wrong = model_probability(&forecast, 23.0, Comparison::AtLeast, sigma, WeatherMetric::TemperatureMax);
        assert!(prob_wrong > 0.99, "Without conversion prob should be ~1.0, got {}", prob_wrong);

        // Correct: convert 23°C to 73.4°F
        let threshold_f = celsius_to_fahrenheit(23.0);
        let prob_correct = model_probability(&forecast, threshold_f, Comparison::AtLeast, sigma, WeatherMetric::TemperatureMax);
        assert!(
            prob_correct < 0.40,
            "With conversion prob should be ~0.26, got {}",
            prob_correct
        );
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
        // Without dynamic sigma, returns base values
        assert_eq!(sigma_for_metric(&config, WeatherMetric::TemperatureMax, None, false), 3.0);
        assert_eq!(sigma_for_metric(&config, WeatherMetric::TemperatureMin, None, false), 3.0);
        assert_eq!(sigma_for_metric(&config, WeatherMetric::TemperatureAvg, None, false), 3.0);
        assert_eq!(sigma_for_metric(&config, WeatherMetric::Rainfall, None, false), 0.3);
        assert_eq!(sigma_for_metric(&config, WeatherMetric::Snowfall, None, false), 2.0);
        assert_eq!(sigma_for_metric(&config, WeatherMetric::WindSpeed, None, false), 5.0);
    }

    // ──── Dynamic Sigma Tests ────

    #[test]
    fn test_dynamic_sigma_1_day() {
        let config = ForecastErrorConfig::default(); // temp=3.0
        // sqrt(1) = 1.0 → 3.0 * 1.0 = 3.0
        let sigma = sigma_for_metric(&config, WeatherMetric::TemperatureMax, Some(1), true);
        assert!((sigma - 3.0).abs() < 1e-6, "1-day sigma = {}, expected 3.0", sigma);
    }

    #[test]
    fn test_dynamic_sigma_4_days() {
        let config = ForecastErrorConfig::default(); // temp=3.0
        // sqrt(4) = 2.0 → 3.0 * 2.0 = 6.0
        let sigma = sigma_for_metric(&config, WeatherMetric::TemperatureMax, Some(4), true);
        assert!((sigma - 6.0).abs() < 1e-6, "4-day sigma = {}, expected 6.0", sigma);
    }

    #[test]
    fn test_dynamic_sigma_9_days() {
        let config = ForecastErrorConfig::default(); // temp=3.0
        // sqrt(9) = 3.0 → 3.0 * 3.0 = 9.0
        let sigma = sigma_for_metric(&config, WeatherMetric::TemperatureMax, Some(9), true);
        assert!((sigma - 9.0).abs() < 1e-6, "9-day sigma = {}, expected 9.0", sigma);
    }

    #[test]
    fn test_dynamic_sigma_disabled() {
        let config = ForecastErrorConfig::default(); // temp=3.0
        // dynamic_sigma=false → always returns base regardless of days
        let sigma = sigma_for_metric(&config, WeatherMetric::TemperatureMax, Some(9), false);
        assert!((sigma - 3.0).abs() < 1e-6, "disabled sigma = {}, expected 3.0", sigma);
    }

    #[test]
    fn test_dynamic_sigma_none_and_zero_clamp() {
        let config = ForecastErrorConfig::default(); // temp=3.0
        // None → defaults to 1 day → sqrt(1) = 1.0 → 3.0
        let sigma_none = sigma_for_metric(&config, WeatherMetric::TemperatureMax, None, true);
        assert!((sigma_none - 3.0).abs() < 1e-6, "None sigma = {}, expected 3.0", sigma_none);

        // 0 days → clamped to 1 → sqrt(1) = 1.0 → 3.0
        let sigma_zero = sigma_for_metric(&config, WeatherMetric::TemperatureMax, Some(0), true);
        assert!((sigma_zero - 3.0).abs() < 1e-6, "0-day sigma = {}, expected 3.0", sigma_zero);

        // Negative days → clamped to 1 → sqrt(1) = 1.0 → 3.0
        let sigma_neg = sigma_for_metric(&config, WeatherMetric::TemperatureMax, Some(-2), true);
        assert!((sigma_neg - 3.0).abs() < 1e-6, "negative sigma = {}, expected 3.0", sigma_neg);
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
            model_spread: 0.0,
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
            model_spread: 0.0,
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

    // ──── Model Spread / Ensemble Tests ────

    #[test]
    fn test_model_spread_widens_sigma_date_specific() {
        // Date-specific: sigma = sqrt(forecast_error² + model_spread²)
        let forecast = ForecastData {
            values: vec![95.0],
            dates: vec!["2025-07-01".into()],
            mean: 95.0,
            std_dev: 0.0,
            target_value: Some(95.0),
            model_spread: 4.0,
        };
        let sigma = effective_sigma(&forecast, 3.0);
        // sqrt(9 + 16) = sqrt(25) = 5.0
        assert!((sigma - 5.0).abs() < 1e-6, "sigma = {}, expected 5.0", sigma);
    }

    #[test]
    fn test_model_spread_zero_unchanged() {
        // model_spread=0 should give same result as before
        let forecast = ForecastData {
            values: vec![95.0],
            dates: vec!["2025-07-01".into()],
            mean: 95.0,
            std_dev: 0.0,
            target_value: Some(95.0),
            model_spread: 0.0,
        };
        let sigma = effective_sigma(&forecast, 3.0);
        assert!((sigma - 3.0).abs() < 1e-6, "zero spread sigma = {}, expected 3.0", sigma);
    }

    #[test]
    fn test_model_spread_multiday() {
        // Multi-day: sigma = sqrt(std_dev² + forecast_error² + model_spread²)
        let forecast = ForecastData {
            values: vec![90.0, 95.0, 100.0],
            dates: vec!["d1".into(), "d2".into(), "d3".into()],
            mean: 95.0,
            std_dev: 5.0,
            target_value: None,
            model_spread: 4.0,
        };
        let sigma = effective_sigma(&forecast, 3.0);
        // sqrt(25 + 9 + 16) = sqrt(50) ≈ 7.071
        let expected = 50.0_f64.sqrt();
        assert!((sigma - expected).abs() < 1e-6, "multiday sigma = {}, expected {}", sigma, expected);
    }

    #[test]
    fn test_model_spread_pushes_probability_toward_half() {
        // Higher model_spread → wider sigma → probability closer to 0.5
        let forecast_no_spread = ForecastData {
            values: vec![95.0],
            dates: vec!["d1".into()],
            mean: 95.0,
            std_dev: 0.0,
            target_value: Some(95.0),
            model_spread: 0.0,
        };
        let forecast_with_spread = ForecastData {
            values: vec![95.0],
            dates: vec!["d1".into()],
            mean: 95.0,
            std_dev: 0.0,
            target_value: Some(95.0),
            model_spread: 5.0,
        };
        let prob_no = model_probability(&forecast_no_spread, 100.0, Comparison::Above, 3.0, WeatherMetric::TemperatureMax);
        let prob_with = model_probability(&forecast_with_spread, 100.0, Comparison::Above, 3.0, WeatherMetric::TemperatureMax);
        // With spread, sigma is larger, so P(X>100) is closer to 0.5 (higher in this case since mean < threshold)
        assert!(
            prob_with > prob_no,
            "model_spread should push tail prob toward 0.5: {} > {}",
            prob_with, prob_no
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

    // ──── Minimum Cost Floor Tests ────

    #[test]
    fn test_min_cost_floor_bumps_size() {
        // Kelly gives size=0.5, but at price=0.10 cost=$0.05 < $1.00
        // Floor: ceil(1.0/0.10) = 10 shares, cost = $1.00
        let ask_price = dec!(0.10);
        let kelly_size = dec!(0.5);
        let remaining = dec!(20);
        let available = dec!(1000);
        let size = kelly_size.min(remaining).min(available);

        let min_cost_size = (Decimal::ONE / ask_price).ceil();
        assert_eq!(min_cost_size, dec!(10));

        let size = if size < min_cost_size {
            let bumped = min_cost_size.min(remaining).min(available);
            if bumped < min_cost_size { Decimal::ZERO } else { bumped }
        } else {
            size
        };
        assert_eq!(size, dec!(10));
        assert!(ask_price * size >= Decimal::ONE); // cost $1.00
    }

    #[test]
    fn test_min_cost_floor_skip_when_exceeds_max() {
        // Kelly gives size=0.5 at price=0.05: need 20 shares for $1.00
        // But remaining=15 < 20 → cannot meet minimum, skip
        let ask_price = dec!(0.05);
        let remaining = dec!(15);
        let min_cost_size = (Decimal::ONE / ask_price).ceil();
        assert_eq!(min_cost_size, dec!(20));

        let bumped = min_cost_size.min(remaining);
        assert_eq!(bumped, dec!(15));
        assert!(bumped < min_cost_size); // cannot meet → should skip
    }

    #[test]
    fn test_min_cost_floor_no_bump_needed() {
        // Kelly gives size=5 at price=0.50: cost=$2.50 > $1.00, no bump needed
        let ask_price = dec!(0.50);
        let kelly_size = dec!(5);
        let remaining = dec!(20);
        let size = kelly_size.min(remaining);

        let min_cost_size = (Decimal::ONE / ask_price).ceil();
        assert_eq!(min_cost_size, dec!(2));

        assert!(size >= min_cost_size); // no bump needed
        assert_eq!(size, dec!(5));
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
                    model_spread: 0.0,
                },
                fetched_at: Instant::now() - Duration::from_secs(7200), // 2 hours old
                previous_mean: None,
                is_fresh_signal: true,
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
                    model_spread: 0.0,
                },
                fetched_at: Instant::now(),
                previous_mean: None,
                is_fresh_signal: true,
            },
        );

        assert_eq!(cache.len(), 2);

        // Evict entries older than 1 hour
        evict_stale_cache_entries(&mut cache, 3600);

        assert_eq!(cache.len(), 1);
        assert!(cache.contains_key(&2));
        assert!(!cache.contains_key(&1));
    }

    // ──── Forecast Change Detection Tests ────

    #[test]
    fn test_change_detection_first_observation() {
        // No previous value → always significant
        assert!(is_significant_change(95.0, None, 3.0, 0.5));
    }

    #[test]
    fn test_change_detection_above_threshold() {
        // |97.0 - 95.0| = 2.0 > 0.5 * 3.0 = 1.5 → significant
        assert!(is_significant_change(97.0, Some(95.0), 3.0, 0.5));
    }

    #[test]
    fn test_change_detection_below_threshold() {
        // |95.5 - 95.0| = 0.5 < 0.5 * 3.0 = 1.5 → not significant
        assert!(!is_significant_change(95.5, Some(95.0), 3.0, 0.5));
    }

    #[test]
    fn test_change_detection_exact_threshold() {
        // |96.5 - 95.0| = 1.5 = 0.5 * 3.0 → NOT significant (strictly greater required)
        assert!(!is_significant_change(96.5, Some(95.0), 3.0, 0.5));
    }

    #[test]
    fn test_change_detection_reverse_direction() {
        // |93.0 - 95.0| = 2.0 > 0.5 * 3.0 = 1.5 → significant (direction doesn't matter)
        assert!(is_significant_change(93.0, Some(95.0), 3.0, 0.5));
    }

    #[test]
    fn test_change_detection_zero_sigma() {
        // sigma=0 → any nonzero change is significant
        assert!(is_significant_change(95.001, Some(95.0), 0.0, 0.5));
        // No change → not significant
        assert!(!is_significant_change(95.0, Some(95.0), 0.0, 0.5));
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

    // ──── Exit Tests ────

    use pa_core::types::{Outcome, PriceLevel, TokenInfo};
    use alloy::primitives::{B256, U256};

    fn make_weather_market(question: &str) -> MarketInfo {
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

    fn make_weather_book(token_id: U256, best_bid: Decimal) -> OrderBook {
        OrderBook {
            token_id,
            bids: vec![PriceLevel { price: best_bid, size: dec!(500) }],
            asks: vec![PriceLevel { price: best_bid + dec!(0.02), size: dec!(500) }],
            timestamp: Utc::now(),
        }
    }

    fn make_weather_strategy(
        books: HashMap<U256, OrderBook>,
        held: Vec<(U256, Decimal, Decimal)>,
    ) -> WeatherAlphaStrategy {
        let config = WeatherConfig {
            min_edge_bps: 500, // 5%
            max_spread_bps: 1200, // 12%
            max_position_pct: dec!(0.50),
            kelly_fraction: dec!(0.25),
            forecast_error: ForecastErrorConfig::default(),
            refresh_interval_secs: 3600,
            exit_buffer_bps: 50,
            capital_efficiency_threshold: dec!(0.98),
            dynamic_sigma: false,
            forecast_change_detection: false,
            forecast_change_threshold: 0.5,
            max_entry_price: Decimal::ONE, // No price ceiling in test helper
            profit_take_threshold: dec!(0.45),
            max_position_usdc: dec!(100), // Large cap for test helper
            noaa_user_agent: "test".to_string(),
            target_cities: vec![],
        };
        let books = Arc::new(books);
        WeatherAlphaStrategy::new(
            config,
            Decimal::ZERO,
            Box::new(move |tid| books.get(&tid).cloned()),
            Box::new(|| Decimal::MAX),
            Box::new(|_| Decimal::ZERO),
            vec![],
            Box::new(move || held.clone()),
            Box::new(|| dec!(200)), // test balance $200
        )
    }

    #[tokio::test]
    async fn test_exit_capital_efficiency_weather() {
        // Held YES token with bid=0.99 → should emit capital efficiency exit
        let token_id = U256::from(1u64);
        let question = "Will the temperature in NYC exceed 100F this summer?";
        let market = make_weather_market(question);

        let mut books = HashMap::new();
        books.insert(token_id, make_weather_book(token_id, dec!(0.99)));
        books.insert(U256::from(2u64), make_weather_book(U256::from(2u64), dec!(0.01)));

        let held = vec![(token_id, dec!(50), dec!(0.30))];
        let strategy = make_weather_strategy(books, held);

        let exits = strategy.scan_exits(&[market]).await;
        assert_eq!(exits.len(), 1);
        assert!(exits[0].question.starts_with("[EXIT]"));
        match &exits[0].execution_plan {
            ExecutionPlan::DirectionalBuy { side, price, size, .. } => {
                assert_eq!(*side, TradeSide::Sell);
                assert_eq!(*price, dec!(0.99));
                assert_eq!(*size, dec!(50));
            }
            _ => panic!("Expected DirectionalBuy with Sell side"),
        }
    }

    #[tokio::test]
    async fn test_exit_no_positions_weather() {
        let question = "Will the temperature in NYC exceed 100F this summer?";
        let market = make_weather_market(question);
        let mut books = HashMap::new();
        books.insert(U256::from(1u64), make_weather_book(U256::from(1u64), dec!(0.60)));
        books.insert(U256::from(2u64), make_weather_book(U256::from(2u64), dec!(0.40)));

        let strategy = make_weather_strategy(books, vec![]);

        let exits = strategy.scan_exits(&[market]).await;
        assert!(exits.is_empty());
    }

    #[tokio::test]
    async fn test_exit_model_reversal_weather() {
        // Held YES token at bid=0.70, pre-populate cache with forecast that gives
        // model_prob ≈ 0.15 (much below bid) → should trigger model reversal
        let token_id = U256::from(1u64);
        let question = "Will the temperature in NYC exceed 100F this summer?";
        let market = make_weather_market(question);

        let mut books = HashMap::new();
        books.insert(token_id, make_weather_book(token_id, dec!(0.70)));
        books.insert(U256::from(2u64), make_weather_book(U256::from(2u64), dec!(0.30)));

        let held = vec![(token_id, dec!(50), dec!(0.30))];
        let strategy = make_weather_strategy(books, held);

        // Pre-populate forecast cache with data that makes model_prob low
        // mean=70°F, std=5°F, threshold=100°F → z = (100-70)/5 = 6.0 → P(X>100) ≈ 0.0
        // So model_prob_dec ≈ 0.0 < best_bid(0.70) - exit_buffer(0.005) = 0.695 → EXIT
        let cache_key = WeatherAlphaStrategy::question_hash(question);
        {
            let mut cache = strategy.forecast_cache.lock().unwrap();
            cache.insert(cache_key, CachedForecast {
                forecast: ForecastData {
                    values: vec![70.0],
                    dates: vec!["2025-07-15".into()],
                    mean: 70.0,
                    std_dev: 5.0,
                    target_value: Some(70.0),
                    model_spread: 0.0,
                },
                fetched_at: Instant::now(),
                previous_mean: None,
                is_fresh_signal: true,
            });
        }

        let exits = strategy.scan_exits(&[market]).await;
        assert_eq!(exits.len(), 1, "Should detect model reversal exit");
        assert!(exits[0].question.starts_with("[EXIT]"));
    }

    #[tokio::test]
    async fn test_exit_edge_still_positive_weather() {
        // Held YES token at bid=0.10, model says P≈0.84 → edge still positive → no exit
        let token_id = U256::from(1u64);
        let question = "Will the temperature in NYC exceed 100F this summer?";
        let market = make_weather_market(question);

        let mut books = HashMap::new();
        books.insert(token_id, make_weather_book(token_id, dec!(0.10)));
        books.insert(U256::from(2u64), make_weather_book(U256::from(2u64), dec!(0.90)));

        let held = vec![(token_id, dec!(50), dec!(0.08))];
        let strategy = make_weather_strategy(books, held);

        // Pre-populate with forecast: mean=99, std=3, threshold=100
        // z = (100-99)/3 = 0.33 → P(X>100) ≈ 1 - CDF(0.33) ≈ 0.37
        // effective_prob(YES)=0.37 > bid(0.10) - buffer(0.005) = 0.095 → NO exit
        let cache_key = WeatherAlphaStrategy::question_hash(question);
        {
            let mut cache = strategy.forecast_cache.lock().unwrap();
            cache.insert(cache_key, CachedForecast {
                forecast: ForecastData {
                    values: vec![99.0],
                    dates: vec!["2025-07-15".into()],
                    mean: 99.0,
                    std_dev: 3.0,
                    target_value: Some(99.0),
                    model_spread: 0.0,
                },
                fetched_at: Instant::now(),
                previous_mean: None,
                is_fresh_signal: true,
            });
        }

        let exits = strategy.scan_exits(&[market]).await;
        assert!(exits.is_empty(), "Edge still positive, should not exit");
    }

    #[tokio::test]
    async fn test_exit_model_reversal_ignores_change_detection() {
        // Key scenario: forecast_change_detection=true, is_fresh_signal=false,
        // but model has reversed → exit should STILL fire (exits ignore change detection)
        let token_id = U256::from(1u64);
        let question = "Will the temperature in NYC exceed 100F this summer?";
        let market = make_weather_market(question);

        let mut books = HashMap::new();
        books.insert(token_id, make_weather_book(token_id, dec!(0.70)));
        books.insert(U256::from(2u64), make_weather_book(U256::from(2u64), dec!(0.30)));

        // Build strategy with forecast_change_detection=true
        let config = WeatherConfig {
            min_edge_bps: 500,
            max_spread_bps: 1200,
            max_position_pct: dec!(0.50),
            kelly_fraction: dec!(0.25),
            forecast_error: ForecastErrorConfig::default(),
            refresh_interval_secs: 3600,
            exit_buffer_bps: 50,
            capital_efficiency_threshold: dec!(0.98),
            dynamic_sigma: false,
            forecast_change_detection: true,   // ENABLED
            forecast_change_threshold: 0.5,
            max_entry_price: dec!(0.15),
            profit_take_threshold: dec!(0.45),
            max_position_usdc: dec!(2),
            noaa_user_agent: "test".to_string(),
            target_cities: vec![],
        };
        let held = vec![(token_id, dec!(50), dec!(0.30))];
        let held_clone = held.clone();
        let books_arc = Arc::new(books);
        let strategy = WeatherAlphaStrategy::new(
            config,
            Decimal::ZERO,
            Box::new(move |tid| books_arc.get(&tid).cloned()),
            Box::new(|| Decimal::MAX),
            Box::new(|_| Decimal::ZERO),
            vec![],
            Box::new(move || held_clone.clone()),
            Box::new(|| dec!(200)), // test balance $200
        );

        // Pre-populate cache: model_prob ≈ 0.0, is_fresh_signal = false
        // (forecast hasn't changed, but model clearly disagrees with market)
        let cache_key = WeatherAlphaStrategy::question_hash(question);
        {
            let mut cache = strategy.forecast_cache.lock().unwrap();
            cache.insert(cache_key, CachedForecast {
                forecast: ForecastData {
                    values: vec![70.0],
                    dates: vec!["2025-07-15".into()],
                    mean: 70.0,
                    std_dev: 5.0,
                    target_value: Some(70.0),
                    model_spread: 0.0,
                },
                fetched_at: Instant::now(),
                previous_mean: Some(70.0),     // same as current → no change
                is_fresh_signal: false,         // NOT a fresh signal
            });
        }

        let exits = strategy.scan_exits(&[market]).await;
        assert_eq!(exits.len(), 1, "Model reversal exit must fire even when is_fresh_signal=false");
        assert!(exits[0].question.starts_with("[EXIT]"));
    }

    #[tokio::test]
    async fn test_exit_neg_risk_uses_range_probability() {
        // NegRisk outcome "between 84-85°F" should use model_range_probability,
        // not model_probability. With forecast 70°F, P(84 <= X <= 85) ≈ tiny,
        // so buying YES at 0.50 is a losing trade → exit should fire.
        let token_id = U256::from(1u64);
        let question = "Will the highest temperature in Miami be between 84-85°F on February 22?";
        let market = MarketInfo {
            condition_id: B256::ZERO,
            question_id: B256::ZERO,
            question: question.to_string(),
            neg_risk: true,  // NegRisk market!
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
            event_title: Some("Highest temperature in Miami on February 22".to_string()),
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
        };

        let mut books = HashMap::new();
        books.insert(token_id, make_weather_book(token_id, dec!(0.50)));
        books.insert(U256::from(2u64), make_weather_book(U256::from(2u64), dec!(0.50)));

        let held = vec![(token_id, dec!(10), dec!(0.50))]; // bought YES at 0.50
        let strategy = make_weather_strategy(books, held);

        // Pre-populate cache: forecast 70°F (far from 84-85 range)
        // Use location-based cache key (same as get_forecast_by_location)
        let cache_key = WeatherAlphaStrategy::location_hash("Miami", WeatherMetric::TemperatureMax, None);
        {
            let mut cache = strategy.forecast_cache.lock().unwrap();
            cache.insert(cache_key, CachedForecast {
                forecast: ForecastData {
                    values: vec![70.0],
                    dates: vec!["2026-02-22".into()],
                    mean: 70.0,
                    std_dev: 3.0,
                    target_value: Some(70.0),
                    model_spread: 0.0,
                },
                fetched_at: Instant::now(),
                previous_mean: None,
                is_fresh_signal: true,
            });
        }

        let exits = strategy.scan_exits(&[market]).await;
        // P(84 ≤ X ≤ 85 | mean=70, sigma=3) is tiny (~0.00003)
        // effective_prob ≈ 0.00003, best_bid = 0.50
        // 0.00003 < 0.50 - 0.005 → EXIT must fire
        assert_eq!(exits.len(), 1, "NegRisk range exit should fire when forecast is far from range");
        assert!(exits[0].question.starts_with("[EXIT]"));
    }

    #[tokio::test]
    async fn test_spread_filter_rejects_wide_spread() {
        // Test that markets with bid-ask spread > max_spread_bps are rejected
        let token_id = U256::from(1u64);

        // Create order book with 20% spread (YES: bid=0.40, ask=0.50)
        let mut yes_book = make_weather_book(token_id, dec!(0.50));
        yes_book.bids.clear();
        yes_book.bids.push(PriceLevel { price: dec!(0.40), size: dec!(100) });

        let mut no_book = make_weather_book(U256::from(2u64), dec!(0.60));
        no_book.bids.clear();
        no_book.bids.push(PriceLevel { price: dec!(0.50), size: dec!(100) });

        let mut books = HashMap::new();
        books.insert(token_id, yes_book);
        books.insert(U256::from(2u64), no_book);

        let held = vec![];
        let strategy = make_weather_strategy(books, held);

        // Pre-populate cache with strong forecast (110°F for "exceed 100°F")
        let question = "Will the temperature in NYC exceed 100°F on March 5?";
        let cache_key = WeatherAlphaStrategy::question_hash(question);
        {
            let mut cache = strategy.forecast_cache.lock().unwrap();
            cache.insert(cache_key, CachedForecast {
                forecast: ForecastData {
                    values: vec![110.0],
                    dates: vec!["2026-03-05".into()],
                    mean: 110.0,
                    std_dev: 3.0,
                    target_value: Some(110.0),
                    model_spread: 0.0,
                },
                fetched_at: Instant::now(),
                previous_mean: None,
                is_fresh_signal: true,
            });
        }

        let market = make_weather_market(question);
        let parsed = WeatherQuestion {
            metric: WeatherMetric::TemperatureMax,
            location: "NYC".to_string(),
            threshold: 100.0,
            comparison: Comparison::Above,
        };

        // Spread = (0.50 - 0.40) / 0.50 = 0.20 = 2000 bps > 1200 bps
        // Should be rejected despite strong edge
        let result = strategy.detect_weather_opportunity(&market, &parsed).await;
        assert!(result.is_none(), "Market with 20% spread should be rejected (max 12%)");
    }

    #[tokio::test]
    async fn test_spread_filter_accepts_narrow_spread() {
        // Test that markets with bid-ask spread <= max_spread_bps are accepted
        let token_id = U256::from(1u64);

        // Create order book with 8% spread (YES: bid=0.46, ask=0.50)
        let mut yes_book = make_weather_book(token_id, dec!(0.50));
        yes_book.bids.clear();
        yes_book.bids.push(PriceLevel { price: dec!(0.46), size: dec!(100) });

        let mut no_book = make_weather_book(U256::from(2u64), dec!(0.54));
        no_book.bids.clear();
        no_book.bids.push(PriceLevel { price: dec!(0.50), size: dec!(100) });

        let mut books = HashMap::new();
        books.insert(token_id, yes_book);
        books.insert(U256::from(2u64), no_book);

        let held = vec![];
        let strategy = make_weather_strategy(books, held);

        let question = "Will the temperature in NYC exceed 100°F on March 5?";
        let cache_key = WeatherAlphaStrategy::question_hash(question);
        {
            let mut cache = strategy.forecast_cache.lock().unwrap();
            cache.insert(cache_key, CachedForecast {
                forecast: ForecastData {
                    values: vec![110.0],
                    dates: vec!["2026-03-05".into()],
                    mean: 110.0,
                    std_dev: 3.0,
                    target_value: Some(110.0),
                    model_spread: 0.0,
                },
                fetched_at: Instant::now(),
                previous_mean: None,
                is_fresh_signal: true,
            });
        }

        let market = make_weather_market(question);
        let parsed = WeatherQuestion {
            metric: WeatherMetric::TemperatureMax,
            location: "NYC".to_string(),
            threshold: 100.0,
            comparison: Comparison::Above,
        };

        // Spread = (0.50 - 0.46) / 0.50 = 0.08 = 800 bps < 1200 bps
        // Should be accepted
        let result = strategy.detect_weather_opportunity(&market, &parsed).await;
        assert!(result.is_some(), "Market with 8% spread should be accepted (max 12%)");
    }


    #[tokio::test]
    async fn test_exit_celsius_neg_risk_converts_threshold() {
        // NegRisk outcome in °C: "23°C or higher" with forecast 71.5°F (=21.9°C)
        // Without conversion: P(X >= 23) ≈ 1.0 → no exit (wrong!)
        // With conversion: P(X >= 73.4) ≈ 0.26 → exit fires (correct!)
        let token_id = U256::from(1u64);
        let question = "Will the highest temperature in Wellington be 23°C or higher on February 22?";
        let market = MarketInfo {
            condition_id: B256::ZERO,
            question_id: B256::ZERO,
            question: question.to_string(),
            neg_risk: true,
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
            event_title: Some("Highest temperature in Wellington on February 22".to_string()),
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
        };

        let mut books = HashMap::new();
        books.insert(token_id, make_weather_book(token_id, dec!(0.52)));
        books.insert(U256::from(2u64), make_weather_book(U256::from(2u64), dec!(0.48)));

        let held = vec![(token_id, dec!(12.50), dec!(0.52))]; // bought YES at 0.52
        let strategy = make_weather_strategy(books, held);

        // Pre-populate cache: forecast 71.5°F (=21.9°C, below 23°C threshold)
        // Use location-based cache key
        let cache_key = WeatherAlphaStrategy::location_hash("Wellington", WeatherMetric::TemperatureMax, None);
        {
            let mut cache = strategy.forecast_cache.lock().unwrap();
            cache.insert(cache_key, CachedForecast {
                forecast: ForecastData {
                    values: vec![71.5],
                    dates: vec!["2026-02-22".into()],
                    mean: 71.5,
                    std_dev: 3.0,
                    target_value: Some(71.5),
                    model_spread: 0.0,
                },
                fetched_at: Instant::now(),
                previous_mean: None,
                is_fresh_signal: true,
            });
        }

        let exits = strategy.scan_exits(&[market]).await;
        // With °C conversion: 23°C = 73.4°F, P(X >= 73.4 | mean=71.5, sigma=3) ≈ 0.26
        // NegRisk uses parse_outcome_range: "23°C or higher" → lower=Some(23) → converted to 73.4
        // effective_prob ≈ 0.26, best_bid = 0.52
        // 0.26 < 0.52 - 0.005 = 0.515 → EXIT must fire
        assert_eq!(exits.len(), 1, "Celsius NegRisk exit should fire with correct conversion");
        assert!(exits[0].question.starts_with("[EXIT]"));
    }

    // ──── NOAA Client Tests ────

    #[test]
    fn test_geocode_known_city() {
        let (lat, lon) = NoaaClient::geocode("New York").unwrap();
        assert!((lat - 40.7128).abs() < 0.01);
        assert!((lon - (-74.0060)).abs() < 0.01);
    }

    #[test]
    fn test_geocode_known_city_case_insensitive() {
        let (lat, _) = NoaaClient::geocode("chicago").unwrap();
        assert!((lat - 41.8781).abs() < 0.01);
    }

    #[test]
    fn test_geocode_unknown_city() {
        assert!(NoaaClient::geocode("London").is_err());
        assert!(NoaaClient::geocode("Tokyo").is_err());
        assert!(NoaaClient::geocode("Unknown City").is_err());
    }

    #[test]
    fn test_parse_noaa_valid_date() {
        let date = parse_noaa_date("2026-03-10T06:00:00+00:00/PT6H").unwrap();
        assert_eq!(date, NaiveDate::from_ymd_opt(2026, 3, 10).unwrap());
    }

    #[test]
    fn test_parse_noaa_date_no_duration() {
        let date = parse_noaa_date("2026-01-15T12:00:00+00:00").unwrap();
        assert_eq!(date, NaiveDate::from_ymd_opt(2026, 1, 15).unwrap());
    }

    #[test]
    fn test_parse_noaa_invalid_date() {
        assert!(parse_noaa_date("not-a-date").is_none());
        assert!(parse_noaa_date("").is_none());
    }

    #[test]
    fn test_kmh_to_mph() {
        assert!((kmh_to_mph(100.0) - 62.1371).abs() < 0.01);
        assert!((kmh_to_mph(0.0) - 0.0).abs() < 0.001);
    }

    #[test]
    fn test_mm_to_inches() {
        assert!((mm_to_inches(25.4) - 1.0).abs() < 0.001);
        assert!((mm_to_inches(0.0) - 0.0).abs() < 0.001);
    }

    // ──── Price Ceiling Tests ────

    #[tokio::test]
    async fn test_price_ceiling_rejects_expensive() {
        // Token priced at 0.20, max_entry_price=0.15 → should reject
        let token_id = U256::from(1u64);
        let mut books = HashMap::new();
        books.insert(token_id, make_weather_book(token_id, dec!(0.20)));
        books.insert(U256::from(2u64), make_weather_book(U256::from(2u64), dec!(0.80)));

        let held = vec![];
        let books_arc = Arc::new(books);
        let config = WeatherConfig {
            min_edge_bps: 500,
            max_spread_bps: 5000,
            max_position_pct: dec!(0.50),
            kelly_fraction: dec!(0.25),
            forecast_error: ForecastErrorConfig::default(),
            refresh_interval_secs: 3600,
            exit_buffer_bps: 50,
            capital_efficiency_threshold: dec!(0.98),
            dynamic_sigma: false,
            forecast_change_detection: false,
            forecast_change_threshold: 0.5,
            max_entry_price: dec!(0.15),  // Only buy below 15 cents
            profit_take_threshold: dec!(0.45),
            max_position_usdc: dec!(2),
            noaa_user_agent: "test".to_string(),
            target_cities: vec![],
        };
        let strategy = WeatherAlphaStrategy::new(
            config,
            Decimal::ZERO,
            Box::new(move |tid| books_arc.get(&tid).cloned()),
            Box::new(|| Decimal::MAX),
            Box::new(|_| Decimal::ZERO),
            vec![],
            Box::new(move || held.clone()),
            Box::new(|| dec!(200)),
        );

        // Pre-populate cache: model says 80% (strong edge over 0.20 ask)
        let question = "Will the temperature in NYC exceed 50°F on March 5?";
        let cache_key = WeatherAlphaStrategy::question_hash(question);
        {
            let mut cache = strategy.forecast_cache.lock().unwrap();
            cache.insert(cache_key, CachedForecast {
                forecast: ForecastData {
                    values: vec![80.0],
                    dates: vec!["2026-03-05".into()],
                    mean: 80.0,
                    std_dev: 3.0,
                    target_value: Some(80.0),
                    model_spread: 0.0,
                },
                fetched_at: Instant::now(),
                previous_mean: None,
                is_fresh_signal: true,
            });
        }

        let market = make_weather_market(question);
        let parsed = parse_weather_question(question).unwrap();
        let result = strategy.detect_weather_opportunity(&market, &parsed).await;
        assert!(result.is_none(), "Token at 0.20 should be rejected (max_entry_price=0.15)");
    }

    #[tokio::test]
    async fn test_price_ceiling_accepts_cheap() {
        // Token priced at 0.10, max_entry_price=0.15 → should accept
        let token_id = U256::from(1u64);
        let mut books = HashMap::new();
        books.insert(token_id, make_weather_book(token_id, dec!(0.10)));
        books.insert(U256::from(2u64), make_weather_book(U256::from(2u64), dec!(0.90)));

        let held = vec![];
        let books_arc = Arc::new(books);
        let config = WeatherConfig {
            min_edge_bps: 500,
            max_spread_bps: 5000,
            max_position_pct: dec!(0.50),
            kelly_fraction: dec!(0.25),
            forecast_error: ForecastErrorConfig::default(),
            refresh_interval_secs: 3600,
            exit_buffer_bps: 50,
            capital_efficiency_threshold: dec!(0.98),
            dynamic_sigma: false,
            forecast_change_detection: false,
            forecast_change_threshold: 0.5,
            max_entry_price: dec!(0.15),  // Only buy below 15 cents
            profit_take_threshold: dec!(0.45),
            max_position_usdc: dec!(100),
            noaa_user_agent: "test".to_string(),
            target_cities: vec![],
        };
        let strategy = WeatherAlphaStrategy::new(
            config,
            Decimal::ZERO,
            Box::new(move |tid| books_arc.get(&tid).cloned()),
            Box::new(|| Decimal::MAX),
            Box::new(|_| Decimal::ZERO),
            vec![],
            Box::new(move || held.clone()),
            Box::new(|| dec!(200)),
        );

        // Pre-populate cache: model says 80% (strong edge over 0.10 ask)
        let question = "Will the temperature in NYC exceed 50°F on March 5?";
        let cache_key = WeatherAlphaStrategy::question_hash(question);
        {
            let mut cache = strategy.forecast_cache.lock().unwrap();
            cache.insert(cache_key, CachedForecast {
                forecast: ForecastData {
                    values: vec![80.0],
                    dates: vec!["2026-03-05".into()],
                    mean: 80.0,
                    std_dev: 3.0,
                    target_value: Some(80.0),
                    model_spread: 0.0,
                },
                fetched_at: Instant::now(),
                previous_mean: None,
                is_fresh_signal: true,
            });
        }

        let market = make_weather_market(question);
        let parsed = parse_weather_question(question).unwrap();
        let result = strategy.detect_weather_opportunity(&market, &parsed).await;
        assert!(result.is_some(), "Token at 0.10 should be accepted (max_entry_price=0.15)");
    }

    // ──── Profit-Take Exit Tests ────

    #[tokio::test]
    async fn test_profit_take_exit() {
        // Bought YES at 0.10, best_bid rises to 0.50 (> profit_take_threshold=0.45)
        let token_id = U256::from(1u64);
        let mut books = HashMap::new();
        let mut book = make_weather_book(token_id, dec!(0.50));
        book.bids.clear();
        book.bids.push(PriceLevel { price: dec!(0.50), size: dec!(100) });
        books.insert(token_id, book);
        books.insert(U256::from(2u64), make_weather_book(U256::from(2u64), dec!(0.50)));

        let held = vec![(token_id, dec!(20), dec!(0.10))]; // bought at 0.10
        let strategy = make_weather_strategy(books, held);

        let market = make_weather_market("Will the temperature in NYC exceed 100F this summer?");
        let exits = strategy.scan_exits(&[market]).await;
        // best_bid 0.50 >= profit_take_threshold 0.45 → should trigger
        assert_eq!(exits.len(), 1, "Profit-take exit should fire when best_bid >= 0.45");
        assert!(exits[0].question.contains("[EXIT]"));
    }

    #[tokio::test]
    async fn test_profit_take_below_threshold() {
        // Bought YES at 0.10, best_bid at 0.40 (< profit_take_threshold=0.45)
        let token_id = U256::from(1u64);
        let mut books = HashMap::new();
        let mut book = make_weather_book(token_id, dec!(0.50));
        book.bids.clear();
        book.bids.push(PriceLevel { price: dec!(0.40), size: dec!(100) });
        books.insert(token_id, book);
        books.insert(U256::from(2u64), make_weather_book(U256::from(2u64), dec!(0.60)));

        let held = vec![(token_id, dec!(20), dec!(0.10))]; // bought at 0.10
        let strategy = make_weather_strategy(books, held);

        // Pre-populate cache so model reversal doesn't fire
        let question = "Will the temperature in NYC exceed 100F this summer?";
        let cache_key = WeatherAlphaStrategy::question_hash(question);
        {
            let mut cache = strategy.forecast_cache.lock().unwrap();
            cache.insert(cache_key, CachedForecast {
                forecast: ForecastData {
                    values: vec![110.0],
                    dates: vec!["2026-07-01".into()],
                    mean: 110.0,
                    std_dev: 3.0,
                    target_value: Some(110.0),
                    model_spread: 0.0,
                },
                fetched_at: Instant::now(),
                previous_mean: None,
                is_fresh_signal: true,
            });
        }

        let market = make_weather_market(question);
        let exits = strategy.scan_exits(&[market]).await;
        // best_bid 0.40 < profit_take_threshold 0.45 → should NOT trigger profit-take
        // model_prob ≈ 1.0 (forecast 110 >> threshold 100), best_bid 0.40 → no model reversal
        // avg_cost 0.10, best_bid 0.40 → not a deep loss
        assert_eq!(exits.len(), 0, "Should not exit when best_bid < profit_take_threshold");
    }

    // ──── Target City Filter Tests ────

    #[test]
    fn test_target_city_filter_us_cities() {
        let config = WeatherConfig {
            target_cities: vec!["New York".to_string(), "Chicago".to_string()],
            ..Default::default()
        };
        let strategy = WeatherAlphaStrategy::new(
            config,
            Decimal::ZERO,
            Box::new(|_| None),
            Box::new(|| Decimal::ZERO),
            Box::new(|_| Decimal::ZERO),
            vec![],
            Box::new(|| vec![]),
            Box::new(|| Decimal::ZERO),
        );

        assert!(strategy.is_target_city("New York"));
        assert!(strategy.is_target_city("NYC")); // alias
        assert!(strategy.is_target_city("Chicago"));
        assert!(!strategy.is_target_city("London"));
        assert!(!strategy.is_target_city("Miami"));
    }

    #[test]
    fn test_target_city_filter_empty_allows_all() {
        let config = WeatherConfig {
            target_cities: vec![],
            ..Default::default()
        };
        let strategy = WeatherAlphaStrategy::new(
            config,
            Decimal::ZERO,
            Box::new(|_| None),
            Box::new(|| Decimal::ZERO),
            Box::new(|_| Decimal::ZERO),
            vec![],
            Box::new(|| vec![]),
            Box::new(|| Decimal::ZERO),
        );

        assert!(strategy.is_target_city("New York"));
        assert!(strategy.is_target_city("London"));
        assert!(strategy.is_target_city("Anywhere"));
    }

    // ──── Fixed Sizing Tests ────

    #[test]
    fn test_fixed_sizing_caps_at_max() {
        // max_position_usdc = $2, no existing position → size = $2
        let max_usdc = dec!(2);
        let existing_cost = Decimal::ZERO;
        let available = dec!(1000);
        let remaining = (max_usdc - existing_cost).max(Decimal::ZERO);
        let size = max_usdc.min(remaining).min(available);
        assert_eq!(size, dec!(2));
    }

    #[test]
    fn test_fixed_sizing_position_aware() {
        // max_position_usdc = $2, existing $1.50 → remaining $0.50
        let max_usdc = dec!(2);
        let existing_cost = dec!(1.5);
        let available = dec!(1000);
        let remaining = (max_usdc - existing_cost).max(Decimal::ZERO);
        let size = max_usdc.min(remaining).min(available);
        assert_eq!(size, dec!(0.5));
    }

    #[test]
    fn test_fixed_sizing_at_max_skips() {
        // max_position_usdc = $2, existing $2 → remaining $0
        let max_usdc = dec!(2);
        let existing_cost = dec!(2);
        let remaining = (max_usdc - existing_cost).max(Decimal::ZERO);
        assert_eq!(remaining, Decimal::ZERO);
    }
}
