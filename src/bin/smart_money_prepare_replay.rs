use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use clap::Parser;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

#[derive(Debug, Parser)]
#[command(
    name = "smart-money-prepare-replay",
    about = "Normalize smart-money JSONL samples into smart_money_replay input"
)]
struct Args {
    /// Input JSONL path. Each row may omit optional replay fields.
    #[arg(long)]
    input: PathBuf,

    /// Output JSONL path suitable for smart_money_replay.
    #[arg(long)]
    output: PathBuf,

    /// Optional JSON summary output path.
    #[arg(long)]
    summary_output: Option<PathBuf>,

    /// Default source when a row omits `source`.
    #[arg(long, default_value = "data_api")]
    default_source: String,

    /// Default fee rate in bps when omitted.
    #[arg(long, default_value = "200")]
    default_fee_rate_bps: u32,

    /// Default market liquidity when omitted.
    #[arg(long, default_value = "1000")]
    default_liquidity: Decimal,

    /// Default top-of-book size on both sides when omitted.
    #[arg(long, default_value = "500")]
    default_book_size: Decimal,
}

#[derive(Debug, Deserialize)]
struct RawReplayLine {
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
    #[serde(default)]
    best_bid_size: Option<Decimal>,
    best_ask: Decimal,
    #[serde(default)]
    best_ask_size: Option<Decimal>,
    #[serde(default)]
    fee_rate_bps: Option<u32>,
    #[serde(default)]
    liquidity: Option<Decimal>,
}

#[derive(Debug, Serialize)]
struct ReplayLine {
    timestamp: DateTime<Utc>,
    token_id: String,
    condition_id: String,
    question: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    signal_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    wallet_address: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    wallet_label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    wallet_weight: Option<Decimal>,
    #[serde(skip_serializing_if = "Option::is_none")]
    wallet_size: Option<Decimal>,
    #[serde(skip_serializing_if = "Option::is_none")]
    delta: Option<Decimal>,
    source: String,
    best_bid: Decimal,
    best_bid_size: Decimal,
    best_ask: Decimal,
    best_ask_size: Decimal,
    fee_rate_bps: u32,
    liquidity: Decimal,
}

#[derive(Debug, Serialize, Default)]
struct PreparationSummary {
    input_rows: usize,
    emitted_rows: usize,
    signal_rows: usize,
    source_defaults_applied: usize,
    fee_defaults_applied: usize,
    liquidity_defaults_applied: usize,
    bid_size_defaults_applied: usize,
    ask_size_defaults_applied: usize,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let file = File::open(&args.input)
        .with_context(|| format!("failed to open input {}", args.input.display()))?;
    let reader = BufReader::new(file);
    let mut rows = Vec::new();
    let mut summary = PreparationSummary::default();

    for (line_no, line) in reader.lines().enumerate() {
        let line = line.with_context(|| format!("failed to read line {}", line_no + 1))?;
        if line.trim().is_empty() {
            continue;
        }
        let raw: RawReplayLine = serde_json::from_str(&line)
            .with_context(|| format!("invalid JSON on line {}", line_no + 1))?;
        summary.input_rows += 1;
        if raw.signal_type.is_some() {
            summary.signal_rows += 1;
        }
        if raw.source.is_none() {
            summary.source_defaults_applied += 1;
        }
        if raw.fee_rate_bps.is_none() {
            summary.fee_defaults_applied += 1;
        }
        if raw.liquidity.is_none() {
            summary.liquidity_defaults_applied += 1;
        }
        if raw.best_bid_size.is_none() {
            summary.bid_size_defaults_applied += 1;
        }
        if raw.best_ask_size.is_none() {
            summary.ask_size_defaults_applied += 1;
        }
        rows.push(ReplayLine {
            timestamp: raw.timestamp,
            token_id: raw.token_id,
            condition_id: raw.condition_id,
            question: raw.question,
            signal_type: raw.signal_type,
            wallet_address: raw.wallet_address,
            wallet_label: raw.wallet_label,
            wallet_weight: raw.wallet_weight,
            wallet_size: raw.wallet_size,
            delta: raw.delta,
            source: raw.source.unwrap_or_else(|| args.default_source.clone()),
            best_bid: raw.best_bid,
            best_bid_size: raw.best_bid_size.unwrap_or(args.default_book_size),
            best_ask: raw.best_ask,
            best_ask_size: raw.best_ask_size.unwrap_or(args.default_book_size),
            fee_rate_bps: raw.fee_rate_bps.unwrap_or(args.default_fee_rate_bps),
            liquidity: raw.liquidity.unwrap_or(args.default_liquidity),
        });
    }

    rows.sort_by(|a, b| {
        a.timestamp
            .cmp(&b.timestamp)
            .then_with(|| a.token_id.cmp(&b.token_id))
            .then_with(|| a.condition_id.cmp(&b.condition_id))
    });
    summary.emitted_rows = rows.len();

    let output = File::create(&args.output)
        .with_context(|| format!("failed to create output {}", args.output.display()))?;
    let mut writer = BufWriter::new(output);
    for row in rows {
        serde_json::to_writer(&mut writer, &row).context("failed to serialize replay row")?;
        writer
            .write_all(b"\n")
            .context("failed to write replay newline")?;
    }
    writer.flush().context("failed to flush output")?;

    if let Some(path) = args.summary_output.as_ref() {
        let file = File::create(path)
            .with_context(|| format!("failed to create summary output {}", path.display()))?;
        let mut writer = BufWriter::new(file);
        serde_json::to_writer_pretty(&mut writer, &summary)
            .context("failed to serialize summary")?;
        writer.write_all(b"\n")?;
        writer.flush()?;
    }

    println!("input_rows: {}", summary.input_rows);
    println!("emitted_rows: {}", summary.emitted_rows);
    println!("signal_rows: {}", summary.signal_rows);
    println!(
        "defaults_applied: source={}, fee={}, liquidity={}, bid_size={}, ask_size={}",
        summary.source_defaults_applied,
        summary.fee_defaults_applied,
        summary.liquidity_defaults_applied,
        summary.bid_size_defaults_applied,
        summary.ask_size_defaults_applied
    );
    Ok(())
}
