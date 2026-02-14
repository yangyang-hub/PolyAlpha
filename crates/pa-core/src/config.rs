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
    /// Minimum order size in USDC. Orders below this are skipped.
    #[serde(default = "default_min_order_usdc")]
    pub min_order_usdc: Decimal,
    /// Minimum profit in USDC to execute a trade (overrides hard-coded value).
    #[serde(default = "default_min_profit_usdc")]
    pub min_profit_usdc: Decimal,
}

fn default_min_order_usdc() -> Decimal {
    Decimal::ONE
}

fn default_min_profit_usdc() -> Decimal {
    Decimal::new(20, 2) // 0.20
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
    /// Per-metric forecast error standard deviations.
    #[serde(default)]
    pub forecast_error: ForecastErrorConfig,
    /// How often to refresh forecasts (seconds).
    pub refresh_interval_secs: u64,
}

impl Default for WeatherConfig {
    fn default() -> Self {
        Self {
            min_edge_bps: 500,
            max_position_usdc: Decimal::from(50),
            kelly_fraction: Decimal::new(25, 2), // 0.25 (quarter Kelly)
            forecast_error: ForecastErrorConfig::default(),
            refresh_interval_secs: 3600,
        }
    }
}

/// Per-metric forecast error standard deviations (absolute units).
/// These represent the expected error between the forecast and actual observed value.
#[derive(Debug, Deserialize, Clone)]
pub struct ForecastErrorConfig {
    #[serde(default = "default_temp_sigma")]
    pub temperature_sigma_f: f64,
    #[serde(default = "default_precip_sigma")]
    pub precipitation_sigma_in: f64,
    #[serde(default = "default_snow_sigma")]
    pub snowfall_sigma_in: f64,
    #[serde(default = "default_wind_sigma")]
    pub wind_sigma_mph: f64,
}

fn default_temp_sigma() -> f64 { 3.0 }
fn default_precip_sigma() -> f64 { 0.3 }
fn default_snow_sigma() -> f64 { 2.0 }
fn default_wind_sigma() -> f64 { 5.0 }

impl Default for ForecastErrorConfig {
    fn default() -> Self {
        Self {
            temperature_sigma_f: default_temp_sigma(),
            precipitation_sigma_in: default_precip_sigma(),
            snowfall_sigma_in: default_snow_sigma(),
            wind_sigma_mph: default_wind_sigma(),
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
