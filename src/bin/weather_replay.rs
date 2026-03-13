use anyhow::{Context, Result};
use chrono::NaiveDate;
use clap::Parser;
use serde::Serialize;

use pa_core::weather::{WeatherProvider, weather_location};
use pa_strategy::weather::{NoaaClient, OpenMeteoClient, WeatherMetric};

#[derive(Parser)]
#[command(
    name = "weather-replay",
    about = "Inspect live forecast, archived forecast, and historical value for a weather city/date"
)]
struct Args {
    #[arg(long)]
    location: String,

    #[arg(long, value_parser = ["temp_max", "temp_min", "temp_avg"])]
    metric: String,

    #[arg(long)]
    date: String,

    #[arg(long, default_value = "text", value_parser = ["text", "json"])]
    output: String,
}

#[derive(Debug, Serialize)]
struct ReplayReport {
    location: String,
    provider: String,
    trade_enabled: bool,
    settlement_note: Option<String>,
    target_date: String,
    live_forecast_target_value: Option<f64>,
    historical_forecast_archive_target_value: Option<f64>,
    historical_actual: Option<f64>,
    live_vs_archive_delta: Option<f64>,
    archive_vs_actual_delta: Option<f64>,
    live_vs_actual_delta: Option<f64>,
}

fn parse_metric(metric: &str) -> WeatherMetric {
    match metric {
        "temp_max" => WeatherMetric::TemperatureMax,
        "temp_min" => WeatherMetric::TemperatureMin,
        "temp_avg" => WeatherMetric::TemperatureAvg,
        _ => unreachable!("validated by clap"),
    }
}

fn delta(lhs: Option<f64>, rhs: Option<f64>) -> Option<f64> {
    Some(lhs? - rhs?)
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let target_date = NaiveDate::parse_from_str(&args.date, "%Y-%m-%d")
        .with_context(|| format!("invalid date: {}", args.date))?;
    let metric = parse_metric(&args.metric);

    let location = weather_location(&args.location)
        .with_context(|| format!("unsupported weather location: {}", args.location))?;

    let report = match location.provider {
        WeatherProvider::Noaa => {
            let client = NoaaClient::new("polyalpha-weather-replay/0.1");
            let (lat, lon) = NoaaClient::geocode(location.canonical_name)?;
            let live = client
                .forecast(lat, lon, metric, Some(target_date), "inch")
                .await?;
            let historical_actual = client
                .fetch_historical(lat, lon, metric, target_date, "inch")
                .await
                .ok();

            ReplayReport {
                location: location.canonical_name.to_string(),
                provider: format!("{:?}", location.provider),
                trade_enabled: location.trade_enabled,
                settlement_note: location.settlement_note.map(ToOwned::to_owned),
                target_date: target_date.to_string(),
                live_forecast_target_value: live.target_value,
                historical_forecast_archive_target_value: None,
                historical_actual,
                live_vs_archive_delta: None,
                archive_vs_actual_delta: None,
                live_vs_actual_delta: delta(live.target_value, historical_actual),
            }
        }
        WeatherProvider::OpenMeteo => {
            let client = OpenMeteoClient::new();
            let (lat, lon) = OpenMeteoClient::geocode(location.canonical_name)?;
            let live = client
                .forecast(lat, lon, location.canonical_name, metric, Some(target_date), "inch")
                .await?;
            let archived_forecast = client
                .fetch_historical_forecast(
                    lat,
                    lon,
                    location.canonical_name,
                    metric,
                    target_date,
                    "inch",
                )
                .await
                .ok();
            let historical_actual = client
                .fetch_historical(
                    lat,
                    lon,
                    location.canonical_name,
                    metric,
                    target_date,
                    "inch",
                )
                .await
                .ok();
            let archived_target = archived_forecast.as_ref().and_then(|f| f.target_value);

            ReplayReport {
                location: location.canonical_name.to_string(),
                provider: format!("{:?}", location.provider),
                trade_enabled: location.trade_enabled,
                settlement_note: location.settlement_note.map(ToOwned::to_owned),
                target_date: target_date.to_string(),
                live_forecast_target_value: live.target_value,
                historical_forecast_archive_target_value: archived_target,
                historical_actual,
                live_vs_archive_delta: delta(live.target_value, archived_target),
                archive_vs_actual_delta: delta(archived_target, historical_actual),
                live_vs_actual_delta: delta(live.target_value, historical_actual),
            }
        }
    };

    if args.output == "json" {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    println!("location: {}", report.location);
    println!("provider: {}", report.provider);
    println!("trade_enabled: {}", report.trade_enabled);
    if let Some(note) = &report.settlement_note {
        println!("settlement_note: {note}");
    }
    println!("target_date: {}", report.target_date);
    println!();
    println!("live_forecast_target_value: {:?}", report.live_forecast_target_value);
    println!(
        "historical_forecast_archive_target_value: {:?}",
        report.historical_forecast_archive_target_value
    );
    println!("historical_actual: {:?}", report.historical_actual);
    println!();
    println!("live_vs_archive_delta: {:?}", report.live_vs_archive_delta);
    println!("archive_vs_actual_delta: {:?}", report.archive_vs_actual_delta);
    println!("live_vs_actual_delta: {:?}", report.live_vs_actual_delta);

    Ok(())
}
