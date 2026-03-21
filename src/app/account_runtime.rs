//! Per-account runtime orchestration.
//!
//! This module wires strategy engines and account-scoped background tasks for a
//! single account after the account has already been constructed.

use std::sync::Arc;

use arc_swap::ArcSwap;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

use pa_core::config::Settings;
use pa_market_data::event_calendar::EventCalendarService;
use pa_market_data::service::MarketDataService;
use pa_strategy::engine::StrategyEngine;

use crate::app::liquidity_rewards::spawn_liquidity_rewards_task;
use crate::app::tasks::{
    spawn_auto_redeem, spawn_balance_refresh, spawn_daily_reset, spawn_position_sync,
};
use crate::app::types::AccountContext;

pub struct AccountRuntimeArtifacts {
    pub engine_handle: Option<tokio::task::JoinHandle<()>>,
    pub smart_money_token_map: Option<
        Arc<
            std::sync::RwLock<
                std::collections::HashMap<alloy::primitives::U256, alloy::primitives::B256>,
            >,
        >,
    >,
}

pub async fn spawn_account_runtime(
    settings: &Settings,
    ctx: &AccountContext,
    market_data: &Arc<MarketDataService>,
    shared_markets: &Arc<tokio::sync::RwLock<Vec<pa_core::types::MarketInfo>>>,
    neg_risk_events: &[pa_core::types::NegRiskEvent],
    binary_event_groups: &[pa_core::types::BinaryEventGroup],
    lr_runtime_status: Arc<tokio::sync::RwLock<pa_monitor::api::LrRuntimeStatus>>,
    cancel: tokio_util::sync::CancellationToken,
) -> AccountRuntimeArtifacts {
    let acct_name = ctx.name.clone();
    let acct_strategies = ctx.strategies.clone();

    spawn_balance_refresh(
        acct_name.clone(),
        Arc::clone(&ctx.executor),
        Arc::clone(&ctx.usdc_balance),
        cancel.clone(),
    );

    let enabled_strategies: Vec<String> = settings
        .strategy
        .enabled
        .iter()
        .filter(|s| acct_strategies.contains(s))
        .cloned()
        .collect();

    let mut engine_handle = None;
    let mut smart_money_token_map = None;

    if !enabled_strategies.is_empty() {
        let make_capital_fn =
            |bal: Arc<ArcSwap<Decimal>>| -> Box<dyn Fn() -> Decimal + Send + Sync> {
                Box::new(move || {
                    let balance = **bal.load();
                    balance.max(Decimal::ZERO)
                })
            };

        let make_balance_fn =
            |bal: Arc<ArcSwap<Decimal>>| -> Box<dyn Fn() -> Decimal + Send + Sync> {
                Box::new(move || **bal.load())
            };

        let event_calendar = if settings.event_calendar.enabled {
            let ec = Arc::new(EventCalendarService::new(settings.event_calendar.clone()));
            ec.refresh().await;
            Some(ec)
        } else {
            None
        };

        let mut strategies: Vec<Box<dyn pa_core::traits::Strategy>> = Vec::new();

        if enabled_strategies.contains(&"weather".to_string()) {
            let weather_cache = market_data.cache().clone();
            let rm_pos = Arc::clone(&ctx.risk_manager_impl);
            let rm_held = Arc::clone(&ctx.risk_manager_impl);
            let weather_strategy = pa_strategy::weather::WeatherAlphaStrategy::new(
                settings.weather.clone(),
                dec!(0.00),
                pa_strategy::weather::WeatherAlphaDeps {
                    get_orderbook: Box::new(move |token_id| weather_cache.get(&token_id)),
                    get_available_capital: make_capital_fn(Arc::clone(&ctx.usdc_balance)),
                    get_position: Box::new(move |tid: alloy::primitives::U256| {
                        rm_pos.get_position_size(&tid)
                    }),
                    get_held_positions: Box::new(move || {
                        rm_held.positions_by_strategy(pa_core::types::StrategyType::Weather)
                    }),
                    neg_risk_events: neg_risk_events.to_vec(),
                },
            );
            strategies.push(Box::new(weather_strategy));
        }

        if enabled_strategies.contains(&"crypto".to_string()) {
            let crypto_cache = market_data.cache().clone();
            let rm_pos_crypto = Arc::clone(&ctx.risk_manager_impl);
            let rm_held_crypto = Arc::clone(&ctx.risk_manager_impl);
            let crypto = pa_strategy::crypto_alpha::CryptoAlphaStrategy::new(
                settings.crypto_alpha.clone(),
                dec!(0.00),
                pa_strategy::crypto_alpha::CryptoAlphaDeps {
                    base_min_size_retention_ratio: settings.risk.min_size_retention_ratio,
                    base_max_slippage_bps: settings.risk.max_slippage_bps,
                    execution_quality_profit_weight: settings.risk.execution_quality_profit_weight,
                    execution_quality_size_weight: settings.risk.execution_quality_size_weight,
                    execution_quality_slippage_weight: settings
                        .risk
                        .execution_quality_slippage_weight,
                    get_orderbook: Box::new(move |token_id| crypto_cache.get(&token_id)),
                    get_available_capital: make_capital_fn(Arc::clone(&ctx.usdc_balance)),
                    get_position: Box::new(move |tid: alloy::primitives::U256| {
                        rm_pos_crypto.get_position_size(&tid)
                    }),
                    get_held_positions: Box::new(move || {
                        rm_held_crypto
                            .positions_by_strategy(pa_core::types::StrategyType::CryptoAlpha)
                    }),
                    get_balance: make_balance_fn(Arc::clone(&ctx.usdc_balance)),
                    neg_risk_events: neg_risk_events.to_vec(),
                    binary_event_groups: binary_event_groups.to_vec(),
                    event_calendar: event_calendar.clone(),
                },
            );
            strategies.push(Box::new(crypto));
        }

        if enabled_strategies.contains(&"smart_money".to_string()) {
            let sm_cache = market_data.cache().clone();
            let rm_pos_sm = Arc::clone(&ctx.risk_manager_impl);
            let rm_held_sm = Arc::clone(&ctx.risk_manager_impl);

            let sm_markets_snapshot = shared_markets.read().await;
            let sm_token_to_cid: Arc<
                std::sync::RwLock<
                    std::collections::HashMap<alloy::primitives::U256, alloy::primitives::B256>,
                >,
            > = Arc::new(std::sync::RwLock::new(
                sm_markets_snapshot
                    .iter()
                    .flat_map(|m| m.tokens.iter().map(|t| (t.token_id, m.condition_id)))
                    .collect(),
            ));

            let sm_markets: Arc<
                std::sync::RwLock<
                    std::collections::HashMap<alloy::primitives::B256, pa_core::types::MarketInfo>,
                >,
            > = Arc::new(std::sync::RwLock::new(
                sm_markets_snapshot
                    .iter()
                    .map(|m| (m.condition_id, m.clone()))
                    .collect(),
            ));
            drop(sm_markets_snapshot);

            let tracker = pa_market_data::wallet_tracker::WalletTracker::new(
                settings.smart_money.clone(),
                Arc::clone(&sm_token_to_cid),
            );
            smart_money_token_map = Some(Arc::clone(&sm_token_to_cid));
            let sm_signals = tracker.signals_ref();

            let smart_money = pa_strategy::smart_money::SmartMoneyStrategy::new(
                settings.smart_money.clone(),
                dec!(0.00),
                pa_strategy::smart_money::SmartMoneyStrategyDeps {
                    get_orderbook: Box::new(move |token_id| sm_cache.get(&token_id)),
                    get_available_capital: make_capital_fn(Arc::clone(&ctx.usdc_balance)),
                    get_position: Box::new(move |tid: alloy::primitives::U256| {
                        rm_pos_sm.get_position_size(&tid)
                    }),
                    get_held_positions: Box::new(move || {
                        rm_held_sm.positions_by_strategy(pa_core::types::StrategyType::SmartMoney)
                    }),
                    signals: sm_signals,
                    markets: Arc::clone(&sm_markets),
                },
            );
            strategies.push(Box::new(smart_money));

            let tracker_cancel = cancel.clone();
            let sm_rpc_url = settings.chain.rpc_url.clone();
            tokio::spawn(async move {
                tracker.run(tracker_cancel, &sm_rpc_url).await;
            });
        }

        if !strategies.is_empty() {
            let engine_cache = market_data.cache().clone();
            let engine_rm_all = Arc::clone(&ctx.risk_manager_impl);
            let engine = StrategyEngine::new(
                strategies,
                ctx.executor.clone(),
                ctx.risk_manager.clone(),
                pa_strategy::engine::StrategyEngineDeps {
                    get_orderbook: Box::new(move |token_id| engine_cache.get(&token_id)),
                    get_available_capital: make_capital_fn(Arc::clone(&ctx.usdc_balance)),
                    get_all_positions: Box::new(move || {
                        engine_rm_all
                            .snapshot_positions()
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
                },
                pa_strategy::engine::StrategyEngineOptions {
                    scan_interval_ms: settings.strategy.scan_interval_ms,
                    event_calendar,
                    min_order_usdc: settings.risk.min_order_usdc,
                    max_market_end_days: settings.strategy.max_market_end_days,
                    max_slippage_bps: settings.risk.max_slippage_bps,
                    min_profit_retention_ratio: settings.risk.min_profit_retention_ratio,
                    min_size_retention_ratio: settings.risk.min_size_retention_ratio,
                    execution_quality_profit_weight: settings.risk.execution_quality_profit_weight,
                    execution_quality_size_weight: settings.risk.execution_quality_size_weight,
                    execution_quality_slippage_weight: settings
                        .risk
                        .execution_quality_slippage_weight,
                },
            );

            let engine_shared = Arc::clone(shared_markets);
            let engine_cancel = cancel.clone();
            let update_rx = market_data.ws_feed().await.subscribe_updates();
            let name = acct_name.clone();
            engine_handle = Some(tokio::spawn(async move {
                tracing::info!(account = %name, "Strategy engine started");
                engine.run(engine_shared, update_rx, engine_cancel).await;
            }));

            tracing::info!(
                account = %acct_name,
                strategies = ?enabled_strategies,
                trading = ctx.trading_enabled,
                "Strategy engine initialized"
            );
        }
    }

    if acct_strategies.contains(&"liquidity_rewards".to_string())
        && settings.liquidity_rewards.enabled
        && ctx.trading_enabled
    {
        spawn_liquidity_rewards_task(
            acct_name.clone(),
            settings.liquidity_rewards.clone(),
            market_data.cache().clone(),
            cancel.clone(),
            Arc::clone(&ctx.risk_manager_impl),
            Arc::clone(shared_markets),
            ctx.private_key.clone(),
            settings.clob.host.clone(),
            ctx.signature_type,
            ctx.chain_id,
            market_data.ws_feed().await.subscribe_updates(),
            lr_runtime_status,
        );
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

    spawn_auto_redeem(
        acct_name.clone(),
        ctx.private_key.clone(),
        ctx.chain_id,
        settings.chain.rpc_url.clone(),
        ctx.proxy_addr,
        ctx.signature_type,
        cancel.clone(),
    )
    .await;

    spawn_position_sync(
        acct_name.clone(),
        ctx.proxy_addr,
        Arc::clone(&ctx.risk_manager_impl),
        Arc::clone(shared_markets),
        neg_risk_events.to_vec(),
        cancel.clone(),
    );

    spawn_daily_reset(
        Arc::clone(&ctx.risk_manager) as Arc<dyn pa_core::traits::RiskManager>,
        cancel,
    );

    tracing::info!(account = %acct_name, "All tasks started for account");
    AccountRuntimeArtifacts {
        engine_handle,
        smart_money_token_map,
    }
}
