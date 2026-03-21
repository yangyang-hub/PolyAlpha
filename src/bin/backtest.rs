use anyhow::{Context, Result};
use chrono::NaiveDateTime;
use clap::Parser;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

use pa_backtest::data_loader::DataLoader;
use pa_backtest::engine::BacktestEngine;
use pa_backtest::report::BacktestConfig;
use pa_backtest::simulator::SimulatorConfig;
use pa_core::config::Settings;
use pa_storage::repository::Repository;

/// PolyAlpha Backtest Runner
///
/// Replays historical order book snapshots through trading strategies and
/// produces a performance report (PnL, Sharpe ratio, drawdown, win rate).
#[derive(Parser)]
#[command(name = "pa-backtest", about = "PolyAlpha Backtest Runner")]
struct Args {
    /// Start datetime (format: "2025-01-01T00:00:00")
    #[arg(long)]
    from: String,

    /// End datetime (format: "2025-01-31T23:59:59")
    #[arg(long)]
    to: String,

    /// Initial simulated balance in USD
    #[arg(long, default_value = "10000")]
    balance: Decimal,

    /// Slippage in basis points (10 = 0.1%)
    #[arg(long, default_value = "10")]
    slippage_bps: u32,

    /// Fill ratio for FOK orders (1.0 = 100%)
    #[arg(long, default_value = "1.0")]
    fill_ratio: Decimal,

    /// Simulated gas cost per transaction in USD
    #[arg(long, default_value = "0.01")]
    gas_cost: Decimal,

    /// Taker fee rate in basis points (200 = 2%)
    #[arg(long, default_value = "200")]
    fee_rate_bps: u32,

    /// Output format: "text" or "json"
    #[arg(long, default_value = "text")]
    output: String,

    /// Database URL (overrides config/env)
    #[arg(long)]
    database_url: Option<String>,
}

fn parse_datetime(s: &str) -> Result<chrono::DateTime<chrono::Utc>> {
    let naive = NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S").with_context(|| {
        format!("Invalid datetime format: '{s}'. Expected: 2025-01-01T00:00:00")
    })?;
    Ok(naive.and_utc())
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with(fmt::layer().with_target(false))
        .init();

    let args = Args::parse();

    let from = parse_datetime(&args.from)?;
    let to = parse_datetime(&args.to)?;

    if from >= to {
        anyhow::bail!("--from must be before --to");
    }

    // Resolve database URL: CLI arg > env var > config file
    let db_url = if let Some(url) = args.database_url {
        url
    } else if let Ok(url) = std::env::var("PA_DATABASE__URL") {
        url
    } else if let Ok(url) = std::env::var("DATABASE_URL") {
        url
    } else {
        let settings = Settings::load().context("Failed to load configuration")?;
        settings.database.url
    };

    tracing::info!(
        from = %from,
        to = %to,
        balance = %args.balance,
        slippage_bps = args.slippage_bps,
        "Connecting to database..."
    );

    let repo = Repository::connect(&db_url, 5)
        .await
        .context("Failed to connect to database")?;

    let loader = DataLoader::new(repo);
    let engine = BacktestEngine::new(loader);

    let sim_config = SimulatorConfig {
        slippage_bps: args.slippage_bps,
        fill_ratio: args.fill_ratio,
        gas_cost_usd: args.gas_cost,
        fee_rate_bps: args.fee_rate_bps,
    };

    // Load strategy + risk config from settings (or use sensible defaults)
    let (strategy_config, risk_config) = match Settings::load() {
        Ok(s) => (s.strategy, s.risk),
        Err(_) => {
            tracing::warn!("Could not load config file, using defaults");
            (default_strategy_config(), default_risk_config())
        }
    };

    let backtest_config = BacktestConfig {
        from,
        to,
        initial_balance: args.balance,
        simulator: sim_config,
    };

    tracing::info!("Running backtest...");
    let result = engine
        .run(backtest_config, risk_config, strategy_config)
        .await?;

    match args.output.as_str() {
        "json" => {
            let json = result.to_json().context("Failed to serialize result")?;
            println!("{json}");
        }
        _ => {
            result.print_summary();
        }
    }

    Ok(())
}

fn default_strategy_config() -> pa_core::config::StrategyConfig {
    pa_core::config::StrategyConfig {
        enabled: vec!["weather".into(), "crypto".into()],
        scan_interval_ms: 1000,
        min_spread_bps: 50,
        min_profit_usdc: dec!(0.50),
        max_trade_size_usdc: dec!(100),
        order_type: "FOK".into(),
        max_market_end_days: None,
    }
}

fn default_risk_config() -> pa_core::config::RiskConfig {
    pa_core::config::RiskConfig {
        max_position_per_market: dec!(500),
        max_total_exposure: dec!(5000),
        max_daily_loss: dec!(100),
        circuit_breaker_loss: dec!(200),
        circuit_breaker_consecutive_losses: 5,
        max_slippage_bps: 50,
        min_profit_retention_ratio: dec!(0.50),
        min_size_retention_ratio: dec!(0.50),
        execution_quality_profit_weight: Decimal::ONE,
        execution_quality_size_weight: Decimal::ONE,
        execution_quality_slippage_weight: Decimal::ONE,
        min_order_usdc: dec!(1),
        min_profit_usdc: dec!(0.01),
        max_exposure_per_strategy: dec!(5000),
        max_markets_per_strategy: 50,
    }
}
