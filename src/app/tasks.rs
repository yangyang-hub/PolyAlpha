//! Shared background task implementations.
//!
//! These tasks are reused across accounts or across the whole process and are
//! intentionally separated from startup orchestration.

use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use alloy::primitives::{B256, U256};
use alloy::providers::ProviderBuilder;
use alloy::signers::Signer as _;
use alloy::signers::local::PrivateKeySigner;
use arc_swap::ArcSwap;
use chrono::{NaiveDate, Utc};
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal_macros::dec;
use tokio_util::sync::CancellationToken;

use pa_core::config::{DatabaseConfig, WeatherConfig};
use pa_core::traits::{Executor, MarketDataFeed, RiskManager as _};
use pa_core::types::{MarketInfo, NegRiskEvent};
use pa_core::weather::{WEATHER_LOCATIONS, WeatherProvider};
use pa_execution::ctf_executor::CtfExecutor;
use pa_execution::safe_redeemer::SafeRedeemer;
use pa_market_data::cache::OrderBookCache;
use pa_market_data::data_api::PositionLoader;
use pa_market_data::service::MarketDataService;
use pa_risk::manager::RiskManagerImpl;
use pa_storage::models::WeatherForecastSnapshotRow;
use pa_storage::repository::Repository;
use pa_strategy::weather::{
    KmaClient, MetOfficeClient, NoaaClient, OpenMeteoClient, WeatherMetric,
};

use crate::app::helpers::{build_ws_token_list, infer_strategy_type, seed_market_cache};
use crate::app::types::AccountContext;

pub fn spawn_balance_refresh(
    account_name: String,
    executor: Arc<dyn Executor>,
    balance_state: Arc<ArcSwap<Decimal>>,
    cancel: CancellationToken,
) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(30));
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = interval.tick() => {
                    match executor.get_balance().await {
                        Ok(bal) => {
                            let prev = **balance_state.load();
                            if bal != prev {
                                tracing::info!(
                                    account = %account_name,
                                    balance_usdc = %bal,
                                    prev = %prev,
                                    "USDC balance updated"
                                );
                            }
                            balance_state.store(Arc::new(bal));
                        }
                        Err(e) => {
                            tracing::debug!(
                                account = %account_name,
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

pub fn spawn_daily_reset(
    risk_manager: Arc<dyn pa_core::traits::RiskManager>,
    cancel: CancellationToken,
) {
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
                _ = cancel.cancelled() => break,
                _ = tokio::time::sleep(until_midnight) => {
                    risk_manager.reset_daily();
                    tracing::info!("Daily risk counters reset at midnight UTC");
                }
            }
        }
    });
}

pub fn spawn_positions_snapshot_refresh(
    account_contexts: &[AccountContext],
    shared_markets: Arc<tokio::sync::RwLock<Vec<pa_core::types::MarketInfo>>>,
    cache: OrderBookCache,
    shared_positions: Arc<tokio::sync::RwLock<Vec<pa_monitor::api::PositionApiEntry>>>,
    shared_positions_updated_at: Arc<tokio::sync::RwLock<Option<chrono::DateTime<Utc>>>>,
    wallet_balance: Arc<tokio::sync::RwLock<Decimal>>,
    cancel: CancellationToken,
) {
    let risk_managers: Vec<Arc<RiskManagerImpl>> = account_contexts
        .iter()
        .map(|ctx| Arc::clone(&ctx.risk_manager_impl))
        .collect();
    let balances: Vec<Arc<ArcSwap<Decimal>>> = account_contexts
        .iter()
        .map(|ctx| Arc::clone(&ctx.usdc_balance))
        .collect();

    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(30));
        interval.tick().await;
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = interval.tick() => {
                    let markets_snapshot = shared_markets.read().await;
                    let entries: Vec<pa_monitor::api::PositionApiEntry> = {
                        let mut all = Vec::new();
                        for rm in &risk_managers {
                            for (token_id, pe) in rm.snapshot_positions() {
                                if pe.size < dec!(0.1) {
                                    continue;
                                }
                                let (question, outcome, _cid, event_title) = markets_snapshot.iter()
                                    .find_map(|m| {
                                        m.tokens.iter().find(|t| t.token_id == token_id).map(|t| {
                                            let o = match t.outcome {
                                                pa_core::types::Outcome::Yes => "YES",
                                                pa_core::types::Outcome::No => "NO",
                                            };
                                            (m.question.as_str(), o, m.condition_id, m.event_title.as_deref())
                                        })
                                    })
                                    .unwrap_or(("", "", alloy::primitives::B256::ZERO, None));
                                let asset = pa_strategy::crypto_alpha::parse_crypto_question(question)
                                    .map(|parsed| parsed.asset.name.to_string())
                                    .or_else(|| {
                                        event_title
                                            .and_then(pa_strategy::crypto_alpha::parse_crypto_event_title)
                                            .map(|(asset, _)| asset.name.to_string())
                                    });
                                let direction = if outcome.is_empty() {
                                    None
                                } else {
                                    pa_strategy::crypto_alpha::infer_crypto_direction_label(
                                        question,
                                        Some(outcome),
                                    )
                                    .map(str::to_string)
                                };

                                let current_price = cache.get(&token_id)
                                    .and_then(|ob| ob.bids.first().map(|b| b.price));
                                let unrealized_pnl = current_price.map(|p| pe.size * (p - pe.avg_cost));

                                let strategy_name = pe.strategy_type.map(|st| match st {
                                    pa_core::types::StrategyType::Weather => "weather",
                                    pa_core::types::StrategyType::CryptoAlpha => "crypto_alpha",
                                    pa_core::types::StrategyType::LiquidityRewards => "liquidity_rewards",
                                    pa_core::types::StrategyType::SmartMoney => "smart_money",
                                });

                                all.push(pa_monitor::api::PositionApiEntry {
                                    token_id: format!("{:#x}", token_id),
                                    size: pe.size,
                                    avg_cost: pe.avg_cost,
                                    cost_basis: pe.size * pe.avg_cost,
                                    strategy: strategy_name.map(|s| s.to_string()),
                                    asset,
                                    direction,
                                    condition_id: pe.condition_id.map(|c| format!("{:#x}", c)),
                                    question: if question.is_empty() { None } else { Some(question.to_string()) },
                                    outcome: if outcome.is_empty() { None } else { Some(outcome.to_string()) },
                                    current_price,
                                    unrealized_pnl,
                                });
                            }
                        }
                        all
                    };
                    *shared_positions.write().await = entries;
                    *shared_positions_updated_at.write().await = Some(Utc::now());

                    let total_bal: Decimal = balances.iter().map(|b| **b.load()).sum();
                    *wallet_balance.write().await = total_bal;
                    let total_exp: Decimal = risk_managers.iter().map(|rm| rm.total_exposure()).sum();
                    let market_value: Decimal = {
                        let positions = shared_positions.read().await;
                        positions.iter()
                            .filter_map(|p| p.current_price.map(|cp| p.size * cp))
                            .sum()
                    };
                    pa_monitor::metrics::USDC_BALANCE.set(total_bal.to_f64().unwrap_or(0.0));
                    pa_monitor::metrics::TOTAL_EXPOSURE.set(total_exp.to_f64().unwrap_or(0.0));
                    pa_monitor::metrics::POSITIONS_MARKET_VALUE.set(market_value.to_f64().unwrap_or(0.0));
                }
            }
        }
    });
}

pub fn spawn_weather_forecast_snapshot_refresh(
    weather_config: WeatherConfig,
    database_config: DatabaseConfig,
    cancel: CancellationToken,
) {
    if database_config.url.trim().is_empty() {
        tracing::info!("Weather forecast snapshot archive disabled — no database URL configured");
        return;
    }

    tokio::spawn(async move {
        let repo = match Repository::connect(&database_config.url, database_config.max_connections)
            .await
        {
            Ok(repo) => repo,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "Weather forecast snapshot archive disabled — failed to connect database"
                );
                return;
            }
        };
        if let Err(e) = repo.migrate().await {
            tracing::warn!(
                error = %e,
                "Weather forecast snapshot archive disabled — failed to apply migrations"
            );
            return;
        }

        let noaa = NoaaClient::new(&weather_config.noaa_user_agent);
        let open_meteo = OpenMeteoClient::new();
        let kma = KmaClient::new(&weather_config.kma_api_key);
        let met_office = MetOfficeClient::new(
            &weather_config.met_office_api_key,
            &weather_config.met_office_obs_api_key,
        );
        let mut interval = tokio::time::interval(Duration::from_secs(1800));

        loop {
            let mut written = 0u32;
            for location in WEATHER_LOCATIONS
                .iter()
                .filter(|entry| !entry.trade_enabled)
            {
                for metric in [
                    WeatherMetric::TemperatureMax,
                    WeatherMetric::TemperatureMin,
                    WeatherMetric::TemperatureAvg,
                ] {
                    match fetch_snapshot_forecast(
                        &noaa,
                        &open_meteo,
                        &kma,
                        &met_office,
                        location.canonical_name,
                        metric,
                    )
                    .await
                    {
                        Ok(snapshot) => {
                            for row in snapshot_rows(
                                location.provider,
                                location.canonical_name,
                                metric,
                                &snapshot,
                            ) {
                                match repo.insert_weather_forecast_snapshot(&row).await {
                                    Ok(()) => written += 1,
                                    Err(e) => {
                                        tracing::debug!(
                                            location = location.canonical_name,
                                            metric = weather_metric_name(metric),
                                            error = %e,
                                            "Failed to persist weather forecast snapshot"
                                        );
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            tracing::debug!(
                                location = location.canonical_name,
                                provider = weather_provider_name(location.provider),
                                metric = weather_metric_name(metric),
                                error = %e,
                                "Weather snapshot refresh skipped"
                            );
                        }
                    }
                }
            }

            if written > 0 {
                tracing::info!(rows = written, "Weather forecast snapshots archived");
            }

            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = interval.tick() => {}
            }
        }
    });
}

fn weather_metric_name(metric: WeatherMetric) -> &'static str {
    match metric {
        WeatherMetric::TemperatureMax => "temp_max",
        WeatherMetric::TemperatureMin => "temp_min",
        WeatherMetric::TemperatureAvg => "temp_avg",
        WeatherMetric::Rainfall => "rainfall",
        WeatherMetric::Snowfall => "snowfall",
        WeatherMetric::WindSpeed => "wind_speed",
    }
}

fn weather_provider_name(provider: WeatherProvider) -> &'static str {
    match provider {
        WeatherProvider::Noaa => "noaa",
        WeatherProvider::OpenMeteo => "open_meteo",
        WeatherProvider::Kma => "kma",
        WeatherProvider::MetOffice => "met_office",
    }
}

async fn fetch_snapshot_forecast(
    noaa: &NoaaClient,
    open_meteo: &OpenMeteoClient,
    kma: &KmaClient,
    met_office: &MetOfficeClient,
    location: &str,
    metric: WeatherMetric,
) -> anyhow::Result<pa_strategy::weather::ForecastData> {
    match pa_core::weather::weather_location(location)
        .map(|entry| entry.provider)
        .ok_or_else(|| anyhow::anyhow!("Unsupported weather snapshot location: {}", location))?
    {
        WeatherProvider::Noaa => {
            let (lat, lon) = NoaaClient::geocode(location)?;
            noaa.forecast(lat, lon, metric, None, "inch").await
        }
        WeatherProvider::OpenMeteo => {
            let (lat, lon) = OpenMeteoClient::geocode(location)?;
            open_meteo
                .forecast(lat, lon, location, metric, None, "inch")
                .await
        }
        WeatherProvider::Kma => kma.forecast(location, metric, None, "inch").await,
        WeatherProvider::MetOffice => met_office.forecast(location, metric, None, "inch").await,
    }
}

fn snapshot_rows(
    provider: WeatherProvider,
    location: &str,
    metric: WeatherMetric,
    snapshot: &pa_strategy::weather::ForecastData,
) -> Vec<WeatherForecastSnapshotRow> {
    let recorded_at = Utc::now();
    let provider = weather_provider_name(provider).to_string();
    let location = location.to_string();
    let metric_name = weather_metric_name(metric).to_string();
    let values_json = serde_json::to_value(&snapshot.values).unwrap_or(serde_json::Value::Null);
    let dates_json = serde_json::to_value(&snapshot.dates).unwrap_or(serde_json::Value::Null);

    snapshot
        .dates
        .iter()
        .zip(snapshot.values.iter())
        .filter_map(|(date, value)| {
            let target_date = NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?;
            Some(WeatherForecastSnapshotRow {
                id: 0,
                provider: provider.clone(),
                location: location.clone(),
                metric: metric_name.clone(),
                target_date,
                recorded_at,
                target_value: Some(*value),
                mean: snapshot.mean,
                std_dev: snapshot.std_dev,
                model_spread: snapshot.model_spread,
                values: values_json.clone(),
                dates: dates_json.clone(),
            })
        })
        .collect()
}

pub fn spawn_position_sync(
    account_name: String,
    proxy_addr: alloy::primitives::Address,
    risk_manager: Arc<RiskManagerImpl>,
    shared_markets: Arc<tokio::sync::RwLock<Vec<MarketInfo>>>,
    neg_risk_events: Vec<NegRiskEvent>,
    cancel: CancellationToken,
) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(300));
        interval.tick().await;
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = interval.tick() => {
                    let loader = match PositionLoader::new(proxy_addr) {
                        Ok(l) => l,
                        Err(e) => {
                            tracing::warn!(account = %account_name, error = %e, "Position sync: failed to create loader");
                            continue;
                        }
                    };
                    let api_positions = match loader.load_positions().await {
                        Ok(p) => p,
                        Err(e) => {
                            tracing::warn!(account = %account_name, error = %e, "Position sync: Data API fetch failed");
                            continue;
                        }
                    };

                    let current = risk_manager.snapshot_positions();
                    let known: HashMap<U256, Decimal> =
                        current.iter().map(|(tid, e)| (*tid, e.size)).collect();

                    let markets_snapshot = shared_markets.read().await;
                    let mut added = 0u32;
                    let mut updated = 0u32;
                    let mut retagged = 0u32;
                    for pos in &api_positions {
                        let existing_size =
                            known.get(&pos.token_id).copied().unwrap_or(Decimal::ZERO);

                        if existing_size == pos.size && existing_size > Decimal::ZERO {
                            let needs_retag = current.iter().any(|(tid, entry)| {
                                *tid == pos.token_id && entry.strategy_type.is_none()
                            });
                            if needs_retag {
                                let strategy_type =
                                    infer_strategy_type(pos.token_id, &markets_snapshot, &neg_risk_events);
                                if strategy_type.is_some() {
                                    risk_manager.sync_position(
                                        pos.token_id,
                                        pos.size,
                                        pos.avg_price,
                                        strategy_type,
                                        Some(pos.condition_id),
                                    );
                                    tracing::info!(
                                        account = %account_name,
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
                            let strategy_type =
                                infer_strategy_type(pos.token_id, &markets_snapshot, &neg_risk_events);
                            risk_manager.sync_position(
                                pos.token_id,
                                pos.size,
                                pos.avg_price,
                                strategy_type,
                                Some(pos.condition_id),
                            );
                            tracing::info!(
                                account = %account_name,
                                token_id = %pos.token_id,
                                size = %pos.size,
                                strategy = ?strategy_type,
                                "Position sync: discovered missing position"
                            );
                            added += 1;
                        } else if pos.size > Decimal::ZERO
                            && existing_size > Decimal::ZERO
                            && pos.size != existing_size
                        {
                            let strategy_type =
                                infer_strategy_type(pos.token_id, &markets_snapshot, &neg_risk_events);
                            risk_manager.sync_position(
                                pos.token_id,
                                pos.size,
                                pos.avg_price,
                                strategy_type,
                                Some(pos.condition_id),
                            );
                            tracing::info!(
                                account = %account_name,
                                token_id = %pos.token_id,
                                prev_size = %existing_size,
                                new_size = %pos.size,
                                "Position sync: size changed"
                            );
                            updated += 1;
                        } else if pos.size == Decimal::ZERO && existing_size > Decimal::ZERO {
                            risk_manager.sync_position(
                                pos.token_id,
                                Decimal::ZERO,
                                Decimal::ZERO,
                                None,
                                Some(pos.condition_id),
                            );
                            tracing::info!(
                                account = %account_name,
                                token_id = %pos.token_id,
                                prev_size = %existing_size,
                                "Position sync: cleared stale position"
                            );
                            updated += 1;
                        }
                    }
                    let api_tokens: HashSet<U256> = api_positions.iter().map(|p| p.token_id).collect();
                    for (tid, entry) in &current {
                        if entry.size > Decimal::ZERO && !api_tokens.contains(tid) {
                            risk_manager.sync_position(
                                *tid,
                                Decimal::ZERO,
                                Decimal::ZERO,
                                None,
                                entry.condition_id,
                            );
                            tracing::info!(
                                account = %account_name,
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
                            account = %account_name,
                            added,
                            updated,
                            retagged,
                            total_api = api_positions.len(),
                            "Position sync complete"
                        );
                    }
                }
            }
        }
    });
}

pub fn spawn_market_refresh(
    refresh_interval_secs: u64,
    shared_markets: Arc<tokio::sync::RwLock<Vec<MarketInfo>>>,
    market_data: Arc<MarketDataService>,
    active_enabled_strategies: Vec<String>,
    ws_max_instruments: usize,
    risk_managers: Vec<Arc<RiskManagerImpl>>,
    smart_money_token_maps: Vec<Arc<std::sync::RwLock<HashMap<U256, B256>>>>,
    cancel: CancellationToken,
) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(refresh_interval_secs));
        interval.tick().await;

        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = interval.tick() => {
                    tracing::debug!("Periodic market refresh starting...");
                    match market_data.discover_markets().await {
                        Ok(new_all) => {
                            let mut current = shared_markets.write().await;
                            let old_ids: HashSet<B256> =
                                current.iter().map(|m| m.condition_id).collect();

                            let mut added = 0u32;
                            let refresh_cache = market_data.cache().as_ref().clone();
                            for m in new_all {
                                if !old_ids.contains(&m.condition_id) {
                                    for token in &m.tokens {
                                        for token_map in &smart_money_token_maps {
                                            token_map
                                                .write()
                                                .unwrap()
                                                .insert(token.token_id, m.condition_id);
                                        }
                                    }
                                    seed_market_cache(&refresh_cache, &m);
                                    current.push(m);
                                    added += 1;
                                }
                            }

                            if added > 0 {
                                tracing::info!(added, total = current.len(), "New markets discovered");
                                pa_monitor::metrics::MONITORED_MARKETS.set(current.len() as f64);

                                let mut held_tokens: Vec<U256> = Vec::new();
                                for rm in &risk_managers {
                                    for (tid, _) in rm.snapshot_positions() {
                                        if !held_tokens.contains(&tid) {
                                            held_tokens.push(tid);
                                        }
                                    }
                                }
                                let token_ids = build_ws_token_list(
                                    &current,
                                    &held_tokens,
                                    &active_enabled_strategies,
                                    ws_max_instruments,
                                );
                                drop(current);
                                if let Err(e) = market_data.resubscribe(&token_ids).await {
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
}

pub async fn spawn_auto_redeem(
    account_name: String,
    private_key: String,
    chain_id: u64,
    rpc_url: String,
    proxy_addr: alloy::primitives::Address,
    signature_type: u8,
    cancel: CancellationToken,
) {
    let redeem_signer = match PrivateKeySigner::from_str(&private_key) {
        Ok(s) => s.with_chain_id(Some(chain_id)),
        Err(e) => {
            tracing::warn!(
                account = %account_name,
                error = %e,
                "Invalid private key for redeem signer, skipping auto-redeem"
            );
            return;
        }
    };
    let redeem_provider = match ProviderBuilder::new()
        .wallet(redeem_signer.clone())
        .connect(&rpc_url)
        .await
    {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(
                account = %account_name,
                error = %e,
                "Failed to connect RPC for redeem, skipping auto-redeem"
            );
            return;
        }
    };

    if signature_type == 2 {
        let safe_redeemer = SafeRedeemer::new(redeem_provider, redeem_signer, proxy_addr);
        match safe_redeemer.verify_ownership().await {
            Ok(true) => {
                tracing::info!(account = %account_name, safe = %proxy_addr, "SafeRedeemer: EOA is Safe owner");
            }
            Ok(false) => {
                tracing::warn!(account = %account_name, safe = %proxy_addr, "SafeRedeemer: EOA is NOT a Safe owner, skipping auto-redeem");
                return;
            }
            Err(e) => {
                tracing::warn!(account = %account_name, error = %e, "SafeRedeemer: could not verify ownership, skipping auto-redeem");
                return;
            }
        }

        let redeem_name = account_name.clone();
        tokio::spawn(async move {
            let redeem_loader = match PositionLoader::new(proxy_addr) {
                Ok(l) => l,
                Err(e) => {
                    tracing::error!(account = %redeem_name, error = %e, "Failed to create redeem position loader");
                    return;
                }
            };
            let mut interval = tokio::time::interval(Duration::from_secs(300));
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => break,
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
                                        let amount_raw = pos.size * Decimal::from(1_000_000u64);
                                        let amount =
                                            U256::from(amount_raw.to_u64().unwrap_or(0));
                                        let amounts = if pos.outcome_index == 0 {
                                            vec![amount, U256::ZERO]
                                        } else {
                                            vec![U256::ZERO, amount]
                                        };
                                        safe_redeemer.redeem_neg_risk(pos.condition_id, amounts).await
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
        tracing::info!(account = %account_name, "Auto-redeem task started (GnosisSafe)");
    } else {
        let ctf = match CtfExecutor::with_neg_risk(redeem_provider, chain_id) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(account = %account_name, error = %e, "Failed to create CtfExecutor for redeem, skipping auto-redeem");
                return;
            }
        };

        let redeem_name = account_name.clone();
        tokio::spawn(async move {
            let redeem_loader = match PositionLoader::new(proxy_addr) {
                Ok(l) => l,
                Err(e) => {
                    tracing::error!(account = %redeem_name, error = %e, "Failed to create redeem position loader");
                    return;
                }
            };
            let mut interval = tokio::time::interval(Duration::from_secs(300));
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => break,
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
                                        let amount_raw = pos.size * Decimal::from(1_000_000u64);
                                        let amount =
                                            U256::from(amount_raw.to_u64().unwrap_or(0));
                                        let amounts = if pos.outcome_index == 0 {
                                            vec![amount, U256::ZERO]
                                        } else {
                                            vec![U256::ZERO, amount]
                                        };
                                        ctf.redeem_neg_risk(pos.condition_id, amounts).await
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
        tracing::info!(account = %account_name, "Auto-redeem task started (direct CTF)");
    }
}
