use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Top-level application settings, loaded from config files + env vars.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Settings {
    pub chain: ChainConfig,
    pub clob: ClobConfig,
    pub gamma: GammaConfig,
    pub strategy: StrategyConfig,
    pub risk: RiskConfig,
    #[serde(default)]
    pub database: DatabaseConfig,
    pub monitor: MonitorConfig,
    pub market_filter: MarketFilterConfig,
    #[serde(default)]
    pub weather: WeatherConfig,
    #[serde(default)]
    pub crypto_alpha: CryptoAlphaConfig,
    #[serde(default)]
    pub event_calendar: EventCalendarConfig,
    #[serde(default)]
    pub liquidity_rewards: LiquidityRewardsConfig,
    #[serde(default)]
    pub smart_money: SmartMoneyConfig,
    /// Named trading accounts. Trading is disabled unless accounts are provided
    /// via `PA_ACCOUNT_<N>_*` environment variables or TOML `[[accounts]]` sections.
    #[serde(default)]
    pub accounts: Vec<AccountConfig>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ChainConfig {
    pub chain_id: u64,
    pub rpc_url: String,
    #[serde(default)]
    pub rpc_fallbacks: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ClobConfig {
    pub host: String,
    pub ws_host: String,
    /// Signature type for CLOB authentication and balance queries.
    /// 0 = EOA (default), 1 = Proxy (email/magic wallet), 2 = GnosisSafe (browser wallet proxy).
    /// Use 2 if you deposited funds through the Polymarket website.
    #[serde(default)]
    pub signature_type: u8,
    /// Polymarket proxy wallet address for Data API queries.
    /// Leave empty to fallback to the EOA signer address.
    #[serde(default)]
    pub proxy_wallet: String,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct GammaConfig {
    pub host: String,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct StrategyConfig {
    pub enabled: Vec<String>,
    pub scan_interval_ms: u64,
    pub min_spread_bps: u32,
    pub min_profit_usdc: Decimal,
    pub max_trade_size_usdc: Decimal,
    pub order_type: String,
    /// Only trade markets with end_date within this many days. None = no filter.
    #[serde(default)]
    pub max_market_end_days: Option<u64>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct RiskConfig {
    pub max_position_per_market: Decimal,
    pub max_total_exposure: Decimal,
    pub max_daily_loss: Decimal,
    pub circuit_breaker_loss: Decimal,
    pub circuit_breaker_consecutive_losses: u32,
    pub max_slippage_bps: u32,
    /// Minimum fraction of original estimated profit that must remain after
    /// execution freshness repricing for buy orders to stay executable.
    #[serde(default = "default_min_profit_retention_ratio")]
    pub min_profit_retention_ratio: Decimal,
    /// Minimum fraction of original requested size that must remain after
    /// buy-side freshness scaling for the opportunity to stay executable.
    #[serde(default = "default_min_size_retention_ratio")]
    pub min_size_retention_ratio: Decimal,
    /// Execution-quality weight for retained profit when ranking buy opportunities.
    #[serde(default = "default_execution_quality_profit_weight")]
    pub execution_quality_profit_weight: Decimal,
    /// Execution-quality weight for retained size when ranking buy opportunities.
    #[serde(default = "default_execution_quality_size_weight")]
    pub execution_quality_size_weight: Decimal,
    /// Execution-quality weight for slippage quality when ranking buy opportunities.
    #[serde(default = "default_execution_quality_slippage_weight")]
    pub execution_quality_slippage_weight: Decimal,
    /// Minimum order size in USDC. Orders below this are skipped.
    #[serde(default = "default_min_order_usdc")]
    pub min_order_usdc: Decimal,
    /// Minimum profit in USDC to execute a trade (overrides hard-coded value).
    #[serde(default = "default_min_profit_usdc")]
    pub min_profit_usdc: Decimal,
    /// Maximum total USDC exposure per strategy (across all markets).
    #[serde(default = "default_max_exposure_per_strategy")]
    pub max_exposure_per_strategy: Decimal,
    /// Maximum number of distinct markets a single strategy can hold positions in.
    #[serde(default = "default_max_markets_per_strategy")]
    pub max_markets_per_strategy: usize,
}

fn default_min_order_usdc() -> Decimal {
    Decimal::ONE
}

fn default_min_profit_retention_ratio() -> Decimal {
    Decimal::new(50, 2) // 0.50
}

fn default_min_size_retention_ratio() -> Decimal {
    Decimal::new(50, 2) // 0.50
}

fn default_execution_quality_profit_weight() -> Decimal {
    Decimal::ONE
}

fn default_execution_quality_size_weight() -> Decimal {
    Decimal::ONE
}

fn default_execution_quality_slippage_weight() -> Decimal {
    Decimal::ONE
}

fn default_min_profit_usdc() -> Decimal {
    Decimal::new(20, 2) // 0.20
}

fn default_max_exposure_per_strategy() -> Decimal {
    Decimal::from(5000)
}

fn default_max_markets_per_strategy() -> usize {
    50
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct DatabaseConfig {
    #[serde(default)]
    pub url: String,
    #[serde(default = "default_max_connections")]
    pub max_connections: u32,
}

fn default_max_connections() -> u32 {
    10
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            url: String::new(),
            max_connections: default_max_connections(),
        }
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct MonitorConfig {
    pub prometheus_port: u16,
    pub health_port: u16,
    #[serde(default)]
    pub alert_webhook: String,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct MarketFilterConfig {
    pub min_liquidity: Decimal,
    pub min_volume_24h: Decimal,
    pub max_markets: usize,
    pub ws_max_instruments: usize,
    /// How often to re-discover markets from Gamma API (seconds). 0 = disabled.
    #[serde(default = "default_market_refresh_interval")]
    pub market_refresh_interval_secs: u64,
}

fn default_market_refresh_interval() -> u64 {
    1800
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct WeatherConfig {
    /// Minimum edge (model_prob - market_price) to trigger a trade, in basis points.
    pub min_edge_bps: u32,
    /// Maximum bid-ask spread in basis points. Reject markets with wider spreads.
    #[serde(default = "default_max_spread_bps")]
    pub max_spread_bps: u32,
    /// Maximum position size as a fraction of wallet balance (0.0-1.0).
    /// E.g. 0.50 = up to 50% of current balance per market.
    pub max_position_pct: Decimal,
    /// Kelly fraction cap (0.0-1.0). Limits position sizing aggressiveness.
    pub kelly_fraction: Decimal,
    /// Per-metric forecast error standard deviations.
    #[serde(default)]
    pub forecast_error: ForecastErrorConfig,
    /// How often to refresh forecasts (seconds).
    pub refresh_interval_secs: u64,
    /// Exit buffer in basis points. Sell when model_prob < best_bid - exit_buffer.
    #[serde(default = "default_exit_buffer_bps")]
    pub exit_buffer_bps: u32,
    /// Sell when best_bid >= this threshold (capital efficiency exit).
    #[serde(default = "default_capital_efficiency_threshold")]
    pub capital_efficiency_threshold: Decimal,
    /// Scale forecast error sigma by sqrt(days_to_event). More distant forecasts get wider sigma.
    #[serde(default = "default_true")]
    pub dynamic_sigma: bool,
    /// Only trade when the forecast has changed significantly since last check.
    #[serde(default = "default_true")]
    pub forecast_change_detection: bool,
    /// Forecast change threshold in multiples of sigma. Trade only when
    /// |new_forecast - previous_forecast| > threshold * sigma.
    #[serde(default = "default_forecast_change_threshold")]
    pub forecast_change_threshold: f64,
    /// Maximum entry price — only buy tokens priced below this (low-price strategy).
    #[serde(default = "default_max_entry_price")]
    pub max_entry_price: Decimal,
    /// Relative stop-loss ratio — exit when best_bid falls below avg_cost * ratio.
    #[serde(default = "default_relative_stop_loss_ratio")]
    pub relative_stop_loss_ratio: Decimal,
    /// Maximum position size in USDC per trade.
    #[serde(default = "default_weather_max_position_usdc")]
    pub max_position_usdc: Decimal,
    /// NOAA API requires a User-Agent header identifying the application.
    #[serde(default = "default_noaa_user_agent")]
    pub noaa_user_agent: String,
    /// Optional KMA API key for Korea Meteorological Administration forecast access.
    #[serde(default)]
    pub kma_api_key: String,
    /// Optional Met Office Weather DataHub API key for London forecast access.
    #[serde(default)]
    pub met_office_api_key: String,
    /// Optional Met Office Land Observations API key for London actuals access.
    #[serde(default)]
    pub met_office_obs_api_key: String,
    /// Target US cities for weather scanning. Only markets in these cities are scanned.
    #[serde(default = "default_target_cities")]
    pub target_cities: Vec<String>,
}

fn default_exit_buffer_bps() -> u32 {
    50
}
fn default_capital_efficiency_threshold() -> Decimal {
    Decimal::new(98, 2)
} // 0.98
fn default_max_spread_bps() -> u32 {
    1700
} // 17% max spread
fn default_true() -> bool {
    true
}
fn default_forecast_change_threshold() -> f64 {
    0.35
}
fn default_max_entry_price() -> Decimal {
    Decimal::new(38, 2)
} // 0.38
fn default_relative_stop_loss_ratio() -> Decimal {
    Decimal::new(80, 2)
} // 0.80
fn default_weather_max_position_usdc() -> Decimal {
    Decimal::new(4, 0)
} // $4
fn default_noaa_user_agent() -> String {
    "PolyAlpha/1.0".to_string()
}
fn default_target_cities() -> Vec<String> {
    vec![]
}

impl Default for WeatherConfig {
    fn default() -> Self {
        Self {
            min_edge_bps: 450,
            max_spread_bps: default_max_spread_bps(),
            max_position_pct: Decimal::new(50, 2), // 0.50 = 50% of balance
            kelly_fraction: Decimal::new(25, 2),   // 0.25 (quarter Kelly)
            forecast_error: ForecastErrorConfig::default(),
            refresh_interval_secs: 120,
            exit_buffer_bps: default_exit_buffer_bps(),
            capital_efficiency_threshold: default_capital_efficiency_threshold(),
            dynamic_sigma: default_true(),
            forecast_change_detection: false,
            forecast_change_threshold: default_forecast_change_threshold(),
            max_entry_price: default_max_entry_price(),
            relative_stop_loss_ratio: default_relative_stop_loss_ratio(),
            max_position_usdc: default_weather_max_position_usdc(),
            noaa_user_agent: default_noaa_user_agent(),
            kma_api_key: String::new(),
            met_office_api_key: String::new(),
            met_office_obs_api_key: String::new(),
            target_cities: default_target_cities(),
        }
    }
}

/// Per-metric forecast error standard deviations (absolute units).
/// These represent the expected error between the forecast and actual observed value.
#[derive(Debug, Deserialize, Serialize, Clone)]
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

fn default_temp_sigma() -> f64 {
    3.0
}
fn default_precip_sigma() -> f64 {
    0.3
}
fn default_snow_sigma() -> f64 {
    2.0
}
fn default_wind_sigma() -> f64 {
    5.0
}

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

/// Configuration for the Crypto Alpha strategy.
///
/// Uses real-time crypto prices + GBM model to find mispriced crypto prediction markets.
#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct CryptoCalibrationOverride {
    /// Asset selector: BTCUSDT / ETHUSDT / *.
    #[serde(default)]
    pub asset: String,
    /// Asset-class selector: major / alt / any.
    #[serde(default)]
    pub asset_class: String,
    /// Horizon selector: short / medium / long / any.
    #[serde(default)]
    pub horizon: String,
    /// Resolution-bucket selector: same_day / next_day / legacy / any.
    #[serde(default)]
    pub resolution_bucket: String,
    /// Market-type selector: binary / range / any.
    #[serde(default)]
    pub market_type: String,
    /// Event-subtype selector: unlock / upgrade / regulatory / any.
    #[serde(default)]
    pub event_subtype: String,
    /// Optional probability shrink factor override.
    #[serde(default)]
    pub probability_calibration: Option<Decimal>,
    /// Optional sigma multiplier override.
    #[serde(default)]
    pub sigma_multiplier: Option<Decimal>,
    /// Optional entry-size multiplier override.
    #[serde(default)]
    pub size_multiplier: Option<Decimal>,
    /// Optional entry-depth-ratio multiplier override.
    #[serde(default)]
    pub depth_ratio_multiplier: Option<Decimal>,
    /// Optional min-edge multiplier override.
    #[serde(default)]
    pub min_edge_multiplier: Option<Decimal>,
    /// Optional max-spread multiplier override.
    #[serde(default)]
    pub max_spread_multiplier: Option<Decimal>,
    /// Optional hold-edge multiplier override for post-entry management.
    #[serde(default)]
    pub hold_edge_multiplier: Option<Decimal>,
    /// Optional edge-decay exit multiplier override for post-entry management.
    #[serde(default)]
    pub edge_decay_exit_multiplier: Option<Decimal>,
    /// Optional edge-decay confirmation-scan multiplier override for post-entry management.
    #[serde(default)]
    pub edge_decay_confirmation_scan_multiplier: Option<Decimal>,
    /// Optional edge-decay confirmation-window multiplier override for post-entry management.
    #[serde(default)]
    pub edge_decay_confirmation_window_multiplier: Option<Decimal>,
    /// Optional edge-decay cooldown multiplier override for post-entry management.
    #[serde(default)]
    pub edge_decay_cooldown_multiplier: Option<Decimal>,
    /// Optional capital-efficiency threshold multiplier override for post-entry management.
    #[serde(default)]
    pub capital_efficiency_multiplier: Option<Decimal>,
    /// Optional model-reversal exit-buffer multiplier override for post-entry management.
    #[serde(default)]
    pub model_reversal_buffer_multiplier: Option<Decimal>,
    /// Optional buy-side minimum profit-retention multiplier override for execution freshness.
    #[serde(default)]
    pub profit_retention_multiplier: Option<Decimal>,
    /// Optional buy-side slippage-budget multiplier override for execution freshness.
    #[serde(default)]
    pub slippage_multiplier: Option<Decimal>,
    /// Optional buy-side minimum size-retention multiplier override for execution freshness.
    #[serde(default)]
    pub size_retention_multiplier: Option<Decimal>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct CryptoAlphaConfig {
    /// Minimum edge in basis points.
    #[serde(default = "default_crypto_min_edge")]
    pub min_edge_bps: u32,
    /// Maximum position size as a fraction of wallet balance (0.0-1.0).
    #[serde(default = "default_crypto_max_position_pct")]
    pub max_position_pct: Decimal,
    /// Kelly fraction cap (0.0-1.0).
    #[serde(default = "default_crypto_kelly")]
    pub kelly_fraction: Decimal,
    /// Legacy shared price-data refresh interval (seconds). Used as a fallback when the
    /// more granular spot/history/IV refresh intervals are not configured.
    #[serde(default = "default_crypto_refresh")]
    pub refresh_interval_secs: u64,
    /// Spot-price refresh interval (seconds).
    #[serde(default = "default_crypto_spot_refresh")]
    pub spot_refresh_interval_secs: u64,
    /// Historical close refresh interval (seconds).
    #[serde(default = "default_crypto_history_refresh")]
    pub history_refresh_interval_secs: u64,
    /// Implied-volatility refresh interval (seconds).
    #[serde(default = "default_crypto_iv_refresh")]
    pub iv_refresh_interval_secs: u64,
    /// CoinGecko Demo API key (empty = fallback disabled).
    #[serde(default)]
    pub coingecko_api_key: String,
    /// Minimum order-book depth multiple required at the chosen entry limit price.
    #[serde(default = "default_crypto_min_entry_depth_ratio")]
    pub min_entry_depth_ratio: Decimal,
    /// Number of recent gate-scale decisions to inspect when applying adaptive pre-sizing.
    #[serde(default = "default_crypto_gate_scale_feedback_lookback")]
    pub gate_scale_feedback_lookback: usize,
    /// Minimum matching gate-scale count before adaptive pre-sizing begins tightening size.
    #[serde(default = "default_crypto_gate_scale_feedback_trigger_count")]
    pub gate_scale_feedback_trigger_count: u32,
    /// Per-step size multiplier applied once a gate-scale bucket repeatedly pre-scales entries.
    #[serde(default = "default_crypto_gate_scale_feedback_step_multiplier")]
    pub gate_scale_feedback_step_multiplier: Decimal,
    /// Maximum number of adaptive pre-sizing steps applied from repeated gate-scale friction.
    #[serde(default = "default_crypto_gate_scale_feedback_max_steps")]
    pub gate_scale_feedback_max_steps: u32,
    /// Extra Gamma public-search terms appended to the shared crypto discovery term set.
    #[serde(default)]
    pub discovery_search_terms: Vec<String>,
    /// Exit buffer in basis points. Sell when model_prob < best_bid - exit_buffer.
    #[serde(default = "default_exit_buffer_bps")]
    pub exit_buffer_bps: u32,
    /// Sell when best_bid >= this threshold (capital efficiency exit).
    #[serde(default = "default_capital_efficiency_threshold")]
    pub capital_efficiency_threshold: Decimal,
    /// Drift decay factor (0.0 = risk-neutral, 1.0 = full historical drift).
    /// Historical 30-day μ is unreliable for long-horizon predictions.
    /// Default 0.0 aligns with Black-Scholes risk-neutral pricing.
    #[serde(default = "default_drift_decay")]
    pub drift_decay: f64,
    /// Maximum bid-ask spread in basis points. Markets wider than this are skipped.
    /// Crypto markets often have 10-20% spreads; buying into wide spreads causes
    /// immediate mark-to-market losses.
    #[serde(default = "default_crypto_max_spread_bps")]
    pub max_spread_bps: u32,
    /// Relative stop-loss ratio — exit when best_bid falls below avg_cost * ratio.
    #[serde(default = "default_crypto_relative_stop_loss_ratio")]
    pub relative_stop_loss_ratio: Decimal,
    /// Maximum aggregate exposure to a single crypto asset as a fraction of wallet balance.
    #[serde(default = "default_crypto_max_exposure_per_asset_pct")]
    pub max_exposure_per_asset_pct: Decimal,
    /// Maximum aggregate exposure to a single crypto asset direction bucket
    /// as a fraction of wallet balance.
    #[serde(default = "default_crypto_max_exposure_per_asset_direction_pct")]
    pub max_exposure_per_asset_direction_pct: Decimal,
    /// When a low-impact event matches a crypto market, multiply min edge by this factor.
    #[serde(default = "default_crypto_low_event_min_edge_multiplier")]
    pub low_event_min_edge_multiplier: Decimal,
    /// When a medium-impact event matches a crypto market, multiply min edge by this factor.
    #[serde(
        default = "default_crypto_medium_event_min_edge_multiplier",
        alias = "event_min_edge_multiplier"
    )]
    pub medium_event_min_edge_multiplier: Decimal,
    /// When a high-impact event matches a crypto market, multiply min edge by this factor.
    #[serde(default = "default_crypto_high_event_min_edge_multiplier")]
    pub high_event_min_edge_multiplier: Decimal,
    /// When a low-impact event matches a crypto market, multiply max spread by this factor.
    #[serde(default = "default_crypto_low_event_max_spread_multiplier")]
    pub low_event_max_spread_multiplier: Decimal,
    /// When a medium-impact event matches a crypto market, multiply max spread by this factor.
    #[serde(
        default = "default_crypto_medium_event_max_spread_multiplier",
        alias = "event_max_spread_multiplier"
    )]
    pub medium_event_max_spread_multiplier: Decimal,
    /// When a high-impact event matches a crypto market, multiply max spread by this factor.
    #[serde(default = "default_crypto_high_event_max_spread_multiplier")]
    pub high_event_max_spread_multiplier: Decimal,
    /// When a low-impact event matches a crypto market, multiply effective sigma by this factor.
    #[serde(default = "default_crypto_low_event_sigma_multiplier")]
    pub low_event_sigma_multiplier: Decimal,
    /// When a medium-impact event matches a crypto market, multiply effective sigma by this factor.
    #[serde(default = "default_crypto_medium_event_sigma_multiplier")]
    pub medium_event_sigma_multiplier: Decimal,
    /// When a high-impact event matches a crypto market, multiply effective sigma by this factor.
    #[serde(default = "default_crypto_high_event_sigma_multiplier")]
    pub high_event_sigma_multiplier: Decimal,
    /// Additional sigma multiplier for macro-category events.
    #[serde(default = "default_crypto_macro_event_sigma_multiplier")]
    pub macro_event_sigma_multiplier: Decimal,
    /// Additional sigma multiplier for crypto-category events.
    #[serde(default = "default_crypto_crypto_event_sigma_multiplier")]
    pub crypto_event_sigma_multiplier: Decimal,
    /// When a low-impact event matches a crypto market, multiply entry sizing by this factor.
    #[serde(default = "default_crypto_low_event_size_multiplier")]
    pub low_event_size_multiplier: Decimal,
    /// When a medium-impact event matches a crypto market, multiply entry sizing by this factor.
    #[serde(default = "default_crypto_medium_event_size_multiplier")]
    pub medium_event_size_multiplier: Decimal,
    /// When a high-impact event matches a crypto market, multiply entry sizing by this factor.
    #[serde(default = "default_crypto_high_event_size_multiplier")]
    pub high_event_size_multiplier: Decimal,
    /// Additional size multiplier for macro-category events.
    #[serde(default = "default_crypto_macro_event_size_multiplier")]
    pub macro_event_size_multiplier: Decimal,
    /// Additional size multiplier for crypto-category events.
    #[serde(default = "default_crypto_crypto_event_size_multiplier")]
    pub crypto_event_size_multiplier: Decimal,
    /// Probability shrink factor for BTC markets before horizon calibration.
    #[serde(default = "default_crypto_btc_probability_calibration")]
    pub btc_probability_calibration: Decimal,
    /// Probability shrink factor for ETH markets before horizon calibration.
    #[serde(default = "default_crypto_eth_probability_calibration")]
    pub eth_probability_calibration: Decimal,
    /// Probability shrink factor for non-BTC/ETH crypto markets before horizon calibration.
    #[serde(default = "default_crypto_alt_probability_calibration")]
    pub alt_probability_calibration: Decimal,
    /// Probability shrink factor for binary crypto markets after asset/horizon calibration.
    #[serde(default = "default_crypto_binary_probability_calibration")]
    pub binary_probability_calibration: Decimal,
    /// Probability shrink factor for range/NegRisk crypto markets after asset/horizon calibration.
    #[serde(default = "default_crypto_range_probability_calibration")]
    pub range_probability_calibration: Decimal,
    /// Blend factor for runtime probability overrides vs the default baseline factor.
    /// `0` keeps the baseline, `1` applies the override fully.
    #[serde(default = "default_crypto_override_probability_blend")]
    pub override_probability_blend: Decimal,
    /// Maximum absolute deviation allowed between the runtime probability factor and the
    /// default baseline factor, expressed in basis points of the calibration factor scale.
    #[serde(default = "default_crypto_override_probability_max_delta_bps")]
    pub override_probability_max_delta_bps: u32,
    /// Blend factor for runtime multiplier overrides vs the neutral baseline multiplier of `1.0`.
    /// `0` keeps the baseline, `1` applies the override fully.
    #[serde(default = "default_crypto_override_multiplier_blend")]
    pub override_multiplier_blend: Decimal,
    /// Maximum absolute deviation allowed between a runtime multiplier override and the neutral
    /// baseline multiplier of `1.0`, expressed in basis points of the multiplier scale.
    #[serde(default = "default_crypto_override_multiplier_max_delta_bps")]
    pub override_multiplier_max_delta_bps: u32,
    /// Optional table-driven calibration overrides keyed by asset+horizon+market_type.
    #[serde(default)]
    pub calibration_overrides: Vec<CryptoCalibrationOverride>,
    /// Markets resolving within this many days are treated as short-dated.
    #[serde(default = "default_crypto_short_horizon_max_days")]
    pub short_horizon_max_days: u32,
    /// Markets resolving within this many days are treated as medium-dated.
    #[serde(default = "default_crypto_medium_horizon_max_days")]
    pub medium_horizon_max_days: u32,
    /// Hard cap for new crypto entries. Markets beyond this resolution window are ignored for
    /// entry generation but can still be scanned for exits if already held.
    #[serde(default = "default_crypto_max_entry_days")]
    pub max_entry_days: u32,
    /// Additional probability shrink factor for same-day alt markets.
    #[serde(default = "default_crypto_same_day_alt_probability_multiplier")]
    pub same_day_alt_probability_multiplier: Decimal,
    /// Number of distinct recent bad same-day range exits required before temporarily
    /// skipping new same-day range entries for the same asset/subtype bucket.
    #[serde(default = "default_crypto_same_day_range_bad_exit_cooldown_trigger_count")]
    pub same_day_range_bad_exit_cooldown_trigger_count: u32,
    /// Sliding window for same-day range bad-exit cooldown, in seconds.
    #[serde(default = "default_crypto_same_day_range_bad_exit_cooldown_secs")]
    pub same_day_range_bad_exit_cooldown_secs: u64,
    /// Number of distinct recent bad same-day alt directional exits required before temporarily
    /// skipping new same-day alt directional entries for the same asset/subtype bucket.
    #[serde(default = "default_crypto_same_day_alt_bad_exit_cooldown_trigger_count")]
    pub same_day_alt_bad_exit_cooldown_trigger_count: u32,
    /// Sliding window for same-day alt directional bad-exit cooldown, in seconds.
    #[serde(default = "default_crypto_same_day_alt_bad_exit_cooldown_secs")]
    pub same_day_alt_bad_exit_cooldown_secs: u64,
    /// Multiply execution-quality profit-retention weight for same-day entries.
    #[serde(default = "default_crypto_same_day_execution_quality_profit_weight_multiplier")]
    pub same_day_execution_quality_profit_weight_multiplier: Decimal,
    /// Additional execution-quality profit-retention multiplier for same-day alt entries.
    #[serde(default = "default_crypto_same_day_alt_execution_quality_profit_weight_multiplier")]
    pub same_day_alt_execution_quality_profit_weight_multiplier: Decimal,
    /// Additional execution-quality profit-retention multiplier for same-day range/NegRisk entries.
    #[serde(default = "default_crypto_same_day_range_execution_quality_profit_weight_multiplier")]
    pub same_day_range_execution_quality_profit_weight_multiplier: Decimal,
    /// Multiply execution-quality size-retention weight for same-day entries.
    #[serde(default = "default_crypto_same_day_execution_quality_size_weight_multiplier")]
    pub same_day_execution_quality_size_weight_multiplier: Decimal,
    /// Additional execution-quality size-retention multiplier for same-day alt entries.
    #[serde(default = "default_crypto_same_day_alt_execution_quality_size_weight_multiplier")]
    pub same_day_alt_execution_quality_size_weight_multiplier: Decimal,
    /// Additional execution-quality size-retention multiplier for same-day range/NegRisk entries.
    #[serde(default = "default_crypto_same_day_range_execution_quality_size_weight_multiplier")]
    pub same_day_range_execution_quality_size_weight_multiplier: Decimal,
    /// Multiply execution-quality slippage-quality weight for same-day entries.
    #[serde(default = "default_crypto_same_day_execution_quality_slippage_weight_multiplier")]
    pub same_day_execution_quality_slippage_weight_multiplier: Decimal,
    /// Additional execution-quality slippage-quality multiplier for same-day alt entries.
    #[serde(default = "default_crypto_same_day_alt_execution_quality_slippage_weight_multiplier")]
    pub same_day_alt_execution_quality_slippage_weight_multiplier: Decimal,
    /// Additional execution-quality slippage-quality multiplier for same-day range/NegRisk entries.
    #[serde(
        default = "default_crypto_same_day_range_execution_quality_slippage_weight_multiplier"
    )]
    pub same_day_range_execution_quality_slippage_weight_multiplier: Decimal,
    /// Multiply execution-quality profit-retention weight for next-day entries.
    #[serde(default = "default_crypto_short_execution_quality_profit_weight_multiplier")]
    pub short_execution_quality_profit_weight_multiplier: Decimal,
    /// Multiply execution-quality size-retention weight for next-day entries.
    #[serde(default = "default_crypto_short_execution_quality_size_weight_multiplier")]
    pub short_execution_quality_size_weight_multiplier: Decimal,
    /// Multiply execution-quality slippage-quality weight for next-day entries.
    #[serde(default = "default_crypto_short_execution_quality_slippage_weight_multiplier")]
    pub short_execution_quality_slippage_weight_multiplier: Decimal,
    /// Probability shrink factor for same-day markets.
    #[serde(default = "default_crypto_same_day_probability_calibration")]
    pub same_day_probability_calibration: Decimal,
    /// Additional probability shrink factor for same-day range/NegRisk markets.
    #[serde(default = "default_crypto_same_day_range_probability_multiplier")]
    pub same_day_range_probability_multiplier: Decimal,
    /// Probability shrink factor for short-dated markets.
    #[serde(default = "default_crypto_short_horizon_probability_calibration")]
    pub short_horizon_probability_calibration: Decimal,
    /// Probability shrink factor for medium-dated markets.
    #[serde(default = "default_crypto_medium_horizon_probability_calibration")]
    pub medium_horizon_probability_calibration: Decimal,
    /// Multiply entry sizing by this factor for same-day markets.
    #[serde(default = "default_crypto_same_day_size_multiplier")]
    pub same_day_size_multiplier: Decimal,
    /// Additional size multiplier for same-day alt markets.
    #[serde(default = "default_crypto_same_day_alt_size_multiplier")]
    pub same_day_alt_size_multiplier: Decimal,
    /// Additional size multiplier for same-day range/NegRisk markets.
    #[serde(default = "default_crypto_same_day_range_size_multiplier")]
    pub same_day_range_size_multiplier: Decimal,
    /// Multiply entry sizing by this factor for short-dated markets.
    #[serde(default = "default_crypto_short_horizon_size_multiplier")]
    pub short_horizon_size_multiplier: Decimal,
    /// Multiply entry sizing by this factor for medium-dated markets.
    #[serde(default = "default_crypto_medium_horizon_size_multiplier")]
    pub medium_horizon_size_multiplier: Decimal,
    /// Multiply min edge by this factor for same-day markets.
    #[serde(default = "default_crypto_same_day_min_edge_multiplier")]
    pub same_day_min_edge_multiplier: Decimal,
    /// Additional min-edge multiplier for same-day alt markets.
    #[serde(default = "default_crypto_same_day_alt_min_edge_multiplier")]
    pub same_day_alt_min_edge_multiplier: Decimal,
    /// Additional min-edge multiplier for same-day range/NegRisk markets.
    #[serde(default = "default_crypto_same_day_range_min_edge_multiplier")]
    pub same_day_range_min_edge_multiplier: Decimal,
    /// Multiply min edge by this factor for short-dated markets.
    #[serde(default = "default_crypto_short_horizon_min_edge_multiplier")]
    pub short_horizon_min_edge_multiplier: Decimal,
    /// Multiply min edge by this factor for medium-dated markets.
    #[serde(default = "default_crypto_medium_horizon_min_edge_multiplier")]
    pub medium_horizon_min_edge_multiplier: Decimal,
    /// Multiply max spread by this factor for same-day markets.
    #[serde(default = "default_crypto_same_day_max_spread_multiplier")]
    pub same_day_max_spread_multiplier: Decimal,
    /// Additional max-spread multiplier for same-day alt markets.
    #[serde(default = "default_crypto_same_day_alt_max_spread_multiplier")]
    pub same_day_alt_max_spread_multiplier: Decimal,
    /// Additional max-spread multiplier for same-day range/NegRisk markets.
    #[serde(default = "default_crypto_same_day_range_max_spread_multiplier")]
    pub same_day_range_max_spread_multiplier: Decimal,
    /// Multiply max spread by this factor for short-dated markets.
    #[serde(default = "default_crypto_short_horizon_max_spread_multiplier")]
    pub short_horizon_max_spread_multiplier: Decimal,
    /// Multiply max spread by this factor for medium-dated markets.
    #[serde(default = "default_crypto_medium_horizon_max_spread_multiplier")]
    pub medium_horizon_max_spread_multiplier: Decimal,
    /// Capital-efficiency exit threshold for same-day markets.
    #[serde(default = "default_crypto_same_day_capital_efficiency_threshold")]
    pub same_day_capital_efficiency_threshold: Decimal,
    /// Additional capital-efficiency multiplier for same-day alt markets.
    #[serde(default = "default_crypto_same_day_alt_capital_efficiency_multiplier")]
    pub same_day_alt_capital_efficiency_multiplier: Decimal,
    /// Capital-efficiency exit threshold for short-dated markets.
    #[serde(default = "default_crypto_short_horizon_capital_efficiency_threshold")]
    pub short_horizon_capital_efficiency_threshold: Decimal,
    /// Capital-efficiency exit threshold for medium-dated markets.
    #[serde(default = "default_crypto_medium_horizon_capital_efficiency_threshold")]
    pub medium_horizon_capital_efficiency_threshold: Decimal,
    /// Multiply exit buffer by this factor for same-day markets.
    #[serde(default = "default_crypto_same_day_exit_buffer_multiplier")]
    pub same_day_exit_buffer_multiplier: Decimal,
    /// Additional exit-buffer multiplier for same-day alt markets.
    #[serde(default = "default_crypto_same_day_alt_exit_buffer_multiplier")]
    pub same_day_alt_exit_buffer_multiplier: Decimal,
    /// Additional exit-buffer multiplier for same-day range/NegRisk markets.
    #[serde(default = "default_crypto_same_day_range_exit_buffer_multiplier")]
    pub same_day_range_exit_buffer_multiplier: Decimal,
    /// Multiply exit buffer by this factor for short-dated markets.
    #[serde(default = "default_crypto_short_horizon_exit_buffer_multiplier")]
    pub short_horizon_exit_buffer_multiplier: Decimal,
    /// Multiply exit buffer by this factor for medium-dated markets.
    #[serde(default = "default_crypto_medium_horizon_exit_buffer_multiplier")]
    pub medium_horizon_exit_buffer_multiplier: Decimal,
    /// Minimum model edge to keep holding a position, in basis points.
    #[serde(default = "default_crypto_hold_min_edge_bps")]
    pub hold_min_edge_bps: u32,
    /// Multiply hold-min-edge by this factor for same-day markets.
    #[serde(default = "default_crypto_same_day_hold_edge_multiplier")]
    pub same_day_hold_edge_multiplier: Decimal,
    /// Additional hold-edge multiplier for same-day alt markets.
    #[serde(default = "default_crypto_same_day_alt_hold_edge_multiplier")]
    pub same_day_alt_hold_edge_multiplier: Decimal,
    /// Additional hold-edge multiplier for same-day range/NegRisk markets.
    #[serde(default = "default_crypto_same_day_range_hold_edge_multiplier")]
    pub same_day_range_hold_edge_multiplier: Decimal,
    /// Multiply hold-min-edge by this factor for short-dated markets.
    #[serde(default = "default_crypto_short_horizon_hold_edge_multiplier")]
    pub short_horizon_hold_edge_multiplier: Decimal,
    /// Multiply hold-min-edge by this factor for medium-dated markets.
    #[serde(default = "default_crypto_medium_horizon_hold_edge_multiplier")]
    pub medium_horizon_hold_edge_multiplier: Decimal,
    /// Base fraction of position to sell when edge-decay exit triggers.
    #[serde(default = "default_crypto_edge_decay_exit_fraction")]
    pub edge_decay_exit_fraction: Decimal,
    /// Additional fraction to sell for each extra edge-decay confirmation beyond the minimum.
    #[serde(default = "default_crypto_edge_decay_exit_fraction_step")]
    pub edge_decay_exit_fraction_step: Decimal,
    /// Additional thin-edge gap in basis points that upgrades edge-decay to the moderate band.
    #[serde(default = "default_crypto_edge_decay_moderate_gap_bps")]
    pub edge_decay_moderate_gap_bps: u32,
    /// Additional thin-edge gap in basis points that upgrades edge-decay to the severe band.
    #[serde(default = "default_crypto_edge_decay_severe_gap_bps")]
    pub edge_decay_severe_gap_bps: u32,
    /// Multiply edge-decay exit fraction when the thin-edge gap reaches the moderate band.
    #[serde(default = "default_crypto_edge_decay_moderate_exit_multiplier")]
    pub edge_decay_moderate_exit_multiplier: Decimal,
    /// Multiply edge-decay exit fraction when the thin-edge gap reaches the severe band.
    #[serde(default = "default_crypto_edge_decay_severe_exit_multiplier")]
    pub edge_decay_severe_exit_multiplier: Decimal,
    /// Multiply edge-decay cooldown when the thin-edge gap reaches the moderate band.
    #[serde(default = "default_crypto_edge_decay_moderate_cooldown_multiplier")]
    pub edge_decay_moderate_cooldown_multiplier: Decimal,
    /// Multiply edge-decay cooldown when the thin-edge gap reaches the severe band.
    #[serde(default = "default_crypto_edge_decay_severe_cooldown_multiplier")]
    pub edge_decay_severe_cooldown_multiplier: Decimal,
    /// Multiply edge-decay exit fraction for same-day markets.
    #[serde(default = "default_crypto_same_day_edge_decay_exit_multiplier")]
    pub same_day_edge_decay_exit_multiplier: Decimal,
    /// Multiply edge-decay exit fraction for short-dated markets.
    #[serde(default = "default_crypto_short_horizon_edge_decay_exit_multiplier")]
    pub short_horizon_edge_decay_exit_multiplier: Decimal,
    /// Multiply edge-decay exit fraction for medium-dated markets.
    #[serde(default = "default_crypto_medium_horizon_edge_decay_exit_multiplier")]
    pub medium_horizon_edge_decay_exit_multiplier: Decimal,
    /// Cooldown between repeated edge-decay exits on the same token.
    #[serde(default = "default_crypto_edge_decay_cooldown_secs")]
    pub edge_decay_cooldown_secs: u64,
    /// Number of consecutive thin-edge scans required before edge-decay can trigger.
    #[serde(default = "default_crypto_edge_decay_confirmation_scans")]
    pub edge_decay_confirmation_scans: u32,
    /// Confirmation scans required for same-day edge-decay.
    #[serde(default = "default_crypto_same_day_edge_decay_confirmation_scans")]
    pub same_day_edge_decay_confirmation_scans: u32,
    /// Confirmation scans required for short-dated edge-decay.
    #[serde(default = "default_crypto_short_horizon_edge_decay_confirmation_scans")]
    pub short_horizon_edge_decay_confirmation_scans: u32,
    /// Confirmation scans required for medium-dated edge-decay.
    #[serde(default = "default_crypto_medium_horizon_edge_decay_confirmation_scans")]
    pub medium_horizon_edge_decay_confirmation_scans: u32,
    /// Multiply edge-decay confirmation scans when the thin-edge gap reaches the moderate band.
    #[serde(default = "default_crypto_edge_decay_moderate_confirmation_scan_multiplier")]
    pub edge_decay_moderate_confirmation_scan_multiplier: Decimal,
    /// Multiply edge-decay confirmation scans when the thin-edge gap reaches the severe band.
    #[serde(default = "default_crypto_edge_decay_severe_confirmation_scan_multiplier")]
    pub edge_decay_severe_confirmation_scan_multiplier: Decimal,
    /// Maximum allowed gap between thin-edge confirmations before the sequence resets.
    #[serde(default = "default_crypto_edge_decay_confirmation_window_secs")]
    pub edge_decay_confirmation_window_secs: u64,
    /// Multiply edge-decay confirmation window for same-day markets.
    #[serde(default = "default_crypto_same_day_edge_decay_confirmation_window_multiplier")]
    pub same_day_edge_decay_confirmation_window_multiplier: Decimal,
    /// Multiply edge-decay confirmation window for short-dated markets.
    #[serde(default = "default_crypto_short_horizon_edge_decay_confirmation_window_multiplier")]
    pub short_horizon_edge_decay_confirmation_window_multiplier: Decimal,
    /// Multiply edge-decay confirmation window for medium-dated markets.
    #[serde(default = "default_crypto_medium_horizon_edge_decay_confirmation_window_multiplier")]
    pub medium_horizon_edge_decay_confirmation_window_multiplier: Decimal,
    /// Multiply edge-decay confirmation window when the thin-edge gap reaches the moderate band.
    #[serde(default = "default_crypto_edge_decay_moderate_confirmation_window_multiplier")]
    pub edge_decay_moderate_confirmation_window_multiplier: Decimal,
    /// Multiply edge-decay confirmation window when the thin-edge gap reaches the severe band.
    #[serde(default = "default_crypto_edge_decay_severe_confirmation_window_multiplier")]
    pub edge_decay_severe_confirmation_window_multiplier: Decimal,
    /// Multiply edge-decay cooldown for short-dated markets.
    #[serde(default = "default_crypto_short_horizon_edge_decay_cooldown_multiplier")]
    pub short_horizon_edge_decay_cooldown_multiplier: Decimal,
    /// Multiply edge-decay cooldown for same-day markets.
    #[serde(default = "default_crypto_same_day_edge_decay_cooldown_multiplier")]
    pub same_day_edge_decay_cooldown_multiplier: Decimal,
    /// Multiply edge-decay cooldown for medium-dated markets.
    #[serde(default = "default_crypto_medium_horizon_edge_decay_cooldown_multiplier")]
    pub medium_horizon_edge_decay_cooldown_multiplier: Decimal,
}

fn default_drift_decay() -> f64 {
    0.0
}
fn default_crypto_max_spread_bps() -> u32 {
    1500
}
fn default_crypto_min_edge() -> u32 {
    500
}
fn default_crypto_max_position_pct() -> Decimal {
    Decimal::new(50, 2)
} // 0.50
fn default_crypto_kelly() -> Decimal {
    Decimal::new(25, 2)
}
fn default_crypto_refresh() -> u64 {
    300
}
fn default_crypto_spot_refresh() -> u64 {
    30
}
fn default_crypto_history_refresh() -> u64 {
    1800
}
fn default_crypto_iv_refresh() -> u64 {
    300
}
fn default_crypto_min_entry_depth_ratio() -> Decimal {
    Decimal::new(125, 2)
} // 1.25
fn default_crypto_gate_scale_feedback_lookback() -> usize {
    24
}
fn default_crypto_gate_scale_feedback_trigger_count() -> u32 {
    3
}
fn default_crypto_gate_scale_feedback_step_multiplier() -> Decimal {
    Decimal::new(90, 2)
} // 0.90
fn default_crypto_gate_scale_feedback_max_steps() -> u32 {
    2
}
fn default_crypto_relative_stop_loss_ratio() -> Decimal {
    Decimal::new(80, 2)
} // 0.80
fn default_crypto_max_exposure_per_asset_pct() -> Decimal {
    Decimal::new(75, 2)
} // 0.75
fn default_crypto_max_exposure_per_asset_direction_pct() -> Decimal {
    Decimal::new(45, 2)
} // 0.45
fn default_crypto_low_event_min_edge_multiplier() -> Decimal {
    Decimal::new(12, 1)
} // 1.2
fn default_crypto_medium_event_min_edge_multiplier() -> Decimal {
    Decimal::new(15, 1)
} // 1.5
fn default_crypto_high_event_min_edge_multiplier() -> Decimal {
    Decimal::new(20, 1)
} // 2.0
fn default_crypto_low_event_max_spread_multiplier() -> Decimal {
    Decimal::new(90, 2)
} // 0.90
fn default_crypto_medium_event_max_spread_multiplier() -> Decimal {
    Decimal::new(80, 2)
} // 0.80
fn default_crypto_high_event_max_spread_multiplier() -> Decimal {
    Decimal::new(65, 2)
} // 0.65
fn default_crypto_low_event_sigma_multiplier() -> Decimal {
    Decimal::new(105, 2)
} // 1.05
fn default_crypto_medium_event_sigma_multiplier() -> Decimal {
    Decimal::new(115, 2)
} // 1.15
fn default_crypto_high_event_sigma_multiplier() -> Decimal {
    Decimal::new(130, 2)
} // 1.30
fn default_crypto_macro_event_sigma_multiplier() -> Decimal {
    Decimal::new(110, 2)
} // 1.10
fn default_crypto_crypto_event_sigma_multiplier() -> Decimal {
    Decimal::new(120, 2)
} // 1.20
fn default_crypto_low_event_size_multiplier() -> Decimal {
    Decimal::new(90, 2)
} // 0.90
fn default_crypto_medium_event_size_multiplier() -> Decimal {
    Decimal::new(75, 2)
} // 0.75
fn default_crypto_high_event_size_multiplier() -> Decimal {
    Decimal::new(50, 2)
} // 0.50
fn default_crypto_macro_event_size_multiplier() -> Decimal {
    Decimal::new(85, 2)
} // 0.85
fn default_crypto_crypto_event_size_multiplier() -> Decimal {
    Decimal::new(75, 2)
} // 0.75
fn default_crypto_btc_probability_calibration() -> Decimal {
    Decimal::new(95, 2)
} // 0.95
fn default_crypto_eth_probability_calibration() -> Decimal {
    Decimal::new(93, 2)
} // 0.93
fn default_crypto_alt_probability_calibration() -> Decimal {
    Decimal::new(88, 2)
} // 0.88
fn default_crypto_binary_probability_calibration() -> Decimal {
    Decimal::new(97, 2)
} // 0.97
fn default_crypto_range_probability_calibration() -> Decimal {
    Decimal::new(90, 2)
} // 0.90
fn default_crypto_override_probability_blend() -> Decimal {
    Decimal::new(50, 2)
} // 0.50
fn default_crypto_override_probability_max_delta_bps() -> u32 {
    1000
}
fn default_crypto_override_multiplier_blend() -> Decimal {
    Decimal::ONE
}
fn default_crypto_override_multiplier_max_delta_bps() -> u32 {
    2500
}
fn default_crypto_short_horizon_max_days() -> u32 {
    1
}
fn default_crypto_medium_horizon_max_days() -> u32 {
    7
}
fn default_crypto_max_entry_days() -> u32 {
    1
}
fn default_crypto_same_day_range_bad_exit_cooldown_trigger_count() -> u32 {
    2
}
fn default_crypto_same_day_range_bad_exit_cooldown_secs() -> u64 {
    1800
}
fn default_crypto_same_day_alt_bad_exit_cooldown_trigger_count() -> u32 {
    2
}
fn default_crypto_same_day_alt_bad_exit_cooldown_secs() -> u64 {
    1800
}
fn default_crypto_same_day_execution_quality_profit_weight_multiplier() -> Decimal {
    Decimal::new(80, 2)
} // 0.80
fn default_crypto_same_day_alt_execution_quality_profit_weight_multiplier() -> Decimal {
    Decimal::new(90, 2)
} // 0.90
fn default_crypto_same_day_range_execution_quality_profit_weight_multiplier() -> Decimal {
    Decimal::new(85, 2)
} // 0.85
fn default_crypto_same_day_execution_quality_size_weight_multiplier() -> Decimal {
    Decimal::new(120, 2)
} // 1.20
fn default_crypto_same_day_alt_execution_quality_size_weight_multiplier() -> Decimal {
    Decimal::new(110, 2)
} // 1.10
fn default_crypto_same_day_range_execution_quality_size_weight_multiplier() -> Decimal {
    Decimal::new(110, 2)
} // 1.10
fn default_crypto_same_day_execution_quality_slippage_weight_multiplier() -> Decimal {
    Decimal::new(130, 2)
} // 1.30
fn default_crypto_same_day_alt_execution_quality_slippage_weight_multiplier() -> Decimal {
    Decimal::new(115, 2)
} // 1.15
fn default_crypto_same_day_range_execution_quality_slippage_weight_multiplier() -> Decimal {
    Decimal::new(120, 2)
} // 1.20
fn default_crypto_short_execution_quality_profit_weight_multiplier() -> Decimal {
    Decimal::new(115, 2)
} // 1.15
fn default_crypto_short_execution_quality_size_weight_multiplier() -> Decimal {
    Decimal::new(95, 2)
} // 0.95
fn default_crypto_short_execution_quality_slippage_weight_multiplier() -> Decimal {
    Decimal::new(90, 2)
} // 0.90
fn default_crypto_same_day_probability_calibration() -> Decimal {
    Decimal::new(80, 2)
} // 0.80
fn default_crypto_same_day_alt_probability_multiplier() -> Decimal {
    Decimal::new(95, 2)
} // 0.95
fn default_crypto_same_day_range_probability_multiplier() -> Decimal {
    Decimal::new(90, 2)
} // 0.90
fn default_crypto_short_horizon_probability_calibration() -> Decimal {
    Decimal::new(85, 2)
} // 0.85
fn default_crypto_medium_horizon_probability_calibration() -> Decimal {
    Decimal::new(92, 2)
} // 0.92
fn default_crypto_same_day_size_multiplier() -> Decimal {
    Decimal::new(45, 2)
} // 0.45
fn default_crypto_same_day_alt_size_multiplier() -> Decimal {
    Decimal::new(80, 2)
} // 0.80
fn default_crypto_same_day_range_size_multiplier() -> Decimal {
    Decimal::new(75, 2)
} // 0.75
fn default_crypto_short_horizon_size_multiplier() -> Decimal {
    Decimal::new(60, 2)
} // 0.60
fn default_crypto_medium_horizon_size_multiplier() -> Decimal {
    Decimal::new(80, 2)
} // 0.80
fn default_crypto_same_day_min_edge_multiplier() -> Decimal {
    Decimal::new(17, 1)
} // 1.7
fn default_crypto_same_day_alt_min_edge_multiplier() -> Decimal {
    Decimal::new(110, 2)
} // 1.10
fn default_crypto_same_day_range_min_edge_multiplier() -> Decimal {
    Decimal::new(115, 2)
} // 1.15
fn default_crypto_short_horizon_min_edge_multiplier() -> Decimal {
    Decimal::new(15, 1)
} // 1.5
fn default_crypto_medium_horizon_min_edge_multiplier() -> Decimal {
    Decimal::new(12, 1)
} // 1.2
fn default_crypto_same_day_max_spread_multiplier() -> Decimal {
    Decimal::new(65, 2)
} // 0.65
fn default_crypto_same_day_alt_max_spread_multiplier() -> Decimal {
    Decimal::new(85, 2)
} // 0.85
fn default_crypto_same_day_range_max_spread_multiplier() -> Decimal {
    Decimal::new(85, 2)
} // 0.85
fn default_crypto_short_horizon_max_spread_multiplier() -> Decimal {
    Decimal::new(75, 2)
} // 0.75
fn default_crypto_medium_horizon_max_spread_multiplier() -> Decimal {
    Decimal::new(90, 2)
} // 0.90
fn default_crypto_same_day_capital_efficiency_threshold() -> Decimal {
    Decimal::new(90, 2)
} // 0.90
fn default_crypto_same_day_alt_capital_efficiency_multiplier() -> Decimal {
    Decimal::new(98, 2)
} // 0.98
fn default_crypto_short_horizon_capital_efficiency_threshold() -> Decimal {
    Decimal::new(92, 2)
} // 0.92
fn default_crypto_medium_horizon_capital_efficiency_threshold() -> Decimal {
    Decimal::new(95, 2)
} // 0.95
fn default_crypto_same_day_exit_buffer_multiplier() -> Decimal {
    Decimal::new(40, 2)
} // 0.40
fn default_crypto_same_day_alt_exit_buffer_multiplier() -> Decimal {
    Decimal::new(90, 2)
} // 0.90
fn default_crypto_same_day_range_exit_buffer_multiplier() -> Decimal {
    Decimal::new(85, 2)
} // 0.85
fn default_crypto_short_horizon_exit_buffer_multiplier() -> Decimal {
    Decimal::new(50, 2)
} // 0.50
fn default_crypto_medium_horizon_exit_buffer_multiplier() -> Decimal {
    Decimal::new(80, 2)
} // 0.80
fn default_crypto_hold_min_edge_bps() -> u32 {
    100
}
fn default_crypto_same_day_hold_edge_multiplier() -> Decimal {
    Decimal::new(17, 1)
} // 1.7
fn default_crypto_same_day_alt_hold_edge_multiplier() -> Decimal {
    Decimal::new(110, 2)
} // 1.10
fn default_crypto_same_day_range_hold_edge_multiplier() -> Decimal {
    Decimal::new(110, 2)
} // 1.10
fn default_crypto_short_horizon_hold_edge_multiplier() -> Decimal {
    Decimal::new(15, 1)
} // 1.5
fn default_crypto_medium_horizon_hold_edge_multiplier() -> Decimal {
    Decimal::new(12, 1)
} // 1.2
fn default_crypto_edge_decay_exit_fraction() -> Decimal {
    Decimal::new(25, 2)
} // 0.25
fn default_crypto_edge_decay_exit_fraction_step() -> Decimal {
    Decimal::new(10, 2)
} // 0.10
fn default_crypto_edge_decay_moderate_gap_bps() -> u32 {
    50
}
fn default_crypto_edge_decay_severe_gap_bps() -> u32 {
    150
}
fn default_crypto_edge_decay_moderate_exit_multiplier() -> Decimal {
    Decimal::new(125, 2)
} // 1.25
fn default_crypto_edge_decay_severe_exit_multiplier() -> Decimal {
    Decimal::new(150, 2)
} // 1.50
fn default_crypto_edge_decay_moderate_cooldown_multiplier() -> Decimal {
    Decimal::new(75, 2)
} // 0.75
fn default_crypto_edge_decay_severe_cooldown_multiplier() -> Decimal {
    Decimal::new(50, 2)
} // 0.50
fn default_crypto_short_horizon_edge_decay_exit_multiplier() -> Decimal {
    Decimal::new(15, 1)
} // 1.5
fn default_crypto_same_day_edge_decay_exit_multiplier() -> Decimal {
    Decimal::new(18, 1)
} // 1.8
fn default_crypto_medium_horizon_edge_decay_exit_multiplier() -> Decimal {
    Decimal::new(12, 1)
} // 1.2
fn default_crypto_edge_decay_cooldown_secs() -> u64 {
    1800
}
fn default_crypto_edge_decay_confirmation_scans() -> u32 {
    2
}
fn default_crypto_short_horizon_edge_decay_confirmation_scans() -> u32 {
    1
}
fn default_crypto_same_day_edge_decay_confirmation_scans() -> u32 {
    1
}
fn default_crypto_medium_horizon_edge_decay_confirmation_scans() -> u32 {
    2
}
fn default_crypto_edge_decay_moderate_confirmation_scan_multiplier() -> Decimal {
    Decimal::new(75, 2)
} // 0.75
fn default_crypto_edge_decay_severe_confirmation_scan_multiplier() -> Decimal {
    Decimal::new(50, 2)
} // 0.50
fn default_crypto_edge_decay_confirmation_window_secs() -> u64 {
    900
}
fn default_crypto_short_horizon_edge_decay_confirmation_window_multiplier() -> Decimal {
    Decimal::new(50, 2)
} // 0.50
fn default_crypto_same_day_edge_decay_confirmation_window_multiplier() -> Decimal {
    Decimal::new(40, 2)
} // 0.40
fn default_crypto_medium_horizon_edge_decay_confirmation_window_multiplier() -> Decimal {
    Decimal::new(75, 2)
} // 0.75
fn default_crypto_edge_decay_moderate_confirmation_window_multiplier() -> Decimal {
    Decimal::new(75, 2)
} // 0.75
fn default_crypto_edge_decay_severe_confirmation_window_multiplier() -> Decimal {
    Decimal::new(50, 2)
} // 0.50
fn default_crypto_short_horizon_edge_decay_cooldown_multiplier() -> Decimal {
    Decimal::new(50, 2)
} // 0.50
fn default_crypto_same_day_edge_decay_cooldown_multiplier() -> Decimal {
    Decimal::new(40, 2)
} // 0.40
fn default_crypto_medium_horizon_edge_decay_cooldown_multiplier() -> Decimal {
    Decimal::new(75, 2)
} // 0.75

impl Default for CryptoAlphaConfig {
    fn default() -> Self {
        Self {
            min_edge_bps: default_crypto_min_edge(),
            max_position_pct: default_crypto_max_position_pct(),
            kelly_fraction: default_crypto_kelly(),
            refresh_interval_secs: default_crypto_refresh(),
            spot_refresh_interval_secs: default_crypto_spot_refresh(),
            history_refresh_interval_secs: default_crypto_history_refresh(),
            iv_refresh_interval_secs: default_crypto_iv_refresh(),
            coingecko_api_key: String::new(),
            min_entry_depth_ratio: default_crypto_min_entry_depth_ratio(),
            gate_scale_feedback_lookback: default_crypto_gate_scale_feedback_lookback(),
            gate_scale_feedback_trigger_count: default_crypto_gate_scale_feedback_trigger_count(),
            gate_scale_feedback_step_multiplier: default_crypto_gate_scale_feedback_step_multiplier(
            ),
            gate_scale_feedback_max_steps: default_crypto_gate_scale_feedback_max_steps(),
            discovery_search_terms: Vec::new(),
            exit_buffer_bps: default_exit_buffer_bps(),
            capital_efficiency_threshold: default_capital_efficiency_threshold(),
            drift_decay: default_drift_decay(),
            max_spread_bps: default_crypto_max_spread_bps(),
            relative_stop_loss_ratio: default_crypto_relative_stop_loss_ratio(),
            max_exposure_per_asset_pct: default_crypto_max_exposure_per_asset_pct(),
            max_exposure_per_asset_direction_pct:
                default_crypto_max_exposure_per_asset_direction_pct(),
            low_event_min_edge_multiplier: default_crypto_low_event_min_edge_multiplier(),
            medium_event_min_edge_multiplier: default_crypto_medium_event_min_edge_multiplier(),
            high_event_min_edge_multiplier: default_crypto_high_event_min_edge_multiplier(),
            low_event_max_spread_multiplier: default_crypto_low_event_max_spread_multiplier(),
            medium_event_max_spread_multiplier: default_crypto_medium_event_max_spread_multiplier(),
            high_event_max_spread_multiplier: default_crypto_high_event_max_spread_multiplier(),
            low_event_sigma_multiplier: default_crypto_low_event_sigma_multiplier(),
            medium_event_sigma_multiplier: default_crypto_medium_event_sigma_multiplier(),
            high_event_sigma_multiplier: default_crypto_high_event_sigma_multiplier(),
            macro_event_sigma_multiplier: default_crypto_macro_event_sigma_multiplier(),
            crypto_event_sigma_multiplier: default_crypto_crypto_event_sigma_multiplier(),
            low_event_size_multiplier: default_crypto_low_event_size_multiplier(),
            medium_event_size_multiplier: default_crypto_medium_event_size_multiplier(),
            high_event_size_multiplier: default_crypto_high_event_size_multiplier(),
            macro_event_size_multiplier: default_crypto_macro_event_size_multiplier(),
            crypto_event_size_multiplier: default_crypto_crypto_event_size_multiplier(),
            btc_probability_calibration: default_crypto_btc_probability_calibration(),
            eth_probability_calibration: default_crypto_eth_probability_calibration(),
            alt_probability_calibration: default_crypto_alt_probability_calibration(),
            binary_probability_calibration: default_crypto_binary_probability_calibration(),
            range_probability_calibration: default_crypto_range_probability_calibration(),
            override_probability_blend: default_crypto_override_probability_blend(),
            override_probability_max_delta_bps: default_crypto_override_probability_max_delta_bps(),
            override_multiplier_blend: default_crypto_override_multiplier_blend(),
            override_multiplier_max_delta_bps: default_crypto_override_multiplier_max_delta_bps(),
            calibration_overrides: Vec::new(),
            short_horizon_max_days: default_crypto_short_horizon_max_days(),
            medium_horizon_max_days: default_crypto_medium_horizon_max_days(),
            max_entry_days: default_crypto_max_entry_days(),
            same_day_alt_probability_multiplier: default_crypto_same_day_alt_probability_multiplier(
            ),
            same_day_range_bad_exit_cooldown_trigger_count:
                default_crypto_same_day_range_bad_exit_cooldown_trigger_count(),
            same_day_range_bad_exit_cooldown_secs:
                default_crypto_same_day_range_bad_exit_cooldown_secs(),
            same_day_alt_bad_exit_cooldown_trigger_count:
                default_crypto_same_day_alt_bad_exit_cooldown_trigger_count(),
            same_day_alt_bad_exit_cooldown_secs: default_crypto_same_day_alt_bad_exit_cooldown_secs(
            ),
            same_day_execution_quality_profit_weight_multiplier:
                default_crypto_same_day_execution_quality_profit_weight_multiplier(),
            same_day_alt_execution_quality_profit_weight_multiplier:
                default_crypto_same_day_alt_execution_quality_profit_weight_multiplier(),
            same_day_range_execution_quality_profit_weight_multiplier:
                default_crypto_same_day_range_execution_quality_profit_weight_multiplier(),
            same_day_execution_quality_size_weight_multiplier:
                default_crypto_same_day_execution_quality_size_weight_multiplier(),
            same_day_alt_execution_quality_size_weight_multiplier:
                default_crypto_same_day_alt_execution_quality_size_weight_multiplier(),
            same_day_range_execution_quality_size_weight_multiplier:
                default_crypto_same_day_range_execution_quality_size_weight_multiplier(),
            same_day_execution_quality_slippage_weight_multiplier:
                default_crypto_same_day_execution_quality_slippage_weight_multiplier(),
            same_day_alt_execution_quality_slippage_weight_multiplier:
                default_crypto_same_day_alt_execution_quality_slippage_weight_multiplier(),
            same_day_range_execution_quality_slippage_weight_multiplier:
                default_crypto_same_day_range_execution_quality_slippage_weight_multiplier(),
            short_execution_quality_profit_weight_multiplier:
                default_crypto_short_execution_quality_profit_weight_multiplier(),
            short_execution_quality_size_weight_multiplier:
                default_crypto_short_execution_quality_size_weight_multiplier(),
            short_execution_quality_slippage_weight_multiplier:
                default_crypto_short_execution_quality_slippage_weight_multiplier(),
            same_day_probability_calibration: default_crypto_same_day_probability_calibration(),
            same_day_range_probability_multiplier:
                default_crypto_same_day_range_probability_multiplier(),
            short_horizon_probability_calibration:
                default_crypto_short_horizon_probability_calibration(),
            medium_horizon_probability_calibration:
                default_crypto_medium_horizon_probability_calibration(),
            same_day_size_multiplier: default_crypto_same_day_size_multiplier(),
            same_day_alt_size_multiplier: default_crypto_same_day_alt_size_multiplier(),
            same_day_range_size_multiplier: default_crypto_same_day_range_size_multiplier(),
            short_horizon_size_multiplier: default_crypto_short_horizon_size_multiplier(),
            medium_horizon_size_multiplier: default_crypto_medium_horizon_size_multiplier(),
            same_day_min_edge_multiplier: default_crypto_same_day_min_edge_multiplier(),
            same_day_alt_min_edge_multiplier: default_crypto_same_day_alt_min_edge_multiplier(),
            same_day_range_min_edge_multiplier: default_crypto_same_day_range_min_edge_multiplier(),
            short_horizon_min_edge_multiplier: default_crypto_short_horizon_min_edge_multiplier(),
            medium_horizon_min_edge_multiplier: default_crypto_medium_horizon_min_edge_multiplier(),
            same_day_max_spread_multiplier: default_crypto_same_day_max_spread_multiplier(),
            same_day_alt_max_spread_multiplier: default_crypto_same_day_alt_max_spread_multiplier(),
            same_day_range_max_spread_multiplier:
                default_crypto_same_day_range_max_spread_multiplier(),
            short_horizon_max_spread_multiplier: default_crypto_short_horizon_max_spread_multiplier(
            ),
            medium_horizon_max_spread_multiplier:
                default_crypto_medium_horizon_max_spread_multiplier(),
            same_day_capital_efficiency_threshold:
                default_crypto_same_day_capital_efficiency_threshold(),
            same_day_alt_capital_efficiency_multiplier:
                default_crypto_same_day_alt_capital_efficiency_multiplier(),
            short_horizon_capital_efficiency_threshold:
                default_crypto_short_horizon_capital_efficiency_threshold(),
            medium_horizon_capital_efficiency_threshold:
                default_crypto_medium_horizon_capital_efficiency_threshold(),
            same_day_exit_buffer_multiplier: default_crypto_same_day_exit_buffer_multiplier(),
            same_day_alt_exit_buffer_multiplier: default_crypto_same_day_alt_exit_buffer_multiplier(
            ),
            same_day_range_exit_buffer_multiplier:
                default_crypto_same_day_range_exit_buffer_multiplier(),
            short_horizon_exit_buffer_multiplier:
                default_crypto_short_horizon_exit_buffer_multiplier(),
            medium_horizon_exit_buffer_multiplier:
                default_crypto_medium_horizon_exit_buffer_multiplier(),
            hold_min_edge_bps: default_crypto_hold_min_edge_bps(),
            same_day_hold_edge_multiplier: default_crypto_same_day_hold_edge_multiplier(),
            same_day_alt_hold_edge_multiplier: default_crypto_same_day_alt_hold_edge_multiplier(),
            same_day_range_hold_edge_multiplier: default_crypto_same_day_range_hold_edge_multiplier(
            ),
            short_horizon_hold_edge_multiplier: default_crypto_short_horizon_hold_edge_multiplier(),
            medium_horizon_hold_edge_multiplier: default_crypto_medium_horizon_hold_edge_multiplier(
            ),
            edge_decay_exit_fraction: default_crypto_edge_decay_exit_fraction(),
            edge_decay_exit_fraction_step: default_crypto_edge_decay_exit_fraction_step(),
            edge_decay_moderate_gap_bps: default_crypto_edge_decay_moderate_gap_bps(),
            edge_decay_severe_gap_bps: default_crypto_edge_decay_severe_gap_bps(),
            edge_decay_moderate_exit_multiplier: default_crypto_edge_decay_moderate_exit_multiplier(
            ),
            edge_decay_severe_exit_multiplier: default_crypto_edge_decay_severe_exit_multiplier(),
            edge_decay_moderate_cooldown_multiplier:
                default_crypto_edge_decay_moderate_cooldown_multiplier(),
            edge_decay_severe_cooldown_multiplier:
                default_crypto_edge_decay_severe_cooldown_multiplier(),
            same_day_edge_decay_exit_multiplier: default_crypto_same_day_edge_decay_exit_multiplier(
            ),
            short_horizon_edge_decay_exit_multiplier:
                default_crypto_short_horizon_edge_decay_exit_multiplier(),
            medium_horizon_edge_decay_exit_multiplier:
                default_crypto_medium_horizon_edge_decay_exit_multiplier(),
            edge_decay_cooldown_secs: default_crypto_edge_decay_cooldown_secs(),
            edge_decay_confirmation_scans: default_crypto_edge_decay_confirmation_scans(),
            same_day_edge_decay_confirmation_scans:
                default_crypto_same_day_edge_decay_confirmation_scans(),
            short_horizon_edge_decay_confirmation_scans:
                default_crypto_short_horizon_edge_decay_confirmation_scans(),
            medium_horizon_edge_decay_confirmation_scans:
                default_crypto_medium_horizon_edge_decay_confirmation_scans(),
            edge_decay_moderate_confirmation_scan_multiplier:
                default_crypto_edge_decay_moderate_confirmation_scan_multiplier(),
            edge_decay_severe_confirmation_scan_multiplier:
                default_crypto_edge_decay_severe_confirmation_scan_multiplier(),
            edge_decay_confirmation_window_secs: default_crypto_edge_decay_confirmation_window_secs(
            ),
            same_day_edge_decay_confirmation_window_multiplier:
                default_crypto_same_day_edge_decay_confirmation_window_multiplier(),
            short_horizon_edge_decay_confirmation_window_multiplier:
                default_crypto_short_horizon_edge_decay_confirmation_window_multiplier(),
            medium_horizon_edge_decay_confirmation_window_multiplier:
                default_crypto_medium_horizon_edge_decay_confirmation_window_multiplier(),
            edge_decay_moderate_confirmation_window_multiplier:
                default_crypto_edge_decay_moderate_confirmation_window_multiplier(),
            edge_decay_severe_confirmation_window_multiplier:
                default_crypto_edge_decay_severe_confirmation_window_multiplier(),
            same_day_edge_decay_cooldown_multiplier:
                default_crypto_same_day_edge_decay_cooldown_multiplier(),
            short_horizon_edge_decay_cooldown_multiplier:
                default_crypto_short_horizon_edge_decay_cooldown_multiplier(),
            medium_horizon_edge_decay_cooldown_multiplier:
                default_crypto_medium_horizon_edge_decay_cooldown_multiplier(),
        }
    }
}

/// Configuration for the Event Calendar filter.
///
/// When enabled, reduces position sizes during high-impact event windows
/// (e.g. FOMC, CPI, token unlocks) to avoid model unreliability.
#[derive(Debug, Deserialize, Serialize, Clone)]
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

fn default_ec_refresh() -> u64 {
    3600
}
fn default_ec_pre_hours() -> u32 {
    4
}
fn default_ec_post_hours() -> u32 {
    2
}
fn default_ec_high_mult() -> Decimal {
    Decimal::new(25, 2)
} // 0.25
fn default_ec_medium_mult() -> Decimal {
    Decimal::new(50, 2)
} // 0.50
fn default_ec_low_mult() -> Decimal {
    Decimal::new(75, 2)
} // 0.75

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
#[derive(Debug, Deserialize, Serialize, Clone)]
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

/// Named trading account configuration.
///
/// Each account uses a separate private key, proxy wallet, and can run
/// different strategies independently. Accounts share market data but have
/// isolated execution, risk management, and position tracking.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct AccountConfig {
    /// Unique name for this account (used as reference in logs/metrics).
    pub name: String,
    /// Name of the environment variable holding the private key hex string.
    #[serde(default = "default_private_key_env")]
    pub private_key_env: String,
    /// CLOB signature type: 0=EOA, 1=Proxy, 2=GnosisSafe.
    #[serde(default)]
    pub signature_type: u8,
    /// Polymarket proxy wallet address. Leave empty to use the EOA address.
    #[serde(default)]
    pub proxy_wallet: String,
    /// List of strategies this account runs (e.g., ["weather", "crypto", "liquidity_rewards"]).
    /// Must match strategy names in `strategy.enabled` or "liquidity_rewards" for LR.
    #[serde(default)]
    pub strategies: Vec<String>,
}

fn default_private_key_env() -> String {
    "POLYMARKET_PRIVATE_KEY".to_string()
}

/// Configuration for the Liquidity Rewards background task.
///
/// Places GTC limit orders within the rewards spread band on both YES and NO sides
/// to earn Polymarket CLOB liquidity rewards. Automatically discovers markets with
/// active rewards and ranks them by reward density.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct LiquidityRewardsConfig {
    /// Whether liquidity rewards quoting is enabled.
    #[serde(default)]
    pub enabled: bool,
    /// Maximum number of markets to quote simultaneously.
    #[serde(default = "default_lr_max_markets")]
    pub max_markets: usize,
    /// Maximum position per market in USDC.
    #[serde(default = "default_lr_max_position")]
    pub max_position_per_market: Decimal,
    /// Maximum total exposure across all reward markets in USDC.
    #[serde(default = "default_lr_max_total_exposure")]
    pub max_total_exposure: Decimal,
    /// How often to re-select and re-rank reward markets (seconds).
    #[serde(default = "default_lr_market_refresh")]
    pub market_refresh_secs: u64,
    /// How often to cancel-then-replace quotes (seconds).
    #[serde(default = "default_lr_quote_refresh")]
    pub quote_refresh_secs: u64,
    /// Fraction of rewards_max_spread to use (0.0-1.0).
    #[serde(default = "default_lr_spread_fraction")]
    pub spread_fraction: Decimal,
    /// Minimum order size in USDC.
    #[serde(default = "default_lr_min_order_size")]
    pub min_order_size: Decimal,
    /// Inventory skew factor (0.0-1.0). Widens spread on heavy side.
    #[serde(default = "default_lr_skew")]
    pub inventory_skew_factor: Decimal,
    /// Minimum daily reward rate (USDC) to consider a market.
    #[serde(default = "default_lr_min_daily_rate")]
    pub min_daily_rate: Decimal,
    /// Mid-price drift threshold in bps to trigger WS-driven re-quote.
    /// When the midpoint moves more than this from the last quoted mid,
    /// orders for that market are cancelled and immediately re-quoted.
    #[serde(default = "default_lr_requote_trigger")]
    pub requote_trigger_bps: u32,
    /// Per-market cooldown between WS-driven re-quotes (seconds).
    /// Prevents excessive re-quoting from rapid orderbook updates.
    #[serde(default = "default_lr_requote_cooldown")]
    pub requote_cooldown_secs: u64,
    /// Whether to verify order scoring via the CLOB API.
    #[serde(default)]
    pub verify_scoring: bool,
    /// Whether to quote on the YES side.
    #[serde(default = "default_true")]
    pub quote_yes: bool,
    /// Whether to quote on the NO side.
    #[serde(default = "default_true")]
    pub quote_no: bool,
    /// How often to check for filled orders (seconds). 0 = disabled.
    #[serde(default = "default_lr_fill_check")]
    pub fill_check_secs: u64,
    /// Order depth level: place orders at Nth price level in the orderbook.
    /// 0 = use legacy midpoint-based compute_quotes(), N > 0 = use Nth level.
    /// Example: 3 = place bid at buy3 price, ask at sell3 price.
    #[serde(default)]
    pub order_depth_level: usize,
    /// Cancel depth level: cancel and re-quote when order reaches this depth.
    /// Example: 2 = cancel when order is at buy2/sell2 position.
    #[serde(default = "default_lr_cancel_depth")]
    pub cancel_depth_level: usize,
    /// Cooldown (seconds) after a failed order before retrying that (token, side, price).
    #[serde(default = "default_lr_failed_cooldown")]
    pub failed_cooldown_secs: u64,
    /// Market selection mode: "auto" (default), "manual", or "hybrid".
    #[serde(default = "default_lr_market_mode")]
    pub market_mode: String,
    /// Manually managed markets with optional per-market config overrides.
    #[serde(default)]
    pub manual_markets: Vec<LrMarketOverride>,
    /// Whether to allow NegRisk multi-outcome markets for LR quoting.
    #[serde(default)]
    pub allow_neg_risk: bool,
}

fn default_lr_max_markets() -> usize {
    10
}
fn default_lr_max_position() -> Decimal {
    Decimal::from(100)
}
fn default_lr_max_total_exposure() -> Decimal {
    Decimal::from(500)
}
fn default_lr_market_refresh() -> u64 {
    1800
}
fn default_lr_quote_refresh() -> u64 {
    60
}
fn default_lr_requote_trigger() -> u32 {
    30
}
fn default_lr_requote_cooldown() -> u64 {
    3
}
fn default_lr_spread_fraction() -> Decimal {
    Decimal::new(80, 2)
} // 0.80
fn default_lr_min_order_size() -> Decimal {
    Decimal::from(5)
}
fn default_lr_skew() -> Decimal {
    Decimal::new(50, 2)
} // 0.50
fn default_lr_min_daily_rate() -> Decimal {
    Decimal::ONE
}
fn default_lr_fill_check() -> u64 {
    10
}
fn default_lr_cancel_depth() -> usize {
    2
}
fn default_lr_failed_cooldown() -> u64 {
    60
}
fn default_lr_market_mode() -> String {
    "auto".to_string()
}

/// Per-market configuration override for liquidity rewards.
///
/// Allows customizing quoting parameters for specific markets when using
/// manual or hybrid market selection mode.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct LrMarketOverride {
    /// Condition ID hex string (e.g. "0x1234...").
    pub condition_id: String,
    /// Override max position per market (USDC).
    #[serde(default)]
    pub max_position_per_market: Option<Decimal>,
    /// Override spread fraction.
    #[serde(default)]
    pub spread_fraction: Option<Decimal>,
    /// Override whether to quote YES side.
    #[serde(default)]
    pub quote_yes: Option<bool>,
    /// Override whether to quote NO side.
    #[serde(default)]
    pub quote_no: Option<bool>,
    /// Override order depth level.
    #[serde(default)]
    pub order_depth_level: Option<usize>,
}

impl Default for LiquidityRewardsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_markets: default_lr_max_markets(),
            max_position_per_market: default_lr_max_position(),
            max_total_exposure: default_lr_max_total_exposure(),
            market_refresh_secs: default_lr_market_refresh(),
            quote_refresh_secs: default_lr_quote_refresh(),
            spread_fraction: default_lr_spread_fraction(),
            min_order_size: default_lr_min_order_size(),
            inventory_skew_factor: default_lr_skew(),
            min_daily_rate: default_lr_min_daily_rate(),
            requote_trigger_bps: default_lr_requote_trigger(),
            requote_cooldown_secs: default_lr_requote_cooldown(),
            verify_scoring: false,
            quote_yes: true,
            quote_no: true,
            fill_check_secs: default_lr_fill_check(),
            order_depth_level: 0,
            cancel_depth_level: default_lr_cancel_depth(),
            failed_cooldown_secs: default_lr_failed_cooldown(),
            market_mode: default_lr_market_mode(),
            manual_markets: vec![],
            allow_neg_risk: false,
        }
    }
}

/// Configuration for the SmartMoney copy-trading strategy.
///
/// Monitors high-PnL wallets on Polymarket and follows their position changes
/// proportionally. Supports Data API polling and optional on-chain Transfer event
/// monitoring for real-time detection.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct SmartMoneyConfig {
    /// Manually configured wallets to track.
    #[serde(default)]
    pub wallets: Vec<TrackedWalletConfig>,
    /// Wallets that should never be tracked or auto-discovered.
    #[serde(default)]
    pub blocked_wallets: Vec<String>,
    /// Per-wallet weight multipliers used to degrade noisy leaders without fully blocking them.
    #[serde(default)]
    pub degraded_wallets: Vec<DegradedWalletConfig>,
    /// Optional per-wallet market routing rules used to limit which markets a leader can trigger.
    #[serde(default)]
    pub leader_routes: Vec<SmartMoneyLeaderRouteConfig>,
    /// Fraction of tracked wallet's position to follow (0.0-1.0).
    #[serde(default = "default_sm_follow_ratio")]
    pub follow_ratio: Decimal,
    /// Maximum position per market in USDC.
    #[serde(default = "default_sm_max_position")]
    pub max_position_usdc: Decimal,
    /// Data API poll interval (seconds).
    #[serde(default = "default_sm_poll_interval")]
    pub poll_interval_secs: u64,
    /// Signal time-to-live (seconds). Stale signals are discarded.
    #[serde(default = "default_sm_signal_ttl")]
    pub signal_ttl_secs: u64,
    /// Exit buffer in bps.
    #[serde(default = "default_exit_buffer_bps")]
    pub exit_buffer_bps: u32,
    /// Capital efficiency threshold. Auto-sell when best_bid >= this value.
    #[serde(default = "default_capital_efficiency_threshold")]
    pub capital_efficiency_threshold: Decimal,
    /// Enable on-chain Transfer event monitoring for real-time detection.
    #[serde(default)]
    pub onchain_enabled: bool,
    /// On-chain log poll interval (seconds). Only used if onchain_enabled=true.
    #[serde(default = "default_sm_onchain_poll")]
    pub onchain_poll_secs: u64,
    /// Enable auto-discovery of high-PnL wallets from candidate list.
    #[serde(default)]
    pub auto_discover_enabled: bool,
    /// Candidate wallet addresses for auto-discovery scoring.
    #[serde(default)]
    pub auto_discover_candidates: Vec<String>,
    /// Auto-discovery re-evaluation interval (seconds).
    #[serde(default = "default_sm_discover_interval")]
    pub auto_discover_interval_secs: u64,
    /// Minimum P&L-to-volume ratio to auto-track a wallet.
    #[serde(default = "default_sm_min_score")]
    pub min_wallet_score: Decimal,
    /// Minimum lifetime volume required before a wallet's profile score is trusted.
    #[serde(default = "default_sm_min_wallet_volume_usdc")]
    pub min_wallet_volume_usdc: Decimal,
    /// Maximum number of wallets to track (manual + auto-discovered).
    #[serde(default = "default_sm_max_wallets")]
    pub max_wallets: usize,
    /// How strongly profile score changes should alter the configured base wallet weight.
    #[serde(default = "default_sm_wallet_profile_blend")]
    pub wallet_profile_blend: Decimal,
    /// Bonus added to effective weight for each recent actionable signal.
    #[serde(default = "default_sm_wallet_signal_bonus_per_event")]
    pub wallet_signal_bonus_per_event: Decimal,
    /// Maximum total recency bonus added to a wallet's effective weight multiplier.
    #[serde(default = "default_sm_wallet_signal_bonus_cap")]
    pub wallet_signal_bonus_cap: Decimal,
    /// Flat decay step applied when a tracked wallet underperforms its minimum score.
    #[serde(default = "default_sm_wallet_underperform_decay_step")]
    pub wallet_underperform_decay_step: Decimal,
    /// Lower clamp for effective wallet weights after dynamic scoring.
    #[serde(default = "default_sm_wallet_min_effective_weight")]
    pub wallet_min_effective_weight: Decimal,
    /// Upper clamp for effective wallet weights after dynamic scoring.
    #[serde(default = "default_sm_wallet_max_effective_weight")]
    pub wallet_max_effective_weight: Decimal,
    /// Lookback window used to count recent wallet signal activity for recency bonuses.
    #[serde(default = "default_sm_wallet_signal_lookback_secs")]
    pub wallet_signal_lookback_secs: u64,
    /// Minimum signal notional in USDC before a wallet move is considered actionable.
    #[serde(default = "default_sm_min_signal_notional_usdc")]
    pub min_signal_notional_usdc: Decimal,
    /// Minimum share delta before a wallet move is considered actionable.
    #[serde(default = "default_sm_min_signal_delta_shares")]
    pub min_signal_delta_shares: Decimal,
    /// Minimum configured wallet weight required for a signal to be considered.
    #[serde(default = "default_sm_min_wallet_weight")]
    pub min_wallet_weight: Decimal,
    /// Minimum number of wallets agreeing on the same token to allow an entry.
    #[serde(default = "default_sm_min_consensus_wallets")]
    pub min_consensus_wallets: usize,
    /// Maximum acceptable signal age for following an entry.
    #[serde(default = "default_sm_max_signal_age_secs")]
    pub max_signal_age_secs: u64,
    /// Maximum entry price when following a leader into a market.
    #[serde(default = "default_sm_max_entry_price")]
    pub max_entry_price: Decimal,
    /// Maximum best-bid / best-ask spread in bps for a follow entry.
    #[serde(default = "default_sm_max_spread_bps")]
    pub max_spread_bps: u32,
    /// Minimum notional resting on the best ask before we follow.
    #[serde(default = "default_sm_min_top_level_depth_usdc")]
    pub min_top_level_depth_usdc: Decimal,
    /// Minimum market liquidity required before we follow.
    #[serde(default = "default_sm_min_market_liquidity")]
    pub min_market_liquidity: Decimal,
    /// Whether on-chain transfer signals must be confirmed by the next Data API snapshot.
    #[serde(default = "default_sm_confirm_onchain_with_data_api")]
    pub confirm_onchain_with_data_api: bool,
    /// Window used to suppress duplicate same-wallet same-token signals.
    #[serde(default = "default_sm_dedup_window_secs")]
    pub dedup_window_secs: u64,
    /// Additional sizing bonus applied per agreeing wallet beyond the first.
    #[serde(default = "default_sm_consensus_bonus_per_wallet")]
    pub consensus_bonus_per_wallet: Decimal,
    /// Cap on the total consensus sizing bonus.
    #[serde(default = "default_sm_consensus_bonus_cap")]
    pub consensus_bonus_cap: Decimal,
    /// Half-life for sizing decay as signals age.
    #[serde(default = "default_sm_freshness_half_life_secs")]
    pub freshness_half_life_secs: u64,
    /// Floor applied to leader delta-ratio sizing so small adds do not vanish completely.
    #[serde(default = "default_sm_leader_delta_ratio_floor")]
    pub leader_delta_ratio_floor: Decimal,
    /// Existing position notional above which new smart-money entries are scaled down.
    #[serde(default = "default_sm_position_concentration_soft_cap_usdc")]
    pub position_concentration_soft_cap_usdc: Decimal,
    /// Lower bound for concentration-based sizing penalties.
    #[serde(default = "default_sm_position_concentration_min_multiplier")]
    pub position_concentration_min_multiplier: Decimal,
    /// Minimum leader delta ratio required before a partial decrease triggers a follow exit.
    #[serde(default = "default_sm_leader_exit_min_delta_ratio")]
    pub leader_exit_min_delta_ratio: Decimal,
    /// Maximum time to keep a smart-money position without a fresh exit trigger.
    #[serde(default = "default_sm_max_hold_secs")]
    pub max_hold_secs: u64,
    /// Minimum profit in bps above average cost before profit-protect exits activate.
    #[serde(default = "default_sm_profit_protect_min_gain_bps")]
    pub profit_protect_min_gain_bps: u32,
    /// Allowed drawdown from peak bid, in bps, once profit protection is active.
    #[serde(default = "default_sm_profit_protect_drawdown_bps")]
    pub profit_protect_drawdown_bps: u32,
    /// Maximum tolerated drawdown from average cost before forcing an exit.
    #[serde(default = "default_sm_max_drawdown_bps")]
    pub max_drawdown_bps: u32,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TrackedWalletConfig {
    pub address: String,
    #[serde(default)]
    pub label: String,
    #[serde(default = "default_sm_wallet_weight")]
    pub weight: Decimal,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DegradedWalletConfig {
    pub address: String,
    #[serde(default = "default_sm_degrade_multiplier")]
    pub multiplier: Decimal,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SmartMoneyLeaderRouteConfig {
    pub address: String,
    #[serde(default)]
    pub categories: Vec<String>,
    #[serde(default)]
    pub question_keywords: Vec<String>,
    #[serde(default)]
    pub event_title_keywords: Vec<String>,
}

fn default_sm_follow_ratio() -> Decimal {
    Decimal::new(10, 2)
} // 0.10
fn default_sm_degrade_multiplier() -> Decimal {
    Decimal::new(50, 2)
} // 0.50
fn default_sm_max_position() -> Decimal {
    Decimal::from(100)
}
fn default_sm_poll_interval() -> u64 {
    30
}
fn default_sm_signal_ttl() -> u64 {
    300
}
fn default_sm_onchain_poll() -> u64 {
    4
}
fn default_sm_discover_interval() -> u64 {
    3600
}
fn default_sm_min_score() -> Decimal {
    Decimal::new(5, 2)
} // 0.05
fn default_sm_min_wallet_volume_usdc() -> Decimal {
    Decimal::from(250)
}
fn default_sm_max_wallets() -> usize {
    20
}
fn default_sm_wallet_weight() -> Decimal {
    Decimal::ONE
}
fn default_sm_wallet_profile_blend() -> Decimal {
    Decimal::new(5, 1)
} // 0.5
fn default_sm_wallet_signal_bonus_per_event() -> Decimal {
    Decimal::new(5, 2)
} // 0.05
fn default_sm_wallet_signal_bonus_cap() -> Decimal {
    Decimal::new(25, 2)
} // 0.25
fn default_sm_wallet_underperform_decay_step() -> Decimal {
    Decimal::new(10, 2)
} // 0.10
fn default_sm_wallet_min_effective_weight() -> Decimal {
    Decimal::new(25, 2)
} // 0.25
fn default_sm_wallet_max_effective_weight() -> Decimal {
    Decimal::new(15, 1)
} // 1.5
fn default_sm_wallet_signal_lookback_secs() -> u64 {
    86_400
}
fn default_sm_min_signal_notional_usdc() -> Decimal {
    Decimal::from(25)
}
fn default_sm_min_signal_delta_shares() -> Decimal {
    Decimal::from(20)
}
fn default_sm_min_wallet_weight() -> Decimal {
    Decimal::new(5, 1)
} // 0.5
fn default_sm_min_consensus_wallets() -> usize {
    1
}
fn default_sm_max_signal_age_secs() -> u64 {
    90
}
fn default_sm_max_entry_price() -> Decimal {
    Decimal::new(85, 2)
} // 0.85
fn default_sm_max_spread_bps() -> u32 {
    300
}
fn default_sm_min_top_level_depth_usdc() -> Decimal {
    Decimal::from(50)
}
fn default_sm_min_market_liquidity() -> Decimal {
    Decimal::from(500)
}
fn default_sm_confirm_onchain_with_data_api() -> bool {
    true
}
fn default_sm_dedup_window_secs() -> u64 {
    45
}
fn default_sm_consensus_bonus_per_wallet() -> Decimal {
    Decimal::new(10, 2)
} // 0.10
fn default_sm_consensus_bonus_cap() -> Decimal {
    Decimal::new(30, 2)
} // 0.30
fn default_sm_freshness_half_life_secs() -> u64 {
    45
}
fn default_sm_leader_delta_ratio_floor() -> Decimal {
    Decimal::new(20, 2)
} // 0.20
fn default_sm_position_concentration_soft_cap_usdc() -> Decimal {
    Decimal::from(60)
}
fn default_sm_position_concentration_min_multiplier() -> Decimal {
    Decimal::new(25, 2)
} // 0.25
fn default_sm_leader_exit_min_delta_ratio() -> Decimal {
    Decimal::new(25, 2)
} // 0.25
fn default_sm_max_hold_secs() -> u64 {
    21_600
}
fn default_sm_profit_protect_min_gain_bps() -> u32 {
    800
}
fn default_sm_profit_protect_drawdown_bps() -> u32 {
    500
}
fn default_sm_max_drawdown_bps() -> u32 {
    1200
}

impl Default for SmartMoneyConfig {
    fn default() -> Self {
        Self {
            wallets: vec![],
            blocked_wallets: vec![],
            degraded_wallets: vec![],
            leader_routes: vec![],
            follow_ratio: default_sm_follow_ratio(),
            max_position_usdc: default_sm_max_position(),
            poll_interval_secs: default_sm_poll_interval(),
            signal_ttl_secs: default_sm_signal_ttl(),
            exit_buffer_bps: default_exit_buffer_bps(),
            capital_efficiency_threshold: default_capital_efficiency_threshold(),
            onchain_enabled: false,
            onchain_poll_secs: default_sm_onchain_poll(),
            auto_discover_enabled: false,
            auto_discover_candidates: vec![],
            auto_discover_interval_secs: default_sm_discover_interval(),
            min_wallet_score: default_sm_min_score(),
            min_wallet_volume_usdc: default_sm_min_wallet_volume_usdc(),
            max_wallets: default_sm_max_wallets(),
            wallet_profile_blend: default_sm_wallet_profile_blend(),
            wallet_signal_bonus_per_event: default_sm_wallet_signal_bonus_per_event(),
            wallet_signal_bonus_cap: default_sm_wallet_signal_bonus_cap(),
            wallet_underperform_decay_step: default_sm_wallet_underperform_decay_step(),
            wallet_min_effective_weight: default_sm_wallet_min_effective_weight(),
            wallet_max_effective_weight: default_sm_wallet_max_effective_weight(),
            wallet_signal_lookback_secs: default_sm_wallet_signal_lookback_secs(),
            min_signal_notional_usdc: default_sm_min_signal_notional_usdc(),
            min_signal_delta_shares: default_sm_min_signal_delta_shares(),
            min_wallet_weight: default_sm_min_wallet_weight(),
            min_consensus_wallets: default_sm_min_consensus_wallets(),
            max_signal_age_secs: default_sm_max_signal_age_secs(),
            max_entry_price: default_sm_max_entry_price(),
            max_spread_bps: default_sm_max_spread_bps(),
            min_top_level_depth_usdc: default_sm_min_top_level_depth_usdc(),
            min_market_liquidity: default_sm_min_market_liquidity(),
            confirm_onchain_with_data_api: default_sm_confirm_onchain_with_data_api(),
            dedup_window_secs: default_sm_dedup_window_secs(),
            consensus_bonus_per_wallet: default_sm_consensus_bonus_per_wallet(),
            consensus_bonus_cap: default_sm_consensus_bonus_cap(),
            freshness_half_life_secs: default_sm_freshness_half_life_secs(),
            leader_delta_ratio_floor: default_sm_leader_delta_ratio_floor(),
            position_concentration_soft_cap_usdc: default_sm_position_concentration_soft_cap_usdc(),
            position_concentration_min_multiplier: default_sm_position_concentration_min_multiplier(
            ),
            leader_exit_min_delta_ratio: default_sm_leader_exit_min_delta_ratio(),
            max_hold_secs: default_sm_max_hold_secs(),
            profit_protect_min_gain_bps: default_sm_profit_protect_min_gain_bps(),
            profit_protect_drawdown_bps: default_sm_profit_protect_drawdown_bps(),
            max_drawdown_bps: default_sm_max_drawdown_bps(),
        }
    }
}

impl Settings {
    /// Return a copy with sensitive fields redacted for API responses.
    pub fn redacted(&self) -> Self {
        let mut s = self.clone();
        s.chain.rpc_url = "***".into();
        s.chain.rpc_fallbacks = vec!["***".into()];
        s.database.url = "***".into();
        s.clob.proxy_wallet = "***".into();
        s.crypto_alpha.coingecko_api_key = "***".into();
        s.event_calendar.finnhub_api_key = "***".into();
        s.event_calendar.coinmarketcal_api_key = "***".into();
        s.accounts = vec![];
        s
    }

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

    /// Re-apply `PA_` environment variable overrides onto an existing Settings value.
    ///
    /// This is needed when other layers (e.g. DB-backed UI overrides) are applied
    /// after `Settings::load()`, because runtime semantics require env vars to stay
    /// the highest-priority source.
    pub fn reapply_env_overrides(&mut self) -> crate::Result<()> {
        let env_cfg = config::Config::builder()
            .add_source(
                config::Environment::with_prefix("PA")
                    .separator("__")
                    .try_parsing(true)
                    .convert_case(config::Case::Snake),
            )
            .build()?;

        let env_value: Value = env_cfg.try_deserialize()?;
        if env_value.is_null() {
            return Ok(());
        }

        let mut base_value = serde_json::to_value(&*self)
            .map_err(|e| config::ConfigError::Message(format!("serialize settings: {e}")))?;
        merge_json_value(&mut base_value, env_value);
        *self = serde_json::from_value(base_value)
            .map_err(|e| config::ConfigError::Message(format!("deserialize settings: {e}")))?;
        self.apply_direct_env_backfills();
        Ok(())
    }

    /// Backfill selected settings directly from environment variables.
    ///
    /// Some high-value fields are cheap to read explicitly and should not depend
    /// solely on `config`'s nested env deserialization behavior.
    pub fn apply_direct_env_backfills(&mut self) {
        if let Ok(url) = std::env::var("PA_DATABASE__URL")
            && !url.trim().is_empty()
        {
            self.database.url = url;
        }
        if let Ok(key) = std::env::var("PA_WEATHER__KMA_API_KEY")
            && !key.trim().is_empty()
        {
            self.weather.kma_api_key = key;
        }
        if let Ok(key) = std::env::var("PA_WEATHER__MET_OFFICE_API_KEY")
            && !key.trim().is_empty()
        {
            self.weather.met_office_api_key = key;
        }
        if let Ok(key) = std::env::var("PA_WEATHER__MET_OFFICE_OBS_API_KEY")
            && !key.trim().is_empty()
        {
            self.weather.met_office_obs_api_key = key;
        }
    }

    /// Merge strategies referenced by configured accounts into the global enabled list.
    ///
    /// This keeps discovery/status views aligned with the strategies that can actually
    /// run on at least one configured account.
    pub fn merge_account_strategies_into_enabled(&mut self) {
        for account in self.resolved_accounts() {
            for strategy in account.strategies {
                if !self.strategy.enabled.contains(&strategy) {
                    self.strategy.enabled.push(strategy);
                }
            }
        }
    }

    /// Strategies that are both globally enabled and assigned to at least one account.
    ///
    /// Used by discovery/subscription code so the process doesn't scan markets for
    /// strategies that no configured account can actually execute.
    pub fn active_account_enabled_strategies(&self) -> Vec<String> {
        let accounts = self.resolved_accounts();
        if accounts.is_empty() {
            return self.strategy.enabled.clone();
        }

        let mut active = Vec::new();
        for strategy in &self.strategy.enabled {
            if accounts.iter().any(|account| {
                account
                    .strategies
                    .iter()
                    .any(|assigned| assigned == strategy)
            }) && !active.contains(strategy)
            {
                active.push(strategy.clone());
            }
        }

        if self.liquidity_rewards.enabled
            && accounts.iter().any(|account| {
                account
                    .strategies
                    .iter()
                    .any(|strategy| strategy == "liquidity_rewards")
            })
            && !active
                .iter()
                .any(|strategy| strategy == "liquidity_rewards")
        {
            active.push("liquidity_rewards".to_string());
        }

        active
    }

    /// Resolve the effective list of accounts.
    ///
    /// Priority (highest to lowest):
    /// 1. Environment variables `PA_ACCOUNT_<N>_*` (e.g. `PA_ACCOUNT_1_NAME=main`)
    /// 2. TOML `[[accounts]]` sections
    ///
    /// Environment variable format:
    /// ```text
    /// PA_ACCOUNT_1_NAME=main
    /// PA_ACCOUNT_1_PRIVATE_KEY_ENV=POLYMARKET_PRIVATE_KEY
    /// PA_ACCOUNT_1_SIGNATURE_TYPE=2
    /// PA_ACCOUNT_1_PROXY_WALLET=0x...
    /// PA_ACCOUNT_1_STRATEGIES=weather,crypto
    ///
    /// PA_ACCOUNT_2_NAME=lr_bot
    /// PA_ACCOUNT_2_PRIVATE_KEY_ENV=POLYMARKET_PRIVATE_KEY_2
    /// PA_ACCOUNT_2_SIGNATURE_TYPE=2
    /// PA_ACCOUNT_2_PROXY_WALLET=0x...
    /// PA_ACCOUNT_2_STRATEGIES=liquidity_rewards
    /// ```
    pub fn resolved_accounts(&self) -> Vec<AccountConfig> {
        // Priority 1: environment variables PA_ACCOUNT_<N>_*
        let env_accounts = Self::parse_env_accounts();
        if !env_accounts.is_empty() {
            return env_accounts;
        }

        // Priority 2: TOML [[accounts]] sections
        if !self.accounts.is_empty() {
            return self.accounts.clone();
        }

        Vec::new()
    }

    /// Parse accounts from `PA_ACCOUNT_<N>_*` environment variables.
    ///
    /// Scans for sequential indices starting at 1. Stops at the first missing
    /// `PA_ACCOUNT_<N>_NAME`. Only `NAME` is required; other fields have defaults.
    fn parse_env_accounts() -> Vec<AccountConfig> {
        let mut accounts = Vec::new();
        for idx in 1..=100 {
            let prefix = format!("PA_ACCOUNT_{idx}_");
            let name = match std::env::var(format!("{prefix}NAME")) {
                Ok(n) if !n.is_empty() => n,
                _ => break,
            };

            let private_key_env = std::env::var(format!("{prefix}PRIVATE_KEY_ENV"))
                .unwrap_or_else(|_| "POLYMARKET_PRIVATE_KEY".to_string());
            let signature_type = std::env::var(format!("{prefix}SIGNATURE_TYPE"))
                .ok()
                .and_then(|s| s.parse::<u8>().ok())
                .unwrap_or(0);
            let proxy_wallet = std::env::var(format!("{prefix}PROXY_WALLET")).unwrap_or_default();
            let strategies = std::env::var(format!("{prefix}STRATEGIES"))
                .unwrap_or_default()
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();

            accounts.push(AccountConfig {
                name,
                private_key_env,
                signature_type,
                proxy_wallet,
                strategies,
            });
        }
        accounts
    }
}

fn merge_json_value(base: &mut Value, overlay: Value) {
    match (base, overlay) {
        (Value::Object(base_map), Value::Object(overlay_map)) => {
            for (key, overlay_value) in overlay_map {
                match base_map.get_mut(&key) {
                    Some(base_value) => merge_json_value(base_value, overlay_value),
                    None => {
                        base_map.insert(key, overlay_value);
                    }
                }
            }
        }
        (base_slot, overlay_value) => {
            *base_slot = overlay_value;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    fn env_mutex() -> &'static Mutex<()> {
        static ENV_MUTEX: OnceLock<Mutex<()>> = OnceLock::new();
        ENV_MUTEX.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn test_weather_api_key_env_backfills() {
        let _guard = env_mutex().lock().unwrap();

        unsafe {
            std::env::set_var("PA_WEATHER__KMA_API_KEY", "kma-test-key");
            std::env::set_var("PA_WEATHER__MET_OFFICE_API_KEY", "met-forecast-test-key");
            std::env::set_var("PA_WEATHER__MET_OFFICE_OBS_API_KEY", "met-obs-test-key");
        }

        let mut settings = Settings {
            chain: ChainConfig {
                chain_id: 137,
                rpc_url: "https://polygon-rpc.com".to_string(),
                rpc_fallbacks: Vec::new(),
            },
            clob: ClobConfig {
                host: "https://clob.polymarket.com".to_string(),
                ws_host: "wss://ws-subscriptions-clob.polymarket.com/ws/market".to_string(),
                signature_type: 2,
                proxy_wallet: String::new(),
            },
            gamma: GammaConfig {
                host: "https://gamma-api.polymarket.com".to_string(),
            },
            strategy: StrategyConfig {
                enabled: vec!["weather".to_string()],
                scan_interval_ms: 100,
                min_spread_bps: 100,
                min_profit_usdc: Decimal::new(20, 2),
                max_trade_size_usdc: Decimal::ONE,
                order_type: "limit".to_string(),
                max_market_end_days: None,
            },
            risk: RiskConfig {
                max_position_per_market: Decimal::ONE,
                max_total_exposure: Decimal::ONE,
                max_daily_loss: Decimal::ONE,
                circuit_breaker_loss: Decimal::ONE,
                circuit_breaker_consecutive_losses: 3,
                max_slippage_bps: 100,
                min_profit_retention_ratio: default_min_profit_retention_ratio(),
                min_size_retention_ratio: default_min_size_retention_ratio(),
                execution_quality_profit_weight: default_execution_quality_profit_weight(),
                execution_quality_size_weight: default_execution_quality_size_weight(),
                execution_quality_slippage_weight: default_execution_quality_slippage_weight(),
                min_order_usdc: default_min_order_usdc(),
                min_profit_usdc: default_min_profit_usdc(),
                max_exposure_per_strategy: default_max_exposure_per_strategy(),
                max_markets_per_strategy: default_max_markets_per_strategy(),
            },
            database: DatabaseConfig::default(),
            monitor: MonitorConfig {
                prometheus_port: 9090,
                health_port: 18381,
                alert_webhook: String::new(),
            },
            market_filter: MarketFilterConfig {
                min_liquidity: Decimal::ZERO,
                min_volume_24h: Decimal::ZERO,
                max_markets: 100,
                ws_max_instruments: 350,
                market_refresh_interval_secs: default_market_refresh_interval(),
            },
            weather: WeatherConfig::default(),
            crypto_alpha: CryptoAlphaConfig::default(),
            event_calendar: EventCalendarConfig::default(),
            liquidity_rewards: LiquidityRewardsConfig::default(),
            smart_money: SmartMoneyConfig::default(),
            accounts: Vec::new(),
        };
        settings.apply_direct_env_backfills();

        assert_eq!(settings.weather.kma_api_key, "kma-test-key");
        assert_eq!(settings.weather.met_office_api_key, "met-forecast-test-key");
        assert_eq!(settings.weather.met_office_obs_api_key, "met-obs-test-key");

        unsafe {
            std::env::remove_var("PA_WEATHER__KMA_API_KEY");
            std::env::remove_var("PA_WEATHER__MET_OFFICE_API_KEY");
            std::env::remove_var("PA_WEATHER__MET_OFFICE_OBS_API_KEY");
        }
    }
}
