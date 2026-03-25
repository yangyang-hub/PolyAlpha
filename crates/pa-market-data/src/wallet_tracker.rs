use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use alloy::primitives::{B256, U256};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use tokio_util::sync::CancellationToken;

use pa_core::config::SmartMoneyConfig;
use pa_monitor::diagnostics::{SmartMoneyWalletScoreEntry, record_smart_money_wallet_scores};

// ──── On-chain Constants ────

/// ConditionalTokens contract address on Polygon.
const CT_ADDRESS: &str = "0x4D97DCd97eC945f40cF65F87097ACe5EA0476045";

/// TransferSingle event signature: keccak256("TransferSingle(address,address,address,uint256,uint256)")
const TRANSFER_SINGLE_TOPIC: &str =
    "0xc3d58168c5ae7397731d063d5bbf3d657854427343f4c083240f7aacaa2d0f62";

// ──── Signal Types ────

/// Signal emitted when a tracked wallet's position changes.
#[derive(Debug, Clone)]
pub struct SmartMoneySignal {
    pub signal_type: SignalType,
    pub wallet_address: String,
    pub wallet_label: Option<String>,
    pub wallet_weight: Decimal,
    pub token_id: U256,
    pub condition_id: B256,
    /// Wallet's NEW position size (after the change).
    pub wallet_size: Decimal,
    /// Change amount (positive for increase, full size for new entry).
    pub delta: Decimal,
    /// Approximate signal notional used for first-pass de-noising.
    pub signal_notional_usdc: Decimal,
    pub source: SmartMoneySignalSource,
    pub detected_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SignalType {
    /// New position (wasn't there before).
    Entry,
    /// Position size increased.
    Increase,
    /// Position size decreased (partial exit).
    Decrease,
    /// Position fully closed.
    Exit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SmartMoneySignalSource {
    DataApi,
    Onchain,
}

/// Position snapshot for a tracked wallet.
#[derive(Debug, Clone)]
pub struct WalletPosition {
    pub token_id: U256,
    pub size: Decimal,
    pub condition_id: B256,
}

/// Wallet profile from Data API (for auto-discovery scoring).
#[derive(Debug)]
struct WalletProfile {
    pnl: Decimal,
    volume: Decimal,
}

/// Raw position from Data API (wallet-level query).
#[derive(Debug, serde::Deserialize)]
struct RawWalletPosition {
    #[serde(rename = "asset")]
    pub asset: U256,
    #[serde(rename = "size")]
    pub size: Decimal,
    #[serde(rename = "conditionId")]
    pub condition_id: B256,
}

/// Raw profile activity from Data API.
#[derive(Debug, serde::Deserialize)]
struct RawProfileActivity {
    #[serde(default, rename = "profitLoss")]
    pub profit_loss: Decimal,
    #[serde(default, rename = "totalVolume")]
    pub total_volume: Decimal,
}

// ──── Tracked Wallet (runtime) ────

#[derive(Debug, Clone)]
struct TrackedWallet {
    address: String,
    label: String,
    base_weight: Decimal,
    effective_weight: Decimal,
    profile_score: Decimal,
    recent_signal_times: VecDeque<DateTime<Utc>>,
    /// Whether this wallet was auto-discovered (vs manually configured).
    auto_discovered: bool,
}

#[derive(Debug, Clone)]
struct RecentSignalMeta {
    detected_at: DateTime<Utc>,
    delta: Decimal,
}

// ──── WalletTracker ────

pub struct WalletTracker {
    config: SmartMoneyConfig,
    http_client: reqwest::Client,
    /// Current position snapshots per wallet.
    /// Key: lowercase address, Value: HashMap<token_id, WalletPosition>
    snapshots: Arc<RwLock<HashMap<String, HashMap<U256, WalletPosition>>>>,
    /// Pending signals to be consumed by the strategy.
    signals: Arc<RwLock<Vec<SmartMoneySignal>>>,
    /// Recent emitted signals used for deduplication.
    recent_signal_index: Arc<RwLock<HashMap<(String, U256, SignalType), RecentSignalMeta>>>,
    /// On-chain signals waiting for the next Data API snapshot to confirm the move.
    pending_onchain_signals: Arc<RwLock<HashMap<(String, U256, SignalType), SmartMoneySignal>>>,
    /// token_id → condition_id mapping for on-chain event resolution.
    token_to_condition: Arc<RwLock<HashMap<U256, B256>>>,
    /// Runtime wallet list (manual + auto-discovered).
    tracked_wallets: Arc<RwLock<Vec<TrackedWallet>>>,
}

impl WalletTracker {
    pub fn new(
        config: SmartMoneyConfig,
        token_to_condition: Arc<RwLock<HashMap<U256, B256>>>,
    ) -> Self {
        let http_client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap_or_default();

        // Seed tracked wallets from config
        let tracked_wallets: Vec<TrackedWallet> = config
            .wallets
            .iter()
            .map(|w| TrackedWallet {
                address: w.address.to_lowercase(),
                label: w.label.clone(),
                base_weight: w.weight,
                effective_weight: w.weight,
                profile_score: Decimal::ZERO,
                recent_signal_times: VecDeque::new(),
                auto_discovered: false,
            })
            .collect();

        tracing::info!(
            wallets = tracked_wallets.len(),
            "SmartMoney: WalletTracker initialized"
        );

        Self {
            config,
            http_client,
            snapshots: Arc::new(RwLock::new(HashMap::new())),
            signals: Arc::new(RwLock::new(Vec::new())),
            recent_signal_index: Arc::new(RwLock::new(HashMap::new())),
            pending_onchain_signals: Arc::new(RwLock::new(HashMap::new())),
            token_to_condition,
            tracked_wallets: Arc::new(RwLock::new(tracked_wallets)),
        }
    }

    /// Return a shared reference to the signal queue (for SmartMoneyStrategy).
    pub fn signals_ref(&self) -> Arc<RwLock<Vec<SmartMoneySignal>>> {
        Arc::clone(&self.signals)
    }

    /// Main run loop — spawned as a background task.
    pub async fn run(&self, cancel: CancellationToken, rpc_url: &str) {
        let poll_interval = Duration::from_secs(self.config.poll_interval_secs);
        let onchain_interval = Duration::from_secs(self.config.onchain_poll_secs);
        let discover_interval = Duration::from_secs(self.config.auto_discover_interval_secs);

        let mut poll_tick = tokio::time::interval(poll_interval);
        let mut onchain_tick = tokio::time::interval(onchain_interval);
        let mut discover_tick = tokio::time::interval(discover_interval);
        let mut last_block: u64 = 0;

        let rpc_url_owned = rpc_url.to_string();
        let onchain_enabled = self.config.onchain_enabled;

        if onchain_enabled {
            tracing::info!("SmartMoney: on-chain Transfer monitoring enabled");
        }

        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    tracing::info!("SmartMoney: WalletTracker shutting down");
                    break;
                }
                _ = poll_tick.tick() => {
                    if let Err(e) = self.poll_data_api().await {
                        tracing::warn!(error = %e, "SmartMoney: Data API poll failed");
                    }
                    self.prune_stale_signals();
                    self.prune_stale_pending_onchain_signals();
                    self.prune_recent_signal_index();
                }
                _ = onchain_tick.tick(), if onchain_enabled => {
                    match self.poll_onchain_logs(&rpc_url_owned, last_block).await {
                        Ok(new_block) => last_block = new_block,
                        Err(e) => tracing::warn!(error = %e, "SmartMoney: on-chain poll failed"),
                    }
                }
                _ = discover_tick.tick() => {
                    if let Err(e) = self.refresh_wallet_profiles().await {
                        tracing::warn!(error = %e, "SmartMoney: wallet score refresh failed");
                    }
                    if self.config.auto_discover_enabled {
                        if let Err(e) = self.auto_discover().await {
                            tracing::warn!(error = %e, "SmartMoney: auto-discovery failed");
                        }
                    }
                }
            }
        }
    }

    // ──── Data API Polling ────

    /// Poll all tracked wallets via Data API and emit signals on position changes.
    async fn poll_data_api(&self) -> Result<()> {
        let wallets = self.tracked_wallets.read().unwrap().clone();
        if wallets.is_empty() {
            return Ok(());
        }

        for wallet in &wallets {
            if let Ok(profile) = self.fetch_profile(&wallet.address).await {
                self.refresh_wallet_profile_score(&wallet.address, &profile);
            }
            let effective_weight = self.wallet_weight(&wallet.address);
            let new_positions = match self.fetch_wallet_positions(&wallet.address).await {
                Ok(p) => p,
                Err(e) => {
                    tracing::debug!(
                        wallet = %wallet.address,
                        error = %e,
                        "SmartMoney: failed to fetch positions"
                    );
                    continue;
                }
            };

            // Build new snapshot map
            let new_map: HashMap<U256, WalletPosition> =
                new_positions.into_iter().map(|p| (p.token_id, p)).collect();

            // Get old snapshot
            let old_map = {
                let snapshots = self.snapshots.read().unwrap();
                snapshots.get(&wallet.address).cloned().unwrap_or_default()
            };

            // Diff and emit signals
            let signals = diff_snapshots(
                &old_map,
                &new_map,
                &wallet.address,
                Some(wallet.label.as_str()),
                effective_weight,
            );

            if !signals.is_empty() {
                tracing::info!(
                    wallet = %wallet.label,
                    address = %wallet.address,
                    signals = signals.len(),
                    "SmartMoney: position changes detected"
                );
                for signal in signals {
                    self.confirm_or_enqueue_data_signal(signal);
                }
            }

            // Update snapshot
            {
                let mut snapshots = self.snapshots.write().unwrap();
                snapshots.insert(wallet.address.clone(), new_map);
            }
        }

        self.publish_wallet_score_snapshot();

        Ok(())
    }

    /// Fetch a wallet's positions from Polymarket Data API.
    async fn fetch_wallet_positions(&self, address: &str) -> Result<Vec<WalletPosition>> {
        let url = format!(
            "https://data-api.polymarket.com/positions?user={}&sizeThreshold=0",
            address
        );

        let resp: Vec<RawWalletPosition> = self
            .http_client
            .get(&url)
            .send()
            .await
            .context("SmartMoney Data API request failed")?
            .json()
            .await
            .context("SmartMoney Data API JSON parse failed")?;

        Ok(resp
            .into_iter()
            .filter(|p| p.size > Decimal::ZERO)
            .map(|p| WalletPosition {
                token_id: p.asset,
                size: p.size,
                condition_id: p.condition_id,
            })
            .collect())
    }

    // ──── On-chain Transfer Monitoring (raw JSON-RPC) ────

    /// Poll ConditionalTokens TransferSingle events via eth_getLogs JSON-RPC.
    async fn poll_onchain_logs(&self, rpc_url: &str, from_block: u64) -> Result<u64> {
        // Get latest block number
        let latest = self.rpc_block_number(rpc_url).await?;
        if from_block >= latest {
            return Ok(from_block);
        }

        let tracked: HashSet<String> = {
            let wallets = self.tracked_wallets.read().unwrap();
            wallets.iter().map(|w| w.address.clone()).collect()
        };

        if tracked.is_empty() {
            return Ok(latest);
        }

        // eth_getLogs with filter for TransferSingle events on ConditionalTokens
        let logs = self
            .rpc_get_logs(
                rpc_url,
                CT_ADDRESS,
                TRANSFER_SINGLE_TOPIC,
                from_block + 1,
                latest,
            )
            .await?;

        let token_to_cid = self.token_to_condition.read().unwrap();
        let mut signal_count = 0usize;

        for log in &logs {
            // TransferSingle has 3 indexed topics: event sig, operator, from, to
            // Plus data: id (uint256), value (uint256)
            let topics = match log.get("topics").and_then(|t| t.as_array()) {
                Some(t) if t.len() >= 4 => t,
                _ => continue,
            };

            // topics[2] = from (indexed), topics[3] = to (indexed)
            let from_hex = topics[2].as_str().unwrap_or_default();
            let to_hex = topics[3].as_str().unwrap_or_default();

            // Extract address from topic (last 40 hex chars of 66-char hex string)
            let from_addr = extract_address_from_topic(from_hex);
            let to_addr = extract_address_from_topic(to_hex);

            // Parse data: 64 hex chars for id + 64 hex chars for value (after 0x prefix)
            let data = log.get("data").and_then(|d| d.as_str()).unwrap_or("0x");
            let data_bytes = data.strip_prefix("0x").unwrap_or(data);
            if data_bytes.len() < 128 {
                continue;
            }

            let token_id = match U256::from_str_radix(&data_bytes[..64], 16) {
                Ok(id) => id,
                Err(_) => continue,
            };
            let value_u128 =
                u128::from_str_radix(&data_bytes[64..128].trim_start_matches('0').max("0"), 16)
                    .unwrap_or(0);
            let value = Decimal::from(value_u128);

            if value <= Decimal::ZERO {
                continue;
            }

            let condition_id = token_to_cid.get(&token_id).copied().unwrap_or_default();

            // Incoming transfer → potential Entry/Increase
            if tracked.contains(&to_addr) {
                let weight = self.wallet_weight(&to_addr);
                self.enqueue_or_stage_onchain_signal(SmartMoneySignal {
                    signal_type: SignalType::Entry,
                    wallet_address: to_addr.clone(),
                    wallet_label: None,
                    wallet_weight: weight,
                    token_id,
                    condition_id,
                    wallet_size: value,
                    delta: value,
                    signal_notional_usdc: value,
                    source: SmartMoneySignalSource::Onchain,
                    detected_at: Utc::now(),
                });
                signal_count += 1;
            }

            // Outgoing transfer → potential Decrease/Exit
            if tracked.contains(&from_addr) {
                let weight = self.wallet_weight(&from_addr);
                self.enqueue_or_stage_onchain_signal(SmartMoneySignal {
                    signal_type: SignalType::Decrease,
                    wallet_address: from_addr.clone(),
                    wallet_label: None,
                    wallet_weight: weight,
                    token_id,
                    condition_id,
                    wallet_size: Decimal::ZERO,
                    delta: value,
                    signal_notional_usdc: value,
                    source: SmartMoneySignalSource::Onchain,
                    detected_at: Utc::now(),
                });
                signal_count += 1;
            }
        }

        if signal_count > 0 {
            tracing::debug!(
                signals = signal_count,
                from_block = from_block + 1,
                to_block = latest,
                "SmartMoney: processed on-chain Transfer events"
            );
        }

        Ok(latest)
    }

    /// Call eth_blockNumber via JSON-RPC.
    async fn rpc_block_number(&self, rpc_url: &str) -> Result<u64> {
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "eth_blockNumber",
            "params": [],
            "id": 1
        });
        let resp: serde_json::Value = self
            .http_client
            .post(rpc_url)
            .json(&body)
            .send()
            .await
            .context("SmartMoney: eth_blockNumber request failed")?
            .json()
            .await
            .context("SmartMoney: eth_blockNumber parse failed")?;

        let hex = resp["result"].as_str().unwrap_or("0x0");
        let num = u64::from_str_radix(hex.strip_prefix("0x").unwrap_or(hex), 16).unwrap_or(0);
        Ok(num)
    }

    /// Call eth_getLogs via JSON-RPC.
    async fn rpc_get_logs(
        &self,
        rpc_url: &str,
        contract: &str,
        event_topic: &str,
        from_block: u64,
        to_block: u64,
    ) -> Result<Vec<serde_json::Value>> {
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "eth_getLogs",
            "params": [{
                "address": contract,
                "topics": [event_topic],
                "fromBlock": format!("0x{:x}", from_block),
                "toBlock": format!("0x{:x}", to_block),
            }],
            "id": 1
        });
        let resp: serde_json::Value = self
            .http_client
            .post(rpc_url)
            .json(&body)
            .send()
            .await
            .context("SmartMoney: eth_getLogs request failed")?
            .json()
            .await
            .context("SmartMoney: eth_getLogs parse failed")?;

        let logs = resp["result"].as_array().cloned().unwrap_or_default();
        Ok(logs)
    }

    /// Get the weight for a wallet address.
    fn wallet_weight(&self, address: &str) -> Decimal {
        let wallets = self.tracked_wallets.read().unwrap();
        wallets
            .iter()
            .find(|w| w.address == address)
            .map(|w| w.effective_weight)
            .unwrap_or(Decimal::ONE)
    }

    fn prune_wallet_recent_signals_for_config(
        lookback_secs: u64,
        wallet: &mut TrackedWallet,
        now: DateTime<Utc>,
    ) {
        let cutoff = now - chrono::Duration::seconds(lookback_secs as i64);
        while wallet
            .recent_signal_times
            .front()
            .copied()
            .is_some_and(|ts| ts <= cutoff)
        {
            wallet.recent_signal_times.pop_front();
        }
    }

    fn effective_wallet_weight_for_config(
        config: &SmartMoneyConfig,
        wallet: &TrackedWallet,
    ) -> Decimal {
        let profile_multiplier = if wallet.profile_score <= Decimal::ZERO {
            (Decimal::ONE - config.wallet_underperform_decay_step).max(Decimal::ZERO)
        } else {
            let ratio = if config.min_wallet_score > Decimal::ZERO {
                wallet.profile_score / config.min_wallet_score
            } else {
                Decimal::ONE
            };
            Decimal::ONE + (ratio - Decimal::ONE) * config.wallet_profile_blend
        };
        let signal_bonus = (Decimal::from(wallet.recent_signal_times.len() as u64)
            * config.wallet_signal_bonus_per_event)
            .min(config.wallet_signal_bonus_cap);
        let raw = wallet.base_weight * (profile_multiplier + signal_bonus);
        raw.max(config.wallet_min_effective_weight)
            .min(config.wallet_max_effective_weight)
    }

    fn recompute_wallet_effective_weight(&self, wallet: &mut TrackedWallet, now: DateTime<Utc>) {
        Self::prune_wallet_recent_signals_for_config(
            self.config.wallet_signal_lookback_secs,
            wallet,
            now,
        );
        wallet.effective_weight = Self::effective_wallet_weight_for_config(&self.config, wallet);
    }

    fn record_wallet_signal_activity(&self, wallet_address: &str, detected_at: DateTime<Utc>) {
        let mut wallets = self.tracked_wallets.write().unwrap();
        if let Some(wallet) = wallets
            .iter_mut()
            .find(|wallet| wallet.address == wallet_address)
        {
            wallet.recent_signal_times.push_back(detected_at);
            self.recompute_wallet_effective_weight(wallet, detected_at);
        }
    }

    fn refresh_wallet_profile_score(&self, wallet_address: &str, profile: &WalletProfile) {
        let mut wallets = self.tracked_wallets.write().unwrap();
        let Some(wallet) = wallets
            .iter_mut()
            .find(|wallet| wallet.address == wallet_address)
        else {
            return;
        };
        wallet.profile_score = if profile.volume >= self.config.min_wallet_volume_usdc
            && profile.volume > Decimal::ZERO
        {
            profile.pnl / profile.volume
        } else {
            Decimal::ZERO
        };
        self.recompute_wallet_effective_weight(wallet, Utc::now());
    }

    fn publish_wallet_score_snapshot(&self) {
        let now = Utc::now();
        let mut wallets = self.tracked_wallets.write().unwrap();
        for wallet in wallets.iter_mut() {
            Self::prune_wallet_recent_signals_for_config(
                self.config.wallet_signal_lookback_secs,
                wallet,
                now,
            );
            wallet.effective_weight =
                Self::effective_wallet_weight_for_config(&self.config, wallet);
        }
        let entries: Vec<_> = wallets
            .iter()
            .map(|wallet| SmartMoneyWalletScoreEntry {
                address: wallet.address.clone(),
                label: wallet.label.clone(),
                base_weight: wallet.base_weight,
                effective_weight: wallet.effective_weight,
                profile_score: wallet.profile_score,
                recent_signal_count: wallet.recent_signal_times.len(),
                auto_discovered: wallet.auto_discovered,
            })
            .collect();
        drop(wallets);
        record_smart_money_wallet_scores(entries);
    }

    async fn refresh_wallet_profiles(&self) -> Result<()> {
        let wallets = self.tracked_wallets.read().unwrap().clone();
        for wallet in wallets {
            match self.fetch_profile(&wallet.address).await {
                Ok(profile) => self.refresh_wallet_profile_score(&wallet.address, &profile),
                Err(e) => {
                    tracing::debug!(
                        wallet = %wallet.address,
                        error = %e,
                        "SmartMoney: failed to refresh wallet profile"
                    );
                }
            }
        }
        self.publish_wallet_score_snapshot();
        Ok(())
    }

    fn score_wallet_profile(&self, profile: &WalletProfile) -> Decimal {
        if profile.volume < self.config.min_wallet_volume_usdc || profile.volume <= Decimal::ZERO {
            return Decimal::ZERO;
        }
        let base_score = profile.pnl / profile.volume;
        let volume_multiplier =
            (profile.volume / self.config.min_wallet_volume_usdc).min(Decimal::from(3));
        base_score * volume_multiplier
    }

    fn should_emit_signal(&self, signal: &SmartMoneySignal) -> bool {
        signal.delta >= self.config.min_signal_delta_shares
            && signal.signal_notional_usdc >= self.config.min_signal_notional_usdc
            && signal.wallet_weight >= self.config.min_wallet_weight
    }

    fn is_duplicate_signal(&self, signal: &SmartMoneySignal) -> bool {
        let key = (
            signal.wallet_address.clone(),
            signal.token_id,
            signal.signal_type,
        );
        let recent = self.recent_signal_index.read().unwrap();
        let Some(existing) = recent.get(&key) else {
            return false;
        };
        let window = chrono::Duration::seconds(self.config.dedup_window_secs as i64);
        if signal.detected_at - existing.detected_at > window {
            return false;
        }
        let tolerance = self.config.min_signal_delta_shares.min(Decimal::ONE);
        (signal.delta - existing.delta).abs() <= tolerance
    }

    fn record_signal_emitted(&self, signal: &SmartMoneySignal) {
        let mut recent = self.recent_signal_index.write().unwrap();
        recent.insert(
            (
                signal.wallet_address.clone(),
                signal.token_id,
                signal.signal_type,
            ),
            RecentSignalMeta {
                detected_at: signal.detected_at,
                delta: signal.delta,
            },
        );
    }

    fn enqueue_signal(&self, signal: SmartMoneySignal) {
        if !self.should_emit_signal(&signal) || self.is_duplicate_signal(&signal) {
            return;
        }
        self.record_wallet_signal_activity(&signal.wallet_address, signal.detected_at);
        self.record_signal_emitted(&signal);
        self.signals.write().unwrap().push(signal);
    }

    fn enqueue_or_stage_onchain_signal(&self, signal: SmartMoneySignal) {
        if !self.should_emit_signal(&signal) {
            return;
        }
        if !self.config.confirm_onchain_with_data_api {
            self.enqueue_signal(signal);
            return;
        }
        self.pending_onchain_signals.write().unwrap().insert(
            (
                signal.wallet_address.clone(),
                signal.token_id,
                signal.signal_type,
            ),
            signal,
        );
    }

    fn signal_types_match_for_confirmation(pending: SignalType, confirmed: SignalType) -> bool {
        matches!(
            (pending, confirmed),
            (SignalType::Entry, SignalType::Entry)
                | (SignalType::Entry, SignalType::Increase)
                | (SignalType::Increase, SignalType::Entry)
                | (SignalType::Increase, SignalType::Increase)
                | (SignalType::Decrease, SignalType::Decrease)
                | (SignalType::Decrease, SignalType::Exit)
                | (SignalType::Exit, SignalType::Decrease)
                | (SignalType::Exit, SignalType::Exit)
        )
    }

    fn take_confirmed_pending_onchain_signal(
        &self,
        signal: &SmartMoneySignal,
    ) -> Option<SmartMoneySignal> {
        let mut pending = self.pending_onchain_signals.write().unwrap();
        let keys: Vec<_> = pending
            .keys()
            .filter(|(wallet, token_id, pending_type)| {
                wallet == &signal.wallet_address
                    && *token_id == signal.token_id
                    && Self::signal_types_match_for_confirmation(*pending_type, signal.signal_type)
            })
            .cloned()
            .collect();
        let key = keys
            .into_iter()
            .min_by_key(|(_, _, signal_type)| match signal_type {
                SignalType::Entry => 0,
                SignalType::Increase => 1,
                SignalType::Decrease => 2,
                SignalType::Exit => 3,
            })?;
        pending.remove(&key)
    }

    fn confirm_or_enqueue_data_signal(&self, mut signal: SmartMoneySignal) {
        signal.source = SmartMoneySignalSource::DataApi;
        if let Some(mut staged) = self.take_confirmed_pending_onchain_signal(&signal) {
            staged.wallet_size = signal.wallet_size;
            staged.delta = signal.delta;
            staged.signal_notional_usdc = signal.signal_notional_usdc;
            staged.condition_id = signal.condition_id;
            staged.detected_at = signal.detected_at;
            self.enqueue_signal(staged);
            return;
        }
        self.enqueue_signal(signal);
    }

    // ──── Signal Management ────

    /// Remove signals older than TTL.
    fn prune_stale_signals(&self) {
        let ttl = chrono::Duration::seconds(self.config.signal_ttl_secs as i64);
        let cutoff = Utc::now() - ttl;
        let mut signals = self.signals.write().unwrap();
        let before = signals.len();
        signals.retain(|s| s.detected_at > cutoff);
        let pruned = before - signals.len();
        if pruned > 0 {
            tracing::debug!(
                pruned,
                remaining = signals.len(),
                "SmartMoney: pruned stale signals"
            );
        }
    }

    fn prune_stale_pending_onchain_signals(&self) {
        let ttl = chrono::Duration::seconds(self.config.signal_ttl_secs as i64);
        let cutoff = Utc::now() - ttl;
        self.pending_onchain_signals
            .write()
            .unwrap()
            .retain(|_, signal| signal.detected_at > cutoff);
    }

    fn prune_recent_signal_index(&self) {
        let ttl = chrono::Duration::seconds(self.config.dedup_window_secs as i64);
        let cutoff = Utc::now() - ttl;
        self.recent_signal_index
            .write()
            .unwrap()
            .retain(|_, meta| meta.detected_at > cutoff);
    }

    // ──── Auto-Discovery ────

    /// Evaluate candidate wallets and promote high-scoring ones to tracked list.
    async fn auto_discover(&self) -> Result<()> {
        let candidates = self.config.auto_discover_candidates.clone();
        if candidates.is_empty() {
            return Ok(());
        }

        let max = self.config.max_wallets;

        for address in &candidates {
            if self.tracked_wallets.read().unwrap().len() >= max {
                break;
            }

            // Skip if already tracked
            {
                let wallets = self.tracked_wallets.read().unwrap();
                if wallets.iter().any(|w| w.address == address.to_lowercase()) {
                    continue;
                }
            }

            let profile = match self.fetch_profile(address).await {
                Ok(p) => p,
                Err(e) => {
                    tracing::debug!(address, error = %e, "SmartMoney: failed to fetch profile");
                    continue;
                }
            };

            let score = self.score_wallet_profile(&profile);

            if score >= self.config.min_wallet_score {
                tracing::info!(
                    address,
                    pnl = %profile.pnl,
                    volume = %profile.volume,
                    score = %score,
                    "SmartMoney: auto-discovered high-PnL wallet"
                );
                let mut wallets = self.tracked_wallets.write().unwrap();
                wallets.push(TrackedWallet {
                    address: address.to_lowercase(),
                    label: format!("auto_{}", &address[..8.min(address.len())]),
                    base_weight: Decimal::ONE,
                    effective_weight: Decimal::ONE,
                    profile_score: score,
                    recent_signal_times: VecDeque::new(),
                    auto_discovered: true,
                });
            }
        }

        self.publish_wallet_score_snapshot();

        Ok(())
    }

    /// Fetch wallet profile stats from Polymarket Data API.
    async fn fetch_profile(&self, address: &str) -> Result<WalletProfile> {
        let url = format!("https://data-api.polymarket.com/activity?user={}", address);

        let resp = self
            .http_client
            .get(&url)
            .send()
            .await
            .context("SmartMoney profile request failed")?;

        if !resp.status().is_success() {
            return Ok(WalletProfile {
                pnl: Decimal::ZERO,
                volume: Decimal::ZERO,
            });
        }

        // Try to parse; return zero profile on failure
        let raw: RawProfileActivity = resp.json().await.unwrap_or(RawProfileActivity {
            profit_loss: Decimal::ZERO,
            total_volume: Decimal::ZERO,
        });

        Ok(WalletProfile {
            pnl: raw.profit_loss,
            volume: raw.total_volume,
        })
    }
}

/// Extract a lowercase 0x-prefixed address from a 32-byte hex topic.
/// Topic format: "0x000000000000000000000000aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
fn extract_address_from_topic(topic: &str) -> String {
    let hex = topic.strip_prefix("0x").unwrap_or(topic);
    if hex.len() >= 40 {
        format!("0x{}", &hex[hex.len() - 40..]).to_lowercase()
    } else {
        topic.to_lowercase()
    }
}

// ──── Snapshot Diffing (pure function, testable) ────

/// Compare old and new position snapshots, emit signals for all changes.
pub fn diff_snapshots(
    old: &HashMap<U256, WalletPosition>,
    new: &HashMap<U256, WalletPosition>,
    wallet_address: &str,
    wallet_label: Option<&str>,
    wallet_weight: Decimal,
) -> Vec<SmartMoneySignal> {
    let mut signals = Vec::new();
    let now = Utc::now();

    // Check new/increased positions
    for (tid, new_pos) in new {
        match old.get(tid) {
            None => {
                // Entry: wasn't there before
                signals.push(SmartMoneySignal {
                    signal_type: SignalType::Entry,
                    wallet_address: wallet_address.to_string(),
                    wallet_label: wallet_label.map(ToString::to_string),
                    wallet_weight,
                    token_id: *tid,
                    condition_id: new_pos.condition_id,
                    wallet_size: new_pos.size,
                    delta: new_pos.size,
                    signal_notional_usdc: new_pos.size,
                    source: SmartMoneySignalSource::DataApi,
                    detected_at: now,
                });
            }
            Some(old_pos) if new_pos.size > old_pos.size => {
                // Increase
                signals.push(SmartMoneySignal {
                    signal_type: SignalType::Increase,
                    wallet_address: wallet_address.to_string(),
                    wallet_label: wallet_label.map(ToString::to_string),
                    wallet_weight,
                    token_id: *tid,
                    condition_id: new_pos.condition_id,
                    wallet_size: new_pos.size,
                    delta: new_pos.size - old_pos.size,
                    signal_notional_usdc: new_pos.size - old_pos.size,
                    source: SmartMoneySignalSource::DataApi,
                    detected_at: now,
                });
            }
            Some(old_pos) if new_pos.size < old_pos.size => {
                // Decrease (partial exit)
                signals.push(SmartMoneySignal {
                    signal_type: SignalType::Decrease,
                    wallet_address: wallet_address.to_string(),
                    wallet_label: wallet_label.map(ToString::to_string),
                    wallet_weight,
                    token_id: *tid,
                    condition_id: new_pos.condition_id,
                    wallet_size: new_pos.size,
                    delta: old_pos.size - new_pos.size,
                    signal_notional_usdc: old_pos.size - new_pos.size,
                    source: SmartMoneySignalSource::DataApi,
                    detected_at: now,
                });
            }
            _ => {} // unchanged
        }
    }

    // Check fully exited positions (in old but not in new)
    for (tid, old_pos) in old {
        if !new.contains_key(tid) {
            signals.push(SmartMoneySignal {
                signal_type: SignalType::Exit,
                wallet_address: wallet_address.to_string(),
                wallet_label: wallet_label.map(ToString::to_string),
                wallet_weight,
                token_id: *tid,
                condition_id: old_pos.condition_id,
                wallet_size: Decimal::ZERO,
                delta: old_pos.size,
                signal_notional_usdc: old_pos.size,
                source: SmartMoneySignalSource::DataApi,
                detected_at: now,
            });
        }
    }

    signals
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn make_pos(token_id: u64, size: Decimal) -> WalletPosition {
        WalletPosition {
            token_id: U256::from(token_id),
            size,
            condition_id: B256::ZERO,
        }
    }

    fn make_map(positions: Vec<WalletPosition>) -> HashMap<U256, WalletPosition> {
        positions.into_iter().map(|p| (p.token_id, p)).collect()
    }

    #[test]
    fn test_diff_new_entry() {
        let old = HashMap::new();
        let new = make_map(vec![make_pos(1, dec!(100))]);

        let signals = diff_snapshots(&old, &new, "0xabc", None, Decimal::ONE);

        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0].signal_type, SignalType::Entry);
        assert_eq!(signals[0].delta, dec!(100));
        assert_eq!(signals[0].wallet_size, dec!(100));
    }

    #[test]
    fn test_diff_size_increase() {
        let old = make_map(vec![make_pos(1, dec!(50))]);
        let new = make_map(vec![make_pos(1, dec!(80))]);

        let signals = diff_snapshots(&old, &new, "0xabc", None, Decimal::ONE);

        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0].signal_type, SignalType::Increase);
        assert_eq!(signals[0].delta, dec!(30));
        assert_eq!(signals[0].wallet_size, dec!(80));
    }

    #[test]
    fn test_diff_full_exit() {
        let old = make_map(vec![make_pos(1, dec!(100))]);
        let new = HashMap::new();

        let signals = diff_snapshots(&old, &new, "0xabc", None, Decimal::ONE);

        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0].signal_type, SignalType::Exit);
        assert_eq!(signals[0].delta, dec!(100));
        assert_eq!(signals[0].wallet_size, Decimal::ZERO);
    }

    #[test]
    fn test_diff_partial_exit() {
        let old = make_map(vec![make_pos(1, dec!(100))]);
        let new = make_map(vec![make_pos(1, dec!(40))]);

        let signals = diff_snapshots(&old, &new, "0xabc", None, Decimal::ONE);

        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0].signal_type, SignalType::Decrease);
        assert_eq!(signals[0].delta, dec!(60));
        assert_eq!(signals[0].wallet_size, dec!(40));
    }

    #[test]
    fn test_signal_ttl_prune() {
        let config = SmartMoneyConfig {
            signal_ttl_secs: 1, // 1 second TTL
            ..Default::default()
        };
        let tracker = WalletTracker::new(config, Arc::new(RwLock::new(HashMap::new())));

        // Insert a stale signal
        {
            let mut signals = tracker.signals.write().unwrap();
            signals.push(SmartMoneySignal {
                signal_type: SignalType::Entry,
                wallet_address: "0xabc".to_string(),
                wallet_label: None,
                wallet_weight: Decimal::ONE,
                token_id: U256::from(1u64),
                condition_id: B256::ZERO,
                wallet_size: dec!(100),
                delta: dec!(100),
                signal_notional_usdc: dec!(100),
                source: SmartMoneySignalSource::DataApi,
                detected_at: Utc::now() - chrono::Duration::seconds(10),
            });
        }

        tracker.prune_stale_signals();

        let signals = tracker.signals.read().unwrap();
        assert!(signals.is_empty(), "stale signal should have been pruned");
    }

    #[test]
    fn test_diff_no_change() {
        let old = make_map(vec![make_pos(1, dec!(100))]);
        let new = make_map(vec![make_pos(1, dec!(100))]);

        let signals = diff_snapshots(&old, &new, "0xabc", None, Decimal::ONE);
        assert!(signals.is_empty());
    }

    #[test]
    fn test_diff_multiple_tokens() {
        let old = make_map(vec![make_pos(1, dec!(100)), make_pos(2, dec!(50))]);
        let new = make_map(vec![
            make_pos(1, dec!(150)), // increased
            // token 2 gone → exit
            make_pos(3, dec!(30)), // new entry
        ]);

        let signals = diff_snapshots(&old, &new, "0xabc", None, dec!(0.8));

        assert_eq!(signals.len(), 3);

        let entry = signals
            .iter()
            .find(|s| s.signal_type == SignalType::Entry)
            .unwrap();
        assert_eq!(entry.token_id, U256::from(3u64));
        assert_eq!(entry.wallet_weight, dec!(0.8));

        let increase = signals
            .iter()
            .find(|s| s.signal_type == SignalType::Increase)
            .unwrap();
        assert_eq!(increase.token_id, U256::from(1u64));
        assert_eq!(increase.delta, dec!(50));

        let exit = signals
            .iter()
            .find(|s| s.signal_type == SignalType::Exit)
            .unwrap();
        assert_eq!(exit.token_id, U256::from(2u64));
        assert_eq!(exit.delta, dec!(50));
    }

    #[test]
    fn test_should_emit_signal_filters_small_delta() {
        let config = SmartMoneyConfig {
            min_signal_delta_shares: dec!(20),
            min_signal_notional_usdc: dec!(25),
            ..Default::default()
        };
        let tracker = WalletTracker::new(config, Arc::new(RwLock::new(HashMap::new())));
        let signal = SmartMoneySignal {
            signal_type: SignalType::Entry,
            wallet_address: "0xabc".to_string(),
            wallet_label: None,
            wallet_weight: Decimal::ONE,
            token_id: U256::from(1u64),
            condition_id: B256::ZERO,
            wallet_size: dec!(10),
            delta: dec!(10),
            signal_notional_usdc: dec!(10),
            source: SmartMoneySignalSource::DataApi,
            detected_at: Utc::now(),
        };

        assert!(!tracker.should_emit_signal(&signal));
    }

    #[test]
    fn test_duplicate_signal_suppressed_within_window() {
        let config = SmartMoneyConfig {
            dedup_window_secs: 45,
            min_signal_delta_shares: dec!(1),
            min_signal_notional_usdc: dec!(1),
            ..Default::default()
        };
        let tracker = WalletTracker::new(config, Arc::new(RwLock::new(HashMap::new())));
        let signal = SmartMoneySignal {
            signal_type: SignalType::Entry,
            wallet_address: "0xabc".to_string(),
            wallet_label: None,
            wallet_weight: Decimal::ONE,
            token_id: U256::from(1u64),
            condition_id: B256::ZERO,
            wallet_size: dec!(100),
            delta: dec!(100),
            signal_notional_usdc: dec!(100),
            source: SmartMoneySignalSource::DataApi,
            detected_at: Utc::now(),
        };

        tracker.enqueue_signal(signal.clone());
        tracker.enqueue_signal(signal);

        let signals = tracker.signals.read().unwrap();
        assert_eq!(signals.len(), 1);
    }
}
