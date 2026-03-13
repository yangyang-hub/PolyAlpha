mod app;

use std::sync::Arc;

use anyhow::Result;
use tokio_util::sync::CancellationToken;

use crate::app::bootstrap::{BootstrapArtifacts, init_tracing, load_runtime_settings};
use crate::app::account_runtime::spawn_account_runtime;
use crate::app::accounts::build_account_contexts;
use crate::app::market_runtime::{
    initialize_market_runtime, populate_initial_positions_snapshot, shutdown_runtime,
    spawn_shared_runtime_tasks,
};

#[tokio::main]
async fn main() -> Result<()> {
    // Load .env file if present
    dotenvy::dotenv().ok();

    init_tracing();

    tracing::info!("PolyAlpha starting...");

    let BootstrapArtifacts {
        settings,
        resolved_accounts,
        active_enabled_strategies,
        config_arc,
        config_tx,
        config_store,
    } = load_runtime_settings().await?;

    tracing::info!(
        chain_id = settings.chain.chain_id,
        clob_host = %settings.clob.host,
        "Configuration loaded"
    );

    // --- Global cancellation token ---
    let cancel = CancellationToken::new();

    let Some(runtime) = initialize_market_runtime(
        &settings,
        &active_enabled_strategies,
        &resolved_accounts,
        Arc::clone(&config_arc),
        config_tx,
        config_store,
    )
    .await?
    else {
        tracing::error!("All market discovery attempts failed, exiting");
        return Ok(());
    };
    let market_data = runtime.market_data;
    let lr_runtime_status = runtime.lr_runtime_status;
    let shared_positions = runtime.shared_positions;
    let shared_positions_updated_at = runtime.shared_positions_updated_at;
    let startup_ready = runtime.startup_ready;
    let shared_markets = runtime.shared_markets;
    let neg_risk_events = runtime.neg_risk_events;
    let binary_event_groups = runtime.binary_event_groups;

    // --- Build per-account contexts ---
    let account_contexts = build_account_contexts(
        &settings,
        &resolved_accounts,
        &market_data,
        &shared_markets,
        &neg_risk_events,
    )
    .await?;

    if account_contexts.is_empty() {
        tracing::error!("No valid accounts configured — exiting");
        return Ok(());
    }

    tracing::info!(
        active_accounts = account_contexts.len(),
        "All accounts initialized"
    );

    populate_initial_positions_snapshot(
        &account_contexts,
        &shared_markets,
        &market_data,
        &shared_positions,
        &shared_positions_updated_at,
    )
    .await;

    let mut engine_handles: Vec<tokio::task::JoinHandle<()>> = Vec::new();
    let mut smart_money_token_maps = Vec::new();
    for ctx in &account_contexts {
        let artifacts = spawn_account_runtime(
            &settings,
            ctx,
            &market_data,
            &shared_markets,
            &neg_risk_events,
            &binary_event_groups,
            Arc::clone(&lr_runtime_status),
            cancel.clone(),
        )
        .await;
        if let Some(handle) = artifacts.engine_handle {
            engine_handles.push(handle);
        }
        if let Some(token_map) = artifacts.smart_money_token_map {
            smart_money_token_maps.push(token_map);
        }
    }

    startup_ready.store(true, std::sync::atomic::Ordering::Relaxed);

    spawn_shared_runtime_tasks(
        &settings,
        &account_contexts,
        Arc::clone(&shared_markets),
        Arc::clone(&market_data),
        Arc::clone(&shared_positions),
        Arc::clone(&shared_positions_updated_at),
        active_enabled_strategies.clone(),
        smart_money_token_maps,
        cancel.clone(),
    );

    shutdown_runtime(&account_contexts, engine_handles, cancel).await
}
