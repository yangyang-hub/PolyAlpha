use std::collections::{HashMap, VecDeque};
use std::sync::{LazyLock, Mutex};

use alloy::primitives::B256;
use chrono::{DateTime, Duration, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

const MAX_CRYPTO_DECISIONS: usize = 100;
const MAX_CRYPTO_EXITS: usize = 100;
const MAX_CRYPTO_PATCH_EXPORTS: usize = 100;
const MAX_SMART_MONEY_DECISIONS: usize = 200;
const MAX_SMART_MONEY_EXITS: usize = 100;
const CRYPTO_EXIT_DEDUP_WINDOW_SECS: i64 = 5;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CryptoCandidateDecision {
    pub recorded_at: DateTime<Utc>,
    pub asset: String,
    pub direction: String,
    pub action: String,
    pub reason: String,
    pub event_context_source: Option<String>,
    pub event_title: Option<String>,
    pub event_category: Option<String>,
    pub event_subtype: Option<String>,
    pub selected_question: String,
    pub selected_condition_id: B256,
    pub selected_market_type: String,
    pub selected_modeled_prob: Decimal,
    pub selected_is_yes: bool,
    pub selected_days_to_resolution: u32,
    pub replaced_question: Option<String>,
    pub replaced_condition_id: Option<B256>,
    pub replaced_market_type: Option<String>,
    pub replaced_modeled_prob: Option<Decimal>,
    pub replaced_is_yes: Option<bool>,
    pub replaced_days_to_resolution: Option<u32>,
    pub selected_estimated_profit: Decimal,
    pub replaced_estimated_profit: Option<Decimal>,
    pub selected_efficiency: Decimal,
    pub replaced_efficiency: Option<Decimal>,
    pub selected_executable_profit_retention: Decimal,
    pub replaced_executable_profit_retention: Option<Decimal>,
    pub selected_executable_size_retention: Decimal,
    pub replaced_executable_size_retention: Option<Decimal>,
    pub selected_executable_quality_score: Decimal,
    pub replaced_executable_quality_score: Option<Decimal>,
    pub selected_executable_efficiency: Decimal,
    pub replaced_executable_efficiency: Option<Decimal>,
    pub selected_depth_buffer: Decimal,
    pub replaced_depth_buffer: Option<Decimal>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CryptoExitDecision {
    pub recorded_at: DateTime<Utc>,
    pub asset: Option<String>,
    pub reason: String,
    pub event_context_source: Option<String>,
    pub event_title: Option<String>,
    pub event_category: Option<String>,
    pub event_subtype: Option<String>,
    pub question: String,
    pub market_type: Option<String>,
    pub held_is_yes: Option<bool>,
    pub modeled_prob: Option<Decimal>,
    pub days_to_resolution: Option<u32>,
    pub best_bid: Decimal,
    pub avg_cost: Decimal,
    pub size: Decimal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CryptoOverridePatchExportDecision {
    pub recorded_at: DateTime<Utc>,
    pub mode: String,
    pub format: String,
    pub filename: String,
    pub export_sha: String,
    pub scope_label: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmartMoneyDecision {
    pub recorded_at: DateTime<Utc>,
    pub token_id: String,
    pub condition_id: String,
    pub signal_type: String,
    pub accepted: bool,
    pub reject_reason: Option<String>,
    pub wallet_count: usize,
    pub max_wallet_weight: Decimal,
    pub source_data_api: bool,
    pub source_onchain: bool,
    pub leader_addresses: Vec<String>,
    pub leader_labels: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmartMoneyWalletScoreEntry {
    pub address: String,
    pub label: String,
    pub base_weight: Decimal,
    pub effective_weight: Decimal,
    pub profile_score: Decimal,
    pub recent_signal_count: usize,
    pub auto_discovered: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmartMoneyExitDecision {
    pub recorded_at: DateTime<Utc>,
    pub token_id: String,
    pub condition_id: String,
    pub reason: String,
    pub question: String,
    pub best_bid: Decimal,
    pub avg_cost: Decimal,
    pub size: Decimal,
    pub estimated_profit: Decimal,
    pub attributed_leaders: Vec<SmartMoneyLeaderAttributionSlice>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmartMoneyLeaderAttributionSlice {
    pub leader: String,
    pub estimated_size: Decimal,
    pub estimated_profit: Decimal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmartMoneyLeaderPnlAttributionEntry {
    pub leader: String,
    pub estimated_open_size: Decimal,
    pub estimated_exited_size: Decimal,
    pub estimated_realized_pnl: Decimal,
    pub estimated_exit_count: usize,
}

static CRYPTO_CANDIDATE_DECISIONS: LazyLock<Mutex<VecDeque<CryptoCandidateDecision>>> =
    LazyLock::new(|| Mutex::new(VecDeque::new()));
static CRYPTO_EXIT_DECISIONS: LazyLock<Mutex<VecDeque<CryptoExitDecision>>> =
    LazyLock::new(|| Mutex::new(VecDeque::new()));
static CRYPTO_PATCH_EXPORT_DECISIONS: LazyLock<Mutex<VecDeque<CryptoOverridePatchExportDecision>>> =
    LazyLock::new(|| Mutex::new(VecDeque::new()));
static SMART_MONEY_DECISIONS: LazyLock<Mutex<VecDeque<SmartMoneyDecision>>> =
    LazyLock::new(|| Mutex::new(VecDeque::new()));
static SMART_MONEY_WALLET_SCORES: LazyLock<Mutex<Vec<SmartMoneyWalletScoreEntry>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));
static SMART_MONEY_EXIT_DECISIONS: LazyLock<Mutex<VecDeque<SmartMoneyExitDecision>>> =
    LazyLock::new(|| Mutex::new(VecDeque::new()));
static SMART_MONEY_LEADER_PNL_ATTRIBUTION: LazyLock<
    Mutex<Vec<SmartMoneyLeaderPnlAttributionEntry>>,
> = LazyLock::new(|| Mutex::new(Vec::new()));
static SMART_MONEY_OPPORTUNITY_ATTRIBUTION: LazyLock<
    Mutex<HashMap<Uuid, Vec<SmartMoneyLeaderAttributionSlice>>>,
> = LazyLock::new(|| Mutex::new(HashMap::new()));

pub fn record_crypto_candidate_decision(entry: CryptoCandidateDecision) {
    let mut entries = CRYPTO_CANDIDATE_DECISIONS.lock().unwrap();
    entries.push_front(entry);
    while entries.len() > MAX_CRYPTO_DECISIONS {
        entries.pop_back();
    }
}

pub fn recent_crypto_candidate_decisions() -> Vec<CryptoCandidateDecision> {
    CRYPTO_CANDIDATE_DECISIONS
        .lock()
        .unwrap()
        .iter()
        .cloned()
        .collect()
}

pub fn clear_crypto_candidate_decisions() {
    CRYPTO_CANDIDATE_DECISIONS.lock().unwrap().clear();
}

pub fn record_crypto_exit_decision(entry: CryptoExitDecision) {
    let mut entries = CRYPTO_EXIT_DECISIONS.lock().unwrap();
    let dedup_window = Duration::seconds(CRYPTO_EXIT_DEDUP_WINDOW_SECS);
    let duplicated_recent_exit = entries.iter().any(|recent| {
        let within_dedup_window = (entry.recorded_at - recent.recorded_at) <= dedup_window;
        let same_exit = recent.asset == entry.asset
            && recent.reason == entry.reason
            && recent.event_context_source == entry.event_context_source
            && recent.event_title == entry.event_title
            && recent.event_category == entry.event_category
            && recent.event_subtype == entry.event_subtype
            && recent.question == entry.question
            && recent.market_type == entry.market_type
            && recent.held_is_yes == entry.held_is_yes
            && recent.days_to_resolution == entry.days_to_resolution
            && recent.avg_cost == entry.avg_cost
            && recent.size == entry.size;
        within_dedup_window && same_exit
    });
    if duplicated_recent_exit {
        return;
    }
    while entries.len() > MAX_CRYPTO_EXITS {
        entries.pop_back();
    }
    entries.push_front(entry);
    while entries.len() > MAX_CRYPTO_EXITS {
        entries.pop_back();
    }
}

pub fn recent_crypto_exit_decisions() -> Vec<CryptoExitDecision> {
    CRYPTO_EXIT_DECISIONS
        .lock()
        .unwrap()
        .iter()
        .cloned()
        .collect()
}

pub fn clear_crypto_exit_decisions() {
    CRYPTO_EXIT_DECISIONS.lock().unwrap().clear();
}

pub fn record_crypto_override_patch_export(entry: CryptoOverridePatchExportDecision) {
    let mut entries = CRYPTO_PATCH_EXPORT_DECISIONS.lock().unwrap();
    entries.push_front(entry);
    while entries.len() > MAX_CRYPTO_PATCH_EXPORTS {
        entries.pop_back();
    }
}

pub fn recent_crypto_override_patch_exports() -> Vec<CryptoOverridePatchExportDecision> {
    CRYPTO_PATCH_EXPORT_DECISIONS
        .lock()
        .unwrap()
        .iter()
        .cloned()
        .collect()
}

pub fn clear_crypto_override_patch_exports() {
    CRYPTO_PATCH_EXPORT_DECISIONS.lock().unwrap().clear();
}

pub fn record_smart_money_decision(entry: SmartMoneyDecision) {
    let mut entries = SMART_MONEY_DECISIONS.lock().unwrap();
    entries.push_front(entry);
    while entries.len() > MAX_SMART_MONEY_DECISIONS {
        entries.pop_back();
    }
}

pub fn recent_smart_money_decisions() -> Vec<SmartMoneyDecision> {
    SMART_MONEY_DECISIONS
        .lock()
        .unwrap()
        .iter()
        .cloned()
        .collect()
}

pub fn clear_smart_money_decisions() {
    SMART_MONEY_DECISIONS.lock().unwrap().clear();
}

pub fn record_smart_money_wallet_scores(entries: Vec<SmartMoneyWalletScoreEntry>) {
    let mut scores = SMART_MONEY_WALLET_SCORES.lock().unwrap();
    *scores = entries;
}

pub fn smart_money_wallet_scores() -> Vec<SmartMoneyWalletScoreEntry> {
    SMART_MONEY_WALLET_SCORES.lock().unwrap().clone()
}

pub fn clear_smart_money_wallet_scores() {
    SMART_MONEY_WALLET_SCORES.lock().unwrap().clear();
}

pub fn record_smart_money_exit_decision(entry: SmartMoneyExitDecision) {
    let mut entries = SMART_MONEY_EXIT_DECISIONS.lock().unwrap();
    entries.push_front(entry);
    while entries.len() > MAX_SMART_MONEY_EXITS {
        entries.pop_back();
    }
}

pub fn recent_smart_money_exit_decisions() -> Vec<SmartMoneyExitDecision> {
    SMART_MONEY_EXIT_DECISIONS
        .lock()
        .unwrap()
        .iter()
        .cloned()
        .collect()
}

pub fn clear_smart_money_exit_decisions() {
    SMART_MONEY_EXIT_DECISIONS.lock().unwrap().clear();
}

pub fn record_smart_money_leader_pnl_attribution(
    entries: Vec<SmartMoneyLeaderPnlAttributionEntry>,
) {
    let mut attribution = SMART_MONEY_LEADER_PNL_ATTRIBUTION.lock().unwrap();
    *attribution = entries;
}

pub fn smart_money_leader_pnl_attribution() -> Vec<SmartMoneyLeaderPnlAttributionEntry> {
    SMART_MONEY_LEADER_PNL_ATTRIBUTION.lock().unwrap().clone()
}

pub fn clear_smart_money_leader_pnl_attribution() {
    SMART_MONEY_LEADER_PNL_ATTRIBUTION.lock().unwrap().clear();
}

pub fn record_smart_money_opportunity_attribution(
    opportunity_id: Uuid,
    attribution: Vec<SmartMoneyLeaderAttributionSlice>,
) {
    let mut entries = SMART_MONEY_OPPORTUNITY_ATTRIBUTION.lock().unwrap();
    entries.insert(opportunity_id, attribution);
}

pub fn take_smart_money_opportunity_attribution(
    opportunity_id: &Uuid,
) -> Option<Vec<SmartMoneyLeaderAttributionSlice>> {
    SMART_MONEY_OPPORTUNITY_ATTRIBUTION
        .lock()
        .unwrap()
        .remove(opportunity_id)
}

pub fn smart_money_opportunity_attribution(
    opportunity_id: &Uuid,
) -> Option<Vec<SmartMoneyLeaderAttributionSlice>> {
    SMART_MONEY_OPPORTUNITY_ATTRIBUTION
        .lock()
        .unwrap()
        .get(opportunity_id)
        .cloned()
}

pub fn clear_smart_money_opportunity_attribution() {
    SMART_MONEY_OPPORTUNITY_ATTRIBUTION.lock().unwrap().clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use std::str::FromStr;

    fn sample_exit(recorded_at: DateTime<Utc>) -> CryptoExitDecision {
        CryptoExitDecision {
            recorded_at,
            asset: Some("Bitcoin".into()),
            reason: "capital_efficiency".into(),
            event_context_source: None,
            event_title: None,
            event_category: None,
            event_subtype: None,
            question: "Will Bitcoin hit $150k by tomorrow?".into(),
            market_type: Some("binary".into()),
            held_is_yes: Some(false),
            modeled_prob: None,
            days_to_resolution: Some(1),
            best_bid: Decimal::from_str("0.999").unwrap(),
            avg_cost: Decimal::from_str("0.44").unwrap(),
            size: Decimal::from_str("0.01").unwrap(),
        }
    }

    #[test]
    fn record_crypto_exit_decision_deduplicates_identical_recent_exit() {
        clear_crypto_exit_decisions();
        let now = Utc::now();
        record_crypto_exit_decision(sample_exit(now));
        record_crypto_exit_decision(sample_exit(now + Duration::seconds(1)));
        let exits = recent_crypto_exit_decisions();
        assert_eq!(exits.len(), 1);
        clear_crypto_exit_decisions();
    }

    #[test]
    fn record_crypto_exit_decision_keeps_distinct_or_stale_exit() {
        clear_crypto_exit_decisions();
        let now = Utc::now();
        record_crypto_exit_decision(sample_exit(now));
        let mut changed_bid = sample_exit(now + Duration::seconds(1));
        changed_bid.best_bid = Decimal::from_str("0.998").unwrap();
        record_crypto_exit_decision(changed_bid);
        record_crypto_exit_decision(sample_exit(now + Duration::seconds(6)));
        let exits = recent_crypto_exit_decisions();
        assert_eq!(exits.len(), 2);
        clear_crypto_exit_decisions();
    }

    #[test]
    fn record_crypto_exit_decision_keeps_distinct_reason_within_window() {
        clear_crypto_exit_decisions();
        let now = Utc::now();
        record_crypto_exit_decision(sample_exit(now));
        let mut changed_reason = sample_exit(now + Duration::seconds(1));
        changed_reason.reason = "model_reversal".into();
        record_crypto_exit_decision(changed_reason);
        let exits = recent_crypto_exit_decisions();
        assert_eq!(exits.len(), 2);
        clear_crypto_exit_decisions();
    }

    #[test]
    fn record_crypto_exit_decision_deduplicates_interleaved_recent_exit() {
        clear_crypto_exit_decisions();
        let now = Utc::now();
        record_crypto_exit_decision(sample_exit(now));

        let mut distinct_reason = sample_exit(now + Duration::seconds(1));
        distinct_reason.reason = "model_reversal".into();
        record_crypto_exit_decision(distinct_reason);

        record_crypto_exit_decision(sample_exit(now + Duration::seconds(2)));

        let exits = recent_crypto_exit_decisions();
        assert_eq!(exits.len(), 2);
        clear_crypto_exit_decisions();
    }
}
