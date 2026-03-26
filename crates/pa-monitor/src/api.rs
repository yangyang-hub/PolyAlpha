use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use arc_swap::ArcSwap;
use axum::extract::{Path, Query, State};
use axum::response::Json;
use axum::{
    Router,
    routing::{get, post},
};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tower_http::cors::CorsLayer;
use tower_http::services::{ServeDir, ServeFile};

use pa_core::config::Settings;
use pa_core::config::TrackedWalletConfig;
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

    fn preview_multiplier_for(target_field: &str, direction: &str) -> Option<&'static str> {
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

            if preview_multiplier_for(&target_field, &direction).is_none() {
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
                if let Some(value) = preview_multiplier_for(&target_field, &direction) {
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
                && entry.days_to_resolution == Some(0)
                && (now - entry.recorded_at) <= cooldown_window
        }) {
            let Some(asset) = entry.asset.clone() else {
                continue;
            };
            if label_kind == "same_day_alt" && asset_class_for_label(Some(&asset)) != "alt" {
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
            settings
                .crypto_alpha
                .same_day_range_bad_exit_cooldown_trigger_count as usize,
            settings.crypto_alpha.same_day_range_bad_exit_cooldown_secs as i64,
            "same_day_range",
        );
        entries.extend(active_cooldown_entries(
            &recent_exits,
            "binary",
            settings
                .crypto_alpha
                .same_day_alt_bad_exit_cooldown_trigger_count as usize,
            settings.crypto_alpha.same_day_alt_bad_exit_cooldown_secs as i64,
            "same_day_alt",
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
