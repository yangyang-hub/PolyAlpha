use std::collections::VecDeque;
use std::sync::{LazyLock, Mutex};

use alloy::primitives::B256;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

const MAX_CRYPTO_DECISIONS: usize = 100;
const MAX_CRYPTO_EXITS: usize = 100;

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
