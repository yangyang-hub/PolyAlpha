//! Small shared runtime types used across `app` modules.

use std::sync::Arc;

use alloy::primitives::Address;
use arc_swap::ArcSwap;
use rust_decimal::Decimal;

use pa_core::traits::{Executor, RiskManager};
use pa_risk::manager::RiskManagerImpl;

/// Metadata for a tracked LR order, used by fill detection to sync positions.
#[derive(Clone, Debug)]
pub struct LrOrderMeta {
    pub token_id: alloy::primitives::U256,
    pub is_buy: bool,
    pub price: Decimal,
    pub size: Decimal,
    /// Cumulative size_matched already synced to RiskManager.
    /// Used to compute delta on partial fills: new_fill = api.size_matched - last_synced.
    pub last_synced_matched: Decimal,
}

/// Per-account runtime context bundling all account-specific resources.
pub struct AccountContext {
    pub name: String,
    pub trading_enabled: bool,
    pub executor: Arc<dyn Executor>,
    pub risk_manager_impl: Arc<RiskManagerImpl>,
    pub risk_manager: Arc<dyn RiskManager>,
    pub usdc_balance: Arc<ArcSwap<Decimal>>,
    pub proxy_addr: Address,
    pub private_key: String,
    pub signature_type: u8,
    pub chain_id: u64,
    /// Strategies assigned to this account (e.g., ["weather", "crypto", "liquidity_rewards"]).
    pub strategies: Vec<String>,
}

pub type ClobRewards = Vec<pa_strategy::liquidity_rewards::ClobRewardData>;
pub type LrQuoteResult = (
    Vec<(String, LrOrderMeta)>,
    Decimal,
    Option<Decimal>,
    Option<Decimal>,
);

pub type LrCooldownMap =
    std::collections::HashMap<(alloy::primitives::U256, bool, Decimal), std::time::Instant>;
