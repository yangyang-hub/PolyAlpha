//! Account construction and initial reconciliation.
//!
//! This module builds `AccountContext` values by loading credentials,
//! authenticating executors, pulling balances and positions, and seeding the
//! risk manager with the starting state for each configured account.

use std::collections::HashSet;
use std::str::FromStr;
use std::sync::Arc;

use alloy::signers::Signer as _;
use alloy::signers::local::PrivateKeySigner;
use anyhow::{Context, Result};
use arc_swap::ArcSwap;
use async_trait::async_trait;
use rust_decimal::Decimal;

use pa_core::config::Settings;
use pa_core::traits::RiskManager as _;
use pa_execution::clob_executor::ClobExecutor;
use pa_execution::ctf_executor::CtfExecutor;
use pa_execution::orchestrator::HybridOrchestrator;
use pa_market_data::data_api::PositionLoader;
use pa_market_data::service::MarketDataService;
use pa_risk::manager::RiskManagerImpl;

use crate::app::helpers::{infer_strategy_type, seed_market_cache};
use crate::app::types::AccountContext;

/// No-op executor used when CLOB authentication fails (observe-only mode).
struct DryRunExecutor;

#[async_trait]
impl pa_core::traits::Executor for DryRunExecutor {
    async fn execute(
        &self,
        opportunity: &pa_core::types::TradingOpportunity,
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

pub async fn build_account_contexts(
    settings: &Settings,
    resolved_accounts: &[pa_core::config::AccountConfig],
    market_data: &Arc<MarketDataService>,
    shared_markets: &Arc<tokio::sync::RwLock<Vec<pa_core::types::MarketInfo>>>,
    neg_risk_events: &[pa_core::types::NegRiskEvent],
) -> Result<Vec<AccountContext>> {
    let mut account_contexts = Vec::new();

    for acct_config in resolved_accounts {
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
            acct_config
                .proxy_wallet
                .parse::<alloy::primitives::Address>()
                .context(format!(
                    "Invalid proxy_wallet for account {}",
                    acct_config.name
                ))?
        };

        tracing::info!(
            account = %acct_config.name,
            address = %wallet_address,
            proxy = %proxy_addr,
            strategies = ?acct_config.strategies,
            "Account loaded"
        );

        let clob =
            match ClobExecutor::connect(&settings.clob.host, signer, acct_config.signature_type)
                .await
            {
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
                .context(format!(
                    "Failed to connect RPC for account {}",
                    acct_config.name
                ))?;
            let ctf =
                CtfExecutor::with_neg_risk(provider, settings.chain.chain_id).context(format!(
                    "Failed to create CTF executor for account {}",
                    acct_config.name
                ))?;
            Arc::new(HybridOrchestrator::new(clob, ctf))
        } else {
            Arc::new(DryRunExecutor)
        };

        let usdc_balance: Arc<ArcSwap<Decimal>> = Arc::new(ArcSwap::from_pointee(Decimal::ZERO));
        match executor.get_balance().await {
            Ok(bal) => {
                usdc_balance.store(Arc::new(bal));
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

        let risk_manager_impl = Arc::new(RiskManagerImpl::new(settings.risk.clone()));

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

        if !api_positions.is_empty() {
            let markets_snapshot = shared_markets.read().await;
            let known_condition_ids: HashSet<_> =
                markets_snapshot.iter().map(|m| m.condition_id).collect();
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
                let position_markets = market_data.fetch_position_markets(&missing_condition_ids).await;
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

        let markets_snapshot = shared_markets.read().await;
        let initial_positions: Vec<_> = api_positions
            .iter()
            .map(|p| {
                let strategy_type =
                    infer_strategy_type(p.token_id, &markets_snapshot, neg_risk_events);
                if strategy_type.is_some() {
                    tracing::debug!(
                        account = %acct_config.name,
                        token_id = %p.token_id,
                        strategy = ?strategy_type,
                        size = %p.size,
                        "Position tagged"
                    );
                } else {
                    let question = markets_snapshot
                        .iter()
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
                (
                    p.token_id,
                    p.size,
                    p.avg_price,
                    strategy_type,
                    Some(p.condition_id),
                )
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

    Ok(account_contexts)
}
