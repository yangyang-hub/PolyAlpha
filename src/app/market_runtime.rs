//! Shared market/runtime orchestration.
//!
//! This module owns market-data startup, shared API-facing state, initial
//! discovery/subscription, and process-wide runtime tasks and shutdown flow.

use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use anyhow::{Context, Result};
use chrono::Utc;

use pa_core::config::{AccountConfig, Settings};
use pa_core::traits::MarketDataFeed;
use pa_market_data::data_api::PositionLoader;
use pa_market_data::gamma_feed::GammaFeed;
use pa_market_data::service::MarketDataService;

use crate::app::bootstrap::start_api_server;
use crate::app::helpers::{build_position_snapshot, build_ws_token_list, seed_market_cache};
use crate::app::types::AccountContext;

pub struct MarketRuntimeArtifacts {
    pub market_data: Arc<MarketDataService>,
    pub lr_runtime_status: Arc<tokio::sync::RwLock<pa_monitor::api::LrRuntimeStatus>>,
    pub shared_positions: Arc<tokio::sync::RwLock<Vec<pa_monitor::api::PositionApiEntry>>>,
    pub shared_positions_updated_at:
        Arc<tokio::sync::RwLock<Option<chrono::DateTime<Utc>>>>,
    pub startup_ready: Arc<AtomicBool>,
    pub shared_markets: Arc<tokio::sync::RwLock<Vec<pa_core::types::MarketInfo>>>,
    pub neg_risk_events: Vec<pa_core::types::NegRiskEvent>,
    pub binary_event_groups: Vec<pa_core::types::BinaryEventGroup>,
}

pub async fn initialize_market_runtime(
    settings: &Settings,
    active_enabled_strategies: &[String],
    resolved_accounts: &[AccountConfig],
    config_arc: Arc<arc_swap::ArcSwap<Settings>>,
) -> Result<Option<MarketRuntimeArtifacts>> {
    let mut discovery_settings = settings.clone();
    discovery_settings.strategy.enabled = active_enabled_strategies.to_vec();
    let market_data = Arc::new(
        MarketDataService::new(&discovery_settings)
            .context("Failed to initialize market data service")?,
    );
    tracing::info!("Market data service initialized");

    let ws_connected = market_data.ws_feed_ws_connected().await;
    let lr_runtime_status: Arc<tokio::sync::RwLock<pa_monitor::api::LrRuntimeStatus>> =
        Arc::new(tokio::sync::RwLock::new(pa_monitor::api::LrRuntimeStatus::default()));
    let shared_positions: Arc<tokio::sync::RwLock<Vec<pa_monitor::api::PositionApiEntry>>> =
        Arc::new(tokio::sync::RwLock::new(Vec::new()));
    let shared_positions_updated_at: Arc<tokio::sync::RwLock<Option<chrono::DateTime<Utc>>>> =
        Arc::new(tokio::sync::RwLock::new(None));
    let startup_ready = Arc::new(AtomicBool::new(false));
    start_api_server(
        settings,
        config_arc,
        Arc::clone(&ws_connected),
        Arc::clone(&lr_runtime_status),
        Arc::clone(&shared_positions),
        Arc::clone(&shared_positions_updated_at),
        Arc::clone(&startup_ready),
    );

    let Some(markets) = crate::app::bootstrap::discover_initial_markets(market_data.as_ref()).await else {
        return Ok(None);
    };
    tracing::info!(count = markets.len(), "Markets discovered");

    let neg_risk_events = GammaFeed::group_neg_risk_events(&markets);
    tracing::info!(
        neg_risk_events = neg_risk_events.len(),
        neg_risk_outcomes = neg_risk_events.iter().map(|e| e.markets.len()).sum::<usize>(),
        "NegRisk events discovered"
    );

    let binary_event_groups = GammaFeed::group_binary_events(&markets);
    tracing::info!(
        binary_event_groups = binary_event_groups.len(),
        grouped_markets = binary_event_groups.iter().map(|g| g.markets.len()).sum::<usize>(),
        "Binary event groups discovered"
    );

    pa_monitor::metrics::MONITORED_MARKETS.set(markets.len() as f64);
    let shared_markets = Arc::new(tokio::sync::RwLock::new(markets));

    {
        let seed_cache = market_data.cache().clone();
        let markets_snapshot = shared_markets.read().await;
        let mut seeded = 0u32;
        let mut no_price_data = 0u32;

        for m in markets_snapshot.iter() {
            if seed_market_cache(&seed_cache, m) {
                seeded += 1;
            } else if m.tokens.len() >= 2 {
                no_price_data += 1;
            }
        }

        tracing::info!(seeded, no_price_data, "OrderBookCache seeded with gamma prices");
    }

    let held_position_token_ids = load_held_position_token_ids(resolved_accounts).await;
    let ws_max = settings.market_filter.ws_max_instruments;
    {
        let markets_snapshot = shared_markets.read().await;
        let token_ids = build_ws_token_list(
            &markets_snapshot,
            &held_position_token_ids,
            active_enabled_strategies,
            ws_max,
        );

        tracing::info!(tokens = token_ids.len(), "Subscribing to order book updates (smart ordering)");
        market_data.subscribe(&token_ids).await?;
        pa_monitor::metrics::ACTIVE_SUBSCRIPTIONS.set(token_ids.len() as f64);
    }

    Ok(Some(MarketRuntimeArtifacts {
        market_data,
        lr_runtime_status,
        shared_positions,
        shared_positions_updated_at,
        startup_ready,
        shared_markets,
        neg_risk_events,
        binary_event_groups,
    }))
}

pub async fn populate_initial_positions_snapshot(
    account_contexts: &[AccountContext],
    shared_markets: &Arc<tokio::sync::RwLock<Vec<pa_core::types::MarketInfo>>>,
    market_data: &Arc<MarketDataService>,
    shared_positions: &Arc<tokio::sync::RwLock<Vec<pa_monitor::api::PositionApiEntry>>>,
    shared_positions_updated_at: &Arc<tokio::sync::RwLock<Option<chrono::DateTime<Utc>>>>,
) {
    let markets_snapshot = shared_markets.read().await;
    let api_cache = market_data.cache().clone();
    let entries = build_position_snapshot(account_contexts, &markets_snapshot, &api_cache);
    let count = entries.len();
    *shared_positions.write().await = entries;
    *shared_positions_updated_at.write().await = Some(Utc::now());
    if count > 0 {
        tracing::info!(positions = count, "API positions snapshot populated");
    }
}

pub fn spawn_shared_runtime_tasks(
    settings: &Settings,
    account_contexts: &[AccountContext],
    shared_markets: Arc<tokio::sync::RwLock<Vec<pa_core::types::MarketInfo>>>,
    market_data: Arc<MarketDataService>,
    shared_positions: Arc<tokio::sync::RwLock<Vec<pa_monitor::api::PositionApiEntry>>>,
    shared_positions_updated_at: Arc<tokio::sync::RwLock<Option<chrono::DateTime<Utc>>>>,
    active_enabled_strategies: Vec<String>,
    smart_money_token_maps: Vec<
        Arc<
            std::sync::RwLock<
                std::collections::HashMap<alloy::primitives::U256, alloy::primitives::B256>,
            >,
        >,
    >,
    cancel: tokio_util::sync::CancellationToken,
) {
    crate::app::tasks::spawn_positions_snapshot_refresh(
        account_contexts,
        Arc::clone(&shared_markets),
        market_data.cache().as_ref().clone(),
        shared_positions,
        shared_positions_updated_at,
        cancel.clone(),
    );

    crate::app::tasks::spawn_weather_forecast_snapshot_refresh(
        settings.weather.clone(),
        settings.database.clone(),
        cancel.clone(),
    );

    let refresh_interval = settings.market_filter.market_refresh_interval_secs;
    if refresh_interval > 0 {
        let refresh_risk_managers = account_contexts
            .iter()
            .map(|ctx| Arc::clone(&ctx.risk_manager_impl))
            .collect();
        crate::app::tasks::spawn_market_refresh(
            refresh_interval,
            shared_markets,
            market_data,
            active_enabled_strategies,
            settings.market_filter.ws_max_instruments,
            refresh_risk_managers,
            smart_money_token_maps,
            cancel,
        );
        tracing::info!(
            interval_secs = refresh_interval,
            "Periodic market refresh task started"
        );
    }
}

pub async fn shutdown_runtime(
    account_contexts: &[AccountContext],
    engine_handles: Vec<tokio::task::JoinHandle<()>>,
    cancel: tokio_util::sync::CancellationToken,
) -> Result<()> {
    tokio::signal::ctrl_c().await?;
    tracing::info!("Shutdown signal received");

    cancel.cancel();

    tracing::info!("Cancelling outstanding orders...");
    for ctx in account_contexts {
        if let Err(e) = ctx.executor.cancel_all().await {
            tracing::error!(account = %ctx.name, error = %e, "Failed to cancel orders on shutdown");
        }
    }

    for handle in engine_handles {
        let _ = handle.await;
    }

    tracing::info!("PolyAlpha shutdown complete");
    Ok(())
}

async fn load_held_position_token_ids(
    resolved_accounts: &[AccountConfig],
) -> Vec<alloy::primitives::U256> {
    let mut all_tokens: Vec<alloy::primitives::U256> = Vec::new();
    for acct in resolved_accounts {
        let proxy = if acct.proxy_wallet.is_empty() {
            let pk = match std::env::var(&acct.private_key_env) {
                Ok(k) => k,
                Err(_) => continue,
            };
            let s = match alloy::signers::local::PrivateKeySigner::from_str(&pk) {
                Ok(s) => s,
                Err(_) => continue,
            };
            s.address()
        } else {
            match acct.proxy_wallet.parse::<alloy::primitives::Address>() {
                Ok(a) => a,
                Err(_) => continue,
            }
        };
        let loader = match PositionLoader::new(proxy) {
            Ok(l) => l,
            Err(_) => continue,
        };
        match loader.load_positions().await {
            Ok(positions) => {
                for p in &positions {
                    if !all_tokens.contains(&p.token_id) {
                        all_tokens.push(p.token_id);
                    }
                }
            }
            Err(e) => {
                tracing::warn!(
                    account = %acct.name,
                    error = %e,
                    "Could not pre-load position tokens for WS priority"
                );
            }
        }
    }
    all_tokens
}
