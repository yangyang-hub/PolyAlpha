use std::str::FromStr;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use alloy::signers::Signer as _;
use alloy::signers::local::PrivateKeySigner;
use anyhow::{Context, Result};
use chrono::Utc;
use rust_decimal_macros::dec;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

use pa_core::config::Settings;
use pa_core::traits::MarketDataFeed;
use pa_execution::clob_executor::ClobExecutor;
use pa_execution::ctf_executor::CtfExecutor;
use pa_execution::orchestrator::HybridOrchestrator;
use pa_market_data::gamma_feed::GammaFeed;
use pa_market_data::service::MarketDataService;
use pa_monitor::health::HealthState;
use pa_risk::manager::RiskManagerImpl;
use pa_storage::models::OrderBookSnapshotRow;
use pa_storage::repository::Repository;
use pa_strategy::cross_market::{CrossMarketArbitrage, detect_cross_market_pairs};
use pa_strategy::engine::StrategyEngine;
use pa_strategy::neg_risk::NegRiskArbitrage;
use pa_strategy::yes_no::YesNoArbitrage;

#[tokio::main]
async fn main() -> Result<()> {
    // Load .env file if present
    dotenvy::dotenv().ok();

    // Initialize tracing
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with(fmt::layer().with_target(true).with_thread_ids(true))
        .init();

    tracing::info!("PolyAlpha starting...");

    // --- Load configuration ---
    let settings = Settings::load().context("Failed to load configuration")?;
    tracing::info!(
        chain_id = settings.chain.chain_id,
        clob_host = %settings.clob.host,
        "Configuration loaded"
    );

    // --- Load signer from env ---
    let private_key = std::env::var("POLYMARKET_PRIVATE_KEY")
        .context("POLYMARKET_PRIVATE_KEY environment variable not set")?;
    let signer = PrivateKeySigner::from_str(&private_key)
        .context("Invalid private key")?
        .with_chain_id(Some(settings.chain.chain_id));
    tracing::info!(address = %signer.address(), "Wallet loaded");

    // --- Initialize database ---
    let repo = Repository::connect(&settings.database.url, settings.database.max_connections)
        .await
        .context("Failed to connect to database")?;
    repo.migrate().await.context("Failed to run database migrations")?;
    tracing::info!("Database connected and migrations applied");

    // --- Global cancellation token ---
    let cancel = CancellationToken::new();

    // --- Initialize market data ---
    let market_data = MarketDataService::with_defaults(&settings);
    tracing::info!("Market data service initialized");

    // --- Start health/metrics server ---
    let ws_connected = market_data.ws_feed_ws_connected().await;
    let health_state = Arc::new(HealthState {
        checks: vec![
            (
                "websocket",
                Box::new({
                    let ws = Arc::clone(&ws_connected);
                    move || ws.load(Ordering::Relaxed)
                }),
            ),
        ],
        start_time: Utc::now(),
    });
    let health_port = settings.monitor.health_port;
    tokio::spawn(async move {
        if let Err(e) = pa_monitor::health::start_server(health_port, health_state).await {
            tracing::error!(error = %e, "Health server failed");
        }
    });

    // --- Discover markets ---
    tracing::info!("Discovering markets from Gamma API...");
    let markets = market_data.discover_markets().await?;
    tracing::info!(count = markets.len(), "Markets discovered");

    if markets.is_empty() {
        tracing::warn!("No markets found, exiting");
        return Ok(());
    }

    // Group NegRisk multi-outcome events
    let neg_risk_events = GammaFeed::group_neg_risk_events(&markets);
    tracing::info!(
        neg_risk_events = neg_risk_events.len(),
        neg_risk_outcomes = neg_risk_events.iter().map(|e| e.markets.len()).sum::<usize>(),
        "NegRisk events discovered"
    );

    // Detect cross-market pairs
    let cross_market_pairs = detect_cross_market_pairs(&markets);

    // Update metrics
    pa_monitor::metrics::MONITORED_MARKETS.set(markets.len() as f64);

    // --- Subscribe to WebSocket order book updates ---
    let mut token_ids = GammaFeed::extract_token_ids(&markets);
    let neg_risk_token_ids = GammaFeed::extract_neg_risk_token_ids(&neg_risk_events);
    // Merge NegRisk token IDs (some may overlap with binary market tokens)
    for tid in &neg_risk_token_ids {
        if !token_ids.contains(tid) {
            token_ids.push(*tid);
        }
    }
    tracing::info!(tokens = token_ids.len(), "Subscribing to order book updates");
    market_data.subscribe(&token_ids).await?;
    pa_monitor::metrics::ACTIVE_SUBSCRIPTIONS.set(token_ids.len() as f64);

    // Get broadcast receiver for strategy engine
    let update_rx = market_data.ws_feed().await.subscribe_updates();

    // --- Snapshot recording pipeline ---
    let snapshot_repo = repo.clone();
    let snapshot_cache = market_data.cache().clone();
    let snapshot_cancel = cancel.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        loop {
            tokio::select! {
                _ = snapshot_cancel.cancelled() => break,
                _ = interval.tick() => {
                    let token_ids = snapshot_cache.token_ids();
                    let mut recorded = 0u64;
                    for token_id in &token_ids {
                        if let Some(book) = snapshot_cache.get(token_id) {
                            let bids_json = price_levels_to_json(&book.bids);
                            let asks_json = price_levels_to_json(&book.asks);
                            let row = OrderBookSnapshotRow {
                                id: 0,
                                token_id: token_id.to_string(),
                                timestamp: book.timestamp,
                                bids: bids_json,
                                asks: asks_json,
                                best_bid: book.best_bid().map(|l| l.price),
                                best_ask: book.best_ask().map(|l| l.price),
                                midpoint: book.midpoint(),
                            };
                            if let Err(e) = snapshot_repo.insert_orderbook_snapshot(&row).await {
                                tracing::warn!(error = %e, token_id = %token_id, "Failed to record snapshot");
                            } else {
                                recorded += 1;
                            }
                        }
                    }
                    if recorded > 0 {
                        pa_monitor::metrics::SNAPSHOTS_RECORDED.inc_by(recorded);
                        tracing::debug!(count = recorded, "Snapshots recorded");
                    }
                }
            }
        }
    });

    // --- Initialize execution layer ---
    tracing::info!("Authenticating with CLOB API...");
    let clob = ClobExecutor::connect(&settings.clob.host, signer).await
        .context("Failed to authenticate with CLOB API")?;
    tracing::info!("CLOB authenticated");

    let provider = alloy::providers::ProviderBuilder::new()
        .connect(&settings.chain.rpc_url)
        .await
        .context("Failed to connect to RPC")?;
    let ctf = CtfExecutor::with_neg_risk(provider, settings.chain.chain_id)
        .context("Failed to create CTF executor")?;
    tracing::info!("CTF executor initialized");

    let orchestrator = HybridOrchestrator::new(clob, ctf);
    let executor: Arc<dyn pa_core::traits::Executor> = Arc::new(orchestrator);

    // --- Initialize risk manager ---
    let risk_manager: Arc<dyn pa_core::traits::RiskManager> =
        Arc::new(RiskManagerImpl::new(settings.risk.clone()));
    tracing::info!("Risk manager initialized");

    // --- Initialize strategy engine ---
    let cache = market_data.cache().clone();
    let cache2 = market_data.cache().clone();

    let yes_no_strategy = YesNoArbitrage::new(
        settings.strategy.min_spread_bps,
        settings.strategy.max_trade_size_usdc,
        settings.strategy.min_profit_usdc,
        dec!(0.01), // gas cost estimate in USD
        Box::new(move |token_id| cache.get(&token_id)),
    );

    let mut strategies: Vec<Box<dyn pa_core::traits::Strategy>> =
        vec![Box::new(yes_no_strategy)];

    // Add NegRisk strategy if events were found
    if !neg_risk_events.is_empty() {
        let neg_risk_strategy = NegRiskArbitrage::new(
            settings.strategy.min_spread_bps,
            settings.strategy.max_trade_size_usdc,
            settings.strategy.min_profit_usdc,
            dec!(0.01), // gas cost estimate in USD
            neg_risk_events,
            Box::new(move |token_id| cache2.get(&token_id)),
        );
        strategies.push(Box::new(neg_risk_strategy));
        tracing::info!("NegRisk arbitrage strategy enabled");
    }

    // Add cross-market strategy if pairs were found
    if !cross_market_pairs.is_empty() {
        let cache3 = market_data.cache().clone();
        let cross_market_strategy = CrossMarketArbitrage::new(
            settings.strategy.min_spread_bps,
            settings.strategy.max_trade_size_usdc,
            settings.strategy.min_profit_usdc,
            dec!(0.02), // 2x gas for two on-chain txs
            cross_market_pairs,
            Box::new(move |token_id| cache3.get(&token_id)),
        );
        strategies.push(Box::new(cross_market_strategy));
        tracing::info!("CrossMarket arbitrage strategy enabled");
    }

    let engine = StrategyEngine::new(
        strategies,
        executor.clone(),
        risk_manager.clone(),
        settings.strategy.scan_interval_ms,
    );

    tracing::info!("PolyAlpha initialized — entering trading loop");

    // --- Run trading loop with graceful shutdown ---
    let engine_cancel = cancel.clone();
    let engine_handle = tokio::spawn(async move {
        engine.run(&markets, update_rx, engine_cancel).await;
    });

    // Wait for Ctrl+C
    tokio::signal::ctrl_c().await?;
    tracing::info!("Shutdown signal received");

    // Cancel all tasks
    cancel.cancel();

    // Cancel outstanding orders before exit
    tracing::info!("Cancelling outstanding orders...");
    if let Err(e) = executor.cancel_all().await {
        tracing::error!(error = %e, "Failed to cancel orders on shutdown");
    }

    // Wait for engine to finish
    let _ = engine_handle.await;

    tracing::info!("PolyAlpha shutdown complete");
    Ok(())
}

/// Convert price levels to JSON array format: [[price, size], ...]
fn price_levels_to_json(levels: &[pa_core::types::PriceLevel]) -> serde_json::Value {
    serde_json::Value::Array(
        levels
            .iter()
            .map(|l| {
                serde_json::Value::Array(vec![
                    serde_json::Value::String(l.price.to_string()),
                    serde_json::Value::String(l.size.to_string()),
                ])
            })
            .collect(),
    )
}
