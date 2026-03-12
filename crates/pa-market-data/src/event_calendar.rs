use std::collections::HashMap;
use std::sync::{Arc, LazyLock};
use std::time::Instant;

use chrono::{DateTime, Duration, Utc};
use rust_decimal::Decimal;
use tokio::sync::RwLock;

use pa_core::config::{EventCalendarConfig, StaticEventConfig};
use pa_core::types::{EventCategory, EventImpact};

/// A calendar event that may affect market model reliability.
#[derive(Debug, Clone)]
pub struct CalendarEvent {
    pub title: String,
    pub category: EventCategory,
    pub event_time: DateTime<Utc>,
    pub impact: EventImpact,
    pub keywords: Vec<String>,
    pub source: String,
}

/// Service that tracks upcoming events and computes position multipliers.
///
/// When a market's question matches an active event's keywords, the position
/// size is scaled down by the event's impact multiplier. If multiple events
/// match, the minimum multiplier is used (most conservative).
pub struct EventCalendarService {
    config: EventCalendarConfig,
    events: Arc<RwLock<Vec<CalendarEvent>>>,
    last_refresh: Arc<RwLock<Instant>>,
    http: reqwest::Client,
}

impl EventCalendarService {
    pub fn new(config: EventCalendarConfig) -> Self {
        let static_events = load_static_events(&config.static_events);
        Self {
            config,
            events: Arc::new(RwLock::new(static_events)),
            last_refresh: Arc::new(RwLock::new(Instant::now())),
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap_or_default(),
        }
    }

    /// Refresh events from all configured external providers.
    pub async fn refresh(&self) {
        let mut new_events = load_static_events(&self.config.static_events);

        // Fetch from Finnhub (macro events)
        if !self.config.finnhub_api_key.is_empty() {
            match self.fetch_finnhub().await {
                Ok(events) => {
                    tracing::info!(count = events.len(), "Finnhub events loaded");
                    new_events.extend(events);
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to fetch Finnhub events");
                }
            }
        }

        // Fetch from CoinMarketCal (crypto events)
        if !self.config.coinmarketcal_api_key.is_empty() {
            match self.fetch_coinmarketcal().await {
                Ok(events) => {
                    tracing::info!(count = events.len(), "CoinMarketCal events loaded");
                    new_events.extend(events);
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to fetch CoinMarketCal events");
                }
            }
        }

        let total = new_events.len();
        *self.events.write().await = new_events;
        *self.last_refresh.write().await = Instant::now();
        tracing::info!(total_events = total, "Event calendar refreshed");
    }

    /// Refresh events only if the refresh interval has elapsed.
    pub async fn refresh_if_needed(&self) {
        let elapsed = self.last_refresh.read().await.elapsed();
        if elapsed.as_secs() >= self.config.refresh_interval_secs {
            self.refresh().await;
        }
    }

    /// Get all events whose window includes the given timestamp.
    ///
    /// An event is "active" if `now` is within `[event_time - pre_hours, event_time + post_hours]`.
    pub async fn get_active_events(&self, now: DateTime<Utc>) -> Vec<CalendarEvent> {
        let events = self.events.read().await;
        events
            .iter()
            .filter(|e| {
                let window_start =
                    e.event_time - Duration::hours(self.config.pre_event_hours as i64);
                let window_end =
                    e.event_time + Duration::hours(self.config.post_event_hours as i64);
                now >= window_start && now <= window_end
            })
            .cloned()
            .collect()
    }

    /// Compute the position size multiplier for a market question.
    ///
    /// Returns `Decimal::ONE` if no active events match.
    /// Otherwise returns the minimum multiplier across all matching events.
    pub async fn position_multiplier(&self, question: &str, now: DateTime<Utc>) -> Decimal {
        let active = self.get_active_events(now).await;
        if active.is_empty() {
            return Decimal::ONE;
        }

        let lower_question = question.to_lowercase();
        let mut min_multiplier = Decimal::ONE;

        for event in &active {
            if event_matches_market(event, &lower_question) {
                let mult = self.impact_multiplier(event.impact);
                if mult < min_multiplier {
                    min_multiplier = mult;
                }
            }
        }

        min_multiplier
    }

    /// Number of events currently tracked.
    pub async fn event_count(&self) -> usize {
        self.events.read().await.len()
    }

    fn impact_multiplier(&self, impact: EventImpact) -> Decimal {
        match impact {
            EventImpact::High => self.config.high_impact_multiplier,
            EventImpact::Medium => self.config.medium_impact_multiplier,
            EventImpact::Low => self.config.low_impact_multiplier,
        }
    }

    /// Fetch economic calendar events from Finnhub.
    async fn fetch_finnhub(&self) -> anyhow::Result<Vec<CalendarEvent>> {
        let today = Utc::now().format("%Y-%m-%d").to_string();
        let end = (Utc::now() + Duration::days(30))
            .format("%Y-%m-%d")
            .to_string();

        let url = format!(
            "https://finnhub.io/api/v1/calendar/economic?from={}&to={}&token={}",
            today, end, self.config.finnhub_api_key
        );

        let resp = self.http.get(&url).send().await?;
        let resp = resp.error_for_status()?;
        let body: serde_json::Value = resp.json().await?;

        parse_finnhub_response(&body)
    }

    /// Fetch crypto events from CoinMarketCal.
    async fn fetch_coinmarketcal(&self) -> anyhow::Result<Vec<CalendarEvent>> {
        let today = Utc::now().format("%Y-%m-%d").to_string();
        let end = (Utc::now() + Duration::days(30))
            .format("%Y-%m-%d")
            .to_string();

        let url = format!(
            "https://developers.coinmarketcal.com/v1/events?max=75&dateRangeStart={}&dateRangeEnd={}",
            today, end
        );

        let resp = self
            .http
            .get(&url)
            .header("x-api-key", &self.config.coinmarketcal_api_key)
            .send()
            .await?;
        let resp = resp.error_for_status()?;
        let body: serde_json::Value = resp.json().await?;

        parse_coinmarketcal_response(&body)
    }
}

// ──── Keyword Matching ────

/// Static keyword expansion maps: event title triggers → market question keywords.
/// Allocated once and reused across all calls.
static MACRO_KEYWORD_MAP: LazyLock<HashMap<&'static str, Vec<&'static str>>> =
    LazyLock::new(|| {
        let mut m = HashMap::new();
        m.insert(
            "fomc",
            vec![
                "interest rate",
                "federal reserve",
                "fed",
                "monetary policy",
                "rate cut",
                "rate hike",
            ],
        );
        m.insert("cpi", vec!["inflation", "consumer price", "cpi"]);
        m.insert(
            "nonfarm",
            vec!["employment", "jobs", "labor", "unemployment", "nonfarm"],
        );
        m.insert("gdp", vec!["economic growth", "gdp", "recession"]);
        m
    });

static POLITICAL_KEYWORD_MAP: LazyLock<HashMap<&'static str, Vec<&'static str>>> =
    LazyLock::new(|| {
        let mut m = HashMap::new();
        m.insert(
            "election",
            vec!["election", "vote", "president", "congress", "senate"],
        );
        m.insert("hearing", vec!["hearing", "testimony", "committee"]);
        m
    });

static SPORTS_KEYWORD_MAP: LazyLock<HashMap<&'static str, Vec<&'static str>>> =
    LazyLock::new(|| {
        let mut m = HashMap::new();
        m.insert("super bowl", vec!["super bowl", "nfl", "football"]);
        m.insert("nba finals", vec!["nba", "basketball", "finals"]);
        m.insert("world cup", vec!["world cup", "fifa", "soccer"]);
        m
    });

/// Check whether a calendar event is relevant to a market question.
fn event_matches_market(event: &CalendarEvent, lower_question: &str) -> bool {
    // 1. Direct keyword match: any of the event's keywords found as substring
    for kw in &event.keywords {
        if lower_question.contains(&kw.to_lowercase()) {
            return true;
        }
    }

    // 2. Expanded keyword match: use category-specific keyword maps
    let lower_title = event.title.to_lowercase();

    let maps: &[&HashMap<&str, Vec<&str>>] = match event.category {
        EventCategory::Macro => &[&MACRO_KEYWORD_MAP],
        EventCategory::Political => &[&POLITICAL_KEYWORD_MAP],
        EventCategory::Sports => &[&SPORTS_KEYWORD_MAP],
        EventCategory::Crypto => {
            // For crypto, we rely on direct keywords (coin symbols)
            return false;
        }
    };

    for map in maps {
        for (trigger, expanded) in map.iter() {
            if lower_title.contains(trigger) {
                for kw in expanded {
                    if lower_question.contains(kw) {
                        return true;
                    }
                }
            }
        }
    }

    false
}

// ──── Response Parsing ────

/// Parse Finnhub economic calendar JSON response.
fn parse_finnhub_response(body: &serde_json::Value) -> anyhow::Result<Vec<CalendarEvent>> {
    let mut events = Vec::new();

    let calendar = body
        .get("economicCalendar")
        .and_then(|v| v.as_array())
        .unwrap_or(&Vec::new())
        .clone();

    for item in &calendar {
        let country = item.get("country").and_then(|v| v.as_str()).unwrap_or("");
        if country != "US" {
            continue;
        }

        let title = item
            .get("event")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if title.is_empty() {
            continue;
        }

        let time_str = item.get("time").and_then(|v| v.as_str()).unwrap_or("");
        let event_time = match parse_datetime(time_str) {
            Some(t) => t,
            None => continue,
        };

        let impact = parse_finnhub_impact(item.get("impact"));

        let keywords = extract_macro_keywords(&title);

        events.push(CalendarEvent {
            title,
            category: EventCategory::Macro,
            event_time,
            impact,
            keywords,
            source: "finnhub".to_string(),
        });
    }

    Ok(events)
}

/// Parse CoinMarketCal events JSON response.
fn parse_coinmarketcal_response(body: &serde_json::Value) -> anyhow::Result<Vec<CalendarEvent>> {
    let mut events = Vec::new();

    let body_arr = body
        .get("body")
        .and_then(|v| v.as_array())
        .unwrap_or(&Vec::new())
        .clone();

    for item in &body_arr {
        let title = item
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if title.is_empty() {
            continue;
        }

        let date_str = item
            .get("date_event")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let event_time = match parse_datetime(date_str) {
            Some(t) => t,
            None => continue,
        };

        let percentage = item
            .get("percentage")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let impact = if percentage >= 80.0 {
            EventImpact::High
        } else if percentage >= 50.0 {
            EventImpact::Medium
        } else {
            EventImpact::Low
        };

        // Extract coin symbols and names as keywords
        let mut keywords = Vec::new();
        if let Some(coins) = item.get("coins").and_then(|v| v.as_array()) {
            for coin in coins {
                if let Some(symbol) = coin.get("symbol").and_then(|v| v.as_str()) {
                    keywords.push(symbol.to_lowercase());
                }
                if let Some(fullname) = coin.get("fullname").and_then(|v| v.as_str()) {
                    keywords.push(fullname.to_lowercase());
                }
            }
        }

        events.push(CalendarEvent {
            title,
            category: EventCategory::Crypto,
            event_time,
            impact,
            keywords,
            source: "coinmarketcal".to_string(),
        });
    }

    Ok(events)
}

/// Load static events from config.
fn load_static_events(configs: &[StaticEventConfig]) -> Vec<CalendarEvent> {
    let mut events = Vec::new();
    for cfg in configs {
        let category = match cfg.category.to_lowercase().as_str() {
            "macro" => EventCategory::Macro,
            "crypto" => EventCategory::Crypto,
            "political" => EventCategory::Political,
            "sports" => EventCategory::Sports,
            other => {
                tracing::warn!(category = other, title = %cfg.title, "Unknown event category, skipping");
                continue;
            }
        };

        let impact = match cfg.impact.to_lowercase().as_str() {
            "high" => EventImpact::High,
            "medium" => EventImpact::Medium,
            "low" => EventImpact::Low,
            other => {
                tracing::warn!(impact = other, title = %cfg.title, "Unknown impact level, defaulting to Medium");
                EventImpact::Medium
            }
        };

        let event_time = match parse_datetime(&cfg.event_time) {
            Some(t) => t,
            None => {
                tracing::warn!(time = %cfg.event_time, title = %cfg.title, "Invalid event_time, skipping");
                continue;
            }
        };

        events.push(CalendarEvent {
            title: cfg.title.clone(),
            category,
            event_time,
            impact,
            keywords: cfg.keywords.iter().map(|k| k.to_lowercase()).collect(),
            source: "static".to_string(),
        });
    }
    events
}

// ──── Helpers ────

/// Parse ISO 8601 datetime string.
fn parse_datetime(s: &str) -> Option<DateTime<Utc>> {
    // Try full ISO 8601 with timezone
    if let Ok(dt) = s.parse::<DateTime<Utc>>() {
        return Some(dt);
    }
    // Try without timezone (assume UTC)
    if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S") {
        return Some(naive.and_utc());
    }
    // Try date only
    if let Ok(naive) = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return Some(naive.and_hms_opt(0, 0, 0)?.and_utc());
    }
    None
}

/// Parse Finnhub impact value to EventImpact.
///
/// Finnhub sends impact as either a number (1=low, 2=medium, 3=high)
/// or a string ("low", "medium", "high"). Handle both formats.
fn parse_finnhub_impact(val: Option<&serde_json::Value>) -> EventImpact {
    match val {
        Some(serde_json::Value::String(s)) => match s.to_lowercase().as_str() {
            "high" | "3" => EventImpact::High,
            "medium" | "2" => EventImpact::Medium,
            _ => EventImpact::Low,
        },
        Some(serde_json::Value::Number(n)) => {
            if let Some(i) = n.as_i64() {
                match i {
                    3 => EventImpact::High,
                    2 => EventImpact::Medium,
                    _ => EventImpact::Low,
                }
            } else {
                EventImpact::Low
            }
        }
        _ => EventImpact::Low,
    }
}

/// Extract relevant keywords from a macro event title.
fn extract_macro_keywords(title: &str) -> Vec<String> {
    let lower = title.to_lowercase();
    for (trigger, expanded) in MACRO_KEYWORD_MAP.iter() {
        if lower.contains(trigger) {
            return expanded.iter().map(|s| s.to_string()).collect();
        }
    }
    // Fallback: use the title words as keywords
    lower
        .split_whitespace()
        .filter(|w| w.len() > 3)
        .map(|w| w.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use rust_decimal_macros::dec;
    use serde_json::json;

    fn make_config() -> EventCalendarConfig {
        EventCalendarConfig {
            enabled: true,
            finnhub_api_key: String::new(),
            coinmarketcal_api_key: String::new(),
            refresh_interval_secs: 3600,
            pre_event_hours: 4,
            post_event_hours: 2,
            high_impact_multiplier: dec!(0.25),
            medium_impact_multiplier: dec!(0.50),
            low_impact_multiplier: dec!(0.75),
            static_events: vec![],
        }
    }

    #[test]
    fn test_finnhub_response_parsing() {
        let body = json!({
            "economicCalendar": [
                {
                    "country": "US",
                    "event": "FOMC Rate Decision",
                    "impact": 3,
                    "time": "2026-03-18T18:00:00Z"
                },
                {
                    "country": "EU",
                    "event": "ECB Rate Decision",
                    "impact": 3,
                    "time": "2026-03-20T12:00:00Z"
                },
                {
                    "country": "US",
                    "event": "CPI Release",
                    "impact": 2,
                    "time": "2026-03-12T12:30:00Z"
                },
                {
                    "country": "US",
                    "event": "Jobless Claims",
                    "impact": "low",
                    "time": "2026-03-13T12:30:00Z"
                }
            ]
        });

        let events = parse_finnhub_response(&body).unwrap();
        // Should filter out EU event
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].title, "FOMC Rate Decision");
        assert_eq!(events[0].category, EventCategory::Macro);
        assert_eq!(events[0].impact, EventImpact::High); // numeric 3 → High
        assert_eq!(events[0].source, "finnhub");
        assert_eq!(events[1].title, "CPI Release");
        assert_eq!(events[1].impact, EventImpact::Medium); // numeric 2 → Medium
        assert_eq!(events[2].title, "Jobless Claims");
        assert_eq!(events[2].impact, EventImpact::Low); // string "low" → Low
    }

    #[test]
    fn test_coinmarketcal_response_parsing() {
        let body = json!({
            "body": [
                {
                    "title": "Token Unlock",
                    "coins": [
                        {"symbol": "BTC", "fullname": "Bitcoin"},
                        {"symbol": "ETH", "fullname": "Ethereum"}
                    ],
                    "date_event": "2026-03-01T00:00:00Z",
                    "percentage": 85.0
                },
                {
                    "title": "Mainnet Launch",
                    "coins": [{"symbol": "SOL", "fullname": "Solana"}],
                    "date_event": "2026-03-15T00:00:00Z",
                    "percentage": 45.0
                }
            ]
        });

        let events = parse_coinmarketcal_response(&body).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].title, "Token Unlock");
        assert_eq!(events[0].category, EventCategory::Crypto);
        assert_eq!(events[0].impact, EventImpact::High); // 85% >= 80
        assert!(events[0].keywords.contains(&"btc".to_string()));
        assert!(events[0].keywords.contains(&"bitcoin".to_string()));
        assert_eq!(events[1].impact, EventImpact::Low); // 45% < 50
        assert!(events[1].keywords.contains(&"sol".to_string()));
    }

    #[test]
    fn test_static_event_loading() {
        let configs = vec![
            StaticEventConfig {
                title: "FOMC Meeting".to_string(),
                category: "macro".to_string(),
                event_time: "2026-03-18T18:00:00Z".to_string(),
                impact: "high".to_string(),
                keywords: vec!["fed".to_string(), "interest rate".to_string()],
            },
            StaticEventConfig {
                title: "Unknown Event".to_string(),
                category: "invalid_category".to_string(),
                event_time: "2026-04-01T00:00:00Z".to_string(),
                impact: "low".to_string(),
                keywords: vec![],
            },
            StaticEventConfig {
                title: "Super Bowl".to_string(),
                category: "sports".to_string(),
                event_time: "2026-02-08T23:30:00Z".to_string(),
                impact: "medium".to_string(),
                keywords: vec!["super bowl".to_string(), "nfl".to_string()],
            },
        ];

        let events = load_static_events(&configs);
        // "invalid_category" should be skipped
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].category, EventCategory::Macro);
        assert_eq!(events[0].impact, EventImpact::High);
        assert_eq!(events[1].category, EventCategory::Sports);
    }

    #[tokio::test]
    async fn test_event_within_pre_window() {
        let mut config = make_config();
        config.pre_event_hours = 4;
        config.post_event_hours = 2;

        let event_time = Utc.with_ymd_and_hms(2026, 3, 18, 18, 0, 0).unwrap();
        let now = event_time - Duration::hours(2); // 2h before → within pre-window

        let svc = EventCalendarService::new(config);
        {
            let mut events = svc.events.write().await;
            events.push(CalendarEvent {
                title: "FOMC".to_string(),
                category: EventCategory::Macro,
                event_time,
                impact: EventImpact::High,
                keywords: vec!["fed".to_string()],
                source: "test".to_string(),
            });
        }

        let active = svc.get_active_events(now).await;
        assert_eq!(active.len(), 1);
    }

    #[tokio::test]
    async fn test_event_within_post_window() {
        let mut config = make_config();
        config.pre_event_hours = 4;
        config.post_event_hours = 2;

        let event_time = Utc.with_ymd_and_hms(2026, 3, 18, 18, 0, 0).unwrap();
        let now = event_time + Duration::hours(1); // 1h after → within post-window

        let svc = EventCalendarService::new(config);
        {
            let mut events = svc.events.write().await;
            events.push(CalendarEvent {
                title: "FOMC".to_string(),
                category: EventCategory::Macro,
                event_time,
                impact: EventImpact::High,
                keywords: vec!["fed".to_string()],
                source: "test".to_string(),
            });
        }

        let active = svc.get_active_events(now).await;
        assert_eq!(active.len(), 1);
    }

    #[tokio::test]
    async fn test_event_outside_window() {
        let mut config = make_config();
        config.pre_event_hours = 4;
        config.post_event_hours = 2;

        let event_time = Utc.with_ymd_and_hms(2026, 3, 18, 18, 0, 0).unwrap();
        let now = event_time - Duration::hours(12); // 12h before → outside

        let svc = EventCalendarService::new(config);
        {
            let mut events = svc.events.write().await;
            events.push(CalendarEvent {
                title: "FOMC".to_string(),
                category: EventCategory::Macro,
                event_time,
                impact: EventImpact::High,
                keywords: vec!["fed".to_string()],
                source: "test".to_string(),
            });
        }

        let active = svc.get_active_events(now).await;
        assert!(active.is_empty());
    }

    #[tokio::test]
    async fn test_high_impact_multiplier() {
        let config = make_config();
        let event_time = Utc::now() + Duration::hours(1);

        let svc = EventCalendarService::new(config);
        {
            let mut events = svc.events.write().await;
            events.push(CalendarEvent {
                title: "FOMC Rate Decision".to_string(),
                category: EventCategory::Macro,
                event_time,
                impact: EventImpact::High,
                keywords: vec!["interest rate".to_string()],
                source: "test".to_string(),
            });
        }

        let mult = svc
            .position_multiplier("Will the interest rate increase?", Utc::now())
            .await;
        assert_eq!(mult, dec!(0.25));
    }

    #[tokio::test]
    async fn test_medium_impact_multiplier() {
        let config = make_config();
        let event_time = Utc::now() + Duration::hours(1);

        let svc = EventCalendarService::new(config);
        {
            let mut events = svc.events.write().await;
            events.push(CalendarEvent {
                title: "CPI Release".to_string(),
                category: EventCategory::Macro,
                event_time,
                impact: EventImpact::Medium,
                keywords: vec!["inflation".to_string()],
                source: "test".to_string(),
            });
        }

        let mult = svc
            .position_multiplier("Will inflation exceed 3%?", Utc::now())
            .await;
        assert_eq!(mult, dec!(0.50));
    }

    #[tokio::test]
    async fn test_low_impact_multiplier() {
        let config = make_config();
        let event_time = Utc::now() + Duration::hours(1);

        let svc = EventCalendarService::new(config);
        {
            let mut events = svc.events.write().await;
            events.push(CalendarEvent {
                title: "GDP Report".to_string(),
                category: EventCategory::Macro,
                event_time,
                impact: EventImpact::Low,
                keywords: vec!["gdp".to_string()],
                source: "test".to_string(),
            });
        }

        let mult = svc
            .position_multiplier("Will GDP growth exceed 2%?", Utc::now())
            .await;
        assert_eq!(mult, dec!(0.75));
    }

    #[tokio::test]
    async fn test_overlapping_events_min() {
        let config = make_config();
        let event_time = Utc::now() + Duration::hours(1);

        let svc = EventCalendarService::new(config);
        {
            let mut events = svc.events.write().await;
            // High impact event matching "fed"
            events.push(CalendarEvent {
                title: "FOMC".to_string(),
                category: EventCategory::Macro,
                event_time,
                impact: EventImpact::High,
                keywords: vec!["fed".to_string(), "interest rate".to_string()],
                source: "test".to_string(),
            });
            // Medium impact event also matching "interest rate"
            events.push(CalendarEvent {
                title: "Rate Forecast".to_string(),
                category: EventCategory::Macro,
                event_time,
                impact: EventImpact::Medium,
                keywords: vec!["interest rate".to_string()],
                source: "test".to_string(),
            });
        }

        // Both match — should take minimum (0.25 from High)
        let mult = svc
            .position_multiplier("Will the interest rate change?", Utc::now())
            .await;
        assert_eq!(mult, dec!(0.25));
    }

    #[tokio::test]
    async fn test_keyword_matching_macro() {
        // An FOMC event should match a market about "interest rate"
        // via expanded keyword map, even without explicit keywords
        let config = make_config();
        let event_time = Utc::now() + Duration::hours(1);

        let svc = EventCalendarService::new(config);
        {
            let mut events = svc.events.write().await;
            events.push(CalendarEvent {
                title: "FOMC Rate Decision".to_string(),
                category: EventCategory::Macro,
                event_time,
                impact: EventImpact::High,
                keywords: vec![], // no direct keywords — relies on expansion
                source: "test".to_string(),
            });
        }

        // "federal reserve" is in the FOMC expanded keywords
        let mult = svc
            .position_multiplier("Will the federal reserve cut rates?", Utc::now())
            .await;
        assert_eq!(mult, dec!(0.25));

        // Unrelated market should not match
        let mult_no = svc
            .position_multiplier("Will it snow in NYC tomorrow?", Utc::now())
            .await;
        assert_eq!(mult_no, Decimal::ONE);
    }

    #[tokio::test]
    async fn test_no_match_different_category() {
        // A crypto event should not match a political market
        let config = make_config();
        let event_time = Utc::now() + Duration::hours(1);

        let svc = EventCalendarService::new(config);
        {
            let mut events = svc.events.write().await;
            events.push(CalendarEvent {
                title: "Bitcoin Halving".to_string(),
                category: EventCategory::Crypto,
                event_time,
                impact: EventImpact::High,
                keywords: vec!["btc".to_string(), "bitcoin".to_string()],
                source: "test".to_string(),
            });
        }

        // Political question — crypto event should not match
        let mult = svc
            .position_multiplier("Will the president win the election?", Utc::now())
            .await;
        assert_eq!(mult, Decimal::ONE);
    }
}
