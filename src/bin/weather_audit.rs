use anyhow::{Context, Result};
use clap::Parser;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;

use pa_core::weather::{normalize_weather_location_name, weather_location};
use pa_strategy::weather::{
    parse_target_date_server_local, parse_weather_event_title, parse_weather_question,
};

#[derive(Parser)]
#[command(
    name = "weather-audit",
    about = "Build a weather market audit sample set"
)]
struct Args {
    /// Output format: text or json
    #[arg(long, default_value = "text")]
    output: String,

    /// Max number of supported event samples to include
    #[arg(long, default_value_t = 20)]
    limit: usize,

    /// Include unsupported weather events in the output
    #[arg(long, default_value_t = false)]
    include_unsupported: bool,
}

#[derive(Debug, Clone, Serialize)]
struct AuditEntry {
    event_title: String,
    question: String,
    location: String,
    normalized_location: Option<String>,
    metric: String,
    target_date: Option<String>,
    weather_supported: bool,
    trade_enabled: bool,
    provider: Option<String>,
}

async fn fetch_weather_events() -> Result<Vec<(String, String)>> {
    let client = reqwest::Client::builder()
        .http1_only()
        .timeout(std::time::Duration::from_secs(30))
        .user_agent("polyalpha-weather-audit/0.1")
        .build()?;

    let terms = ["temperature", "rainfall", "snowfall", "wind speed"];
    let mut seen = BTreeMap::<String, (String, String)>::new();

    for term in terms {
        let resp: Value = client
            .get("https://gamma-api.polymarket.com/public-search")
            .query(&[
                ("q", term),
                ("limit_per_type", "100"),
                ("events_status", "active"),
            ])
            .send()
            .await
            .with_context(|| format!("search failed for term={term}"))?
            .json()
            .await
            .with_context(|| format!("invalid JSON for term={term}"))?;

        if let Some(events) = resp.get("events").and_then(Value::as_array) {
            for event in events {
                let event_title = event
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let Some(markets) = event.get("markets").and_then(Value::as_array) else {
                    continue;
                };
                for market in markets {
                    let question = market
                        .get("question")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    if question.is_empty() {
                        continue;
                    }
                    let key = if event_title.is_empty() {
                        market
                            .get("condition_id")
                            .and_then(Value::as_str)
                            .map(ToOwned::to_owned)
                            .unwrap_or_else(|| question.clone())
                    } else {
                        event_title.clone()
                    };
                    seen.entry(key).or_insert((event_title.clone(), question));
                }
            }
        }
    }

    Ok(seen.into_values().collect())
}

fn to_audit_entry(event_title: String, question: String) -> Option<AuditEntry> {
    if let Some((metric, location)) = parse_weather_event_title(&event_title) {
        let normalized = normalize_weather_location_name(&location).map(ToOwned::to_owned);
        let metadata = normalized.as_deref().and_then(weather_location);
        return Some(AuditEntry {
            target_date: parse_target_date_server_local(&event_title).map(|d| d.to_string()),
            weather_supported: normalized.is_some(),
            trade_enabled: metadata.map(|entry| entry.trade_enabled).unwrap_or(false),
            provider: metadata.map(|entry| format!("{:?}", entry.provider)),
            normalized_location: normalized,
            event_title,
            question,
            location,
            metric: format!("{metric:?}"),
        });
    }

    if let Some(parsed) = parse_weather_question(&question) {
        let normalized = normalize_weather_location_name(&parsed.location).map(ToOwned::to_owned);
        let metadata = normalized.as_deref().and_then(weather_location);
        return Some(AuditEntry {
            target_date: parse_target_date_server_local(&question).map(|d| d.to_string()),
            weather_supported: normalized.is_some(),
            trade_enabled: metadata.map(|entry| entry.trade_enabled).unwrap_or(false),
            provider: metadata.map(|entry| format!("{:?}", entry.provider)),
            normalized_location: normalized,
            event_title,
            question,
            location: parsed.location,
            metric: format!("{:?}", parsed.metric),
        });
    }

    None
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let raw = fetch_weather_events().await?;
    let mut entries: Vec<_> = raw
        .into_iter()
        .filter_map(|(event_title, question)| to_audit_entry(event_title, question))
        .collect();

    entries.sort_by(|a, b| {
        b.weather_supported
            .cmp(&a.weather_supported)
            .then_with(|| a.target_date.cmp(&b.target_date))
            .then_with(|| a.event_title.cmp(&b.event_title))
    });

    let covered_count = entries.iter().filter(|e| e.weather_supported).count();
    let unsupported_count = entries.iter().filter(|e| !e.weather_supported).count();

    let display_entries: Vec<_> = if args.include_unsupported {
        entries.into_iter().take(args.limit).collect()
    } else {
        entries
            .into_iter()
            .filter(|e| e.weather_supported)
            .take(args.limit)
            .collect()
    };

    if args.output == "json" {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "covered_count": covered_count,
                "unsupported_count": unsupported_count,
                "entries": display_entries,
            }))?
        );
        return Ok(());
    }

    println!("Supported weather events: {covered_count}");
    println!("Unsupported weather events: {unsupported_count}");
    println!();
    for (idx, entry) in display_entries.iter().enumerate() {
        println!(
            "{}. [{}] {} | {} | {} | supported={} | trade_enabled={} | provider={}",
            idx + 1,
            entry.metric,
            entry
                .normalized_location
                .as_deref()
                .unwrap_or(entry.location.as_str()),
            entry.target_date.as_deref().unwrap_or("unknown-date"),
            entry.event_title,
            entry.weather_supported,
            entry.trade_enabled,
            entry.provider.as_deref().unwrap_or("-"),
        );
        println!("   {}", entry.question);
    }

    Ok(())
}
