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
    pub asset: Option<String>,
    pub direction: Option<String>,
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
    /// True once startup has completed enough for the bot to be considered ready.
    pub startup_ready: Arc<AtomicBool>,
}

/// Build the full Axum router with health, metrics, config API, and SPA fallback.
pub fn build_router(state: Arc<ApiState>) -> Router {
    let api_routes = Router::new()
        .route("/api/config", get(get_all_config))
        .route("/api/config/meta/{section}", get(get_section_meta))
        .route("/api/config/{section}", get(get_section))
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
            "scope_label": bucket_scope_label(selector_asset_class, selector_event_subtype),
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
    let mut crypto_entry_tuning_hints = Vec::new();
    let mut crypto_override_suggestions = Vec::new();
    let mut crypto_post_entry_tuning_hints = Vec::new();
    let mut crypto_post_entry_override_suggestions = Vec::new();

    let mut bucket_reject_reason_counts: std::collections::HashMap<
        (String, String),
        std::collections::HashMap<String, usize>,
    > = std::collections::HashMap::new();
    let mut bucket_scale_reason_counts: std::collections::HashMap<
        (String, String),
        std::collections::HashMap<String, usize>,
    > = std::collections::HashMap::new();
    let mut bucket_asset_counts: std::collections::HashMap<
        (String, String),
        std::collections::HashMap<String, usize>,
    > = std::collections::HashMap::new();

    for decision in &recent_gate_rejects {
        let key = (
            asset_class_for_label(Some(&decision.asset)).to_string(),
            normalized_event_subtype(decision.event_subtype.as_deref()).to_string(),
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
    let mut bucket_keys: std::collections::BTreeSet<(String, String)> =
        std::collections::BTreeSet::new();
    bucket_keys.extend(bucket_reject_reason_counts.keys().cloned());
    bucket_keys.extend(bucket_scale_reason_counts.keys().cloned());
    for (asset_class, event_subtype) in bucket_keys {
        let reject_reasons = bucket_reject_reason_counts
            .get(&(asset_class.clone(), event_subtype.clone()))
            .cloned()
            .unwrap_or_default();
        let scale_reasons = bucket_scale_reason_counts
            .get(&(asset_class.clone(), event_subtype.clone()))
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
            .get(&(asset_class.clone(), event_subtype.clone()))
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
            family.to_string(),
            dominant_reason,
            support_count,
            top_asset,
        ));
    }
    entry_bucket_actions.sort_by(|a, b| {
        b.4.cmp(&a.4)
            .then_with(|| a.0.cmp(&b.0))
            .then_with(|| a.1.cmp(&b.1))
    });
    for (asset_class, event_subtype, family, dominant_reason, support_count, top_asset) in
        entry_bucket_actions.into_iter().take(6)
    {
        let scope_label = bucket_scope_label(&asset_class, &event_subtype);
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
                    "edge_below_threshold",
                    format!("{scope_label} 最近主要因为 edge 不够进不去，先看 min_edge 是否过严。"),
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
        (String, String),
        std::collections::HashMap<String, usize>,
    > = std::collections::HashMap::new();
    let mut exit_bucket_asset_counts: std::collections::HashMap<
        (String, String),
        std::collections::HashMap<String, usize>,
    > = std::collections::HashMap::new();
    for decision in &recent_exits {
        let asset_label = decision.asset.as_deref().unwrap_or_default();
        let key = (
            asset_class_for_label(Some(asset_label)).to_string(),
            normalized_event_subtype(decision.event_subtype.as_deref()).to_string(),
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
    for ((asset_class, event_subtype), reason_counts_by_bucket) in exit_bucket_reason_counts {
        let Some((dominant_reason, support_count)) =
            dominant_bucket_reason(&reason_counts_by_bucket)
        else {
            continue;
        };
        let top_asset = exit_bucket_asset_counts
            .get(&(asset_class.clone(), event_subtype.clone()))
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
            dominant_reason,
            support_count,
            top_asset,
        ));
    }
    exit_bucket_actions.sort_by(|a, b| {
        b.3.cmp(&a.3)
            .then_with(|| a.0.cmp(&b.0))
            .then_with(|| a.1.cmp(&b.1))
    });
    for (asset_class, event_subtype, dominant_reason, support_count, top_asset) in
        exit_bucket_actions.into_iter().take(4)
    {
        let scope_label = bucket_scope_label(&asset_class, &event_subtype);
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
        "crypto_entry_tuning_hints": crypto_entry_tuning_hints,
        "crypto_override_suggestions": crypto_override_suggestions,
        "crypto_post_entry_tuning_hints": crypto_post_entry_tuning_hints,
        "crypto_post_entry_override_suggestions": crypto_post_entry_override_suggestions,
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
