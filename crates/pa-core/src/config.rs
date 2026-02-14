use rust_decimal::Decimal;
use serde::Deserialize;

/// Top-level application settings, loaded from config files + env vars.
#[derive(Debug, Deserialize, Clone)]
pub struct Settings {
    pub chain: ChainConfig,
    pub clob: ClobConfig,
    pub gamma: GammaConfig,
    pub strategy: StrategyConfig,
    pub risk: RiskConfig,
    pub database: DatabaseConfig,
    pub monitor: MonitorConfig,
    pub market_filter: MarketFilterConfig,
    #[serde(default)]
    pub weather: WeatherConfig,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ChainConfig {
    pub chain_id: u64,
    pub rpc_url: String,
    #[serde(default)]
    pub rpc_fallbacks: Vec<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ClobConfig {
    pub host: String,
    pub ws_host: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct GammaConfig {
    pub host: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct StrategyConfig {
    pub enabled: Vec<String>,
    pub scan_interval_ms: u64,
    pub min_spread_bps: u32,
    pub min_profit_usdc: Decimal,
    pub max_trade_size_usdc: Decimal,
    pub order_type: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct RiskConfig {
    pub max_position_per_market: Decimal,
    pub max_total_exposure: Decimal,
    pub max_daily_loss: Decimal,
    pub circuit_breaker_loss: Decimal,
    pub circuit_breaker_consecutive_losses: u32,
    pub max_slippage_bps: u32,
}

#[derive(Debug, Deserialize, Clone)]
pub struct DatabaseConfig {
    pub url: String,
    #[serde(default = "default_max_connections")]
    pub max_connections: u32,
}

fn default_max_connections() -> u32 {
    10
}

#[derive(Debug, Deserialize, Clone)]
pub struct MonitorConfig {
    pub prometheus_port: u16,
    pub health_port: u16,
    #[serde(default)]
    pub alert_webhook: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct MarketFilterConfig {
    pub min_liquidity: Decimal,
    pub min_volume_24h: Decimal,
    pub max_markets: usize,
    pub ws_max_instruments: usize,
}

#[derive(Debug, Deserialize, Clone)]
pub struct WeatherConfig {
    /// Minimum edge (model_prob - market_price) to trigger a trade, in basis points.
    pub min_edge_bps: u32,
    /// Maximum position size per weather market in USDC.
    pub max_position_usdc: Decimal,
    /// Kelly fraction cap (0.0-1.0). Limits position sizing aggressiveness.
    pub kelly_fraction: Decimal,
    /// Forecast uncertainty (std dev) to add to model.
    pub forecast_uncertainty_pct: Decimal,
    /// How often to refresh forecasts (seconds).
    pub refresh_interval_secs: u64,
}

impl Default for WeatherConfig {
    fn default() -> Self {
        Self {
            min_edge_bps: 500,
            max_position_usdc: Decimal::from(50),
            kelly_fraction: Decimal::new(25, 2), // 0.25 (quarter Kelly)
            forecast_uncertainty_pct: Decimal::from(10),
            refresh_interval_secs: 3600,
        }
    }
}

impl Settings {
    /// Load settings from config files and environment variables.
    ///
    /// Priority (highest to lowest):
    /// 1. Environment variables with `PA_` prefix (e.g. `PA_CHAIN__RPC_URL`)
    /// 2. `config/{RUN_MODE}.toml` (defaults to "default")
    /// 3. `config/default.toml`
    pub fn load() -> crate::Result<Self> {
        let run_mode = std::env::var("RUN_MODE").unwrap_or_else(|_| "default".into());

        let settings = config::Config::builder()
            .add_source(config::File::with_name("config/default"))
            .add_source(config::File::with_name(&format!("config/{run_mode}")).required(false))
            .add_source(
                config::Environment::with_prefix("PA")
                    .separator("__")
                    .try_parsing(true)
                    .convert_case(config::Case::Snake),
            )
            .build()?;

        Ok(settings.try_deserialize()?)
    }
}
