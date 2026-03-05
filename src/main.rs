use std::collections::HashSet;
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
use pa_execution::safe_redeemer::SafeRedeemer;
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

/// Metadata for a tracked LR order, used by fill detection to sync positions.
#[derive(Clone, Debug)]
struct LrOrderMeta {
    token_id: alloy::primitives::U256,
    is_buy: bool,
    price: Decimal,
    size: Decimal,
    /// Cumulative size_matched already synced to RiskManager.
    /// Used to compute delta on partial fills: new_fill = api.size_matched - last_synced.
    last_synced_matched: Decimal,
}

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
    let market_data = Arc::new(
        MarketDataService::new(&settings)
            .context("Failed to initialize market data service")?
    );
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

    // Wrap markets in shared state for periodic refresh
    let shared_markets = Arc::new(tokio::sync::RwLock::new(markets));

    // --- Seed OrderBookCache with gamma best_bid/best_ask ---
    // Directional strategies (crypto, weather, convergence) need order book prices to
    // detect edges. The WS subscription can only hold 500 instruments (~250 markets),
    // but we discover 500+ markets. By seeding the cache with gamma API best_bid/best_ask,
    // ALL markets have baseline price data for strategy evaluation.
    // WS updates will overwrite these with real-time data for subscribed markets.
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

        tracing::info!(
            seeded,
            no_price_data,
            "OrderBookCache seeded with gamma prices"
        );
    }

    // --- Compute proxy wallet address (used by position loader and Safe redeemer) ---
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

    // --- Load held position token IDs (needed for WS subscription priority) ---
    let held_position_token_ids: Vec<alloy::primitives::U256> = {
        let loader = PositionLoader::new(proxy_addr)?;
        match loader.load_positions().await {
            Ok(positions) => positions.iter().map(|p| p.token_id).collect(),
            Err(e) => {
                tracing::warn!(error = %e, "Could not pre-load position tokens for WS priority");
                Vec::new()
            }
        }
    };

    // --- Subscribe to WebSocket order book updates ---
    // Smart ordering: filter extreme prices for ALL markets, then prioritize by strategy relevance + mid-ness.
    let ws_max = settings.market_filter.ws_max_instruments;
    {
        let markets_snapshot = shared_markets.read().await;
        let token_ids = build_ws_token_list(
            &markets_snapshot,
            &held_position_token_ids,
            &settings.strategy.enabled,
            ws_max,
        );

        tracing::info!(
            tokens = token_ids.len(),
            "Subscribing to order book updates (smart ordering)"
        );
        market_data.subscribe(&token_ids).await?;
        pa_monitor::metrics::ACTIVE_SUBSCRIPTIONS.set(token_ids.len() as f64);
    }

    // Get broadcast receiver for strategy engine
    let update_rx = market_data.ws_feed().await.subscribe_updates();

    // --- Market spread observer (observation mode) ---
    // Every 60s, scans all binary markets and logs spread distribution.
    // Helps evaluate arbitrage opportunity density without executing trades.
    let observer_cache = market_data.cache().clone();
    let observer_markets = shared_markets.read().await.clone();
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

                    tracing::debug!(
                        total_markets,
                        neg_risk = neg_risk_count,
                        binary_checked = total_checked,
                        with_orderbook = has_book,
                        missing_asks = no_asks,
                        missing_bids = no_bids,
                        cache_size = observer_cache.len(),
                        "━━━ Market Spread Observer ━━━"
                    );
                    tracing::debug!(
                        any_spread = bm_any_spread,
                        pass_bps = bm_above_min_bps,
                        pass_profit = bm_above_min_profit,
                        best_spread_bps = %format!("{:.0}", bm_best_spread * dec!(10000)),
                        tightest_gap_bps = %format!("{:.0}", bm_tightest_gap * dec!(10000)),
                        "[BuyAndMerge]"
                    );
                    if !bm_best_question.is_empty() {
                        tracing::debug!(market = %bm_best_question, "[BuyAndMerge] best");
                    }
                    if !bm_tightest_info.is_empty() {
                        tracing::debug!(market = %bm_tightest_info, "[BuyAndMerge] tightest");
                    }
                    tracing::debug!(
                        any_spread = ss_any_spread,
                        pass_bps = ss_above_min_bps,
                        pass_profit = ss_above_min_profit,
                        best_spread_bps = %format!("{:.0}", ss_best_spread * dec!(10000)),
                        tightest_gap_bps = %format!("{:.0}", ss_tightest_gap * dec!(10000)),
                        "[SplitAndSell]"
                    );
                    if !ss_best_question.is_empty() {
                        tracing::debug!(market = %ss_best_question, "[SplitAndSell] best");
                    }
                    if !ss_tightest_info.is_empty() {
                        tracing::debug!(market = %ss_tightest_info, "[SplitAndSell] tightest");
                    }
                    // Log sample prices for data validation
                    for (i, sample) in sample_prices.iter().enumerate() {
                        tracing::debug!(idx = i, sample = %sample, "[Sample]");
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
    let position_loader = PositionLoader::new(proxy_addr)?;
    let api_positions = position_loader.load_positions().await
        .context("Failed to load positions from Data API")?;

    // Ensure held position markets are in the shared markets list,
    // even if they're expired/closed (needed for exit scanning).
    if !api_positions.is_empty() {
        let markets_snapshot = shared_markets.read().await;
        let known_condition_ids: HashSet<_> = markets_snapshot
            .iter()
            .map(|m| m.condition_id)
            .collect();
        let missing_condition_ids: Vec<_> = api_positions
            .iter()
            .map(|p| p.condition_id)
            .filter(|cid| !known_condition_ids.contains(cid))
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        drop(markets_snapshot);

        if !missing_condition_ids.is_empty() {
            tracing::info!(
                missing = missing_condition_ids.len(),
                "Fetching market data for held positions not in discovery (expired/closed)"
            );
            let position_markets = market_data
                .fetch_position_markets(&missing_condition_ids)
                .await;
            if !position_markets.is_empty() {
                // Seed the order book cache with gamma prices for these markets
                let seed_cache = market_data.cache();
                let mut seeded = 0;
                for m in &position_markets {
                    if seed_market_cache(seed_cache, m) {
                        seeded += 1;
                    }
                }
                tracing::info!(
                    fetched = position_markets.len(),
                    seeded,
                    "Position markets seeded into order book cache"
                );
                // Add to shared markets
                let mut markets_write = shared_markets.write().await;
                markets_write.extend(position_markets);
                tracing::info!(
                    total_markets = markets_write.len(),
                    "Position markets injected into shared market list"
                );
            }
        }
    }

    let markets_snapshot = shared_markets.read().await;
    let initial_positions: Vec<_> = api_positions
        .iter()
        .map(|p| {
            // Infer strategy_type from market data so exit logic works after restart
            let strategy_type = infer_strategy_type(p.token_id, &markets_snapshot, &neg_risk_events);
            if strategy_type.is_some() {
                tracing::debug!(
                    token_id = %p.token_id,
                    strategy = ?strategy_type,
                    size = %p.size,
                    "Position tagged with inferred strategy"
                );
            } else {
                // Find the market question for diagnostics
                let question = markets_snapshot.iter()
                    .find(|m| m.tokens.iter().any(|t| t.token_id == p.token_id))
                    .map(|m| m.question.as_str())
                    .unwrap_or("<market not found in discovery>");
                tracing::warn!(
                    token_id = %p.token_id,
                    size = %p.size,
                    avg_cost = %p.avg_price,
                    question = %question,
                    "UNTAGGED position — strategy inference failed, strategy exits won't work (stop-loss safety net active)"
                );
            }
            (p.token_id, p.size, p.avg_price, strategy_type, Some(p.condition_id))
        })
        .collect();
    drop(markets_snapshot);
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

    // Shared available capital closure: returns current USDC balance.
    // The balance already reflects money spent on existing positions —
    // subtracting exposure would double-count (the cash is already gone).
    // Per-strategy and per-market limits are enforced by RiskManager::check_pre_trade().
    let make_capital_fn = |bal: Arc<std::sync::RwLock<Decimal>>|
     -> Box<dyn Fn() -> Decimal + Send + Sync> {
        Box::new(move || {
            let balance = *bal.read().unwrap();
            balance.max(Decimal::ZERO)
        })
    };

    // Shared balance closure: returns current wallet USDC balance.
    // Used by strategies for dynamic position sizing (max_position_pct * balance).
    let make_balance_fn = |bal: Arc<std::sync::RwLock<Decimal>>|
     -> Box<dyn Fn() -> Decimal + Send + Sync> {
        Box::new(move || *bal.read().unwrap())
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
            make_capital_fn(Arc::clone(&usdc_balance)),
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
            make_capital_fn(Arc::clone(&usdc_balance)),
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
            make_capital_fn(Arc::clone(&usdc_balance)),
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
            make_capital_fn(Arc::clone(&usdc_balance)),
            Box::new(move |tid: alloy::primitives::U256| rm_pos.get_position_size(&tid)),
            neg_risk_events.clone(),
            Box::new(move || rm_held.positions_by_strategy(pa_core::types::StrategyType::Weather)),
            make_balance_fn(Arc::clone(&usdc_balance)),
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
            make_capital_fn(Arc::clone(&usdc_balance)),
            Box::new(move |tid: alloy::primitives::U256| rm_pos_conv.get_position_size(&tid)),
            Box::new(move || rm_held_conv.positions_by_strategy(pa_core::types::StrategyType::ResolutionConvergence)),
            make_balance_fn(Arc::clone(&usdc_balance)),
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
            make_capital_fn(Arc::clone(&usdc_balance)),
            Box::new(move |tid: alloy::primitives::U256| rm_pos_crypto.get_position_size(&tid)),
            neg_risk_events.clone(),
            binary_event_groups.clone(),
            Box::new(move || rm_held_crypto.positions_by_strategy(pa_core::types::StrategyType::CryptoAlpha)),
            make_balance_fn(Arc::clone(&usdc_balance)),
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
    let engine_rm_all = Arc::clone(&risk_manager_impl);
    let engine = StrategyEngine::new(
        strategies,
        executor.clone(),
        risk_manager.clone(),
        settings.strategy.scan_interval_ms,
        event_calendar,
        Box::new(move |token_id| engine_cache.get(&token_id)),
        make_capital_fn(Arc::clone(&usdc_balance)),
        Box::new(move || {
            engine_rm_all.snapshot_positions()
                .into_iter()
                .map(|(token_id, entry)| pa_strategy::engine::StopLossPosition {
                    token_id,
                    size: entry.size,
                    avg_cost: entry.avg_cost,
                    strategy_type: entry.strategy_type,
                    condition_id: entry.condition_id,
                })
                .collect()
        }),
        settings.risk.min_order_usdc,
        settings.strategy.max_market_end_days,
    );

    tracing::info!(
        trading = trading_enabled,
        "PolyAlpha initialized — entering trading loop"
    );

    // --- Run trading loop with graceful shutdown ---
    let engine_shared = Arc::clone(&shared_markets);
    let engine_cancel = cancel.clone();
    let engine_handle = tokio::spawn(async move {
        engine.run(engine_shared, update_rx, engine_cancel).await;
    });

    // --- Liquidity Rewards background task ---
    if settings.liquidity_rewards.enabled && trading_enabled {
        let lr_config = settings.liquidity_rewards.clone();
        let lr_cache = market_data.cache().clone();
        let lr_cancel = cancel.clone();
        let lr_rm = Arc::clone(&risk_manager_impl);
        let lr_shared = Arc::clone(&shared_markets);
        let lr_private_key = private_key.clone();
        let lr_clob_host = settings.clob.host.clone();
        let lr_sig_type = settings.clob.signature_type;
        let lr_chain_id = settings.chain.chain_id;
        // Subscribe to WS order book updates for re-quote trigger (before spawn)
        let lr_update_rx = market_data.ws_feed().await.subscribe_updates();

        tokio::spawn(async move {
            let lr_signer = match PrivateKeySigner::from_str(&lr_private_key) {
                Ok(s) => s.with_chain_id(Some(lr_chain_id)),
                Err(e) => {
                    tracing::error!(error = %e, "LR: failed to parse signer");
                    return;
                }
            };
            let lr_clob = match ClobExecutor::connect(&lr_clob_host, lr_signer, lr_sig_type).await {
                Ok(c) => c,
                Err(e) => {
                    tracing::error!(error = %e, "LR: CLOB authentication failed, liquidity rewards disabled");
                    return;
                }
            };
            tracing::info!("LR: CLOB authenticated, starting liquidity rewards");

            // Track all outstanding order IDs + metadata per market condition_id
            let mut outstanding_orders: std::collections::HashMap<
                alloy::primitives::B256,
                std::collections::HashMap<String, LrOrderMeta>,
            > = std::collections::HashMap::new();

            // Drift tracking: last quoted midpoint per token + token→condition mapping
            let mut last_quoted_mid: std::collections::HashMap<alloy::primitives::U256, Decimal> = std::collections::HashMap::new();
            let mut token_to_condition: std::collections::HashMap<alloy::primitives::U256, alloy::primitives::B256> = std::collections::HashMap::new();

            // WS re-quote: per-market cooldown to prevent rate limiting
            let mut last_quote_time: std::collections::HashMap<alloy::primitives::B256, std::time::Instant> = std::collections::HashMap::new();
            // Cached candidates + index for fast condition_id lookup
            let markets_init = lr_shared.read().await;
            let mut active_candidates = pa_strategy::liquidity_rewards::select_reward_markets(
                &markets_init, &lr_config,
            );
            drop(markets_init);
            let mut cid_to_candidate_idx: std::collections::HashMap<alloy::primitives::B256, usize> = std::collections::HashMap::new();

            let mut lr_update_rx = lr_update_rx;

            let mut fallback_interval = tokio::time::interval(Duration::from_secs(lr_config.quote_refresh_secs));
            let mut market_interval = tokio::time::interval(Duration::from_secs(lr_config.market_refresh_secs));
            let requote_cooldown = Duration::from_secs(lr_config.requote_cooldown_secs);
            let fill_check_enabled = lr_config.fill_check_secs > 0;
            let mut fill_check_interval = tokio::time::interval(Duration::from_secs(
                if fill_check_enabled { lr_config.fill_check_secs } else { 86400 },
            ));

            // --- Initial mapping + quoting for pre-selected candidates ---
            {
                // Build token→condition and cid→candidate index
                token_to_condition.clear();
                cid_to_candidate_idx.clear();
                for (idx, c) in active_candidates.iter().enumerate() {
                    let cid = c.market.condition_id;
                    cid_to_candidate_idx.insert(cid, idx);
                    if c.market.tokens.len() >= 2 {
                        token_to_condition.insert(c.market.tokens[0].token_id, cid);
                        token_to_condition.insert(c.market.tokens[1].token_id, cid);
                    }
                }

                pa_monitor::metrics::LR_ACTIVE_MARKETS.set(active_candidates.len() as f64);

                // Initial full quote
                let mut total_exposure = Decimal::ZERO;
                for candidate in &active_candidates {
                    let (metas, exp, yes_mid, no_mid) = lr_quote_one_market(
                        &candidate.market, &lr_config, &lr_cache, &lr_rm, &lr_clob, total_exposure,
                    ).await;
                    total_exposure += exp;
                    let cid = candidate.market.condition_id;
                    if !metas.is_empty() {
                        outstanding_orders.insert(cid, metas.into_iter().collect());
                    }
                    // Track mids
                    if candidate.market.tokens.len() >= 2 {
                        if let Some(m) = yes_mid { last_quoted_mid.insert(candidate.market.tokens[0].token_id, m); }
                        if let Some(m) = no_mid { last_quoted_mid.insert(candidate.market.tokens[1].token_id, m); }
                    }
                    let now = std::time::Instant::now();
                    last_quote_time.insert(cid, now);
                }

                tracing::info!(
                    active_markets = outstanding_orders.len(),
                    total_candidates = active_candidates.len(),
                    "LR: initial market selection and quote complete"
                );
            }

            loop {
                tokio::select! {
                    _ = lr_cancel.cancelled() => {
                        // Shutdown: cancel all outstanding orders
                        let all_ids: Vec<String> = outstanding_orders.values()
                            .flat_map(|m| m.keys().cloned())
                            .collect();
                        if !all_ids.is_empty() {
                            let refs: Vec<&str> = all_ids.iter().map(|s| s.as_str()).collect();
                            if let Err(e) = lr_clob.cancel_orders(&refs).await {
                                tracing::warn!(error = %e, "LR: failed to cancel orders on shutdown");
                            }
                            pa_monitor::metrics::LR_ORDERS_CANCELLED.inc_by(all_ids.len() as u64);
                        }
                        tracing::info!("LR: shutdown, cancelled {} orders", all_ids.len());
                        break;
                    }
                    // WS-driven re-quote: cancel + re-quote on significant drift
                    result = lr_update_rx.recv() => {
                        if let Ok(update) = result {
                            let tid = update.token_id;
                            let Some(&cid) = token_to_condition.get(&tid) else { continue };
                            let Some(new_mid) = lr_cache.get(&tid).and_then(|b| b.midpoint()) else { continue };
                            let Some(&old_mid) = last_quoted_mid.get(&tid) else { continue };
                            if old_mid <= Decimal::ZERO { continue; }

                            let drift_bps = ((new_mid - old_mid).abs() / old_mid * dec!(10000)).to_u32().unwrap_or(0);
                            if drift_bps < lr_config.requote_trigger_bps { continue; }

                            // Check per-market cooldown
                            let now = std::time::Instant::now();
                            if let Some(&last_t) = last_quote_time.get(&cid) {
                                if now.duration_since(last_t) < requote_cooldown { continue; }
                            }

                            tracing::info!(
                                token = %tid, market = %cid,
                                old_mid = %old_mid, new_mid = %new_mid,
                                drift_bps = drift_bps,
                                "LR: WS re-quote triggered"
                            );

                            // Cancel outstanding orders for this market
                            if let Some(order_map) = outstanding_orders.remove(&cid) {
                                if !order_map.is_empty() {
                                    let id_strs: Vec<String> = order_map.into_keys().collect();
                                    let refs: Vec<&str> = id_strs.iter().map(|s| s.as_str()).collect();
                                    if let Err(e) = lr_clob.cancel_orders(&refs).await {
                                        tracing::warn!(error = %e, "LR: WS re-quote cancel failed");
                                    }
                                    pa_monitor::metrics::LR_ORDERS_CANCELLED.inc_by(id_strs.len() as u64);
                                }
                            }

                            // Clear tracked mids for this market's tokens
                            last_quoted_mid.remove(&tid);

                            // Re-quote this single market
                            if let Some(&idx) = cid_to_candidate_idx.get(&cid) {
                                if let Some(candidate) = active_candidates.get(idx) {
                                    // Compute current total exposure (approximation: sum of outstanding)
                                    let current_exposure = Decimal::ZERO; // conservative: allow full budget for re-quote
                                    let (metas, _exp, yes_mid, no_mid) = lr_quote_one_market(
                                        &candidate.market, &lr_config, &lr_cache, &lr_rm, &lr_clob, current_exposure,
                                    ).await;
                                    if !metas.is_empty() {
                                        outstanding_orders.insert(cid, metas.into_iter().collect());
                                    }
                                    // Update tracked mids
                                    if candidate.market.tokens.len() >= 2 {
                                        if let Some(m) = yes_mid { last_quoted_mid.insert(candidate.market.tokens[0].token_id, m); }
                                        if let Some(m) = no_mid { last_quoted_mid.insert(candidate.market.tokens[1].token_id, m); }
                                    }
                                    last_quote_time.insert(cid, now);
                                }
                            }
                        }
                    }
                    // Fallback timer: full cancel + re-quote all candidates
                    _ = fallback_interval.tick() => {
                        // Cancel all previous orders
                        let prev_ids: Vec<String> = outstanding_orders.drain()
                            .flat_map(|(_, m)| m.into_keys())
                            .collect();
                        if !prev_ids.is_empty() {
                            let refs: Vec<&str> = prev_ids.iter().map(|s| s.as_str()).collect();
                            if let Err(e) = lr_clob.cancel_orders(&refs).await {
                                tracing::warn!(error = %e, "LR: batch cancel failed");
                            }
                            pa_monitor::metrics::LR_ORDERS_CANCELLED.inc_by(prev_ids.len() as u64);
                        }

                        // Clear drift tracking (will be rebuilt)
                        last_quoted_mid.clear();

                        if active_candidates.is_empty() {
                            tracing::debug!("LR: no eligible reward markets");
                            continue;
                        }

                        let mut total_exposure = Decimal::ZERO;

                        for candidate in &active_candidates {
                            let cid = candidate.market.condition_id;
                            let (metas, exp, yes_mid, no_mid) = lr_quote_one_market(
                                &candidate.market, &lr_config, &lr_cache, &lr_rm, &lr_clob, total_exposure,
                            ).await;
                            total_exposure += exp;
                            if !metas.is_empty() {
                                outstanding_orders.insert(cid, metas.into_iter().collect());
                            }
                            if candidate.market.tokens.len() >= 2 {
                                if let Some(m) = yes_mid { last_quoted_mid.insert(candidate.market.tokens[0].token_id, m); }
                                if let Some(m) = no_mid { last_quoted_mid.insert(candidate.market.tokens[1].token_id, m); }
                            }
                            last_quote_time.insert(cid, std::time::Instant::now());
                        }

                        tracing::debug!(
                            active_markets = outstanding_orders.len(),
                            total_candidates = active_candidates.len(),
                            "LR: fallback quote refresh complete"
                        );
                    }
                    // Market re-selection timer: re-discover and re-rank reward markets
                    _ = market_interval.tick() => {
                        // Cancel all existing orders first
                        let prev_ids: Vec<String> = outstanding_orders.drain()
                            .flat_map(|(_, m)| m.into_keys())
                            .collect();
                        if !prev_ids.is_empty() {
                            let refs: Vec<&str> = prev_ids.iter().map(|s| s.as_str()).collect();
                            if let Err(e) = lr_clob.cancel_orders(&refs).await {
                                tracing::warn!(error = %e, "LR: market refresh cancel failed");
                            }
                            pa_monitor::metrics::LR_ORDERS_CANCELLED.inc_by(prev_ids.len() as u64);
                        }

                        // Re-select markets
                        let markets_snapshot = lr_shared.read().await;
                        active_candidates = pa_strategy::liquidity_rewards::select_reward_markets(
                            &markets_snapshot, &lr_config,
                        );
                        drop(markets_snapshot);

                        // Rebuild token→condition and cid→candidate index
                        token_to_condition.clear();
                        cid_to_candidate_idx.clear();
                        last_quoted_mid.clear();
                        last_quote_time.clear();
                        for (idx, c) in active_candidates.iter().enumerate() {
                            let cid = c.market.condition_id;
                            cid_to_candidate_idx.insert(cid, idx);
                            if c.market.tokens.len() >= 2 {
                                token_to_condition.insert(c.market.tokens[0].token_id, cid);
                                token_to_condition.insert(c.market.tokens[1].token_id, cid);
                            }
                        }

                        pa_monitor::metrics::LR_ACTIVE_MARKETS.set(active_candidates.len() as f64);

                        // Full re-quote
                        let mut total_exposure = Decimal::ZERO;
                        for candidate in &active_candidates {
                            let cid = candidate.market.condition_id;
                            let (metas, exp, yes_mid, no_mid) = lr_quote_one_market(
                                &candidate.market, &lr_config, &lr_cache, &lr_rm, &lr_clob, total_exposure,
                            ).await;
                            total_exposure += exp;
                            if !metas.is_empty() {
                                outstanding_orders.insert(cid, metas.into_iter().collect());
                            }
                            if candidate.market.tokens.len() >= 2 {
                                if let Some(m) = yes_mid { last_quoted_mid.insert(candidate.market.tokens[0].token_id, m); }
                                if let Some(m) = no_mid { last_quoted_mid.insert(candidate.market.tokens[1].token_id, m); }
                            }
                            last_quote_time.insert(cid, std::time::Instant::now());
                        }

                        tracing::info!(
                            active_markets = outstanding_orders.len(),
                            total_candidates = active_candidates.len(),
                            "LR: market re-selection complete"
                        );
                    }
                    // Fill detection: poll CLOB for order status, sync positions + re-quote on fills
                    _ = fill_check_interval.tick(), if fill_check_enabled => {
                        let cids: Vec<alloy::primitives::B256> = outstanding_orders.keys().cloned().collect();
                        for cid in cids {
                            let api_orders = match lr_clob.get_orders_by_market(cid).await {
                                Ok(o) => o,
                                Err(e) => {
                                    tracing::debug!(error = %e, market = %cid, "LR: fill check query failed");
                                    continue;
                                }
                            };

                            let tracked = match outstanding_orders.get_mut(&cid) {
                                Some(m) => m,
                                None => continue,
                            };

                            // Detect fills: full match, partial match (new size_matched), or missing from API
                            let mut any_full_fill = false;
                            let mut fully_filled_ids: Vec<String> = Vec::new();

                            for (oid, meta) in tracked.iter_mut() {
                                let api_match = api_orders.iter().find(|o| o.order_id == *oid);

                                let (is_fully_done, api_matched_size) = match api_match {
                                    Some(o) => (o.is_matched, o.size_matched),
                                    None => (true, meta.size), // missing = fully settled
                                };

                                let delta = api_matched_size - meta.last_synced_matched;
                                if delta > Decimal::ZERO {
                                    // New fill volume to sync
                                    let current_pos = lr_rm.get_position_size(&meta.token_id);
                                    let new_pos = if meta.is_buy {
                                        current_pos + delta
                                    } else {
                                        (current_pos - delta).max(Decimal::ZERO)
                                    };
                                    lr_rm.sync_position(
                                        meta.token_id,
                                        new_pos,
                                        meta.price,
                                        Some(pa_core::types::StrategyType::LiquidityRewards),
                                        Some(cid),
                                    );
                                    meta.last_synced_matched = api_matched_size;
                                    pa_monitor::metrics::LR_FILLS_DETECTED.inc();
                                    tracing::info!(
                                        market = %cid, token = %meta.token_id,
                                        side = if meta.is_buy { "buy" } else { "sell" },
                                        delta = %delta, total_matched = %api_matched_size,
                                        new_pos = %new_pos,
                                        fully_done = is_fully_done,
                                        "LR: fill detected, position synced"
                                    );
                                }

                                if is_fully_done {
                                    fully_filled_ids.push(oid.clone());
                                    any_full_fill = true;
                                }
                            }

                            // Remove fully filled orders from tracking
                            for id in &fully_filled_ids {
                                tracked.remove(id);
                            }

                            // If any order fully filled: cancel remaining + re-quote for fresh inventory
                            if any_full_fill {
                                // Cancel remaining live orders for this market
                                let remaining_ids: Vec<String> = tracked.keys().cloned().collect();
                                if !remaining_ids.is_empty() {
                                    let refs: Vec<&str> = remaining_ids.iter().map(|s| s.as_str()).collect();
                                    if let Err(e) = lr_clob.cancel_orders(&refs).await {
                                        tracing::debug!(error = %e, "LR: fill re-quote cancel failed");
                                    }
                                    pa_monitor::metrics::LR_ORDERS_CANCELLED.inc_by(remaining_ids.len() as u64);
                                }
                                outstanding_orders.remove(&cid);

                                // Re-quote with updated inventory
                                if let Some(&idx) = cid_to_candidate_idx.get(&cid) {
                                    if let Some(candidate) = active_candidates.get(idx) {
                                        let current_exposure = Decimal::ZERO;
                                        let (metas, _exp, yes_mid, no_mid) = lr_quote_one_market(
                                            &candidate.market, &lr_config, &lr_cache, &lr_rm, &lr_clob, current_exposure,
                                        ).await;
                                        if !metas.is_empty() {
                                            outstanding_orders.insert(cid, metas.into_iter().collect());
                                        }
                                        if candidate.market.tokens.len() >= 2 {
                                            if let Some(m) = yes_mid { last_quoted_mid.insert(candidate.market.tokens[0].token_id, m); }
                                            if let Some(m) = no_mid { last_quoted_mid.insert(candidate.market.tokens[1].token_id, m); }
                                        }
                                        last_quote_time.insert(cid, std::time::Instant::now());
                                        pa_monitor::metrics::LR_FILL_REQUOTES.inc();
                                    }
                                }
                            }
                        }
                    }
                }
            }
        });
        tracing::info!(
            max_markets = settings.liquidity_rewards.max_markets,
            quote_refresh_secs = settings.liquidity_rewards.quote_refresh_secs,
            "Liquidity rewards task started"
        );
    } else if settings.liquidity_rewards.enabled && !trading_enabled {
        tracing::warn!("Liquidity rewards enabled but CLOB auth failed — LR disabled");
    }

    // --- Periodic market refresh background task ---
    let refresh_interval = settings.market_filter.market_refresh_interval_secs;
    if refresh_interval > 0 {
        let refresh_markets = Arc::clone(&shared_markets);
        let refresh_market_data = Arc::clone(&market_data);
        let refresh_cache = market_data.cache().clone();
        let refresh_cancel = cancel.clone();
        let refresh_enabled_strategies = settings.strategy.enabled.clone();
        let refresh_ws_max = settings.market_filter.ws_max_instruments;
        let refresh_risk_manager = Arc::clone(&risk_manager_impl);

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(refresh_interval));
            interval.tick().await; // skip first immediate tick

            loop {
                tokio::select! {
                    _ = refresh_cancel.cancelled() => break,
                    _ = interval.tick() => {
                        tracing::debug!("Periodic market refresh starting...");
                        match refresh_market_data.discover_markets().await {
                            Ok(new_all) => {
                                let mut current = refresh_markets.write().await;
                                let old_ids: HashSet<alloy::primitives::B256> = current.iter()
                                    .map(|m| m.condition_id).collect();

                                let mut added = 0u32;
                                for m in new_all {
                                    if !old_ids.contains(&m.condition_id) {
                                        seed_market_cache(&refresh_cache, &m);
                                        current.push(m);
                                        added += 1;
                                    }
                                }

                                if added > 0 {
                                    tracing::info!(added, total = current.len(), "New markets discovered");
                                    pa_monitor::metrics::MONITORED_MARKETS.set(current.len() as f64);

                                    // Rebuild WS token list and resubscribe
                                    // Include current held position tokens so exit scanning keeps working
                                    let held_tokens: Vec<alloy::primitives::U256> = refresh_risk_manager
                                        .snapshot_positions()
                                        .iter()
                                        .map(|(tid, _)| *tid)
                                        .collect();
                                    let token_ids = build_ws_token_list(
                                        &current,
                                        &held_tokens,
                                        &refresh_enabled_strategies,
                                        refresh_ws_max,
                                    );
                                    drop(current); // release write lock before WS call
                                    if let Err(e) = refresh_market_data.resubscribe(&token_ids).await {
                                        tracing::warn!(error = %e, "WS resubscribe failed after market refresh");
                                    } else {
                                        pa_monitor::metrics::ACTIVE_SUBSCRIPTIONS.set(token_ids.len() as f64);
                                        tracing::info!(tokens = token_ids.len(), "WS resubscribed after market refresh");
                                    }
                                } else {
                                    tracing::debug!("No new markets found in periodic refresh");
                                }
                            }
                            Err(e) => {
                                tracing::warn!(error = %e, "Periodic market refresh failed");
                            }
                        }
                    }
                }
            }
        });
        tracing::info!(
            interval_secs = refresh_interval,
            "Periodic market refresh task started"
        );
    }

    // --- Auto-redeem resolved positions (every 5 minutes) ---
    // SafeRedeemer routes CTF redeemPositions through GnosisSafe.execTransaction()
    // because tokens live in the Safe proxy wallet, not the EOA.
    let redeem_signer = PrivateKeySigner::from_str(&private_key)
        .context("Invalid private key for redeem signer")?
        .with_chain_id(Some(settings.chain.chain_id));
    let redeem_provider = alloy::providers::ProviderBuilder::new()
        .wallet(redeem_signer.clone())
        .connect(&settings.chain.rpc_url)
        .await
        .context("Failed to connect to RPC for redeem executor")?;
    let safe_redeemer = SafeRedeemer::new(redeem_provider, redeem_signer, proxy_addr);
    // Verify EOA is an owner of the Safe (diagnostic for GS013 errors)
    match safe_redeemer.verify_ownership().await {
        Ok(true) => tracing::info!(safe = %proxy_addr, "SafeRedeemer initialized — EOA is Safe owner"),
        Ok(false) => tracing::error!(safe = %proxy_addr, "SafeRedeemer: EOA is NOT a Safe owner — redeem will fail"),
        Err(e) => tracing::warn!(safe = %proxy_addr, error = %e, "SafeRedeemer: could not verify ownership"),
    }

    if trading_enabled {
        let redeem_loader = PositionLoader::new(proxy_addr)?;
        let redeem_cancel = cancel.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(300));
            // First tick fires immediately — no skip
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
                                        outcome_index = pos.outcome_index,
                                        "Redeeming resolved position via GnosisSafe"
                                    );
                                    let result = if pos.neg_risk {
                                        // NegRisk: convert size to CTF amount (6 decimals)
                                        // Only redeem the outcome we actually hold (other is 0)
                                        let amount_raw = pos.size * rust_decimal::Decimal::from(1_000_000u64);
                                        let amount = alloy::primitives::U256::from(
                                            amount_raw.to_u64().unwrap_or(0)
                                        );
                                        let amounts = if pos.outcome_index == 0 {
                                            vec![amount, alloy::primitives::U256::ZERO]
                                        } else {
                                            vec![alloy::primitives::U256::ZERO, amount]
                                        };
                                        safe_redeemer.redeem_neg_risk(
                                            pos.condition_id,
                                            amounts,
                                        ).await
                                    } else {
                                        safe_redeemer.redeem(pos.condition_id).await
                                    };
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

    // --- Periodic position sync (reconcile with Data API every 5 min) ---
    // Safety net: catches positions missed due to Delayed status, crashes, etc.
    {
        let sync_rm = Arc::clone(&risk_manager_impl);
        let sync_markets = Arc::clone(&shared_markets);
        let sync_neg_risk = neg_risk_events.clone();
        let sync_cancel = cancel.clone();
        let sync_proxy = proxy_addr;
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(300));
            interval.tick().await; // skip immediate tick
            loop {
                tokio::select! {
                    _ = sync_cancel.cancelled() => break,
                    _ = interval.tick() => {
                        let loader = match PositionLoader::new(sync_proxy) {
                            Ok(l) => l,
                            Err(e) => {
                                tracing::warn!(error = %e, "Position sync: failed to create loader");
                                continue;
                            }
                        };
                        let api_positions = match loader.load_positions().await {
                            Ok(p) => p,
                            Err(e) => {
                                tracing::warn!(error = %e, "Position sync: Data API fetch failed");
                                continue;
                            }
                        };

                        // Build set of known positions
                        let current = sync_rm.snapshot_positions();
                        let known: std::collections::HashMap<alloy::primitives::U256, Decimal> =
                            current.iter().map(|(tid, e)| (*tid, e.size)).collect();

                        let markets_snapshot = sync_markets.read().await;
                        let mut added = 0u32;
                        let mut updated = 0u32;
                        let mut retagged = 0u32;
                        for pos in &api_positions {
                            let existing_size = known.get(&pos.token_id).copied().unwrap_or(Decimal::ZERO);

                            // Re-tag positions with missing strategy_type even if size is unchanged.
                            // This fixes positions that failed inference at startup (e.g. market not yet discovered).
                            if existing_size == pos.size && existing_size > Decimal::ZERO {
                                // Check if strategy_type is None — try to re-infer
                                let needs_retag = current.iter().any(|(tid, entry)| {
                                    *tid == pos.token_id && entry.strategy_type.is_none()
                                });
                                if needs_retag {
                                    let strategy_type = infer_strategy_type(
                                        pos.token_id, &markets_snapshot, &sync_neg_risk,
                                    );
                                    if strategy_type.is_some() {
                                        sync_rm.sync_position(
                                            pos.token_id, pos.size, pos.avg_price,
                                            strategy_type, Some(pos.condition_id),
                                        );
                                        tracing::info!(
                                            token_id = %pos.token_id,
                                            strategy = ?strategy_type,
                                            "Position sync: re-tagged untagged position"
                                        );
                                        retagged += 1;
                                    }
                                }
                                continue;
                            }
                            if existing_size == Decimal::ZERO && pos.size > Decimal::ZERO {
                                // New position not tracked by RiskManager
                                let strategy_type = infer_strategy_type(
                                    pos.token_id, &markets_snapshot, &sync_neg_risk,
                                );
                                sync_rm.sync_position(
                                    pos.token_id, pos.size, pos.avg_price,
                                    strategy_type, Some(pos.condition_id),
                                );
                                tracing::info!(
                                    token_id = %pos.token_id,
                                    size = %pos.size,
                                    strategy = ?strategy_type,
                                    "Position sync: discovered missing position"
                                );
                                added += 1;
                            } else if pos.size > Decimal::ZERO && existing_size > Decimal::ZERO && pos.size != existing_size {
                                // Size changed (partial fill, external trade, etc.)
                                let strategy_type = infer_strategy_type(
                                    pos.token_id, &markets_snapshot, &sync_neg_risk,
                                );
                                sync_rm.sync_position(
                                    pos.token_id, pos.size, pos.avg_price,
                                    strategy_type, Some(pos.condition_id),
                                );
                                tracing::info!(
                                    token_id = %pos.token_id,
                                    prev_size = %existing_size,
                                    new_size = %pos.size,
                                    "Position sync: size changed"
                                );
                                updated += 1;
                            } else if pos.size == Decimal::ZERO && existing_size > Decimal::ZERO {
                                // Position closed externally (redeemed, etc.) — zero it out
                                sync_rm.sync_position(
                                    pos.token_id, Decimal::ZERO, Decimal::ZERO,
                                    None, Some(pos.condition_id),
                                );
                                tracing::info!(
                                    token_id = %pos.token_id,
                                    prev_size = %existing_size,
                                    "Position sync: cleared stale position"
                                );
                                updated += 1;
                            }
                        }
                        // Also check for positions in RiskManager but gone from Data API (redeemed)
                        let api_tokens: HashSet<alloy::primitives::U256> =
                            api_positions.iter().map(|p| p.token_id).collect();
                        for (tid, entry) in &current {
                            if entry.size > Decimal::ZERO && !api_tokens.contains(tid) {
                                sync_rm.sync_position(
                                    *tid, Decimal::ZERO, Decimal::ZERO,
                                    None, entry.condition_id,
                                );
                                tracing::info!(
                                    token_id = %tid,
                                    prev_size = %entry.size,
                                    "Position sync: cleared position no longer in Data API"
                                );
                                updated += 1;
                            }
                        }
                        drop(markets_snapshot);

                        if added > 0 || updated > 0 || retagged > 0 {
                            tracing::info!(
                                added, updated, retagged,
                                total_api = api_positions.len(),
                                "Position sync complete — reconciled with Data API"
                            );
                        }
                    }
                }
            }
        });
        tracing::info!("Position sync task started (reconciles every 5 min)");
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

/// Quote a single market for LR. Returns (order_id+meta pairs, exposure_added, yes_mid, no_mid).
async fn lr_quote_one_market(
    market: &pa_core::types::MarketInfo,
    config: &pa_core::config::LiquidityRewardsConfig,
    cache: &pa_market_data::cache::OrderBookCache,
    rm: &RiskManagerImpl,
    clob: &ClobExecutor,
    current_exposure: Decimal,
) -> (Vec<(String, LrOrderMeta)>, Decimal, Option<Decimal>, Option<Decimal>) {
    let cid = market.condition_id;
    let yes_tid = market.tokens[0].token_id;
    let no_tid = market.tokens[1].token_id;
    let rewards_max_spread = market.rewards_max_spread.unwrap_or(Decimal::ZERO);
    let rewards_min_size = market.rewards_min_size.unwrap_or(Decimal::ZERO);

    let mut order_metas: Vec<(String, LrOrderMeta)> = Vec::new();
    let mut exposure_added = Decimal::ZERO;
    let mut yes_mid_out: Option<Decimal> = None;
    let mut no_mid_out: Option<Decimal> = None;

    // Quote YES side
    if config.quote_yes {
        let yes_position = rm.get_position_size(&yes_tid);
        if let Some(yes_book) = cache.get(&yes_tid) {
            if let Some(mid) = yes_book.midpoint() {
                yes_mid_out = Some(mid);

                if let Some(quote) = pa_strategy::liquidity_rewards::compute_quotes(
                    mid, rewards_max_spread, yes_position,
                    config, rewards_min_size, market.tick_size,
                ) {
                    let remaining_pos = (config.max_position_per_market - yes_position).max(Decimal::ZERO);
                    let remaining_exp = (config.max_total_exposure - current_exposure - exposure_added).max(Decimal::ZERO);
                    let bid_size = quote.size.min(remaining_pos).min(remaining_exp);

                    if bid_size >= config.min_order_size {
                        match clob.buy_limit_post_only(yes_tid, quote.bid_price, bid_size).await {
                            Ok(r) if !r.order_id.is_empty() => {
                                pa_monitor::metrics::LR_ORDERS_PLACED.inc();
                                exposure_added += bid_size * quote.bid_price;
                                order_metas.push((r.order_id, LrOrderMeta {
                                    token_id: yes_tid, is_buy: true,
                                    price: quote.bid_price, size: bid_size,
                                    last_synced_matched: Decimal::ZERO,
                                }));
                            }
                            Ok(_) => {}
                            Err(e) => tracing::debug!(error = %e, market = %cid, "LR: YES bid failed"),
                        }
                    }

                    if yes_position > Decimal::ZERO {
                        let sell_size = quote.size.min(yes_position);
                        if sell_size >= config.min_order_size {
                            match clob.sell_limit_post_only(yes_tid, quote.ask_price, sell_size).await {
                                Ok(r) if !r.order_id.is_empty() => {
                                    pa_monitor::metrics::LR_ORDERS_PLACED.inc();
                                    order_metas.push((r.order_id, LrOrderMeta {
                                        token_id: yes_tid, is_buy: false,
                                        price: quote.ask_price, size: sell_size,
                                        last_synced_matched: Decimal::ZERO,
                                    }));
                                }
                                Ok(_) => {}
                                Err(e) => tracing::debug!(error = %e, market = %cid, "LR: YES ask failed"),
                            }
                        }
                    }
                }
            }
        }
    }

    // Quote NO side
    if config.quote_no {
        let no_position = rm.get_position_size(&no_tid);
        if let Some(no_book) = cache.get(&no_tid) {
            if let Some(mid) = no_book.midpoint() {
                no_mid_out = Some(mid);

                if let Some(quote) = pa_strategy::liquidity_rewards::compute_quotes(
                    mid, rewards_max_spread, no_position,
                    config, rewards_min_size, market.tick_size,
                ) {
                    let remaining_pos = (config.max_position_per_market - no_position).max(Decimal::ZERO);
                    let remaining_exp = (config.max_total_exposure - current_exposure - exposure_added).max(Decimal::ZERO);
                    let bid_size = quote.size.min(remaining_pos).min(remaining_exp);

                    if bid_size >= config.min_order_size {
                        match clob.buy_limit_post_only(no_tid, quote.bid_price, bid_size).await {
                            Ok(r) if !r.order_id.is_empty() => {
                                pa_monitor::metrics::LR_ORDERS_PLACED.inc();
                                exposure_added += bid_size * quote.bid_price;
                                order_metas.push((r.order_id, LrOrderMeta {
                                    token_id: no_tid, is_buy: true,
                                    price: quote.bid_price, size: bid_size,
                                    last_synced_matched: Decimal::ZERO,
                                }));
                            }
                            Ok(_) => {}
                            Err(e) => tracing::debug!(error = %e, market = %cid, "LR: NO bid failed"),
                        }
                    }

                    if no_position > Decimal::ZERO {
                        let sell_size = quote.size.min(no_position);
                        if sell_size >= config.min_order_size {
                            match clob.sell_limit_post_only(no_tid, quote.ask_price, sell_size).await {
                                Ok(r) if !r.order_id.is_empty() => {
                                    pa_monitor::metrics::LR_ORDERS_PLACED.inc();
                                    order_metas.push((r.order_id, LrOrderMeta {
                                        token_id: no_tid, is_buy: false,
                                        price: quote.ask_price, size: sell_size,
                                        last_synced_matched: Decimal::ZERO,
                                    }));
                                }
                                Ok(_) => {}
                                Err(e) => tracing::debug!(error = %e, market = %cid, "LR: NO ask failed"),
                            }
                        }
                    }
                }
            }
        }
    }

    (order_metas, exposure_added, yes_mid_out, no_mid_out)
}

/// Seed the OrderBookCache for a single market using its gamma prices.
///
/// Uses `gamma_best_bid/ask` when available, falls back to `outcome_prices`.
/// Returns true if the market was seeded, false otherwise.
fn seed_market_cache(cache: &pa_market_data::cache::OrderBookCache, m: &pa_core::types::MarketInfo) -> bool {
    if m.tokens.len() < 2 {
        return false;
    }

    let (yes_bid, yes_ask) = if let (Some(bid), Some(ask)) = (m.gamma_best_bid, m.gamma_best_ask) {
        if ask > Decimal::ZERO && ask <= Decimal::ONE && bid > Decimal::ZERO {
            (bid, ask)
        } else {
            match m.outcome_prices.as_ref().and_then(|p| p.first().copied()) {
                Some(yp) if yp > Decimal::ZERO && yp < Decimal::ONE => {
                    ((yp - dec!(0.01)).max(dec!(0.01)), yp)
                }
                _ => return false,
            }
        }
    } else {
        match m.outcome_prices.as_ref().and_then(|p| p.first().copied()) {
            Some(yp) if yp > Decimal::ZERO && yp < Decimal::ONE => {
                ((yp - dec!(0.01)).max(dec!(0.01)), yp)
            }
            _ => return false,
        }
    };

    let no_ask = (Decimal::ONE - yes_bid).min(dec!(0.99));
    let no_bid = (Decimal::ONE - yes_ask).max(dec!(0.01));

    cache.update(
        m.tokens[0].token_id,
        pa_core::types::OrderBook {
            token_id: m.tokens[0].token_id,
            bids: vec![pa_core::types::PriceLevel { price: yes_bid, size: dec!(1000) }],
            asks: vec![pa_core::types::PriceLevel { price: yes_ask, size: dec!(1000) }],
            timestamp: Utc::now(),
        },
    );

    cache.update(
        m.tokens[1].token_id,
        pa_core::types::OrderBook {
            token_id: m.tokens[1].token_id,
            bids: vec![pa_core::types::PriceLevel { price: no_bid, size: dec!(1000) }],
            asks: vec![pa_core::types::PriceLevel { price: no_ask, size: dec!(1000) }],
            timestamp: Utc::now(),
        },
    );

    true
}

/// Build the smart-ordered WS token subscription list.
///
/// Priority: held position tokens > strategy-relevant mid-range > general mid-range > NegRisk.
/// Filters out extreme-priced markets (YES < 0.05 or > 0.95).
fn build_ws_token_list(
    markets: &[pa_core::types::MarketInfo],
    held_position_token_ids: &[alloy::primitives::U256],
    enabled_strategies: &[String],
    ws_max: usize,
) -> Vec<alloy::primitives::U256> {
    let mut strategy_mid: Vec<(alloy::primitives::U256, alloy::primitives::U256, f64)> = Vec::new();
    let mut general_mid: Vec<(alloy::primitives::U256, alloy::primitives::U256, f64)> = Vec::new();

    for m in markets {
        if m.neg_risk || m.tokens.len() != 2 || !m.active {
            continue;
        }

        let yes_price = m.gamma_best_ask
            .and_then(|p| p.to_f64())
            .or_else(|| {
                m.outcome_prices.as_ref()
                    .and_then(|p| p.first().copied())
                    .and_then(|p| p.to_f64())
            });

        if let Some(yp) = yes_price {
            if yp < 0.05 || yp > 0.95 {
                continue;
            }
            let dist = (yp - 0.50_f64).abs();
            if GammaFeed::is_relevant_for_strategies(&m.question, enabled_strategies) {
                strategy_mid.push((m.tokens[0].token_id, m.tokens[1].token_id, dist));
            } else {
                general_mid.push((m.tokens[0].token_id, m.tokens[1].token_id, dist));
            }
        } else {
            if GammaFeed::is_relevant_for_strategies(&m.question, enabled_strategies) {
                strategy_mid.push((m.tokens[0].token_id, m.tokens[1].token_id, 1.0));
            } else {
                general_mid.push((m.tokens[0].token_id, m.tokens[1].token_id, 1.0));
            }
        }
    }

    strategy_mid.sort_by(|a, b| a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal));
    general_mid.sort_by(|a, b| a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal));

    let mut token_ids: Vec<alloy::primitives::U256> = Vec::new();

    for tid in held_position_token_ids {
        if !token_ids.contains(tid) {
            token_ids.push(*tid);
        }
    }

    for (yes_tid, no_tid, _) in &strategy_mid {
        if !token_ids.contains(yes_tid) { token_ids.push(*yes_tid); }
        if !token_ids.contains(no_tid) { token_ids.push(*no_tid); }
    }
    for (yes_tid, no_tid, _) in &general_mid {
        if !token_ids.contains(yes_tid) { token_ids.push(*yes_tid); }
        if !token_ids.contains(no_tid) { token_ids.push(*no_tid); }
    }

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

    token_ids.truncate(ws_max);
    token_ids
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

        // Convergence: non-NegRisk binary market with end_date AND
        // outcome price near 0 or 1 (convergence only buys tokens priced > 0.90).
        // Without the price check, ALL markets with end_date get tagged as convergence,
        // preventing proper exit scanning by the real owning strategy.
        if !market.neg_risk && market.end_date.is_some() && market.tokens.len() == 2 {
            let looks_converging = market.outcome_prices.as_ref()
                .and_then(|p| p.first())
                .map(|&yp| yp >= dec!(0.90) || yp <= dec!(0.10))
                .unwrap_or(false);
            if looks_converging {
                return Some(StrategyType::ResolutionConvergence);
            }
        }
    }

    None
}