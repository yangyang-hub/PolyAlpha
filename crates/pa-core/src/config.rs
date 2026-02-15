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
    #[serde(default)]
    pub convergence: ConvergenceConfig,
    #[serde(default)]
    pub crypto_alpha: CryptoAlphaConfig,
    #[serde(default)]
    pub event_calendar: EventCalendarConfig,
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
    /// Signature type for CLOB authentication and balance queries.
    /// 0 = EOA (default), 1 = Proxy (email/magic wallet), 2 = GnosisSafe (browser wallet proxy).
    /// Use 2 if you deposited funds through the Polymarket website.
    #[serde(default)]
    pub signature_type: u8,
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

/// Configuration for the Resolution Convergence strategy.
///
/// Targets tokens priced near 0 or 1 in markets approaching resolution,
/// where outcomes become increasingly certain.
#[derive(Debug, Deserialize, Clone)]
pub struct ConvergenceConfig {
    /// Minimum token price to consider (e.g. 0.93 = 93% implied certainty).
    #[serde(default = "default_min_price_threshold")]
    pub min_price_threshold: Decimal,
    /// Maximum days until resolution to consider a market.
    #[serde(default = "default_max_days_to_resolution")]
    pub max_days_to_resolution: u32,
    /// Maximum position size per market in USDC.
    #[serde(default = "default_conv_max_position")]
    pub max_position_usdc: Decimal,
    /// Kelly fraction cap (0.0-1.0).
    #[serde(default = "default_conv_kelly")]
    pub kelly_fraction: Decimal,
    /// Confidence boost: higher model probability for markets closer to resolution.
    #[serde(default = "default_time_decay_boost")]
    pub time_decay_boost: bool,
}

fn default_min_price_threshold() -> Decimal { Decimal::new(93, 2) }
fn default_max_days_to_resolution() -> u32 { 7 }
fn default_conv_max_position() -> Decimal { Decimal::from(100) }
fn default_conv_kelly() -> Decimal { Decimal::new(25, 2) }
fn default_time_decay_boost() -> bool { true }

impl Default for ConvergenceConfig {
    fn default() -> Self {
        Self {
            min_price_threshold: default_min_price_threshold(),
            max_days_to_resolution: default_max_days_to_resolution(),
            max_position_usdc: default_conv_max_position(),
            kelly_fraction: default_conv_kelly(),
            time_decay_boost: default_time_decay_boost(),
        }
    }
}

/// Configuration for the Crypto Alpha strategy.
///
/// Uses real-time crypto prices + GBM model to find mispriced crypto prediction markets.
#[derive(Debug, Deserialize, Clone)]
pub struct CryptoAlphaConfig {
    /// Minimum edge in basis points.
    #[serde(default = "default_crypto_min_edge")]
    pub min_edge_bps: u32,
    /// Maximum position size per market in USDC.
    #[serde(default = "default_crypto_max_position")]
    pub max_position_usdc: Decimal,
    /// Kelly fraction cap (0.0-1.0).
    #[serde(default = "default_crypto_kelly")]
    pub kelly_fraction: Decimal,
    /// Price data refresh interval (seconds).
    #[serde(default = "default_crypto_refresh")]
    pub refresh_interval_secs: u64,
    /// CoinGecko Demo API key (empty = fallback disabled).
    #[serde(default)]
    pub coingecko_api_key: String,
}

fn default_crypto_min_edge() -> u32 { 500 }
fn default_crypto_max_position() -> Decimal { Decimal::from(100) }
fn default_crypto_kelly() -> Decimal { Decimal::new(25, 2) }
fn default_crypto_refresh() -> u64 { 300 }

impl Default for CryptoAlphaConfig {
    fn default() -> Self {
        Self {
            min_edge_bps: default_crypto_min_edge(),
            max_position_usdc: default_crypto_max_position(),
            kelly_fraction: default_crypto_kelly(),
            refresh_interval_secs: default_crypto_refresh(),
            coingecko_api_key: String::new(),
        }
    }
}

/// Configuration for the Event Calendar filter.
///
/// When enabled, reduces position sizes during high-impact event windows
/// (e.g. FOMC, CPI, token unlocks) to avoid model unreliability.
#[derive(Debug, Deserialize, Clone)]
pub struct EventCalendarConfig {
    /// Whether the event calendar filter is enabled.
    #[serde(default)]
    pub enabled: bool,
    /// Finnhub API key for economic calendar data.
    #[serde(default)]
    pub finnhub_api_key: String,
    /// CoinMarketCal API key for crypto event data.
    #[serde(default)]
    pub coinmarketcal_api_key: String,
    /// How often to refresh event data (seconds).
    #[serde(default = "default_ec_refresh")]
    pub refresh_interval_secs: u64,
    /// Hours before an event to start reducing positions.
    #[serde(default = "default_ec_pre_hours")]
    pub pre_event_hours: u32,
    /// Hours after an event to keep reducing positions.
    #[serde(default = "default_ec_post_hours")]
    pub post_event_hours: u32,
    /// Position multiplier for high-impact events (0.0-1.0).
    #[serde(default = "default_ec_high_mult")]
    pub high_impact_multiplier: Decimal,
    /// Position multiplier for medium-impact events (0.0-1.0).
    #[serde(default = "default_ec_medium_mult")]
    pub medium_impact_multiplier: Decimal,
    /// Position multiplier for low-impact events (0.0-1.0).
    #[serde(default = "default_ec_low_mult")]
    pub low_impact_multiplier: Decimal,
    /// Static/manual events defined in config file.
    #[serde(default)]
    pub static_events: Vec<StaticEventConfig>,
}

fn default_ec_refresh() -> u64 { 3600 }
fn default_ec_pre_hours() -> u32 { 4 }
fn default_ec_post_hours() -> u32 { 2 }
fn default_ec_high_mult() -> Decimal { Decimal::new(25, 2) }   // 0.25
fn default_ec_medium_mult() -> Decimal { Decimal::new(50, 2) } // 0.50
fn default_ec_low_mult() -> Decimal { Decimal::new(75, 2) }    // 0.75

impl Default for EventCalendarConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            finnhub_api_key: String::new(),
            coinmarketcal_api_key: String::new(),
            refresh_interval_secs: default_ec_refresh(),
            pre_event_hours: default_ec_pre_hours(),
            post_event_hours: default_ec_post_hours(),
            high_impact_multiplier: default_ec_high_mult(),
            medium_impact_multiplier: default_ec_medium_mult(),
            low_impact_multiplier: default_ec_low_mult(),
            static_events: vec![],
        }
    }
}

/// A manually-defined event in the config file.
#[derive(Debug, Deserialize, Clone)]
pub struct StaticEventConfig {
    pub title: String,
    /// Category: "macro", "crypto", "political", "sports"
    pub category: String,
    /// ISO 8601 datetime string (e.g. "2026-03-18T18:00:00Z")
    pub event_time: String,
    /// Impact level: "high", "medium", "low"
    pub impact: String,
    #[serde(default)]
    pub keywords: Vec<String>,
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
