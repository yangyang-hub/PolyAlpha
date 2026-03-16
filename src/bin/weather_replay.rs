use anyhow::{Context, Result};
use chrono::NaiveDate;
use clap::Parser;
use serde::Serialize;

use pa_core::config::Settings;
use pa_core::weather::{WeatherProvider, weather_location};
use pa_storage::models::WeatherForecastSnapshotRow;
use pa_storage::repository::Repository;
use pa_strategy::weather::{
    KmaClient, MetOfficeClient, NoaaClient, OpenMeteoClient, WeatherMetric,
};

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

    #[arg(long, default_value_t = false)]
    seed_archive_if_missing: bool,
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

async fn load_db_archived_target(
    provider: WeatherProvider,
    location: &str,
    metric: WeatherMetric,
    target_date: NaiveDate,
) -> Option<f64> {
    let database_url = std::env::var("PA_DATABASE__URL")
        .or_else(|_| std::env::var("DATABASE_URL"))
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            Settings::load()
                .ok()
                .map(|settings| settings.database.url)
                .filter(|value| !value.trim().is_empty())
        })?;
    let repo = Repository::connect(&database_url, 2).await.ok()?;
    let _ = repo.migrate().await;
    let provider = match provider {
        WeatherProvider::Noaa => "noaa",
        WeatherProvider::OpenMeteo => "open_meteo",
        WeatherProvider::Kma => "kma",
        WeatherProvider::MetOffice => "met_office",
    };
    let metric = match metric {
        WeatherMetric::TemperatureMax => "temp_max",
        WeatherMetric::TemperatureMin => "temp_min",
        WeatherMetric::TemperatureAvg => "temp_avg",
        WeatherMetric::Rainfall => "rainfall",
        WeatherMetric::Snowfall => "snowfall",
        WeatherMetric::WindSpeed => "wind_speed",
    };
    repo.load_latest_weather_forecast_snapshot(provider, location, metric, target_date)
        .await
        .ok()
        .flatten()
        .and_then(|row| row.target_value)
}

fn provider_name(provider: WeatherProvider) -> &'static str {
    match provider {
        WeatherProvider::Noaa => "noaa",
        WeatherProvider::OpenMeteo => "open_meteo",
        WeatherProvider::Kma => "kma",
        WeatherProvider::MetOffice => "met_office",
    }
}

fn metric_name(metric: WeatherMetric) -> &'static str {
    match metric {
        WeatherMetric::TemperatureMax => "temp_max",
        WeatherMetric::TemperatureMin => "temp_min",
        WeatherMetric::TemperatureAvg => "temp_avg",
        WeatherMetric::Rainfall => "rainfall",
        WeatherMetric::Snowfall => "snowfall",
        WeatherMetric::WindSpeed => "wind_speed",
    }
}

async fn seed_db_archive_from_forecast(
    provider: WeatherProvider,
    location: &str,
    metric: WeatherMetric,
    target_date: NaiveDate,
    forecast: &pa_strategy::weather::ForecastData,
) -> Option<f64> {
    let database_url = std::env::var("PA_DATABASE__URL")
        .or_else(|_| std::env::var("DATABASE_URL"))
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            Settings::load()
                .ok()
                .map(|settings| settings.database.url)
                .filter(|value| !value.trim().is_empty())
        })?;
    let repo = Repository::connect(&database_url, 2).await.ok()?;
    let _ = repo.migrate().await;
    let provider_name = provider_name(provider).to_string();
    let location = location.to_string();
    let metric_name = metric_name(metric).to_string();
    let recorded_at = chrono::Utc::now();
    let values_json = serde_json::to_value(&forecast.values).ok()?;
    let dates_json = serde_json::to_value(&forecast.dates).ok()?;
    let mut seeded_target = None;
    for (date, value) in forecast.dates.iter().zip(forecast.values.iter()) {
        let row_target_date = NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?;
        if row_target_date == target_date {
            seeded_target = Some(*value);
        }
        let row = WeatherForecastSnapshotRow {
            id: 0,
            provider: provider_name.clone(),
            location: location.clone(),
            metric: metric_name.clone(),
            target_date: row_target_date,
            recorded_at,
            target_value: Some(*value),
            mean: forecast.mean,
            std_dev: forecast.std_dev,
            model_spread: forecast.model_spread,
            values: values_json.clone(),
            dates: dates_json.clone(),
        };
        repo.insert_weather_forecast_snapshot(&row).await.ok()?;
    }
    seeded_target
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

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
                .forecast(
                    lat,
                    lon,
                    location.canonical_name,
                    metric,
                    Some(target_date),
                    "inch",
                )
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
            let db_archived_target = load_db_archived_target(
                location.provider,
                location.canonical_name,
                metric,
                target_date,
            )
            .await;
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
            let mut archived_target = archived_forecast
                .as_ref()
                .and_then(|f| f.target_value)
                .or(db_archived_target);
            if archived_target.is_none() && args.seed_archive_if_missing {
                let seeded_target = seed_db_archive_from_forecast(
                    location.provider,
                    location.canonical_name,
                    metric,
                    target_date,
                    &live,
                )
                .await;
                archived_target = load_db_archived_target(
                    location.provider,
                    location.canonical_name,
                    metric,
                    target_date,
                )
                .await
                .or(seeded_target);
            }

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
        WeatherProvider::Kma => {
            let client = KmaClient::new(
                &std::env::var("PA_WEATHER__KMA_API_KEY")
                    .or_else(|_| std::env::var("KMA_API_KEY"))
                    .unwrap_or_default(),
            );
            let live = client
                .forecast(location.canonical_name, metric, Some(target_date), "inch")
                .await?;
            let archived_forecast = client
                .fetch_historical_forecast(location.canonical_name, metric, target_date, "inch")
                .await
                .ok();
            let db_archived_target = load_db_archived_target(
                location.provider,
                location.canonical_name,
                metric,
                target_date,
            )
            .await;
            let historical_actual = client
                .fetch_historical(location.canonical_name, metric, target_date, "inch")
                .await
                .ok();
            let mut archived_target = archived_forecast
                .as_ref()
                .and_then(|f| f.target_value)
                .or(db_archived_target);
            if archived_target.is_none() && args.seed_archive_if_missing {
                let seeded_target = seed_db_archive_from_forecast(
                    location.provider,
                    location.canonical_name,
                    metric,
                    target_date,
                    &live,
                )
                .await;
                archived_target = load_db_archived_target(
                    location.provider,
                    location.canonical_name,
                    metric,
                    target_date,
                )
                .await
                .or(seeded_target);
            }

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
        WeatherProvider::MetOffice => {
            let client = MetOfficeClient::new(
                &std::env::var("PA_WEATHER__MET_OFFICE_API_KEY")
                    .or_else(|_| std::env::var("MET_OFFICE_API_KEY"))
                    .unwrap_or_default(),
                &std::env::var("PA_WEATHER__MET_OFFICE_OBS_API_KEY")
                    .or_else(|_| std::env::var("MET_OFFICE_OBS_API_KEY"))
                    .unwrap_or_default(),
            );
            let live = client
                .forecast(location.canonical_name, metric, Some(target_date), "inch")
                .await?;
            let archived_forecast = client
                .fetch_historical_forecast(location.canonical_name, metric, target_date, "inch")
                .await
                .ok();
            let db_archived_target = load_db_archived_target(
                location.provider,
                location.canonical_name,
                metric,
                target_date,
            )
            .await;
            let historical_actual = client
                .fetch_historical(location.canonical_name, metric, target_date, "inch")
                .await
                .ok();
            let mut archived_target = archived_forecast
                .as_ref()
                .and_then(|f| f.target_value)
                .or(db_archived_target);
            if archived_target.is_none() && args.seed_archive_if_missing {
                let seeded_target = seed_db_archive_from_forecast(
                    location.provider,
                    location.canonical_name,
                    metric,
                    target_date,
                    &live,
                )
                .await;
                archived_target = load_db_archived_target(
                    location.provider,
                    location.canonical_name,
                    metric,
                    target_date,
                )
                .await
                .or(seeded_target);
            }

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
    println!(
        "live_forecast_target_value: {:?}",
        report.live_forecast_target_value
    );
    println!(
        "historical_forecast_archive_target_value: {:?}",
        report.historical_forecast_archive_target_value
    );
    println!("historical_actual: {:?}", report.historical_actual);
    println!();
    println!("live_vs_archive_delta: {:?}", report.live_vs_archive_delta);
    println!(
        "archive_vs_actual_delta: {:?}",
        report.archive_vs_actual_delta
    );
    println!("live_vs_actual_delta: {:?}", report.live_vs_actual_delta);

    Ok(())
}
