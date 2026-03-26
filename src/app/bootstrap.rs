//! Startup-time bootstrap helpers.
//!
//! This module owns process-level initialization such as tracing, configuration
//! loading, and API server startup.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::atomic::{AtomicBool, AtomicI64};

use anyhow::{Context, Result};
use arc_swap::ArcSwap;
use chrono::Utc;
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

use pa_core::config::{AccountConfig, Settings};
use pa_core::traits::MarketDataFeed;
use pa_market_data::service::MarketDataService;
use pa_monitor::api::{ApiState, LrRuntimeStatus, PositionApiEntry};
use pa_storage::repository::Repository;

pub struct BootstrapArtifacts {
    pub settings: Settings,
    pub resolved_accounts: Vec<AccountConfig>,
    pub active_enabled_strategies: Vec<String>,
    pub config_arc: Arc<ArcSwap<Settings>>,
    pub smart_money_config_arc: Arc<ArcSwap<pa_core::config::SmartMoneyConfig>>,
}

pub fn init_tracing() {
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            EnvFilter::new(
                "info,polymarket_client_sdk=warn,polymarket_client_sdk::serde_helpers=off",
            )
        }))
        .with(fmt::layer().with_target(true).with_thread_ids(true))
        .init();
}

pub async fn load_runtime_settings() -> Result<BootstrapArtifacts> {
    let mut settings = Settings::load().context("Failed to load configuration")?;
    if let Err(e) = settings.reapply_env_overrides() {
        tracing::warn!(error = %e, "Failed to re-apply environment config overrides");
    }

    if settings.database.url.is_empty() {
        tracing::info!("No database URL configured — config store disabled");
    } else {
        tracing::info!("Database URL configured");
    }

    let resolved_accounts = settings.resolved_accounts();
    tracing::info!(
        count = resolved_accounts.len(),
        names = ?resolved_accounts.iter().map(|a| &a.name).collect::<Vec<_>>(),
        "Trading accounts resolved"
    );

    settings.merge_account_strategies_into_enabled();
    let active_enabled_strategies = settings.active_account_enabled_strategies();
    let config_arc = Arc::new(ArcSwap::new(Arc::new(settings.clone())));
    let smart_money_config_arc = Arc::new(ArcSwap::new(Arc::new(settings.smart_money.clone())));

    Ok(BootstrapArtifacts {
        settings,
        resolved_accounts,
        active_enabled_strategies,
        config_arc,
        smart_money_config_arc,
    })
}

pub fn start_api_server(
    settings: &Settings,
    config_arc: Arc<ArcSwap<Settings>>,
    smart_money_config_arc: Arc<ArcSwap<pa_core::config::SmartMoneyConfig>>,
    ws_connected: Arc<std::sync::atomic::AtomicBool>,
    ws_last_message_unix: Arc<AtomicI64>,
    lr_runtime_status: Arc<tokio::sync::RwLock<LrRuntimeStatus>>,
    shared_positions: Arc<tokio::sync::RwLock<Vec<PositionApiEntry>>>,
    shared_positions_updated_at: Arc<tokio::sync::RwLock<Option<chrono::DateTime<Utc>>>>,
    wallet_balance: Arc<tokio::sync::RwLock<rust_decimal::Decimal>>,
    strategy_financials: Arc<
        tokio::sync::RwLock<
            std::collections::HashMap<String, pa_monitor::api::StrategyFinancialEntry>,
        >,
    >,
    repository: Option<Arc<Repository>>,
    startup_ready: Arc<AtomicBool>,
) {
    let websocket_health_grace_secs = 180i64;
    let api_state = Arc::new(ApiState {
        config: config_arc,
        smart_money_config: smart_money_config_arc,
        start_time: Utc::now(),
        health_checks: vec![(
            "websocket",
            Box::new(move || {
                if ws_connected.load(Ordering::Relaxed) {
                    return true;
                }
                let last_message_unix = ws_last_message_unix.load(Ordering::Relaxed);
                last_message_unix > 0
                    && (Utc::now().timestamp() - last_message_unix) <= websocket_health_grace_secs
            }),
        )],
        lr_status: Some(lr_runtime_status),
        positions: shared_positions,
        positions_updated_at: shared_positions_updated_at,
        wallet_balance,
        strategy_financials,
        repository,
        startup_ready,
    });
    pa_monitor::api::spawn_crypto_override_patch_auto_apply(Arc::clone(&api_state));
    let health_port = settings.monitor.health_port;
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .thread_name("api-server")
            .build()
            .expect("Failed to build API runtime");
        rt.block_on(async move {
            if let Err(e) = pa_monitor::api::start_server(health_port, api_state).await {
                tracing::error!(error = %e, "API server failed");
            }
        });
    });
}

pub async fn discover_initial_markets(
    market_data: &MarketDataService,
) -> Option<Vec<pa_core::types::MarketInfo>> {
    let mut markets = Vec::new();
    for attempt in 0..5u32 {
        if attempt > 0 {
            let delay = std::time::Duration::from_secs(10 * (attempt as u64));
            tracing::warn!(
                attempt = attempt + 1,
                delay_secs = delay.as_secs(),
                "Retrying market discovery"
            );
            tokio::time::sleep(delay).await;
        }
        tracing::info!("Discovering markets from Gamma API...");
        match market_data.discover_markets().await {
            Ok(m) if !m.is_empty() => {
                markets = m;
                break;
            }
            Ok(_) => {
                tracing::warn!(attempt = attempt + 1, "No markets found, will retry");
            }
            Err(e) => {
                tracing::error!(attempt = attempt + 1, error = %e, "Market discovery failed, will retry");
            }
        }
    }

    if markets.is_empty() {
        None
    } else {
        Some(markets)
    }
}
