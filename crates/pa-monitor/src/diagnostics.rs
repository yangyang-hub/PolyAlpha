use std::collections::VecDeque;
use std::sync::{LazyLock, Mutex};

use alloy::primitives::B256;
use chrono::{DateTime, Duration, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

const MAX_CRYPTO_DECISIONS: usize = 100;
const MAX_CRYPTO_EXITS: usize = 100;
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

static CRYPTO_CANDIDATE_DECISIONS: LazyLock<Mutex<VecDeque<CryptoCandidateDecision>>> =
    LazyLock::new(|| Mutex::new(VecDeque::new()));
static CRYPTO_EXIT_DECISIONS: LazyLock<Mutex<VecDeque<CryptoExitDecision>>> =
    LazyLock::new(|| Mutex::new(VecDeque::new()));

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
    if let Some(latest) = entries.front() {
        let within_dedup_window = (entry.recorded_at - latest.recorded_at)
            <= Duration::seconds(CRYPTO_EXIT_DEDUP_WINDOW_SECS);
        let same_exit = latest.asset == entry.asset
            && latest.reason == entry.reason
            && latest.event_context_source == entry.event_context_source
            && latest.event_title == entry.event_title
            && latest.event_category == entry.event_category
            && latest.event_subtype == entry.event_subtype
            && latest.question == entry.question
            && latest.market_type == entry.market_type
            && latest.held_is_yes == entry.held_is_yes
            && latest.modeled_prob == entry.modeled_prob
            && latest.days_to_resolution == entry.days_to_resolution
            && latest.best_bid == entry.best_bid
            && latest.avg_cost == entry.avg_cost
            && latest.size == entry.size;
        if within_dedup_window && same_exit {
            return;
        }
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
        assert_eq!(exits.len(), 3);
        clear_crypto_exit_decisions();
    }
}
