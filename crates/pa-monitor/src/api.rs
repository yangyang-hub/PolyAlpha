use std::sync::Arc;

use arc_swap::ArcSwap;
use axum::extract::{Path, State};
use axum::response::Json;
use axum::{Router, routing::get};
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use tower_http::cors::CorsLayer;
use tower_http::services::{ServeDir, ServeFile};

use pa_core::config::Settings;
use pa_storage::config_store::{ConfigStore, validate_section, extract_section};

use crate::health::HealthCheck;

/// Shared API state for all Axum handlers.
pub struct ApiState {
    pub config: Arc<ArcSwap<Settings>>,
    pub config_store: ConfigStore,
    pub config_tx: tokio::sync::watch::Sender<u64>,
    pub start_time: DateTime<Utc>,
    pub health_checks: Vec<(&'static str, HealthCheck)>,
}

/// Build the full Axum router with health, metrics, config API, and SPA fallback.
pub fn build_router(state: Arc<ApiState>) -> Router {
    let api_routes = Router::new()
        .route("/api/config", get(get_all_config))
        .route("/api/config/{section}", get(get_section).put(put_section))
        .route("/api/config/history/{section}", get(get_history))
        .route("/api/status", get(get_status));

    Router::new()
        .route("/health", get(health_handler))
        .route("/ready", get(readiness_handler))
        .route("/metrics", get(metrics_handler))
        .merge(api_routes)
        .fallback_service(
            ServeDir::new("frontend/dist")
                .fallback(ServeFile::new("frontend/dist/index.html")),
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
        checks.insert(
            name.to_string(),
            json!(if ok { "ok" } else { "error" }),
        );
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
    if all_ok {
        (axum::http::StatusCode::OK, Json(json!({"ready": true})))
    } else {
        (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"ready": false})),
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

/// PUT /api/config/:section — update section, validate, save to DB, hot reload.
async fn put_section(
    State(state): State<Arc<ApiState>>,
    Path(section): Path<String>,
    Json(body): Json<Value>,
) -> (axum::http::StatusCode, Json<Value>) {
    // Reject accounts section
    if section == "accounts" {
        return (
            axum::http::StatusCode::FORBIDDEN,
            Json(json!({"error": "accounts section cannot be modified via API"})),
        );
    }

    // Validate by deserializing into typed struct
    if let Err(e) = validate_section(&section, &body) {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("Validation failed: {}", e)})),
        );
    }

    // Save to DB
    let new_version = match state.config_store.save_section(&section, &body).await {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(error = %e, section = %section, "Failed to save config section");
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("Failed to save: {}", e)})),
            );
        }
    };

    // Rebuild full settings: load TOML base → apply all DB overrides
    let mut new_settings = match Settings::load() {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "Failed to reload TOML settings");
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("Failed to reload base config: {}", e)})),
            );
        }
    };

    match state.config_store.load_all().await {
        Ok(all_overrides) => {
            if let Err(e) = ConfigStore::apply_overrides(&mut new_settings, &all_overrides) {
                tracing::error!(error = %e, "Failed to apply DB overrides");
                return (
                    axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": format!("Failed to apply overrides: {}", e)})),
                );
            }
        }
        Err(e) => {
            tracing::error!(error = %e, "Failed to load all overrides");
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("Failed to load overrides: {}", e)})),
            );
        }
    }

    // Hot swap
    state.config.store(Arc::new(new_settings));

    // Notify watchers
    let _ = state.config_tx.send(new_version as u64);

    tracing::info!(section = %section, version = new_version, "Config section updated via API");

    (
        axum::http::StatusCode::OK,
        Json(json!({
            "section": section,
            "version": new_version,
            "status": "applied"
        })),
    )
}

/// GET /api/config/history/:section — change history.
async fn get_history(
    State(state): State<Arc<ApiState>>,
    Path(section): Path<String>,
) -> (axum::http::StatusCode, Json<Value>) {
    match state.config_store.history(&section, 50).await {
        Ok(rows) => {
            let entries: Vec<Value> = rows
                .iter()
                .map(|r| {
                    json!({
                        "version": r.version,
                        "data": r.data,
                        "changed_by": r.changed_by,
                        "created_at": r.created_at.to_rfc3339(),
                    })
                })
                .collect();
            (axum::http::StatusCode::OK, Json(json!(entries)))
        }
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("Failed to load history: {}", e)})),
        ),
    }
}

/// GET /api/status — bot status summary.
async fn get_status(State(state): State<Arc<ApiState>>) -> Json<Value> {
    let uptime_secs = (Utc::now() - state.start_time).num_seconds();
    let settings = state.config.load();

    Json(json!({
        "uptime_seconds": uptime_secs,
        "enabled_strategies": settings.strategy.enabled,
        "scan_interval_ms": settings.strategy.scan_interval_ms,
        "health_port": settings.monitor.health_port,
        "lr_enabled": settings.liquidity_rewards.enabled,
        "event_calendar_enabled": settings.event_calendar.enabled,
    }))
}
