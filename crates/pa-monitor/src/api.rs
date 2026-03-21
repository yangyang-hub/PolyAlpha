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

    fn top_label(entries: &[serde_json::Value]) -> Option<String> {
        entries
            .first()
            .and_then(|entry| entry.get("label"))
            .and_then(|value| value.as_str())
            .map(ToString::to_string)
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

    fn push_override_suggestion(
        suggestions: &mut Vec<serde_json::Value>,
        priority: &str,
        target_field: &str,
        direction: &str,
        asset: Option<&str>,
        subtype: Option<&str>,
        source_reason: &str,
        rationale: String,
    ) {
        let asset_class = asset_class_for_label(asset);
        let event_subtype = normalized_event_subtype(subtype);
        suggestions.push(json!({
            "priority": priority,
            "target_field": target_field,
            "direction": direction,
            "selector_asset_class": asset_class,
            "selector_event_subtype": event_subtype,
            "scope_label": match (asset, subtype) {
                (Some(asset), Some(subtype)) if subtype != "generic" => format!("{asset} / {subtype}"),
                (Some(asset), _) => asset.to_string(),
                (_, Some(subtype)) if subtype != "generic" => subtype.to_string(),
                _ => asset_class.to_string(),
            },
            "source_reason": source_reason,
            "rationale": rationale,
        }));
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

    let mut crypto_entry_tuning_hints = Vec::new();
    let mut crypto_override_suggestions = Vec::new();
    if let Some(reason) = top_label(&reason_counts) {
        let asset = top_label(&asset_counts).unwrap_or_else(|| "当前资产池".into());
        let subtype = top_label(&subtype_counts).unwrap_or_else(|| "generic".into());
        match reason.as_str() {
            "asset_exposure_cap" => push_hint(
                &mut crypto_entry_tuning_hints,
                "gate_reject",
                "high",
                "先看敞口上限",
                format!(
                    "{asset} 最近最常撞前门敞口限制，先检查单资产/单方向上限，而不是先放宽 entry 参数。事件类型：{subtype}。"
                ),
            ),
            "min_order_or_budget" => push_hint(
                &mut crypto_entry_tuning_hints,
                "gate_reject",
                "high",
                "先看预算与 sizing",
                format!(
                    "{asset} 最近更像是预算或最小下单约束问题，优先检查 sizing、可用资金和最小成交额。事件类型：{subtype}。"
                ),
            ),
            "edge_below_threshold" => push_hint(
                &mut crypto_entry_tuning_hints,
                "gate_reject",
                "medium",
                "先看 edge 门槛",
                format!(
                    "{asset} 最近主要卡在 edge，不要先动流动性参数；优先检查 min_edge 与概率校准。事件类型：{subtype}。"
                ),
            ),
            "spread_too_wide" => push_hint(
                &mut crypto_entry_tuning_hints,
                "gate_reject",
                "medium",
                "先看 spread 与市场流动性",
                format!(
                    "{asset} 最近主要卡在价差，优先确认 market spread 是否长期偏宽，再决定是否放宽 max_spread。事件类型：{subtype}。"
                ),
            ),
            "insufficient_depth_buffer" | "insufficient_size_retention" => push_hint(
                &mut crypto_entry_tuning_hints,
                "gate_reject",
                "medium",
                "先看执行约束",
                format!(
                    "{asset} 最近主要卡在 depth/size retention，优先检查 depth ratio、size retention 和 event-aware execution tuning。事件类型：{subtype}。"
                ),
            ),
            _ => {}
        }
        match reason.as_str() {
            "edge_below_threshold" => push_override_suggestion(
                &mut crypto_override_suggestions,
                "medium",
                "min_edge_multiplier",
                "loosen",
                Some(&asset),
                Some(&subtype),
                "edge_below_threshold",
                format!(
                    "{asset} 最近主要因为 edge 不足被挡在前门，适合先审视对应桶的 min_edge 是否过严。"
                ),
            ),
            "spread_too_wide" => push_override_suggestion(
                &mut crypto_override_suggestions,
                "medium",
                "max_spread_multiplier",
                "loosen",
                Some(&asset),
                Some(&subtype),
                "spread_too_wide",
                format!(
                    "{asset} 最近主要因为 spread 过宽被挡掉，若确认市场长期偏宽，可放松对应桶的 max_spread。"
                ),
            ),
            "insufficient_depth_buffer" => push_override_suggestion(
                &mut crypto_override_suggestions,
                "medium",
                "depth_ratio_multiplier",
                "loosen",
                Some(&asset),
                Some(&subtype),
                "insufficient_depth_buffer",
                format!(
                    "{asset} 最近主要卡在 depth buffer，若不是想继续牺牲成交概率，可先放松对应桶的 depth ratio。"
                ),
            ),
            "insufficient_size_retention" => push_override_suggestion(
                &mut crypto_override_suggestions,
                "medium",
                "size_retention_multiplier",
                "loosen",
                Some(&asset),
                Some(&subtype),
                "insufficient_size_retention",
                format!(
                    "{asset} 最近主要卡在 retained size，若不是想继续缩量，可先放松对应桶的 retained-size 约束。"
                ),
            ),
            "asset_exposure_cap" => push_override_suggestion(
                &mut crypto_override_suggestions,
                "high",
                "max_exposure_per_asset_pct",
                "raise",
                Some(&asset),
                Some(&subtype),
                "asset_exposure_cap",
                format!(
                    "{asset} 最近反复撞资产敞口上限，更像是风控容量问题，而不是 entry 参数问题。"
                ),
            ),
            _ => {}
        }
    }
    if let Some(reason) = top_label(&gate_scale_reason_counts_view) {
        let asset = top_label(&gate_scale_asset_counts_view).unwrap_or_else(|| "当前资产池".into());
        let subtype =
            top_label(&gate_scale_subtype_counts_view).unwrap_or_else(|| "generic".into());
        match reason.as_str() {
            "scaled_for_depth_buffer" => push_hint(
                &mut crypto_entry_tuning_hints,
                "gate_scale",
                "medium",
                "前门主要在预缩量而非拒绝",
                format!(
                    "{asset} 最近更多是为满足 depth buffer 被前置缩量，说明 entry 能过但目标下单量偏乐观。优先检查 depth ratio、Kelly 和事件桶 depth multiplier。事件类型：{subtype}。"
                ),
            ),
            "scaled_for_size_retention" => push_hint(
                &mut crypto_entry_tuning_hints,
                "gate_scale",
                "medium",
                "数量保真主导缩量",
                format!(
                    "{asset} 最近更多是为了满足 retained size 被缩量，说明 execution 质量约束在主导。优先检查 size retention multiplier 和 event-aware sizing。事件类型：{subtype}。"
                ),
            ),
            _ => {}
        }
        match reason.as_str() {
            "scaled_for_depth_buffer" => push_override_suggestion(
                &mut crypto_override_suggestions,
                "medium",
                "size_multiplier",
                "tighten",
                Some(&asset),
                Some(&subtype),
                "scaled_for_depth_buffer",
                format!(
                    "{asset} 最近更多是为了满足 depth buffer 被预缩量，说明这个桶的目标下单量偏大，适合先收紧 size。"
                ),
            ),
            "scaled_for_size_retention" => push_override_suggestion(
                &mut crypto_override_suggestions,
                "medium",
                "size_multiplier",
                "tighten",
                Some(&asset),
                Some(&subtype),
                "scaled_for_size_retention",
                format!(
                    "{asset} 最近更多是为了满足 retained size 被预缩量，说明这个桶的 sizing 对当前流动性过于乐观。"
                ),
            ),
            _ => {}
        }
    }
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
