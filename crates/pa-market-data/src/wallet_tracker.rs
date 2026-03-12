use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use alloy::primitives::{B256, U256};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use tokio_util::sync::CancellationToken;

use pa_core::config::SmartMoneyConfig;

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
    pub wallet_weight: Decimal,
    pub token_id: U256,
    pub condition_id: B256,
    /// Wallet's NEW position size (after the change).
    pub wallet_size: Decimal,
    /// Change amount (positive for increase, full size for new entry).
    pub delta: Decimal,
    pub detected_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    weight: Decimal,
    /// Whether this wallet was auto-discovered (vs manually configured).
    _auto_discovered: bool,
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
                weight: w.weight,
                _auto_discovered: false,
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
                }
                _ = onchain_tick.tick(), if onchain_enabled => {
                    match self.poll_onchain_logs(&rpc_url_owned, last_block).await {
                        Ok(new_block) => last_block = new_block,
                        Err(e) => tracing::warn!(error = %e, "SmartMoney: on-chain poll failed"),
                    }
                }
                _ = discover_tick.tick(), if self.config.auto_discover_enabled => {
                    if let Err(e) = self.auto_discover().await {
                        tracing::warn!(error = %e, "SmartMoney: auto-discovery failed");
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
            let signals = diff_snapshots(&old_map, &new_map, &wallet.address, wallet.weight);

            if !signals.is_empty() {
                tracing::info!(
                    wallet = %wallet.label,
                    address = %wallet.address,
                    signals = signals.len(),
                    "SmartMoney: position changes detected"
                );
                let mut sig_queue = self.signals.write().unwrap();
                sig_queue.extend(signals);
            }

            // Update snapshot
            {
                let mut snapshots = self.snapshots.write().unwrap();
                snapshots.insert(wallet.address.clone(), new_map);
            }
        }

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
                let mut sig_queue = self.signals.write().unwrap();
                sig_queue.push(SmartMoneySignal {
                    signal_type: SignalType::Entry,
                    wallet_address: to_addr.clone(),
                    wallet_weight: weight,
                    token_id,
                    condition_id,
                    wallet_size: value,
                    delta: value,
                    detected_at: Utc::now(),
                });
                signal_count += 1;
            }

            // Outgoing transfer → potential Decrease/Exit
            if tracked.contains(&from_addr) {
                let weight = self.wallet_weight(&from_addr);
                let mut sig_queue = self.signals.write().unwrap();
                sig_queue.push(SmartMoneySignal {
                    signal_type: SignalType::Decrease,
                    wallet_address: from_addr.clone(),
                    wallet_weight: weight,
                    token_id,
                    condition_id,
                    wallet_size: Decimal::ZERO,
                    delta: value,
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
            .map(|w| w.weight)
            .unwrap_or(Decimal::ONE)
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

    // ──── Auto-Discovery ────

    /// Evaluate candidate wallets and promote high-scoring ones to tracked list.
    async fn auto_discover(&self) -> Result<()> {
        let candidates = self.config.auto_discover_candidates.clone();
        if candidates.is_empty() {
            return Ok(());
        }

        let current_count = self.tracked_wallets.read().unwrap().len();
        let max = self.config.max_wallets;

        for address in &candidates {
            if current_count >= max {
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

            let volume = profile.volume.max(Decimal::ONE);
            let score = profile.pnl / volume;

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
                    weight: Decimal::ONE,
                    _auto_discovered: true,
                });
            }
        }

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
                    wallet_weight,
                    token_id: *tid,
                    condition_id: new_pos.condition_id,
                    wallet_size: new_pos.size,
                    delta: new_pos.size,
                    detected_at: now,
                });
            }
            Some(old_pos) if new_pos.size > old_pos.size => {
                // Increase
                signals.push(SmartMoneySignal {
                    signal_type: SignalType::Increase,
                    wallet_address: wallet_address.to_string(),
                    wallet_weight,
                    token_id: *tid,
                    condition_id: new_pos.condition_id,
                    wallet_size: new_pos.size,
                    delta: new_pos.size - old_pos.size,
                    detected_at: now,
                });
            }
            Some(old_pos) if new_pos.size < old_pos.size => {
                // Decrease (partial exit)
                signals.push(SmartMoneySignal {
                    signal_type: SignalType::Decrease,
                    wallet_address: wallet_address.to_string(),
                    wallet_weight,
                    token_id: *tid,
                    condition_id: new_pos.condition_id,
                    wallet_size: new_pos.size,
                    delta: old_pos.size - new_pos.size,
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
                wallet_weight,
                token_id: *tid,
                condition_id: old_pos.condition_id,
                wallet_size: Decimal::ZERO,
                delta: old_pos.size,
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

        let signals = diff_snapshots(&old, &new, "0xabc", Decimal::ONE);

        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0].signal_type, SignalType::Entry);
        assert_eq!(signals[0].delta, dec!(100));
        assert_eq!(signals[0].wallet_size, dec!(100));
    }

    #[test]
    fn test_diff_size_increase() {
        let old = make_map(vec![make_pos(1, dec!(50))]);
        let new = make_map(vec![make_pos(1, dec!(80))]);

        let signals = diff_snapshots(&old, &new, "0xabc", Decimal::ONE);

        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0].signal_type, SignalType::Increase);
        assert_eq!(signals[0].delta, dec!(30));
        assert_eq!(signals[0].wallet_size, dec!(80));
    }

    #[test]
    fn test_diff_full_exit() {
        let old = make_map(vec![make_pos(1, dec!(100))]);
        let new = HashMap::new();

        let signals = diff_snapshots(&old, &new, "0xabc", Decimal::ONE);

        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0].signal_type, SignalType::Exit);
        assert_eq!(signals[0].delta, dec!(100));
        assert_eq!(signals[0].wallet_size, Decimal::ZERO);
    }

    #[test]
    fn test_diff_partial_exit() {
        let old = make_map(vec![make_pos(1, dec!(100))]);
        let new = make_map(vec![make_pos(1, dec!(40))]);

        let signals = diff_snapshots(&old, &new, "0xabc", Decimal::ONE);

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
                wallet_weight: Decimal::ONE,
                token_id: U256::from(1u64),
                condition_id: B256::ZERO,
                wallet_size: dec!(100),
                delta: dec!(100),
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

        let signals = diff_snapshots(&old, &new, "0xabc", Decimal::ONE);
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

        let signals = diff_snapshots(&old, &new, "0xabc", dec!(0.8));

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
}
