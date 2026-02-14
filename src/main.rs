use std::str::FromStr;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use alloy::signers::Signer as _;
use alloy::signers::local::PrivateKeySigner;
use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::Utc;
use rust_decimal::Decimal;
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
use pa_market_data::event_calendar::EventCalendarService;
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
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            EnvFilter::new("info,polymarket_client_sdk::serde_helpers=error")
        }))
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
    let wallet_address = signer.address();
    tracing::info!(address = %wallet_address, "Wallet loaded");

    // --- Initialize database ---
    let db_url = std::env::var("PA_DATABASE__URL")
        .unwrap_or_else(|_| settings.database.url.clone());
    tracing::info!(
        db_host = db_url.split('@').last().unwrap_or("?"),
        from_env = std::env::var("PA_DATABASE__URL").is_ok(),
        "Connecting to database"
    );
    let repo = Repository::connect(&db_url, settings.database.max_connections)
        .await
        .context("Failed to connect to database")?;
    repo.migrate().await.context("Failed to run database migrations")?;
    tracing::info!("Database connected and migrations applied");

    // --- Global cancellation token ---
    let cancel = CancellationToken::new();

    // --- Initialize market data ---
    let market_data = MarketDataService::new(&settings)
        .context("Failed to initialize market data service")?;
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

    // --- Market spread observer (observation mode) ---
    // Every 60s, scans all binary markets and logs spread distribution.
    // Helps evaluate arbitrage opportunity density without executing trades.
    let observer_cache = market_data.cache().clone();
    let observer_markets = markets.clone();
    let observer_cancel = cancel.clone();
    let observer_settings = settings.strategy.clone();
    tokio::spawn(async move {
        // Wait 30s for order books to populate
        tokio::time::sleep(Duration::from_secs(30)).await;
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        let profit_calc = pa_strategy::profitability::ProfitCalculator::new(dec!(0.01));
        loop {
            tokio::select! {
                _ = observer_cancel.cancelled() => break,
                _ = interval.tick() => {
                    let total_markets = observer_markets.len();
                    let neg_risk_count = observer_markets.iter().filter(|m| m.neg_risk).count();
                    let mut total_checked = 0u32;
                    let mut has_book = 0u32;
                    let mut no_asks = 0u32;  // markets missing ask data
                    let mut no_bids = 0u32;  // markets missing bid data
                    // BuyAndMerge observations
                    let mut bm_any_spread = 0u32;       // YES+NO ask < 1.00
                    let mut bm_above_min_bps = 0u32;    // spread >= min_spread_bps
                    let mut bm_above_min_profit = 0u32;  // net_profit >= min_profit
                    let mut bm_best_spread = Decimal::ZERO;
                    let mut bm_best_question = String::new();
                    // SplitAndSell observations
                    let mut ss_any_spread = 0u32;
                    let mut ss_above_min_bps = 0u32;
                    let mut ss_above_min_profit = 0u32;
                    let mut ss_best_spread = Decimal::ZERO;
                    let mut ss_best_question = String::new();
                    // Track closest-to-opportunity (even when no arb exists)
                    let mut bm_tightest_gap = Decimal::new(999, 0);  // distance from $1.00
                    let mut bm_tightest_info = String::new();
                    let mut ss_tightest_gap = Decimal::new(999, 0);
                    let mut ss_tightest_info = String::new();
                    // Sample some actual prices for diagnostics
                    let mut sample_prices: Vec<String> = Vec::new();

                    for market in &observer_markets {
                        if market.neg_risk || market.tokens.len() != 2 || !market.active {
                            continue;
                        }
                        total_checked += 1;

                        let yes_book = match observer_cache.get(&market.tokens[0].token_id) {
                            Some(b) => b,
                            None => continue,
                        };
                        let no_book = match observer_cache.get(&market.tokens[1].token_id) {
                            Some(b) => b,
                            None => continue,
                        };
                        has_book += 1;

                        // --- BuyAndMerge check (ask side) ---
                        if let (Some(yes_ask), Some(no_ask)) = (yes_book.best_ask(), no_book.best_ask()) {
                            let total_cost = yes_ask.price + no_ask.price;
                            let gap = total_cost - Decimal::ONE; // positive = no arb, negative = arb exists

                            // Track tightest (closest to opportunity)
                            if gap < bm_tightest_gap {
                                bm_tightest_gap = gap;
                                let q = &market.question[..market.question.len().min(40)];
                                bm_tightest_info = format!(
                                    "{} Y_ask:{} N_ask:{} sum:{} gap:{}bps",
                                    q, yes_ask.price, no_ask.price, total_cost,
                                    (gap * dec!(10000)).round(),
                                );
                            }

                            // Sample first 5 markets for price diagnostics
                            if sample_prices.len() < 5 {
                                let q = &market.question[..market.question.len().min(30)];
                                sample_prices.push(format!(
                                    "{}.. Y:{}/{} N:{}/{} fee:{}bps",
                                    q,
                                    yes_book.best_bid().map(|b| b.price.to_string()).unwrap_or("-".into()),
                                    yes_ask.price,
                                    no_book.best_bid().map(|b| b.price.to_string()).unwrap_or("-".into()),
                                    no_ask.price,
                                    market.fee_rate_bps,
                                ));
                            }

                            if total_cost < Decimal::ONE {
                                let spread = Decimal::ONE - total_cost;
                                let spread_bps = (spread * dec!(10000)).to_string().parse::<u32>().unwrap_or(0);
                                let max_size = yes_ask.size.min(no_ask.size).min(observer_settings.max_trade_size_usdc);
                                let est = profit_calc.buy_and_merge_profit(yes_ask.price, no_ask.price, max_size, market.fee_rate_bps);

                                bm_any_spread += 1;
                                if spread_bps >= observer_settings.min_spread_bps {
                                    bm_above_min_bps += 1;
                                }
                                if est.net_profit >= observer_settings.min_profit_usdc {
                                    bm_above_min_profit += 1;
                                }
                                if spread > bm_best_spread {
                                    bm_best_spread = spread;
                                    let q = &market.question[..market.question.len().min(50)];
                                    bm_best_question = format!(
                                        "{} (ask:{:.2}+{:.2}={:.2}, spread:{:.1}bps, profit:${:.4}, size:{:.0})",
                                        q, yes_ask.price, no_ask.price, total_cost,
                                        spread * dec!(10000), est.net_profit, max_size,
                                    );
                                }
                            }
                        } else {
                            no_asks += 1;
                        }

                        // --- SplitAndSell check (bid side) ---
                        if let (Some(yes_bid), Some(no_bid)) = (yes_book.best_bid(), no_book.best_bid()) {
                            let total_revenue = yes_bid.price + no_bid.price;
                            let gap = Decimal::ONE - total_revenue; // positive = no arb

                            if gap < ss_tightest_gap {
                                ss_tightest_gap = gap;
                                let q = &market.question[..market.question.len().min(40)];
                                ss_tightest_info = format!(
                                    "{} Y_bid:{} N_bid:{} sum:{} gap:{}bps",
                                    q, yes_bid.price, no_bid.price, total_revenue,
                                    (gap * dec!(10000)).round(),
                                );
                            }

                            if total_revenue > Decimal::ONE {
                                let spread = total_revenue - Decimal::ONE;
                                let spread_bps = (spread * dec!(10000)).to_string().parse::<u32>().unwrap_or(0);
                                let max_size = yes_bid.size.min(no_bid.size).min(observer_settings.max_trade_size_usdc);
                                let est = profit_calc.split_and_sell_profit(yes_bid.price, no_bid.price, max_size, market.fee_rate_bps);

                                ss_any_spread += 1;
                                if spread_bps >= observer_settings.min_spread_bps {
                                    ss_above_min_bps += 1;
                                }
                                if est.net_profit >= observer_settings.min_profit_usdc {
                                    ss_above_min_profit += 1;
                                }
                                if spread > ss_best_spread {
                                    ss_best_spread = spread;
                                    let q = &market.question[..market.question.len().min(50)];
                                    ss_best_question = format!(
                                        "{} (bid:{:.2}+{:.2}={:.2}, spread:{:.1}bps, profit:${:.4}, size:{:.0})",
                                        q, yes_bid.price, no_bid.price, total_revenue,
                                        spread * dec!(10000), est.net_profit, max_size,
                                    );
                                }
                            }
                        } else {
                            no_bids += 1;
                        }
                    }

                    tracing::info!(
                        total_markets,
                        neg_risk = neg_risk_count,
                        binary_checked = total_checked,
                        with_orderbook = has_book,
                        missing_asks = no_asks,
                        missing_bids = no_bids,
                        "━━━ Market Spread Observer ━━━"
                    );
                    tracing::info!(
                        any_spread = bm_any_spread,
                        pass_bps = bm_above_min_bps,
                        pass_profit = bm_above_min_profit,
                        best_spread_bps = %format!("{:.0}", bm_best_spread * dec!(10000)),
                        tightest_gap_bps = %format!("{:.0}", bm_tightest_gap * dec!(10000)),
                        "[BuyAndMerge]"
                    );
                    if !bm_best_question.is_empty() {
                        tracing::info!(market = %bm_best_question, "[BuyAndMerge] best");
                    }
                    if !bm_tightest_info.is_empty() {
                        tracing::info!(market = %bm_tightest_info, "[BuyAndMerge] tightest");
                    }
                    tracing::info!(
                        any_spread = ss_any_spread,
                        pass_bps = ss_above_min_bps,
                        pass_profit = ss_above_min_profit,
                        best_spread_bps = %format!("{:.0}", ss_best_spread * dec!(10000)),
                        tightest_gap_bps = %format!("{:.0}", ss_tightest_gap * dec!(10000)),
                        "[SplitAndSell]"
                    );
                    if !ss_best_question.is_empty() {
                        tracing::info!(market = %ss_best_question, "[SplitAndSell] best");
                    }
                    if !ss_tightest_info.is_empty() {
                        tracing::info!(market = %ss_tightest_info, "[SplitAndSell] tightest");
                    }
                    // Log sample prices for data validation
                    for (i, sample) in sample_prices.iter().enumerate() {
                        tracing::info!(idx = i, sample = %sample, "[Sample]");
                    }
                }
            }
        }
    });

    // --- Initialize execution layer ---
    tracing::info!("Authenticating with CLOB API...");
    let clob = match ClobExecutor::connect(&settings.clob.host, signer).await {
        Ok(c) => {
            tracing::info!("CLOB authenticated");
            Some(c)
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "CLOB authentication failed — running in OBSERVE-ONLY mode. \
                 Ensure your wallet has logged into polymarket.com at least once."
            );
            None
        }
    };

    let provider = alloy::providers::ProviderBuilder::new()
        .connect(&settings.chain.rpc_url)
        .await
        .context("Failed to connect to RPC")?;
    let balance_provider = provider.clone();
    let ctf = CtfExecutor::with_neg_risk(provider, settings.chain.chain_id)
        .context("Failed to create CTF executor")?;
    tracing::info!("CTF executor initialized");

    // --- Query USDC balance ---
    let usdc_balance: Arc<std::sync::RwLock<Decimal>> =
        Arc::new(std::sync::RwLock::new(Decimal::ZERO));

    match query_usdc_balance(&balance_provider, wallet_address).await {
        Ok(bal) => {
            *usdc_balance.write().unwrap() = bal;
            tracing::info!(balance_usdc = %bal, "Wallet USDC balance loaded");
        }
        Err(e) => {
            tracing::warn!(error = %e, "Failed to query USDC balance — position sizing may be limited");
        }
    }

    // Periodic balance refresh (every 30s)
    let bal_state = Arc::clone(&usdc_balance);
    let bal_cancel = cancel.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(30));
        loop {
            tokio::select! {
                _ = bal_cancel.cancelled() => break,
                _ = interval.tick() => {
                    match query_usdc_balance(&balance_provider, wallet_address).await {
                        Ok(bal) => {
                            let prev = *bal_state.read().unwrap();
                            if bal != prev {
                                tracing::info!(balance_usdc = %bal, prev = %prev, "USDC balance updated");
                            }
                            *bal_state.write().unwrap() = bal;
                        }
                        Err(e) => {
                            tracing::debug!(error = %e, "Balance refresh failed");
                        }
                    }
                }
            }
        }
    });

    // Build executor: real orchestrator if CLOB available, dry-run otherwise
    let trading_enabled = clob.is_some();
    let executor: Arc<dyn pa_core::traits::Executor> = if let Some(clob) = clob {
        Arc::new(HybridOrchestrator::new(clob, ctf))
    } else {
        Arc::new(DryRunExecutor)
    };

    // --- Initialize risk manager ---
    let risk_manager_impl = Arc::new(RiskManagerImpl::new(settings.risk.clone()));
    let risk_manager: Arc<dyn pa_core::traits::RiskManager> =
        Arc::clone(&risk_manager_impl) as Arc<dyn pa_core::traits::RiskManager>;
    tracing::info!("Risk manager initialized");

    // --- Initialize strategy engine ---
    let cache = market_data.cache().clone();
    let cache2 = market_data.cache().clone();

    // Shared available capital closure: balance - total_exposure
    // Cloned for each strategy that needs it.
    let make_capital_fn = |bal: Arc<std::sync::RwLock<Decimal>>,
                           rm: Arc<dyn pa_core::traits::RiskManager>|
     -> Box<dyn Fn() -> Decimal + Send + Sync> {
        Box::new(move || {
            let balance = *bal.read().unwrap();
            let exposure = rm.total_exposure();
            (balance - exposure).max(Decimal::ZERO)
        })
    };

    let yes_no_strategy = YesNoArbitrage::new(
        settings.strategy.min_spread_bps,
        settings.strategy.max_trade_size_usdc,
        settings.strategy.min_profit_usdc,
        dec!(0.01), // gas cost estimate in USD
        Box::new(move |token_id| cache.get(&token_id)),
        make_capital_fn(Arc::clone(&usdc_balance), Arc::clone(&risk_manager)),
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
            neg_risk_events.clone(),
            Box::new(move |token_id| cache2.get(&token_id)),
            make_capital_fn(Arc::clone(&usdc_balance), Arc::clone(&risk_manager)),
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
            make_capital_fn(Arc::clone(&usdc_balance), Arc::clone(&risk_manager)),
        );
        strategies.push(Box::new(cross_market_strategy));
        tracing::info!("CrossMarket arbitrage strategy enabled");
    }

    // Add weather alpha strategy if enabled
    if settings.strategy.enabled.contains(&"weather".to_string()) {
        let weather_cache = market_data.cache().clone();
        let rm_pos = Arc::clone(&risk_manager_impl);
        let weather_strategy = pa_strategy::weather::WeatherAlphaStrategy::new(
            settings.weather.clone(),
            dec!(0.00), // no gas for CLOB-only
            Box::new(move |token_id| weather_cache.get(&token_id)),
            make_capital_fn(Arc::clone(&usdc_balance), Arc::clone(&risk_manager)),
            Box::new(move |tid: alloy::primitives::U256| rm_pos.get_position_size(&tid)),
            neg_risk_events.clone(),
        );
        strategies.push(Box::new(weather_strategy));
        tracing::info!("Weather alpha strategy enabled (binary + NegRisk)");
    }

    // Add resolution convergence strategy if enabled
    if settings.strategy.enabled.contains(&"convergence".to_string()) {
        let conv_cache = market_data.cache().clone();
        let rm_pos_conv = Arc::clone(&risk_manager_impl);
        let convergence = pa_strategy::convergence::ResolutionConvergenceStrategy::new(
            settings.convergence.clone(),
            dec!(0.00), // no gas for CLOB-only
            Box::new(move |token_id| conv_cache.get(&token_id)),
            make_capital_fn(Arc::clone(&usdc_balance), Arc::clone(&risk_manager)),
            Box::new(move |tid: alloy::primitives::U256| rm_pos_conv.get_position_size(&tid)),
        );
        strategies.push(Box::new(convergence));
        tracing::info!("Resolution convergence strategy enabled");
    }

    // Add crypto alpha strategy if enabled
    if settings.strategy.enabled.contains(&"crypto".to_string()) {
        let crypto_cache = market_data.cache().clone();
        let rm_pos_crypto = Arc::clone(&risk_manager_impl);
        let crypto = pa_strategy::crypto_alpha::CryptoAlphaStrategy::new(
            settings.crypto_alpha.clone(),
            dec!(0.00), // no gas for CLOB-only
            Box::new(move |token_id| crypto_cache.get(&token_id)),
            make_capital_fn(Arc::clone(&usdc_balance), Arc::clone(&risk_manager)),
            Box::new(move |tid: alloy::primitives::U256| rm_pos_crypto.get_position_size(&tid)),
        );
        strategies.push(Box::new(crypto));
        tracing::info!("Crypto alpha strategy enabled");
    }

    // --- Initialize event calendar filter ---
    let event_calendar = if settings.event_calendar.enabled {
        let ec = Arc::new(EventCalendarService::new(settings.event_calendar.clone()));
        ec.refresh().await;
        tracing::info!(events = ec.event_count().await, "Event calendar initialized");
        Some(ec)
    } else {
        None
    };

    let engine = StrategyEngine::new(
        strategies,
        executor.clone(),
        risk_manager.clone(),
        settings.strategy.scan_interval_ms,
        event_calendar,
    );

    tracing::info!(
        trading = trading_enabled,
        "PolyAlpha initialized — entering trading loop"
    );

    // --- Run trading loop with graceful shutdown ---
    let engine_cancel = cancel.clone();
    let engine_handle = tokio::spawn(async move {
        engine.run(&markets, update_rx, engine_cancel).await;
    });

    // --- Daily circuit breaker reset at midnight UTC ---
    let daily_rm = Arc::clone(&risk_manager);
    let daily_cancel = cancel.clone();
    tokio::spawn(async move {
        loop {
            // Calculate seconds until next midnight UTC
            let now = chrono::Utc::now();
            let today = now.date_naive();
            let next_midnight = (today + chrono::Duration::days(1))
                .and_hms_opt(0, 0, 0)
                .unwrap();
            let until_midnight = next_midnight
                .signed_duration_since(now.naive_utc())
                .to_std()
                .unwrap_or(Duration::from_secs(3600));

            tokio::select! {
                _ = daily_cancel.cancelled() => break,
                _ = tokio::time::sleep(until_midnight) => {
                    daily_rm.reset_daily();
                    tracing::info!("Daily risk counters reset at midnight UTC");
                }
            }
        }
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

/// No-op executor used when CLOB authentication fails (observe-only mode).
///
/// Logs detected opportunities without executing any trades.
struct DryRunExecutor;

#[async_trait]
impl pa_core::traits::Executor for DryRunExecutor {
    async fn execute(
        &self,
        opportunity: &pa_core::types::ArbitrageOpportunity,
    ) -> pa_core::Result<pa_core::types::ExecutionResult> {
        tracing::info!(
            id = %opportunity.id,
            strategy = ?opportunity.strategy_type,
            profit = %opportunity.estimated_profit,
            "[DRY-RUN] Would execute opportunity"
        );
        Ok(pa_core::types::ExecutionResult {
            opportunity_id: opportunity.id,
            status: pa_core::types::ExecutionStatus::NoFill,
            trades: vec![],
            realized_profit: rust_decimal::Decimal::ZERO,
            total_fees: rust_decimal::Decimal::ZERO,
            total_gas: rust_decimal::Decimal::ZERO,
            executed_at: chrono::Utc::now(),
        })
    }

    async fn cancel_all(&self) -> pa_core::Result<()> {
        Ok(())
    }
}

/// Query USDC balance for a wallet address on Polygon.
///
/// Uses alloy's sol! macro to call `balanceOf(address)` on the USDC contract.
/// USDC on Polygon has 6 decimals, so the returned value is scaled accordingly.
async fn query_usdc_balance(
    provider: &impl alloy::providers::Provider,
    wallet: alloy::primitives::Address,
) -> anyhow::Result<Decimal> {
    alloy::sol! {
        #[sol(rpc)]
        contract IERC20 {
            function balanceOf(address account) external view returns (uint256);
        }
    }

    let usdc = pa_execution::ctf_executor::USDC_ADDRESS;
    let contract = IERC20::new(usdc, provider);
    let result = contract.balanceOf(wallet).call().await?;

    // USDC has 6 decimals — use try_into to avoid panic on absurd values
    let balance_u64: u64 = result.try_into().unwrap_or(u64::MAX);
    Ok(Decimal::from(balance_u64) / Decimal::from(1_000_000u64))
}
