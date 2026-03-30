use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use alloy::primitives::B256;
use arc_swap::ArcSwap;
use axum::extract::{Path, Query, State};
use axum::http::header::{CONTENT_DISPOSITION, CONTENT_TYPE};
use axum::http::{HeaderMap, HeaderValue};
use axum::response::{IntoResponse, Json, Response};
use axum::{
    Router,
    routing::{get, post},
};
use chrono::{DateTime, Datelike, Utc};
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
    pub uses_conservative_post_entry: bool,
    pub uses_fallback_post_entry: bool,
    pub uses_entry_fallback: bool,
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

#[derive(Debug, Clone, Serialize)]
struct CryptoSubtypeWindowSummaryEntry {
    window_label: &'static str,
    resolution_bucket: String,
    shape: String,
    asset_class: String,
    event_subtype: String,
    trade_count: usize,
    realized_pnl: Decimal,
    open_positions: usize,
    open_pnl_bid: Decimal,
    bad_exit_count: usize,
}

#[derive(Debug, Clone)]
struct CryptoAssetLongWindowSummaryEntry {
    asset: String,
    trade_count: usize,
    realized_pnl: Decimal,
    open_positions: usize,
    open_pnl_bid: Decimal,
    bad_exit_count: usize,
    pressure_score: i64,
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

    fn count_weather_reasons(
        buckets: &[crate::diagnostics::WeatherRejectionBucket],
        min_minute_start_unix: Option<i64>,
    ) -> std::collections::HashMap<String, usize> {
        let mut counts = std::collections::HashMap::new();
        for bucket in buckets {
            if min_minute_start_unix.is_some_and(|cutoff| bucket.minute_start_unix < cutoff) {
                continue;
            }
            for (reason, count) in &bucket.reason_counts {
                *counts.entry(reason.clone()).or_insert(0) += *count;
            }
        }
        counts
    }

    fn sum_counts(counts: &std::collections::HashMap<String, usize>) -> usize {
        counts.values().copied().sum()
    }

    fn weather_blocker_counts(
        counts: &std::collections::HashMap<String, usize>,
    ) -> std::collections::HashMap<String, usize> {
        counts
            .iter()
            .filter(|(reason, _)| reason.as_str() != "unsupported_city")
            .map(|(reason, count)| (reason.clone(), *count))
            .collect()
    }

    fn count_weather_cities_for_reason(
        buckets: &[crate::diagnostics::WeatherRejectionBucket],
        min_minute_start_unix: Option<i64>,
        reason: &str,
    ) -> std::collections::HashMap<String, usize> {
        let mut counts = std::collections::HashMap::new();
        for bucket in buckets {
            if min_minute_start_unix.is_some_and(|cutoff| bucket.minute_start_unix < cutoff) {
                continue;
            }
            if let Some(city_counts) = bucket.reason_city_counts.get(reason) {
                for (city, count) in city_counts {
                    *counts.entry(city.clone()).or_insert(0) += *count;
                }
            }
        }
        counts
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
            ) && infer_exit_shape_label(entry.market_type.as_deref(), Some(entry.question.as_str()))
                == normalized_shape_label(Some(market_type))
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
                "shape": infer_exit_shape_label(Some(market_type), None),
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
    let recent_weather_rejection_buckets = crate::diagnostics::recent_weather_rejection_buckets();
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
    let current_minute_start_unix = (Utc::now().timestamp() / 60) * 60;
    let weather_reason_counts_retained =
        count_weather_reasons(&recent_weather_rejection_buckets, None);
    let weather_blocker_counts_retained = weather_blocker_counts(&weather_reason_counts_retained);
    let weather_reason_counts_1h = count_weather_reasons(
        &recent_weather_rejection_buckets,
        Some(current_minute_start_unix - 60 * 60),
    );
    let weather_blocker_counts_1h = weather_blocker_counts(&weather_reason_counts_1h);
    let weather_reason_counts_6h = count_weather_reasons(
        &recent_weather_rejection_buckets,
        Some(current_minute_start_unix - 6 * 60 * 60),
    );
    let weather_blocker_counts_6h = weather_blocker_counts(&weather_reason_counts_6h);
    let weather_spread_city_counts_1h = count_weather_cities_for_reason(
        &recent_weather_rejection_buckets,
        Some(current_minute_start_unix - 60 * 60),
        "spread_too_wide",
    );
    let weather_spread_city_counts_6h = count_weather_cities_for_reason(
        &recent_weather_rejection_buckets,
        Some(current_minute_start_unix - 6 * 60 * 60),
        "spread_too_wide",
    );
    let weather_price_city_counts_1h = count_weather_cities_for_reason(
        &recent_weather_rejection_buckets,
        Some(current_minute_start_unix - 60 * 60),
        "price_above_max_entry",
    );
    let weather_price_city_counts_6h = count_weather_cities_for_reason(
        &recent_weather_rejection_buckets,
        Some(current_minute_start_unix - 6 * 60 * 60),
        "price_above_max_entry",
    );
    let recent_gate_scales: Vec<_> = recent_candidate_decisions
        .iter()
        .filter(|decision| decision.action == "gate_scale")
        .take(24)
        .cloned()
        .collect();
    let crypto_generic_day_market_summary =
        build_crypto_generic_day_market_summary(&recent_candidate_decisions);
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
        "last_8": count_reason_window(&recent_gate_rejects, 8),
        "last_24": count_reason_window(&recent_gate_rejects, 24),
    });
    let subtype_windows = json!({
        "last_8": count_subtype_window(&recent_gate_rejects, 8),
        "last_24": count_subtype_window(&recent_gate_rejects, 24),
    });
    let asset_windows = json!({
        "last_8": count_asset_window(&recent_gate_rejects, 8),
        "last_24": count_asset_window(&recent_gate_rejects, 24),
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
            infer_exit_shape_label(
                decision.market_type.as_deref(),
                Some(decision.question.as_str()),
            )
            .to_string(),
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
        let entry_patch_rows = crypto_override_patch_preview
            .get("rows")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let post_entry_patch_rows = crypto_post_entry_override_patch_preview
            .get("rows")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let current_scope_scores =
            build_current_cooldown_scope_scores(&crypto_cooldown_buckets, &trade_rows, &positions);
        let audit_rows = repo
            .load_config_history("crypto_override_patch", Some("crypto_override_patch_"), 50)
            .await
            .unwrap_or_default();
        let effectiveness_entries = build_crypto_auto_patch_effectiveness_entries(
            &audit_rows,
            &trade_rows,
            &positions,
            &recent_exits,
        );
        let all_patch_effects: Vec<_> = effectiveness_entries
            .iter()
            .map(|entry| {
                let effective_streak =
                    scope_effective_streak(&entry.scope_labels, &effectiveness_entries);
                let max_long_window_pressure = compute_scope_set_long_window_pressure(
                    &entry.scope_labels,
                    &trade_rows,
                    &positions,
                    &recent_exits,
                );
                let blocked_by_relax_step_cooldown = scope_has_recent_patch_mode(
                    &audit_rows,
                    &entry.scope_labels,
                    "relax_candidate",
                    settings
                        .crypto_alpha
                        .auto_apply_cooldown_priority_patch_min_reapply_secs
                        as i64,
                );
                let recommended_action = match entry.outcome {
                    "effective"
                        if effective_streak >= 3
                            && entry.current_open_positions == 0
                            && max_long_window_pressure == 0
                            && !blocked_by_relax_step_cooldown =>
                    {
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
                    current_long_window_pressure_score,
                ) = entry
                    .scope_labels
                    .iter()
                    .filter_map(|scope_label| current_scope_scores.get(scope_label))
                    .copied()
                    .max_by_key(|(priority, _, _, _)| *priority)
                    .unwrap_or((0, 0, 0, 0));
                let priority_reason_label = auto_patch_priority_reason_label(
                    current_priority_score,
                    current_cooldown_severity_score,
                    current_window_pressure_score,
                    current_long_window_pressure_score,
                );
                let (
                    relax_uses_conservative_post_entry,
                    relax_uses_fallback_post_entry,
                    relax_uses_entry_fallback,
                ) = if recommended_action == "consider_relax" {
                    compute_relax_tier_for_scope_labels(
                        &entry_patch_rows,
                        &post_entry_patch_rows,
                        &entry.scope_labels,
                    )
                } else {
                    (false, false, false)
                };
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
                    "blocked_by_long_window_relax_guard": entry.outcome == "effective"
                        && effective_streak >= 3
                        && entry.current_open_positions == 0
                        && max_long_window_pressure > 0,
                    "blocked_by_relax_step_cooldown": entry.outcome == "effective"
                        && effective_streak >= 3
                        && entry.current_open_positions == 0
                        && blocked_by_relax_step_cooldown,
                    "current_priority_score": current_priority_score,
                    "current_cooldown_severity_score": current_cooldown_severity_score,
                    "current_window_pressure_score": current_window_pressure_score,
                    "current_long_window_pressure_score": current_long_window_pressure_score,
                    "priority_reason_label": priority_reason_label,
                    "relax_uses_conservative_post_entry": relax_uses_conservative_post_entry,
                    "relax_uses_fallback_post_entry": relax_uses_fallback_post_entry,
                    "relax_uses_entry_fallback": relax_uses_entry_fallback,
                })
            })
            .collect();
        let patches: Vec<_> = all_patch_effects.iter().take(8).cloned().collect();
        let long_window_relax_guard_summary = {
            let rows = all_patch_effects
                .iter()
                .filter(|patch| {
                    patch.get("blocked_by_long_window_relax_guard")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                })
                .map(|patch| {
                    let post_apply_bad_exit_count = patch
                        .get("post_apply_bad_exit_count")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    let post_apply_realized_pnl = patch
                        .get("post_apply_realized_pnl")
                        .and_then(Value::as_str)
                        .and_then(|value| Decimal::from_str_exact(value).ok())
                        .unwrap_or(Decimal::ZERO);
                    let current_open_pnl_bid = patch
                        .get("current_open_pnl_bid")
                        .and_then(Value::as_str)
                        .and_then(|value| Decimal::from_str_exact(value).ok())
                        .unwrap_or(Decimal::ZERO);
                    let effect_label = if post_apply_bad_exit_count > 0
                        || post_apply_realized_pnl < Decimal::ZERO
                        || current_open_pnl_bid < Decimal::ZERO
                    {
                        "继续承压"
                    } else {
                        "逐步稳定"
                    };
                    let window_effect_label = if patch
                        .get("current_window_pressure_score")
                        .and_then(Value::as_i64)
                        .unwrap_or(0)
                        > 0
                    {
                        "6h 内仍承压"
                    } else {
                        "短窗已趋稳"
                    };
                    json!({
                        "runtime_applied_at": patch.get("runtime_applied_at").cloned().unwrap_or(Value::Null),
                        "scope_labels": patch.get("scope_labels").cloned().unwrap_or_else(|| json!([])),
                        "effective_streak": patch.get("effective_streak").cloned().unwrap_or_else(|| json!(0)),
                        "current_long_window_pressure_score": patch.get("current_long_window_pressure_score").cloned().unwrap_or_else(|| json!(0)),
                        "current_open_positions": patch.get("current_open_positions").cloned().unwrap_or_else(|| json!(0)),
                        "current_open_pnl_bid": patch.get("current_open_pnl_bid").cloned().unwrap_or_else(|| json!("0")),
                        "post_apply_bad_exit_count": post_apply_bad_exit_count,
                        "post_apply_realized_pnl": post_apply_realized_pnl,
                        "effect_label": effect_label,
                        "window_effect_label": window_effect_label,
                        "note": "24h 慢变量仍承压，暂不进入建议回退",
                    })
                })
                .collect::<Vec<_>>();
            let continuing_pressure_count = rows
                .iter()
                .filter(|row| row.get("effect_label").and_then(Value::as_str) == Some("继续承压"))
                .count();
            let stabilizing_count = rows.len().saturating_sub(continuing_pressure_count);
            let leader_label = if rows.is_empty() {
                "暂无 bucket 被 24h 护栏拦住回退".to_string()
            } else if continuing_pressure_count > stabilizing_count {
                "24h 护栏拦住的 bucket 目前仍以继续承压为主".to_string()
            } else if stabilizing_count > continuing_pressure_count {
                "24h 护栏拦住的 bucket 目前以逐步稳定为主".to_string()
            } else {
                "24h 护栏拦住的 bucket 当前承压与稳定信号接近".to_string()
            };
            let cadence_blocked_count_all = all_patch_effects
                .iter()
                .filter(|patch| {
                    patch
                        .get("blocked_by_relax_step_cooldown")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                })
                .count();
            json!({
                "blocked_count": rows.len(),
                "continuing_pressure_count": continuing_pressure_count,
                "stabilizing_count": stabilizing_count,
                "cadence_blocked_count": cadence_blocked_count_all,
                "leader_label": leader_label,
                "rows": rows,
            })
        };
        let relax_pressure_summary = {
            let mut same_day_count = 0usize;
            let mut next_day_count = 0usize;
            let mut mixed_count = 0usize;
            let mut unknown_count = 0usize;
            let mut same_day_pressure_score = 0i64;
            let mut next_day_pressure_score = 0i64;
            for patch in &all_patch_effects {
                if patch.get("recommended_action").and_then(Value::as_str) != Some("consider_relax")
                {
                    continue;
                }
                let scope_labels = patch
                    .get("scope_labels")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let has_same_day = scope_labels.iter().any(|label| {
                    label
                        .as_str()
                        .map(|value| value.starts_with("same_day / "))
                        .unwrap_or(false)
                });
                let has_next_day = scope_labels.iter().any(|label| {
                    label
                        .as_str()
                        .map(|value| value.starts_with("next_day / "))
                        .unwrap_or(false)
                });
                let tier_weight = if patch
                    .get("relax_uses_entry_fallback")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    3
                } else if patch
                    .get("relax_uses_fallback_post_entry")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    2
                } else if patch
                    .get("relax_uses_conservative_post_entry")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    1
                } else {
                    0
                };
                match (has_same_day, has_next_day) {
                    (true, false) => {
                        same_day_count += 1;
                        same_day_pressure_score += tier_weight;
                    }
                    (false, true) => {
                        next_day_count += 1;
                        next_day_pressure_score += tier_weight;
                    }
                    (true, true) => mixed_count += 1,
                    (false, false) => unknown_count += 1,
                }
            }
            let leader_label = if same_day_pressure_score == 0 && next_day_pressure_score == 0 {
                "暂无明确回退压力"
            } else if same_day_pressure_score > next_day_pressure_score {
                "当前回退压力主要来自 same-day"
            } else if next_day_pressure_score > same_day_pressure_score {
                "当前回退压力主要来自 next-day"
            } else {
                "same-day / next-day 回退压力接近"
            };
            json!({
                "leader_label": leader_label,
                "same_day_count": same_day_count,
                "next_day_count": next_day_count,
                "mixed_count": mixed_count,
                "unknown_count": unknown_count,
                "same_day_pressure_score": same_day_pressure_score,
                "next_day_pressure_score": next_day_pressure_score,
            })
        };
        let priority_bucket_summary = build_priority_bucket_summary(
            &current_scope_scores,
            &patches,
            &entry_patch_rows,
            &post_entry_patch_rows,
            &crypto_cooldown_buckets,
        );
        json!({
            "recent_count": patches.len(),
            "patches": patches,
            "long_window_relax_guard_summary": long_window_relax_guard_summary,
            "relax_pressure_summary": relax_pressure_summary,
            "priority_bucket_summary": priority_bucket_summary,
        })
    } else {
        json!({
            "recent_count": 0,
            "patches": [],
            "long_window_relax_guard_summary": {
                "blocked_count": 0,
                "continuing_pressure_count": 0,
                "stabilizing_count": 0,
                "cadence_blocked_count": 0,
                "leader_label": "暂无 bucket 被 24h 护栏拦住回退",
                "rows": [],
            },
            "relax_pressure_summary": {
                "leader_label": "暂无明确回退压力",
                "same_day_count": 0,
                "next_day_count": 0,
                "mixed_count": 0,
                "unknown_count": 0,
                "same_day_pressure_score": 0,
                "next_day_pressure_score": 0,
            },
            "priority_bucket_summary": {
                "row_count": 0,
                "leader_scope_label": "",
                "leader_label": "当前没有明显恶化的冷却 bucket",
                "leader_recommended_action": "observe",
                "leader_action_label": "继续观察",
                "leader_field_action_label": "暂无字段级建议",
                "leader_target_fields": [],
                "subtype_focus_label": "暂无明显主导 subtype",
                "subtype_focus_action_label": "subtype 建议：继续观察",
                "subtype_focus_summary_label": "当前暂无明确主导 subtype 动作",
                "subtype_focus_field_summary_label": "subtype 字段建议：继续观察",
                "subtype_focus_event_subtype": "",
                "subtype_focus_scope_labels": [],
                "subtype_focus_recommended_action": "observe",
                "subtype_focus_target_fields": [],
                "asset_focus_label": "暂无明显主导资产",
                "asset_focus_action_label": "资产建议：继续观察",
                "asset_focus_summary_label": "当前暂无明确主导资产动作",
                "asset_focus_field_summary_label": "资产字段建议：继续观察",
                "asset_focus_asset": "",
                "asset_focus_scope_labels": [],
                "asset_focus_recommended_action": "observe",
                "asset_focus_target_fields": [],
                "rows": [],
            },
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
    let crypto_subtype_window_summary = if let Some(repo) = &state.repository {
        let positions = state.positions.read().await.clone();
        let trade_rows = repo
            .load_trade_history(500, Some("crypto_alpha"), None, None)
            .await
            .unwrap_or_default();
        let rows = build_crypto_subtype_window_summary(&trade_rows, &positions, &recent_exits)
            .into_iter()
            .map(|entry| {
                json!({
                    "window_label": entry.window_label,
                    "resolution_bucket": entry.resolution_bucket,
                    "shape": entry.shape,
                    "asset_class": entry.asset_class,
                    "event_subtype": entry.event_subtype,
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
    let crypto_asset_long_window_summary = if let Some(repo) = &state.repository {
        let positions = state.positions.read().await.clone();
        let trade_rows = repo
            .load_trade_history(500, Some("crypto_alpha"), None, None)
            .await
            .unwrap_or_default();
        let rows = build_crypto_asset_long_window_summary(&trade_rows, &positions, &recent_exits);
        let leader = rows.first().cloned();
        let leader_label = if let Some(leader) = &leader {
            format!("近 24h 压力最集中的资产是 {}", leader.asset)
        } else {
            "近 24h 暂无明显主导资产".to_string()
        };
        let leader_action_label = if let Some(leader) = &leader {
            if leader.bad_exit_count > 0
                || leader.realized_pnl < Decimal::ZERO
                || leader.open_pnl_bid < Decimal::ZERO
            {
                format!("资产建议：继续观察 {}，暂不建议放松", leader.asset)
            } else {
                format!("资产建议：{} 当前压力较低", leader.asset)
            }
        } else {
            "资产建议：继续观察".to_string()
        };
        json!({
            "row_count": rows.len(),
            "leader_asset": leader.as_ref().map(|row| row.asset.clone()),
            "leader_label": leader_label,
            "leader_action_label": leader_action_label,
            "rows": rows.into_iter().map(|entry| json!({
                "asset": entry.asset,
                "trade_count": entry.trade_count,
                "realized_pnl": entry.realized_pnl,
                "open_positions": entry.open_positions,
                "open_pnl_bid": entry.open_pnl_bid,
                "bad_exit_count": entry.bad_exit_count,
                "pressure_score": entry.pressure_score,
            })).collect::<Vec<_>>(),
        })
    } else {
        json!({
            "row_count": 0,
            "leader_asset": null,
            "leader_label": "近 24h 暂无明显主导资产",
            "leader_action_label": "资产建议：继续观察",
            "rows": [],
        })
    };
    let crypto_same_day_major_range_summary = if let Some(repo) = &state.repository {
        let positions = state.positions.read().await.clone();
        let trade_rows = repo
            .load_trade_history(500, Some("crypto_alpha"), None, None)
            .await
            .unwrap_or_default();
        let recent_exits = crate::diagnostics::recent_crypto_exit_decisions()
            .into_iter()
            .take(200)
            .collect::<Vec<_>>();
        let current_scope_scores =
            build_current_cooldown_scope_scores(&crypto_cooldown_buckets, &trade_rows, &positions);
        let patches = crypto_auto_patch_effectiveness_summary
            .get("patches")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let entry_patch_rows = crypto_override_patch_preview
            .get("rows")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let post_entry_patch_rows = crypto_post_entry_override_patch_preview
            .get("rows")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        build_same_day_major_range_summary(
            &current_scope_scores,
            &patches,
            &entry_patch_rows,
            &post_entry_patch_rows,
            &trade_rows,
            &positions,
            &recent_exits,
        )
    } else {
        json!({
            "leader_scope_label": "",
            "leader_label": "same_day major range 当前无明显样本压力",
            "recommended_action": "observe",
            "action_label": "same_day major range 建议：当前无 active cooldown scope，以下为模板级收紧方向",
            "field_summary_label": "same_day major range 模板字段建议：先看 max_spread_multiplier",
            "uses_template_guidance": true,
            "target_fields": [],
            "trade_count_24h": 0,
            "realized_pnl_24h": Decimal::ZERO,
            "bad_exit_count_24h": 0,
            "open_positions": 0,
            "open_pnl_bid": Decimal::ZERO,
        })
    };
    let crypto_eth_same_day_range_window_summary = if let Some(repo) = &state.repository {
        let positions = state.positions.read().await.clone();
        let trade_rows = repo
            .load_trade_history(500, Some("crypto_alpha"), None, None)
            .await
            .unwrap_or_default();
        let recent_exits = crate::diagnostics::recent_crypto_exit_decisions()
            .into_iter()
            .take(200)
            .collect::<Vec<_>>();
        build_eth_same_day_range_window_summary(
            &trade_rows,
            &positions,
            &recent_exits,
            &crypto_cooldown_buckets,
        )
    } else {
        json!({
            "leader_label": "ETH same-day range 当前无明显样本压力",
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

    let weather_retention_minutes = crate::diagnostics::weather_rejection_retention_minutes();

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
        "weather_rejection_summary": {
            "retained_window_minutes": weather_retention_minutes,
            "retained_count": sum_counts(&weather_reason_counts_retained),
            "unsupported_city_count": weather_reason_counts_retained.get("unsupported_city").copied().unwrap_or(0),
            "retained_top": sorted_count_entries(&weather_blocker_counts_retained),
            "recent_1h": {
                "count": sum_counts(&weather_reason_counts_1h),
                "unsupported_city_count": weather_reason_counts_1h.get("unsupported_city").copied().unwrap_or(0),
                "top_reasons": sorted_count_entries(&weather_blocker_counts_1h),
                "top_reason": top_count_entry(&weather_blocker_counts_1h),
                "top_spread_cities": sorted_count_entries(&weather_spread_city_counts_1h),
                "top_price_cities": sorted_count_entries(&weather_price_city_counts_1h),
            },
            "recent_6h": {
                "count": sum_counts(&weather_reason_counts_6h),
                "unsupported_city_count": weather_reason_counts_6h.get("unsupported_city").copied().unwrap_or(0),
                "top_reasons": sorted_count_entries(&weather_blocker_counts_6h),
                "top_reason": top_count_entry(&weather_blocker_counts_6h),
                "top_spread_cities": sorted_count_entries(&weather_spread_city_counts_6h),
                "top_price_cities": sorted_count_entries(&weather_price_city_counts_6h),
            },
        },
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
                "last_8": count_reason_window(&recent_gate_scales, 8),
                "last_24": count_reason_window(&recent_gate_scales, 24),
            },
            "subtype_windows": {
                "last_8": count_subtype_window(&recent_gate_scales, 8),
                "last_24": count_subtype_window(&recent_gate_scales, 24),
            },
            "asset_windows": {
                "last_8": count_asset_window(&recent_gate_scales, 8),
                "last_24": count_asset_window(&recent_gate_scales, 24),
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
        "crypto_subtype_window_summary": crypto_subtype_window_summary,
        "crypto_asset_long_window_summary": crypto_asset_long_window_summary,
        "crypto_generic_day_market_summary": crypto_generic_day_market_summary,
        "crypto_same_day_major_range_summary": crypto_same_day_major_range_summary,
        "crypto_eth_same_day_range_window_summary": crypto_eth_same_day_range_window_summary,
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
    uses_conservative_post_entry: Option<bool>,
    uses_fallback_post_entry: Option<bool>,
    uses_entry_fallback: Option<bool>,
    selected_target_fields: Option<Vec<String>>,
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
    selected_field_count: usize,
    field_level: bool,
    selected_target_fields: Vec<String>,
    uses_conservative_post_entry: bool,
    uses_fallback_post_entry: bool,
    uses_entry_fallback: bool,
    focus_label: Option<String>,
    recommended_action: Option<String>,
    action_label: Option<String>,
    note: Option<String>,
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
        crate::diagnostics::recent_crypto_candidate_decisions_limited(1000)
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
        if let Ok(target_without_year) =
            chrono::NaiveDate::parse_from_str(candidate.trim(), "%B %d")
            && let Some(target_date) = chrono::NaiveDate::from_ymd_opt(
                reference.year(),
                target_without_year.month(),
                target_without_year.day(),
            )
        {
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

fn infer_exit_shape_label(market_type: Option<&str>, question: Option<&str>) -> &'static str {
    if infer_trade_shape(question) == "range" {
        "range"
    } else {
        normalized_market_shape_label(market_type)
    }
}

fn classify_capital_efficiency_exit(best_bid: Decimal, avg_cost: Decimal) -> &'static str {
    if avg_cost <= Decimal::ZERO {
        return "flat";
    }
    let tolerance = avg_cost * Decimal::new(1, 2);
    if best_bid >= avg_cost + tolerance {
        "profit"
    } else if best_bid <= avg_cost - tolerance {
        "loss"
    } else {
        "flat"
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
    audit_rows: &[ConfigHistoryRow],
    trade_rows: &[TradeHistoryRow],
    positions: &[PositionApiEntry],
    recent_exits: &[crate::diagnostics::CryptoExitDecision],
) -> Vec<CryptoAutoPatchEffectivenessEntry> {
    audit_rows
        .iter()
        .filter(|row| {
            row.data
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
                                    && infer_exit_shape_label(
                                        decision.market_type.as_deref(),
                                        Some(decision.question.as_str()),
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
        ("72h", chrono::Duration::hours(72)),
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
                                && infer_exit_shape_label(
                                    decision.market_type.as_deref(),
                                    Some(decision.question.as_str()),
                                ) == shape
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

fn build_crypto_subtype_window_summary(
    trade_rows: &[TradeHistoryRow],
    positions: &[PositionApiEntry],
    recent_exits: &[crate::diagnostics::CryptoExitDecision],
) -> Vec<CryptoSubtypeWindowSummaryEntry> {
    let now = Utc::now();
    let windows = [
        ("1h", chrono::Duration::hours(1)),
        ("6h", chrono::Duration::hours(6)),
        ("24h", chrono::Duration::hours(24)),
    ];
    let buckets = ["same_day", "next_day"];
    let shapes = ["range", "directional"];
    let asset_classes = ["major", "alt"];
    let mut subtype_labels = trade_rows
        .iter()
        .filter_map(|trade| trade.question.as_deref())
        .map(|question| infer_question_event_subtype(Some(question)).to_string())
        .chain(
            positions
                .iter()
                .filter_map(|position| position.question.as_deref())
                .map(|question| infer_question_event_subtype(Some(question)).to_string()),
        )
        .chain(recent_exits.iter().map(|decision| {
            infer_question_event_subtype(Some(decision.question.as_str())).to_string()
        }))
        .collect::<std::collections::BTreeSet<_>>();
    if subtype_labels.is_empty() {
        subtype_labels.insert("generic".to_string());
    }
    let mut entries = Vec::new();

    for (window_label, window_duration) in windows {
        let window_start = now - window_duration;
        for resolution_bucket in buckets {
            for shape in shapes {
                for asset_class in asset_classes {
                    for event_subtype in &subtype_labels {
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
                                    && infer_question_event_subtype(trade.question.as_deref())
                                        == event_subtype.as_str()
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
                                    && infer_question_event_subtype(trade.question.as_deref())
                                        == event_subtype.as_str()
                            })
                            .map(|trade| trade.actual_profit.unwrap_or(Decimal::ZERO))
                            .sum();
                        let bad_exit_count = recent_exits
                            .iter()
                            .filter(|decision| {
                                decision.recorded_at >= window_start
                                    && normalized_resolution_bucket_label(
                                        decision.days_to_resolution,
                                    ) == resolution_bucket
                                    && infer_exit_shape_label(
                                        decision.market_type.as_deref(),
                                        Some(decision.question.as_str()),
                                    ) == shape
                                    && asset_class_for_asset_label(
                                        decision.asset.as_deref().unwrap_or_default(),
                                    ) == asset_class
                                    && infer_question_event_subtype(Some(
                                        decision.question.as_str(),
                                    )) == event_subtype.as_str()
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
                                    && infer_question_event_subtype(position.question.as_deref())
                                        == event_subtype.as_str()
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
                                    && infer_question_event_subtype(position.question.as_deref())
                                        == event_subtype.as_str()
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
                        entries.push(CryptoSubtypeWindowSummaryEntry {
                            window_label,
                            resolution_bucket: resolution_bucket.to_string(),
                            shape: shape.to_string(),
                            asset_class: asset_class.to_string(),
                            event_subtype: event_subtype.clone(),
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
            .then_with(|| a.event_subtype.cmp(&b.event_subtype))
            .then_with(|| a.shape.cmp(&b.shape))
    });
    entries
}

fn build_crypto_asset_long_window_summary(
    trade_rows: &[TradeHistoryRow],
    positions: &[PositionApiEntry],
    recent_exits: &[crate::diagnostics::CryptoExitDecision],
) -> Vec<CryptoAssetLongWindowSummaryEntry> {
    let now = Utc::now();
    let window_start = now - chrono::Duration::hours(24);
    let assets = trade_rows
        .iter()
        .filter_map(|trade| trade.question.as_deref())
        .map(|question| infer_trade_asset(Some(question)).to_string())
        .chain(
            positions
                .iter()
                .filter_map(|position| position.asset.clone()),
        )
        .chain(
            recent_exits
                .iter()
                .filter_map(|decision| decision.asset.clone()),
        )
        .filter(|asset| !asset.is_empty())
        .collect::<std::collections::BTreeSet<_>>();
    if assets.is_empty() {
        return Vec::new();
    }
    let mut rows = Vec::new();
    for asset in assets.iter() {
        let trade_count = trade_rows
            .iter()
            .filter(|trade| {
                let executed_at = trade.executed_at.unwrap_or(trade.created_at);
                executed_at >= window_start
                    && infer_trade_asset(trade.question.as_deref()) == *asset
            })
            .count();
        let realized_pnl: Decimal = trade_rows
            .iter()
            .filter(|trade| {
                let executed_at = trade.executed_at.unwrap_or(trade.created_at);
                executed_at >= window_start
                    && infer_trade_asset(trade.question.as_deref()) == *asset
            })
            .map(|trade| trade.actual_profit.unwrap_or(Decimal::ZERO))
            .sum();
        let bad_exit_count = recent_exits
            .iter()
            .filter(|decision| {
                decision.recorded_at >= window_start
                    && decision.asset.as_deref() == Some(asset.as_str())
                    && matches!(
                        decision.reason.as_str(),
                        "model_reversal" | "relative_stop_loss"
                    )
            })
            .count();
        let open_positions = positions
            .iter()
            .filter(|position| position.asset.as_deref() == Some(asset.as_str()))
            .count();
        let open_pnl_bid: Decimal = positions
            .iter()
            .filter(|position| position.asset.as_deref() == Some(asset.as_str()))
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
        let pressure_score = (bad_exit_count as i64) * 4_000
            + decimal_loss_score(realized_pnl) * 5
            + decimal_loss_score(open_pnl_bid) * 2;
        rows.push(CryptoAssetLongWindowSummaryEntry {
            asset: asset.clone(),
            trade_count,
            realized_pnl,
            open_positions,
            open_pnl_bid,
            bad_exit_count,
            pressure_score,
        });
    }
    rows.sort_by(|a, b| {
        b.pressure_score
            .cmp(&a.pressure_score)
            .then_with(|| a.asset.cmp(&b.asset))
    });
    rows
}

fn build_same_day_major_range_summary(
    current_scope_scores: &std::collections::HashMap<String, (i64, i64, i64, i64)>,
    patches: &[Value],
    entry_patch_rows: &[Value],
    post_entry_patch_rows: &[Value],
    trade_rows: &[TradeHistoryRow],
    positions: &[PositionApiEntry],
    recent_exits: &[crate::diagnostics::CryptoExitDecision],
) -> Value {
    let now = Utc::now();
    let window_start = now - chrono::Duration::hours(24);
    let efficiency_exit_decisions = recent_exits
        .iter()
        .filter(|decision| {
            decision.recorded_at >= window_start
                && normalized_resolution_bucket_label(decision.days_to_resolution) == "same_day"
                && infer_exit_shape_label(
                    decision.market_type.as_deref(),
                    Some(decision.question.as_str()),
                ) == "range"
                && asset_class_for_asset_label(decision.asset.as_deref().unwrap_or_default())
                    == "major"
                && decision.reason == "capital_efficiency"
        })
        .collect::<Vec<_>>();
    let matching_scopes = current_scope_scores
        .iter()
        .filter_map(|(scope_label, (priority_score, _, _, _))| {
            let (resolution_bucket, asset_class, _, shape) =
                parse_bucketed_shaped_scope_label(scope_label)?;
            if resolution_bucket == "same_day" && asset_class == "major" && shape == "range" {
                Some((scope_label.clone(), *priority_score))
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    let leader_scope = matching_scopes
        .iter()
        .max_by_key(|(_, priority_score)| *priority_score)
        .map(|(scope_label, _)| scope_label.clone())
        .unwrap_or_default();
    let recommended_action = if !leader_scope.is_empty() {
        patches
            .iter()
            .find(|patch| {
                patch
                    .get("scope_labels")
                    .and_then(Value::as_array)
                    .map(|labels| {
                        labels
                            .iter()
                            .filter_map(Value::as_str)
                            .any(|label| label == leader_scope)
                    })
                    .unwrap_or(false)
            })
            .and_then(|patch| patch.get("recommended_action"))
            .and_then(Value::as_str)
            .unwrap_or("observe")
            .to_string()
    } else {
        "observe".to_string()
    };
    let using_template_guidance = leader_scope.is_empty();
    let mut target_fields = if !leader_scope.is_empty() && recommended_action == "consider_relax" {
        build_scope_relax_field_targets(entry_patch_rows, post_entry_patch_rows, &leader_scope, 4)
    } else if !leader_scope.is_empty() {
        let mut fields = collect_scope_field_targets_by_support(entry_patch_rows, &leader_scope, 4);
        if fields.len() < 4 {
            let remaining = 4usize.saturating_sub(fields.len());
            let mut post_entry_fields = collect_scope_field_targets_by_support(
                post_entry_patch_rows,
                &leader_scope,
                remaining,
            );
            fields.append(&mut post_entry_fields);
            fields.dedup();
            fields.truncate(4);
        }
        fields
    } else {
        vec![
            "max_spread_multiplier".to_string(),
            "size_multiplier".to_string(),
            "hold_edge_multiplier".to_string(),
            "capital_efficiency_multiplier".to_string(),
        ]
    };
    target_fields.dedup();

    let trade_count = trade_rows
        .iter()
        .filter(|trade| {
            let executed_at = trade.executed_at.unwrap_or(trade.created_at);
            executed_at >= window_start
                && infer_trade_resolution_bucket(
                    trade.question.as_deref(),
                    trade.executed_at,
                    trade.created_at,
                ) == "same_day"
                && infer_trade_shape(trade.question.as_deref()) == "range"
                && asset_class_for_asset_label(infer_trade_asset(trade.question.as_deref()))
                    == "major"
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
                ) == "same_day"
                && infer_trade_shape(trade.question.as_deref()) == "range"
                && asset_class_for_asset_label(infer_trade_asset(trade.question.as_deref()))
                    == "major"
        })
        .map(|trade| trade.actual_profit.unwrap_or(Decimal::ZERO))
        .sum();
    let bad_exit_count = recent_exits
        .iter()
        .filter(|decision| {
            decision.recorded_at >= window_start
                && normalized_resolution_bucket_label(decision.days_to_resolution) == "same_day"
                && infer_exit_shape_label(
                    decision.market_type.as_deref(),
                    Some(decision.question.as_str()),
                ) == "range"
                && asset_class_for_asset_label(decision.asset.as_deref().unwrap_or_default())
                    == "major"
                && matches!(
                    decision.reason.as_str(),
                    "model_reversal" | "relative_stop_loss"
                )
        })
        .count();
    let capital_efficiency_exit_count = efficiency_exit_decisions.len();
    let capital_efficiency_profit_exit_count = efficiency_exit_decisions
        .iter()
        .filter(|decision| {
            classify_capital_efficiency_exit(decision.best_bid, decision.avg_cost) == "profit"
        })
        .count();
    let capital_efficiency_loss_exit_count = efficiency_exit_decisions
        .iter()
        .filter(|decision| {
            classify_capital_efficiency_exit(decision.best_bid, decision.avg_cost) == "loss"
        })
        .count();
    let capital_efficiency_flat_exit_count = capital_efficiency_exit_count
        .saturating_sub(capital_efficiency_profit_exit_count + capital_efficiency_loss_exit_count);
    let open_positions = positions
        .iter()
        .filter(|position| {
            position.resolution_bucket.as_deref() == Some("same_day")
                && infer_position_shape(position) == "range"
                && asset_class_for_asset_label(position.asset.as_deref().unwrap_or_default())
                    == "major"
        })
        .count();
    let open_pnl_bid: Decimal = positions
        .iter()
        .filter(|position| {
            position.resolution_bucket.as_deref() == Some("same_day")
                && infer_position_shape(position) == "range"
                && asset_class_for_asset_label(position.asset.as_deref().unwrap_or_default())
                    == "major"
        })
        .map(|position| {
            position
                .unrealized_pnl_bid
                .or(position.unrealized_pnl)
                .unwrap_or(Decimal::ZERO)
        })
        .sum();
    let leader_label = if trade_count == 0
        && realized_pnl == Decimal::ZERO
        && bad_exit_count == 0
        && open_positions == 0
        && open_pnl_bid == Decimal::ZERO
    {
        "same_day major range 当前无明显样本压力".to_string()
    } else {
        format!(
            "same_day major range 近24h 成交 {} 笔，坏退出 {}，效率退出 {}，已实现 ${:.2}，当前持仓 {} 笔 / Bid 浮盈亏 ${:.2}",
            trade_count,
            bad_exit_count,
            capital_efficiency_exit_count,
            realized_pnl,
            open_positions,
            open_pnl_bid
        )
    };
    let action_label = if using_template_guidance {
        "same_day major range 建议：当前无 active cooldown scope，以下为模板级收紧方向".to_string()
    } else if target_fields.is_empty() {
        format!(
            "same_day major range 建议：{}",
            auto_patch_action_label(&recommended_action)
        )
    } else {
        format!(
            "same_day major range 建议：{}（{}）",
            auto_patch_action_label(&recommended_action),
            target_fields.join(" / ")
        )
    };
    let field_summary_label = if using_template_guidance {
        format!(
            "same_day major range 模板字段建议：先看 {}",
            target_fields
                .first()
                .cloned()
                .unwrap_or_else(|| "max_spread_multiplier".to_string())
        )
    } else if target_fields.is_empty() {
        "same_day major range 字段建议：继续观察".to_string()
    } else {
        format!("same_day major range 字段建议：先动 {}", target_fields[0])
    };

    json!({
        "leader_scope_label": leader_scope,
        "leader_label": leader_label,
        "recommended_action": recommended_action,
        "action_label": action_label,
        "field_summary_label": field_summary_label,
        "uses_template_guidance": using_template_guidance,
        "target_fields": target_fields,
        "trade_count_24h": trade_count,
        "realized_pnl_24h": realized_pnl,
        "bad_exit_count_24h": bad_exit_count,
        "capital_efficiency_exit_count_24h": capital_efficiency_exit_count,
        "capital_efficiency_profit_exit_count_24h": capital_efficiency_profit_exit_count,
        "capital_efficiency_loss_exit_count_24h": capital_efficiency_loss_exit_count,
        "capital_efficiency_flat_exit_count_24h": capital_efficiency_flat_exit_count,
        "open_positions": open_positions,
        "open_pnl_bid": open_pnl_bid,
    })
}

fn build_eth_same_day_range_window_summary(
    trade_rows: &[TradeHistoryRow],
    positions: &[PositionApiEntry],
    recent_exits: &[crate::diagnostics::CryptoExitDecision],
    cooldown_buckets: &[Value],
) -> Value {
    fn hourly_eth_range_pressure(
        hours: i64,
        bad_exit_count: usize,
        loss_efficiency_exit_count: usize,
        flat_efficiency_exit_count: usize,
    ) -> f64 {
        let hours = hours.max(1) as f64;
        ((bad_exit_count as f64) * 3.0
            + (loss_efficiency_exit_count as f64) * 2.0
            + (flat_efficiency_exit_count as f64))
            / hours
    }

    let now = Utc::now();
    let windows = [
        ("1h", chrono::Duration::hours(1)),
        ("6h", chrono::Duration::hours(6)),
        ("24h", chrono::Duration::hours(24)),
        ("72h", chrono::Duration::hours(72)),
    ];
    let mut rows = Vec::new();
    let mut leader_trade_count = 0usize;
    let mut leader_realized_pnl = Decimal::ZERO;
    let mut leader_bad_exit_count = 0usize;
    let mut leader_capital_efficiency_exit_count = 0usize;
    let mut leader_capital_efficiency_profit_exit_count = 0usize;
    let mut leader_capital_efficiency_loss_exit_count = 0usize;
    let mut leader_capital_efficiency_flat_exit_count = 0usize;
    let mut leader_open_positions = 0usize;
    let mut leader_open_pnl_bid = Decimal::ZERO;
    for (window_label, window_duration) in windows {
        let window_start = now - window_duration;
        let efficiency_exit_decisions = recent_exits
            .iter()
            .filter(|decision| {
                decision.recorded_at >= window_start
                    && normalized_resolution_bucket_label(decision.days_to_resolution) == "same_day"
                    && infer_exit_shape_label(
                        decision.market_type.as_deref(),
                        Some(decision.question.as_str()),
                    ) == "range"
                    && decision.asset.as_deref() == Some("Ethereum")
                    && decision.reason == "capital_efficiency"
            })
            .collect::<Vec<_>>();
        let trade_count = trade_rows
            .iter()
            .filter(|trade| {
                let executed_at = trade.executed_at.unwrap_or(trade.created_at);
                executed_at >= window_start
                    && infer_trade_resolution_bucket(
                        trade.question.as_deref(),
                        trade.executed_at,
                        trade.created_at,
                    ) == "same_day"
                    && infer_trade_shape(trade.question.as_deref()) == "range"
                    && infer_trade_asset(trade.question.as_deref()) == "Ethereum"
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
                    ) == "same_day"
                    && infer_trade_shape(trade.question.as_deref()) == "range"
                    && infer_trade_asset(trade.question.as_deref()) == "Ethereum"
            })
            .map(|trade| trade.actual_profit.unwrap_or(Decimal::ZERO))
            .sum();
        let bad_exit_count = recent_exits
            .iter()
            .filter(|decision| {
                decision.recorded_at >= window_start
                    && normalized_resolution_bucket_label(decision.days_to_resolution) == "same_day"
                    && infer_exit_shape_label(
                        decision.market_type.as_deref(),
                        Some(decision.question.as_str()),
                    ) == "range"
                    && decision.asset.as_deref() == Some("Ethereum")
                    && matches!(
                        decision.reason.as_str(),
                        "model_reversal" | "relative_stop_loss"
                    )
            })
            .count();
        let capital_efficiency_exit_count = efficiency_exit_decisions.len();
        let capital_efficiency_profit_exit_count = efficiency_exit_decisions
            .iter()
            .filter(|decision| {
                classify_capital_efficiency_exit(decision.best_bid, decision.avg_cost) == "profit"
            })
            .count();
        let capital_efficiency_loss_exit_count = efficiency_exit_decisions
            .iter()
            .filter(|decision| {
                classify_capital_efficiency_exit(decision.best_bid, decision.avg_cost) == "loss"
            })
            .count();
        let capital_efficiency_flat_exit_count = capital_efficiency_exit_count.saturating_sub(
            capital_efficiency_profit_exit_count + capital_efficiency_loss_exit_count,
        );
        let open_positions = positions
            .iter()
            .filter(|position| {
                position.resolution_bucket.as_deref() == Some("same_day")
                    && infer_position_shape(position) == "range"
                    && position.asset.as_deref() == Some("Ethereum")
            })
            .count();
        let open_pnl_bid: Decimal = positions
            .iter()
            .filter(|position| {
                position.resolution_bucket.as_deref() == Some("same_day")
                    && infer_position_shape(position) == "range"
                    && position.asset.as_deref() == Some("Ethereum")
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
        if window_label == "24h" {
            leader_trade_count = trade_count;
            leader_realized_pnl = realized_pnl;
            leader_bad_exit_count = bad_exit_count;
            leader_capital_efficiency_exit_count = capital_efficiency_exit_count;
            leader_capital_efficiency_profit_exit_count = capital_efficiency_profit_exit_count;
            leader_capital_efficiency_loss_exit_count = capital_efficiency_loss_exit_count;
            leader_capital_efficiency_flat_exit_count = capital_efficiency_flat_exit_count;
            leader_open_positions = open_positions;
            leader_open_pnl_bid = open_pnl_bid;
        }
        rows.push(json!({
            "window_label": window_label,
            "trade_count": trade_count,
            "realized_pnl": realized_pnl,
            "bad_exit_count": bad_exit_count,
            "capital_efficiency_exit_count": capital_efficiency_exit_count,
            "capital_efficiency_profit_exit_count": capital_efficiency_profit_exit_count,
            "capital_efficiency_loss_exit_count": capital_efficiency_loss_exit_count,
            "capital_efficiency_flat_exit_count": capital_efficiency_flat_exit_count,
            "open_positions": open_positions,
            "open_pnl_bid": open_pnl_bid,
        }));
    }
    let leader_label = if leader_trade_count == 0
        && leader_realized_pnl == Decimal::ZERO
        && leader_bad_exit_count == 0
        && leader_open_positions == 0
        && leader_open_pnl_bid == Decimal::ZERO
    {
        "ETH same-day range 当前无明显样本压力".to_string()
    } else {
        format!(
            "ETH same-day range 近24h：成交 {}，坏退出 {}，效率退出 {}，已实现 ${:.2}，当前持仓 {} / Bid 浮盈亏 ${:.2}",
            leader_trade_count,
            leader_bad_exit_count,
            leader_capital_efficiency_exit_count,
            leader_realized_pnl,
            leader_open_positions,
            leader_open_pnl_bid
        )
    };
    let active_eth_same_day_range_cooldowns = cooldown_buckets
        .iter()
        .filter(|bucket| {
            bucket.get("kind").and_then(Value::as_str) == Some("same_day_range")
                && bucket.get("asset").and_then(Value::as_str) == Some("Ethereum")
        })
        .count();
    let automation_status_label = if leader_bad_exit_count == 0
        && leader_capital_efficiency_loss_exit_count == 0
        && leader_capital_efficiency_flat_exit_count == 0
        && leader_open_positions == 0
    {
        "ETH same-day range 当前无活跃自动化压力".to_string()
    } else if active_eth_same_day_range_cooldowns > 0 {
        "ETH same-day range 坏退出已进入 cooldown 观察".to_string()
    } else if leader_bad_exit_count > 0 {
        "ETH same-day range 仍有坏退出，但当前未见 active cooldown".to_string()
    } else {
        "ETH same-day range 当前主要是效率退出，不是坏退出主导".to_string()
    };
    let validation_label = if (leader_bad_exit_count > 0
        || leader_capital_efficiency_loss_exit_count > 0)
        && active_eth_same_day_range_cooldowns > 0
    {
        format!(
            "ETH same-day range 验证：cooldown/auto-patch 已开始接住这类 exits（cooldown {} 个）",
            active_eth_same_day_range_cooldowns
        )
    } else if leader_bad_exit_count > 0 || leader_capital_efficiency_loss_exit_count > 0 {
        "ETH same-day range 验证：当前仍有退出压力，但 live cooldown/auto-patch 还没显式接住"
            .to_string()
    } else {
        "ETH same-day range 验证：当前未见需要自动化接管的坏退出压力".to_string()
    };
    let (recommended_action, target_field, action_label) =
        if leader_bad_exit_count > 0 || leader_capital_efficiency_loss_exit_count > 0 {
            if leader_capital_efficiency_flat_exit_count
                >= leader_capital_efficiency_loss_exit_count.max(1)
            {
                (
                    "continue_tighten",
                    "size_multiplier",
                    "ETH same-day range 建议：先继续收紧 size，再看 capital_efficiency".to_string(),
                )
            } else if leader_capital_efficiency_loss_exit_count >= leader_bad_exit_count.max(1) {
                (
                    "continue_tighten",
                    "capital_efficiency_multiplier",
                    "ETH same-day range 建议：继续收紧 capital_efficiency".to_string(),
                )
            } else {
                (
                    "continue_tighten",
                    "hold_edge_multiplier",
                    "ETH same-day range 建议：继续收紧 hold_edge / model_buffer".to_string(),
                )
            }
        } else if leader_capital_efficiency_flat_exit_count > 0 {
            (
                "observe",
                "size_multiplier",
                "ETH same-day range 建议：继续观察；如果近平盘 churn 持续，再收 size".to_string(),
            )
        } else if leader_capital_efficiency_profit_exit_count > 0 {
            (
                "observe",
                "capital_efficiency_multiplier",
                "ETH same-day range 建议：继续观察，当前更像盈利效率退出".to_string(),
            )
        } else {
            (
                "observe",
                "",
                "ETH same-day range 建议：继续观察".to_string(),
            )
        };
    let final_action_label = if recommended_action == "continue_tighten" && !target_field.is_empty()
    {
        format!("当前优先继续收紧 ETH same-day range 的 {}", target_field)
    } else if recommended_action == "observe" && !target_field.is_empty() {
        format!("当前优先继续观察 ETH same-day range 的 {}", target_field)
    } else {
        "当前优先继续观察 ETH same-day range".to_string()
    };
    let row_for = |label: &str| {
        rows.iter()
            .find(|row| row.get("window_label").and_then(Value::as_str) == Some(label))
    };
    let one_hour_bad_exits = row_for("1h")
        .and_then(|row| row.get("bad_exit_count"))
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    let one_hour_loss_efficiency_exits = row_for("1h")
        .and_then(|row| row.get("capital_efficiency_loss_exit_count"))
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    let one_hour_flat_efficiency_exits = row_for("1h")
        .and_then(|row| row.get("capital_efficiency_flat_exit_count"))
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    let six_hour_bad_exits = row_for("6h")
        .and_then(|row| row.get("bad_exit_count"))
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    let six_hour_loss_efficiency_exits = row_for("6h")
        .and_then(|row| row.get("capital_efficiency_loss_exit_count"))
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    let six_hour_flat_efficiency_exits = row_for("6h")
        .and_then(|row| row.get("capital_efficiency_flat_exit_count"))
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    let twenty_four_hour_pressure = hourly_eth_range_pressure(
        24,
        leader_bad_exit_count,
        leader_capital_efficiency_loss_exit_count,
        leader_capital_efficiency_flat_exit_count,
    );
    let one_hour_pressure = hourly_eth_range_pressure(
        1,
        one_hour_bad_exits,
        one_hour_loss_efficiency_exits,
        one_hour_flat_efficiency_exits,
    );
    let six_hour_pressure = hourly_eth_range_pressure(
        6,
        six_hour_bad_exits,
        six_hour_loss_efficiency_exits,
        six_hour_flat_efficiency_exits,
    );
    let live_effect_label = if leader_trade_count == 0
        && leader_bad_exit_count == 0
        && leader_capital_efficiency_exit_count == 0
    {
        "ETH same-day range 暂无新样本，先继续观察".to_string()
    } else if one_hour_pressure <= twenty_four_hour_pressure * 0.5
        && six_hour_pressure <= twenty_four_hour_pressure * 0.75
    {
        "ETH same-day range 近窗压力低于 24h 均值，收紧后有改善迹象".to_string()
    } else if one_hour_pressure >= twenty_four_hour_pressure * 1.25 {
        "ETH same-day range 近 1h 压力仍高于 24h 均值，收紧效果还不够".to_string()
    } else {
        "ETH same-day range 近窗压力与 24h 均值接近，仍需继续观察".to_string()
    };
    let reactivate_threshold_label =
        "重新激活收紧条件：1h 坏退出 >= 2，或 1h 亏损效率退出 >= 3，或 6h churn 压力重新高于 24h 均值".to_string();
    let observation_state_label = if leader_trade_count == 0
        && leader_bad_exit_count == 0
        && leader_capital_efficiency_exit_count == 0
        && active_eth_same_day_range_cooldowns == 0
    {
        "当前无活跃压力，保持观察，不继续收紧".to_string()
    } else {
        "当前仍需结合 live 样本继续观察是否要重新收紧".to_string()
    };
    let short_window_reactivation_label = if one_hour_bad_exits >= 2
        || one_hour_loss_efficiency_exits >= 3
        || six_hour_pressure > twenty_four_hour_pressure
    {
        "短窗恢复判定：已满足重新激活收紧条件".to_string()
    } else {
        "短窗恢复判定：仍未达到重新激活收紧阈值".to_string()
    };
    let auto_patch_rearm_label = if leader_trade_count == 0
        && active_eth_same_day_range_cooldowns == 0
    {
        "自动化恢复验证：当前无活跃样本；若 same-day range 再次进入 cooldown，auto-patch 会重新参与"
            .to_string()
    } else if active_eth_same_day_range_cooldowns > 0 {
        "自动化恢复验证：cooldown/auto-patch 当前已在链路中".to_string()
    } else {
        "自动化恢复验证：有新样本但暂未看到 cooldown/auto-patch 重新介入".to_string()
    };
    let spot_refresh_recommendation_label = if leader_bad_exit_count > 0
        || leader_capital_efficiency_loss_exit_count > 0
    {
        "现价提频建议：暂不建议提高 spot 刷新频率，当前更像 same-day range churn / post-entry 语义问题".to_string()
    } else {
        "现价提频建议：继续保持当前 spot 刷新频率".to_string()
    };
    json!({
        "leader_label": leader_label,
        "automation_status_label": automation_status_label,
        "validation_label": validation_label,
        "recommended_action": recommended_action,
        "target_field": target_field,
        "action_label": action_label,
        "final_action_label": final_action_label,
        "live_effect_label": live_effect_label,
        "reactivate_threshold_label": reactivate_threshold_label,
        "observation_state_label": observation_state_label,
        "short_window_reactivation_label": short_window_reactivation_label,
        "auto_patch_rearm_label": auto_patch_rearm_label,
        "spot_refresh_recommendation_label": spot_refresh_recommendation_label,
        "row_count": rows.len(),
        "rows": rows,
    })
}

fn build_crypto_generic_day_market_summary(
    candidate_decisions: &[crate::diagnostics::CryptoCandidateDecision],
) -> Value {
    let now = Utc::now();
    let windows = [
        ("1h", chrono::Duration::hours(1)),
        ("6h", chrono::Duration::hours(6)),
        ("24h", chrono::Duration::hours(24)),
    ];
    let mut rows = Vec::new();
    let mut leader_candidate_count = 0usize;
    let mut leader_range_count = 0usize;
    let mut leader_binary_count = 0usize;
    let mut leader_spread_reject_count = 0usize;
    let mut leader_viable_count = 0usize;
    let mut leader_top_assets: Vec<(String, usize)> = Vec::new();

    for (window_label, window_duration) in windows {
        let window_start = now - window_duration;
        let matching = candidate_decisions
            .iter()
            .filter(|decision| {
                decision.recorded_at >= window_start
                    && decision.selected_days_to_resolution == 0
                    && decision.event_subtype.as_deref().unwrap_or("generic") == "generic"
            })
            .collect::<Vec<_>>();

        let candidate_count = matching
            .iter()
            .map(|decision| decision.selected_condition_id)
            .collect::<std::collections::HashSet<_>>()
            .len();
        let range_count = matching
            .iter()
            .filter(|decision| {
                normalized_market_shape_label(Some(decision.selected_market_type.as_str()))
                    == "range"
            })
            .map(|decision| decision.selected_condition_id)
            .collect::<std::collections::HashSet<_>>()
            .len();
        let binary_count = matching
            .iter()
            .filter(|decision| {
                normalized_market_shape_label(Some(decision.selected_market_type.as_str()))
                    == "directional"
            })
            .map(|decision| decision.selected_condition_id)
            .collect::<std::collections::HashSet<_>>()
            .len();
        let spread_reject_count = matching
            .iter()
            .filter(|decision| {
                decision.action == "gate_reject" && decision.reason == "spread_too_wide"
            })
            .map(|decision| decision.selected_condition_id)
            .collect::<std::collections::HashSet<_>>()
            .len();
        let viable_count = matching
            .iter()
            .filter(|decision| decision.action != "gate_reject")
            .map(|decision| decision.selected_condition_id)
            .collect::<std::collections::HashSet<_>>()
            .len();
        let mut spread_asset_conditions: std::collections::HashMap<
            String,
            std::collections::HashSet<B256>,
        > = std::collections::HashMap::new();
        for decision in matching.iter().filter(|decision| {
            decision.action == "gate_reject" && decision.reason == "spread_too_wide"
        }) {
            spread_asset_conditions
                .entry(decision.asset.clone())
                .or_default()
                .insert(decision.selected_condition_id);
        }
        let spread_asset_counts: std::collections::HashMap<String, usize> = spread_asset_conditions
            .into_iter()
            .map(|(asset, conditions)| (asset, conditions.len()))
            .collect();
        let mut top_assets = spread_asset_counts
            .iter()
            .map(|(label, count)| json!({ "label": label, "count": count }))
            .collect::<Vec<_>>();
        top_assets.sort_by(|a, b| {
            let count_a = a.get("count").and_then(Value::as_u64).unwrap_or(0);
            let count_b = b.get("count").and_then(Value::as_u64).unwrap_or(0);
            let label_a = a.get("label").and_then(Value::as_str).unwrap_or("");
            let label_b = b.get("label").and_then(Value::as_str).unwrap_or("");
            count_b.cmp(&count_a).then_with(|| label_a.cmp(label_b))
        });
        let spread_reject_ratio = if candidate_count == 0 {
            Decimal::ZERO
        } else {
            Decimal::from(spread_reject_count as u64) / Decimal::from(candidate_count as u64)
        };

        if window_label == "24h" {
            leader_candidate_count = candidate_count;
            leader_range_count = range_count;
            leader_binary_count = binary_count;
            leader_spread_reject_count = spread_reject_count;
            leader_viable_count = viable_count;
            leader_top_assets = top_assets
                .iter()
                .filter_map(|value: &Value| {
                    Some((
                        value.get("label")?.as_str()?.to_string(),
                        value.get("count")?.as_u64()? as usize,
                    ))
                })
                .take(2)
                .collect();
        }

        rows.push(json!({
            "window_label": window_label,
            "candidate_count": candidate_count,
            "range_count": range_count,
            "binary_count": binary_count,
            "spread_reject_count": spread_reject_count,
            "viable_count": viable_count,
            "spread_reject_ratio": spread_reject_ratio,
            "top_assets": top_assets,
        }));
    }

    let leader_asset_label = if leader_top_assets.is_empty() {
        "generic same-day market".to_string()
    } else {
        leader_top_assets
            .iter()
            .map(|(asset, _)| asset.clone())
            .collect::<Vec<_>>()
            .join("/")
    };
    let leader_shape_label = if leader_range_count > leader_binary_count {
        "range"
    } else if leader_binary_count > leader_range_count {
        "binary"
    } else if leader_range_count > 0 || leader_binary_count > 0 {
        "mixed"
    } else {
        "none"
    };
    let leader_label = if leader_candidate_count == 0 {
        "generic same-day market 当前无明显候选样本".to_string()
    } else if leader_spread_reject_count
        >= leader_candidate_count.saturating_sub(leader_viable_count)
    {
        match leader_shape_label {
            "range" => format!(
                "当前主要因为 {} generic same-day range spread 过宽而无单",
                leader_asset_label
            ),
            "binary" => format!(
                "当前主要因为 {} generic same-day binary spread 过宽而无单",
                leader_asset_label
            ),
            _ => format!(
                "当前主要因为 {} generic same-day market spread 过宽而无单（range / binary 混合）",
                leader_asset_label
            ),
        }
    } else {
        format!(
            "generic same-day market 近24h：候选 {}，被 spread 挡掉 {}，仍可交易 {}",
            leader_candidate_count, leader_spread_reject_count, leader_viable_count
        )
    };
    let action_label = if leader_candidate_count == 0 {
        "当前 generic same-day market 样本很少，先继续观察，不建议全局放松".to_string()
    } else if leader_shape_label == "range" {
        "当前主导样本是 generic same-day range；现有 spread relief 只覆盖 binary，不建议继续沿用同一旋钮放松".to_string()
    } else if leader_shape_label == "mixed" {
        "当前 generic same-day 样本是 range / binary 混合；现有 spread relief 只直接覆盖 binary，先继续观察形态占比变化".to_string()
    } else if leader_spread_reject_count > leader_viable_count {
        format!(
            "如果要恢复新单，优先小步放松 {} generic same-day market 的 spread，不建议全局放松",
            leader_asset_label
        )
    } else {
        "generic same-day market 当前并非完全被 spread 主导，先继续观察".to_string()
    };
    let validation_label = if leader_candidate_count == 0 {
        "generic same-day market 验证：当前未见足够的新候选样本，先继续观察".to_string()
    } else if leader_shape_label == "range" && leader_viable_count == 0 {
        format!(
            "generic same-day market 验证：近24h 主导样本是 range，且仍几乎全部被 spread 挡掉；当前 binary-only spread relief 还接不到这部分（主资产：{}）",
            leader_asset_label
        )
    } else if leader_shape_label == "range" {
        format!(
            "generic same-day market 验证：近24h 主导样本是 range；当前 binary-only spread relief 不足以解释这部分恢复情况（主资产：{}）",
            leader_asset_label
        )
    } else if leader_shape_label == "mixed" {
        format!(
            "generic same-day market 验证：近24h 样本是 range / binary 混合；当前 binary-only spread relief 只能部分覆盖（主资产：{}）",
            leader_asset_label
        )
    } else if leader_viable_count == 0 {
        format!(
            "generic same-day market 验证：近24h 候选仍几乎全部被 spread 挡掉，尚未恢复成交条件（主资产：{}）",
            leader_asset_label
        )
    } else if leader_spread_reject_count > leader_viable_count {
        format!(
            "generic same-day market 验证：已有可交易候选，但 spread 仍是主导摩擦（主资产：{}）",
            leader_asset_label
        )
    } else {
        "generic same-day market 验证：spread relief 后已出现可交易候选，下一步应观察是否伴随坏退出抬头"
            .to_string()
    };
    let final_action_label = if leader_candidate_count == 0 {
        "当前 generic same-day market 无活跃样本，保持观察，不继续追加放松".to_string()
    } else if leader_shape_label == "range" && leader_viable_count == 0 {
        format!(
            "当前 generic same-day 主导样本是 range，且仍主要被 spread 挡住；现有 binary-only spread 放松不应被误当成这部分的下一步动作（主资产：{}）",
            leader_asset_label
        )
    } else if leader_shape_label == "range" {
        format!(
            "当前 generic same-day 主导样本仍是 range；先继续观察或单独评估 range 路径，不继续追加 binary-only spread 放松（主资产：{}）",
            leader_asset_label
        )
    } else if leader_shape_label == "mixed" {
        format!(
            "当前 generic same-day 样本仍是 range / binary 混合；先观察哪一类在主导恢复，不继续盲目追加统一 spread 放松（主资产：{}）",
            leader_asset_label
        )
    } else if leader_viable_count == 0 {
        format!(
            "当前 generic same-day market 仍主要被 spread 挡住；如果要恢复新单，优先只继续小步放松 {} generic 桶",
            leader_asset_label
        )
    } else if leader_spread_reject_count > leader_viable_count {
        format!(
            "当前 generic same-day market 已开始恢复可交易候选，但 spread 仍偏宽；先观察新成交是否伴随坏退出抬头（主资产：{}）",
            leader_asset_label
        )
    } else {
        "当前 generic same-day market 已恢复可交易候选，优先观察成交后的坏退出与效率退出，不继续追加 spread 放松".to_string()
    };

    json!({
        "leader_label": leader_label,
        "action_label": action_label,
        "validation_label": validation_label,
        "final_action_label": final_action_label,
        "row_count": rows.len(),
        "rows": rows,
    })
}

fn scope_has_repeated_effective_auto_patches(
    scope_labels: &[String],
    entries: &[CryptoAutoPatchEffectivenessEntry],
    min_effective_count: usize,
) -> bool {
    scope_effective_streak(scope_labels, entries) >= min_effective_count
}

fn scope_has_recent_patch_mode(
    audit_rows: &[ConfigHistoryRow],
    scope_labels: &[String],
    mode: &str,
    max_age_secs: i64,
) -> bool {
    let scope_set = scope_labels
        .iter()
        .map(|label| label.as_str())
        .collect::<std::collections::HashSet<_>>();
    audit_rows.iter().any(|row| {
        if row.data.get("mode").and_then(Value::as_str) != Some(mode) {
            return false;
        }
        if !row
            .data
            .get("runtime_applied")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            return false;
        }
        if (Utc::now() - row.created_at).num_seconds() > max_age_secs {
            return false;
        }
        row.data
            .get("scope_labels")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .any(|label| scope_set.contains(label))
            })
            .unwrap_or(false)
    })
}

fn scope_effective_streak(
    scope_labels: &[String],
    entries: &[CryptoAutoPatchEffectivenessEntry],
) -> usize {
    if scope_labels.is_empty() {
        return 0;
    }
    let mut matching = entries
        .iter()
        .filter(|entry| {
            scope_labels
                .iter()
                .all(|scope_label| entry.scope_labels.iter().any(|label| label == scope_label))
        })
        .collect::<Vec<_>>();
    matching.sort_by(|a, b| b.runtime_applied_at.cmp(&a.runtime_applied_at));
    let mut streak = 0usize;
    for entry in matching {
        if entry.outcome == "effective" {
            streak += 1;
        } else {
            break;
        }
    }
    streak
}

fn compute_scope_set_long_window_pressure(
    scope_labels: &[String],
    trade_rows: &[TradeHistoryRow],
    positions: &[PositionApiEntry],
    recent_exits: &[crate::diagnostics::CryptoExitDecision],
) -> i64 {
    scope_labels
        .iter()
        .filter_map(|label| parse_bucketed_shaped_scope_label(label))
        .map(|(resolution_bucket, asset_class, event_subtype, shape)| {
            compute_scope_window_pressure_score(
                trade_rows,
                positions,
                recent_exits,
                &resolution_bucket,
                &shape,
                &asset_class,
                &event_subtype,
                &[(chrono::Duration::hours(24), 1_i64)],
            )
        })
        .max()
        .unwrap_or(0)
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

fn rewrite_patch_rows_direction_for_fields(
    rows: &[Value],
    scope_labels: &std::collections::BTreeSet<String>,
    direction: &str,
    allowed_fields: &[&str],
) -> Vec<Value> {
    let allowed = allowed_fields
        .iter()
        .copied()
        .collect::<std::collections::HashSet<_>>();
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
                        if !allowed.contains(target_field) {
                            return None;
                        }
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

fn build_staged_relax_post_entry_rows(
    post_entry_rows: &[Value],
    relax_scope_labels: &std::collections::BTreeSet<String>,
) -> (Vec<Value>, bool, bool) {
    let staged_fields = [
        ["hold_edge_multiplier"].as_slice(),
        ["capital_efficiency_multiplier"].as_slice(),
        ["model_reversal_buffer_multiplier"].as_slice(),
        ["edge_decay_exit_multiplier"].as_slice(),
        ["edge_decay_confirmation_scan_multiplier"].as_slice(),
        ["edge_decay_confirmation_window_multiplier"].as_slice(),
        ["edge_decay_cooldown_multiplier"].as_slice(),
    ];
    let mut selected = Vec::new();
    let mut covered_scopes = std::collections::BTreeSet::new();
    for allowed_fields in staged_fields {
        let staged_rows = rewrite_patch_rows_direction_for_fields(
            post_entry_rows,
            relax_scope_labels,
            "loosen",
            allowed_fields,
        )
        .into_iter()
        .filter(|row| row.get("source_bucket").and_then(Value::as_str) != Some("legacy"))
        .filter(|row| {
            row_scope_key(row)
                .map(|label| !covered_scopes.contains(&label))
                .unwrap_or(false)
        })
        .collect::<Vec<_>>();
        selected.extend(staged_rows);
        covered_scopes = selected
            .iter()
            .filter_map(row_scope_key)
            .collect::<std::collections::BTreeSet<_>>();
    }
    let mut uses_fallback_post_entry = false;
    if covered_scopes.len() < relax_scope_labels.len() {
        let fallback_post_entry =
            rewrite_patch_rows_direction(post_entry_rows, relax_scope_labels, "loosen")
                .into_iter()
                .filter(|row| row.get("source_bucket").and_then(Value::as_str) != Some("legacy"))
                .filter(|row| {
                    row_scope_key(row)
                        .map(|label| !covered_scopes.contains(&label))
                        .unwrap_or(false)
                })
                .collect::<Vec<_>>();
        uses_fallback_post_entry = !fallback_post_entry.is_empty();
        selected.extend(fallback_post_entry);
    }
    (
        selected.clone(),
        !selected.is_empty(),
        uses_fallback_post_entry,
    )
}

fn target_field_display_label(target_field: &str) -> String {
    match target_field {
        "max_spread_multiplier" => "max_spread".to_string(),
        "min_edge_multiplier" => "min_edge".to_string(),
        "size_multiplier" => "size".to_string(),
        "hold_edge_multiplier" => "hold_edge".to_string(),
        "capital_efficiency_multiplier" => "capital_efficiency".to_string(),
        "model_reversal_buffer_multiplier" => "model_buffer".to_string(),
        "edge_decay_exit_multiplier" => "edge_decay_exit".to_string(),
        "edge_decay_confirmation_scan_multiplier" => "edge_decay_confirm_scan".to_string(),
        "edge_decay_confirmation_window_multiplier" => "edge_decay_confirm_window".to_string(),
        "edge_decay_cooldown_multiplier" => "edge_decay_cooldown".to_string(),
        "profit_retention_multiplier" => "profit_retention".to_string(),
        "slippage_multiplier" => "slippage".to_string(),
        "size_retention_multiplier" => "size_retention".to_string(),
        "depth_ratio_multiplier" => "depth_ratio".to_string(),
        "probability_calibration" => "probability".to_string(),
        _ => target_field.to_string(),
    }
}

fn target_field_tighten_priority(target_field: &str) -> i64 {
    match target_field {
        "max_spread_multiplier" => 600,
        "size_multiplier" => 500,
        "min_edge_multiplier" => 450,
        "hold_edge_multiplier" => 350,
        "capital_efficiency_multiplier" => 250,
        "model_reversal_buffer_multiplier" => 200,
        "edge_decay_exit_multiplier" => 150,
        "edge_decay_confirmation_scan_multiplier" => 140,
        "edge_decay_confirmation_window_multiplier" => 130,
        "edge_decay_cooldown_multiplier" => 120,
        "profit_retention_multiplier" => 110,
        "slippage_multiplier" => 100,
        "size_retention_multiplier" => 90,
        "depth_ratio_multiplier" => 80,
        "probability_calibration" => 70,
        _ => 10,
    }
}

fn target_field_relax_priority(target_field: &str) -> i64 {
    match target_field {
        "hold_edge_multiplier" => 600,
        "capital_efficiency_multiplier" => 500,
        "model_reversal_buffer_multiplier" => 400,
        "edge_decay_exit_multiplier" => 300,
        "edge_decay_confirmation_scan_multiplier" => 260,
        "edge_decay_confirmation_window_multiplier" => 240,
        "edge_decay_cooldown_multiplier" => 220,
        "size_multiplier" => 140,
        "min_edge_multiplier" => 120,
        "max_spread_multiplier" => 100,
        "profit_retention_multiplier" => 90,
        "slippage_multiplier" => 80,
        "size_retention_multiplier" => 70,
        "depth_ratio_multiplier" => 60,
        "probability_calibration" => 50,
        _ => 10,
    }
}

fn patch_field_priority(field: &Value) -> i64 {
    let Some(target_field) = field.get("target_field").and_then(Value::as_str) else {
        return 0;
    };
    let support_count = field
        .get("support_count")
        .and_then(Value::as_u64)
        .unwrap_or(0) as i64;
    let direction = field
        .get("direction")
        .and_then(Value::as_str)
        .unwrap_or("tighten");
    let base_priority = if direction == "loosen" {
        target_field_relax_priority(target_field)
    } else {
        target_field_tighten_priority(target_field)
    };
    base_priority * 1_000 + support_count
}

fn patch_row_field_priority_score(row: &Value) -> i64 {
    row.get("fields")
        .and_then(Value::as_array)
        .map(|fields| fields.iter().map(patch_field_priority).max().unwrap_or(0))
        .unwrap_or(0)
}

fn collapse_patch_rows_to_primary_field(rows: &[Value]) -> Vec<Value> {
    rows.iter()
        .filter_map(|row| {
            let mut cloned = row.clone();
            let fields = cloned.get_mut("fields").and_then(Value::as_array_mut)?;
            let mut sorted_fields = fields.clone();
            sorted_fields.sort_by(|a, b| {
                patch_field_priority(b)
                    .cmp(&patch_field_priority(a))
                    .then_with(|| {
                        b.get("target_field")
                            .and_then(Value::as_str)
                            .cmp(&a.get("target_field").and_then(Value::as_str))
                    })
            });
            let Some(primary_field) = sorted_fields.into_iter().next() else {
                return None;
            };
            *fields = vec![primary_field];
            Some(cloned)
        })
        .collect()
}

fn filter_patch_rows_by_scope_labels(
    rows: &[Value],
    scope_labels: &std::collections::BTreeSet<String>,
) -> Vec<Value> {
    rows.iter()
        .filter(|row| {
            row_scope_key(row)
                .map(|scope_label| scope_labels.contains(&scope_label))
                .unwrap_or(false)
        })
        .cloned()
        .collect()
}

fn collect_scope_field_targets_by_support(
    rows: &[Value],
    scope_label: &str,
    limit: usize,
) -> Vec<String> {
    let mut scored = rows
        .iter()
        .filter(|row| row_scope_key(row).as_deref() == Some(scope_label))
        .flat_map(|row| {
            row.get("fields")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|field| {
                    Some((
                        field
                            .get("support_count")
                            .and_then(Value::as_u64)
                            .unwrap_or(0),
                        field
                            .get("target_field")
                            .and_then(Value::as_str)?
                            .to_string(),
                    ))
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let mut seen = std::collections::BTreeSet::new();
    scored
        .into_iter()
        .filter_map(|(_, target_field)| {
            if seen.insert(target_field.clone()) {
                Some(target_field_display_label(&target_field))
            } else {
                None
            }
        })
        .take(limit)
        .collect()
}

fn collect_scope_field_targets_in_row_order(
    rows: &[Value],
    scope_label: &str,
    limit: usize,
) -> Vec<String> {
    let mut seen = std::collections::BTreeSet::new();
    let mut ordered = Vec::new();
    for row in rows {
        if row_scope_key(row).as_deref() != Some(scope_label) {
            continue;
        }
        for field in row
            .get("fields")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(target_field) = field.get("target_field").and_then(Value::as_str) else {
                continue;
            };
            if seen.insert(target_field.to_string()) {
                ordered.push(target_field_display_label(target_field));
            }
            if ordered.len() >= limit {
                return ordered;
            }
        }
    }
    ordered
}

fn build_scope_relax_field_targets(
    entry_rows: &[Value],
    post_entry_rows: &[Value],
    scope_label: &str,
    limit: usize,
) -> Vec<String> {
    let relax_scope_labels = [scope_label.to_string()]
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>();
    let filtered_post_entry_rows = post_entry_rows
        .iter()
        .filter(|row| row.get("source_bucket").and_then(Value::as_str) != Some("legacy"))
        .cloned()
        .collect::<Vec<_>>();
    let (post_entry_selected, _, _) =
        build_staged_relax_post_entry_rows(&filtered_post_entry_rows, &relax_scope_labels);
    let mut fields =
        collect_scope_field_targets_in_row_order(&post_entry_selected, scope_label, limit);
    if fields.len() < limit {
        let entry_selected =
            rewrite_patch_rows_direction(entry_rows, &relax_scope_labels, "loosen")
                .into_iter()
                .filter(|row| row.get("source_bucket").and_then(Value::as_str) != Some("legacy"))
                .collect::<Vec<_>>();
        let remaining = limit.saturating_sub(fields.len());
        fields.extend(collect_scope_field_targets_in_row_order(
            &entry_selected,
            scope_label,
            remaining,
        ));
    }
    fields
}

fn compute_relax_tier_for_scope_labels(
    entry_rows: &[Value],
    post_entry_rows: &[Value],
    scope_labels: &[String],
) -> (bool, bool, bool) {
    let relax_scope_labels = scope_labels
        .iter()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    let (post_entry_selected, uses_conservative_post_entry, uses_fallback_post_entry) =
        build_staged_relax_post_entry_rows(post_entry_rows, &relax_scope_labels);
    let post_entry_scope_labels = post_entry_selected
        .iter()
        .filter_map(row_scope_key)
        .collect::<std::collections::BTreeSet<_>>();
    let uses_entry_fallback =
        rewrite_patch_rows_direction(entry_rows, &relax_scope_labels, "loosen")
            .into_iter()
            .filter(|row| {
                row.get("source_bucket").and_then(Value::as_str) != Some("legacy")
                    && row_scope_key(row)
                        .map(|label| !post_entry_scope_labels.contains(&label))
                        .unwrap_or(false)
            })
            .next()
            .is_some();
    (
        uses_conservative_post_entry,
        uses_fallback_post_entry,
        uses_entry_fallback,
    )
}

fn auto_patch_priority_reason_label(
    current_priority_score: i64,
    current_cooldown_severity_score: i64,
    current_window_pressure_score: i64,
    current_long_window_pressure_score: i64,
) -> &'static str {
    if current_priority_score <= 0 {
        "当前压力较低"
    } else {
        let total_window_pressure =
            current_window_pressure_score + current_long_window_pressure_score;
        if current_cooldown_severity_score > 0 && total_window_pressure > 0 {
            let gap = (current_cooldown_severity_score - total_window_pressure).abs();
            if gap <= 2 {
                "冷却坏退出与窗口损失共同主导"
            } else if current_cooldown_severity_score > total_window_pressure {
                "冷却坏退出主导"
            } else if current_long_window_pressure_score > current_window_pressure_score {
                "24h 持续损失主导"
            } else {
                "近窗损失主导"
            }
        } else if current_cooldown_severity_score > 0 {
            "冷却坏退出主导"
        } else if current_long_window_pressure_score > current_window_pressure_score
            && current_long_window_pressure_score > 0
        {
            "24h 持续损失主导"
        } else if total_window_pressure > 0 {
            "近窗损失主导"
        } else {
            "当前压力较低"
        }
    }
}

fn auto_patch_action_label(action: &str) -> &'static str {
    match action {
        "hold" => "停止重复收紧",
        "consider_relax" => "建议小步回退",
        "continue_tighten" => "继续收紧",
        _ => "继续观察",
    }
}

fn build_priority_bucket_summary(
    current_scope_scores: &std::collections::HashMap<String, (i64, i64, i64, i64)>,
    patches: &[Value],
    entry_patch_rows: &[Value],
    post_entry_patch_rows: &[Value],
    cooldown_buckets: &[Value],
) -> Value {
    let scope_action_for = |scope_label: &str| {
        patches
            .iter()
            .find(|patch| {
                patch
                    .get("scope_labels")
                    .and_then(Value::as_array)
                    .map(|labels| {
                        labels
                            .iter()
                            .filter_map(Value::as_str)
                            .any(|label| label == scope_label)
                    })
                    .unwrap_or(false)
            })
            .and_then(|patch| patch.get("recommended_action"))
            .and_then(Value::as_str)
            .unwrap_or("observe")
            .to_string()
    };
    let mut rows = current_scope_scores
        .iter()
        .filter_map(
            |(
                scope_label,
                (
                    priority_score,
                    cooldown_severity_score,
                    window_pressure_score,
                    long_window_pressure_score,
                ),
            )| {
                if *priority_score <= 0 {
                    return None;
                }
                let (resolution_bucket, asset_class, event_subtype, shape) =
                    parse_bucketed_shaped_scope_label(scope_label)?;
                Some(json!({
                    "scope_label": scope_label,
                    "resolution_bucket": resolution_bucket,
                    "asset_class": asset_class,
                    "event_subtype": event_subtype,
                    "shape": shape,
                    "priority_score": priority_score,
                    "cooldown_severity_score": cooldown_severity_score,
                    "window_pressure_score": window_pressure_score,
                    "long_window_pressure_score": long_window_pressure_score,
                    "priority_reason_label": auto_patch_priority_reason_label(
                        *priority_score,
                        *cooldown_severity_score,
                        *window_pressure_score,
                        *long_window_pressure_score,
                    ),
                }))
            },
        )
        .collect::<Vec<_>>();
    rows.sort_by(|a, b| {
        let a_priority = a.get("priority_score").and_then(Value::as_i64).unwrap_or(0);
        let b_priority = b.get("priority_score").and_then(Value::as_i64).unwrap_or(0);
        b_priority.cmp(&a_priority).then_with(|| {
            b.get("scope_label")
                .and_then(Value::as_str)
                .cmp(&a.get("scope_label").and_then(Value::as_str))
        })
    });
    if rows.len() > 5 {
        rows.truncate(5);
    }
    let leader_scope_label = rows
        .first()
        .and_then(|row| row.get("scope_label"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let leader_recommended_action = scope_action_for(&leader_scope_label);
    let leader_action_label = auto_patch_action_label(&leader_recommended_action).to_string();
    let leader_target_fields = if leader_scope_label.is_empty() {
        Vec::new()
    } else if leader_recommended_action == "consider_relax" {
        build_scope_relax_field_targets(
            entry_patch_rows,
            post_entry_patch_rows,
            &leader_scope_label,
            4,
        )
    } else {
        let mut fields =
            collect_scope_field_targets_by_support(entry_patch_rows, &leader_scope_label, 4);
        if fields.len() < 4 {
            let remaining = 4usize.saturating_sub(fields.len());
            let mut post_entry_fields = collect_scope_field_targets_by_support(
                post_entry_patch_rows,
                &leader_scope_label,
                remaining,
            );
            fields.append(&mut post_entry_fields);
            fields.dedup();
            fields.truncate(4);
        }
        fields
    };
    let leader_field_action_label = if leader_target_fields.is_empty() {
        match leader_recommended_action.as_str() {
            "hold" => "维持当前收紧，无新增字段动作".to_string(),
            "continue_tighten" => "继续收紧，但当前没有可导出的字段候选".to_string(),
            "consider_relax" => "建议小步回退，但当前没有可导出的字段候选".to_string(),
            _ => "继续观察，无新增字段动作".to_string(),
        }
    } else {
        match leader_recommended_action.as_str() {
            "hold" => format!(
                "维持当前收紧，优先保持：{}",
                leader_target_fields.join(" / ")
            ),
            "continue_tighten" => {
                format!("建议优先收紧：{}", leader_target_fields.join(" / "))
            }
            "consider_relax" => {
                format!("建议优先小步回退：{}", leader_target_fields.join(" / "))
            }
            _ => format!("继续观察：{}", leader_target_fields.join(" / ")),
        }
    };
    let mut subtype_scores = std::collections::BTreeMap::<String, i64>::new();
    let mut subtype_scope_leaders = std::collections::BTreeMap::<String, (String, i64)>::new();
    for (scope_label, (priority_score, _, _, _)) in current_scope_scores {
        if *priority_score <= 0 {
            continue;
        }
        let Some((_, _, event_subtype, _)) = parse_bucketed_shaped_scope_label(scope_label) else {
            continue;
        };
        *subtype_scores.entry(event_subtype.to_string()).or_insert(0) += *priority_score;
        subtype_scope_leaders
            .entry(event_subtype.to_string())
            .and_modify(|(leader_scope, leader_score)| {
                if *priority_score > *leader_score {
                    *leader_scope = scope_label.clone();
                    *leader_score = *priority_score;
                }
            })
            .or_insert_with(|| (scope_label.clone(), *priority_score));
    }
    let total_subtype_score: i64 = subtype_scores.values().sum();
    let subtype_focus_event_subtype = subtype_scores
        .iter()
        .max_by_key(|(_, score)| **score)
        .map(|(subtype, _)| subtype.clone())
        .unwrap_or_default();
    let subtype_focus_scope_labels = current_scope_scores
        .iter()
        .filter_map(|(scope_label, (priority_score, _, _, _))| {
            if *priority_score <= 0 {
                return None;
            }
            let (_, _, event_subtype, _) = parse_bucketed_shaped_scope_label(scope_label)?;
            if event_subtype == subtype_focus_event_subtype {
                Some(scope_label.clone())
            } else {
                None
            }
        })
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let subtype_focus_leader_scope = subtype_scope_leaders
        .get(&subtype_focus_event_subtype)
        .map(|(scope_label, _)| scope_label.clone())
        .unwrap_or_default();
    let subtype_focus_recommended_action = if subtype_focus_leader_scope.is_empty() {
        "observe".to_string()
    } else {
        scope_action_for(&subtype_focus_leader_scope)
    };
    let subtype_focus_target_fields = if subtype_focus_leader_scope.is_empty() {
        Vec::new()
    } else if subtype_focus_recommended_action == "consider_relax" {
        build_scope_relax_field_targets(
            entry_patch_rows,
            post_entry_patch_rows,
            &subtype_focus_leader_scope,
            3,
        )
    } else {
        collect_scope_field_targets_by_support(entry_patch_rows, &subtype_focus_leader_scope, 3)
    };
    let subtype_focus_label = if let Some(score) = subtype_scores.get(&subtype_focus_event_subtype)
    {
        let subtype_label =
            if matches!(subtype_focus_event_subtype.as_str(), "" | "generic" | "any") {
                "generic".to_string()
            } else {
                subtype_focus_event_subtype.clone()
            };
        let concentration = if total_subtype_score > 0 && *score * 10 >= total_subtype_score * 7 {
            "风险较集中"
        } else {
            "风险较分散"
        };
        format!("当前主导恶化 subtype 为 {subtype_label}，{concentration}")
    } else {
        "暂无明显主导 subtype".to_string()
    };
    let subtype_focus_action_label = if subtype_focus_leader_scope.is_empty() {
        "subtype 建议：继续观察".to_string()
    } else {
        let subtype_label =
            if matches!(subtype_focus_event_subtype.as_str(), "" | "generic" | "any") {
                "generic".to_string()
            } else {
                subtype_focus_event_subtype.clone()
            };
        if subtype_focus_target_fields.is_empty() {
            format!(
                "subtype 建议：{}（{}）",
                auto_patch_action_label(&subtype_focus_recommended_action),
                subtype_label
            )
        } else {
            format!(
                "subtype 建议：{}（{}：{}）",
                auto_patch_action_label(&subtype_focus_recommended_action),
                subtype_label,
                subtype_focus_target_fields.join(" / ")
            )
        }
    };
    let subtype_focus_summary_label = if subtype_focus_leader_scope.is_empty() {
        "当前暂无明确主导 subtype 动作".to_string()
    } else {
        format!("{subtype_focus_label}；{subtype_focus_action_label}")
    };
    let subtype_focus_field_summary_label = if subtype_focus_target_fields.is_empty() {
        "subtype 字段建议：继续观察".to_string()
    } else {
        format!(
            "subtype 字段建议：{} 先动 {}",
            if subtype_focus_recommended_action == "consider_relax" {
                "建议回退"
            } else if subtype_focus_recommended_action == "continue_tighten" {
                "建议收紧"
            } else {
                "建议观察"
            },
            subtype_focus_target_fields[0]
        )
    };
    let bucket_scope_lookup = cooldown_buckets
        .iter()
        .filter_map(|bucket| {
            let kind = bucket
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("same_day_range");
            let resolution_bucket = cooldown_kind_resolution_bucket(kind);
            let asset = bucket
                .get("asset")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let event_subtype = bucket
                .get("event_subtype")
                .and_then(Value::as_str)
                .unwrap_or("generic");
            let shape = bucket
                .get("shape")
                .and_then(Value::as_str)
                .unwrap_or("directional");
            let scope_label = bucketed_scope_label(
                resolution_bucket,
                asset_class_for_asset_label(asset),
                event_subtype,
                shape,
            );
            Some((scope_label, asset.to_string()))
        })
        .collect::<std::collections::HashMap<_, _>>();
    let mut asset_scope_leaders = std::collections::BTreeMap::<String, (String, i64)>::new();
    let mut asset_scores = std::collections::BTreeMap::<String, i64>::new();
    for (scope_label, (priority_score, _, _, _)) in current_scope_scores {
        if *priority_score <= 0 {
            continue;
        }
        let Some(asset) = bucket_scope_lookup.get(scope_label) else {
            continue;
        };
        *asset_scores.entry(asset.clone()).or_insert(0) += *priority_score;
        asset_scope_leaders
            .entry(asset.clone())
            .and_modify(|(leader_scope, leader_score)| {
                if *priority_score > *leader_score {
                    *leader_scope = scope_label.clone();
                    *leader_score = *priority_score;
                }
            })
            .or_insert_with(|| (scope_label.clone(), *priority_score));
    }
    let total_asset_score: i64 = asset_scores.values().sum();
    let asset_focus_asset = asset_scores
        .iter()
        .max_by_key(|(_, score)| **score)
        .map(|(asset, _)| asset.clone())
        .unwrap_or_default();
    let asset_focus_scope_labels = current_scope_scores
        .iter()
        .filter_map(|(scope_label, (priority_score, _, _, _))| {
            if *priority_score <= 0 {
                return None;
            }
            if bucket_scope_lookup.get(scope_label) == Some(&asset_focus_asset) {
                Some(scope_label.clone())
            } else {
                None
            }
        })
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let asset_focus_leader_scope = asset_scope_leaders
        .get(&asset_focus_asset)
        .map(|(scope_label, _)| scope_label.clone())
        .unwrap_or_default();
    let asset_focus_recommended_action = if asset_focus_leader_scope.is_empty() {
        "observe".to_string()
    } else {
        scope_action_for(&asset_focus_leader_scope)
    };
    let asset_focus_target_fields = if asset_focus_leader_scope.is_empty() {
        Vec::new()
    } else if asset_focus_recommended_action == "consider_relax" {
        build_scope_relax_field_targets(
            entry_patch_rows,
            post_entry_patch_rows,
            &asset_focus_leader_scope,
            3,
        )
    } else {
        collect_scope_field_targets_by_support(entry_patch_rows, &asset_focus_leader_scope, 3)
    };
    let asset_focus_label = if let Some(score) = asset_scores.get(&asset_focus_asset) {
        let concentration = if total_asset_score > 0 && *score * 10 >= total_asset_score * 7 {
            "可优先考虑资产级微调"
        } else {
            "暂不建议资产级单点微调"
        };
        format!(
            "当前冷却压力主要集中在 {}，{}",
            asset_focus_asset, concentration
        )
    } else {
        "暂无明显主导资产".to_string()
    };
    let asset_focus_action_label = if asset_focus_leader_scope.is_empty() {
        "资产建议：继续观察".to_string()
    } else if asset_focus_target_fields.is_empty() {
        format!(
            "资产建议：{}（{}）",
            auto_patch_action_label(&asset_focus_recommended_action),
            asset_focus_asset
        )
    } else {
        format!(
            "资产建议：{}（{}：{}）",
            auto_patch_action_label(&asset_focus_recommended_action),
            asset_focus_asset,
            asset_focus_target_fields.join(" / ")
        )
    };
    let asset_focus_summary_label = if asset_focus_leader_scope.is_empty() {
        "当前暂无明确主导资产动作".to_string()
    } else {
        format!("{asset_focus_label}；{asset_focus_action_label}")
    };
    let asset_focus_field_summary_label = if asset_focus_target_fields.is_empty() {
        "资产字段建议：继续观察".to_string()
    } else {
        format!(
            "资产字段建议：{} 先动 {}",
            if asset_focus_recommended_action == "consider_relax" {
                "建议回退"
            } else if asset_focus_recommended_action == "continue_tighten" {
                "建议收紧"
            } else {
                "建议观察"
            },
            asset_focus_target_fields[0]
        )
    };
    let leader_label = rows
        .first()
        .and_then(|row| {
            let event_subtype = row.get("event_subtype")?.as_str()?;
            let subtype_label = if matches!(event_subtype, "any" | "generic" | "") {
                String::new()
            } else {
                format!(" / {}", event_subtype)
            };
            Some(format!(
                "当前最差冷却 bucket 主导在 {} {}{} {}，且 {}",
                row.get("resolution_bucket")?.as_str()?,
                row.get("asset_class")?.as_str()?,
                subtype_label,
                row.get("shape")?.as_str()?,
                row.get("priority_reason_label")?.as_str()?,
            ))
        })
        .unwrap_or_else(|| "当前没有明显恶化的冷却 bucket".to_string());
    json!({
        "row_count": rows.len(),
        "leader_scope_label": leader_scope_label,
        "leader_label": leader_label,
        "leader_recommended_action": leader_recommended_action,
        "leader_action_label": leader_action_label,
        "leader_field_action_label": leader_field_action_label,
        "leader_target_fields": leader_target_fields,
        "subtype_focus_label": subtype_focus_label,
        "subtype_focus_action_label": subtype_focus_action_label,
        "subtype_focus_summary_label": subtype_focus_summary_label,
        "subtype_focus_field_summary_label": subtype_focus_field_summary_label,
        "subtype_focus_event_subtype": subtype_focus_event_subtype,
        "subtype_focus_scope_labels": subtype_focus_scope_labels,
        "subtype_focus_recommended_action": subtype_focus_recommended_action,
        "subtype_focus_target_fields": subtype_focus_target_fields,
        "asset_focus_label": asset_focus_label,
        "asset_focus_action_label": asset_focus_action_label,
        "asset_focus_summary_label": asset_focus_summary_label,
        "asset_focus_field_summary_label": asset_focus_field_summary_label,
        "asset_focus_asset": asset_focus_asset,
        "asset_focus_scope_labels": asset_focus_scope_labels,
        "asset_focus_recommended_action": asset_focus_recommended_action,
        "asset_focus_target_fields": asset_focus_target_fields,
        "rows": rows,
    })
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

fn finalize_generated_patch_export(
    mode: &str,
    filename: String,
    scope_label: Option<String>,
    scope_labels: Vec<String>,
    entry_rows: Vec<Value>,
    post_entry_rows: Vec<Value>,
    uses_conservative_post_entry: bool,
    uses_fallback_post_entry: bool,
    uses_entry_fallback: bool,
    focus_label: Option<String>,
    recommended_action: Option<String>,
    action_label: Option<String>,
    note: Option<String>,
) -> GeneratedCryptoOverridePatch {
    let entry_rows = collapse_patch_rows_to_primary_field(&entry_rows);
    let post_entry_rows = collapse_patch_rows_to_primary_field(&post_entry_rows);
    let selected_target_fields = entry_rows
        .iter()
        .chain(post_entry_rows.iter())
        .filter_map(|row| {
            row.get("fields")
                .and_then(Value::as_array)
                .and_then(|fields| fields.first())
                .and_then(|field| field.get("target_field"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let selected_field_count = entry_rows.len() + post_entry_rows.len();
    let toml = [
        render_patch_rows_to_toml(&entry_rows),
        render_patch_rows_to_toml(&post_entry_rows),
    ]
    .into_iter()
    .filter(|value| !value.trim().is_empty())
    .collect::<Vec<_>>()
    .join("\n\n");
    let export_sha = patch_export_digest(&toml);
    let generated_at = Utc::now().to_rfc3339();
    GeneratedCryptoOverridePatch {
        mode: mode.to_string(),
        filename,
        export_sha,
        generated_at,
        scope_label,
        scope_labels,
        toml,
        selected_bucket_count: 0,
        entry_row_count: entry_rows.len(),
        post_entry_row_count: post_entry_rows.len(),
        selected_field_count,
        field_level: true,
        selected_target_fields,
        uses_conservative_post_entry,
        uses_fallback_post_entry,
        uses_entry_fallback,
        focus_label,
        recommended_action,
        action_label,
        note,
    }
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

fn compute_scope_window_pressure_score(
    trade_rows: &[TradeHistoryRow],
    positions: &[PositionApiEntry],
    recent_exits: &[crate::diagnostics::CryptoExitDecision],
    resolution_bucket: &str,
    shape: &str,
    asset_class: &str,
    event_subtype: &str,
    windows: &[(chrono::Duration, i64)],
) -> i64 {
    let now = Utc::now();
    let mut score = 0_i64;

    for (window_duration, window_weight) in windows {
        let window_start = now - *window_duration;
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
                    && asset_class_for_asset_label(infer_trade_asset(trade.question.as_deref()))
                        == asset_class
                    && (event_subtype == "any"
                        || infer_question_event_subtype(trade.question.as_deref()) == event_subtype)
            })
            .map(|trade| trade.actual_profit.unwrap_or(Decimal::ZERO))
            .sum();
        let open_pnl_bid: Decimal = positions
            .iter()
            .filter(|position| {
                position.resolution_bucket.as_deref() == Some(resolution_bucket)
                    && infer_position_shape(position) == shape
                    && asset_class_for_asset_label(position.asset.as_deref().unwrap_or_default())
                        == asset_class
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
        let bad_exit_count = recent_exits
            .iter()
            .filter(|decision| {
                decision.recorded_at >= window_start
                    && normalized_resolution_bucket_label(decision.days_to_resolution)
                        == resolution_bucket
                    && infer_exit_shape_label(
                        decision.market_type.as_deref(),
                        Some(decision.question.as_str()),
                    ) == shape
                    && asset_class_for_asset_label(decision.asset.as_deref().unwrap_or_default())
                        == asset_class
                    && (event_subtype == "any"
                        || infer_question_event_subtype(Some(decision.question.as_str()))
                            == event_subtype)
                    && matches!(
                        decision.reason.as_str(),
                        "model_reversal" | "relative_stop_loss"
                    )
            })
            .count();
        score += (bad_exit_count as i64) * 4_000 * *window_weight
            + decimal_loss_score(realized_pnl) * 5 * *window_weight
            + decimal_loss_score(open_pnl_bid) * 2 * *window_weight;
    }

    score
}

fn build_current_cooldown_scope_scores(
    cooldown_buckets: &[Value],
    trade_rows: &[TradeHistoryRow],
    positions: &[PositionApiEntry],
) -> std::collections::HashMap<String, (i64, i64, i64, i64)> {
    let recent_exits = crate::diagnostics::recent_crypto_exit_decisions()
        .into_iter()
        .take(24)
        .collect::<Vec<_>>();
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
        let window_pressure_score = compute_scope_window_pressure_score(
            trade_rows,
            positions,
            &recent_exits,
            resolution_bucket,
            shape,
            asset_class_for_asset_label(asset),
            event_subtype,
            &[
                (chrono::Duration::hours(1), 10_i64),
                (chrono::Duration::hours(6), 4_i64),
            ],
        );
        let long_window_pressure_score = compute_scope_window_pressure_score(
            trade_rows,
            positions,
            &recent_exits,
            resolution_bucket,
            shape,
            asset_class_for_asset_label(asset),
            event_subtype,
            &[(chrono::Duration::hours(24), 1_i64)],
        );
        let scope_label = bucketed_scope_label(
            resolution_bucket,
            asset_class_for_asset_label(asset),
            event_subtype,
            shape,
        );
        scores.insert(
            scope_label,
            (
                cooldown_severity_score + window_pressure_score + long_window_pressure_score,
                cooldown_severity_score,
                window_pressure_score,
                long_window_pressure_score,
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
            .then_with(|| patch_row_field_priority_score(b).cmp(&patch_row_field_priority_score(a)))
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
    let current_scope_scores =
        build_current_cooldown_scope_scores(&cooldown_buckets, &trade_rows, &positions);

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
                    && (event_subtype == "any"
                        || infer_question_event_subtype(trade.question.as_deref()) == event_subtype)
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
        let should_keep_tight = post_trigger_bad_exit_count > 0
            || post_trigger_realized < Decimal::ZERO
            || open_pnl_bid < Decimal::ZERO;
        if should_keep_tight {
            let scope_label = bucketed_scope_label(
                resolution_bucket,
                asset_class_for_asset_label(asset),
                event_subtype,
                shape,
            );
            let severity_score = current_scope_scores
                .get(&scope_label)
                .map(|(priority_score, _, _, _)| *priority_score)
                .unwrap_or_else(|| {
                    (post_trigger_bad_exit_count as i64) * 10_000
                        + decimal_loss_score(post_trigger_realized) * 10
                        + decimal_loss_score(open_pnl_bid)
                });
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
                .then_with(|| {
                    patch_row_field_priority_score(&b.1).cmp(&patch_row_field_priority_score(&a.1))
                })
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
        let mut combined_ranked = entry_selected
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
        combined_ranked.sort_by(|a, b| {
            patch_row_field_priority_score(&b.1)
                .cmp(&patch_row_field_priority_score(&a.1))
                .then_with(|| patch_row_support_score(&b.1).cmp(&patch_row_support_score(&a.1)))
                .then_with(|| {
                    b.1.get("scope_label")
                        .and_then(Value::as_str)
                        .cmp(&a.1.get("scope_label").and_then(Value::as_str))
                })
        });
        let mut selected_scope_labels = std::collections::HashSet::new();
        let mut selected_row_keys = std::collections::HashSet::new();
        for (kind, row) in combined_ranked {
            let Some(scope_label) = row_scope_key(&row) else {
                continue;
            };
            if !selected_scope_labels.insert(scope_label.clone()) {
                continue;
            }
            let row_key = format!("{kind}::{scope_label}");
            selected_row_keys.insert(row_key);
            if max_rows > 0 && selected_scope_labels.len() >= max_rows {
                break;
            }
        }
        entry_selected.retain(|row| {
            row_scope_key(row)
                .map(|scope_label| selected_row_keys.contains(&format!("entry::{scope_label}")))
                .unwrap_or(false)
        });
        post_entry_selected.retain(|row| {
            row_scope_key(row)
                .map(|scope_label| selected_row_keys.contains(&format!("post::{scope_label}")))
                .unwrap_or(false)
        });
    }
    let filename = "crypto_cooldown_priority_override_patch.toml".to_string();
    let mut generated = finalize_generated_patch_export(
        "cooldown_priority",
        filename.clone(),
        None,
        selected_scopes
            .iter()
            .map(|(asset_class, event_subtype, shape, source_bucket, _)| {
                bucketed_scope_label(source_bucket, asset_class, event_subtype, shape)
            })
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect(),
        entry_selected,
        post_entry_selected,
        false,
        false,
        false,
        None,
        Some("continue_tighten".to_string()),
        Some("继续收紧".to_string()),
        Some("字段级 cooldown_priority patch，优先仅导出当前 bucket 最该收紧的字段".to_string()),
    );
    generated.selected_bucket_count = selected_scopes.len();
    if record_export {
        crate::diagnostics::record_crypto_override_patch_export(
            crate::diagnostics::CryptoOverridePatchExportDecision {
                recorded_at: Utc::now(),
                mode: "cooldown_priority".into(),
                format: if wants_toml { "toml" } else { "json" }.into(),
                filename: filename.clone(),
                export_sha: generated.export_sha.clone(),
                scope_label: None,
            },
        );
    }
    Ok(generated)
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

    let filtered_post_entry_rows = post_entry_rows
        .iter()
        .filter(|row| row.get("source_bucket").and_then(Value::as_str) != Some("legacy"))
        .cloned()
        .collect::<Vec<_>>();
    let (post_entry_selected, uses_conservative_post_entry, uses_fallback_post_entry) =
        build_staged_relax_post_entry_rows(&filtered_post_entry_rows, &relax_scope_labels);
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
    let uses_entry_fallback = !entry_selected.is_empty();

    let filename = "crypto_relax_candidate_override_patch.toml".to_string();
    let mut generated = finalize_generated_patch_export(
        "relax_candidate",
        filename.clone(),
        None,
        relax_scope_labels.iter().cloned().collect(),
        entry_selected,
        post_entry_selected,
        uses_conservative_post_entry,
        uses_fallback_post_entry,
        uses_entry_fallback,
        None,
        Some("consider_relax".to_string()),
        Some("建议小步回退".to_string()),
        Some("字段级 relax patch，优先仅导出当前 scope 最保守的回退字段".to_string()),
    );
    generated.selected_bucket_count = generated.scope_labels.len();
    if record_export {
        crate::diagnostics::record_crypto_override_patch_export(
            crate::diagnostics::CryptoOverridePatchExportDecision {
                recorded_at: Utc::now(),
                mode: "relax_candidate".into(),
                format: if wants_toml { "toml" } else { "json" }.into(),
                filename: filename.clone(),
                export_sha: generated.export_sha.clone(),
                scope_label: None,
            },
        );
    }
    Ok(generated)
}

fn build_focus_patch_export_from_status(
    status_payload: &Value,
    mode: &str,
) -> Result<GeneratedCryptoOverridePatch, String> {
    let priority_summary = status_payload
        .get("crypto_auto_patch_effectiveness_summary")
        .and_then(|value| value.get("priority_bucket_summary"))
        .ok_or_else(|| "Missing crypto priority bucket summary".to_string())?;
    let (focus_label, scope_labels, recommended_action, action_label, note, filename) = match mode {
        "subtype_focus" => (
            priority_summary
                .get("subtype_focus_label")
                .and_then(Value::as_str)
                .unwrap_or("暂无明显主导 subtype")
                .to_string(),
            priority_summary
                .get("subtype_focus_scope_labels")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<std::collections::BTreeSet<_>>(),
            priority_summary
                .get("subtype_focus_recommended_action")
                .and_then(Value::as_str)
                .unwrap_or("observe")
                .to_string(),
            priority_summary
                .get("subtype_focus_action_label")
                .and_then(Value::as_str)
                .unwrap_or("subtype 建议：继续观察")
                .to_string(),
            "字段级 subtype patch，优先仅导出当前主导 subtype 最该动作的字段".to_string(),
            "crypto_subtype_focus_override_patch.toml".to_string(),
        ),
        "asset_focus" => (
            priority_summary
                .get("asset_focus_label")
                .and_then(Value::as_str)
                .unwrap_or("暂无明显主导资产")
                .to_string(),
            priority_summary
                .get("asset_focus_scope_labels")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<std::collections::BTreeSet<_>>(),
            priority_summary
                .get("asset_focus_recommended_action")
                .and_then(Value::as_str)
                .unwrap_or("observe")
                .to_string(),
            priority_summary
                .get("asset_focus_action_label")
                .and_then(Value::as_str)
                .unwrap_or("资产建议：继续观察")
                .to_string(),
            "资产级 patch 仍使用现有 selector 维度，仅按当前主导资产关联的 scope 生成只读字段级候选".to_string(),
            "crypto_asset_focus_override_patch.toml".to_string(),
        ),
        "asset_long_window_focus" => {
            let asset_summary = status_payload
                .get("crypto_asset_long_window_summary")
                .ok_or_else(|| "Missing crypto asset long-window summary".to_string())?;
            let leader_asset = asset_summary
                .get("leader_asset")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let asset_focus_asset = priority_summary
                .get("asset_focus_asset")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let scope_labels = if !leader_asset.is_empty() && leader_asset == asset_focus_asset {
                priority_summary
                    .get("asset_focus_scope_labels")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect::<std::collections::BTreeSet<_>>()
            } else {
                std::collections::BTreeSet::new()
            };
            (
                asset_summary
                    .get("leader_label")
                    .and_then(Value::as_str)
                    .unwrap_or("近 24h 暂无明显主导资产")
                    .to_string(),
                scope_labels,
                priority_summary
                    .get("asset_focus_recommended_action")
                    .and_then(Value::as_str)
                    .unwrap_or("observe")
                    .to_string(),
                asset_summary
                    .get("leader_action_label")
                    .and_then(Value::as_str)
                    .unwrap_or("资产建议：继续观察")
                    .to_string(),
                "资产级长期样本 patch 候选仅在 24h 主导资产同时也是当前冷却焦点资产时生成，避免把长窗噪音直接映射成运行态 patch".to_string(),
                "crypto_asset_long_window_focus_override_patch.toml".to_string(),
            )
        }
        _ => return Err("unsupported focus patch mode".to_string()),
    };

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

    let (
        entry_selected,
        post_entry_selected,
        uses_conservative_post_entry,
        uses_fallback_post_entry,
        uses_entry_fallback,
    ) = if recommended_action == "consider_relax" {
        let filtered_post_entry_rows = post_entry_rows
            .iter()
            .filter(|row| row.get("source_bucket").and_then(Value::as_str) != Some("legacy"))
            .cloned()
            .collect::<Vec<_>>();
        let (post_entry_selected, uses_conservative_post_entry, uses_fallback_post_entry) =
            build_staged_relax_post_entry_rows(&filtered_post_entry_rows, &scope_labels);
        let post_entry_scope_labels = post_entry_selected
            .iter()
            .filter_map(row_scope_key)
            .collect::<std::collections::BTreeSet<_>>();
        let entry_selected = rewrite_patch_rows_direction(&entry_rows, &scope_labels, "loosen")
            .into_iter()
            .filter(|row| {
                row.get("source_bucket").and_then(Value::as_str) != Some("legacy")
                    && row_scope_key(row)
                        .map(|label| !post_entry_scope_labels.contains(&label))
                        .unwrap_or(false)
            })
            .collect::<Vec<_>>();
        let uses_entry_fallback = !entry_selected.is_empty();
        (
            entry_selected,
            post_entry_selected,
            uses_conservative_post_entry,
            uses_fallback_post_entry,
            uses_entry_fallback,
        )
    } else {
        (
            filter_patch_rows_by_scope_labels(&entry_rows, &scope_labels),
            filter_patch_rows_by_scope_labels(&post_entry_rows, &scope_labels),
            false,
            false,
            false,
        )
    };

    let mut generated = finalize_generated_patch_export(
        mode,
        filename.clone(),
        None,
        scope_labels.into_iter().collect(),
        entry_selected,
        post_entry_selected,
        uses_conservative_post_entry,
        uses_fallback_post_entry,
        uses_entry_fallback,
        Some(focus_label),
        Some(recommended_action),
        Some(action_label),
        Some(note),
    );
    generated.selected_bucket_count = generated.scope_labels.len();
    Ok(generated)
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

    if mode == "full" {
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
        let generated = finalize_generated_patch_export(
            "full",
            "crypto_runtime_override_patch.toml".to_string(),
            None,
            Vec::new(),
            entry_rows,
            post_entry_rows,
            false,
            false,
            false,
            None,
            None,
            None,
            Some("字段级 full patch，优先只导出每行当前最核心的字段".to_string()),
        );
        crate::diagnostics::record_crypto_override_patch_export(
            crate::diagnostics::CryptoOverridePatchExportDecision {
                recorded_at: Utc::now(),
                mode: "full".into(),
                format: if wants_toml { "toml" } else { "json" }.into(),
                filename: generated.filename.clone(),
                export_sha: generated.export_sha.clone(),
                scope_label: None,
            },
        );
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
                "mode": "full",
                "filename": generated.filename,
                "export_sha": generated.export_sha,
                "generated_at": generated.generated_at,
                "entry_row_count": generated.entry_row_count,
                "post_entry_row_count": generated.post_entry_row_count,
                "selected_field_count": generated.selected_field_count,
                "field_level": generated.field_level,
                "selected_target_fields": generated.selected_target_fields,
                "note": generated.note,
                "toml": generated.toml,
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
        let generated = finalize_generated_patch_export(
            "selected",
            format!("crypto_{}_{}_override_patch.toml", bucket, shape),
            Some(format!("{bucket} / {shape}")),
            Vec::new(),
            entry_selected,
            post_entry_selected,
            false,
            false,
            false,
            Some(format!("{bucket} / {shape}")),
            None,
            None,
            Some("字段级 selected patch，仅导出所选 bucket + shape 当前最核心的字段".to_string()),
        );
        crate::diagnostics::record_crypto_override_patch_export(
            crate::diagnostics::CryptoOverridePatchExportDecision {
                recorded_at: Utc::now(),
                mode: "selected".into(),
                format: if wants_toml { "toml" } else { "json" }.into(),
                filename: generated.filename.clone(),
                export_sha: generated.export_sha.clone(),
                scope_label: Some(format!("{bucket} / {shape}")),
            },
        );
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
                "mode": "selected",
                "scope_label": format!("{bucket} / {shape}"),
                "filename": generated.filename,
                "export_sha": generated.export_sha,
                "generated_at": generated.generated_at,
                "bucket": bucket,
                "shape": shape,
                "entry_row_count": generated.entry_row_count,
                "post_entry_row_count": generated.post_entry_row_count,
                "selected_field_count": generated.selected_field_count,
                "field_level": generated.field_level,
                "selected_target_fields": generated.selected_target_fields,
                "note": generated.note,
                "toml": generated.toml,
            })),
        )
            .into_response();
    }

    if mode == "subtype_focus" || mode == "asset_focus" || mode == "asset_long_window_focus" {
        let generated = match build_focus_patch_export_from_status(&status_payload, mode) {
            Ok(generated) => generated,
            Err(error) => {
                return (
                    axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": error})),
                )
                    .into_response();
            }
        };
        crate::diagnostics::record_crypto_override_patch_export(
            crate::diagnostics::CryptoOverridePatchExportDecision {
                recorded_at: Utc::now(),
                mode: mode.into(),
                format: if wants_toml { "toml" } else { "json" }.into(),
                filename: generated.filename.clone(),
                export_sha: generated.export_sha.clone(),
                scope_label: generated.focus_label.clone(),
            },
        );
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
                "mode": generated.mode,
                "filename": generated.filename,
                "export_sha": generated.export_sha,
                "generated_at": generated.generated_at,
                "scope_label": generated.scope_label,
                "focus_label": generated.focus_label,
                "recommended_action": generated.recommended_action,
                "action_label": generated.action_label,
                "entry_row_count": generated.entry_row_count,
                "post_entry_row_count": generated.post_entry_row_count,
                "selected_field_count": generated.selected_field_count,
                "field_level": generated.field_level,
                "selected_target_fields": generated.selected_target_fields,
                "uses_conservative_post_entry": generated.uses_conservative_post_entry,
                "uses_fallback_post_entry": generated.uses_fallback_post_entry,
                "uses_entry_fallback": generated.uses_entry_fallback,
                "note": generated.note,
                "toml": generated.toml,
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
                "selected_field_count": generated.selected_field_count,
                "field_level": generated.field_level,
                "selected_target_fields": generated.selected_target_fields,
                "uses_conservative_post_entry": generated.uses_conservative_post_entry,
                "uses_fallback_post_entry": generated.uses_fallback_post_entry,
                "uses_entry_fallback": generated.uses_entry_fallback,
                "recommended_action": generated.recommended_action,
                "action_label": generated.action_label,
                "note": generated.note,
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
            "selected_field_count": generated.selected_field_count,
            "field_level": generated.field_level,
            "selected_target_fields": generated.selected_target_fields,
            "uses_conservative_post_entry": generated.uses_conservative_post_entry,
            "uses_fallback_post_entry": generated.uses_fallback_post_entry,
            "uses_entry_fallback": generated.uses_entry_fallback,
            "recommended_action": generated.recommended_action,
            "action_label": generated.action_label,
            "note": generated.note,
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
        "uses_conservative_post_entry": body.uses_conservative_post_entry.unwrap_or(false),
        "uses_fallback_post_entry": body.uses_fallback_post_entry.unwrap_or(false),
        "uses_entry_fallback": body.uses_entry_fallback.unwrap_or(false),
        "selected_target_fields": body.selected_target_fields.clone().unwrap_or_default(),
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
                    uses_conservative_post_entry: row
                        .data
                        .get("uses_conservative_post_entry")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    uses_fallback_post_entry: row
                        .data
                        .get("uses_fallback_post_entry")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    uses_entry_fallback: row
                        .data
                        .get("uses_entry_fallback")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
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
                        &rows,
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
                uses_conservative_post_entry: Some(generated.uses_conservative_post_entry),
                uses_fallback_post_entry: Some(generated.uses_fallback_post_entry),
                uses_entry_fallback: Some(generated.uses_entry_fallback),
                selected_target_fields: Some(generated.selected_target_fields),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_infer_exit_shape_prefers_range_question_text() {
        assert_eq!(
            infer_exit_shape_label(
                Some("binary"),
                Some("Will the price of Ethereum be between $2,000 and $2,100 on March 28?")
            ),
            "range"
        );
        assert_eq!(
            infer_exit_shape_label(
                Some("binary"),
                Some("Will Ethereum reach $2,200 on March 28?")
            ),
            "directional"
        );
    }
}
