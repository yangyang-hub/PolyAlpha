use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use arc_swap::ArcSwap;
use axum::extract::{Path, Query, State};
use axum::response::Json;
use axum::{Router, routing::get};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tower_http::cors::CorsLayer;
use tower_http::services::{ServeDir, ServeFile};

use pa_core::config::Settings;

type HealthCheck = Box<dyn Fn() -> bool + Send + Sync>;

/// Runtime status of a single LR-quoted market.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LrMarketStatus {
    pub condition_id: String,
    pub question: String,
    pub daily_rate: Decimal,
    pub outstanding_orders: usize,
    pub yes_bid: Option<Decimal>,
    pub yes_ask: Option<Decimal>,
    pub no_bid: Option<Decimal>,
    pub no_ask: Option<Decimal>,
}

/// LR task runtime status, written by the LR background task and read by the API.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LrRuntimeStatus {
    pub active_markets: Vec<LrMarketStatus>,
    pub total_exposure: Decimal,
    pub cached_balance: Decimal,
    pub market_mode: String,
    pub last_refresh: Option<DateTime<Utc>>,
}

/// A single position entry for the API response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PositionApiEntry {
    pub token_id: String,
    pub size: Decimal,
    pub avg_cost: Decimal,
    pub cost_basis: Decimal,
    pub strategy: Option<String>,
    pub condition_id: Option<String>,
    pub question: Option<String>,
    pub outcome: Option<String>,
    pub current_price: Option<Decimal>,
    pub unrealized_pnl: Option<Decimal>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountStatusEntry {
    pub name: String,
    pub strategies: Vec<String>,
    pub proxy_wallet: String,
    pub private_key_env: String,
    pub private_key_present: bool,
}

/// Shared API state for all Axum handlers.
pub struct ApiState {
    pub config: Arc<ArcSwap<Settings>>,
    pub start_time: DateTime<Utc>,
    pub health_checks: Vec<(&'static str, HealthCheck)>,
    /// Optional LR runtime status, populated by the LR background task.
    pub lr_status: Option<Arc<tokio::sync::RwLock<LrRuntimeStatus>>>,
    /// Live positions, populated after account init and updated by position sync.
    pub positions: Arc<tokio::sync::RwLock<Vec<PositionApiEntry>>>,
    /// Timestamp of the last positions snapshot refresh.
    pub positions_updated_at: Arc<tokio::sync::RwLock<Option<DateTime<Utc>>>>,
    /// True once startup has completed enough for the bot to be considered ready.
    pub startup_ready: Arc<AtomicBool>,
}

/// Build the full Axum router with health, metrics, config API, and SPA fallback.
pub fn build_router(state: Arc<ApiState>) -> Router {
    let api_routes = Router::new()
        .route("/api/config", get(get_all_config))
        .route("/api/config/meta/{section}", get(get_section_meta))
        .route("/api/config/{section}", get(get_section))
        .route("/api/status", get(get_status))
        .route("/api/positions", get(get_positions))
        .route("/api/lr/status", get(get_lr_status));

    Router::new()
        .route("/health", get(health_handler))
        .route("/ready", get(readiness_handler))
        .route("/metrics", get(metrics_handler))
        .merge(api_routes)
        .fallback_service(
            ServeDir::new("frontend/dist").fallback(ServeFile::new("frontend/dist/index.html")),
        )
        .layer(CorsLayer::permissive())
        .with_state(state)
}

/// Start the HTTP server using the new ApiState-based router.
pub async fn start_server(health_port: u16, state: Arc<ApiState>) -> anyhow::Result<()> {
    let app = build_router(state);

    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], health_port));
    tracing::info!(%addr, "Starting health/metrics/config API server");

    let socket = socket2::Socket::new(
        socket2::Domain::IPV4,
        socket2::Type::STREAM,
        Some(socket2::Protocol::TCP),
    )?;
    socket.set_reuse_address(true)?;
    socket.set_nonblocking(true)?;
    socket.bind(&addr.into())?;
    socket.listen(1024)?;
    let listener = tokio::net::TcpListener::from_std(socket.into())?;
    axum::serve(listener, app).await?;
    Ok(())
}

// --- Health / Ready / Metrics handlers (migrated from health.rs) ---

async fn health_handler(State(state): State<Arc<ApiState>>) -> Json<Value> {
    let uptime_secs = (Utc::now() - state.start_time).num_seconds();
    let mut checks = serde_json::Map::new();
    let mut all_ok = true;

    for (name, check) in &state.health_checks {
        let ok = check();
        if !ok {
            all_ok = false;
        }
        checks.insert(name.to_string(), json!(if ok { "ok" } else { "error" }));
    }

    let status = if all_ok { "healthy" } else { "degraded" };
    Json(json!({
        "status": status,
        "service": "polyalpha",
        "uptime_seconds": uptime_secs,
        "checks": checks,
    }))
}

async fn readiness_handler(
    State(state): State<Arc<ApiState>>,
) -> (axum::http::StatusCode, Json<Value>) {
    let all_ok = state.health_checks.iter().all(|(_, check)| check());
    let startup_ready = state.startup_ready.load(std::sync::atomic::Ordering::Relaxed);
    if all_ok && startup_ready {
        (axum::http::StatusCode::OK, Json(json!({"ready": true})))
    } else {
        (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"ready": false, "startup_ready": startup_ready})),
        )
    }
}

async fn metrics_handler() -> String {
    use prometheus::Encoder;
    let encoder = prometheus::TextEncoder::new();
    let metric_families = crate::metrics::REGISTRY.gather();
    let mut buffer = Vec::new();
    encoder.encode(&metric_families, &mut buffer).unwrap();
    String::from_utf8(buffer).unwrap_or_default()
}

// --- Config API handlers ---

/// GET /api/config — full redacted settings JSON.
async fn get_all_config(State(state): State<Arc<ApiState>>) -> Json<Value> {
    let settings = state.config.load();
    let redacted = settings.redacted();
    Json(serde_json::to_value(&redacted).unwrap_or(json!({})))
}

/// GET /api/config/:section — single section JSON.
async fn get_section(
    State(state): State<Arc<ApiState>>,
    Path(section): Path<String>,
) -> (axum::http::StatusCode, Json<Value>) {
    let settings = state.config.load();
    let redacted = settings.redacted();
    match extract_section(&redacted, &section) {
        Ok(val) => (axum::http::StatusCode::OK, Json(val)),
        Err(_) => (
            axum::http::StatusCode::NOT_FOUND,
            Json(json!({"error": format!("Unknown section: {}", section)})),
        ),
    }
}

/// GET /api/config/meta/:section — UI metadata for a config section.
async fn get_section_meta(Path(section): Path<String>) -> (axum::http::StatusCode, Json<Value>) {
    match section.as_str() {
        "weather" => {
            let target_cities = pa_core::weather::noaa_supported_location_names();
            let all_weather_cities = pa_core::weather::weather_supported_location_names();
            let city_risk_tiers: std::collections::HashMap<&str, &str> = all_weather_cities
                .iter()
                .map(|city| {
                    let tier = match pa_core::weather::settlement_risk_tier(city) {
                        pa_core::weather::SettlementRiskTier::Low => "low",
                        pa_core::weather::SettlementRiskTier::Medium => "medium",
                        pa_core::weather::SettlementRiskTier::High => "high",
                    };
                    (*city, tier)
                })
                .collect();
            let city_providers: std::collections::HashMap<&str, &str> = all_weather_cities
                .iter()
                .filter_map(|city| {
                    let location = pa_core::weather::weather_location(city)?;
                    let provider = match location.provider {
                        pa_core::weather::WeatherProvider::Noaa => "noaa",
                        pa_core::weather::WeatherProvider::OpenMeteo => "open_meteo",
                    };
                    Some((*city, provider))
                })
                .collect();
            let city_trade_enabled: std::collections::HashMap<&str, bool> = all_weather_cities
                .iter()
                .filter_map(|city| {
                    let location = pa_core::weather::weather_location(city)?;
                    Some((*city, location.trade_enabled))
                })
                .collect();
            let city_settlement_notes: std::collections::HashMap<&str, &str> = all_weather_cities
                .iter()
                .filter_map(|city| {
                    let location = pa_core::weather::weather_location(city)?;
                    let note = location.settlement_note?;
                    Some((*city, note))
                })
                .collect();

            (
                axum::http::StatusCode::OK,
                Json(json!({
                    "target_cities_options": target_cities,
                    "supported_cities_options": all_weather_cities,
                    "target_cities_empty_means_all": true,
                    "target_cities_risk_tiers": city_risk_tiers,
                    "target_cities_providers": city_providers,
                    "target_cities_trade_enabled": city_trade_enabled,
                    "target_cities_settlement_notes": city_settlement_notes,
                    "target_cities_sigma_multipliers": {
                        "low": pa_core::weather::settlement_sigma_multiplier(pa_core::weather::SettlementRiskTier::Low),
                        "medium": pa_core::weather::settlement_sigma_multiplier(pa_core::weather::SettlementRiskTier::Medium),
                        "high": pa_core::weather::settlement_sigma_multiplier(pa_core::weather::SettlementRiskTier::High),
                    },
                })),
            )
        }
        _ => (axum::http::StatusCode::OK, Json(json!({}))),
    }
}

/// GET /api/status — bot status summary.
async fn get_status(State(state): State<Arc<ApiState>>) -> Json<Value> {
    let uptime_secs = (Utc::now() - state.start_time).num_seconds();
    let settings = state.config.load();
    let accounts = settings.resolved_accounts();
    let account_status: Vec<AccountStatusEntry> = accounts
        .iter()
        .map(|account| AccountStatusEntry {
            name: account.name.clone(),
            strategies: account.strategies.clone(),
            proxy_wallet: account.proxy_wallet.clone(),
            private_key_env: account.private_key_env.clone(),
            private_key_present: std::env::var(&account.private_key_env).is_ok(),
        })
        .collect();
    let ready_accounts = account_status
        .iter()
        .filter(|account| account.private_key_present)
        .count();
    let positions_updated_at = *state.positions_updated_at.read().await;
    let startup_ready = state.startup_ready.load(std::sync::atomic::Ordering::Relaxed);
    let health_ready = state.health_checks.iter().all(|(_, check)| check());

    Json(json!({
        "uptime_seconds": uptime_secs,
        "enabled_strategies": settings.strategy.enabled,
        "scan_interval_ms": settings.strategy.scan_interval_ms,
        "health_port": settings.monitor.health_port,
        "lr_enabled": settings.liquidity_rewards.enabled,
        "event_calendar_enabled": settings.event_calendar.enabled,
        "accounts_configured": account_status.len(),
        "accounts_ready": ready_accounts,
        "trading_ready": ready_accounts > 0 && startup_ready && health_ready,
        "startup_ready": startup_ready,
        "positions_snapshot_updated_at": positions_updated_at.map(|ts| ts.to_rfc3339()),
        "accounts": account_status,
    }))
}

/// Query params for GET /api/positions.
#[derive(Debug, Deserialize)]
struct PositionsQuery {
    strategy: Option<String>,
}

/// GET /api/positions — current positions, optionally filtered by strategy.
async fn get_positions(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<PositionsQuery>,
) -> Json<Value> {
    let positions = state.positions.read().await;
    let filtered: Vec<&PositionApiEntry> = match &query.strategy {
        Some(s) => positions
            .iter()
            .filter(|p| p.strategy.as_deref() == Some(s.as_str()))
            .collect(),
        None => positions.iter().collect(),
    };
    Json(serde_json::to_value(&filtered).unwrap_or(json!([])))
}

/// GET /api/lr/status — LR runtime status.
async fn get_lr_status(State(state): State<Arc<ApiState>>) -> Json<Value> {
    match &state.lr_status {
        Some(lr) => {
            let status = lr.read().await;
            Json(serde_json::to_value(&*status).unwrap_or(json!({})))
        }
        None => Json(json!({"error": "LR not running"})),
    }
}

fn extract_section(settings: &pa_core::config::Settings, section: &str) -> anyhow::Result<Value> {
    match section {
        "strategy" => Ok(serde_json::to_value(&settings.strategy)?),
        "risk" => Ok(serde_json::to_value(&settings.risk)?),
        "market_filter" => Ok(serde_json::to_value(&settings.market_filter)?),
        "weather" => Ok(serde_json::to_value(&settings.weather)?),
        "crypto_alpha" => Ok(serde_json::to_value(&settings.crypto_alpha)?),
        "event_calendar" => Ok(serde_json::to_value(&settings.event_calendar)?),
        "liquidity_rewards" => Ok(serde_json::to_value(&settings.liquidity_rewards)?),
        "smart_money" => Ok(serde_json::to_value(&settings.smart_money)?),
        _ => Err(anyhow::anyhow!("Unknown config section: {}", section)),
    }
}
