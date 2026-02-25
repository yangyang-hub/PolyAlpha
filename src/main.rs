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
use rust_decimal::prelude::ToPrimitive;
use rust_decimal_macros::dec;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

use pa_core::config::Settings;
use pa_core::traits::MarketDataFeed;
use pa_core::traits::RiskManager as _;
use pa_execution::clob_executor::ClobExecutor;
use pa_execution::ctf_executor::CtfExecutor;
use pa_execution::orchestrator::HybridOrchestrator;
use pa_market_data::gamma_feed::GammaFeed;
use pa_market_data::service::MarketDataService;
use pa_market_data::event_calendar::EventCalendarService;
use pa_market_data::data_api::PositionLoader;
use pa_monitor::health::HealthState;
use pa_risk::manager::RiskManagerImpl;
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

    // --- Discover markets (with retry) ---
    let markets = {
        let mut markets = Vec::new();
        for attempt in 0..5u32 {
            if attempt > 0 {
                let delay = std::time::Duration::from_secs(10 * (attempt as u64)); // 10s, 20s, 30s, 40s
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
            tracing::error!("All market discovery attempts failed, exiting");
            return Ok(());
        }
        markets
    };
    tracing::info!(count = markets.len(), "Markets discovered");

    // Group NegRisk multi-outcome events
    let neg_risk_events = GammaFeed::group_neg_risk_events(&markets);
    tracing::info!(
        neg_risk_events = neg_risk_events.len(),
        neg_risk_outcomes = neg_risk_events.iter().map(|e| e.markets.len()).sum::<usize>(),
        "NegRisk events discovered"
    );

    // Group binary events by event title (for grouped binary market strategies)
    let binary_event_groups = GammaFeed::group_binary_events(&markets);
    tracing::info!(
        binary_event_groups = binary_event_groups.len(),
        grouped_markets = binary_event_groups.iter().map(|g| g.markets.len()).sum::<usize>(),
        "Binary event groups discovered"
    );

    // Detect cross-market pairs
    let cross_market_pairs = detect_cross_market_pairs(&markets);

    // Update metrics
    pa_monitor::metrics::MONITORED_MARKETS.set(markets.len() as f64);

    // --- Seed OrderBookCache with gamma best_bid/best_ask ---
    // Directional strategies (crypto, weather, convergence) need order book prices to
    // detect edges. The WS subscription can only hold 500 instruments (~250 markets),
    // but we discover 500+ markets. By seeding the cache with gamma API best_bid/best_ask,
    // ALL markets have baseline price data for strategy evaluation.
    // WS updates will overwrite these with real-time data for subscribed markets.
    {
        let seed_cache = market_data.cache().clone();
        let mut seeded_bid_ask = 0u32;
        let mut seeded_outcome = 0u32;
        let mut no_price_data = 0u32;

        for m in &markets {
            if m.tokens.len() < 2 {
                continue;
            }

            // Prefer gamma best_bid/best_ask (actual CLOB top-of-book) over outcome_prices (last trade)
            let (yes_bid, yes_ask) = if let (Some(bid), Some(ask)) = (m.gamma_best_bid, m.gamma_best_ask) {
                if ask > Decimal::ZERO && ask <= Decimal::ONE && bid > Decimal::ZERO {
                    seeded_bid_ask += 1;
                    (bid, ask)
                } else {
                    // Invalid bid/ask, try outcome_prices
                    match m.outcome_prices.as_ref().and_then(|p| p.first().copied()) {
                        Some(yp) if yp > Decimal::ZERO && yp < Decimal::ONE => {
                            seeded_outcome += 1;
                            ((yp - dec!(0.01)).max(dec!(0.01)), yp)
                        }
                        _ => { no_price_data += 1; continue; }
                    }
                }
            } else {
                // No bid/ask from gamma, try outcome_prices as fallback
                match m.outcome_prices.as_ref().and_then(|p| p.first().copied()) {
                    Some(yp) if yp > Decimal::ZERO && yp < Decimal::ONE => {
                        seeded_outcome += 1;
                        ((yp - dec!(0.01)).max(dec!(0.01)), yp)
                    }
                    _ => { no_price_data += 1; continue; }
                }
            };

            // Compute NO side prices (approximate: NO ≈ 1 - YES)
            let no_ask = (Decimal::ONE - yes_bid).min(dec!(0.99));
            let no_bid = (Decimal::ONE - yes_ask).max(dec!(0.01));

            // Seed YES token order book
            seed_cache.update(
                m.tokens[0].token_id,
                pa_core::types::OrderBook {
                    token_id: m.tokens[0].token_id,
                    bids: vec![pa_core::types::PriceLevel { price: yes_bid, size: dec!(1000) }],
                    asks: vec![pa_core::types::PriceLevel { price: yes_ask, size: dec!(1000) }],
                    timestamp: Utc::now(),
                },
            );

            // Seed NO token order book
            seed_cache.update(
                m.tokens[1].token_id,
                pa_core::types::OrderBook {
                    token_id: m.tokens[1].token_id,
                    bids: vec![pa_core::types::PriceLevel { price: no_bid, size: dec!(1000) }],
                    asks: vec![pa_core::types::PriceLevel { price: no_ask, size: dec!(1000) }],
                    timestamp: Utc::now(),
                },
            );
        }

        tracing::info!(
            seeded_bid_ask,
            seeded_outcome,
            no_price_data,
            total = seeded_bid_ask + seeded_outcome,
            "OrderBookCache seeded with gamma prices"
        );
    }

    // --- Subscribe to WebSocket order book updates ---
    // Smart ordering: filter extreme prices for ALL markets, then prioritize by strategy relevance + mid-ness.
    let ws_max = settings.market_filter.ws_max_instruments;

    let mut strategy_mid: Vec<(alloy::primitives::U256, alloy::primitives::U256, f64)> = Vec::new(); // strategy-relevant, mid-range
    let mut general_mid: Vec<(alloy::primitives::U256, alloy::primitives::U256, f64)> = Vec::new();  // non-strategy, mid-range
    let mut extreme_filtered = 0u32;
    let mut strategy_extreme = 0u32;

    for m in &markets {
        if m.neg_risk || m.tokens.len() != 2 || !m.active {
            continue;
        }

        // Use gamma_best_ask (actual CLOB state) for filtering, fallback to outcome_prices
        let yes_price = m.gamma_best_ask
            .and_then(|p| p.to_f64())
            .or_else(|| {
                m.outcome_prices.as_ref()
                    .and_then(|p| p.first().copied())
                    .and_then(|p| p.to_f64())
            });

        // Filter extreme prices for ALL markets (including strategy-relevant)
        // Markets with YES < 0.05 or > 0.95 have extreme order books (0.001/0.999)
        if let Some(yp) = yes_price {
            if yp < 0.05 || yp > 0.95 {
                if GammaFeed::is_relevant_for_strategies(&m.question, &settings.strategy.enabled) {
                    strategy_extreme += 1;
                }
                extreme_filtered += 1;
                continue;
            }
            let dist = (yp - 0.50_f64).abs();
            if GammaFeed::is_relevant_for_strategies(&m.question, &settings.strategy.enabled) {
                strategy_mid.push((m.tokens[0].token_id, m.tokens[1].token_id, dist));
            } else {
                general_mid.push((m.tokens[0].token_id, m.tokens[1].token_id, dist));
            }
        } else {
            // No price data: include with worst priority in appropriate group
            if GammaFeed::is_relevant_for_strategies(&m.question, &settings.strategy.enabled) {
                strategy_mid.push((m.tokens[0].token_id, m.tokens[1].token_id, 1.0));
            } else {
                general_mid.push((m.tokens[0].token_id, m.tokens[1].token_id, 1.0));
            }
        }
    }

    // Sort each group by mid-ness (closest to 0.50 first)
    strategy_mid.sort_by(|a, b| a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal));
    general_mid.sort_by(|a, b| a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal));

    // Build token list: strategy-relevant first, then general
    let mut token_ids: Vec<alloy::primitives::U256> = Vec::new();
    for (yes_tid, no_tid, _) in &strategy_mid {
        token_ids.push(*yes_tid);
        token_ids.push(*no_tid);
    }
    let strategy_token_count = token_ids.len();
    for (yes_tid, no_tid, _) in &general_mid {
        token_ids.push(*yes_tid);
        token_ids.push(*no_tid);
    }

    // Append NegRisk tokens after binary
    let neg_risk_token_ids: Vec<_> = markets
        .iter()
        .filter(|m| m.neg_risk)
        .flat_map(|m| m.tokens.iter().map(|t| t.token_id))
        .collect();
    for tid in &neg_risk_token_ids {
        if !token_ids.contains(tid) {
            token_ids.push(*tid);
        }
    }

    // Truncate to WS limit
    let total_before_trunc = token_ids.len();
    token_ids.truncate(ws_max);

    tracing::info!(
        tokens = token_ids.len(),
        strategy_mid = strategy_token_count,
        strategy_extreme,
        general_mid = general_mid.len() * 2,
        neg_risk = neg_risk_token_ids.len(),
        extreme_filtered,
        truncated = total_before_trunc.saturating_sub(ws_max),
        "Subscribing to order book updates (smart ordering)"
    );
    market_data.subscribe(&token_ids).await?;
    pa_monitor::metrics::ACTIVE_SUBSCRIPTIONS.set(token_ids.len() as f64);

    // Get broadcast receiver for strategy engine
    let update_rx = market_data.ws_feed().await.subscribe_updates();

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
                                let gamma_yes = market.outcome_prices.as_ref()
                                    .and_then(|p| p.first())
                                    .map(|p| p.to_string())
                                    .unwrap_or("-".into());
                                let gamma_ask = market.gamma_best_ask
                                    .map(|p| p.to_string())
                                    .unwrap_or("-".into());
                                // Detect if book is gamma-seeded (size=1000) vs WS-sourced
                                let source = if yes_ask.size == dec!(1000) { "gamma" } else { "ws" };
                                sample_prices.push(format!(
                                    "{}.. Y:{}/{} N:{}/{} fee:{}bps src:{} gamma_ask:{} outcome:{}",
                                    q,
                                    yes_book.best_bid().map(|b| b.price.to_string()).unwrap_or("-".into()),
                                    yes_ask.price,
                                    no_book.best_bid().map(|b| b.price.to_string()).unwrap_or("-".into()),
                                    no_ask.price,
                                    market.fee_rate_bps,
                                    source,
                                    gamma_ask,
                                    gamma_yes,
                                ));
                            }

                            if total_cost < Decimal::ONE {
                                let spread = Decimal::ONE - total_cost;
                                let spread_bps = {
                                    use rust_decimal::prelude::ToPrimitive;
                                    (spread * dec!(10000)).to_u32().unwrap_or(0)
                                };
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
                                let spread_bps = {
                                    use rust_decimal::prelude::ToPrimitive;
                                    (spread * dec!(10000)).to_u32().unwrap_or(0)
                                };
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
                        cache_size = observer_cache.len(),
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
    let clob = match ClobExecutor::connect(&settings.clob.host, signer, settings.clob.signature_type).await {
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
    let ctf = CtfExecutor::with_neg_risk(provider, settings.chain.chain_id)
        .context("Failed to create CTF executor")?;
    tracing::info!("CTF executor initialized");

    // Second CTF executor for auto-redeem (independent of HybridOrchestrator)
    let redeem_provider = alloy::providers::ProviderBuilder::new()
        .connect(&settings.chain.rpc_url)
        .await
        .context("Failed to connect to RPC for redeem executor")?;
    let redeem_ctf = CtfExecutor::with_neg_risk(redeem_provider, settings.chain.chain_id)
        .context("Failed to create redeem CTF executor")?;

    // Build executor: real orchestrator if CLOB available, dry-run otherwise
    let trading_enabled = clob.is_some();
    let executor: Arc<dyn pa_core::traits::Executor> = if let Some(clob) = clob {
        Arc::new(HybridOrchestrator::new(clob, ctf))
    } else {
        Arc::new(DryRunExecutor)
    };

    // --- Query USDC balance from CLOB API (proxy wallet balance) ---
    let usdc_balance: Arc<std::sync::RwLock<Decimal>> =
        Arc::new(std::sync::RwLock::new(Decimal::ZERO));

    match executor.get_balance().await {
        Ok(bal) => {
            *usdc_balance.write().unwrap() = bal;
            if let Some(f) = bal.to_f64() {
                pa_monitor::metrics::USDC_BALANCE.set(f);
            }
            tracing::info!(balance_usdc = %bal, "CLOB collateral balance loaded");
        }
        Err(e) => {
            tracing::warn!(error = %e, "Failed to query CLOB balance — position sizing may be limited");
        }
    }

    // Periodic balance refresh (every 30s) via CLOB API
    let bal_state = Arc::clone(&usdc_balance);
    let bal_executor = Arc::clone(&executor);
    let bal_cancel = cancel.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(30));
        loop {
            tokio::select! {
                _ = bal_cancel.cancelled() => break,
                _ = interval.tick() => {
                    match bal_executor.get_balance().await {
                        Ok(bal) => {
                            let prev = *bal_state.read().unwrap();
                            if bal != prev {
                                tracing::info!(balance_usdc = %bal, prev = %prev, "USDC balance updated");
                            }
                            *bal_state.write().unwrap() = bal;
                            if let Some(f) = bal.to_f64() {
                                pa_monitor::metrics::USDC_BALANCE.set(f);
                            }
                        }
                        Err(e) => {
                            tracing::debug!(error = %e, "Balance refresh failed");
                        }
                    }
                }
            }
        }
    });

    // --- Initialize risk manager ---
    let risk_manager_impl = Arc::new(RiskManagerImpl::new(settings.risk.clone()));

    // --- Load positions from Data API ---
    let proxy_addr = if settings.clob.proxy_wallet.is_empty() {
        if settings.clob.signature_type != 0 {
            tracing::warn!(
                signature_type = settings.clob.signature_type,
                eoa = %wallet_address,
                "proxy_wallet not configured — querying Data API with EOA address. \
                 If positions show 0 but you have holdings, set clob.proxy_wallet \
                 to your Polymarket proxy wallet address."
            );
        }
        wallet_address
    } else {
        settings.clob.proxy_wallet.parse::<alloy::primitives::Address>()
            .context("Invalid proxy_wallet address")?
    };
    let position_loader = PositionLoader::new(proxy_addr)?;
    let api_positions = position_loader.load_positions().await
        .context("Failed to load positions from Data API")?;
    let initial_positions: Vec<_> = api_positions
        .iter()
        .map(|p| {
            // Infer strategy_type from market data so exit logic works after restart
            let strategy_type = infer_strategy_type(p.token_id, &markets, &neg_risk_events);
            if strategy_type.is_some() {
                tracing::debug!(
                    token_id = %p.token_id,
                    strategy = ?strategy_type,
                    size = %p.size,
                    "Position tagged with inferred strategy"
                );
            }
            (p.token_id, p.size, p.avg_price, strategy_type, Some(p.condition_id))
        })
        .collect();
    let loaded_count = initial_positions.len();
    let tagged_count = initial_positions.iter().filter(|p| p.3.is_some()).count();
    risk_manager_impl.load_initial_positions(initial_positions);
    if loaded_count > 0 {
        tracing::info!(
            loaded = loaded_count,
            tagged = tagged_count,
            untagged = loaded_count - tagged_count,
            "Positions loaded from Data API (exit logic active for tagged positions)"
        );
    }
    tracing::info!(
        loaded = loaded_count,
        exposure = %risk_manager_impl.total_exposure(),
        "Positions loaded from Data API"
    );

    let risk_manager: Arc<dyn pa_core::traits::RiskManager> =
        Arc::clone(&risk_manager_impl) as Arc<dyn pa_core::traits::RiskManager>;
    tracing::info!("Risk manager initialized");

    // --- Initialize strategy engine ---
    let cache = market_data.cache().clone();

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

    let mut strategies: Vec<Box<dyn pa_core::traits::Strategy>> = Vec::new();

    // Add YesNo arbitrage strategy if enabled
    if settings.strategy.enabled.contains(&"yes_no".to_string()) {
        let yes_no_strategy = YesNoArbitrage::new(
            settings.strategy.min_spread_bps,
            settings.strategy.max_trade_size_usdc,
            settings.strategy.min_profit_usdc,
            dec!(0.01), // gas cost estimate in USD
            Box::new({
                let c = cache.clone();
                move |token_id| c.get(&token_id)
            }),
            make_capital_fn(Arc::clone(&usdc_balance), Arc::clone(&risk_manager)),
        );
        strategies.push(Box::new(yes_no_strategy));
        tracing::info!("YesNo arbitrage strategy enabled");
    }

    // Add NegRisk strategy if enabled and events were found
    if settings.strategy.enabled.contains(&"neg_risk".to_string()) && !neg_risk_events.is_empty() {
        let neg_risk_strategy = NegRiskArbitrage::new(
            settings.strategy.min_spread_bps,
            settings.strategy.max_trade_size_usdc,
            settings.strategy.min_profit_usdc,
            dec!(0.01), // gas cost estimate in USD
            neg_risk_events.clone(),
            Box::new({
                let c = cache.clone();
                move |token_id| c.get(&token_id)
            }),
            make_capital_fn(Arc::clone(&usdc_balance), Arc::clone(&risk_manager)),
        );
        strategies.push(Box::new(neg_risk_strategy));
        tracing::info!("NegRisk arbitrage strategy enabled");
    }

    // Add cross-market strategy if enabled and pairs were found
    if settings.strategy.enabled.contains(&"cross_market".to_string()) && !cross_market_pairs.is_empty() {
        let cross_market_strategy = CrossMarketArbitrage::new(
            settings.strategy.min_spread_bps,
            settings.strategy.max_trade_size_usdc,
            settings.strategy.min_profit_usdc,
            dec!(0.02), // 2x gas for two on-chain txs
            cross_market_pairs,
            Box::new({
                let c = cache.clone();
                move |token_id| c.get(&token_id)
            }),
            make_capital_fn(Arc::clone(&usdc_balance), Arc::clone(&risk_manager)),
        );
        strategies.push(Box::new(cross_market_strategy));
        tracing::info!("CrossMarket arbitrage strategy enabled");
    }

    // Add weather alpha strategy if enabled
    if settings.strategy.enabled.contains(&"weather".to_string()) {
        let weather_cache = market_data.cache().clone();
        let rm_pos = Arc::clone(&risk_manager_impl);
        let rm_held = Arc::clone(&risk_manager_impl);
        let weather_strategy = pa_strategy::weather::WeatherAlphaStrategy::new(
            settings.weather.clone(),
            dec!(0.00), // no gas for CLOB-only
            Box::new(move |token_id| weather_cache.get(&token_id)),
            make_capital_fn(Arc::clone(&usdc_balance), Arc::clone(&risk_manager)),
            Box::new(move |tid: alloy::primitives::U256| rm_pos.get_position_size(&tid)),
            neg_risk_events.clone(),
            Box::new(move || rm_held.positions_by_strategy(pa_core::types::StrategyType::Weather)),
        );
        strategies.push(Box::new(weather_strategy));
        tracing::info!("Weather alpha strategy enabled (binary + NegRisk)");
    }

    // Add resolution convergence strategy if enabled
    if settings.strategy.enabled.contains(&"convergence".to_string()) {
        let conv_cache = market_data.cache().clone();
        let rm_pos_conv = Arc::clone(&risk_manager_impl);
        let rm_held_conv = Arc::clone(&risk_manager_impl);
        let convergence = pa_strategy::convergence::ResolutionConvergenceStrategy::new(
            settings.convergence.clone(),
            dec!(0.00), // no gas for CLOB-only
            Box::new(move |token_id| conv_cache.get(&token_id)),
            make_capital_fn(Arc::clone(&usdc_balance), Arc::clone(&risk_manager)),
            Box::new(move |tid: alloy::primitives::U256| rm_pos_conv.get_position_size(&tid)),
            Box::new(move || rm_held_conv.positions_by_strategy(pa_core::types::StrategyType::ResolutionConvergence)),
        );
        strategies.push(Box::new(convergence));
        tracing::info!("Resolution convergence strategy enabled");
    }

    // Add crypto alpha strategy if enabled
    if settings.strategy.enabled.contains(&"crypto".to_string()) {
        let crypto_cache = market_data.cache().clone();
        let rm_pos_crypto = Arc::clone(&risk_manager_impl);
        let rm_held_crypto = Arc::clone(&risk_manager_impl);
        let crypto = pa_strategy::crypto_alpha::CryptoAlphaStrategy::new(
            settings.crypto_alpha.clone(),
            dec!(0.00), // no gas for CLOB-only
            Box::new(move |token_id| crypto_cache.get(&token_id)),
            make_capital_fn(Arc::clone(&usdc_balance), Arc::clone(&risk_manager)),
            Box::new(move |tid: alloy::primitives::U256| rm_pos_crypto.get_position_size(&tid)),
            neg_risk_events.clone(),
            binary_event_groups.clone(),
            Box::new(move || rm_held_crypto.positions_by_strategy(pa_core::types::StrategyType::CryptoAlpha)),
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

    let engine_cache = market_data.cache().clone();
    let engine = StrategyEngine::new(
        strategies,
        executor.clone(),
        risk_manager.clone(),
        settings.strategy.scan_interval_ms,
        event_calendar,
        Box::new(move |token_id| engine_cache.get(&token_id)),
        settings.risk.min_order_usdc,
        settings.strategy.max_market_end_days,
    );

    tracing::info!(
        trading = trading_enabled,
        "PolyAlpha initialized — entering trading loop"
    );

    // --- Run trading loop with graceful shutdown ---
    let all_markets_for_mm = markets.clone(); // clone before markets is moved into engine
    let engine_cancel = cancel.clone();
    let engine_handle = tokio::spawn(async move {
        engine.run(&markets, update_rx, engine_cancel).await;
    });

    // --- Market Making background task ---
    if settings.market_making.enabled && trading_enabled {
        let mm_config = settings.market_making.clone();
        let mm_cache = market_data.cache().clone();
        let mm_cancel = cancel.clone();
        let mm_rm = Arc::clone(&risk_manager_impl);
        let mm_markets = all_markets_for_mm;
        let mm_private_key = private_key.clone();

        // Create a second CLOB executor for MM to avoid request contention
        let mm_clob_host = settings.clob.host.clone();
        let mm_sig_type = settings.clob.signature_type;
        let mm_chain_id = settings.chain.chain_id;
        tokio::spawn(async move {
            // Authenticate a separate CLOB client for market making
            let mm_signer = match PrivateKeySigner::from_str(&mm_private_key) {
                Ok(s) => s.with_chain_id(Some(mm_chain_id)),
                Err(e) => {
                    tracing::error!(error = %e, "MM: failed to parse signer");
                    return;
                }
            };
            let mm_clob = match ClobExecutor::connect(&mm_clob_host, mm_signer, mm_sig_type).await {
                Ok(c) => c,
                Err(e) => {
                    tracing::error!(error = %e, "MM: CLOB authentication failed, market making disabled");
                    return;
                }
            };
            tracing::info!("MM: CLOB authenticated, starting market making");

            // Select top N mid-range, non-NegRisk markets by outcome price proximity to 0.50
            let mut candidates: Vec<&pa_core::types::MarketInfo> = mm_markets
                .iter()
                .filter(|m| {
                    !m.neg_risk
                        && m.active
                        && m.tokens.len() == 2
                        && m.outcome_prices
                            .as_ref()
                            .and_then(|p| p.first())
                            .and_then(|p| p.to_f64())
                            .map(|yp| yp >= 0.20 && yp <= 0.80)
                            .unwrap_or(false)
                })
                .collect();
            candidates.sort_by(|a, b| {
                let ya = a.outcome_prices.as_ref().and_then(|p| p.first()).and_then(|p| p.to_f64()).unwrap_or(0.5);
                let yb = b.outcome_prices.as_ref().and_then(|p| p.first()).and_then(|p| p.to_f64()).unwrap_or(0.5);
                let da = (ya - 0.5_f64).abs();
                let db = (yb - 0.5_f64).abs();
                da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
            });
            candidates.truncate(mm_config.max_markets);

            if candidates.is_empty() {
                tracing::warn!("MM: no suitable markets found for market making");
                return;
            }

            // Collect market data for the MM loop
            let mm_market_infos: Vec<pa_core::types::MarketInfo> = candidates.into_iter().cloned().collect();
            pa_monitor::metrics::MM_ACTIVE_MARKETS.set(mm_market_infos.len() as f64);

            tracing::info!(
                markets = mm_market_infos.len(),
                questions = ?mm_market_infos.iter().map(|m| &m.question[..m.question.len().min(40)]).collect::<Vec<_>>(),
                "MM: selected markets"
            );

            // Track outstanding order IDs per market: condition_id -> (bid_order_id, ask_order_id)
            let mut outstanding_orders: std::collections::HashMap<
                alloy::primitives::B256,
                (Option<String>, Option<String>),
            > = std::collections::HashMap::new();

            let half_spread = Decimal::new(mm_config.target_spread_bps as i64, 4) / Decimal::TWO;
            let mut interval = tokio::time::interval(Duration::from_secs(mm_config.quote_refresh_secs));

            loop {
                tokio::select! {
                    _ = mm_cancel.cancelled() => {
                        // Cancel all MM orders on shutdown
                        let all_ids: Vec<String> = outstanding_orders.values()
                            .flat_map(|(bid, ask)| bid.iter().chain(ask.iter()).cloned())
                            .collect();
                        if !all_ids.is_empty() {
                            let refs: Vec<&str> = all_ids.iter().map(|s| s.as_str()).collect();
                            if let Err(e) = mm_clob.cancel_orders(&refs).await {
                                tracing::warn!(error = %e, "MM: failed to cancel orders on shutdown");
                            }
                            pa_monitor::metrics::MM_ORDERS_CANCELLED.inc_by(all_ids.len() as u64);
                        }
                        tracing::info!("MM: shutdown, cancelled {} orders", all_ids.len());
                        break;
                    }
                    _ = interval.tick() => {
                        for market in &mm_market_infos {
                            let cid = market.condition_id;
                            let yes_tid = market.tokens[0].token_id;
                            let _no_tid = market.tokens[1].token_id;

                            // Cancel previous orders for this market
                            if let Some((bid_id, ask_id)) = outstanding_orders.remove(&cid) {
                                let ids: Vec<String> = bid_id.into_iter().chain(ask_id).collect();
                                if !ids.is_empty() {
                                    let refs: Vec<&str> = ids.iter().map(|s| s.as_str()).collect();
                                    let _ = mm_clob.cancel_orders(&refs).await;
                                    pa_monitor::metrics::MM_ORDERS_CANCELLED.inc_by(ids.len() as u64);
                                }
                            }

                            // Get midpoint from order book cache
                            let midpoint = match mm_cache.get(&yes_tid) {
                                Some(book) => match book.midpoint() {
                                    Some(mid) => mid,
                                    None => continue,
                                },
                                None => continue,
                            };

                            // Compute inventory skew
                            let position = mm_rm.get_position_size(&yes_tid);
                            let skew = if mm_config.max_position_per_market > Decimal::ZERO {
                                (position / mm_config.max_position_per_market) * mm_config.inventory_skew_factor
                            } else {
                                Decimal::ZERO
                            };

                            // Compute bid/ask prices with inventory skew
                            // Positive position → widen bid (lower), tighten ask (higher)
                            let bid_price = (midpoint - half_spread - skew * half_spread)
                                .round_dp(2)
                                .max(Decimal::new(1, 2)); // min 0.01
                            let ask_price = (midpoint + half_spread - skew * half_spread)
                                .round_dp(2)
                                .min(Decimal::new(99, 2)); // max 0.99

                            if bid_price >= ask_price {
                                continue; // spread too tight after skew
                            }

                            // Check position limit
                            let remaining = mm_config.max_position_per_market - position;
                            if remaining <= Decimal::ZERO {
                                // At max position, skip bid (buy) side
                                continue;
                            }
                            let order_size = remaining.min(Decimal::from(10)).round_dp(2);
                            if order_size < Decimal::ONE {
                                continue;
                            }

                            // Place GTC bid (buy YES)
                            let bid_result = mm_clob.buy_limit(yes_tid, bid_price, order_size).await;
                            let bid_order_id = match bid_result {
                                Ok(r) => {
                                    pa_monitor::metrics::MM_ORDERS_PLACED.inc();
                                    Some(r.order_id)
                                }
                                Err(e) => {
                                    tracing::debug!(error = %e, market = %cid, "MM: bid order failed");
                                    None
                                }
                            };

                            // Place GTC ask (sell YES) only if holding inventory
                            let ask_order_id = if position > Decimal::ZERO {
                                let sell_size = position.min(Decimal::from(10)).round_dp(2);
                                if sell_size >= Decimal::ONE {
                                    match mm_clob.sell_limit(yes_tid, ask_price, sell_size).await {
                                        Ok(r) => {
                                            pa_monitor::metrics::MM_ORDERS_PLACED.inc();
                                            Some(r.order_id)
                                        }
                                        Err(e) => {
                                            tracing::debug!(error = %e, market = %cid, "MM: ask order failed");
                                            None
                                        }
                                    }
                                } else {
                                    None
                                }
                            } else {
                                None
                            };

                            outstanding_orders.insert(cid, (bid_order_id, ask_order_id));
                        }

                        let active = outstanding_orders.values()
                            .filter(|(b, a)| b.is_some() || a.is_some())
                            .count();
                        tracing::debug!(
                            active_markets = active,
                            "MM: quote refresh complete"
                        );
                    }
                }
            }
        });
        tracing::info!(
            max_markets = settings.market_making.max_markets,
            spread_bps = settings.market_making.target_spread_bps,
            refresh_secs = settings.market_making.quote_refresh_secs,
            "Market making task started"
        );
    } else if settings.market_making.enabled && !trading_enabled {
        tracing::warn!("Market making enabled but CLOB auth failed — MM disabled");
    }

    // --- Auto-redeem resolved positions (every 5 minutes) ---
    if trading_enabled {
        let redeem_loader = PositionLoader::new(proxy_addr)?;
        let redeem_cancel = cancel.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(300));
            interval.tick().await; // skip immediate tick
            loop {
                tokio::select! {
                    _ = redeem_cancel.cancelled() => break,
                    _ = interval.tick() => {
                        match redeem_loader.find_redeemable().await {
                            Ok(positions) if positions.is_empty() => {}
                            Ok(positions) => {
                                tracing::info!(
                                    count = positions.len(),
                                    "Found redeemable positions, claiming..."
                                );
                                for pos in &positions {
                                    tracing::info!(
                                        condition_id = %pos.condition_id,
                                        title = %pos.title,
                                        size = %pos.size,
                                        neg_risk = pos.neg_risk,
                                        "Redeeming resolved position"
                                    );
                                    let result = redeem_ctf.redeem(pos.condition_id).await;
                                    match result {
                                        Ok(tx) => {
                                            tracing::info!(
                                                condition_id = %pos.condition_id,
                                                tx_hash = %tx.tx_hash,
                                                "Redeem successful"
                                            );
                                        }
                                        Err(e) => {
                                            tracing::warn!(
                                                condition_id = %pos.condition_id,
                                                error = %e,
                                                "Redeem failed"
                                            );
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                tracing::debug!(error = %e, "Redeemable check failed");
                            }
                        }
                    }
                }
            }
        });
        tracing::info!("Auto-redeem task started (checks every 5 min)");
    }

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
            strategy_type: opportunity.strategy_type,
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

/// Infer strategy_type for a loaded position by matching its token_id against discovered markets.
///
/// Checks weather → crypto → convergence in order. Returns None if no match found.
fn infer_strategy_type(
    token_id: alloy::primitives::U256,
    markets: &[pa_core::types::MarketInfo],
    neg_risk_events: &[pa_core::types::NegRiskEvent],
) -> Option<pa_core::types::StrategyType> {
    use pa_core::types::StrategyType;

    // Check NegRisk events first (weather and crypto both use them)
    for event in neg_risk_events {
        let has_token = event.markets.iter().any(|m| {
            m.tokens.iter().any(|t| t.token_id == token_id)
        });
        if has_token {
            if pa_strategy::weather::parse_weather_event_title(&event.title).is_some() {
                return Some(StrategyType::Weather);
            }
            if pa_strategy::crypto_alpha::parse_crypto_event_title(&event.title).is_some() {
                return Some(StrategyType::CryptoAlpha);
            }
        }
    }

    // Check individual markets
    for market in markets {
        let has_token = market.tokens.iter().any(|t| t.token_id == token_id);
        if !has_token {
            continue;
        }

        // Weather binary market
        if pa_strategy::weather::parse_weather_question(&market.question).is_some() {
            return Some(StrategyType::Weather);
        }

        // Crypto binary market
        if pa_strategy::crypto_alpha::parse_crypto_question(&market.question).is_some() {
            return Some(StrategyType::CryptoAlpha);
        }

        // Convergence: non-NegRisk market with end_date
        if !market.neg_risk && market.end_date.is_some() {
            return Some(StrategyType::ResolutionConvergence);
        }
    }

    None
}