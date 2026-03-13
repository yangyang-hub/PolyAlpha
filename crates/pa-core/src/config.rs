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
    /// Profit-take threshold — sell when best_bid rises above this price.
    #[serde(default = "default_profit_take_threshold")]
    pub profit_take_threshold: Decimal,
    /// Maximum position size in USDC per trade.
    #[serde(default = "default_weather_max_position_usdc")]
    pub max_position_usdc: Decimal,
    /// NOAA API requires a User-Agent header identifying the application.
    #[serde(default = "default_noaa_user_agent")]
    pub noaa_user_agent: String,
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
    Decimal::new(35, 2)
} // 0.35
fn default_profit_take_threshold() -> Decimal {
    Decimal::new(34, 2)
} // 0.34
fn default_weather_max_position_usdc() -> Decimal {
    Decimal::new(5, 0)
} // $5
fn default_noaa_user_agent() -> String {
    "PolyAlpha/1.0".to_string()
}
fn default_target_cities() -> Vec<String> {
    vec![]
}

impl Default for WeatherConfig {
    fn default() -> Self {
        Self {
            min_edge_bps: 500,
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
            profit_take_threshold: default_profit_take_threshold(),
            max_position_usdc: default_weather_max_position_usdc(),
            noaa_user_agent: default_noaa_user_agent(),
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
    /// Price data refresh interval (seconds).
    #[serde(default = "default_crypto_refresh")]
    pub refresh_interval_secs: u64,
    /// CoinGecko Demo API key (empty = fallback disabled).
    #[serde(default)]
    pub coingecko_api_key: String,
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

impl Default for CryptoAlphaConfig {
    fn default() -> Self {
        Self {
            min_edge_bps: default_crypto_min_edge(),
            max_position_pct: default_crypto_max_position_pct(),
            kelly_fraction: default_crypto_kelly(),
            refresh_interval_secs: default_crypto_refresh(),
            coingecko_api_key: String::new(),
            exit_buffer_bps: default_exit_buffer_bps(),
            capital_efficiency_threshold: default_capital_efficiency_threshold(),
            drift_decay: default_drift_decay(),
            max_spread_bps: default_crypto_max_spread_bps(),
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
    /// Maximum number of wallets to track (manual + auto-discovered).
    #[serde(default = "default_sm_max_wallets")]
    pub max_wallets: usize,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TrackedWalletConfig {
    pub address: String,
    #[serde(default)]
    pub label: String,
    #[serde(default = "default_sm_wallet_weight")]
    pub weight: Decimal,
}

fn default_sm_follow_ratio() -> Decimal {
    Decimal::new(10, 2)
} // 0.10
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
fn default_sm_max_wallets() -> usize {
    20
}
fn default_sm_wallet_weight() -> Decimal {
    Decimal::ONE
}

impl Default for SmartMoneyConfig {
    fn default() -> Self {
        Self {
            wallets: vec![],
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
            max_wallets: default_sm_max_wallets(),
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
        Ok(())
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
            if accounts
                .iter()
                .any(|account| account.strategies.iter().any(|assigned| assigned == strategy))
                && !active.contains(strategy)
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
            && !active.iter().any(|strategy| strategy == "liquidity_rewards")
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
