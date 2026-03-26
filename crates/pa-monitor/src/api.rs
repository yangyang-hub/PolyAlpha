use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use arc_swap::ArcSwap;
use axum::extract::{Path, Query, State};
use axum::http::header::{CONTENT_DISPOSITION, CONTENT_TYPE};
use axum::http::{HeaderMap, HeaderValue};
use axum::response::{IntoResponse, Json, Response};
use axum::{
    Router,
    routing::{get, post},
};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tower_http::cors::CorsLayer;
use tower_http::services::{ServeDir, ServeFile};

use pa_core::config::Settings;
use pa_core::config::TrackedWalletConfig;
use pa_storage::models::{ConfigHistoryRow, TradeHistoryRow};
use pa_storage::repository::Repository;

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
    pub asset: Option<String>,
    pub direction: Option<String>,
    pub condition_id: Option<String>,
    pub question: Option<String>,
    pub outcome: Option<String>,
    pub bid_price: Option<Decimal>,
    pub mid_price: Option<Decimal>,
    pub unrealized_pnl_bid: Option<Decimal>,
    pub unrealized_pnl_mid: Option<Decimal>,
    pub resolution_bucket: Option<String>,
    pub is_legacy: bool,
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StrategyFinancialEntry {
    pub wallet_balance: Decimal,
    pub positions_market_value: Decimal,
    pub portfolio_value: Decimal,
    pub realized_pnl: Decimal,
}

/// Shared API state for all Axum handlers.
pub struct ApiState {
    pub config: Arc<ArcSwap<Settings>>,
    pub smart_money_config: Arc<ArcSwap<pa_core::config::SmartMoneyConfig>>,
    pub start_time: DateTime<Utc>,
    pub health_checks: Vec<(&'static str, HealthCheck)>,
    /// Optional LR runtime status, populated by the LR background task.
    pub lr_status: Option<Arc<tokio::sync::RwLock<LrRuntimeStatus>>>,
    /// Live positions, populated after account init and updated by position sync.
    pub positions: Arc<tokio::sync::RwLock<Vec<PositionApiEntry>>>,
    /// Timestamp of the last positions snapshot refresh.
    pub positions_updated_at: Arc<tokio::sync::RwLock<Option<DateTime<Utc>>>>,
    /// Latest summed USDC balance across active accounts.
    pub wallet_balance: Arc<tokio::sync::RwLock<Decimal>>,
    /// Strategy-scoped wallet/portfolio snapshots derived from active accounts and positions.
    pub strategy_financials:
        Arc<tokio::sync::RwLock<std::collections::HashMap<String, StrategyFinancialEntry>>>,
    /// Optional repository backing historical opportunities/trades.
    pub repository: Option<Arc<Repository>>,
    /// True once startup has completed enough for the bot to be considered ready.
    pub startup_ready: Arc<AtomicBool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeHistoryApiEntry {
    pub trade_id: String,
    pub opportunity_id: Option<String>,
    pub order_id: Option<String>,
    pub token_id: String,
    pub side: String,
    pub price: Decimal,
    pub size: Decimal,
    pub filled_size: Option<Decimal>,
    pub fee: Option<Decimal>,
    pub tx_type: String,
    pub tx_hash: Option<String>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub strategy: Option<String>,
    pub condition_id: Option<String>,
    pub question: Option<String>,
    pub account_name: Option<String>,
    pub proxy_wallet: Option<String>,
    pub opportunity_status: Option<String>,
    pub estimated_profit: Option<Decimal>,
    pub actual_profit: Option<Decimal>,
    pub detected_at: Option<DateTime<Utc>>,
    pub executed_at: Option<DateTime<Utc>>,
    pub smart_money_attribution: Option<serde_json::Value>,
    pub smart_money_trade_attribution: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
struct SmartMoneyTradeAttributionSlice {
    leader: String,
    actual_filled_size: Decimal,
    actual_fee: Decimal,
    actual_realized_profit: Decimal,
}

#[derive(Debug, Clone, Default)]
struct SmartMoneyTradeAttributionTotals {
    actual_filled_size: Decimal,
    actual_fee: Decimal,
    actual_realized_profit: Decimal,
    trade_count: usize,
}

#[derive(Debug, Serialize)]
struct SmartMoneyLeaderHealthRow {
    leader: String,
    signals: usize,
    accepted: usize,
    rejected: usize,
    accept_rate: f64,
    estimated_realized_pnl: Decimal,
    actual_realized_profit: Decimal,
    trade_count: usize,
    suggested_action: &'static str,
    rationale: String,
}

#[derive(Debug, Serialize)]
struct SmartMoneyReviewQueueRow {
    leader: String,
    address: Option<String>,
    label: Option<String>,
    suggested_action: &'static str,
    current_state: String,
    actionable: bool,
    rationale: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SmartMoneyAuditEntry {
    pub created_at: DateTime<Utc>,
    pub changed_by: String,
    pub version: i32,
    pub blocked_wallet_count: usize,
    pub degraded_wallet_count: usize,
    pub wallet_count: usize,
    pub auto_discover_candidate_count: usize,
    pub route_count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct CryptoOverridePatchAuditEntry {
    pub created_at: DateTime<Utc>,
    pub changed_by: String,
    pub version: i32,
    pub action: String,
    pub mode: String,
    pub filename: String,
    pub export_sha: String,
    pub scope_label: Option<String>,
    pub generated_at: Option<String>,
    pub runtime_applied: bool,
    pub runtime_applied_at: Option<String>,
}

#[derive(Debug, Clone)]
struct CryptoAutoPatchEffectivenessEntry {
    runtime_applied_at: DateTime<Utc>,
    mode: String,
    filename: String,
    export_sha: String,
    scope_labels: Vec<String>,
    post_apply_bad_exit_count: usize,
    post_apply_realized_pnl: Decimal,
    current_open_positions: usize,
    current_open_pnl_bid: Decimal,
    outcome: &'static str,
}

#[derive(Debug, Clone, Serialize)]
struct CryptoBucketWindowSummaryEntry {
    window_label: &'static str,
    resolution_bucket: String,
    shape: String,
    asset_class: String,
    trade_count: usize,
    realized_pnl: Decimal,
    open_positions: usize,
    open_pnl_bid: Decimal,
    bad_exit_count: usize,
}

/// Build the full Axum router with health, metrics, config API, and SPA fallback.
pub fn build_router(state: Arc<ApiState>) -> Router {
    let api_routes = Router::new()
        .route("/api/config", get(get_all_config))
        .route("/api/config/meta/{section}", get(get_section_meta))
        .route("/api/config/{section}", get(get_section))
        .route("/api/trades", get(get_trades))
        .route("/api/smart-money/audit", get(get_smart_money_audit))
        .route("/api/smart-money/leaders", get(get_smart_money_leaders))
        .route(
            "/api/smart-money/leaders/block",
            post(block_smart_money_leader),
        )
        .route(
            "/api/smart-money/leaders/restore",
            post(restore_smart_money_leader),
        )
        .route(
            "/api/smart-money/leaders/degrade",
            post(degrade_smart_money_leader),
        )
        .route(
            "/api/smart-money/leaders/route-template",
            post(apply_smart_money_leader_route_template),
        )
        .route(
            "/api/smart-money/leaders/promote",
            post(promote_smart_money_leader),
        )
        .route("/api/crypto/trades", get(get_crypto_trades))
        .route("/api/crypto/override-patch", get(get_crypto_override_patch))
        .route(
            "/api/crypto/override-patch/audit",
            get(get_crypto_override_patch_audit),
        )
        .route(
            "/api/crypto/override-patch/apply",
            post(apply_crypto_override_patch),
        )
        .route("/api/crypto/decisions", get(get_crypto_candidate_decisions))
        .route("/api/crypto/exits", get(get_crypto_exit_decisions))
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
    let startup_ready = state
        .startup_ready
        .load(std::sync::atomic::Ordering::Relaxed);
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
            let target_cities = pa_core::weather::trade_enabled_weather_location_names();
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
                        pa_core::weather::WeatherProvider::Kma => "kma",
                        pa_core::weather::WeatherProvider::MetOffice => "met_office",
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
            let city_validation_status: std::collections::HashMap<&str, &str> = all_weather_cities
                .iter()
                .map(|city| {
                    let status = match pa_core::weather::settlement_validation_status(city) {
                        pa_core::weather::SettlementValidationStatus::Validated => "validated",
                        pa_core::weather::SettlementValidationStatus::DefaultProtected => {
                            "default_protected"
                        }
                    };
                    (*city, status)
                })
                .collect();
            let city_extra_edge_bps: std::collections::HashMap<&str, u32> = all_weather_cities
                .iter()
                .map(|city| {
                    (
                        *city,
                        pa_core::weather::settlement_extra_edge_bps_for_location(city),
                    )
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
                    "target_cities_validation_status": city_validation_status,
                    "target_cities_extra_edge_bps": city_extra_edge_bps,
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
    fn sorted_count_entries(
        counts: &std::collections::HashMap<String, usize>,
    ) -> Vec<serde_json::Value> {
        let mut entries: Vec<_> = counts.iter().collect();
        entries.sort_by(|(label_a, count_a), (label_b, count_b)| {
            count_b.cmp(count_a).then_with(|| label_a.cmp(label_b))
        });
        entries
            .into_iter()
            .map(|(label, count)| json!({ "label": label, "count": count }))
            .collect()
    }

    fn top_count_entry(
        counts: &std::collections::HashMap<String, usize>,
    ) -> Option<serde_json::Value> {
        counts
            .iter()
            .max_by(|(label_a, count_a), (label_b, count_b)| {
                count_a.cmp(count_b).then_with(|| label_b.cmp(label_a))
            })
            .map(|(label, count)| json!({ "label": label, "count": count }))
    }

    fn count_reason_window(
        decisions: &[crate::diagnostics::CryptoCandidateDecision],
        window: usize,
    ) -> Vec<serde_json::Value> {
        let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for decision in decisions.iter().take(window) {
            *counts.entry(decision.reason.clone()).or_insert(0) += 1;
        }
        sorted_count_entries(&counts)
    }

    fn count_subtype_window(
        decisions: &[crate::diagnostics::CryptoCandidateDecision],
        window: usize,
    ) -> Vec<serde_json::Value> {
        let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for decision in decisions.iter().take(window) {
            *counts
                .entry(
                    decision
                        .event_subtype
                        .clone()
                        .unwrap_or_else(|| "generic".into()),
                )
                .or_insert(0) += 1;
        }
        sorted_count_entries(&counts)
    }

    fn count_asset_window(
        decisions: &[crate::diagnostics::CryptoCandidateDecision],
        window: usize,
    ) -> Vec<serde_json::Value> {
        let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for decision in decisions.iter().take(window) {
            *counts.entry(decision.asset.clone()).or_insert(0) += 1;
        }
        sorted_count_entries(&counts)
    }

    fn top_count(entries: &[serde_json::Value]) -> usize {
        entries
            .first()
            .and_then(|entry| entry.get("count"))
            .and_then(|value| value.as_u64())
            .unwrap_or(0) as usize
    }

    fn push_hint(
        hints: &mut Vec<serde_json::Value>,
        kind: &str,
        priority: &str,
        title: &str,
        detail: String,
    ) {
        hints.push(json!({
            "kind": kind,
            "priority": priority,
            "title": title,
            "detail": detail,
        }));
    }

    fn asset_class_for_label(asset: Option<&str>) -> &'static str {
        match asset.unwrap_or_default().to_ascii_lowercase().as_str() {
            "bitcoin" | "ethereum" => "major",
            "" => "any",
            _ => "alt",
        }
    }

    fn normalized_event_subtype(subtype: Option<&str>) -> &'static str {
        match subtype.unwrap_or_default() {
            "unlock" => "unlock",
            "upgrade" => "upgrade",
            "regulatory" => "regulatory",
            _ => "any",
        }
    }

    fn bucket_scope_label(asset_class: &str, event_subtype: &str) -> String {
        match (asset_class, event_subtype) {
            ("any", "any") => "all".into(),
            (_, "any") => asset_class.to_string(),
            ("any", _) => event_subtype.to_string(),
            _ => format!("{asset_class} / {event_subtype}"),
        }
    }

    fn normalized_shape_label(market_type: Option<&str>) -> &'static str {
        match market_type.unwrap_or_default() {
            "range" => "range",
            _ => "directional",
        }
    }

    fn shaped_scope_label(asset_class: &str, event_subtype: &str, shape: &str) -> String {
        format!(
            "{} / {}",
            bucket_scope_label(asset_class, event_subtype),
            shape
        )
    }

    fn normalized_resolution_bucket(days_to_resolution: Option<u32>) -> &'static str {
        match days_to_resolution {
            Some(0) => "same_day",
            Some(1) => "next_day",
            _ => "legacy",
        }
    }

    fn dominant_bucket_reason(
        counts: &std::collections::HashMap<String, usize>,
    ) -> Option<(String, usize)> {
        counts
            .iter()
            .max_by(|(reason_a, count_a), (reason_b, count_b)| {
                count_a.cmp(count_b).then_with(|| reason_b.cmp(reason_a))
            })
            .map(|(reason, count)| (reason.clone(), *count))
    }

    fn push_scoped_hint(
        hints: &mut Vec<serde_json::Value>,
        kind: &str,
        priority: &str,
        title: &str,
        detail: String,
        scope_label: String,
        support_count: usize,
    ) {
        hints.push(json!({
            "kind": kind,
            "priority": priority,
            "title": title,
            "detail": detail,
            "scope_label": scope_label,
            "support_count": support_count,
        }));
    }

    fn push_scoped_override_suggestion(
        suggestions: &mut Vec<serde_json::Value>,
        kind: &str,
        priority: &str,
        target_field: &str,
        direction: &str,
        selector_asset_class: &str,
        selector_event_subtype: &str,
        selector_shape: &str,
        source_bucket: &str,
        source_reason: &str,
        rationale: String,
        support_count: usize,
    ) {
        suggestions.push(json!({
            "kind": kind,
            "priority": priority,
            "target_field": target_field,
            "direction": direction,
            "selector_asset_class": selector_asset_class,
            "selector_event_subtype": selector_event_subtype,
            "selector_shape": selector_shape,
            "source_bucket": source_bucket,
            "scope_label": shaped_scope_label(
                selector_asset_class,
                selector_event_subtype,
                selector_shape,
            ),
            "source_reason": source_reason,
            "rationale": rationale,
            "support_count": support_count,
        }));
    }

    fn priority_rank(priority: &str) -> u8 {
        match priority {
            "high" => 3,
            "medium" => 2,
            "low" => 1,
            _ => 0,
        }
    }

    fn render_override_patch_preview(suggestions: &[serde_json::Value]) -> serde_json::Value {
        fn preview_horizon_for_source_bucket(source_bucket: &str) -> &'static str {
            match source_bucket {
                "same_day" | "next_day" => "short",
                _ => "any",
            }
        }

        let mut grouped: std::collections::BTreeMap<
            (String, String, String, String),
            Vec<(String, String, String, usize)>,
        > = std::collections::BTreeMap::new();
        let mut unsupported: Vec<String> = Vec::new();

        for suggestion in suggestions {
            let target_field = suggestion
                .get("target_field")
                .and_then(|value| value.as_str())
                .unwrap_or_default()
                .to_string();
            let direction = suggestion
                .get("direction")
                .and_then(|value| value.as_str())
                .unwrap_or_default()
                .to_string();
            let selector_asset_class = suggestion
                .get("selector_asset_class")
                .and_then(|value| value.as_str())
                .unwrap_or("any")
                .to_string();
            let selector_event_subtype = suggestion
                .get("selector_event_subtype")
                .and_then(|value| value.as_str())
                .unwrap_or("any")
                .to_string();
            let selector_shape = suggestion
                .get("selector_shape")
                .and_then(|value| value.as_str())
                .unwrap_or("directional")
                .to_string();
            let source_bucket = suggestion
                .get("source_bucket")
                .and_then(|value| value.as_str())
                .unwrap_or("short")
                .to_string();
            let support_count = suggestion
                .get("support_count")
                .and_then(|value| value.as_u64())
                .unwrap_or(0) as usize;

            if patch_preview_multiplier_for(&target_field, &direction).is_none() {
                unsupported.push(format!(
                    "{} / {} / {} / {} -> {} {}",
                    selector_asset_class,
                    selector_event_subtype,
                    selector_shape,
                    source_bucket,
                    target_field,
                    direction
                ));
                continue;
            }

            grouped
                .entry((
                    selector_asset_class,
                    selector_event_subtype,
                    selector_shape,
                    source_bucket,
                ))
                .or_default()
                .push((
                    target_field,
                    direction,
                    suggestion
                        .get("source_reason")
                        .and_then(|value| value.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    support_count,
                ));
        }

        let mut rows = Vec::new();
        let mut toml_blocks = Vec::new();
        for ((asset_class, event_subtype, shape, source_bucket), mut fields) in grouped {
            fields.sort_by(|a, b| b.3.cmp(&a.3).then_with(|| a.0.cmp(&b.0)));
            let market_type = if shape == "range" { "range" } else { "binary" };
            let scope_label = shaped_scope_label(&asset_class, &event_subtype, &shape);
            let horizon = preview_horizon_for_source_bucket(&source_bucket);

            let mut block = String::new();
            block.push_str(&format!("# source_bucket = {}\n", source_bucket));
            block.push_str("[[crypto_alpha.calibration_overrides]]\n");
            block.push_str("asset = \"*\"\n");
            block.push_str(&format!("asset_class = \"{}\"\n", asset_class));
            block.push_str(&format!("horizon = \"{}\"\n", horizon));
            block.push_str(&format!("resolution_bucket = \"{}\"\n", source_bucket));
            block.push_str(&format!("market_type = \"{}\"\n", market_type));
            block.push_str(&format!("event_subtype = \"{}\"\n", event_subtype));

            let mut field_rows = Vec::new();
            for (target_field, direction, source_reason, support_count) in fields {
                if let Some(value) = patch_preview_multiplier_for(&target_field, &direction) {
                    block.push_str(&format!("{target_field} = {value}\n"));
                    field_rows.push(json!({
                        "target_field": target_field,
                        "direction": direction,
                        "source_reason": source_reason,
                        "support_count": support_count,
                        "preview_value": value,
                    }));
                }
            }

            toml_blocks.push(block.trim_end().to_string());
            rows.push(json!({
                "scope_label": scope_label,
                "selector_asset_class": asset_class,
                "selector_event_subtype": event_subtype,
                "selector_shape": shape,
                "source_bucket": source_bucket,
                "resolution_bucket": source_bucket,
                "horizon": horizon,
                "market_type": market_type,
                "fields": field_rows,
            }));
        }

        json!({
            "supported_row_count": rows.len(),
            "unsupported_suggestion_count": unsupported.len(),
            "unsupported_suggestions": unsupported,
            "rows": rows,
            "toml": toml_blocks.join("\n\n"),
        })
    }

    fn active_cooldown_entries(
        exits: &[crate::diagnostics::CryptoExitDecision],
        market_type: &str,
        target_days_to_resolution: u32,
        trigger_count: usize,
        window_secs: i64,
        label_kind: &str,
    ) -> Vec<serde_json::Value> {
        let now = Utc::now();
        let cooldown_window = chrono::Duration::seconds(window_secs);
        let mut grouped: std::collections::HashMap<
            (String, String),
            std::collections::HashMap<String, DateTime<Utc>>,
        > = std::collections::HashMap::new();

        for entry in exits.iter().filter(|entry| {
            matches!(
                entry.reason.as_str(),
                "model_reversal" | "relative_stop_loss"
            ) && entry.market_type.as_deref() == Some(market_type)
                && entry.days_to_resolution == Some(target_days_to_resolution)
                && (now - entry.recorded_at) <= cooldown_window
        }) {
            let Some(asset) = entry.asset.clone() else {
                continue;
            };
            if matches!(label_kind, "same_day_alt" | "next_day_alt_range")
                && asset_class_for_label(Some(&asset)) != "alt"
            {
                continue;
            }
            let subtype = entry
                .event_subtype
                .clone()
                .unwrap_or_else(|| "generic".into());
            grouped
                .entry((asset, subtype))
                .or_default()
                .entry(entry.question.clone())
                .and_modify(|recorded_at| {
                    if entry.recorded_at > *recorded_at {
                        *recorded_at = entry.recorded_at;
                    }
                })
                .or_insert(entry.recorded_at);
        }

        let mut entries = Vec::new();
        for ((asset, subtype), questions) in grouped {
            let mut timestamps: Vec<_> = questions.into_values().collect();
            timestamps.sort_by(|a, b| b.cmp(a));
            if timestamps.len() < trigger_count {
                continue;
            }
            let threshold_time = timestamps[trigger_count - 1];
            let remaining = (cooldown_window - (now - threshold_time))
                .num_seconds()
                .max(0);
            if remaining <= 0 {
                continue;
            }
            let post_trigger_bad_exit_count = timestamps
                .iter()
                .filter(|recorded_at| **recorded_at > threshold_time)
                .count();
            let scope_label = if subtype == "generic" {
                asset.clone()
            } else {
                format!("{asset} / {subtype}")
            };
            entries.push(json!({
                "kind": label_kind,
                "asset": asset,
                "event_subtype": subtype,
                "shape": normalized_shape_label(Some(market_type)),
                "scope_label": scope_label,
                "trigger_count": trigger_count,
                "current_count": timestamps.len(),
                "triggered_at": threshold_time.to_rfc3339(),
                "post_trigger_bad_exit_count": post_trigger_bad_exit_count,
                "remaining_secs": remaining,
            }));
        }

        entries.sort_by(|a, b| {
            let remaining_a = a
                .get("remaining_secs")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            let remaining_b = b
                .get("remaining_secs")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            remaining_b.cmp(&remaining_a).then_with(|| {
                let label_a = a.get("scope_label").and_then(|v| v.as_str()).unwrap_or("");
                let label_b = b.get("scope_label").and_then(|v| v.as_str()).unwrap_or("");
                label_a.cmp(label_b)
            })
        });
        entries
    }

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
    let wallet_balance = *state.wallet_balance.read().await;
    let all_positions = state.positions.read().await.clone();
    let total_wallet_positions_market_value_bid: Decimal = all_positions
        .iter()
        .map(|position| {
            position
                .bid_price
                .or(position.current_price)
                .unwrap_or(Decimal::ZERO)
                * position.size
        })
        .sum();
    let total_wallet_positions_market_value_mid: Decimal = all_positions
        .iter()
        .map(|position| position.mid_price.unwrap_or(Decimal::ZERO) * position.size)
        .sum();
    let total_wallet_portfolio_value_bid = wallet_balance + total_wallet_positions_market_value_bid;
    let total_wallet_portfolio_value_mid = wallet_balance + total_wallet_positions_market_value_mid;
    let strategy_financials = state.strategy_financials.read().await.clone();
    let startup_ready = state
        .startup_ready
        .load(std::sync::atomic::Ordering::Relaxed);
    let health_ready = state.health_checks.iter().all(|(_, check)| check());
    let recent_candidate_decisions = crate::diagnostics::recent_crypto_candidate_decisions();
    let recent_smart_money_decisions = crate::diagnostics::recent_smart_money_decisions();
    let recent_smart_money_exits = crate::diagnostics::recent_smart_money_exit_decisions();
    let smart_money_leader_candidates = if let Some(repo) = &state.repository {
        repo.load_smart_money_leader_candidates(24)
            .await
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let smart_money_runtime_config = state.smart_money_config.load();
    let mut smart_money_wallet_scores = crate::diagnostics::smart_money_wallet_scores();
    smart_money_wallet_scores.sort_by(|a, b| {
        b.effective_weight
            .cmp(&a.effective_weight)
            .then_with(|| a.label.cmp(&b.label))
            .then_with(|| a.address.cmp(&b.address))
    });
    let recent_gate_rejects: Vec<_> = recent_candidate_decisions
        .iter()
        .filter(|decision| decision.action == "gate_reject")
        .take(24)
        .cloned()
        .collect();
    let recent_gate_scales: Vec<_> = recent_candidate_decisions
        .iter()
        .filter(|decision| decision.action == "gate_scale")
        .take(24)
        .cloned()
        .collect();
    let mut gate_reason_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    let mut gate_asset_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    let mut gate_subtype_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for decision in &recent_gate_rejects {
        *gate_reason_counts
            .entry(decision.reason.clone())
            .or_insert(0) += 1;
        *gate_asset_counts.entry(decision.asset.clone()).or_insert(0) += 1;
        *gate_subtype_counts
            .entry(
                decision
                    .event_subtype
                    .clone()
                    .unwrap_or_else(|| "generic".into()),
            )
            .or_insert(0) += 1;
    }
    let top_reason = gate_reason_counts
        .iter()
        .max_by_key(|(_, count)| *count)
        .map(|(reason, count)| json!({"label": reason, "count": count}));
    let top_asset = gate_asset_counts
        .iter()
        .max_by_key(|(_, count)| *count)
        .map(|(asset, count)| json!({"label": asset, "count": count}));
    let top_subtype = gate_subtype_counts
        .iter()
        .max_by_key(|(_, count)| *count)
        .map(|(subtype, count)| json!({"label": subtype, "count": count}));
    let reason_counts = sorted_count_entries(&gate_reason_counts);
    let asset_counts = sorted_count_entries(&gate_asset_counts);
    let subtype_counts = sorted_count_entries(&gate_subtype_counts);
    let reason_windows = json!({
        "recent_8": count_reason_window(&recent_gate_rejects, 8),
        "recent_24": count_reason_window(&recent_gate_rejects, 24),
    });
    let subtype_windows = json!({
        "recent_8": count_subtype_window(&recent_gate_rejects, 8),
        "recent_24": count_subtype_window(&recent_gate_rejects, 24),
    });
    let asset_windows = json!({
        "recent_8": count_asset_window(&recent_gate_rejects, 8),
        "recent_24": count_asset_window(&recent_gate_rejects, 24),
    });
    let reason_details: Vec<_> = gate_reason_counts
        .keys()
        .map(|reason| {
            let matching_decisions: Vec<_> = recent_gate_rejects
                .iter()
                .filter(|decision| decision.reason == *reason)
                .collect();
            let mut matching_asset_counts: std::collections::HashMap<String, usize> =
                std::collections::HashMap::new();
            let mut matching_subtype_counts: std::collections::HashMap<String, usize> =
                std::collections::HashMap::new();
            for decision in matching_decisions {
                *matching_asset_counts
                    .entry(decision.asset.clone())
                    .or_insert(0) += 1;
                *matching_subtype_counts
                    .entry(
                        decision
                            .event_subtype
                            .clone()
                            .unwrap_or_else(|| "generic".into()),
                    )
                    .or_insert(0) += 1;
            }
            json!({
                "label": reason,
                "count": gate_reason_counts.get(reason).copied().unwrap_or(0),
                "top_asset": top_count_entry(&matching_asset_counts),
                "top_subtype": top_count_entry(&matching_subtype_counts),
            })
        })
        .collect();
    let mut reason_details = reason_details;
    reason_details.sort_by(|a, b| {
        let count_a = a.get("count").and_then(|value| value.as_u64()).unwrap_or(0);
        let count_b = b.get("count").and_then(|value| value.as_u64()).unwrap_or(0);
        let label_a = a
            .get("label")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        let label_b = b
            .get("label")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        count_b.cmp(&count_a).then_with(|| label_a.cmp(label_b))
    });
    let mut gate_scale_reason_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    let mut gate_scale_asset_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    let mut gate_scale_subtype_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for decision in &recent_gate_scales {
        *gate_scale_reason_counts
            .entry(decision.reason.clone())
            .or_insert(0) += 1;
        *gate_scale_asset_counts
            .entry(decision.asset.clone())
            .or_insert(0) += 1;
        *gate_scale_subtype_counts
            .entry(
                decision
                    .event_subtype
                    .clone()
                    .unwrap_or_else(|| "generic".into()),
            )
            .or_insert(0) += 1;
    }
    let gate_scale_reason_details: Vec<_> = gate_scale_reason_counts
        .keys()
        .map(|reason| {
            let matching_decisions: Vec<_> = recent_gate_scales
                .iter()
                .filter(|decision| decision.reason == *reason)
                .collect();
            let mut matching_asset_counts: std::collections::HashMap<String, usize> =
                std::collections::HashMap::new();
            let mut matching_subtype_counts: std::collections::HashMap<String, usize> =
                std::collections::HashMap::new();
            for decision in matching_decisions {
                *matching_asset_counts
                    .entry(decision.asset.clone())
                    .or_insert(0) += 1;
                *matching_subtype_counts
                    .entry(
                        decision
                            .event_subtype
                            .clone()
                            .unwrap_or_else(|| "generic".into()),
                    )
                    .or_insert(0) += 1;
            }
            json!({
                "label": reason,
                "count": gate_scale_reason_counts.get(reason).copied().unwrap_or(0),
                "top_asset": top_count_entry(&matching_asset_counts),
                "top_subtype": top_count_entry(&matching_subtype_counts),
            })
        })
        .collect();
    let mut gate_scale_reason_details = gate_scale_reason_details;
    gate_scale_reason_details.sort_by(|a, b| {
        let count_a = a.get("count").and_then(|value| value.as_u64()).unwrap_or(0);
        let count_b = b.get("count").and_then(|value| value.as_u64()).unwrap_or(0);
        let label_a = a
            .get("label")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        let label_b = b
            .get("label")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        count_b.cmp(&count_a).then_with(|| label_a.cmp(label_b))
    });
    let gate_scale_reason_counts_view = sorted_count_entries(&gate_scale_reason_counts);
    let gate_scale_asset_counts_view = sorted_count_entries(&gate_scale_asset_counts);
    let gate_scale_subtype_counts_view = sorted_count_entries(&gate_scale_subtype_counts);

    let recent_exits: Vec<_> = crate::diagnostics::recent_crypto_exit_decisions()
        .into_iter()
        .take(24)
        .collect();
    let crypto_cooldown_buckets = {
        let mut entries = active_cooldown_entries(
            &recent_exits,
            "range",
            0,
            settings
                .crypto_alpha
                .same_day_range_bad_exit_cooldown_trigger_count as usize,
            settings.crypto_alpha.same_day_range_bad_exit_cooldown_secs as i64,
            "same_day_range",
        );
        entries.extend(active_cooldown_entries(
            &recent_exits,
            "binary",
            0,
            settings
                .crypto_alpha
                .same_day_alt_bad_exit_cooldown_trigger_count as usize,
            settings.crypto_alpha.same_day_alt_bad_exit_cooldown_secs as i64,
            "same_day_alt",
        ));
        entries.extend(active_cooldown_entries(
            &recent_exits,
            "range",
            1,
            settings
                .crypto_alpha
                .next_day_alt_range_bad_exit_cooldown_trigger_count as usize,
            settings
                .crypto_alpha
                .next_day_alt_range_bad_exit_cooldown_secs as i64,
            "next_day_alt_range",
        ));
        entries.sort_by(|a, b| {
            let remaining_a = a
                .get("remaining_secs")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            let remaining_b = b
                .get("remaining_secs")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            remaining_b.cmp(&remaining_a).then_with(|| {
                let label_a = a.get("scope_label").and_then(|v| v.as_str()).unwrap_or("");
                let label_b = b.get("scope_label").and_then(|v| v.as_str()).unwrap_or("");
                label_a.cmp(label_b)
            })
        });
        entries
    };
    let smart_money_recent_entries: Vec<_> = recent_smart_money_decisions
        .iter()
        .filter(|decision| matches!(decision.signal_type.as_str(), "entry" | "increase"))
        .take(48)
        .cloned()
        .collect();
    let smart_money_recent_rejections: Vec<_> = smart_money_recent_entries
        .iter()
        .filter(|decision| !decision.accepted)
        .cloned()
        .collect();
    let mut smart_money_reject_reason_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    let mut smart_money_wallet_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    let mut smart_money_source_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for decision in &smart_money_recent_entries {
        *smart_money_wallet_counts
            .entry(format!("{} wallets", decision.wallet_count))
            .or_insert(0) += 1;
        match (decision.source_data_api, decision.source_onchain) {
            (true, true) => {
                *smart_money_source_counts
                    .entry("data_api+onchain".into())
                    .or_insert(0) += 1;
            }
            (true, false) => {
                *smart_money_source_counts
                    .entry("data_api".into())
                    .or_insert(0) += 1;
            }
            (false, true) => {
                *smart_money_source_counts
                    .entry("onchain".into())
                    .or_insert(0) += 1;
            }
            (false, false) => {}
        }
    }
    for decision in &smart_money_recent_rejections {
        if let Some(reason) = &decision.reject_reason {
            *smart_money_reject_reason_counts
                .entry(reason.clone())
                .or_insert(0) += 1;
        }
    }
    let smart_money_reason_counts = sorted_count_entries(&smart_money_reject_reason_counts);
    let smart_money_wallet_counts = sorted_count_entries(&smart_money_wallet_counts);
    let smart_money_source_counts = sorted_count_entries(&smart_money_source_counts);
    let smart_money_recent_decisions: Vec<_> = smart_money_recent_entries
        .iter()
        .take(12)
        .map(|decision| {
            json!({
                "recorded_at": decision.recorded_at.to_rfc3339(),
                "token_id": decision.token_id,
                "condition_id": decision.condition_id,
                "signal_type": decision.signal_type,
                "accepted": decision.accepted,
                "reject_reason": decision.reject_reason,
                "wallet_count": decision.wallet_count,
                "max_wallet_weight": decision.max_wallet_weight,
                "source_data_api": decision.source_data_api,
                "source_onchain": decision.source_onchain,
                "leader_addresses": decision.leader_addresses,
                "leader_labels": decision.leader_labels,
            })
        })
        .collect();
    let mut smart_money_leader_activity: std::collections::HashMap<String, (usize, usize, usize)> =
        std::collections::HashMap::new();
    for decision in &smart_money_recent_entries {
        let leaders: Vec<String> = if !decision.leader_labels.is_empty() {
            decision.leader_labels.clone()
        } else {
            decision.leader_addresses.clone()
        };
        for leader in leaders {
            let entry = smart_money_leader_activity
                .entry(leader)
                .or_insert((0, 0, 0));
            entry.0 += 1;
            if decision.accepted {
                entry.1 += 1;
            } else {
                entry.2 += 1;
            }
        }
    }
    let mut smart_money_top_leaders: Vec<_> = smart_money_leader_activity
        .into_iter()
        .map(|(leader, (signals, accepted, rejected))| {
            json!({
                "leader": leader,
                "signals": signals,
                "accepted": accepted,
                "rejected": rejected,
                "accept_rate": if signals > 0 { accepted as f64 / signals as f64 } else { 0.0 },
            })
        })
        .collect();
    smart_money_top_leaders.sort_by(|a, b| {
        let signals_a = a
            .get("signals")
            .and_then(|value| value.as_u64())
            .unwrap_or(0);
        let signals_b = b
            .get("signals")
            .and_then(|value| value.as_u64())
            .unwrap_or(0);
        let accepted_a = a
            .get("accepted")
            .and_then(|value| value.as_u64())
            .unwrap_or(0);
        let accepted_b = b
            .get("accepted")
            .and_then(|value| value.as_u64())
            .unwrap_or(0);
        signals_b
            .cmp(&signals_a)
            .then_with(|| accepted_b.cmp(&accepted_a))
    });
    let smart_money_leader_pnl_attribution =
        crate::diagnostics::smart_money_leader_pnl_attribution();
    let smart_money_trade_attribution_totals =
        load_smart_money_trade_attribution_totals(state.as_ref(), 500)
            .await
            .unwrap_or_default();
    let smart_money_trade_attribution_summary =
        smart_money_trade_attribution_summary_json(&smart_money_trade_attribution_totals);
    let mut smart_money_exit_reason_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for exit in &recent_smart_money_exits {
        *smart_money_exit_reason_counts
            .entry(exit.reason.clone())
            .or_insert(0) += 1;
    }
    let smart_money_recent_exits: Vec<_> = recent_smart_money_exits
        .iter()
        .take(12)
        .map(|exit| {
            json!({
                "recorded_at": exit.recorded_at.to_rfc3339(),
                "token_id": exit.token_id,
                "condition_id": exit.condition_id,
                "reason": exit.reason,
                "question": exit.question,
                "best_bid": exit.best_bid,
                "avg_cost": exit.avg_cost,
                "size": exit.size,
                "estimated_profit": exit.estimated_profit,
                "attributed_leaders": exit.attributed_leaders,
            })
        })
        .collect();
    let mut estimated_pnl_by_leader: std::collections::HashMap<String, (Decimal, usize)> =
        std::collections::HashMap::new();
    for entry in &smart_money_leader_pnl_attribution {
        estimated_pnl_by_leader.insert(
            entry.leader.clone(),
            (entry.estimated_realized_pnl, entry.estimated_exit_count),
        );
    }
    let mut leader_health_rows: Vec<SmartMoneyLeaderHealthRow> = smart_money_top_leaders
        .iter()
        .map(|entry| {
            let leader = entry
                .get("leader")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string();
            let signals = entry
                .get("signals")
                .and_then(|value| value.as_u64())
                .unwrap_or(0) as usize;
            let accepted = entry
                .get("accepted")
                .and_then(|value| value.as_u64())
                .unwrap_or(0) as usize;
            let rejected = entry
                .get("rejected")
                .and_then(|value| value.as_u64())
                .unwrap_or(0) as usize;
            let accept_rate = entry
                .get("accept_rate")
                .and_then(|value| value.as_f64())
                .unwrap_or(0.0);
            let estimated_realized_pnl = estimated_pnl_by_leader
                .get(&leader)
                .map(|(pnl, _)| *pnl)
                .unwrap_or(Decimal::ZERO);
            let trade_totals = smart_money_trade_attribution_totals
                .get(&leader)
                .cloned()
                .unwrap_or_default();
            let (suggested_action, rationale) = smart_money_health_action(
                signals,
                accept_rate,
                estimated_realized_pnl,
                trade_totals.actual_realized_profit,
                trade_totals.trade_count,
            );
            SmartMoneyLeaderHealthRow {
                leader,
                signals,
                accepted,
                rejected,
                accept_rate,
                estimated_realized_pnl,
                actual_realized_profit: trade_totals.actual_realized_profit,
                trade_count: trade_totals.trade_count,
                suggested_action,
                rationale,
            }
        })
        .collect();
    leader_health_rows.sort_by(|a, b| {
        let severity_a = smart_money_health_action_rank(a.suggested_action);
        let severity_b = smart_money_health_action_rank(b.suggested_action);
        severity_b
            .cmp(&severity_a)
            .then_with(|| b.actual_realized_profit.cmp(&a.actual_realized_profit))
            .then_with(|| b.signals.cmp(&a.signals))
    });
    let mut review_queue_rows: Vec<SmartMoneyReviewQueueRow> = leader_health_rows
        .iter()
        .filter(|entry| entry.suggested_action != "observe")
        .map(|entry| {
            let matched_candidate = smart_money_leader_candidates.iter().find(|candidate| {
                candidate.address.eq_ignore_ascii_case(&entry.leader)
                    || (!candidate.label.is_empty()
                        && candidate.label.eq_ignore_ascii_case(&entry.leader))
            });
            let (address, label, current_state, actionable) =
                if let Some(candidate) = matched_candidate {
                    let blocked = smart_money_runtime_config
                        .blocked_wallets
                        .iter()
                        .any(|wallet| wallet.eq_ignore_ascii_case(&candidate.address));
                    let degraded = smart_money_runtime_config
                        .degraded_wallets
                        .iter()
                        .find(|wallet| wallet.address.eq_ignore_ascii_case(&candidate.address))
                        .map(|wallet| wallet.multiplier);
                    let current_state = if blocked {
                        "blocked".to_string()
                    } else if let Some(multiplier) = degraded {
                        format!("degraded x{}", multiplier.round_dp(2))
                    } else if candidate.promoted {
                        "promoted".to_string()
                    } else {
                        "candidate".to_string()
                    };
                    let actionable = match entry.suggested_action {
                        "block_candidate" => !blocked,
                        "degrade" => {
                            !blocked && degraded.is_none_or(|multiplier| multiplier >= Decimal::ONE)
                        }
                        "keep_or_promote" => {
                            blocked
                                || degraded.is_some_and(|multiplier| multiplier < Decimal::ONE)
                                || !candidate.promoted
                        }
                        _ => false,
                    };
                    (
                        Some(candidate.address.clone()),
                        if candidate.label.is_empty() {
                            None
                        } else {
                            Some(candidate.label.clone())
                        },
                        current_state,
                        actionable,
                    )
                } else {
                    (None, None, "unmatched".to_string(), false)
                };
            SmartMoneyReviewQueueRow {
                leader: entry.leader.clone(),
                address,
                label,
                suggested_action: entry.suggested_action,
                current_state,
                actionable,
                rationale: entry.rationale.clone(),
            }
        })
        .collect();
    review_queue_rows.sort_by(|a, b| {
        let severity_a = smart_money_health_action_rank(a.suggested_action);
        let severity_b = smart_money_health_action_rank(b.suggested_action);
        b.actionable
            .cmp(&a.actionable)
            .then_with(|| severity_b.cmp(&severity_a))
            .then_with(|| a.leader.cmp(&b.leader))
    });
    let review_queue_action_counts = review_queue_rows.iter().filter(|row| row.actionable).fold(
        std::collections::HashMap::<String, usize>::new(),
        |mut counts, row| {
            *counts.entry(row.suggested_action.to_string()).or_insert(0) += 1;
            counts
        },
    );
    let mut crypto_entry_tuning_hints = Vec::new();
    let mut crypto_override_suggestions = Vec::new();
    let mut crypto_post_entry_tuning_hints = Vec::new();
    let mut crypto_post_entry_override_suggestions = Vec::new();

    let mut bucket_reject_reason_counts: std::collections::HashMap<
        (String, String, String, String),
        std::collections::HashMap<String, usize>,
    > = std::collections::HashMap::new();
    let mut bucket_scale_reason_counts: std::collections::HashMap<
        (String, String, String, String),
        std::collections::HashMap<String, usize>,
    > = std::collections::HashMap::new();
    let mut bucket_asset_counts: std::collections::HashMap<
        (String, String, String, String),
        std::collections::HashMap<String, usize>,
    > = std::collections::HashMap::new();

    for decision in &recent_gate_rejects {
        let key = (
            asset_class_for_label(Some(&decision.asset)).to_string(),
            normalized_event_subtype(decision.event_subtype.as_deref()).to_string(),
            normalized_shape_label(Some(&decision.selected_market_type)).to_string(),
            normalized_resolution_bucket(Some(decision.selected_days_to_resolution)).to_string(),
        );
        *bucket_reject_reason_counts
            .entry(key.clone())
            .or_default()
            .entry(decision.reason.clone())
            .or_insert(0) += 1;
        *bucket_asset_counts
            .entry(key)
            .or_default()
            .entry(decision.asset.clone())
            .or_insert(0) += 1;
    }
    for decision in &recent_gate_scales {
        let key = (
            asset_class_for_label(Some(&decision.asset)).to_string(),
            normalized_event_subtype(decision.event_subtype.as_deref()).to_string(),
            normalized_shape_label(Some(&decision.selected_market_type)).to_string(),
            normalized_resolution_bucket(Some(decision.selected_days_to_resolution)).to_string(),
        );
        *bucket_scale_reason_counts
            .entry(key.clone())
            .or_default()
            .entry(decision.reason.clone())
            .or_insert(0) += 1;
        *bucket_asset_counts
            .entry(key)
            .or_default()
            .entry(decision.asset.clone())
            .or_insert(0) += 1;
    }

    let mut entry_bucket_actions = Vec::new();
    let mut bucket_keys: std::collections::BTreeSet<(String, String, String, String)> =
        std::collections::BTreeSet::new();
    bucket_keys.extend(bucket_reject_reason_counts.keys().cloned());
    bucket_keys.extend(bucket_scale_reason_counts.keys().cloned());
    for (asset_class, event_subtype, shape, source_bucket) in bucket_keys {
        let reject_reasons = bucket_reject_reason_counts
            .get(&(
                asset_class.clone(),
                event_subtype.clone(),
                shape.clone(),
                source_bucket.clone(),
            ))
            .cloned()
            .unwrap_or_default();
        let scale_reasons = bucket_scale_reason_counts
            .get(&(
                asset_class.clone(),
                event_subtype.clone(),
                shape.clone(),
                source_bucket.clone(),
            ))
            .cloned()
            .unwrap_or_default();
        let reject_total: usize = reject_reasons.values().sum();
        let scale_total: usize = scale_reasons.values().sum();
        if reject_total == 0 && scale_total == 0 {
            continue;
        }
        let (family, dominant_reason, support_count) = if scale_total > reject_total {
            let Some((reason, count)) = dominant_bucket_reason(&scale_reasons) else {
                continue;
            };
            ("gate_scale", reason, count)
        } else {
            let Some((reason, count)) = dominant_bucket_reason(&reject_reasons) else {
                continue;
            };
            ("gate_reject", reason, count)
        };
        let top_asset = bucket_asset_counts
            .get(&(
                asset_class.clone(),
                event_subtype.clone(),
                shape.clone(),
                source_bucket.clone(),
            ))
            .and_then(top_count_entry)
            .and_then(|value| {
                value
                    .get("label")
                    .and_then(|label| label.as_str())
                    .map(ToString::to_string)
            })
            .unwrap_or_else(|| asset_class.clone());
        entry_bucket_actions.push((
            asset_class,
            event_subtype,
            shape,
            source_bucket,
            family.to_string(),
            dominant_reason,
            support_count,
            top_asset,
        ));
    }
    entry_bucket_actions.sort_by(|a, b| {
        b.6.cmp(&a.6)
            .then_with(|| a.0.cmp(&b.0))
            .then_with(|| a.1.cmp(&b.1))
            .then_with(|| a.2.cmp(&b.2))
            .then_with(|| a.3.cmp(&b.3))
    });
    for (
        asset_class,
        event_subtype,
        shape,
        source_bucket,
        family,
        dominant_reason,
        support_count,
        top_asset,
    ) in entry_bucket_actions.into_iter().take(6)
    {
        if source_bucket == "legacy" {
            continue;
        }
        let scope_label = shaped_scope_label(&asset_class, &event_subtype, &shape);
        match (family.as_str(), dominant_reason.as_str()) {
            ("gate_reject", "asset_exposure_cap") => {
                push_scoped_hint(
                    &mut crypto_entry_tuning_hints,
                    "gate_reject",
                    "high",
                    "先看敞口上限",
                    format!(
                        "{scope_label} 最近主要被 {top_asset} 的资产敞口限制挡住，先看容量上限，不要先放宽 entry 参数。"
                    ),
                    scope_label.clone(),
                    support_count,
                );
                push_scoped_override_suggestion(
                    &mut crypto_override_suggestions,
                    "entry",
                    "high",
                    "max_exposure_per_asset_pct",
                    "raise",
                    &asset_class,
                    &event_subtype,
                    &shape,
                    &source_bucket,
                    "asset_exposure_cap",
                    format!("{scope_label} 最近更像容量不够，而不是 alpha 不够。"),
                    support_count,
                );
            }
            ("gate_reject", "min_order_or_budget") => {
                push_scoped_hint(
                    &mut crypto_entry_tuning_hints,
                    "gate_reject",
                    "high",
                    "先看预算与 sizing",
                    format!(
                        "{scope_label} 最近主要卡在预算/最小下单，优先检查 sizing 和可用资金，而不是先放宽 edge。"
                    ),
                    scope_label.clone(),
                    support_count,
                );
                push_scoped_override_suggestion(
                    &mut crypto_override_suggestions,
                    "entry",
                    "medium",
                    "size_multiplier",
                    "tighten",
                    &asset_class,
                    &event_subtype,
                    &shape,
                    &source_bucket,
                    "min_order_or_budget",
                    format!(
                        "{scope_label} 最近经常连最小有效下单量都够不到，说明 sizing 对预算仍偏乐观。"
                    ),
                    support_count,
                );
            }
            ("gate_reject", "edge_below_threshold") => {
                push_scoped_hint(
                    &mut crypto_entry_tuning_hints,
                    "gate_reject",
                    "medium",
                    "先看 edge 门槛",
                    format!(
                        "{scope_label} 最近更像 alpha 不足，优先审视 min_edge 和概率校准，而不是先动流动性参数。"
                    ),
                    scope_label.clone(),
                    support_count,
                );
                push_scoped_override_suggestion(
                    &mut crypto_override_suggestions,
                    "entry",
                    "medium",
                    "min_edge_multiplier",
                    "loosen",
                    &asset_class,
                    &event_subtype,
                    &shape,
                    &source_bucket,
                    "edge_below_threshold",
                    format!("{scope_label} 最近主要因为 edge 不够进不去，先看 min_edge 是否过严。"),
                    support_count,
                );
            }
            ("gate_reject", "beyond_entry_horizon") => {
                push_scoped_hint(
                    &mut crypto_entry_tuning_hints,
                    "gate_reject",
                    "high",
                    "先看到期窗口",
                    format!(
                        "{scope_label} 最近更多市场因为超出新开仓期限窗口被直接过滤，先确认 `max_entry_days` 是否符合当前策略目标。"
                    ),
                    scope_label.clone(),
                    support_count,
                );
                push_scoped_override_suggestion(
                    &mut crypto_override_suggestions,
                    "entry",
                    "high",
                    "max_entry_days",
                    "raise",
                    &asset_class,
                    &event_subtype,
                    &shape,
                    &source_bucket,
                    "beyond_entry_horizon",
                    format!(
                        "{scope_label} 最近主要是期限过滤在起作用，不是 edge/spread/depth 本身的问题。"
                    ),
                    support_count,
                );
            }
            ("gate_reject", "spread_too_wide") => {
                push_scoped_hint(
                    &mut crypto_entry_tuning_hints,
                    "gate_reject",
                    "medium",
                    "先看 spread 与流动性",
                    format!(
                        "{scope_label} 最近主要卡在价差，优先确认这类市场是不是长期偏宽，再决定是否放松 max_spread。"
                    ),
                    scope_label.clone(),
                    support_count,
                );
                push_scoped_override_suggestion(
                    &mut crypto_override_suggestions,
                    "entry",
                    "medium",
                    "max_spread_multiplier",
                    "loosen",
                    &asset_class,
                    &event_subtype,
                    &shape,
                    &source_bucket,
                    "spread_too_wide",
                    format!("{scope_label} 最近主要因为 spread 过宽被挡掉，适合先审 max_spread。"),
                    support_count,
                );
            }
            ("gate_reject", "insufficient_depth_buffer") => {
                push_scoped_hint(
                    &mut crypto_entry_tuning_hints,
                    "gate_reject",
                    "medium",
                    "先看 depth 约束",
                    format!(
                        "{scope_label} 最近主要卡在 depth buffer，不像是纯 edge 不足，更像执行约束过紧。"
                    ),
                    scope_label.clone(),
                    support_count,
                );
                push_scoped_override_suggestion(
                    &mut crypto_override_suggestions,
                    "entry",
                    "medium",
                    "depth_ratio_multiplier",
                    "loosen",
                    &asset_class,
                    &event_subtype,
                    &shape,
                    &source_bucket,
                    "insufficient_depth_buffer",
                    format!(
                        "{scope_label} 最近反复卡在 depth buffer，优先看 depth_ratio_multiplier。"
                    ),
                    support_count,
                );
            }
            ("gate_reject", "insufficient_size_retention") => {
                push_scoped_hint(
                    &mut crypto_entry_tuning_hints,
                    "gate_reject",
                    "medium",
                    "先看 retained size 约束",
                    format!(
                        "{scope_label} 最近主要卡在 retained size，更像 execution quality 约束主导。"
                    ),
                    scope_label.clone(),
                    support_count,
                );
                push_scoped_override_suggestion(
                    &mut crypto_override_suggestions,
                    "entry",
                    "medium",
                    "size_retention_multiplier",
                    "loosen",
                    &asset_class,
                    &event_subtype,
                    &shape,
                    &source_bucket,
                    "insufficient_size_retention",
                    format!(
                        "{scope_label} 最近 retained-size 门槛偏严，优先看 size_retention_multiplier。"
                    ),
                    support_count,
                );
            }
            ("gate_scale", "scaled_for_depth_buffer") => {
                push_scoped_hint(
                    &mut crypto_entry_tuning_hints,
                    "gate_scale",
                    "medium",
                    "前门主要在预缩量",
                    format!(
                        "{scope_label} 最近更多是为了满足 depth buffer 被前置缩量，说明目标下单量对当前深度偏乐观。"
                    ),
                    scope_label.clone(),
                    support_count,
                );
                push_scoped_override_suggestion(
                    &mut crypto_override_suggestions,
                    "entry",
                    "medium",
                    "size_multiplier",
                    "tighten",
                    &asset_class,
                    &event_subtype,
                    &shape,
                    &source_bucket,
                    "scaled_for_depth_buffer",
                    format!(
                        "{scope_label} 最近经常因为 depth buffer 被预裁单，先收紧 size 更自然。"
                    ),
                    support_count,
                );
            }
            ("gate_scale", "scaled_for_size_retention") => {
                push_scoped_hint(
                    &mut crypto_entry_tuning_hints,
                    "gate_scale",
                    "medium",
                    "数量保真主导缩量",
                    format!(
                        "{scope_label} 最近更多是为了满足 retained size 被前置缩量，说明 sizing 对实际流动性仍偏乐观。"
                    ),
                    scope_label.clone(),
                    support_count,
                );
                push_scoped_override_suggestion(
                    &mut crypto_override_suggestions,
                    "entry",
                    "medium",
                    "size_multiplier",
                    "tighten",
                    &asset_class,
                    &event_subtype,
                    &shape,
                    &source_bucket,
                    "scaled_for_size_retention",
                    format!(
                        "{scope_label} 最近经常因为 retained size 被预缩量，先收紧 size 更贴近真实可执行量。"
                    ),
                    support_count,
                );
            }
            _ => {}
        }
    }

    let mut exit_bucket_reason_counts: std::collections::HashMap<
        (String, String, String, String),
        std::collections::HashMap<String, usize>,
    > = std::collections::HashMap::new();
    let mut exit_bucket_asset_counts: std::collections::HashMap<
        (String, String, String, String),
        std::collections::HashMap<String, usize>,
    > = std::collections::HashMap::new();
    for decision in &recent_exits {
        let asset_label = decision.asset.as_deref().unwrap_or_default();
        let key = (
            asset_class_for_label(Some(asset_label)).to_string(),
            normalized_event_subtype(decision.event_subtype.as_deref()).to_string(),
            normalized_shape_label(decision.market_type.as_deref()).to_string(),
            normalized_resolution_bucket(decision.days_to_resolution).to_string(),
        );
        *exit_bucket_reason_counts
            .entry(key.clone())
            .or_default()
            .entry(decision.reason.clone())
            .or_insert(0) += 1;
        if !asset_label.is_empty() {
            *exit_bucket_asset_counts
                .entry(key)
                .or_default()
                .entry(asset_label.to_string())
                .or_insert(0) += 1;
        }
    }
    let mut exit_bucket_actions = Vec::new();
    for ((asset_class, event_subtype, shape, source_bucket), reason_counts_by_bucket) in
        exit_bucket_reason_counts
    {
        let Some((dominant_reason, support_count)) =
            dominant_bucket_reason(&reason_counts_by_bucket)
        else {
            continue;
        };
        let top_asset = exit_bucket_asset_counts
            .get(&(
                asset_class.clone(),
                event_subtype.clone(),
                shape.clone(),
                source_bucket.clone(),
            ))
            .and_then(top_count_entry)
            .and_then(|value| {
                value
                    .get("label")
                    .and_then(|label| label.as_str())
                    .map(ToString::to_string)
            })
            .unwrap_or_else(|| asset_class.clone());
        exit_bucket_actions.push((
            asset_class,
            event_subtype,
            shape,
            source_bucket,
            dominant_reason,
            support_count,
            top_asset,
        ));
    }
    exit_bucket_actions.sort_by(|a, b| {
        b.5.cmp(&a.5)
            .then_with(|| a.0.cmp(&b.0))
            .then_with(|| a.1.cmp(&b.1))
            .then_with(|| a.2.cmp(&b.2))
            .then_with(|| a.3.cmp(&b.3))
    });
    for (
        asset_class,
        event_subtype,
        shape,
        source_bucket,
        dominant_reason,
        support_count,
        top_asset,
    ) in exit_bucket_actions.into_iter().take(4)
    {
        let scope_label = shaped_scope_label(&asset_class, &event_subtype, &shape);
        match dominant_reason.as_str() {
            "edge_decay" => {
                push_scoped_hint(
                    &mut crypto_post_entry_tuning_hints,
                    "post_entry",
                    "medium",
                    "持仓后 edge 衰减偏快",
                    format!(
                        "{scope_label} 最近更多由 {top_asset} 的 edge_decay 触发，说明 entry 后 edge 保持性偏弱。"
                    ),
                    scope_label.clone(),
                    support_count,
                );
                push_scoped_override_suggestion(
                    &mut crypto_post_entry_override_suggestions,
                    "post_entry",
                    "medium",
                    "hold_edge_multiplier",
                    "tighten",
                    &asset_class,
                    &event_subtype,
                    &shape,
                    &source_bucket,
                    "edge_decay",
                    format!("{scope_label} 最近频繁 edge_decay，适合先提高 hold-edge 要求。"),
                    support_count,
                );
            }
            "capital_efficiency" => {
                push_scoped_hint(
                    &mut crypto_post_entry_tuning_hints,
                    "post_entry",
                    "medium",
                    "资金效率退出偏多",
                    format!(
                        "{scope_label} 最近更多由 {top_asset} 的 capital_efficiency 离场，说明这类仓位更像短拿而非继续持有。"
                    ),
                    scope_label.clone(),
                    support_count,
                );
                push_scoped_override_suggestion(
                    &mut crypto_post_entry_override_suggestions,
                    "post_entry",
                    "medium",
                    "capital_efficiency_multiplier",
                    "tighten",
                    &asset_class,
                    &event_subtype,
                    &shape,
                    &source_bucket,
                    "capital_efficiency",
                    format!(
                        "{scope_label} 最近资金效率退出偏多，适合让这类仓位更早承认 capital efficiency。"
                    ),
                    support_count,
                );
            }
            "model_reversal" => {
                push_scoped_hint(
                    &mut crypto_post_entry_tuning_hints,
                    "post_entry",
                    "medium",
                    "模型反转退出偏多",
                    format!(
                        "{scope_label} 最近更多由 {top_asset} 的 model_reversal 离场，说明 reversal buffer 可能偏宽。"
                    ),
                    scope_label.clone(),
                    support_count,
                );
                push_scoped_override_suggestion(
                    &mut crypto_post_entry_override_suggestions,
                    "post_entry",
                    "medium",
                    "model_reversal_buffer_multiplier",
                    "tighten",
                    &asset_class,
                    &event_subtype,
                    &shape,
                    &source_bucket,
                    "model_reversal",
                    format!("{scope_label} 最近模型反转退出偏多，适合先收紧 reversal buffer。"),
                    support_count,
                );
            }
            "relative_stop_loss" => {
                push_scoped_hint(
                    &mut crypto_post_entry_tuning_hints,
                    "post_entry",
                    "high",
                    "止损退出偏多",
                    format!(
                        "{scope_label} 最近更多由 {top_asset} 的 relative_stop_loss 离场，说明 entry 后下行容忍度可能过高。"
                    ),
                    scope_label.clone(),
                    support_count,
                );
                push_scoped_override_suggestion(
                    &mut crypto_post_entry_override_suggestions,
                    "post_entry",
                    "high",
                    "size_multiplier",
                    "tighten",
                    &asset_class,
                    &event_subtype,
                    &shape,
                    &source_bucket,
                    "relative_stop_loss",
                    format!(
                        "{scope_label} 最近止损退出偏多，先收紧 size 往往比继续放宽 exits 更稳。"
                    ),
                    support_count,
                );
            }
            _ => {}
        }
    }

    crypto_entry_tuning_hints.sort_by(|a, b| {
        let a_priority = priority_rank(
            a.get("priority")
                .and_then(|value| value.as_str())
                .unwrap_or(""),
        );
        let b_priority = priority_rank(
            b.get("priority")
                .and_then(|value| value.as_str())
                .unwrap_or(""),
        );
        let a_support = a
            .get("support_count")
            .and_then(|value| value.as_u64())
            .unwrap_or(0);
        let b_support = b
            .get("support_count")
            .and_then(|value| value.as_u64())
            .unwrap_or(0);
        b_priority
            .cmp(&a_priority)
            .then_with(|| b_support.cmp(&a_support))
    });
    crypto_override_suggestions.sort_by(|a, b| {
        let a_priority = priority_rank(
            a.get("priority")
                .and_then(|value| value.as_str())
                .unwrap_or(""),
        );
        let b_priority = priority_rank(
            b.get("priority")
                .and_then(|value| value.as_str())
                .unwrap_or(""),
        );
        let a_support = a
            .get("support_count")
            .and_then(|value| value.as_u64())
            .unwrap_or(0);
        let b_support = b
            .get("support_count")
            .and_then(|value| value.as_u64())
            .unwrap_or(0);
        b_priority
            .cmp(&a_priority)
            .then_with(|| b_support.cmp(&a_support))
    });
    crypto_post_entry_tuning_hints.sort_by(|a, b| {
        let a_priority = priority_rank(
            a.get("priority")
                .and_then(|value| value.as_str())
                .unwrap_or(""),
        );
        let b_priority = priority_rank(
            b.get("priority")
                .and_then(|value| value.as_str())
                .unwrap_or(""),
        );
        let a_support = a
            .get("support_count")
            .and_then(|value| value.as_u64())
            .unwrap_or(0);
        let b_support = b
            .get("support_count")
            .and_then(|value| value.as_u64())
            .unwrap_or(0);
        b_priority
            .cmp(&a_priority)
            .then_with(|| b_support.cmp(&a_support))
    });
    crypto_post_entry_override_suggestions.sort_by(|a, b| {
        let a_priority = priority_rank(
            a.get("priority")
                .and_then(|value| value.as_str())
                .unwrap_or(""),
        );
        let b_priority = priority_rank(
            b.get("priority")
                .and_then(|value| value.as_str())
                .unwrap_or(""),
        );
        let a_support = a
            .get("support_count")
            .and_then(|value| value.as_u64())
            .unwrap_or(0);
        let b_support = b
            .get("support_count")
            .and_then(|value| value.as_u64())
            .unwrap_or(0);
        b_priority
            .cmp(&a_priority)
            .then_with(|| b_support.cmp(&a_support))
    });
    let crypto_override_patch_preview = render_override_patch_preview(&crypto_override_suggestions);
    let crypto_post_entry_override_patch_preview =
        render_override_patch_preview(&crypto_post_entry_override_suggestions);
    let recent_crypto_patch_exports = crate::diagnostics::recent_crypto_override_patch_exports();
    let crypto_auto_patch_effectiveness_summary = if let Some(repo) = &state.repository {
        let positions = state.positions.read().await.clone();
        let trade_rows = repo
            .load_trade_history(500, Some("crypto_alpha"), None, None)
            .await
            .unwrap_or_default();
        let current_scope_scores =
            build_current_cooldown_scope_scores(&crypto_cooldown_buckets, &trade_rows, &positions);
        let audit_rows = repo
            .load_config_history("crypto_override_patch", Some("crypto_override_patch_"), 50)
            .await
            .unwrap_or_default();
        let effectiveness_entries = build_crypto_auto_patch_effectiveness_entries(
            audit_rows,
            &trade_rows,
            &positions,
            &recent_exits,
        );
        let patches: Vec<_> = effectiveness_entries
            .iter()
            .take(8)
            .map(|entry| {
                let effective_streak =
                    scope_effective_streak(&entry.scope_labels, &effectiveness_entries);
                let recommended_action = match entry.outcome {
                    "effective" if effective_streak >= 3 && entry.current_open_positions == 0 => {
                        "consider_relax"
                    }
                    "effective" => "hold",
                    "retain_or_tighten" => "continue_tighten",
                    _ => "observe",
                };
                let (
                    current_priority_score,
                    current_cooldown_severity_score,
                    current_window_pressure_score,
                ) = entry
                    .scope_labels
                    .iter()
                    .filter_map(|scope_label| current_scope_scores.get(scope_label))
                    .copied()
                    .max_by_key(|(priority, _, _)| *priority)
                    .unwrap_or((0, 0, 0));
                json!({
                    "runtime_applied_at": entry.runtime_applied_at.to_rfc3339(),
                    "mode": entry.mode,
                    "filename": entry.filename,
                    "export_sha": entry.export_sha,
                    "scope_labels": entry.scope_labels,
                    "post_apply_bad_exit_count": entry.post_apply_bad_exit_count,
                    "post_apply_realized_pnl": entry.post_apply_realized_pnl,
                    "current_open_positions": entry.current_open_positions,
                    "current_open_pnl_bid": entry.current_open_pnl_bid,
                    "outcome": entry.outcome,
                    "effective_streak": effective_streak,
                    "recommended_action": recommended_action,
                    "current_priority_score": current_priority_score,
                    "current_cooldown_severity_score": current_cooldown_severity_score,
                    "current_window_pressure_score": current_window_pressure_score,
                })
            })
            .collect();
        json!({
            "recent_count": patches.len(),
            "patches": patches,
        })
    } else {
        json!({
            "recent_count": 0,
            "patches": [],
        })
    };
    let crypto_bucket_window_summary = if let Some(repo) = &state.repository {
        let positions = state.positions.read().await.clone();
        let trade_rows = repo
            .load_trade_history(500, Some("crypto_alpha"), None, None)
            .await
            .unwrap_or_default();
        let rows = build_crypto_bucket_window_summary(&trade_rows, &positions, &recent_exits)
            .into_iter()
            .map(|entry| {
                json!({
                    "window_label": entry.window_label,
                    "resolution_bucket": entry.resolution_bucket,
                    "shape": entry.shape,
                    "asset_class": entry.asset_class,
                    "trade_count": entry.trade_count,
                    "realized_pnl": entry.realized_pnl,
                    "open_positions": entry.open_positions,
                    "open_pnl_bid": entry.open_pnl_bid,
                    "bad_exit_count": entry.bad_exit_count,
                })
            })
            .collect::<Vec<_>>();
        json!({
            "row_count": rows.len(),
            "rows": rows,
        })
    } else {
        json!({
            "row_count": 0,
            "rows": [],
        })
    };

    if top_count(&gate_scale_reason_counts_view) > top_count(&reason_counts)
        && !gate_scale_reason_counts_view.is_empty()
    {
        push_hint(
            &mut crypto_entry_tuning_hints,
            "summary",
            "low",
            "当前前门摩擦以缩量为主",
            "近期更多候选是被前置裁小而不是直接拒绝，说明参数更可能偏向执行质量约束，而不是纯 edge/spread 前门过严。".into(),
        );
    }

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
        "wallet_balance": wallet_balance,
        "total_wallet_positions_market_value_bid": total_wallet_positions_market_value_bid,
        "total_wallet_positions_market_value_mid": total_wallet_positions_market_value_mid,
        "total_wallet_portfolio_value_bid": total_wallet_portfolio_value_bid,
        "total_wallet_portfolio_value_mid": total_wallet_portfolio_value_mid,
        "strategy_financials": strategy_financials,
        "positions_snapshot_updated_at": positions_updated_at.map(|ts| ts.to_rfc3339()),
        "crypto_gate_reject_summary": {
            "recent_count": recent_gate_rejects.len(),
            "top_reason": top_reason,
            "top_asset": top_asset,
            "top_subtype": top_subtype,
            "reason_counts": reason_counts,
            "asset_counts": asset_counts,
            "subtype_counts": subtype_counts,
            "reason_windows": reason_windows,
            "subtype_windows": subtype_windows,
            "asset_windows": asset_windows,
            "reason_details": reason_details,
        },
        "crypto_gate_scale_summary": {
            "recent_count": recent_gate_scales.len(),
            "top_reason": gate_scale_reason_counts
                .iter()
                .max_by_key(|(_, count)| *count)
                .map(|(reason, count)| json!({"label": reason, "count": count})),
            "top_asset": gate_scale_asset_counts
                .iter()
                .max_by_key(|(_, count)| *count)
                .map(|(asset, count)| json!({"label": asset, "count": count})),
            "top_subtype": gate_scale_subtype_counts
                .iter()
                .max_by_key(|(_, count)| *count)
                .map(|(subtype, count)| json!({"label": subtype, "count": count})),
            "reason_counts": gate_scale_reason_counts_view,
            "asset_counts": gate_scale_asset_counts_view,
            "subtype_counts": gate_scale_subtype_counts_view,
            "reason_windows": {
                "recent_8": count_reason_window(&recent_gate_scales, 8),
                "recent_24": count_reason_window(&recent_gate_scales, 24),
            },
            "subtype_windows": {
                "recent_8": count_subtype_window(&recent_gate_scales, 8),
                "recent_24": count_subtype_window(&recent_gate_scales, 24),
            },
            "asset_windows": {
                "recent_8": count_asset_window(&recent_gate_scales, 8),
                "recent_24": count_asset_window(&recent_gate_scales, 24),
            },
            "reason_details": gate_scale_reason_details,
        },
        "crypto_cooldown_summary": {
            "active_count": crypto_cooldown_buckets.len(),
            "buckets": crypto_cooldown_buckets,
        },
        "crypto_entry_tuning_hints": crypto_entry_tuning_hints,
        "crypto_override_suggestions": crypto_override_suggestions,
        "crypto_override_patch_preview": crypto_override_patch_preview,
        "crypto_post_entry_tuning_hints": crypto_post_entry_tuning_hints,
        "crypto_post_entry_override_suggestions": crypto_post_entry_override_suggestions,
        "crypto_post_entry_override_patch_preview": crypto_post_entry_override_patch_preview,
        "crypto_override_patch_export_audit": recent_crypto_patch_exports,
        "crypto_auto_patch_effectiveness_summary": crypto_auto_patch_effectiveness_summary,
        "crypto_bucket_window_summary": crypto_bucket_window_summary,
        "smart_money_signal_summary": {
            "recent_signal_count": smart_money_recent_entries.len(),
            "recent_entry_attempts": smart_money_recent_entries.len(),
            "recent_entry_accepted": smart_money_recent_entries.iter().filter(|decision| decision.accepted).count(),
            "recent_entry_rejected": smart_money_recent_rejections.len(),
            "wallet_counts": smart_money_wallet_counts,
            "source_counts": smart_money_source_counts,
        },
        "smart_money_gate_reject_summary": {
            "total_rejected": smart_money_recent_rejections.len(),
            "reason_counts": smart_money_reason_counts,
        },
        "smart_money_route_summary": {
            "configured_routes": smart_money_runtime_config.leader_routes.len(),
            "route_mismatch_rejections": smart_money_recent_rejections
                .iter()
                .filter(|decision| decision.reject_reason.as_deref() == Some("route_mismatch"))
                .count(),
        },
        "smart_money_exit_summary": {
            "total_exits": recent_smart_money_exits.len(),
            "reason_counts": sorted_count_entries(&smart_money_exit_reason_counts),
        },
        "smart_money_leader_discovery_summary": {
            "candidate_count": smart_money_leader_candidates.len(),
            "top_candidates": smart_money_leader_candidates
                .iter()
                .take(8)
                .map(|row| smart_money_leader_candidate_json(row, smart_money_runtime_config.as_ref()))
                .collect::<Vec<_>>(),
        },
        "smart_money_leader_attribution_summary": {
            "top_leaders": smart_money_top_leaders.into_iter().take(10).collect::<Vec<_>>(),
        },
        "smart_money_leader_pnl_attribution_summary": {
            "top_leaders": smart_money_leader_pnl_attribution
                .into_iter()
                .take(10)
                .map(|entry| json!({
                    "leader": entry.leader,
                    "estimated_open_size": entry.estimated_open_size,
                    "estimated_exited_size": entry.estimated_exited_size,
                    "estimated_realized_pnl": entry.estimated_realized_pnl,
                    "estimated_exit_count": entry.estimated_exit_count,
                }))
                .collect::<Vec<_>>(),
        },
        "smart_money_trade_attribution_summary": {
            "top_leaders": smart_money_trade_attribution_summary,
        },
        "smart_money_leader_health_summary": {
            "top_leaders": leader_health_rows
                .into_iter()
                .take(10)
                .map(|entry| serde_json::to_value(entry).unwrap_or_else(|_| json!({})))
                .collect::<Vec<_>>(),
        },
        "smart_money_review_queue_summary": {
            "pending_count": review_queue_rows.iter().filter(|row| row.actionable).count(),
            "action_counts": sorted_count_entries(&review_queue_action_counts),
            "top_actions": review_queue_rows
                .into_iter()
                .take(10)
                .map(|entry| serde_json::to_value(entry).unwrap_or_else(|_| json!({})))
                .collect::<Vec<_>>(),
        },
        "smart_money_wallet_scores": smart_money_wallet_scores,
        "smart_money_recent_decisions": smart_money_recent_decisions,
        "smart_money_recent_exits": smart_money_recent_exits,
        "accounts": account_status,
    }))
}

/// Query params for GET /api/positions.
#[derive(Debug, Deserialize)]
struct PositionsQuery {
    strategy: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TradesQuery {
    limit: Option<usize>,
    strategy: Option<String>,
    account_name: Option<String>,
    proxy_wallet: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CryptoOverridePatchQuery {
    mode: Option<String>,
    bucket: Option<String>,
    shape: Option<String>,
    format: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ApplyCryptoOverridePatchRequest {
    action: Option<String>,
    mode: String,
    filename: String,
    export_sha: String,
    toml: String,
    scope_label: Option<String>,
    scope_labels: Option<Vec<String>>,
    generated_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CryptoOverridePatchToml {
    crypto_alpha: CryptoOverridePatchCryptoAlpha,
}

#[derive(Debug, Deserialize)]
struct CryptoOverridePatchCryptoAlpha {
    #[serde(default)]
    calibration_overrides: Vec<pa_core::config::CryptoCalibrationOverride>,
}

#[derive(Debug, Clone)]
struct GeneratedCryptoOverridePatch {
    mode: String,
    filename: String,
    export_sha: String,
    generated_at: String,
    scope_label: Option<String>,
    scope_labels: Vec<String>,
    toml: String,
    selected_bucket_count: usize,
    entry_row_count: usize,
    post_entry_row_count: usize,
}

#[derive(Debug, Deserialize)]
struct PromoteSmartMoneyLeaderRequest {
    address: String,
}

#[derive(Debug, Deserialize)]
struct BlockSmartMoneyLeaderRequest {
    address: String,
}

#[derive(Debug, Deserialize)]
struct DegradeSmartMoneyLeaderRequest {
    address: String,
    multiplier: Option<Decimal>,
}

#[derive(Debug, Deserialize)]
struct RestoreSmartMoneyLeaderRequest {
    address: String,
}

#[derive(Debug, Deserialize)]
struct ApplySmartMoneyLeaderRouteTemplateRequest {
    address: String,
    template: String,
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

/// GET /api/crypto/decisions — recent crypto same-asset candidate decisions.
async fn get_crypto_candidate_decisions() -> Json<Value> {
    Json(json!(
        crate::diagnostics::recent_crypto_candidate_decisions()
    ))
}

/// GET /api/crypto/exits — recent crypto exit decisions.
async fn get_crypto_exit_decisions() -> Json<Value> {
    Json(json!(crate::diagnostics::recent_crypto_exit_decisions()))
}

fn format_optional_bytes_hex(value: Option<&[u8]>) -> Option<String> {
    value.and_then(|bytes| {
        if bytes.len() == 32 {
            Some(format!("{:#x}", alloy::primitives::B256::from_slice(bytes)))
        } else {
            None
        }
    })
}

fn format_optional_tx_hash(value: Option<&[u8]>) -> Option<String> {
    value.and_then(|bytes| {
        if bytes.len() == 32 {
            Some(format!("{:#x}", alloy::primitives::B256::from_slice(bytes)))
        } else {
            None
        }
    })
}

async fn load_trade_history_response(
    state: &ApiState,
    strategy: Option<&str>,
    account_name: Option<&str>,
    proxy_wallet: Option<&str>,
    limit: usize,
) -> (axum::http::StatusCode, Json<Value>) {
    let Some(repo) = &state.repository else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "Trade history database not configured"})),
        );
    };

    match repo
        .load_trade_history(limit as i64, strategy, account_name, proxy_wallet)
        .await
    {
        Ok(rows) => {
            let entries: Vec<TradeHistoryApiEntry> =
                rows.into_iter()
                    .map(|row| TradeHistoryApiEntry {
                        trade_id: row.id.to_string(),
                        opportunity_id: row.opportunity_id.map(|id| id.to_string()),
                        order_id: row.order_id,
                        token_id: row.token_id,
                        side: row.side,
                        price: row.price,
                        size: row.size,
                        filled_size: row.filled_size,
                        fee: row.fee,
                        tx_type: row.tx_type,
                        tx_hash: format_optional_tx_hash(row.tx_hash.as_deref()),
                        status: row.status,
                        created_at: row.created_at,
                        strategy: row.strategy_type,
                        condition_id: format_optional_bytes_hex(row.condition_id.as_deref()),
                        question: row.question,
                        account_name: row.account_name,
                        proxy_wallet: row.proxy_wallet,
                        opportunity_status: row.opportunity_status,
                        estimated_profit: row.estimated_profit,
                        actual_profit: row.actual_profit,
                        detected_at: row.detected_at,
                        executed_at: row.executed_at,
                        smart_money_attribution: row
                            .details
                            .as_ref()
                            .and_then(|details| details.get("smart_money_attribution").cloned()),
                        smart_money_trade_attribution: row.trade_details.as_ref().and_then(
                            |details| details.get("smart_money_trade_attribution").cloned(),
                        ),
                    })
                    .collect();
            (
                axum::http::StatusCode::OK,
                Json(serde_json::to_value(entries).unwrap_or(json!([]))),
            )
        }
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("Failed to load trade history: {e}")})),
        ),
    }
}

async fn load_smart_money_trade_attribution_totals(
    state: &ApiState,
    limit: usize,
) -> anyhow::Result<std::collections::HashMap<String, SmartMoneyTradeAttributionTotals>> {
    let Some(repo) = &state.repository else {
        return Ok(std::collections::HashMap::new());
    };

    let rows = repo
        .load_trade_history(limit as i64, Some("smart_money"), None, None)
        .await?;
    let mut by_leader: std::collections::HashMap<String, SmartMoneyTradeAttributionTotals> =
        std::collections::HashMap::new();

    for row in rows {
        let Some(details) = row.trade_details else {
            continue;
        };
        let Some(attribution) = details.get("smart_money_trade_attribution") else {
            continue;
        };
        let Ok(slices) =
            serde_json::from_value::<Vec<SmartMoneyTradeAttributionSlice>>(attribution.clone())
        else {
            continue;
        };
        for slice in slices {
            let entry = by_leader.entry(slice.leader).or_default();
            entry.actual_filled_size += slice.actual_filled_size;
            entry.actual_fee += slice.actual_fee;
            entry.actual_realized_profit += slice.actual_realized_profit;
            entry.trade_count += 1;
        }
    }

    Ok(by_leader)
}

fn smart_money_trade_attribution_summary_json(
    totals: &std::collections::HashMap<String, SmartMoneyTradeAttributionTotals>,
) -> Vec<Value> {
    let mut leaders: Vec<Value> = totals
        .iter()
        .map(|(leader, totals)| {
            json!({
                "leader": leader,
                "actual_filled_size": totals.actual_filled_size,
                "actual_fee": totals.actual_fee,
                "actual_realized_profit": totals.actual_realized_profit,
                "trade_count": totals.trade_count,
            })
        })
        .collect();
    leaders.sort_by(|a, b| {
        let pnl_a = a
            .get("actual_realized_profit")
            .and_then(|value| value.as_str())
            .unwrap_or("0")
            .parse::<Decimal>()
            .unwrap_or(Decimal::ZERO);
        let pnl_b = b
            .get("actual_realized_profit")
            .and_then(|value| value.as_str())
            .unwrap_or("0")
            .parse::<Decimal>()
            .unwrap_or(Decimal::ZERO);
        let count_a = a
            .get("trade_count")
            .and_then(|value| value.as_u64())
            .unwrap_or(0);
        let count_b = b
            .get("trade_count")
            .and_then(|value| value.as_u64())
            .unwrap_or(0);
        pnl_b.cmp(&pnl_a).then_with(|| count_b.cmp(&count_a))
    });
    leaders.truncate(10);
    leaders
}

fn smart_money_health_action(
    signals: usize,
    accept_rate: f64,
    estimated_realized_pnl: Decimal,
    actual_realized_profit: Decimal,
    trade_count: usize,
) -> (&'static str, String) {
    if trade_count >= 5
        && actual_realized_profit <= Decimal::from(-5)
        && signals >= 5
        && accept_rate < 0.25
    {
        return (
            "block_candidate",
            format!(
                "filled PnL {} across {} trades with accept rate {:.0}%",
                actual_realized_profit.round_dp(2),
                trade_count,
                accept_rate * 100.0
            ),
        );
    }
    if (trade_count >= 3 && actual_realized_profit < Decimal::from(-2))
        || (signals >= 6 && accept_rate < 0.25)
    {
        return (
            "degrade",
            format!(
                "filled PnL {} across {} trades / accept rate {:.0}%",
                actual_realized_profit.round_dp(2),
                trade_count,
                accept_rate * 100.0
            ),
        );
    }
    if trade_count >= 3 && actual_realized_profit > Decimal::from(2) && accept_rate >= 0.5 {
        return (
            "keep_or_promote",
            format!(
                "filled PnL {} across {} trades with accept rate {:.0}%",
                actual_realized_profit.round_dp(2),
                trade_count,
                accept_rate * 100.0
            ),
        );
    }
    if estimated_realized_pnl != Decimal::ZERO {
        return (
            "observe",
            format!(
                "estimated PnL {} with limited filled-trade evidence",
                estimated_realized_pnl.round_dp(2)
            ),
        );
    }
    (
        "observe",
        "not enough filled trade evidence yet".to_string(),
    )
}

fn smart_money_health_action_rank(action: &str) -> usize {
    match action {
        "block_candidate" => 4,
        "degrade" => 3,
        "observe" => 2,
        "keep_or_promote" => 1,
        _ => 0,
    }
}

/// GET /api/trades — recent persisted trade history with optional strategy/account filters.
async fn get_trades(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<TradesQuery>,
) -> (axum::http::StatusCode, Json<Value>) {
    let limit = query.limit.unwrap_or(100).min(500);
    load_trade_history_response(
        state.as_ref(),
        query.strategy.as_deref(),
        query.account_name.as_deref(),
        query.proxy_wallet.as_deref(),
        limit,
    )
    .await
}

/// GET /api/crypto/trades — recent crypto trade history.
async fn get_crypto_trades(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<TradesQuery>,
) -> (axum::http::StatusCode, Json<Value>) {
    let limit = query.limit.unwrap_or(100).min(500);
    load_trade_history_response(
        state.as_ref(),
        Some("crypto_alpha"),
        query.account_name.as_deref(),
        query.proxy_wallet.as_deref(),
        limit,
    )
    .await
}

fn infer_trade_shape(question: Option<&str>) -> &'static str {
    let question = question.unwrap_or_default();
    let tail = question
        .split('→')
        .next_back()
        .map(str::trim)
        .unwrap_or(question);
    if tail.contains(" - ") || tail.to_lowercase().contains("between") {
        "range"
    } else {
        "directional"
    }
}

fn infer_question_event_subtype(question: Option<&str>) -> &'static str {
    let text = question.unwrap_or_default().to_lowercase();
    if text.contains("unlock") {
        "unlock"
    } else if text.contains("upgrade") {
        "upgrade"
    } else if text.contains("regulatory")
        || text.contains("regulation")
        || text.contains("sec ")
        || text.contains(" sec")
        || text.contains("etf")
    {
        "regulatory"
    } else {
        "any"
    }
}

fn infer_trade_asset(question: Option<&str>) -> &'static str {
    let text = question.unwrap_or_default().to_lowercase();
    if text.contains("bitcoin") {
        "Bitcoin"
    } else if text.contains("ethereum") {
        "Ethereum"
    } else if text.contains("solana") {
        "Solana"
    } else if text.contains("dogecoin") {
        "Dogecoin"
    } else if text.contains("xrp") {
        "XRP"
    } else {
        ""
    }
}

fn infer_trade_resolution_bucket(
    question: Option<&str>,
    executed_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
) -> &'static str {
    let question = question.unwrap_or_default();
    let tail = question
        .split('→')
        .next_back()
        .map(str::trim)
        .unwrap_or(question);
    let reference = executed_at.unwrap_or(created_at).date_naive();
    for month in [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ] {
        let Some(start_idx) = tail.find(month) else {
            continue;
        };
        let candidate: String = tail[start_idx..]
            .chars()
            .take_while(|ch| ch.is_ascii_alphanumeric() || matches!(ch, ' ' | ','))
            .collect();
        if let Ok(target_date) = chrono::NaiveDate::parse_from_str(candidate.trim(), "%B %d, %Y") {
            let days = (target_date - reference).num_days();
            return if days <= 0 {
                "same_day"
            } else if days == 1 {
                "next_day"
            } else {
                "legacy"
            };
        }
    }
    "legacy"
}

fn infer_position_shape(position: &PositionApiEntry) -> &'static str {
    match position.direction.as_deref() {
        Some("inside_range") | Some("outside_range") => "range",
        _ => {
            if position
                .question
                .as_deref()
                .map(|q| q.contains(" - ") || q.to_lowercase().contains("between"))
                .unwrap_or(false)
            {
                "range"
            } else {
                "directional"
            }
        }
    }
}

fn asset_class_for_asset_label(asset: &str) -> &'static str {
    match asset.to_lowercase().as_str() {
        "bitcoin" | "ethereum" => "major",
        "" => "any",
        _ => "alt",
    }
}

fn normalized_event_subtype_label(subtype: Option<&str>) -> &'static str {
    match subtype.unwrap_or_default() {
        "unlock" => "unlock",
        "upgrade" => "upgrade",
        "regulatory" => "regulatory",
        _ => "any",
    }
}

fn normalized_resolution_bucket_label(days_to_resolution: Option<u32>) -> &'static str {
    match days_to_resolution {
        Some(0) => "same_day",
        Some(1) => "next_day",
        _ => "legacy",
    }
}

fn normalized_market_shape_label(market_type: Option<&str>) -> &'static str {
    match market_type.unwrap_or_default() {
        "range" => "range",
        _ => "directional",
    }
}

fn patch_preview_multiplier_for(target_field: &str, direction: &str) -> Option<&'static str> {
    match (target_field, direction) {
        ("min_edge_multiplier", "loosen") => Some("0.95"),
        ("min_edge_multiplier", "tighten") => Some("1.05"),
        ("max_spread_multiplier", "loosen") => Some("1.05"),
        ("max_spread_multiplier", "tighten") => Some("0.95"),
        ("size_multiplier", "loosen") => Some("1.10"),
        ("size_multiplier", "tighten") => Some("0.90"),
        ("depth_ratio_multiplier", "loosen") => Some("0.95"),
        ("depth_ratio_multiplier", "tighten") => Some("1.05"),
        ("size_retention_multiplier", "loosen") => Some("0.95"),
        ("size_retention_multiplier", "tighten") => Some("1.05"),
        ("hold_edge_multiplier", "loosen") => Some("0.95"),
        ("hold_edge_multiplier", "tighten") => Some("1.05"),
        ("capital_efficiency_multiplier", "loosen") => Some("1.05"),
        ("capital_efficiency_multiplier", "tighten") => Some("0.95"),
        ("model_reversal_buffer_multiplier", "loosen") => Some("1.05"),
        ("model_reversal_buffer_multiplier", "tighten") => Some("0.95"),
        _ => None,
    }
}

fn global_bucket_scope_label(asset_class: &str, event_subtype: &str) -> String {
    match (asset_class, event_subtype) {
        ("any", "any") => "all".into(),
        (_, "any") => asset_class.to_string(),
        ("any", _) => event_subtype.to_string(),
        _ => format!("{asset_class} / {event_subtype}"),
    }
}

fn global_shaped_scope_label(asset_class: &str, event_subtype: &str, shape: &str) -> String {
    format!(
        "{} / {}",
        global_bucket_scope_label(asset_class, event_subtype),
        shape
    )
}

fn parse_shaped_scope_label(scope_label: &str) -> Option<(&str, &str, &str)> {
    let parts = scope_label.split(" / ").map(str::trim).collect::<Vec<_>>();
    match parts.as_slice() {
        [shape] if matches!(*shape, "range" | "directional") => Some(("any", "any", shape)),
        [first, shape] if matches!(*shape, "range" | "directional") => match *first {
            "all" => Some(("any", "any", shape)),
            "major" | "alt" | "any" => Some((first, "any", shape)),
            "unlock" | "upgrade" | "regulatory" => Some(("any", first, shape)),
            _ => None,
        },
        [asset_class, event_subtype, shape] if matches!(*shape, "range" | "directional") => {
            Some((asset_class, event_subtype, shape))
        }
        _ => None,
    }
}

fn bucketed_scope_label(
    resolution_bucket: &str,
    asset_class: &str,
    event_subtype: &str,
    shape: &str,
) -> String {
    format!(
        "{} / {}",
        resolution_bucket,
        global_shaped_scope_label(asset_class, event_subtype, shape)
    )
}

fn parse_bucketed_shaped_scope_label(
    scope_label: &str,
) -> Option<(String, String, String, String)> {
    let parts = scope_label.split(" / ").map(str::trim).collect::<Vec<_>>();
    match parts.as_slice() {
        [resolution_bucket, asset_class, event_subtype, shape]
            if matches!(*resolution_bucket, "same_day" | "next_day" | "legacy")
                && matches!(*shape, "range" | "directional") =>
        {
            Some((
                resolution_bucket.to_string(),
                asset_class.to_string(),
                event_subtype.to_string(),
                shape.to_string(),
            ))
        }
        [resolution_bucket, first, shape]
            if matches!(*resolution_bucket, "same_day" | "next_day" | "legacy")
                && matches!(*shape, "range" | "directional") =>
        {
            let (asset_class, event_subtype) = match *first {
                "all" => ("any".to_string(), "any".to_string()),
                "major" | "alt" | "any" => (first.to_string(), "any".to_string()),
                "unlock" | "upgrade" | "regulatory" => ("any".to_string(), first.to_string()),
                _ => return None,
            };
            Some((
                resolution_bucket.to_string(),
                asset_class,
                event_subtype,
                shape.to_string(),
            ))
        }
        _ => parse_shaped_scope_label(scope_label).map(|(asset_class, event_subtype, shape)| {
            (
                "same_day".to_string(),
                asset_class.to_string(),
                event_subtype.to_string(),
                shape.to_string(),
            )
        }),
    }
}

fn row_scope_key(row: &Value) -> Option<String> {
    let scope_label = row.get("scope_label").and_then(Value::as_str)?;
    let source_bucket = row
        .get("source_bucket")
        .and_then(Value::as_str)
        .unwrap_or("same_day");
    Some(bucketed_scope_label(
        source_bucket,
        parse_shaped_scope_label(scope_label)
            .map(|(asset_class, _, _)| asset_class)
            .unwrap_or("any"),
        parse_shaped_scope_label(scope_label)
            .map(|(_, event_subtype, _)| event_subtype)
            .unwrap_or("any"),
        parse_shaped_scope_label(scope_label)
            .map(|(_, _, shape)| shape)
            .unwrap_or("directional"),
    ))
}

fn build_crypto_auto_patch_effectiveness_entries(
    audit_rows: Vec<ConfigHistoryRow>,
    trade_rows: &[TradeHistoryRow],
    positions: &[PositionApiEntry],
    recent_exits: &[crate::diagnostics::CryptoExitDecision],
) -> Vec<CryptoAutoPatchEffectivenessEntry> {
    audit_rows
        .into_iter()
        .filter(|row| {
            row.changed_by == "crypto_override_patch_auto_apply_task"
                && row
                    .data
                    .get("runtime_applied")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
        })
        .filter_map(|row| {
            let applied_at = row
                .data
                .get("runtime_applied_at")
                .and_then(Value::as_str)
                .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
                .map(|value| value.with_timezone(&Utc))?;
            let scope_labels: Vec<String> = row
                .data
                .get("scope_labels")
                .and_then(Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_else(|| {
                    row.data
                        .get("scope_label")
                        .and_then(Value::as_str)
                        .map(|value| vec![value.to_string()])
                        .unwrap_or_default()
                });
            if scope_labels.is_empty() {
                return None;
            }
            let parsed_scopes = scope_labels
                .iter()
                .filter_map(|label| parse_bucketed_shaped_scope_label(label))
                .map(|(resolution_bucket, asset_class, event_subtype, shape)| {
                    (
                        resolution_bucket,
                        asset_class.to_string(),
                        event_subtype.to_string(),
                        shape.to_string(),
                    )
                })
                .collect::<Vec<_>>();
            if parsed_scopes.is_empty() {
                return None;
            }
            let post_apply_bad_exit_count = recent_exits
                .iter()
                .filter(|decision| {
                    decision.recorded_at >= applied_at
                        && parsed_scopes.iter().any(
                            |(resolution_bucket, asset_class, event_subtype, shape)| {
                                normalized_resolution_bucket_label(decision.days_to_resolution)
                                    == resolution_bucket
                                    && asset_class_for_asset_label(
                                        decision.asset.as_deref().unwrap_or_default(),
                                    ) == asset_class
                                    && normalized_event_subtype_label(
                                        decision.event_subtype.as_deref(),
                                    ) == event_subtype
                                    && normalized_market_shape_label(
                                        decision.market_type.as_deref(),
                                    ) == shape
                            },
                        )
                })
                .count();
            let post_apply_realized_pnl: Decimal = trade_rows
                .iter()
                .filter(|trade| {
                    let executed_at = trade.executed_at.unwrap_or(trade.created_at);
                    executed_at >= applied_at
                        && parsed_scopes.iter().any(
                            |(resolution_bucket, asset_class, event_subtype, shape)| {
                                infer_trade_resolution_bucket(
                                    trade.question.as_deref(),
                                    trade.executed_at,
                                    trade.created_at,
                                ) == resolution_bucket
                                    && asset_class_for_asset_label(infer_trade_asset(
                                        trade.question.as_deref(),
                                    )) == asset_class
                                    && infer_trade_shape(trade.question.as_deref()) == shape
                                    && (event_subtype == "any"
                                        || infer_question_event_subtype(trade.question.as_deref())
                                            == event_subtype)
                            },
                        )
                })
                .map(|trade| trade.actual_profit.unwrap_or(Decimal::ZERO))
                .sum();
            let current_open_positions = positions
                .iter()
                .filter(|position| {
                    parsed_scopes.iter().any(
                        |(resolution_bucket, asset_class, event_subtype, shape)| {
                            position.resolution_bucket.as_deref()
                                == Some(resolution_bucket.as_str())
                                && asset_class_for_asset_label(
                                    position.asset.as_deref().unwrap_or_default(),
                                ) == asset_class
                                && infer_position_shape(position) == shape
                                && (event_subtype == "any"
                                    || infer_question_event_subtype(position.question.as_deref())
                                        == event_subtype)
                        },
                    )
                })
                .count();
            let current_open_pnl_bid: Decimal = positions
                .iter()
                .filter(|position| {
                    parsed_scopes.iter().any(
                        |(resolution_bucket, asset_class, event_subtype, shape)| {
                            position.resolution_bucket.as_deref()
                                == Some(resolution_bucket.as_str())
                                && asset_class_for_asset_label(
                                    position.asset.as_deref().unwrap_or_default(),
                                ) == asset_class
                                && infer_position_shape(position) == shape
                                && (event_subtype == "any"
                                    || infer_question_event_subtype(position.question.as_deref())
                                        == event_subtype)
                        },
                    )
                })
                .map(|position| {
                    position
                        .unrealized_pnl_bid
                        .or(position.unrealized_pnl)
                        .unwrap_or(Decimal::ZERO)
                })
                .sum();
            let outcome = if post_apply_bad_exit_count > 0
                || post_apply_realized_pnl < Decimal::ZERO
                || current_open_pnl_bid < Decimal::ZERO
            {
                "retain_or_tighten"
            } else if post_apply_bad_exit_count == 0
                && post_apply_realized_pnl >= Decimal::ZERO
                && current_open_pnl_bid >= Decimal::ZERO
            {
                "effective"
            } else {
                "observe"
            };
            Some(CryptoAutoPatchEffectivenessEntry {
                runtime_applied_at: applied_at,
                mode: row
                    .data
                    .get("mode")
                    .and_then(Value::as_str)
                    .unwrap_or("cooldown_priority")
                    .to_string(),
                filename: row
                    .data
                    .get("filename")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                export_sha: row
                    .data
                    .get("export_sha")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                scope_labels,
                post_apply_bad_exit_count,
                post_apply_realized_pnl,
                current_open_positions,
                current_open_pnl_bid,
                outcome,
            })
        })
        .collect()
}

fn build_crypto_bucket_window_summary(
    trade_rows: &[TradeHistoryRow],
    positions: &[PositionApiEntry],
    recent_exits: &[crate::diagnostics::CryptoExitDecision],
) -> Vec<CryptoBucketWindowSummaryEntry> {
    let now = Utc::now();
    let windows = [
        ("1h", chrono::Duration::hours(1)),
        ("6h", chrono::Duration::hours(6)),
        ("24h", chrono::Duration::hours(24)),
    ];
    let buckets = ["same_day", "next_day"];
    let shapes = ["range", "directional"];
    let asset_classes = ["major", "alt"];
    let mut entries = Vec::new();

    for (window_label, window_duration) in windows {
        let window_start = now - window_duration;
        for resolution_bucket in buckets {
            for shape in shapes {
                for asset_class in asset_classes {
                    let trade_count = trade_rows
                        .iter()
                        .filter(|trade| {
                            let executed_at = trade.executed_at.unwrap_or(trade.created_at);
                            executed_at >= window_start
                                && infer_trade_resolution_bucket(
                                    trade.question.as_deref(),
                                    trade.executed_at,
                                    trade.created_at,
                                ) == resolution_bucket
                                && infer_trade_shape(trade.question.as_deref()) == shape
                                && asset_class_for_asset_label(infer_trade_asset(
                                    trade.question.as_deref(),
                                )) == asset_class
                        })
                        .count();
                    let realized_pnl: Decimal = trade_rows
                        .iter()
                        .filter(|trade| {
                            let executed_at = trade.executed_at.unwrap_or(trade.created_at);
                            executed_at >= window_start
                                && infer_trade_resolution_bucket(
                                    trade.question.as_deref(),
                                    trade.executed_at,
                                    trade.created_at,
                                ) == resolution_bucket
                                && infer_trade_shape(trade.question.as_deref()) == shape
                                && asset_class_for_asset_label(infer_trade_asset(
                                    trade.question.as_deref(),
                                )) == asset_class
                        })
                        .map(|trade| trade.actual_profit.unwrap_or(Decimal::ZERO))
                        .sum();
                    let bad_exit_count = recent_exits
                        .iter()
                        .filter(|decision| {
                            decision.recorded_at >= window_start
                                && normalized_resolution_bucket_label(decision.days_to_resolution)
                                    == resolution_bucket
                                && normalized_market_shape_label(decision.market_type.as_deref())
                                    == shape
                                && asset_class_for_asset_label(
                                    decision.asset.as_deref().unwrap_or_default(),
                                ) == asset_class
                                && matches!(
                                    decision.reason.as_str(),
                                    "model_reversal" | "relative_stop_loss"
                                )
                        })
                        .count();
                    let open_positions = positions
                        .iter()
                        .filter(|position| {
                            position.resolution_bucket.as_deref() == Some(resolution_bucket)
                                && infer_position_shape(position) == shape
                                && asset_class_for_asset_label(
                                    position.asset.as_deref().unwrap_or_default(),
                                ) == asset_class
                        })
                        .count();
                    let open_pnl_bid: Decimal = positions
                        .iter()
                        .filter(|position| {
                            position.resolution_bucket.as_deref() == Some(resolution_bucket)
                                && infer_position_shape(position) == shape
                                && asset_class_for_asset_label(
                                    position.asset.as_deref().unwrap_or_default(),
                                ) == asset_class
                        })
                        .map(|position| {
                            position
                                .unrealized_pnl_bid
                                .or(position.unrealized_pnl)
                                .unwrap_or(Decimal::ZERO)
                        })
                        .sum();

                    if trade_count == 0
                        && realized_pnl == Decimal::ZERO
                        && bad_exit_count == 0
                        && open_positions == 0
                        && open_pnl_bid == Decimal::ZERO
                    {
                        continue;
                    }

                    entries.push(CryptoBucketWindowSummaryEntry {
                        window_label,
                        resolution_bucket: resolution_bucket.to_string(),
                        shape: shape.to_string(),
                        asset_class: asset_class.to_string(),
                        trade_count,
                        realized_pnl,
                        open_positions,
                        open_pnl_bid,
                        bad_exit_count,
                    });
                }
            }
        }
    }

    entries.sort_by(|a, b| {
        let window_rank = |label: &str| match label {
            "1h" => 0,
            "6h" => 1,
            "24h" => 2,
            _ => 3,
        };
        window_rank(&a.window_label)
            .cmp(&window_rank(&b.window_label))
            .then_with(|| a.resolution_bucket.cmp(&b.resolution_bucket))
            .then_with(|| a.asset_class.cmp(&b.asset_class))
            .then_with(|| a.shape.cmp(&b.shape))
    });
    entries
}

fn scope_has_repeated_effective_auto_patches(
    scope_labels: &[String],
    entries: &[CryptoAutoPatchEffectivenessEntry],
    min_effective_count: usize,
) -> bool {
    !scope_labels.is_empty()
        && scope_labels.iter().all(|scope_label| {
            entries
                .iter()
                .filter(|entry| {
                    entry.outcome == "effective"
                        && entry.scope_labels.iter().any(|label| label == scope_label)
                })
                .take(min_effective_count)
                .count()
                >= min_effective_count
        })
}

fn scope_effective_streak(
    scope_labels: &[String],
    entries: &[CryptoAutoPatchEffectivenessEntry],
) -> usize {
    if scope_labels.is_empty() {
        return 0;
    }
    entries
        .iter()
        .filter(|entry| {
            entry.outcome == "effective"
                && scope_labels
                    .iter()
                    .all(|scope_label| entry.scope_labels.iter().any(|label| label == scope_label))
        })
        .count()
}

fn rewrite_patch_rows_direction(
    rows: &[Value],
    scope_labels: &std::collections::BTreeSet<String>,
    direction: &str,
) -> Vec<Value> {
    rows.iter()
        .filter_map(|row| {
            let scope_key = row_scope_key(row)?;
            if !scope_labels.contains(&scope_key) {
                return None;
            }
            let mut cloned = row.clone();
            if let Some(fields) = cloned.get_mut("fields").and_then(Value::as_array_mut) {
                let rewritten = fields
                    .iter()
                    .filter_map(|field| {
                        let target_field = field.get("target_field").and_then(Value::as_str)?;
                        let preview_value =
                            patch_preview_multiplier_for(target_field, direction)?.to_string();
                        Some(json!({
                            "target_field": target_field,
                            "direction": direction,
                            "source_reason": field.get("source_reason").and_then(Value::as_str).unwrap_or_default(),
                            "support_count": field.get("support_count").and_then(Value::as_u64).unwrap_or(0),
                            "preview_value": preview_value,
                        }))
                    })
                    .collect::<Vec<_>>();
                *fields = rewritten;
            }
            let field_count = cloned
                .get("fields")
                .and_then(Value::as_array)
                .map(|fields| fields.len())
                .unwrap_or(0);
            if field_count == 0 { None } else { Some(cloned) }
        })
        .collect()
}

fn render_patch_rows_to_toml(rows: &[Value]) -> String {
    rows.iter()
        .map(|row| {
            let selector_asset_class = row
                .get("selector_asset_class")
                .and_then(Value::as_str)
                .unwrap_or("any");
            let selector_event_subtype = row
                .get("selector_event_subtype")
                .and_then(Value::as_str)
                .unwrap_or("any");
            let market_type = row
                .get("market_type")
                .and_then(Value::as_str)
                .unwrap_or("binary");
            let resolution_bucket = row
                .get("resolution_bucket")
                .and_then(Value::as_str)
                .unwrap_or("same_day");
            let mut lines = vec![
                "[[crypto_alpha.calibration_overrides]]".to_string(),
                "asset = \"*\"".to_string(),
                format!("asset_class = \"{}\"", selector_asset_class),
                "horizon = \"short\"".to_string(),
                format!("resolution_bucket = \"{}\"", resolution_bucket),
                format!("market_type = \"{}\"", market_type),
                format!("event_subtype = \"{}\"", selector_event_subtype),
            ];
            if let Some(fields) = row.get("fields").and_then(Value::as_array) {
                for field in fields {
                    let target_field = field
                        .get("target_field")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let preview_value = field
                        .get("preview_value")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    if !target_field.is_empty() && !preview_value.is_empty() {
                        lines.push(format!("{target_field} = {preview_value}"));
                    }
                }
            }
            lines.join("\n")
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn patch_export_digest(toml: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(toml.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn annotate_patch_toml(toml: &str, filename: &str, export_sha: &str, generated_at: &str) -> String {
    let mut annotated = String::new();
    annotated.push_str(&format!("# filename = {}\n", filename));
    annotated.push_str(&format!("# export_sha = {}\n", export_sha));
    annotated.push_str(&format!("# generated_at = {}\n", generated_at));
    if !toml.trim().is_empty() {
        annotated.push('\n');
        annotated.push_str(toml);
    }
    annotated
}

fn patch_row_support_score(row: &Value) -> usize {
    row.get("fields")
        .and_then(Value::as_array)
        .map(|fields| {
            fields
                .iter()
                .map(|field| {
                    field
                        .get("support_count")
                        .and_then(Value::as_u64)
                        .unwrap_or(0) as usize
                })
                .sum()
        })
        .unwrap_or(0)
}

fn cooldown_kind_resolution_bucket(kind: &str) -> &'static str {
    match kind {
        "next_day_alt_range" => "next_day",
        _ => "same_day",
    }
}

fn decimal_loss_score(value: Decimal) -> i64 {
    if value < Decimal::ZERO {
        ((-value) * Decimal::from(1000))
            .round_dp(0)
            .to_i64()
            .unwrap_or(0)
    } else {
        0
    }
}

fn build_bucket_window_pressure_scores(
    entries: &[CryptoBucketWindowSummaryEntry],
) -> std::collections::HashMap<(String, String, String), i64> {
    let mut scores = std::collections::HashMap::new();
    for entry in entries {
        let window_weight = match entry.window_label {
            "1h" => 10_i64,
            "6h" => 4_i64,
            _ => 0_i64,
        };
        if window_weight == 0 {
            continue;
        }
        let score = (entry.bad_exit_count as i64) * 4_000 * window_weight
            + decimal_loss_score(entry.realized_pnl) * 5 * window_weight
            + decimal_loss_score(entry.open_pnl_bid) * 2 * window_weight;
        if score == 0 {
            continue;
        }
        *scores
            .entry((
                entry.resolution_bucket.clone(),
                entry.shape.clone(),
                entry.asset_class.clone(),
            ))
            .or_insert(0) += score;
    }
    scores
}

fn build_current_cooldown_scope_scores(
    cooldown_buckets: &[Value],
    trade_rows: &[TradeHistoryRow],
    positions: &[PositionApiEntry],
) -> std::collections::HashMap<String, (i64, i64, i64)> {
    let recent_exits = crate::diagnostics::recent_crypto_exit_decisions()
        .into_iter()
        .take(24)
        .collect::<Vec<_>>();
    let bucket_window_scores = build_bucket_window_pressure_scores(
        &build_crypto_bucket_window_summary(trade_rows, positions, &recent_exits),
    );
    let mut scores = std::collections::HashMap::new();

    for bucket in cooldown_buckets {
        let kind = bucket
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("same_day_range");
        let resolution_bucket = cooldown_kind_resolution_bucket(kind);
        let asset = bucket.get("asset").and_then(Value::as_str).unwrap_or("");
        let event_subtype = bucket
            .get("event_subtype")
            .and_then(Value::as_str)
            .unwrap_or("generic");
        let shape = bucket
            .get("shape")
            .and_then(Value::as_str)
            .unwrap_or("directional");
        let triggered_at = bucket
            .get("triggered_at")
            .and_then(Value::as_str)
            .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
            .map(|value| value.with_timezone(&Utc));
        let post_trigger_bad_exit_count = bucket
            .get("post_trigger_bad_exit_count")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let Some(triggered_at) = triggered_at else {
            continue;
        };
        let post_trigger_realized: Decimal = trade_rows
            .iter()
            .filter(|trade| {
                let trade_asset = infer_trade_asset(trade.question.as_deref());
                let trade_shape = infer_trade_shape(trade.question.as_deref());
                let trade_timestamp = trade.executed_at.unwrap_or(trade.created_at);
                trade_asset == asset
                    && trade_shape == shape
                    && (event_subtype == "any"
                        || infer_question_event_subtype(trade.question.as_deref()) == event_subtype)
                    && trade_timestamp >= triggered_at
                    && infer_trade_resolution_bucket(
                        trade.question.as_deref(),
                        trade.executed_at,
                        trade.created_at,
                    ) == resolution_bucket
            })
            .map(|trade| trade.actual_profit.unwrap_or(Decimal::ZERO))
            .sum();
        let open_pnl_bid: Decimal = positions
            .iter()
            .filter(|position| {
                position.resolution_bucket.as_deref() == Some(resolution_bucket)
                    && position.asset.as_deref() == Some(asset)
                    && infer_position_shape(position) == shape
                    && (event_subtype == "any"
                        || infer_question_event_subtype(position.question.as_deref())
                            == event_subtype)
            })
            .map(|position| {
                position
                    .unrealized_pnl_bid
                    .or(position.unrealized_pnl)
                    .unwrap_or(Decimal::ZERO)
            })
            .sum();
        let cooldown_severity_score = (post_trigger_bad_exit_count as i64) * 10_000
            + decimal_loss_score(post_trigger_realized) * 10
            + decimal_loss_score(open_pnl_bid);
        let window_pressure_score = bucket_window_scores
            .get(&(
                resolution_bucket.to_string(),
                shape.to_string(),
                asset_class_for_asset_label(asset).to_string(),
            ))
            .copied()
            .unwrap_or(0);
        let scope_label = bucketed_scope_label(
            resolution_bucket,
            asset_class_for_asset_label(asset),
            event_subtype,
            shape,
        );
        scores.insert(
            scope_label,
            (
                cooldown_severity_score + window_pressure_score,
                cooldown_severity_score,
                window_pressure_score,
            ),
        );
    }

    scores
}

fn filter_patch_rows_for_auto_apply(
    rows: &[Value],
    tighten_only: bool,
    max_rows: usize,
) -> Vec<Value> {
    let mut filtered = rows
        .iter()
        .filter_map(|row| {
            let mut cloned = row.clone();
            if tighten_only
                && let Some(fields) = cloned.get_mut("fields").and_then(Value::as_array_mut)
            {
                fields.retain(|field| {
                    field.get("direction").and_then(Value::as_str) == Some("tighten")
                });
            }
            let field_count = cloned
                .get("fields")
                .and_then(Value::as_array)
                .map(|fields| fields.len())
                .unwrap_or(0);
            if field_count == 0 { None } else { Some(cloned) }
        })
        .collect::<Vec<_>>();

    filtered.sort_by(|a, b| {
        patch_row_support_score(b)
            .cmp(&patch_row_support_score(a))
            .then_with(|| {
                b.get("scope_label")
                    .and_then(Value::as_str)
                    .cmp(&a.get("scope_label").and_then(Value::as_str))
            })
    });
    if max_rows > 0 && filtered.len() > max_rows {
        filtered.truncate(max_rows);
    }
    filtered
}

async fn build_cooldown_priority_patch_export(
    state: &Arc<ApiState>,
    record_export: bool,
    wants_toml: bool,
    auto_filter: Option<(bool, usize)>,
) -> Result<GeneratedCryptoOverridePatch, String> {
    let Some(repo) = &state.repository else {
        return Err("Trade history database not configured".to_string());
    };

    let Json(status_payload) = get_status(State(state.clone())).await;
    let trade_rows = repo
        .load_trade_history(500, Some("crypto_alpha"), None, None)
        .await
        .map_err(|e| format!("Failed to load crypto trades: {e}"))?;
    let positions = state.positions.read().await.clone();
    let cooldown_buckets = status_payload
        .get("crypto_cooldown_summary")
        .and_then(|value| value.get("buckets"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let entry_rows = status_payload
        .get("crypto_override_patch_preview")
        .and_then(|value| value.get("rows"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let post_entry_rows = status_payload
        .get("crypto_post_entry_override_patch_preview")
        .and_then(|value| value.get("rows"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let bucket_window_scores =
        build_bucket_window_pressure_scores(&build_crypto_bucket_window_summary(
            &trade_rows,
            &positions,
            &crate::diagnostics::recent_crypto_exit_decisions()
                .into_iter()
                .take(24)
                .collect::<Vec<_>>(),
        ));

    let mut selected_scopes = Vec::<(String, String, String, String, i64)>::new();
    for bucket in cooldown_buckets {
        let kind = bucket
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("same_day_range");
        let resolution_bucket = cooldown_kind_resolution_bucket(kind);
        let asset = bucket.get("asset").and_then(Value::as_str).unwrap_or("");
        let event_subtype = bucket
            .get("event_subtype")
            .and_then(Value::as_str)
            .unwrap_or("generic");
        let shape = bucket
            .get("shape")
            .and_then(Value::as_str)
            .unwrap_or("directional");
        let triggered_at = bucket
            .get("triggered_at")
            .and_then(Value::as_str)
            .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
            .map(|value| value.with_timezone(&Utc));
        let post_trigger_bad_exit_count = bucket
            .get("post_trigger_bad_exit_count")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let Some(triggered_at) = triggered_at else {
            continue;
        };
        let post_trigger_realized: Decimal = trade_rows
            .iter()
            .filter(|trade| {
                let trade_asset = infer_trade_asset(trade.question.as_deref());
                let trade_shape = infer_trade_shape(trade.question.as_deref());
                let trade_timestamp = trade.executed_at.unwrap_or(trade.created_at);
                trade_asset == asset
                    && trade_shape == shape
                    && trade_timestamp >= triggered_at
                    && infer_trade_resolution_bucket(
                        trade.question.as_deref(),
                        trade.executed_at,
                        trade.created_at,
                    ) == resolution_bucket
            })
            .map(|trade| trade.actual_profit.unwrap_or(Decimal::ZERO))
            .sum();
        let open_pnl_bid: Decimal = positions
            .iter()
            .filter(|position| {
                position.resolution_bucket.as_deref() == Some(resolution_bucket)
                    && position.asset.as_deref() == Some(asset)
                    && infer_position_shape(position) == shape
            })
            .map(|position| {
                position
                    .unrealized_pnl_bid
                    .or(position.unrealized_pnl)
                    .unwrap_or(Decimal::ZERO)
            })
            .sum();
        let should_keep_tight = post_trigger_bad_exit_count > 0
            || post_trigger_realized < Decimal::ZERO
            || open_pnl_bid < Decimal::ZERO;
        if should_keep_tight {
            let window_pressure = bucket_window_scores
                .get(&(
                    resolution_bucket.to_string(),
                    shape.to_string(),
                    asset_class_for_asset_label(asset).to_string(),
                ))
                .copied()
                .unwrap_or(0);
            let severity_score = (post_trigger_bad_exit_count as i64) * 10_000
                + decimal_loss_score(post_trigger_realized) * 10
                + decimal_loss_score(open_pnl_bid)
                + window_pressure;
            selected_scopes.push((
                asset_class_for_asset_label(asset).to_string(),
                event_subtype.to_string(),
                shape.to_string(),
                resolution_bucket.to_string(),
                severity_score,
            ));
        }
    }

    let mut entry_selected: Vec<Value> = entry_rows
        .into_iter()
        .filter(|row| {
            row.get("source_bucket").and_then(Value::as_str) != Some("legacy")
                && selected_scopes.iter().any(
                    |(asset_class, event_subtype, shape, source_bucket, _)| {
                        row.get("selector_asset_class").and_then(Value::as_str)
                            == Some(asset_class.as_str())
                            && row.get("selector_event_subtype").and_then(Value::as_str)
                                == Some(event_subtype.as_str())
                            && row.get("selector_shape").and_then(Value::as_str)
                                == Some(shape.as_str())
                            && row.get("source_bucket").and_then(Value::as_str)
                                == Some(source_bucket.as_str())
                    },
                )
        })
        .collect();
    let mut post_entry_selected: Vec<Value> = post_entry_rows
        .into_iter()
        .filter(|row| {
            row.get("source_bucket").and_then(Value::as_str) != Some("legacy")
                && selected_scopes.iter().any(
                    |(asset_class, event_subtype, shape, source_bucket, _)| {
                        row.get("selector_asset_class").and_then(Value::as_str)
                            == Some(asset_class.as_str())
                            && row.get("selector_event_subtype").and_then(Value::as_str)
                                == Some(event_subtype.as_str())
                            && row.get("selector_shape").and_then(Value::as_str)
                                == Some(shape.as_str())
                            && row.get("source_bucket").and_then(Value::as_str)
                                == Some(source_bucket.as_str())
                    },
                )
        })
        .collect();
    if let Some((tighten_only, max_rows)) = auto_filter {
        let scope_scores = selected_scopes
            .iter()
            .map(
                |(asset_class, event_subtype, shape, source_bucket, severity_score)| {
                    let scope_label =
                        bucketed_scope_label(source_bucket, asset_class, event_subtype, shape);
                    ((scope_label, source_bucket.clone()), *severity_score)
                },
            )
            .collect::<std::collections::HashMap<_, _>>();
        let mut all_ranked = entry_selected
            .iter()
            .cloned()
            .map(|row| ("entry".to_string(), row))
            .chain(
                post_entry_selected
                    .iter()
                    .cloned()
                    .map(|row| ("post".to_string(), row)),
            )
            .collect::<Vec<_>>();
        all_ranked.sort_by(|a, b| {
            let a_scope =
                a.1.get("scope_label")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
            let b_scope =
                b.1.get("scope_label")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
            let a_bucket =
                a.1.get("source_bucket")
                    .and_then(Value::as_str)
                    .unwrap_or("same_day");
            let b_bucket =
                b.1.get("source_bucket")
                    .and_then(Value::as_str)
                    .unwrap_or("same_day");
            let a_score = scope_scores
                .get(&(a_scope.to_string(), a_bucket.to_string()))
                .copied()
                .unwrap_or(0);
            let b_score = scope_scores
                .get(&(b_scope.to_string(), b_bucket.to_string()))
                .copied()
                .unwrap_or(0);
            b_score
                .cmp(&a_score)
                .then_with(|| patch_row_support_score(&b.1).cmp(&patch_row_support_score(&a.1)))
                .then_with(|| {
                    b.1.get("scope_label")
                        .and_then(Value::as_str)
                        .cmp(&a.1.get("scope_label").and_then(Value::as_str))
                })
        });
        if max_rows > 0 && all_ranked.len() > max_rows {
            all_ranked.truncate(max_rows);
        }
        let allowed_keys = all_ranked
            .into_iter()
            .filter_map(|(_, row)| row_scope_key(&row))
            .collect::<std::collections::HashSet<_>>();
        entry_selected =
            filter_patch_rows_for_auto_apply(&entry_selected, tighten_only, usize::MAX)
                .into_iter()
                .filter(|row| {
                    row_scope_key(row)
                        .map(|label| allowed_keys.contains(&label))
                        .unwrap_or(false)
                })
                .collect();
        post_entry_selected =
            filter_patch_rows_for_auto_apply(&post_entry_selected, tighten_only, usize::MAX)
                .into_iter()
                .filter(|row| {
                    row_scope_key(row)
                        .map(|label| allowed_keys.contains(&label))
                        .unwrap_or(false)
                })
                .collect();
    }
    let toml = [
        render_patch_rows_to_toml(&entry_selected),
        render_patch_rows_to_toml(&post_entry_selected),
    ]
    .into_iter()
    .filter(|value| !value.trim().is_empty())
    .collect::<Vec<_>>()
    .join("\n\n");
    let export_sha = patch_export_digest(&toml);
    let filename = "crypto_cooldown_priority_override_patch.toml".to_string();
    let generated_at = Utc::now().to_rfc3339();
    if record_export {
        crate::diagnostics::record_crypto_override_patch_export(
            crate::diagnostics::CryptoOverridePatchExportDecision {
                recorded_at: Utc::now(),
                mode: "cooldown_priority".into(),
                format: if wants_toml { "toml" } else { "json" }.into(),
                filename: filename.clone(),
                export_sha: export_sha.clone(),
                scope_label: None,
            },
        );
    }

    Ok(GeneratedCryptoOverridePatch {
        mode: "cooldown_priority".into(),
        filename,
        export_sha,
        generated_at,
        scope_label: None,
        scope_labels: selected_scopes
            .iter()
            .map(|(asset_class, event_subtype, shape, source_bucket, _)| {
                bucketed_scope_label(source_bucket, asset_class, event_subtype, shape)
            })
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect(),
        toml,
        selected_bucket_count: selected_scopes.len(),
        entry_row_count: entry_selected.len(),
        post_entry_row_count: post_entry_selected.len(),
    })
}

async fn build_relax_candidate_patch_export(
    state: &Arc<ApiState>,
    record_export: bool,
    wants_toml: bool,
) -> Result<GeneratedCryptoOverridePatch, String> {
    let Json(status_payload) = get_status(State(state.clone())).await;
    let relax_scope_labels = status_payload
        .get("crypto_auto_patch_effectiveness_summary")
        .and_then(|value| value.get("patches"))
        .and_then(Value::as_array)
        .map(|patches| {
            patches
                .iter()
                .filter(|patch| {
                    patch.get("recommended_action").and_then(Value::as_str)
                        == Some("consider_relax")
                })
                .flat_map(|patch| {
                    patch
                        .get("scope_labels")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect::<Vec<_>>()
                })
                .collect::<std::collections::BTreeSet<_>>()
        })
        .unwrap_or_default();
    let entry_rows = status_payload
        .get("crypto_override_patch_preview")
        .and_then(|value| value.get("rows"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let post_entry_rows = status_payload
        .get("crypto_post_entry_override_patch_preview")
        .and_then(|value| value.get("rows"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let post_entry_selected =
        rewrite_patch_rows_direction(&post_entry_rows, &relax_scope_labels, "loosen")
            .into_iter()
            .filter(|row| row.get("source_bucket").and_then(Value::as_str) != Some("legacy"))
            .collect::<Vec<_>>();
    let post_entry_scope_labels = post_entry_selected
        .iter()
        .filter_map(row_scope_key)
        .collect::<std::collections::BTreeSet<_>>();
    let entry_selected = rewrite_patch_rows_direction(&entry_rows, &relax_scope_labels, "loosen")
        .into_iter()
        .filter(|row| {
            row.get("source_bucket").and_then(Value::as_str) != Some("legacy")
                && row_scope_key(row)
                    .map(|label| !post_entry_scope_labels.contains(&label))
                    .unwrap_or(false)
        })
        .collect::<Vec<_>>();

    let toml = [
        render_patch_rows_to_toml(&entry_selected),
        render_patch_rows_to_toml(&post_entry_selected),
    ]
    .into_iter()
    .filter(|value| !value.trim().is_empty())
    .collect::<Vec<_>>()
    .join("\n\n");
    let export_sha = patch_export_digest(&toml);
    let filename = "crypto_relax_candidate_override_patch.toml".to_string();
    let generated_at = Utc::now().to_rfc3339();
    if record_export {
        crate::diagnostics::record_crypto_override_patch_export(
            crate::diagnostics::CryptoOverridePatchExportDecision {
                recorded_at: Utc::now(),
                mode: "relax_candidate".into(),
                format: if wants_toml { "toml" } else { "json" }.into(),
                filename: filename.clone(),
                export_sha: export_sha.clone(),
                scope_label: None,
            },
        );
    }

    Ok(GeneratedCryptoOverridePatch {
        mode: "relax_candidate".into(),
        filename,
        export_sha,
        generated_at,
        scope_label: None,
        scope_labels: relax_scope_labels.into_iter().collect(),
        toml,
        selected_bucket_count: 0,
        entry_row_count: entry_selected.len(),
        post_entry_row_count: post_entry_selected.len(),
    })
}

/// GET /api/crypto/override-patch — server-rendered crypto override patch exports.
fn toml_download_response(filename: &str, toml: String) -> Response {
    let mut headers = HeaderMap::new();
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    if let Ok(value) = HeaderValue::from_str(&format!("attachment; filename=\"{}\"", filename)) {
        headers.insert(CONTENT_DISPOSITION, value);
    }
    (headers, toml).into_response()
}

async fn get_crypto_override_patch(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<CryptoOverridePatchQuery>,
) -> Response {
    let Json(status_payload) = get_status(State(state.clone())).await;
    let mode = query.mode.as_deref().unwrap_or("full");
    let wants_toml = query.format.as_deref() == Some("toml");

    let full_entry_toml = status_payload
        .get("crypto_override_patch_preview")
        .and_then(|value| value.get("toml"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    let full_post_entry_toml = status_payload
        .get("crypto_post_entry_override_patch_preview")
        .and_then(|value| value.get("toml"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();

    if mode == "full" {
        let toml = [full_entry_toml, full_post_entry_toml]
            .into_iter()
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n");
        let filename = "crypto_runtime_override_patch.toml";
        let export_sha = patch_export_digest(&toml);
        let generated_at = Utc::now().to_rfc3339();
        crate::diagnostics::record_crypto_override_patch_export(
            crate::diagnostics::CryptoOverridePatchExportDecision {
                recorded_at: Utc::now(),
                mode: "full".into(),
                format: if wants_toml { "toml" } else { "json" }.into(),
                filename: filename.into(),
                export_sha: export_sha.clone(),
                scope_label: None,
            },
        );
        if wants_toml {
            return toml_download_response(
                filename,
                annotate_patch_toml(&toml, filename, &export_sha, &generated_at),
            );
        }
        return (
            axum::http::StatusCode::OK,
            Json(json!({
                "mode": "full",
                "filename": filename,
                "export_sha": export_sha,
                "generated_at": generated_at,
                "toml": toml,
            })),
        )
            .into_response();
    }

    if mode == "selected" {
        let bucket = query.bucket.as_deref().unwrap_or("same_day");
        let shape = query.shape.as_deref().unwrap_or("directional");
        let entry_rows = status_payload
            .get("crypto_override_patch_preview")
            .and_then(|value| value.get("rows"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let post_entry_rows = status_payload
            .get("crypto_post_entry_override_patch_preview")
            .and_then(|value| value.get("rows"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let entry_selected: Vec<Value> = entry_rows
            .into_iter()
            .filter(|row| {
                row.get("source_bucket").and_then(Value::as_str) == Some(bucket)
                    && row.get("selector_shape").and_then(Value::as_str) == Some(shape)
            })
            .collect();
        let post_entry_selected: Vec<Value> = post_entry_rows
            .into_iter()
            .filter(|row| {
                row.get("source_bucket").and_then(Value::as_str) == Some(bucket)
                    && row.get("selector_shape").and_then(Value::as_str) == Some(shape)
            })
            .collect();
        let toml = [
            render_patch_rows_to_toml(&entry_selected),
            render_patch_rows_to_toml(&post_entry_selected),
        ]
        .into_iter()
        .filter(|value| !value.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
        let filename = format!("crypto_{}_{}_override_patch.toml", bucket, shape);
        let export_sha = patch_export_digest(&toml);
        let generated_at = Utc::now().to_rfc3339();
        crate::diagnostics::record_crypto_override_patch_export(
            crate::diagnostics::CryptoOverridePatchExportDecision {
                recorded_at: Utc::now(),
                mode: "selected".into(),
                format: if wants_toml { "toml" } else { "json" }.into(),
                filename: filename.clone(),
                export_sha: export_sha.clone(),
                scope_label: Some(format!("{bucket} / {shape}")),
            },
        );
        if wants_toml {
            return toml_download_response(
                &filename,
                annotate_patch_toml(&toml, &filename, &export_sha, &generated_at),
            );
        }
        return (
            axum::http::StatusCode::OK,
            Json(json!({
                "mode": "selected",
                "scope_label": format!("{bucket} / {shape}"),
                "filename": filename,
                "export_sha": export_sha,
                "generated_at": generated_at,
                "bucket": bucket,
                "shape": shape,
                "entry_row_count": entry_selected.len(),
                "post_entry_row_count": post_entry_selected.len(),
                "toml": toml,
            })),
        )
            .into_response();
    }

    if mode == "relax_candidate" {
        let generated = match build_relax_candidate_patch_export(&state, true, wants_toml).await {
            Ok(generated) => generated,
            Err(error) => {
                return (
                    axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": error})),
                )
                    .into_response();
            }
        };

        if wants_toml {
            return toml_download_response(
                &generated.filename,
                annotate_patch_toml(
                    &generated.toml,
                    &generated.filename,
                    &generated.export_sha,
                    &generated.generated_at,
                ),
            );
        }

        return (
            axum::http::StatusCode::OK,
            Json(json!({
                "mode": "relax_candidate",
                "filename": generated.filename,
                "export_sha": generated.export_sha,
                "generated_at": generated.generated_at,
                "selected_bucket_count": generated.selected_bucket_count,
                "entry_row_count": generated.entry_row_count,
                "post_entry_row_count": generated.post_entry_row_count,
                "toml": generated.toml,
            })),
        )
            .into_response();
    }

    if mode != "cooldown_priority" {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(json!({"error": "unsupported mode"})),
        )
            .into_response();
    }

    let generated = match build_cooldown_priority_patch_export(&state, true, wants_toml, None).await
    {
        Ok(generated) => generated,
        Err(error) => {
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": error})),
            )
                .into_response();
        }
    };

    if wants_toml {
        return toml_download_response(
            &generated.filename,
            annotate_patch_toml(
                &generated.toml,
                &generated.filename,
                &generated.export_sha,
                &generated.generated_at,
            ),
        );
    }

    (
        axum::http::StatusCode::OK,
        Json(json!({
            "mode": "cooldown_priority",
            "filename": generated.filename,
            "export_sha": generated.export_sha,
            "generated_at": generated.generated_at,
            "selected_bucket_count": generated.selected_bucket_count,
            "entry_row_count": generated.entry_row_count,
            "post_entry_row_count": generated.post_entry_row_count,
            "toml": generated.toml,
        })),
    )
        .into_response()
}

async fn process_crypto_override_patch_apply(
    state: &Arc<ApiState>,
    repo: &Repository,
    body: &ApplyCryptoOverridePatchRequest,
    changed_by: &str,
) -> Result<Value, String> {
    if body.toml.trim().is_empty() {
        return Err("patch toml is empty".to_string());
    }

    let action = body
        .action
        .as_deref()
        .unwrap_or("review")
        .trim()
        .to_ascii_lowercase();
    if !matches!(action.as_str(), "review" | "approve" | "apply_runtime") {
        return Err("unsupported patch action".to_string());
    }

    let runtime_applied_at = if action == "apply_runtime" {
        Some(Utc::now())
    } else {
        None
    };

    if action == "apply_runtime" {
        let parsed: CryptoOverridePatchToml = toml::from_str(&body.toml)
            .map_err(|error| format!("failed to parse patch toml: {error}"))?;

        let mut updated_settings = state.config.load().as_ref().clone();
        merge_crypto_override_rows(
            &mut updated_settings.crypto_alpha.calibration_overrides,
            parsed.crypto_alpha.calibration_overrides,
        );
        let crypto_alpha_value =
            serde_json::to_value(&updated_settings.crypto_alpha).unwrap_or_else(|_| json!({}));

        repo.upsert_config_section("crypto_alpha", &crypto_alpha_value, changed_by)
            .await
            .map_err(|e| format!("Failed to persist runtime crypto_alpha config: {e}"))?;

        state.config.store(Arc::new(updated_settings));
    }

    let data = json!({
        "action": action,
        "mode": body.mode,
        "filename": body.filename,
        "export_sha": body.export_sha,
        "scope_label": body.scope_label,
        "scope_labels": body.scope_labels,
        "generated_at": body.generated_at,
        "toml": body.toml,
        "runtime_applied": action == "apply_runtime",
        "runtime_applied_at": runtime_applied_at.map(|ts| ts.to_rfc3339()),
    });
    repo.upsert_config_section("crypto_override_patch", &data, changed_by)
        .await
        .map_err(|e| format!("Failed to persist crypto patch: {e}"))?;

    Ok(json!({
        "applied": true,
        "action": action,
        "runtime_applied": action == "apply_runtime",
        "filename": data.get("filename").and_then(Value::as_str).unwrap_or_default(),
        "export_sha": data.get("export_sha").and_then(Value::as_str).unwrap_or_default(),
        "note": match action.as_str() {
            "approve" => "Patch snapshot was approved and persisted to app_config/config_history as crypto_override_patch.",
            "apply_runtime" => "Patch snapshot was approved, merged into live crypto_alpha calibration_overrides, and persisted to config storage.",
            _ => "Patch snapshot was persisted to app_config/config_history as crypto_override_patch for controlled review/deployment.",
        },
    }))
}

/// GET /api/crypto/override-patch/audit — recent approved crypto patch snapshots.
async fn get_crypto_override_patch_audit(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<TradesQuery>,
) -> (axum::http::StatusCode, Json<Value>) {
    let Some(repo) = &state.repository else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "repository not configured"})),
        );
    };

    let limit = query.limit.unwrap_or(50).min(200) as i64;
    match repo
        .load_config_history(
            "crypto_override_patch",
            Some("crypto_override_patch_"),
            limit,
        )
        .await
    {
        Ok(rows) => {
            let entries: Vec<CryptoOverridePatchAuditEntry> = rows
                .into_iter()
                .map(|row| CryptoOverridePatchAuditEntry {
                    created_at: row.created_at,
                    changed_by: row.changed_by,
                    version: row.version,
                    action: row
                        .data
                        .get("action")
                        .and_then(Value::as_str)
                        .unwrap_or("review")
                        .to_string(),
                    mode: row
                        .data
                        .get("mode")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    filename: row
                        .data
                        .get("filename")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    export_sha: row
                        .data
                        .get("export_sha")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    scope_label: row
                        .data
                        .get("scope_label")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    generated_at: row
                        .data
                        .get("generated_at")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    runtime_applied: row
                        .data
                        .get("runtime_applied")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    runtime_applied_at: row
                        .data
                        .get("runtime_applied_at")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                })
                .collect();
            (
                axum::http::StatusCode::OK,
                Json(serde_json::to_value(entries).unwrap_or(json!([]))),
            )
        }
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("Failed to load crypto patch audit log: {e}")})),
        ),
    }
}

/// POST /api/crypto/override-patch/apply — persist an approved patch snapshot for review/deployment.
async fn apply_crypto_override_patch(
    State(state): State<Arc<ApiState>>,
    axum::Json(body): axum::Json<ApplyCryptoOverridePatchRequest>,
) -> (axum::http::StatusCode, Json<Value>) {
    let Some(repo) = &state.repository else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "repository not configured"})),
        );
    };

    let action = body
        .action
        .as_deref()
        .unwrap_or("review")
        .trim()
        .to_ascii_lowercase();
    let changed_by = match action.as_str() {
        "approve" => "crypto_override_patch_approve_api",
        "apply_runtime" => "crypto_override_patch_runtime_apply_api",
        _ => "crypto_override_patch_review_api",
    };
    match process_crypto_override_patch_apply(&state, repo, &body, changed_by).await {
        Ok(payload) => (axum::http::StatusCode::OK, Json(payload)),
        Err(error)
            if error.contains("unsupported patch action")
                || error.contains("patch toml is empty")
                || error.contains("failed to parse patch toml") =>
        {
            (
                axum::http::StatusCode::BAD_REQUEST,
                Json(json!({"error": error})),
            )
        }
        Err(error) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": error})),
        ),
    }
}

pub fn spawn_crypto_override_patch_auto_apply(state: Arc<ApiState>) {
    tokio::spawn(async move {
        loop {
            let (enabled, interval_secs, tighten_only, max_rows, min_reapply_secs) = {
                let settings = state.config.load();
                (
                    settings.crypto_alpha.auto_apply_cooldown_priority_patch,
                    settings
                        .crypto_alpha
                        .auto_apply_cooldown_priority_patch_interval_secs
                        .max(30),
                    settings
                        .crypto_alpha
                        .auto_apply_cooldown_priority_patch_tighten_only,
                    settings
                        .crypto_alpha
                        .auto_apply_cooldown_priority_patch_max_rows,
                    settings
                        .crypto_alpha
                        .auto_apply_cooldown_priority_patch_min_reapply_secs,
                )
            };
            tokio::time::sleep(std::time::Duration::from_secs(interval_secs)).await;

            if !enabled
                || !state
                    .startup_ready
                    .load(std::sync::atomic::Ordering::Relaxed)
            {
                continue;
            }
            let Some(repo) = &state.repository else {
                continue;
            };

            let generated = match build_cooldown_priority_patch_export(
                &state,
                false,
                false,
                Some((tighten_only, max_rows)),
            )
            .await
            {
                Ok(generated) => generated,
                Err(error) => {
                    tracing::debug!(error = %error, "crypto cooldown-priority auto-apply skipped");
                    continue;
                }
            };
            if generated.toml.trim().is_empty() {
                continue;
            }

            let already_applied = match repo
                .load_config_history("crypto_override_patch", Some("crypto_override_patch_"), 50)
                .await
            {
                Ok(rows) => rows.into_iter().any(|row| {
                    row.data
                        .get("runtime_applied")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                        && row.data.get("export_sha").and_then(Value::as_str)
                            == Some(generated.export_sha.as_str())
                }),
                Err(error) => {
                    tracing::debug!(error = %error, "failed to inspect crypto patch audit history");
                    false
                }
            };
            if already_applied {
                continue;
            }

            let blocked_by_bucket_cooldown = match repo
                .load_config_history("crypto_override_patch", Some("crypto_override_patch_"), 100)
                .await
            {
                Ok(rows) => rows.into_iter().any(|row| {
                    let recent_enough = row
                        .data
                        .get("runtime_applied_at")
                        .and_then(Value::as_str)
                        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
                        .map(|value| {
                            (Utc::now() - value.with_timezone(&Utc)).num_seconds()
                                < min_reapply_secs as i64
                        })
                        .unwrap_or(false);
                    if !recent_enough {
                        return false;
                    }
                    let existing_scope_labels = row
                        .data
                        .get("scope_labels")
                        .and_then(Value::as_array)
                        .map(|values| {
                            values
                                .iter()
                                .filter_map(Value::as_str)
                                .collect::<std::collections::HashSet<_>>()
                        })
                        .unwrap_or_default();
                    generated
                        .scope_labels
                        .iter()
                        .any(|label| existing_scope_labels.contains(label.as_str()))
                }),
                Err(error) => {
                    tracing::debug!(error = %error, "failed to inspect crypto patch scope cooldown history");
                    false
                }
            };
            if blocked_by_bucket_cooldown {
                continue;
            }

            let auto_effectiveness_entries = match repo
                .load_config_history("crypto_override_patch", Some("crypto_override_patch_"), 100)
                .await
            {
                Ok(rows) => {
                    let positions = state.positions.read().await.clone();
                    let trade_rows = repo
                        .load_trade_history(500, Some("crypto_alpha"), None, None)
                        .await
                        .unwrap_or_default();
                    let recent_exits = crate::diagnostics::recent_crypto_exit_decisions();
                    build_crypto_auto_patch_effectiveness_entries(
                        rows,
                        &trade_rows,
                        &positions,
                        &recent_exits,
                    )
                }
                Err(error) => {
                    tracing::debug!(error = %error, "failed to inspect crypto patch effectiveness history");
                    Vec::new()
                }
            };
            if scope_has_repeated_effective_auto_patches(
                &generated.scope_labels,
                &auto_effectiveness_entries,
                2,
            ) {
                tracing::info!(
                    scope_labels = ?generated.scope_labels,
                    "Skipping cooldown-priority crypto patch because the same scopes already have repeated effective auto-applies"
                );
                continue;
            }

            let request = ApplyCryptoOverridePatchRequest {
                action: Some("apply_runtime".to_string()),
                mode: generated.mode,
                filename: generated.filename,
                export_sha: generated.export_sha,
                toml: generated.toml,
                scope_label: generated.scope_label,
                scope_labels: Some(generated.scope_labels),
                generated_at: Some(generated.generated_at),
            };
            match process_crypto_override_patch_apply(
                &state,
                repo,
                &request,
                "crypto_override_patch_auto_apply_task",
            )
            .await
            {
                Ok(_) => {
                    tracing::info!(
                        export_sha = %request.export_sha,
                        "Auto-applied cooldown-priority crypto override patch"
                    );
                }
                Err(error) => {
                    tracing::warn!(
                        export_sha = %request.export_sha,
                        error = %error,
                        "Failed to auto-apply cooldown-priority crypto override patch"
                    );
                }
            }
        }
    });
}

/// GET /api/smart-money/audit — recent smart-money operator config changes from config history.
async fn get_smart_money_audit(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<TradesQuery>,
) -> (axum::http::StatusCode, Json<Value>) {
    let Some(repo) = &state.repository else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "repository not configured"})),
        );
    };

    let limit = query.limit.unwrap_or(50).min(200) as i64;
    match repo
        .load_config_history("smart_money", Some("smart_money_"), limit)
        .await
    {
        Ok(rows) => {
            let entries: Vec<SmartMoneyAuditEntry> = rows
                .into_iter()
                .map(|row| {
                    let blocked_wallet_count = row
                        .data
                        .get("blocked_wallets")
                        .and_then(|value| value.as_array())
                        .map(|items| items.len())
                        .unwrap_or(0);
                    let degraded_wallet_count = row
                        .data
                        .get("degraded_wallets")
                        .and_then(|value| value.as_array())
                        .map(|items| items.len())
                        .unwrap_or(0);
                    let wallet_count = row
                        .data
                        .get("wallets")
                        .and_then(|value| value.as_array())
                        .map(|items| items.len())
                        .unwrap_or(0);
                    let auto_discover_candidate_count = row
                        .data
                        .get("auto_discover_candidates")
                        .and_then(|value| value.as_array())
                        .map(|items| items.len())
                        .unwrap_or(0);
                    let route_count = row
                        .data
                        .get("leader_routes")
                        .and_then(|value| value.as_array())
                        .map(|items| items.len())
                        .unwrap_or(0);
                    SmartMoneyAuditEntry {
                        created_at: row.created_at,
                        changed_by: row.changed_by,
                        version: row.version,
                        blocked_wallet_count,
                        degraded_wallet_count,
                        wallet_count,
                        auto_discover_candidate_count,
                        route_count,
                    }
                })
                .collect();
            (
                axum::http::StatusCode::OK,
                Json(serde_json::to_value(entries).unwrap_or(json!([]))),
            )
        }
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("Failed to load smart-money audit log: {e}")})),
        ),
    }
}

/// GET /api/smart-money/leaders — recent discovered smart-money leader candidates.
async fn get_smart_money_leaders(State(state): State<Arc<ApiState>>) -> Json<Value> {
    let rows = if let Some(repo) = &state.repository {
        repo.load_smart_money_leader_candidates(200)
            .await
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let smart_money_config = state.smart_money_config.load();
    Json(json!(
        rows.iter()
            .map(|row| smart_money_leader_candidate_json(row, smart_money_config.as_ref()))
            .collect::<Vec<_>>()
    ))
}

/// POST /api/smart-money/leaders/block — block a discovered leader from tracking/auto-discovery.
async fn block_smart_money_leader(
    State(state): State<Arc<ApiState>>,
    axum::Json(body): axum::Json<BlockSmartMoneyLeaderRequest>,
) -> (axum::http::StatusCode, Json<Value>) {
    let Some(repo) = &state.repository else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "repository not configured"})),
        );
    };

    let address = body.address.trim().to_lowercase();
    let row = match repo.load_smart_money_leader_candidate(&address).await {
        Ok(Some(row)) => row,
        Ok(None) => {
            return (
                axum::http::StatusCode::NOT_FOUND,
                Json(json!({"error": format!("unknown smart-money leader candidate: {address}")})),
            );
        }
        Err(e) => {
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("failed to load leader candidate: {e}")})),
            );
        }
    };

    let mut updated_settings = state.config.load().as_ref().clone();
    if !updated_settings
        .smart_money
        .blocked_wallets
        .iter()
        .any(|blocked| blocked.eq_ignore_ascii_case(&address))
    {
        updated_settings
            .smart_money
            .blocked_wallets
            .push(address.clone());
    }
    updated_settings
        .smart_money
        .wallets
        .retain(|wallet| !wallet.address.eq_ignore_ascii_case(&address));
    updated_settings
        .smart_money
        .auto_discover_candidates
        .retain(|candidate| !candidate.eq_ignore_ascii_case(&address));
    updated_settings
        .smart_money
        .degraded_wallets
        .retain(|wallet| !wallet.address.eq_ignore_ascii_case(&address));

    persist_smart_money_settings_update(
        state.as_ref(),
        repo,
        updated_settings,
        "smart_money_block_api",
    )
    .await;

    (
        axum::http::StatusCode::OK,
        Json(json!({
            "candidate": smart_money_leader_candidate_json(&row, state.smart_money_config.load().as_ref()),
            "blocked": true,
            "note": "Candidate was added to smart_money.blocked_wallets, removed from current wallets/auto_discover_candidates, and the live smart-money config was updated."
        })),
    )
}

/// POST /api/smart-money/leaders/degrade — reduce a leader's runtime weight with a multiplier.
async fn degrade_smart_money_leader(
    State(state): State<Arc<ApiState>>,
    axum::Json(body): axum::Json<DegradeSmartMoneyLeaderRequest>,
) -> (axum::http::StatusCode, Json<Value>) {
    let Some(repo) = &state.repository else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "repository not configured"})),
        );
    };

    let address = body.address.trim().to_lowercase();
    let multiplier = body
        .multiplier
        .unwrap_or_else(|| Decimal::new(50, 2))
        .max(Decimal::ZERO)
        .min(Decimal::ONE);
    let row = match repo.load_smart_money_leader_candidate(&address).await {
        Ok(Some(row)) => row,
        Ok(None) => {
            return (
                axum::http::StatusCode::NOT_FOUND,
                Json(json!({"error": format!("unknown smart-money leader candidate: {address}")})),
            );
        }
        Err(e) => {
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("failed to load leader candidate: {e}")})),
            );
        }
    };

    let mut updated_settings = state.config.load().as_ref().clone();
    updated_settings
        .smart_money
        .blocked_wallets
        .retain(|blocked| !blocked.eq_ignore_ascii_case(&address));
    if let Some(existing) = updated_settings
        .smart_money
        .degraded_wallets
        .iter_mut()
        .find(|wallet| wallet.address.eq_ignore_ascii_case(&address))
    {
        existing.multiplier = multiplier;
    } else {
        updated_settings
            .smart_money
            .degraded_wallets
            .push(pa_core::config::DegradedWalletConfig {
                address: address.clone(),
                multiplier,
            });
    }

    persist_smart_money_settings_update(
        state.as_ref(),
        repo,
        updated_settings,
        "smart_money_degrade_api",
    )
    .await;

    (
        axum::http::StatusCode::OK,
        Json(json!({
            "candidate": smart_money_leader_candidate_json(&row, state.smart_money_config.load().as_ref()),
            "degraded": true,
            "multiplier": multiplier,
            "note": "Candidate was added to smart_money.degraded_wallets and the live smart-money config was updated."
        })),
    )
}

/// POST /api/smart-money/leaders/restore — remove block/degrade overrides for a leader.
async fn restore_smart_money_leader(
    State(state): State<Arc<ApiState>>,
    axum::Json(body): axum::Json<RestoreSmartMoneyLeaderRequest>,
) -> (axum::http::StatusCode, Json<Value>) {
    let Some(repo) = &state.repository else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "repository not configured"})),
        );
    };

    let address = body.address.trim().to_lowercase();
    let row = match repo.load_smart_money_leader_candidate(&address).await {
        Ok(Some(row)) => row,
        Ok(None) => {
            return (
                axum::http::StatusCode::NOT_FOUND,
                Json(json!({"error": format!("unknown smart-money leader candidate: {address}")})),
            );
        }
        Err(e) => {
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("failed to load leader candidate: {e}")})),
            );
        }
    };

    let mut updated_settings = state.config.load().as_ref().clone();
    updated_settings
        .smart_money
        .blocked_wallets
        .retain(|blocked| !blocked.eq_ignore_ascii_case(&address));
    updated_settings
        .smart_money
        .degraded_wallets
        .retain(|wallet| !wallet.address.eq_ignore_ascii_case(&address));

    persist_smart_money_settings_update(
        state.as_ref(),
        repo,
        updated_settings,
        "smart_money_restore_api",
    )
    .await;

    (
        axum::http::StatusCode::OK,
        Json(json!({
            "candidate": smart_money_leader_candidate_json(&row, state.smart_money_config.load().as_ref()),
            "restored": true,
            "note": "Candidate was removed from smart_money.blocked_wallets and smart_money.degraded_wallets, and the live smart-money config was updated."
        })),
    )
}

/// POST /api/smart-money/leaders/route-template — apply a route template or clear an existing route.
async fn apply_smart_money_leader_route_template(
    State(state): State<Arc<ApiState>>,
    axum::Json(body): axum::Json<ApplySmartMoneyLeaderRouteTemplateRequest>,
) -> (axum::http::StatusCode, Json<Value>) {
    let Some(repo) = &state.repository else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "repository not configured"})),
        );
    };

    let address = body.address.trim().to_lowercase();
    let row = match repo.load_smart_money_leader_candidate(&address).await {
        Ok(Some(row)) => row,
        Ok(None) => {
            return (
                axum::http::StatusCode::NOT_FOUND,
                Json(json!({"error": format!("unknown smart-money leader candidate: {address}")})),
            );
        }
        Err(e) => {
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("failed to load leader candidate: {e}")})),
            );
        }
    };

    let template = body.template.trim().to_lowercase();
    let route = match smart_money_route_template(&address, &template) {
        Ok(route) => route,
        Err(error) => {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                Json(json!({ "error": error })),
            );
        }
    };

    let mut updated_settings = state.config.load().as_ref().clone();
    updated_settings
        .smart_money
        .leader_routes
        .retain(|existing| !existing.address.eq_ignore_ascii_case(&address));
    if let Some(route) = route {
        updated_settings.smart_money.leader_routes.push(route);
    }

    persist_smart_money_settings_update(
        state.as_ref(),
        repo,
        updated_settings,
        "smart_money_route_template_api",
    )
    .await;

    let note = if template == "clear" {
        "Leader route was cleared and the live smart-money config was updated."
    } else {
        "Leader route template was applied and the live smart-money config was updated."
    };

    (
        axum::http::StatusCode::OK,
        Json(json!({
            "candidate": smart_money_leader_candidate_json(&row, state.smart_money_config.load().as_ref()),
            "template": template,
            "note": note,
        })),
    )
}

/// POST /api/smart-money/leaders/promote — mark a discovered leader as promoted and return config snippets.
async fn promote_smart_money_leader(
    State(state): State<Arc<ApiState>>,
    axum::Json(body): axum::Json<PromoteSmartMoneyLeaderRequest>,
) -> (axum::http::StatusCode, Json<Value>) {
    let Some(repo) = &state.repository else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "repository not configured"})),
        );
    };

    let address = body.address.trim().to_lowercase();
    let row = match repo.load_smart_money_leader_candidate(&address).await {
        Ok(Some(row)) => row,
        Ok(None) => {
            return (
                axum::http::StatusCode::NOT_FOUND,
                Json(json!({"error": format!("unknown smart-money leader candidate: {address}")})),
            );
        }
        Err(e) => {
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("failed to load leader candidate: {e}")})),
            );
        }
    };

    if let Err(e) = repo
        .set_smart_money_leader_candidate_promoted(&address, true)
        .await
    {
        return (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("failed to update promoted flag: {e}")})),
        );
    }

    let label = smart_money_leader_label(&row);
    let mut updated_settings = state.config.load().as_ref().clone();
    if !updated_settings
        .smart_money
        .wallets
        .iter()
        .any(|wallet| wallet.address.eq_ignore_ascii_case(&row.address))
    {
        updated_settings
            .smart_money
            .wallets
            .push(TrackedWalletConfig {
                address: row.address.clone(),
                label: label.clone(),
                weight: Decimal::ONE,
            });
    }
    if !updated_settings
        .smart_money
        .auto_discover_candidates
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(&row.address))
    {
        updated_settings
            .smart_money
            .auto_discover_candidates
            .push(row.address.clone());
    }
    updated_settings
        .smart_money
        .blocked_wallets
        .retain(|blocked| !blocked.eq_ignore_ascii_case(&row.address));
    persist_smart_money_settings_update(
        state.as_ref(),
        repo,
        updated_settings,
        "smart_money_promote_api",
    )
    .await;

    let wallets_toml = format!(
        "[[smart_money.wallets]]\naddress = \"{}\"\nlabel = \"{}\"\nweight = 1.0\n",
        row.address,
        label.replace('\\', "\\\\").replace('"', "\\\"")
    );

    (
        axum::http::StatusCode::OK,
        Json(json!({
            "candidate": smart_money_leader_candidate_json(&row, state.smart_money_config.load().as_ref()),
            "promoted": true,
            "wallets_toml": wallets_toml,
            "auto_discover_candidate": format!("\"{}\"", row.address),
            "note": "Candidate marked as promoted, appended to the smart_money wallets/candidate pool, persisted to the config store, and pushed into the live smart-money config."
        })),
    )
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

fn normalize_crypto_override_selector(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("any") {
        "any".to_string()
    } else {
        trimmed.to_ascii_lowercase()
    }
}

fn crypto_override_selector_key(
    row: &pa_core::config::CryptoCalibrationOverride,
) -> (String, String, String, String, String, String) {
    (
        normalize_crypto_override_selector(&row.asset),
        normalize_crypto_override_selector(&row.asset_class),
        normalize_crypto_override_selector(&row.horizon),
        normalize_crypto_override_selector(&row.resolution_bucket),
        normalize_crypto_override_selector(&row.market_type),
        normalize_crypto_override_selector(&row.event_subtype),
    )
}

fn merge_crypto_override_rows(
    existing: &mut Vec<pa_core::config::CryptoCalibrationOverride>,
    incoming: Vec<pa_core::config::CryptoCalibrationOverride>,
) {
    for patch_row in incoming {
        let patch_key = crypto_override_selector_key(&patch_row);
        if let Some(existing_row) = existing
            .iter_mut()
            .find(|row| crypto_override_selector_key(row) == patch_key)
        {
            if patch_row.probability_calibration.is_some() {
                existing_row.probability_calibration = patch_row.probability_calibration;
            }
            if patch_row.sigma_multiplier.is_some() {
                existing_row.sigma_multiplier = patch_row.sigma_multiplier;
            }
            if patch_row.size_multiplier.is_some() {
                existing_row.size_multiplier = patch_row.size_multiplier;
            }
            if patch_row.depth_ratio_multiplier.is_some() {
                existing_row.depth_ratio_multiplier = patch_row.depth_ratio_multiplier;
            }
            if patch_row.min_edge_multiplier.is_some() {
                existing_row.min_edge_multiplier = patch_row.min_edge_multiplier;
            }
            if patch_row.max_spread_multiplier.is_some() {
                existing_row.max_spread_multiplier = patch_row.max_spread_multiplier;
            }
            if patch_row.hold_edge_multiplier.is_some() {
                existing_row.hold_edge_multiplier = patch_row.hold_edge_multiplier;
            }
            if patch_row.edge_decay_exit_multiplier.is_some() {
                existing_row.edge_decay_exit_multiplier = patch_row.edge_decay_exit_multiplier;
            }
            if patch_row.edge_decay_confirmation_scan_multiplier.is_some() {
                existing_row.edge_decay_confirmation_scan_multiplier =
                    patch_row.edge_decay_confirmation_scan_multiplier;
            }
            if patch_row
                .edge_decay_confirmation_window_multiplier
                .is_some()
            {
                existing_row.edge_decay_confirmation_window_multiplier =
                    patch_row.edge_decay_confirmation_window_multiplier;
            }
            if patch_row.edge_decay_cooldown_multiplier.is_some() {
                existing_row.edge_decay_cooldown_multiplier =
                    patch_row.edge_decay_cooldown_multiplier;
            }
            if patch_row.capital_efficiency_multiplier.is_some() {
                existing_row.capital_efficiency_multiplier =
                    patch_row.capital_efficiency_multiplier;
            }
            if patch_row.model_reversal_buffer_multiplier.is_some() {
                existing_row.model_reversal_buffer_multiplier =
                    patch_row.model_reversal_buffer_multiplier;
            }
            if patch_row.profit_retention_multiplier.is_some() {
                existing_row.profit_retention_multiplier = patch_row.profit_retention_multiplier;
            }
            if patch_row.slippage_multiplier.is_some() {
                existing_row.slippage_multiplier = patch_row.slippage_multiplier;
            }
            if patch_row.size_retention_multiplier.is_some() {
                existing_row.size_retention_multiplier = patch_row.size_retention_multiplier;
            }
        } else {
            existing.push(patch_row);
        }
    }
}

async fn persist_smart_money_settings_update(
    state: &ApiState,
    repo: &Repository,
    updated_settings: Settings,
    changed_by: &str,
) {
    state.config.store(Arc::new(updated_settings.clone()));
    state
        .smart_money_config
        .store(Arc::new(updated_settings.smart_money.clone()));
    if let Err(e) = repo
        .upsert_config_section(
            "smart_money",
            &serde_json::to_value(&updated_settings.smart_money).unwrap_or_else(|_| json!({})),
            changed_by,
        )
        .await
    {
        tracing::warn!(error = %e, "failed to persist smart-money config section update");
    }
}

fn smart_money_leader_candidate_json(
    row: &pa_storage::models::SmartMoneyLeaderCandidateRow,
    smart_money_config: &pa_core::config::SmartMoneyConfig,
) -> Value {
    let blocked = smart_money_config
        .blocked_wallets
        .iter()
        .any(|address| address.eq_ignore_ascii_case(&row.address));
    let degrade_multiplier = smart_money_config
        .degraded_wallets
        .iter()
        .find(|wallet| wallet.address.eq_ignore_ascii_case(&row.address))
        .map(|wallet| wallet.multiplier);
    let route = smart_money_config
        .leader_routes
        .iter()
        .find(|route| route.address.eq_ignore_ascii_case(&row.address));
    json!({
        "address": row.address,
        "label": row.label,
        "source_tags": row.source_tags,
        "first_seen_at": row.first_seen_at,
        "last_seen_at": row.last_seen_at,
        "leaderboard_rank": row.leaderboard_rank,
        "leaderboard_volume": row.leaderboard_volume,
        "leaderboard_pnl": row.leaderboard_pnl,
        "open_positions_count": row.open_positions_count,
        "open_notional": row.open_notional,
        "closed_positions_count": row.closed_positions_count,
        "closed_total_bought": row.closed_total_bought,
        "closed_realized_pnl": row.closed_realized_pnl,
        "sampled_markets": row.sampled_markets,
        "market_position_count": row.market_position_count,
        "holder_position_count": row.holder_position_count,
        "activity_volume": row.activity_volume,
        "activity_pnl": row.activity_pnl,
        "verified": row.verified,
        "discovery_score": row.discovery_score,
        "promoted": row.promoted,
        "blocked": blocked,
        "degrade_multiplier": degrade_multiplier,
        "route_categories": route.map(|route| route.categories.clone()).unwrap_or_default(),
        "route_question_keywords": route.map(|route| route.question_keywords.clone()).unwrap_or_default(),
        "route_event_title_keywords": route.map(|route| route.event_title_keywords.clone()).unwrap_or_default(),
        "metadata": row.metadata,
        "updated_at": row.updated_at,
    })
}

fn smart_money_route_template(
    address: &str,
    template: &str,
) -> Result<Option<pa_core::config::SmartMoneyLeaderRouteConfig>, String> {
    let route = match template {
        "clear" | "all" => None,
        "crypto" => Some(pa_core::config::SmartMoneyLeaderRouteConfig {
            address: address.to_string(),
            categories: vec!["crypto".into()],
            question_keywords: vec![
                "bitcoin".into(),
                "btc".into(),
                "ethereum".into(),
                "eth".into(),
                "solana".into(),
                "sol".into(),
            ],
            event_title_keywords: vec!["crypto".into()],
        }),
        "politics" => Some(pa_core::config::SmartMoneyLeaderRouteConfig {
            address: address.to_string(),
            categories: vec!["politics".into()],
            question_keywords: vec![
                "election".into(),
                "president".into(),
                "senate".into(),
                "house".into(),
                "governor".into(),
            ],
            event_title_keywords: vec!["election".into(), "politic".into()],
        }),
        "sports" => Some(pa_core::config::SmartMoneyLeaderRouteConfig {
            address: address.to_string(),
            categories: vec!["sports".into()],
            question_keywords: vec![
                "match".into(),
                "game".into(),
                "score".into(),
                "championship".into(),
                "tournament".into(),
            ],
            event_title_keywords: vec!["vs".into(), "match".into(), "tournament".into()],
        }),
        "weather" => Some(pa_core::config::SmartMoneyLeaderRouteConfig {
            address: address.to_string(),
            categories: vec!["weather".into()],
            question_keywords: vec![
                "temperature".into(),
                "rain".into(),
                "snow".into(),
                "wind".into(),
                "weather".into(),
            ],
            event_title_keywords: vec!["weather".into(), "temperature".into()],
        }),
        _ => {
            return Err(format!(
                "unknown smart-money leader route template: {template}"
            ));
        }
    };
    Ok(route)
}

fn smart_money_leader_label(row: &pa_storage::models::SmartMoneyLeaderCandidateRow) -> String {
    let label = row.label.trim();
    if !label.is_empty() {
        label.to_string()
    } else {
        format!("leader_{}", &row.address[2..10.min(row.address.len())])
    }
}
