use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::{Arc, RwLock};

use alloy::primitives::{B256, U256};
use anyhow::{Context, Result};
use arc_swap::ArcSwap;
use chrono::{DateTime, Utc};
use clap::Parser;
use pa_core::config::Settings;
use pa_core::traits::Strategy;
use pa_core::types::{
    ExecutionPlan, MarketInfo, OrderBook, Outcome, PriceLevel, TokenInfo, TradeSide,
};
use pa_market_data::wallet_tracker::{SignalType, SmartMoneySignal, SmartMoneySignalSource};
use pa_monitor::diagnostics::{
    clear_smart_money_decisions, clear_smart_money_exit_decisions, clear_smart_money_wallet_scores,
    recent_smart_money_decisions, recent_smart_money_exit_decisions,
};
use pa_strategy::profitability::ProfitCalculator;
use pa_strategy::smart_money::{SmartMoneyStrategy, SmartMoneyStrategyDeps};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

#[derive(Debug, Parser)]
#[command(
    name = "smart-money-replay",
    about = "Replay smart-money JSONL snapshots through the current SmartMoney strategy"
)]
struct Args {
    /// JSONL input path. Each line is a market snapshot with an optional smart-money signal.
    #[arg(long)]
    input: PathBuf,

    /// Initial simulated USDC balance.
    #[arg(long, default_value = "1000")]
    initial_balance: Decimal,

    /// Output format.
    #[arg(long, default_value = "text", value_parser = ["text", "json"])]
    output: String,

    /// Optional JSON summary output path.
    #[arg(long)]
    summary_output: Option<PathBuf>,

    /// Optional JSON trace output path containing per-step replay details.
    #[arg(long)]
    trace_output: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
struct ReplayLine {
    timestamp: DateTime<Utc>,
    token_id: String,
    condition_id: String,
    question: String,
    #[serde(default)]
    signal_type: Option<String>,
    #[serde(default)]
    wallet_address: Option<String>,
    #[serde(default)]
    wallet_label: Option<String>,
    #[serde(default)]
    wallet_weight: Option<Decimal>,
    #[serde(default)]
    wallet_size: Option<Decimal>,
    #[serde(default)]
    delta: Option<Decimal>,
    #[serde(default)]
    source: Option<String>,
    best_bid: Decimal,
    best_bid_size: Decimal,
    best_ask: Decimal,
    best_ask_size: Decimal,
    #[serde(default)]
    fee_rate_bps: Option<u32>,
    #[serde(default)]
    liquidity: Option<Decimal>,
}

#[derive(Debug, Clone, Default)]
struct ReplayPosition {
    size: Decimal,
    avg_cost: Decimal,
    condition_id: B256,
}

#[derive(Debug, Serialize)]
struct ReplayPositionSummary {
    token_id: String,
    condition_id: String,
    size: Decimal,
    avg_cost: Decimal,
}

#[derive(Debug, Serialize)]
struct ReplaySummary {
    samples_processed: usize,
    signals_enqueued: usize,
    opportunities_generated: usize,
    buys_executed: usize,
    sells_executed: usize,
    final_cash: Decimal,
    realized_pnl: Decimal,
    final_positions: Vec<ReplayPositionSummary>,
    recent_decision_count: usize,
    accepted_decision_count: usize,
    rejected_decision_count: usize,
    reject_reason_counts: Vec<LabelCount>,
    exit_reason_counts: Vec<LabelCount>,
    recent_decisions: Vec<ReplayDecisionSummary>,
    recent_exits: Vec<ReplayExitSummary>,
}

#[derive(Debug, Serialize)]
struct LabelCount {
    label: String,
    count: usize,
}

#[derive(Debug, Serialize)]
struct ReplayDecisionSummary {
    recorded_at: DateTime<Utc>,
    token_id: String,
    signal_type: String,
    accepted: bool,
    reject_reason: Option<String>,
    wallet_count: usize,
    max_wallet_weight: Decimal,
}

#[derive(Debug, Serialize)]
struct ReplayExitSummary {
    recorded_at: DateTime<Utc>,
    token_id: String,
    reason: String,
    question: String,
    best_bid: Decimal,
    avg_cost: Decimal,
    size: Decimal,
}

#[derive(Debug, Serialize)]
struct ReplayTraceEntry {
    timestamp: DateTime<Utc>,
    token_id: String,
    condition_id: String,
    signal_type: Option<String>,
    opportunities_generated: usize,
    buys_executed: usize,
    sells_executed: usize,
    cash_after: Decimal,
    realized_pnl_after: Decimal,
    open_positions: usize,
}

fn parse_token_id(raw: &str) -> Result<U256> {
    if let Some(hex) = raw.strip_prefix("0x") {
        U256::from_str_radix(hex, 16).context("invalid hex token_id")
    } else {
        U256::from_str(raw).context("invalid decimal token_id")
    }
}

fn parse_condition_id(raw: &str) -> Result<B256> {
    B256::from_str(raw).context("invalid condition_id")
}

fn parse_signal_type(raw: &str) -> Result<SignalType> {
    match raw {
        "entry" => Ok(SignalType::Entry),
        "increase" => Ok(SignalType::Increase),
        "decrease" => Ok(SignalType::Decrease),
        "exit" => Ok(SignalType::Exit),
        _ => anyhow::bail!("unsupported signal_type: {raw}"),
    }
}

fn parse_signal_source(raw: Option<&str>) -> SmartMoneySignalSource {
    match raw {
        Some("onchain") => SmartMoneySignalSource::Onchain,
        _ => SmartMoneySignalSource::DataApi,
    }
}

fn build_market(line: &ReplayLine, token_id: U256, condition_id: B256) -> MarketInfo {
    MarketInfo {
        condition_id,
        question_id: B256::ZERO,
        question: line.question.clone(),
        neg_risk: false,
        neg_risk_market_id: None,
        tokens: vec![TokenInfo {
            token_id,
            outcome: Outcome::Yes,
            complement_id: U256::ZERO,
        }],
        tick_size: Decimal::new(1, 2),
        fee_rate_bps: line.fee_rate_bps.unwrap_or(200),
        active: true,
        liquidity: line.liquidity.unwrap_or(Decimal::from(1000)),
        event_title: None,
        end_date: None,
        category: Some("smart_money_replay".into()),
        outcome_prices: None,
        gamma_best_bid: Some(line.best_bid),
        gamma_best_ask: Some(line.best_ask),
        rewards_min_size: None,
        rewards_max_spread: None,
        rewards_daily_rate: None,
        holding_rewards_enabled: false,
        fees_enabled: true,
    }
}

fn build_orderbook(line: &ReplayLine, token_id: U256) -> OrderBook {
    OrderBook {
        token_id,
        bids: vec![PriceLevel {
            price: line.best_bid,
            size: line.best_bid_size,
        }],
        asks: vec![PriceLevel {
            price: line.best_ask,
            size: line.best_ask_size,
        }],
        timestamp: line.timestamp,
    }
}

fn build_signal(
    line: &ReplayLine,
    token_id: U256,
    condition_id: B256,
) -> Result<Option<SmartMoneySignal>> {
    let Some(signal_type) = line.signal_type.as_deref() else {
        return Ok(None);
    };
    let signal_type = parse_signal_type(signal_type)?;
    let delta = line.delta.unwrap_or(Decimal::ZERO);
    Ok(Some(SmartMoneySignal {
        signal_type,
        wallet_address: line
            .wallet_address
            .clone()
            .unwrap_or_else(|| "replay_wallet".into()),
        wallet_label: line.wallet_label.clone(),
        wallet_weight: line.wallet_weight.unwrap_or(Decimal::ONE),
        token_id,
        condition_id,
        wallet_size: line.wallet_size.unwrap_or(delta),
        delta,
        signal_notional_usdc: delta,
        source: parse_signal_source(line.source.as_deref()),
        detected_at: line.timestamp,
    }))
}

fn sorted_counts(map: HashMap<String, usize>) -> Vec<LabelCount> {
    let mut entries: Vec<_> = map
        .into_iter()
        .map(|(label, count)| LabelCount { label, count })
        .collect();
    entries.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.label.cmp(&b.label)));
    entries
}

fn write_summary(summary: &ReplaySummary, output: &str, path: Option<&PathBuf>) -> Result<()> {
    match output {
        "json" => {
            let rendered = serde_json::to_string_pretty(summary)
                .context("failed to serialize replay summary")?;
            println!("{rendered}");
            if let Some(path) = path {
                let file = File::create(path).with_context(|| {
                    format!("failed to create summary output: {}", path.display())
                })?;
                let mut writer = BufWriter::new(file);
                writer.write_all(rendered.as_bytes())?;
                writer.write_all(b"\n")?;
                writer.flush()?;
            }
        }
        _ => {
            println!("samples_processed: {}", summary.samples_processed);
            println!("signals_enqueued: {}", summary.signals_enqueued);
            println!(
                "opportunities_generated: {}",
                summary.opportunities_generated
            );
            println!("buys_executed: {}", summary.buys_executed);
            println!("sells_executed: {}", summary.sells_executed);
            println!("final_cash: {}", summary.final_cash);
            println!("realized_pnl: {}", summary.realized_pnl);
            println!("final_positions: {}", summary.final_positions.len());
            if !summary.reject_reason_counts.is_empty() {
                println!("reject_reason_counts:");
                for entry in &summary.reject_reason_counts {
                    println!("  {}: {}", entry.label, entry.count);
                }
            }
            if !summary.exit_reason_counts.is_empty() {
                println!("exit_reason_counts:");
                for entry in &summary.exit_reason_counts {
                    println!("  {}: {}", entry.label, entry.count);
                }
            }
        }
    }
    Ok(())
}

fn write_trace(entries: &[ReplayTraceEntry], path: Option<&PathBuf>) -> Result<()> {
    let Some(path) = path else {
        return Ok(());
    };
    let rendered =
        serde_json::to_string_pretty(entries).context("failed to serialize replay trace")?;
    let file = File::create(path)
        .with_context(|| format!("failed to create trace output: {}", path.display()))?;
    let mut writer = BufWriter::new(file);
    writer.write_all(rendered.as_bytes())?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    let args = Args::parse();
    let settings = Settings::load().context("failed to load configuration")?;
    let config = settings.smart_money.clone();

    clear_smart_money_decisions();
    clear_smart_money_exit_decisions();
    clear_smart_money_wallet_scores();

    let file = File::open(&args.input)
        .with_context(|| format!("failed to open replay input: {}", args.input.display()))?;
    let reader = BufReader::new(file);

    let orderbooks: Arc<RwLock<HashMap<U256, OrderBook>>> = Arc::new(RwLock::new(HashMap::new()));
    let positions: Arc<RwLock<HashMap<U256, ReplayPosition>>> =
        Arc::new(RwLock::new(HashMap::new()));
    let signals: Arc<RwLock<Vec<SmartMoneySignal>>> = Arc::new(RwLock::new(Vec::new()));
    let replay_now = Arc::new(RwLock::new(Utc::now()));
    let cash = Arc::new(RwLock::new(args.initial_balance));
    let market_by_condition: Arc<RwLock<HashMap<B256, MarketInfo>>> =
        Arc::new(RwLock::new(HashMap::new()));
    let profit_calc = ProfitCalculator::new(Decimal::ZERO);

    let strategy = SmartMoneyStrategy::new(
        Arc::new(ArcSwap::from_pointee(config)),
        Decimal::ZERO,
        SmartMoneyStrategyDeps {
            get_orderbook: Box::new({
                let orderbooks = Arc::clone(&orderbooks);
                move |token_id| orderbooks.read().unwrap().get(&token_id).cloned()
            }),
            get_available_capital: Box::new({
                let cash = Arc::clone(&cash);
                move || *cash.read().unwrap()
            }),
            get_position: Box::new({
                let positions = Arc::clone(&positions);
                move |token_id| {
                    positions
                        .read()
                        .unwrap()
                        .get(&token_id)
                        .map(|position| position.size)
                        .unwrap_or(Decimal::ZERO)
                }
            }),
            get_held_positions: Box::new({
                let positions = Arc::clone(&positions);
                move || {
                    positions
                        .read()
                        .unwrap()
                        .iter()
                        .map(|(token_id, position)| (*token_id, position.size, position.avg_cost))
                        .collect()
                }
            }),
            now: Box::new({
                let replay_now = Arc::clone(&replay_now);
                move || *replay_now.read().unwrap()
            }),
            signals: Arc::clone(&signals),
            markets: Arc::clone(&market_by_condition),
        },
    );

    let mut samples_processed = 0usize;
    let mut signals_enqueued = 0usize;
    let mut opportunities_generated = 0usize;
    let mut buys_executed = 0usize;
    let mut sells_executed = 0usize;
    let mut realized_pnl = Decimal::ZERO;
    let mut trace_entries = Vec::new();

    for (line_no, line) in reader.lines().enumerate() {
        let line = line.with_context(|| format!("failed reading line {}", line_no + 1))?;
        if line.trim().is_empty() {
            continue;
        }
        let sample: ReplayLine = serde_json::from_str(&line)
            .with_context(|| format!("invalid JSON on line {}", line_no + 1))?;
        let token_id = parse_token_id(&sample.token_id)?;
        let condition_id = parse_condition_id(&sample.condition_id)?;
        *replay_now.write().unwrap() = sample.timestamp;

        let market = build_market(&sample, token_id, condition_id);
        let orderbook = build_orderbook(&sample, token_id);
        orderbooks.write().unwrap().insert(token_id, orderbook);
        market_by_condition
            .write()
            .unwrap()
            .insert(condition_id, market.clone());

        let signal = build_signal(&sample, token_id, condition_id)?;
        if let Some(signal) = signal.clone() {
            signals.write().unwrap().push(signal);
            signals_enqueued += 1;
        }

        let opportunities = strategy.scan(&[market.clone()]).await?;
        let line_opportunities = opportunities.len();
        opportunities_generated += line_opportunities;
        let mut line_buys = 0usize;
        let mut line_sells = 0usize;
        for opportunity in opportunities {
            let ExecutionPlan::DirectionalBuy {
                token_id,
                side,
                price,
                size,
                condition_id,
            } = opportunity.execution_plan;
            let fee_per_share = profit_calc.capped_fee(price, market.fee_rate_bps);
            match side {
                TradeSide::Buy => {
                    let total_cost = (price + fee_per_share) * size;
                    let mut cash_guard = cash.write().unwrap();
                    if *cash_guard < total_cost {
                        continue;
                    }
                    *cash_guard -= total_cost;
                    drop(cash_guard);
                    let mut positions_guard = positions.write().unwrap();
                    let entry = positions_guard.entry(token_id).or_insert(ReplayPosition {
                        size: Decimal::ZERO,
                        avg_cost: Decimal::ZERO,
                        condition_id,
                    });
                    let previous_cost_basis = entry.avg_cost * entry.size;
                    let new_cost_basis = previous_cost_basis + total_cost;
                    entry.size += size;
                    entry.avg_cost = if entry.size > Decimal::ZERO {
                        new_cost_basis / entry.size
                    } else {
                        Decimal::ZERO
                    };
                    entry.condition_id = condition_id;
                    buys_executed += 1;
                    line_buys += 1;
                }
                TradeSide::Sell => {
                    let mut positions_guard = positions.write().unwrap();
                    let Some(entry) = positions_guard.get_mut(&token_id) else {
                        continue;
                    };
                    let filled = size.min(entry.size);
                    if filled <= Decimal::ZERO {
                        continue;
                    }
                    let est = profit_calc.directional_sell_profit(
                        price,
                        entry.avg_cost,
                        filled,
                        market.fee_rate_bps,
                    );
                    realized_pnl += est.net_profit;
                    let mut cash_guard = cash.write().unwrap();
                    *cash_guard += (price - fee_per_share) * filled;
                    drop(cash_guard);
                    entry.size -= filled;
                    if entry.size <= Decimal::ZERO {
                        positions_guard.remove(&token_id);
                    }
                    sells_executed += 1;
                    line_sells += 1;
                }
            }
        }
        trace_entries.push(ReplayTraceEntry {
            timestamp: sample.timestamp,
            token_id: sample.token_id.clone(),
            condition_id: sample.condition_id.clone(),
            signal_type: sample.signal_type.clone(),
            opportunities_generated: line_opportunities,
            buys_executed: line_buys,
            sells_executed: line_sells,
            cash_after: *cash.read().unwrap(),
            realized_pnl_after: realized_pnl,
            open_positions: positions.read().unwrap().len(),
        });
        samples_processed += 1;
    }

    let decisions = recent_smart_money_decisions();
    let exits = recent_smart_money_exit_decisions();
    let mut reject_reason_counts = HashMap::new();
    let mut exit_reason_counts = HashMap::new();
    for decision in &decisions {
        if !decision.accepted {
            *reject_reason_counts
                .entry(
                    decision
                        .reject_reason
                        .clone()
                        .unwrap_or_else(|| "rejected".into()),
                )
                .or_insert(0) += 1;
        }
    }
    for exit in &exits {
        *exit_reason_counts.entry(exit.reason.clone()).or_insert(0) += 1;
    }

    let final_positions = positions
        .read()
        .unwrap()
        .iter()
        .map(|(token_id, position)| ReplayPositionSummary {
            token_id: token_id.to_string(),
            condition_id: format!("{:#x}", position.condition_id),
            size: position.size,
            avg_cost: position.avg_cost,
        })
        .collect();

    let summary = ReplaySummary {
        samples_processed,
        signals_enqueued,
        opportunities_generated,
        buys_executed,
        sells_executed,
        final_cash: *cash.read().unwrap(),
        realized_pnl,
        final_positions,
        recent_decision_count: decisions.len(),
        accepted_decision_count: decisions
            .iter()
            .filter(|decision| decision.accepted)
            .count(),
        rejected_decision_count: decisions
            .iter()
            .filter(|decision| !decision.accepted)
            .count(),
        reject_reason_counts: sorted_counts(reject_reason_counts),
        exit_reason_counts: sorted_counts(exit_reason_counts),
        recent_decisions: decisions
            .iter()
            .take(12)
            .map(|decision| ReplayDecisionSummary {
                recorded_at: decision.recorded_at,
                token_id: decision.token_id.clone(),
                signal_type: decision.signal_type.clone(),
                accepted: decision.accepted,
                reject_reason: decision.reject_reason.clone(),
                wallet_count: decision.wallet_count,
                max_wallet_weight: decision.max_wallet_weight,
            })
            .collect(),
        recent_exits: exits
            .iter()
            .take(12)
            .map(|exit| ReplayExitSummary {
                recorded_at: exit.recorded_at,
                token_id: exit.token_id.clone(),
                reason: exit.reason.clone(),
                question: exit.question.clone(),
                best_bid: exit.best_bid,
                avg_cost: exit.avg_cost,
                size: exit.size,
            })
            .collect(),
    };

    write_summary(&summary, &args.output, args.summary_output.as_ref())?;
    write_trace(&trace_entries, args.trace_output.as_ref())?;
    Ok(())
}
