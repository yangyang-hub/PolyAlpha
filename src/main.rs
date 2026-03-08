use std::collections::HashSet;
use std::str::FromStr;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use alloy::signers::Signer as _;
use alloy::signers::local::PrivateKeySigner;
use anyhow::{Context, Result};
use arc_swap::ArcSwap;
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
use pa_monitor::api::ApiState;
use pa_risk::manager::RiskManagerImpl;
use pa_storage::config_store::ConfigStore;
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

/// Per-account runtime context bundling all account-specific resources.
struct AccountContext {
    name: String,
    trading_enabled: bool,
    executor: Arc<dyn pa_core::traits::Executor>,
    risk_manager_impl: Arc<RiskManagerImpl>,
    risk_manager: Arc<dyn pa_core::traits::RiskManager>,
    usdc_balance: Arc<std::sync::RwLock<Decimal>>,
    proxy_addr: alloy::primitives::Address,
    private_key: String,
    signature_type: u8,
    chain_id: u64,
    /// Strategies assigned to this account (e.g., ["weather", "crypto", "liquidity_rewards"]).
    strategies: Vec<String>,
}

/// Fetch current liquidity rewards from the CLOB API.
///
/// Returns a list of markets with active rewards, including their reward parameters
/// (max_spread, min_size, total_daily_rate).
async fn fetch_clob_rewards(
    clob: &ClobExecutor,
) -> anyhow::Result<Vec<pa_strategy::liquidity_rewards::ClobRewardData>> {
    let mut all_rewards = Vec::new();
    let mut next_cursor: Option<String> = None;  // API expects None for first page

    tracing::debug!("Fetching CLOB rewards with next_cursor={:?}", next_cursor);

    // Fetch all pages of rewards
    loop {
        let page = match clob.current_rewards(next_cursor.clone()).await {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    next_cursor = ?next_cursor,
                    "LR: Failed to fetch CLOB rewards, API may have changed"
                );
                return Ok(all_rewards); // Return what we have so far
            }
        };
        
        for reward in page.data {
            // Sum up daily rates from all reward configs
            let total_daily_rate: Decimal = reward.rewards_config
                .iter()
                .map(|r| r.rate_per_day)
                .sum();
            
            all_rewards.push(pa_strategy::liquidity_rewards::ClobRewardData {
                condition_id: reward.condition_id,
                // CLOB API returns spread as percentage (e.g. 4.5 = 4.5%),
                // convert to decimal price spread (0.045) for our quoting math.
                rewards_max_spread: reward.rewards_max_spread / Decimal::from(100),
                rewards_min_size: reward.rewards_min_size,
                total_daily_rate,
            });
        }
        
        // Check if there's a next page (LTE= is the termination marker from API)
        if page.next_cursor.is_empty() || page.next_cursor == "LTE=" {
            break;
        }
        next_cursor = Some(page.next_cursor.clone());
    }
    
    tracing::info!(
        count = all_rewards.len(),
        "LR: Fetched CLOB rewards markets from API"
    );
    
    Ok(all_rewards)
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
    let mut settings = Settings::load().context("Failed to load configuration")?;

    // --- Connect to PostgreSQL for config store (optional) ---
    let config_store = if !settings.database.url.is_empty() {
        match sqlx::PgPool::connect(&settings.database.url).await {
            Ok(pool) => {
                // Run migrations
                if let Err(e) = sqlx::migrate!("./migrations").run(&pool).await {
                    tracing::warn!(error = %e, "DB migration failed (non-fatal for config store)");
                }
                let store = ConfigStore::new(pool);
                // Apply DB config overrides
                match store.load_all().await {
                    Ok(overrides) if !overrides.is_empty() => {
                        if let Err(e) = ConfigStore::apply_overrides(&mut settings, &overrides) {
                            tracing::warn!(error = %e, "Failed to apply DB config overrides");
                        } else {
                            tracing::info!(
                                sections = overrides.len(),
                                "Applied DB config overrides"
                            );
                        }
                    }
                    Ok(_) => {}
                    Err(e) => {
                        tracing::warn!(error = %e, "Failed to load DB config overrides");
                    }
                }
                Some(store)
            }
            Err(e) => {
                tracing::warn!(error = %e, "PostgreSQL connection failed — config store disabled");
                None
            }
        }
    } else {
        tracing::info!("No database URL configured — config store disabled");
        None
    };

    // --- ArcSwap for hot-reloadable config ---
    let config_arc = Arc::new(ArcSwap::new(Arc::new(settings.clone())));
    let (config_tx, _config_rx) = tokio::sync::watch::channel(0u64);

    tracing::info!(
        chain_id = settings.chain.chain_id,
        clob_host = %settings.clob.host,
        "Configuration loaded"
    );

    // --- Resolve trading accounts ---
    let resolved_accounts = settings.resolved_accounts();
    tracing::info!(
        count = resolved_accounts.len(),
        names = ?resolved_accounts.iter().map(|a| &a.name).collect::<Vec<_>>(),
        "Trading accounts resolved"
    );

    // Merge per-account strategies into global enabled list so market discovery
    // covers all required markets (e.g., LR needs general markets, not just weather/crypto).
    for acct in &resolved_accounts {
        for s in &acct.strategies {
            if !settings.strategy.enabled.contains(s) {
                settings.strategy.enabled.push(s.clone());
            }
        }
    }

    // --- Global cancellation token ---
    let cancel = CancellationToken::new();

    // --- Initialize market data ---
    let market_data = Arc::new(
        MarketDataService::new(&settings)
            .context("Failed to initialize market data service")?
    );
    tracing::info!("Market data service initialized");

    // --- Start health/metrics/config API server ---
    let ws_connected = market_data.ws_feed_ws_connected().await;
    let api_state = Arc::new(ApiState {
        config: Arc::clone(&config_arc),
        config_store: config_store.unwrap_or_else(|| {
            // Create a dummy config store with a placeholder pool
            // The API will return errors if no DB is configured
            ConfigStore::new(sqlx::PgPool::connect_lazy("postgres://localhost/dummy").unwrap())
        }),
        config_tx,
        start_time: Utc::now(),
        health_checks: vec![
            (
                "websocket",
                Box::new({
                    let ws = Arc::clone(&ws_connected);
                    move || ws.load(Ordering::Relaxed)
                }),
            ),
        ],
    });
    let health_port = settings.monitor.health_port;
    tokio::spawn(async move {
        if let Err(e) = pa_monitor::api::start_server(health_port, api_state).await {
            tracing::error!(error = %e, "API server failed");
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

    // --- Load held position token IDs from all accounts (needed for WS subscription priority) ---
    let held_position_token_ids: Vec<alloy::primitives::U256> = {
        let mut all_tokens: Vec<alloy::primitives::U256> = Vec::new();
        for acct in &resolved_accounts {
            let proxy = if acct.proxy_wallet.is_empty() {
                // Load signer to get EOA address
                let pk = match std::env::var(&acct.private_key_env) {
                    Ok(k) => k,
                    Err(_) => continue,
                };
                let s = match PrivateKeySigner::from_str(&pk) {
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

    // --- Build per-account contexts ---
    let mut account_contexts: Vec<AccountContext> = Vec::new();

    for acct_config in &resolved_accounts {
        let private_key = match std::env::var(&acct_config.private_key_env) {
            Ok(k) => k,
            Err(_) => {
                tracing::error!(
                    account = %acct_config.name,
                    env_var = %acct_config.private_key_env,
                    "Private key env var not set — skipping account"
                );
                continue;
            }
        };
        let signer = match PrivateKeySigner::from_str(&private_key) {
            Ok(s) => s.with_chain_id(Some(settings.chain.chain_id)),
            Err(e) => {
                tracing::error!(account = %acct_config.name, error = %e, "Invalid private key — skipping account");
                continue;
            }
        };
        let wallet_address = signer.address();

        let proxy_addr = if acct_config.proxy_wallet.is_empty() {
            if acct_config.signature_type != 0 {
                tracing::warn!(
                    account = %acct_config.name,
                    signature_type = acct_config.signature_type,
                    eoa = %wallet_address,
                    "proxy_wallet not configured — using EOA address"
                );
            }
            wallet_address
        } else {
            acct_config.proxy_wallet.parse::<alloy::primitives::Address>()
                .context(format!("Invalid proxy_wallet for account {}", acct_config.name))?
        };

        tracing::info!(
            account = %acct_config.name,
            address = %wallet_address,
            proxy = %proxy_addr,
            strategies = ?acct_config.strategies,
            "Account loaded"
        );

        // Authenticate with CLOB
        let clob = match ClobExecutor::connect(
            &settings.clob.host, signer, acct_config.signature_type,
        ).await {
            Ok(c) => {
                tracing::info!(account = %acct_config.name, "CLOB authenticated");
                Some(c)
            }
            Err(e) => {
                tracing::warn!(
                    account = %acct_config.name,
                    error = %e,
                    "CLOB authentication failed — account in OBSERVE-ONLY mode"
                );
                None
            }
        };

        let trading_enabled = clob.is_some();
        let executor: Arc<dyn pa_core::traits::Executor> = if let Some(clob) = clob {
            let provider = alloy::providers::ProviderBuilder::new()
                .connect(&settings.chain.rpc_url)
                .await
                .context(format!("Failed to connect RPC for account {}", acct_config.name))?;
            let ctf = CtfExecutor::with_neg_risk(provider, settings.chain.chain_id)
                .context(format!("Failed to create CTF executor for account {}", acct_config.name))?;
            Arc::new(HybridOrchestrator::new(clob, ctf))
        } else {
            Arc::new(DryRunExecutor)
        };

        // Query initial USDC balance
        let usdc_balance: Arc<std::sync::RwLock<Decimal>> =
            Arc::new(std::sync::RwLock::new(Decimal::ZERO));
        match executor.get_balance().await {
            Ok(bal) => {
                *usdc_balance.write().unwrap() = bal;
                tracing::info!(
                    account = %acct_config.name,
                    balance_usdc = %bal,
                    "CLOB collateral balance loaded"
                );
            }
            Err(e) => {
                tracing::warn!(
                    account = %acct_config.name,
                    error = %e,
                    "Failed to query CLOB balance"
                );
            }
        }

        // Init risk manager
        let risk_manager_impl = Arc::new(RiskManagerImpl::new(settings.risk.clone()));

        // Load positions from Data API
        let position_loader = PositionLoader::new(proxy_addr)?;
        let api_positions = match position_loader.load_positions().await {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(
                    account = %acct_config.name,
                    error = %e,
                    "Failed to load positions — starting with empty positions"
                );
                Vec::new()
            }
        };

        // Ensure held position markets are in the shared markets list
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
                    account = %acct_config.name,
                    missing = missing_condition_ids.len(),
                    "Fetching market data for held positions not in discovery"
                );
                let position_markets = market_data
                    .fetch_position_markets(&missing_condition_ids)
                    .await;
                if !position_markets.is_empty() {
                    let seed_cache = market_data.cache();
                    let mut seeded = 0;
                    for m in &position_markets {
                        if seed_market_cache(seed_cache, m) {
                            seeded += 1;
                        }
                    }
                    let mut markets_write = shared_markets.write().await;
                    markets_write.extend(position_markets);
                    tracing::info!(
                        account = %acct_config.name,
                        seeded,
                        total_markets = markets_write.len(),
                        "Position markets injected"
                    );
                }
            }
        }

        // Load positions into risk manager
        let markets_snapshot = shared_markets.read().await;
        let initial_positions: Vec<_> = api_positions
            .iter()
            .map(|p| {
                let strategy_type = infer_strategy_type(p.token_id, &markets_snapshot, &neg_risk_events);
                if strategy_type.is_some() {
                    tracing::debug!(
                        account = %acct_config.name,
                        token_id = %p.token_id,
                        strategy = ?strategy_type,
                        size = %p.size,
                        "Position tagged"
                    );
                } else {
                    let question = markets_snapshot.iter()
                        .find(|m| m.tokens.iter().any(|t| t.token_id == p.token_id))
                        .map(|m| m.question.as_str())
                        .unwrap_or("<market not found>");
                    tracing::warn!(
                        account = %acct_config.name,
                        token_id = %p.token_id,
                        size = %p.size,
                        question = %question,
                        "UNTAGGED position — strategy inference failed"
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
                account = %acct_config.name,
                loaded = loaded_count,
                tagged = tagged_count,
                untagged = loaded_count - tagged_count,
                exposure = %risk_manager_impl.total_exposure(),
                "Positions loaded"
            );
        }

        let risk_manager: Arc<dyn pa_core::traits::RiskManager> =
            Arc::clone(&risk_manager_impl) as Arc<dyn pa_core::traits::RiskManager>;

        account_contexts.push(AccountContext {
            name: acct_config.name.clone(),
            trading_enabled,
            executor,
            risk_manager_impl,
            risk_manager,
            usdc_balance,
            proxy_addr,
            private_key,
            signature_type: acct_config.signature_type,
            chain_id: settings.chain.chain_id,
            strategies: acct_config.strategies.clone(),
        });
    }

    if account_contexts.is_empty() {
        tracing::error!("No valid accounts configured — exiting");
        return Ok(());
    }

    tracing::info!(
        active_accounts = account_contexts.len(),
        "All accounts initialized"
    );

    // --- Spawn per-account tasks ---
    let mut engine_handles: Vec<tokio::task::JoinHandle<()>> = Vec::new();

    for ctx in &account_contexts {
        let acct_name = ctx.name.clone();
        let acct_strategies = ctx.strategies.clone();

        // --- Balance refresh (every 30s) ---
        {
            let bal_state = Arc::clone(&ctx.usdc_balance);
            let bal_executor = Arc::clone(&ctx.executor);
            let bal_cancel = cancel.clone();
            let name = acct_name.clone();
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
                                        tracing::info!(
                                            account = %name,
                                            balance_usdc = %bal,
                                            prev = %prev,
                                            "USDC balance updated"
                                        );
                                    }
                                    *bal_state.write().unwrap() = bal;
                                }
                                Err(e) => {
                                    tracing::debug!(
                                        account = %name,
                                        error = %e,
                                        "Balance refresh failed"
                                    );
                                }
                            }
                        }
                    }
                }
            });
        }

        // --- Strategy engine ---
        // Determine which strategies this account should run
        let enabled_strategies: Vec<String> = settings.strategy.enabled.iter()
            .filter(|s| acct_strategies.contains(s))
            .cloned()
            .collect();

        if !enabled_strategies.is_empty() {
            let cache = market_data.cache().clone();

            let make_capital_fn = |bal: Arc<std::sync::RwLock<Decimal>>|
             -> Box<dyn Fn() -> Decimal + Send + Sync> {
                Box::new(move || {
                    let balance = *bal.read().unwrap();
                    balance.max(Decimal::ZERO)
                })
            };

            let make_balance_fn = |bal: Arc<std::sync::RwLock<Decimal>>|
             -> Box<dyn Fn() -> Decimal + Send + Sync> {
                Box::new(move || *bal.read().unwrap())
            };

            let mut strategies: Vec<Box<dyn pa_core::traits::Strategy>> = Vec::new();

            if enabled_strategies.contains(&"yes_no".to_string()) {
                let yes_no_strategy = YesNoArbitrage::new(
                    settings.strategy.min_spread_bps,
                    settings.strategy.max_trade_size_usdc,
                    settings.strategy.min_profit_usdc,
                    dec!(0.01),
                    Box::new({
                        let c = cache.clone();
                        move |token_id| c.get(&token_id)
                    }),
                    make_capital_fn(Arc::clone(&ctx.usdc_balance)),
                );
                strategies.push(Box::new(yes_no_strategy));
            }

            if enabled_strategies.contains(&"neg_risk".to_string()) && !neg_risk_events.is_empty() {
                let neg_risk_strategy = NegRiskArbitrage::new(
                    settings.strategy.min_spread_bps,
                    settings.strategy.max_trade_size_usdc,
                    settings.strategy.min_profit_usdc,
                    dec!(0.01),
                    neg_risk_events.clone(),
                    Box::new({
                        let c = cache.clone();
                        move |token_id| c.get(&token_id)
                    }),
                    make_capital_fn(Arc::clone(&ctx.usdc_balance)),
                );
                strategies.push(Box::new(neg_risk_strategy));
            }

            if enabled_strategies.contains(&"cross_market".to_string()) && !cross_market_pairs.is_empty() {
                let cross_market_strategy = CrossMarketArbitrage::new(
                    settings.strategy.min_spread_bps,
                    settings.strategy.max_trade_size_usdc,
                    settings.strategy.min_profit_usdc,
                    dec!(0.02),
                    cross_market_pairs.clone(),
                    Box::new({
                        let c = cache.clone();
                        move |token_id| c.get(&token_id)
                    }),
                    make_capital_fn(Arc::clone(&ctx.usdc_balance)),
                );
                strategies.push(Box::new(cross_market_strategy));
            }

            if enabled_strategies.contains(&"weather".to_string()) {
                let weather_cache = market_data.cache().clone();
                let rm_pos = Arc::clone(&ctx.risk_manager_impl);
                let rm_held = Arc::clone(&ctx.risk_manager_impl);
                let weather_strategy = pa_strategy::weather::WeatherAlphaStrategy::new(
                    settings.weather.clone(),
                    dec!(0.00),
                    Box::new(move |token_id| weather_cache.get(&token_id)),
                    make_capital_fn(Arc::clone(&ctx.usdc_balance)),
                    Box::new(move |tid: alloy::primitives::U256| rm_pos.get_position_size(&tid)),
                    neg_risk_events.clone(),
                    Box::new(move || rm_held.positions_by_strategy(pa_core::types::StrategyType::Weather)),
                    make_balance_fn(Arc::clone(&ctx.usdc_balance)),
                );
                strategies.push(Box::new(weather_strategy));
            }

            if enabled_strategies.contains(&"convergence".to_string()) {
                let conv_cache = market_data.cache().clone();
                let rm_pos_conv = Arc::clone(&ctx.risk_manager_impl);
                let rm_held_conv = Arc::clone(&ctx.risk_manager_impl);
                let convergence = pa_strategy::convergence::ResolutionConvergenceStrategy::new(
                    settings.convergence.clone(),
                    dec!(0.00),
                    Box::new(move |token_id| conv_cache.get(&token_id)),
                    make_capital_fn(Arc::clone(&ctx.usdc_balance)),
                    Box::new(move |tid: alloy::primitives::U256| rm_pos_conv.get_position_size(&tid)),
                    Box::new(move || rm_held_conv.positions_by_strategy(pa_core::types::StrategyType::ResolutionConvergence)),
                    make_balance_fn(Arc::clone(&ctx.usdc_balance)),
                );
                strategies.push(Box::new(convergence));
            }

            if enabled_strategies.contains(&"crypto".to_string()) {
                let crypto_cache = market_data.cache().clone();
                let rm_pos_crypto = Arc::clone(&ctx.risk_manager_impl);
                let rm_held_crypto = Arc::clone(&ctx.risk_manager_impl);
                let crypto = pa_strategy::crypto_alpha::CryptoAlphaStrategy::new(
                    settings.crypto_alpha.clone(),
                    dec!(0.00),
                    Box::new(move |token_id| crypto_cache.get(&token_id)),
                    make_capital_fn(Arc::clone(&ctx.usdc_balance)),
                    Box::new(move |tid: alloy::primitives::U256| rm_pos_crypto.get_position_size(&tid)),
                    neg_risk_events.clone(),
                    binary_event_groups.clone(),
                    Box::new(move || rm_held_crypto.positions_by_strategy(pa_core::types::StrategyType::CryptoAlpha)),
                    make_balance_fn(Arc::clone(&ctx.usdc_balance)),
                );
                strategies.push(Box::new(crypto));
            }

            if enabled_strategies.contains(&"smart_money".to_string()) {
                let sm_cache = market_data.cache().clone();
                let rm_pos_sm = Arc::clone(&ctx.risk_manager_impl);
                let rm_held_sm = Arc::clone(&ctx.risk_manager_impl);

                // Build token_to_condition map from discovered markets
                let sm_markets_snapshot = shared_markets.read().await;
                let sm_token_to_cid: Arc<std::sync::RwLock<std::collections::HashMap<alloy::primitives::U256, alloy::primitives::B256>>> =
                    Arc::new(std::sync::RwLock::new(
                        sm_markets_snapshot.iter()
                            .flat_map(|m| m.tokens.iter().map(|t| (t.token_id, m.condition_id)))
                            .collect()
                    ));

                // Markets lookup shared with strategy
                let sm_markets: Arc<std::sync::RwLock<std::collections::HashMap<alloy::primitives::B256, pa_core::types::MarketInfo>>> =
                    Arc::new(std::sync::RwLock::new(
                        sm_markets_snapshot.iter().map(|m| (m.condition_id, m.clone())).collect()
                    ));
                drop(sm_markets_snapshot);

                // Create WalletTracker
                let tracker = pa_market_data::wallet_tracker::WalletTracker::new(
                    settings.smart_money.clone(),
                    Arc::clone(&sm_token_to_cid),
                );
                let sm_signals = tracker.signals_ref();

                let smart_money = pa_strategy::smart_money::SmartMoneyStrategy::new(
                    settings.smart_money.clone(),
                    dec!(0.00),
                    Box::new(move |token_id| sm_cache.get(&token_id)),
                    make_capital_fn(Arc::clone(&ctx.usdc_balance)),
                    Box::new(move |tid: alloy::primitives::U256| rm_pos_sm.get_position_size(&tid)),
                    Box::new(move || rm_held_sm.positions_by_strategy(pa_core::types::StrategyType::SmartMoney)),
                    make_balance_fn(Arc::clone(&ctx.usdc_balance)),
                    sm_signals,
                    Arc::clone(&sm_markets),
                );
                strategies.push(Box::new(smart_money));

                // Spawn WalletTracker background task
                let tracker_cancel = cancel.clone();
                let sm_rpc_url = settings.chain.rpc_url.clone();
                tokio::spawn(async move {
                    tracker.run(tracker_cancel, &sm_rpc_url).await;
                });
            }

            if !strategies.is_empty() {
                // Event calendar (shared config, per-account instance)
                let event_calendar = if settings.event_calendar.enabled {
                    let ec = Arc::new(EventCalendarService::new(settings.event_calendar.clone()));
                    ec.refresh().await;
                    Some(ec)
                } else {
                    None
                };

                let engine_cache = market_data.cache().clone();
                let engine_rm_all = Arc::clone(&ctx.risk_manager_impl);
                let engine = StrategyEngine::new(
                    strategies,
                    ctx.executor.clone(),
                    ctx.risk_manager.clone(),
                    settings.strategy.scan_interval_ms,
                    event_calendar,
                    Box::new(move |token_id| engine_cache.get(&token_id)),
                    make_capital_fn(Arc::clone(&ctx.usdc_balance)),
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

                let engine_shared = Arc::clone(&shared_markets);
                let engine_cancel = cancel.clone();
                let update_rx = market_data.ws_feed().await.subscribe_updates();
                let name = acct_name.clone();
                let handle = tokio::spawn(async move {
                    tracing::info!(
                        account = %name,
                        "Strategy engine started"
                    );
                    engine.run(engine_shared, update_rx, engine_cancel).await;
                });
                engine_handles.push(handle);

                tracing::info!(
                    account = %acct_name,
                    strategies = ?enabled_strategies,
                    trading = ctx.trading_enabled,
                    "Strategy engine initialized"
                );
            }
        }

        // --- Liquidity Rewards background task ---
        if acct_strategies.contains(&"liquidity_rewards".to_string())
            && settings.liquidity_rewards.enabled
            && ctx.trading_enabled
        {
            let lr_config = settings.liquidity_rewards.clone();
            let lr_cache = market_data.cache().clone();
            let lr_cancel = cancel.clone();
            let lr_rm = Arc::clone(&ctx.risk_manager_impl);
            let lr_shared = Arc::clone(&shared_markets);
            let lr_private_key = ctx.private_key.clone();
            let lr_clob_host = settings.clob.host.clone();
            let lr_sig_type = ctx.signature_type;
            let lr_chain_id = ctx.chain_id;
            let lr_name = acct_name.clone();
            let lr_update_rx = market_data.ws_feed().await.subscribe_updates();

            tokio::spawn(async move {
                let lr_signer = match PrivateKeySigner::from_str(&lr_private_key) {
                    Ok(s) => s.with_chain_id(Some(lr_chain_id)),
                    Err(e) => {
                        tracing::error!(account = %lr_name, error = %e, "LR: failed to parse signer");
                        return;
                    }
                };
                let lr_clob = match ClobExecutor::connect(&lr_clob_host, lr_signer, lr_sig_type).await {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::error!(account = %lr_name, error = %e, "LR: CLOB auth failed");
                        return;
                    }
                };
                tracing::info!(account = %lr_name, "LR: CLOB authenticated, starting liquidity rewards");

                // Query actual USDC balance to cap exposure
                let effective_max_exposure = match lr_clob.get_balance().await {
                    Ok(bal) => {
                        let cap = bal.min(lr_config.max_total_exposure);
                        tracing::info!(
                            account = %lr_name,
                            balance_usdc = %bal,
                            config_max = %lr_config.max_total_exposure,
                            effective_cap = %cap,
                            "LR: exposure cap set from balance"
                        );
                        cap
                    }
                    Err(e) => {
                        tracing::warn!(account = %lr_name, error = %e, "LR: failed to query balance, using config max");
                        lr_config.max_total_exposure
                    }
                };

                let mut outstanding_orders: std::collections::HashMap<
                    alloy::primitives::B256,
                    std::collections::HashMap<String, LrOrderMeta>,
                > = std::collections::HashMap::new();

                let mut last_quoted_mid: std::collections::HashMap<alloy::primitives::U256, Decimal> = std::collections::HashMap::new();
                let mut token_to_condition: std::collections::HashMap<alloy::primitives::U256, alloy::primitives::B256> = std::collections::HashMap::new();
                let mut last_quote_time: std::collections::HashMap<alloy::primitives::B256, std::time::Instant> = std::collections::HashMap::new();

                // Fetch current rewards from CLOB API
                let clob_rewards = match fetch_clob_rewards(&lr_clob).await {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::warn!(error = %e, "LR: Failed to fetch initial rewards");
                        Vec::new()
                    }
                };
                
                let markets_init = lr_shared.read().await;
                let mut active_candidates = pa_strategy::liquidity_rewards::select_reward_markets_with_clob_data(
                    &markets_init, &clob_rewards, &lr_config,
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

                // Initial mapping + quoting
                {
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

                    let mut total_exposure = Decimal::ZERO;
                    for candidate in &active_candidates {
                        let (metas, exp, yes_mid, no_mid) = lr_quote_one_market(
                            &candidate.market, &lr_config, &lr_cache, &lr_rm, &lr_clob, total_exposure,
                            candidate.clob_rewards_max_spread, candidate.clob_rewards_min_size,
                            effective_max_exposure,
                        ).await;
                        total_exposure += exp;
                        let cid = candidate.market.condition_id;
                        if !metas.is_empty() {
                            outstanding_orders.insert(cid, metas.into_iter().collect());
                        }
                        if candidate.market.tokens.len() >= 2 {
                            if let Some(m) = yes_mid { last_quoted_mid.insert(candidate.market.tokens[0].token_id, m); }
                            if let Some(m) = no_mid { last_quoted_mid.insert(candidate.market.tokens[1].token_id, m); }
                        }
                        let now = std::time::Instant::now();
                        last_quote_time.insert(cid, now);
                    }

                    tracing::info!(
                        account = %lr_name,
                        active_markets = outstanding_orders.len(),
                        total_candidates = active_candidates.len(),
                        "LR: initial market selection and quote complete"
                    );
                }

                loop {
                    tokio::select! {
                        _ = lr_cancel.cancelled() => {
                            let all_ids: Vec<String> = outstanding_orders.values()
                                .flat_map(|m| m.keys().cloned())
                                .collect();
                            if !all_ids.is_empty() {
                                let refs: Vec<&str> = all_ids.iter().map(|s| s.as_str()).collect();
                                if let Err(e) = lr_clob.cancel_orders(&refs).await {
                                    tracing::warn!(account = %lr_name, error = %e, "LR: cancel on shutdown failed");
                                }
                                pa_monitor::metrics::LR_ORDERS_CANCELLED.inc_by(all_ids.len() as u64);
                            }
                            tracing::info!(account = %lr_name, "LR: shutdown, cancelled {} orders", all_ids.len());
                            break;
                        }
                        result = lr_update_rx.recv() => {
                            if let Ok(update) = result {
                                let tid = update.token_id;
                                let Some(&cid) = token_to_condition.get(&tid) else { continue };

                                if lr_config.order_depth_level > 0 {
                                    // ── Depth mode: check if any order has reached cancel depth ──
                                    let now = std::time::Instant::now();
                                    if let Some(&last_t) = last_quote_time.get(&cid) {
                                        if now.duration_since(last_t) < requote_cooldown { continue; }
                                    }

                                    let need_requote = outstanding_orders.get(&cid).is_some_and(|order_map| {
                                        order_map.values().any(|meta| {
                                            lr_cache.get(&meta.token_id).is_some_and(|book| {
                                                pa_strategy::liquidity_rewards::should_cancel_depth_order(
                                                    &book, meta.price, meta.is_buy, lr_config.cancel_depth_level,
                                                )
                                            })
                                        })
                                    });

                                    if !need_requote { continue; }

                                    tracing::info!(
                                        account = %lr_name,
                                        token = %tid, market = %cid,
                                        cancel_depth = lr_config.cancel_depth_level,
                                        "LR: depth cancel triggered"
                                    );

                                    // Cancel all orders for this market and re-quote
                                    if let Some(order_map) = outstanding_orders.remove(&cid) {
                                        if !order_map.is_empty() {
                                            let id_strs: Vec<String> = order_map.into_keys().collect();
                                            let refs: Vec<&str> = id_strs.iter().map(|s| s.as_str()).collect();
                                            if let Err(e) = lr_clob.cancel_orders(&refs).await {
                                                tracing::warn!(error = %e, "LR: depth re-quote cancel failed");
                                            }
                                            pa_monitor::metrics::LR_ORDERS_CANCELLED.inc_by(id_strs.len() as u64);
                                        }
                                    }

                                    if let Some(&idx) = cid_to_candidate_idx.get(&cid) {
                                        if let Some(candidate) = active_candidates.get(idx) {
                                            let current_exposure = Decimal::ZERO;
                                            let (metas, _exp, yes_mid, no_mid) = lr_quote_one_market(
                                                &candidate.market, &lr_config, &lr_cache, &lr_rm, &lr_clob, current_exposure,
                                                candidate.clob_rewards_max_spread, candidate.clob_rewards_min_size,
                                                effective_max_exposure,
                                            ).await;
                                            if !metas.is_empty() {
                                                outstanding_orders.insert(cid, metas.into_iter().collect());
                                            }
                                            if candidate.market.tokens.len() >= 2 {
                                                if let Some(m) = yes_mid { last_quoted_mid.insert(candidate.market.tokens[0].token_id, m); }
                                                if let Some(m) = no_mid { last_quoted_mid.insert(candidate.market.tokens[1].token_id, m); }
                                            }
                                            last_quote_time.insert(cid, now);
                                        }
                                    }
                                } else {
                                    // ── Legacy midpoint drift mode ──
                                    let Some(new_mid) = lr_cache.get(&tid).and_then(|b| b.midpoint()) else { continue };
                                    let Some(&old_mid) = last_quoted_mid.get(&tid) else { continue };
                                    if old_mid <= Decimal::ZERO { continue; }

                                    let drift_bps = ((new_mid - old_mid).abs() / old_mid * dec!(10000)).to_u32().unwrap_or(0);
                                    if drift_bps < lr_config.requote_trigger_bps { continue; }

                                    let now = std::time::Instant::now();
                                    if let Some(&last_t) = last_quote_time.get(&cid) {
                                        if now.duration_since(last_t) < requote_cooldown { continue; }
                                    }

                                    tracing::info!(
                                        account = %lr_name,
                                        token = %tid, market = %cid,
                                        old_mid = %old_mid, new_mid = %new_mid,
                                        drift_bps = drift_bps,
                                        "LR: WS re-quote triggered"
                                    );

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

                                    last_quoted_mid.remove(&tid);

                                    if let Some(&idx) = cid_to_candidate_idx.get(&cid) {
                                        if let Some(candidate) = active_candidates.get(idx) {
                                            let current_exposure = Decimal::ZERO;
                                            let (metas, _exp, yes_mid, no_mid) = lr_quote_one_market(
                                                &candidate.market, &lr_config, &lr_cache, &lr_rm, &lr_clob, current_exposure,
                                                candidate.clob_rewards_max_spread, candidate.clob_rewards_min_size,
                                                effective_max_exposure,
                                            ).await;
                                            if !metas.is_empty() {
                                                outstanding_orders.insert(cid, metas.into_iter().collect());
                                            }
                                            if candidate.market.tokens.len() >= 2 {
                                                if let Some(m) = yes_mid { last_quoted_mid.insert(candidate.market.tokens[0].token_id, m); }
                                                if let Some(m) = no_mid { last_quoted_mid.insert(candidate.market.tokens[1].token_id, m); }
                                            }
                                            last_quote_time.insert(cid, now);
                                        }
                                    }
                                }
                            }
                        }
                        _ = fallback_interval.tick() => {
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

                            last_quoted_mid.clear();

                            if active_candidates.is_empty() {
                                tracing::debug!(account = %lr_name, "LR: no eligible reward markets");
                                continue;
                            }

                            let mut total_exposure = Decimal::ZERO;
                            for candidate in &active_candidates {
                                let cid = candidate.market.condition_id;
                                let (metas, exp, yes_mid, no_mid) = lr_quote_one_market(
                                    &candidate.market, &lr_config, &lr_cache, &lr_rm, &lr_clob, total_exposure,
                                    candidate.clob_rewards_max_spread, candidate.clob_rewards_min_size,
                                    effective_max_exposure,
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
                                account = %lr_name,
                                active_markets = outstanding_orders.len(),
                                "LR: fallback quote refresh complete"
                            );
                        }
                        _ = market_interval.tick() => {
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

                            // Refresh rewards from CLOB API
                            let clob_rewards = match fetch_clob_rewards(&lr_clob).await {
                                Ok(r) => r,
                                Err(e) => {
                                    tracing::warn!(error = %e, "LR: Failed to refresh rewards");
                                    Vec::new()
                                }
                            };
                            
                            let markets_snapshot = lr_shared.read().await;
                            active_candidates = pa_strategy::liquidity_rewards::select_reward_markets_with_clob_data(
                                &markets_snapshot, &clob_rewards, &lr_config,
                            );
                            drop(markets_snapshot);

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

                            let mut total_exposure = Decimal::ZERO;
                            for candidate in &active_candidates {
                                let cid = candidate.market.condition_id;
                                let (metas, exp, yes_mid, no_mid) = lr_quote_one_market(
                                    &candidate.market, &lr_config, &lr_cache, &lr_rm, &lr_clob, total_exposure,
                                    candidate.clob_rewards_max_spread, candidate.clob_rewards_min_size,
                                    effective_max_exposure,
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
                                account = %lr_name,
                                active_markets = outstanding_orders.len(),
                                "LR: market re-selection complete"
                            );
                        }
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

                                let mut any_full_fill = false;
                                let mut fully_filled_ids: Vec<String> = Vec::new();

                                for (oid, meta) in tracked.iter_mut() {
                                    let api_match = api_orders.iter().find(|o| o.order_id == *oid);

                                    let (is_fully_done, api_matched_size) = match api_match {
                                        Some(o) => (o.is_matched, o.size_matched),
                                        None => (true, meta.size),
                                    };

                                    let delta = api_matched_size - meta.last_synced_matched;
                                    if delta > Decimal::ZERO {
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
                                            account = %lr_name,
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

                                for id in &fully_filled_ids {
                                    tracked.remove(id);
                                }

                                if any_full_fill {
                                    let remaining_ids: Vec<String> = tracked.keys().cloned().collect();
                                    if !remaining_ids.is_empty() {
                                        let refs: Vec<&str> = remaining_ids.iter().map(|s| s.as_str()).collect();
                                        if let Err(e) = lr_clob.cancel_orders(&refs).await {
                                            tracing::debug!(error = %e, "LR: fill re-quote cancel failed");
                                        }
                                        pa_monitor::metrics::LR_ORDERS_CANCELLED.inc_by(remaining_ids.len() as u64);
                                    }
                                    outstanding_orders.remove(&cid);

                                    if let Some(&idx) = cid_to_candidate_idx.get(&cid) {
                                        if let Some(candidate) = active_candidates.get(idx) {
                                            let current_exposure = Decimal::ZERO;
                                            let (metas, _exp, yes_mid, no_mid) = lr_quote_one_market(
                                                &candidate.market, &lr_config, &lr_cache, &lr_rm, &lr_clob, current_exposure,
                                                candidate.clob_rewards_max_spread, candidate.clob_rewards_min_size,
                                                effective_max_exposure,
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
                account = %acct_name,
                max_markets = settings.liquidity_rewards.max_markets,
                "Liquidity rewards task started"
            );
        } else if acct_strategies.contains(&"liquidity_rewards".to_string())
            && settings.liquidity_rewards.enabled
            && !ctx.trading_enabled
        {
            tracing::warn!(
                account = %acct_name,
                "LR enabled but CLOB auth failed — LR disabled for this account"
            );
        }

        // --- Auto-redeem resolved positions (every 5 minutes) ---
        // Redeem is a pure on-chain operation (CTF/Safe contracts), independent of CLOB auth,
        // so we do NOT gate on `ctx.trading_enabled`.
        'redeem: {
            let redeem_signer = match PrivateKeySigner::from_str(&ctx.private_key) {
                Ok(s) => s.with_chain_id(Some(ctx.chain_id)),
                Err(e) => {
                    tracing::warn!(account = %acct_name, error = %e, "Invalid private key for redeem signer, skipping auto-redeem");
                    break 'redeem;
                }
            };
            let redeem_provider = match alloy::providers::ProviderBuilder::new()
                .wallet(redeem_signer.clone())
                .connect(&settings.chain.rpc_url)
                .await
            {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!(account = %acct_name, error = %e, "Failed to connect RPC for redeem, skipping auto-redeem");
                    break 'redeem;
                }
            };
            let redeem_proxy = ctx.proxy_addr;
            let redeem_cancel = cancel.clone();
            let redeem_name = acct_name.clone();
            let redeem_chain_id = ctx.chain_id;

            if ctx.signature_type == 2 {
                // GnosisSafe path
                let safe_redeemer = SafeRedeemer::new(redeem_provider, redeem_signer, redeem_proxy);
                match safe_redeemer.verify_ownership().await {
                    Ok(true) => {
                        tracing::info!(account = %acct_name, safe = %redeem_proxy, "SafeRedeemer: EOA is Safe owner");
                    }
                    Ok(false) => {
                        tracing::warn!(account = %acct_name, safe = %redeem_proxy, "SafeRedeemer: EOA is NOT a Safe owner, skipping auto-redeem");
                        break 'redeem;
                    }
                    Err(e) => {
                        tracing::warn!(account = %acct_name, error = %e, "SafeRedeemer: could not verify ownership, skipping auto-redeem");
                        break 'redeem;
                    }
                }
                tokio::spawn(async move {
                    let redeem_loader = match PositionLoader::new(redeem_proxy) {
                        Ok(l) => l,
                        Err(e) => {
                            tracing::error!(account = %redeem_name, error = %e, "Failed to create redeem position loader");
                            return;
                        }
                    };
                    let mut interval = tokio::time::interval(Duration::from_secs(300));
                    loop {
                        tokio::select! {
                            _ = redeem_cancel.cancelled() => break,
                            _ = interval.tick() => {
                                match redeem_loader.find_redeemable().await {
                                    Ok(positions) if positions.is_empty() => {}
                                    Ok(positions) => {
                                        tracing::info!(
                                            account = %redeem_name,
                                            count = positions.len(),
                                            "Found redeemable positions, claiming..."
                                        );
                                        for pos in &positions {
                                            tracing::info!(
                                                account = %redeem_name,
                                                condition_id = %pos.condition_id,
                                                title = %pos.title,
                                                size = %pos.size,
                                                neg_risk = pos.neg_risk,
                                                outcome_index = pos.outcome_index,
                                                "Redeeming resolved position via GnosisSafe"
                                            );
                                            let result = if pos.neg_risk {
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
                                                        account = %redeem_name,
                                                        condition_id = %pos.condition_id,
                                                        tx_hash = %tx.tx_hash,
                                                        "Redeem successful"
                                                    );
                                                }
                                                Err(e) => {
                                                    tracing::warn!(
                                                        account = %redeem_name,
                                                        condition_id = %pos.condition_id,
                                                        error = %e,
                                                        "Redeem failed"
                                                    );
                                                }
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        tracing::debug!(account = %redeem_name, error = %e, "Redeemable check failed");
                                    }
                                }
                            }
                        }
                    }
                });
                tracing::info!(account = %acct_name, "Auto-redeem task started (GnosisSafe)");
            } else {
                // Direct CTF path for EOA/Proxy accounts
                let ctf = match CtfExecutor::with_neg_risk(redeem_provider, redeem_chain_id) {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!(account = %acct_name, error = %e, "Failed to create CtfExecutor for redeem, skipping auto-redeem");
                        break 'redeem;
                    }
                };
                tokio::spawn(async move {
                    let redeem_loader = match PositionLoader::new(redeem_proxy) {
                        Ok(l) => l,
                        Err(e) => {
                            tracing::error!(account = %redeem_name, error = %e, "Failed to create redeem position loader");
                            return;
                        }
                    };
                    let mut interval = tokio::time::interval(Duration::from_secs(300));
                    loop {
                        tokio::select! {
                            _ = redeem_cancel.cancelled() => break,
                            _ = interval.tick() => {
                                match redeem_loader.find_redeemable().await {
                                    Ok(positions) if positions.is_empty() => {}
                                    Ok(positions) => {
                                        tracing::info!(
                                            account = %redeem_name,
                                            count = positions.len(),
                                            "Found redeemable positions, claiming via direct CTF..."
                                        );
                                        for pos in &positions {
                                            tracing::info!(
                                                account = %redeem_name,
                                                condition_id = %pos.condition_id,
                                                title = %pos.title,
                                                size = %pos.size,
                                                neg_risk = pos.neg_risk,
                                                outcome_index = pos.outcome_index,
                                                "Redeeming resolved position via direct CTF"
                                            );
                                            let result = if pos.neg_risk {
                                                let amount_raw = pos.size * rust_decimal::Decimal::from(1_000_000u64);
                                                let amount = alloy::primitives::U256::from(
                                                    amount_raw.to_u64().unwrap_or(0)
                                                );
                                                let amounts = if pos.outcome_index == 0 {
                                                    vec![amount, alloy::primitives::U256::ZERO]
                                                } else {
                                                    vec![alloy::primitives::U256::ZERO, amount]
                                                };
                                                ctf.redeem_neg_risk(
                                                    pos.condition_id,
                                                    amounts,
                                                ).await
                                            } else {
                                                ctf.redeem(pos.condition_id).await
                                            };
                                            match result {
                                                Ok(tx) => {
                                                    tracing::info!(
                                                        account = %redeem_name,
                                                        condition_id = %pos.condition_id,
                                                        tx_hash = %tx.tx_hash,
                                                        "Redeem successful"
                                                    );
                                                }
                                                Err(e) => {
                                                    tracing::warn!(
                                                        account = %redeem_name,
                                                        condition_id = %pos.condition_id,
                                                        error = %e,
                                                        "Redeem failed"
                                                    );
                                                }
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        tracing::debug!(account = %redeem_name, error = %e, "Redeemable check failed");
                                    }
                                }
                            }
                        }
                    }
                });
                tracing::info!(account = %acct_name, "Auto-redeem task started (direct CTF)");
            }
        }

        // --- Periodic position sync (reconcile with Data API every 5 min) ---
        {
            let sync_rm = Arc::clone(&ctx.risk_manager_impl);
            let sync_markets = Arc::clone(&shared_markets);
            let sync_neg_risk = neg_risk_events.clone();
            let sync_cancel = cancel.clone();
            let sync_proxy = ctx.proxy_addr;
            let sync_name = acct_name.clone();
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
                                    tracing::warn!(account = %sync_name, error = %e, "Position sync: failed to create loader");
                                    continue;
                                }
                            };
                            let api_positions = match loader.load_positions().await {
                                Ok(p) => p,
                                Err(e) => {
                                    tracing::warn!(account = %sync_name, error = %e, "Position sync: Data API fetch failed");
                                    continue;
                                }
                            };

                            let current = sync_rm.snapshot_positions();
                            let known: std::collections::HashMap<alloy::primitives::U256, Decimal> =
                                current.iter().map(|(tid, e)| (*tid, e.size)).collect();

                            let markets_snapshot = sync_markets.read().await;
                            let mut added = 0u32;
                            let mut updated = 0u32;
                            let mut retagged = 0u32;
                            for pos in &api_positions {
                                let existing_size = known.get(&pos.token_id).copied().unwrap_or(Decimal::ZERO);

                                if existing_size == pos.size && existing_size > Decimal::ZERO {
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
                                                account = %sync_name,
                                                token_id = %pos.token_id,
                                                strategy = ?strategy_type,
                                                "Position sync: re-tagged"
                                            );
                                            retagged += 1;
                                        }
                                    }
                                    continue;
                                }
                                if existing_size == Decimal::ZERO && pos.size > Decimal::ZERO {
                                    let strategy_type = infer_strategy_type(
                                        pos.token_id, &markets_snapshot, &sync_neg_risk,
                                    );
                                    sync_rm.sync_position(
                                        pos.token_id, pos.size, pos.avg_price,
                                        strategy_type, Some(pos.condition_id),
                                    );
                                    tracing::info!(
                                        account = %sync_name,
                                        token_id = %pos.token_id,
                                        size = %pos.size,
                                        strategy = ?strategy_type,
                                        "Position sync: discovered missing position"
                                    );
                                    added += 1;
                                } else if pos.size > Decimal::ZERO && existing_size > Decimal::ZERO && pos.size != existing_size {
                                    let strategy_type = infer_strategy_type(
                                        pos.token_id, &markets_snapshot, &sync_neg_risk,
                                    );
                                    sync_rm.sync_position(
                                        pos.token_id, pos.size, pos.avg_price,
                                        strategy_type, Some(pos.condition_id),
                                    );
                                    tracing::info!(
                                        account = %sync_name,
                                        token_id = %pos.token_id,
                                        prev_size = %existing_size,
                                        new_size = %pos.size,
                                        "Position sync: size changed"
                                    );
                                    updated += 1;
                                } else if pos.size == Decimal::ZERO && existing_size > Decimal::ZERO {
                                    sync_rm.sync_position(
                                        pos.token_id, Decimal::ZERO, Decimal::ZERO,
                                        None, Some(pos.condition_id),
                                    );
                                    tracing::info!(
                                        account = %sync_name,
                                        token_id = %pos.token_id,
                                        prev_size = %existing_size,
                                        "Position sync: cleared stale position"
                                    );
                                    updated += 1;
                                }
                            }
                            let api_tokens: HashSet<alloy::primitives::U256> =
                                api_positions.iter().map(|p| p.token_id).collect();
                            for (tid, entry) in &current {
                                if entry.size > Decimal::ZERO && !api_tokens.contains(tid) {
                                    sync_rm.sync_position(
                                        *tid, Decimal::ZERO, Decimal::ZERO,
                                        None, entry.condition_id,
                                    );
                                    tracing::info!(
                                        account = %sync_name,
                                        token_id = %tid,
                                        prev_size = %entry.size,
                                        "Position sync: cleared — no longer in Data API"
                                    );
                                    updated += 1;
                                }
                            }
                            drop(markets_snapshot);

                            if added > 0 || updated > 0 || retagged > 0 {
                                tracing::info!(
                                    account = %sync_name,
                                    added, updated, retagged,
                                    total_api = api_positions.len(),
                                    "Position sync complete"
                                );
                            }
                        }
                    }
                }
            });
        }

        // --- Daily circuit breaker reset at midnight UTC ---
        {
            let daily_rm = Arc::clone(&ctx.risk_manager) as Arc<dyn pa_core::traits::RiskManager>;
            let daily_cancel = cancel.clone();
            tokio::spawn(async move {
                loop {
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
        }

        tracing::info!(
            account = %acct_name,
            "All tasks started for account"
        );
    } // end per-account loop

    // --- Periodic market refresh background task (shared across accounts) ---
    let refresh_interval = settings.market_filter.market_refresh_interval_secs;
    if refresh_interval > 0 {
        let refresh_markets = Arc::clone(&shared_markets);
        let refresh_market_data = Arc::clone(&market_data);
        let refresh_cache = market_data.cache().clone();
        let refresh_cancel = cancel.clone();
        let refresh_enabled_strategies = settings.strategy.enabled.clone();
        let refresh_ws_max = settings.market_filter.ws_max_instruments;
        // Collect all accounts' risk managers for held token aggregation
        let refresh_risk_managers: Vec<Arc<RiskManagerImpl>> = account_contexts.iter()
            .map(|ctx| Arc::clone(&ctx.risk_manager_impl))
            .collect();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(refresh_interval));
            interval.tick().await;

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

                                    // Aggregate held tokens across all accounts
                                    let mut held_tokens: Vec<alloy::primitives::U256> = Vec::new();
                                    for rm in &refresh_risk_managers {
                                        for (tid, _) in rm.snapshot_positions() {
                                            if !held_tokens.contains(&tid) {
                                                held_tokens.push(tid);
                                            }
                                        }
                                    }
                                    let token_ids = build_ws_token_list(
                                        &current,
                                        &held_tokens,
                                        &refresh_enabled_strategies,
                                        refresh_ws_max,
                                    );
                                    drop(current);
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

    // Wait for Ctrl+C
    tokio::signal::ctrl_c().await?;
    tracing::info!("Shutdown signal received");

    // Cancel all tasks
    cancel.cancel();

    // Cancel outstanding orders for all accounts
    tracing::info!("Cancelling outstanding orders...");
    for ctx in &account_contexts {
        if let Err(e) = ctx.executor.cancel_all().await {
            tracing::error!(account = %ctx.name, error = %e, "Failed to cancel orders on shutdown");
        }
    }

    // Wait for engines to finish
    for handle in engine_handles {
        let _ = handle.await;
    }

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
    rewards_max_spread: Decimal,
    rewards_min_size: Decimal,
    effective_max_exposure: Decimal,
) -> (Vec<(String, LrOrderMeta)>, Decimal, Option<Decimal>, Option<Decimal>) {
    let cid = market.condition_id;
    let yes_tid = market.tokens[0].token_id;
    let no_tid = market.tokens[1].token_id;

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

                let quote_opt = if config.order_depth_level > 0 {
                    pa_strategy::liquidity_rewards::compute_depth_quotes(
                        &yes_book, config.order_depth_level,
                        rewards_max_spread, yes_position, config, rewards_min_size,
                    )
                } else {
                    pa_strategy::liquidity_rewards::compute_quotes(
                        mid, rewards_max_spread, yes_position,
                        config, rewards_min_size, market.tick_size,
                    )
                };

                if let Some(quote) = quote_opt {
                    tracing::info!(
                        market = %cid, side = "YES", midpoint = %mid,
                        rewards_max_spread = %rewards_max_spread,
                        bid = %quote.bid_price, ask = %quote.ask_price,
                        "LR: computed quotes"
                    );
                    let remaining_pos = (config.max_position_per_market - yes_position).max(Decimal::ZERO);
                    let remaining_exp = (effective_max_exposure - current_exposure - exposure_added).max(Decimal::ZERO);
                    // Convert remaining exposure (USDC) to max token count at bid price
                    let max_from_exp = if quote.bid_price > Decimal::ZERO {
                        remaining_exp / quote.bid_price
                    } else {
                        Decimal::ZERO
                    };
                    let bid_size = quote.size.min(remaining_pos).min(max_from_exp);

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

                let quote_opt = if config.order_depth_level > 0 {
                    pa_strategy::liquidity_rewards::compute_depth_quotes(
                        &no_book, config.order_depth_level,
                        rewards_max_spread, no_position, config, rewards_min_size,
                    )
                } else {
                    pa_strategy::liquidity_rewards::compute_quotes(
                        mid, rewards_max_spread, no_position,
                        config, rewards_min_size, market.tick_size,
                    )
                };

                if let Some(quote) = quote_opt {
                    tracing::info!(
                        market = %cid, side = "NO", midpoint = %mid,
                        rewards_max_spread = %rewards_max_spread,
                        bid = %quote.bid_price, ask = %quote.ask_price,
                        "LR: computed quotes"
                    );
                    let remaining_pos = (config.max_position_per_market - no_position).max(Decimal::ZERO);
                    let remaining_exp = (effective_max_exposure - current_exposure - exposure_added).max(Decimal::ZERO);
                    // Convert remaining exposure (USDC) to max token count at bid price
                    let max_from_exp = if quote.bid_price > Decimal::ZERO {
                        remaining_exp / quote.bid_price
                    } else {
                        Decimal::ZERO
                    };
                    let bid_size = quote.size.min(remaining_pos).min(max_from_exp);

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