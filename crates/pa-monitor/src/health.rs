use std::sync::Arc;

use axum::extract::State;
use axum::response::Json;
use axum::{Router, routing::get};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use std::net::SocketAddr;

/// Health check function: returns true if the dependency is healthy.
pub type HealthCheck = Box<dyn Fn() -> bool + Send + Sync>;

/// Application health state passed to the HTTP server.
pub struct HealthState {
    pub checks: Vec<(&'static str, HealthCheck)>,
    pub start_time: DateTime<Utc>,
}

/// Start the health check and metrics HTTP server.
pub async fn start_server(health_port: u16, state: Arc<HealthState>) -> anyhow::Result<()> {
    let app = Router::new()
        .route("/health", get(health_handler))
        .route("/ready", get(readiness_handler))
        .route("/metrics", get(metrics_handler))
        .with_state(state);

    let addr = SocketAddr::from(([0, 0, 0, 0], health_port));
    tracing::info!(%addr, "Starting health/metrics server");

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

async fn health_handler(State(state): State<Arc<HealthState>>) -> Json<Value> {
    let uptime_secs = (Utc::now() - state.start_time).num_seconds();

    let mut checks = serde_json::Map::new();
    let mut all_ok = true;

    for (name, check) in &state.checks {
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
    State(state): State<Arc<HealthState>>,
) -> (axum::http::StatusCode, Json<Value>) {
    let all_ok = state.checks.iter().all(|(_, check)| check());

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
