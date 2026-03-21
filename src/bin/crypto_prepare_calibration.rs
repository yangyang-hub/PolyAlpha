use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use clap::Parser;
use pa_monitor::diagnostics::{CryptoCandidateDecision, CryptoExitDecision};
use serde::{Deserialize, Serialize};

/// Join exported crypto diagnostics with labeled outcomes into crypto_calibrate samples.
#[derive(Debug, Parser)]
#[command(
    name = "crypto-prepare-calibration",
    about = "Prepare crypto_calibrate JSONL from exported diagnostics plus labeled resolutions"
)]
struct Args {
    /// Path to JSONL exported by `crypto_export_diagnostics`.
    #[arg(long)]
    diagnostics: PathBuf,

    /// Path to JSONL labels keyed by question.
    #[arg(long)]
    labels: PathBuf,

    /// Output JSONL path suitable for `crypto_calibrate`.
    #[arg(long)]
    output: PathBuf,

    /// Optional JSON summary output path.
    #[arg(long)]
    summary_output: Option<PathBuf>,

    /// Keep only `replace` candidate decisions.
    #[arg(long, default_value_t = false)]
    replace_only: bool,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum DiagnosticRow {
    CandidateDecision(CryptoCandidateDecision),
    #[allow(dead_code)]
    ExitDecision(CryptoExitDecision),
}

#[derive(Debug, Deserialize)]
struct ResolutionLabel {
    question: String,
    resolved_yes: Option<bool>,
    resolved_value: Option<f64>,
    resolution_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize)]
struct CalibrationSample {
    question: String,
    asset: String,
    asset_class: String,
    market_type: String,
    event_subtype: Option<String>,
    observed_at: DateTime<Utc>,
    resolution_at: Option<DateTime<Utc>>,
    days_to_resolution: Option<u32>,
    modeled_prob: f64,
    resolved_value: f64,
}

#[derive(Debug, Clone)]
struct CandidateSampleSeed {
    question: String,
    asset: String,
    asset_class: String,
    market_type: String,
    event_subtype: Option<String>,
    observed_at: DateTime<Utc>,
    modeled_prob: f64,
    is_yes: bool,
    days_to_resolution: u32,
}

#[derive(Debug, Default, Serialize)]
struct PreparationSummary {
    total_candidate_rows: usize,
    matched_labels: usize,
    emitted_samples: usize,
    missing_label_rows: usize,
    invalid_label_rows: usize,
    by_asset: BTreeMap<String, usize>,
    by_asset_class: BTreeMap<String, usize>,
    by_market_type: BTreeMap<String, usize>,
    by_event_subtype: BTreeMap<String, usize>,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let seeds = load_candidate_seeds(&args)?;
    let labels = load_labels(&args.labels)?;
    let mut summary = PreparationSummary {
        total_candidate_rows: seeds.len(),
        ..PreparationSummary::default()
    };

    let file = File::create(&args.output)
        .with_context(|| format!("failed to create output file {}", args.output.display()))?;
    let mut writer = BufWriter::new(file);

    for seed in seeds.values() {
        let Some(label) = labels.get(&seed.question) else {
            summary.missing_label_rows += 1;
            continue;
        };
        summary.matched_labels += 1;
        let resolved_value = match held_side_resolved_value(seed.is_yes, label) {
            Ok(value) => value,
            Err(_) => {
                summary.invalid_label_rows += 1;
                continue;
            }
        };
        let days_to_resolution = label
            .resolution_at
            .map(|resolution_at| {
                let duration = resolution_at.signed_duration_since(seed.observed_at);
                if duration.num_seconds() <= 0 {
                    1
                } else {
                    (duration.num_seconds() as f64 / 86_400.0).ceil() as u32
                }
            })
            .or(Some(seed.days_to_resolution));
        let sample = CalibrationSample {
            question: seed.question.clone(),
            asset: seed.asset.clone(),
            asset_class: seed.asset_class.clone(),
            market_type: seed.market_type.clone(),
            event_subtype: seed.event_subtype.clone(),
            observed_at: seed.observed_at,
            resolution_at: label.resolution_at,
            days_to_resolution,
            modeled_prob: seed.modeled_prob,
            resolved_value,
        };
        serde_json::to_writer(&mut writer, &sample).context("failed to serialize sample")?;
        writer
            .write_all(b"\n")
            .context("failed to write sample newline")?;
        summary.emitted_samples += 1;
        *summary.by_asset.entry(seed.asset.clone()).or_default() += 1;
        *summary
            .by_asset_class
            .entry(seed.asset_class.clone())
            .or_default() += 1;
        *summary
            .by_market_type
            .entry(seed.market_type.clone())
            .or_default() += 1;
        *summary
            .by_event_subtype
            .entry(
                seed.event_subtype
                    .clone()
                    .unwrap_or_else(|| "any".to_string()),
            )
            .or_default() += 1;
    }
    writer.flush().context("failed to flush output")?;
    print_summary(&summary);
    write_summary_json(&summary, args.summary_output.as_ref())?;
    Ok(())
}

fn load_candidate_seeds(args: &Args) -> Result<BTreeMap<(String, bool), CandidateSampleSeed>> {
    let file = File::open(&args.diagnostics).with_context(|| {
        format!(
            "failed to open diagnostics file {}",
            args.diagnostics.display()
        )
    })?;
    let reader = BufReader::new(file);
    let mut seeds: BTreeMap<(String, bool), CandidateSampleSeed> = BTreeMap::new();

    for (line_no, line) in reader.lines().enumerate() {
        let line = line.with_context(|| format!("failed to read line {}", line_no + 1))?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let row: DiagnosticRow = serde_json::from_str(trimmed)
            .with_context(|| format!("invalid JSON on line {}", line_no + 1))?;
        let DiagnosticRow::CandidateDecision(decision) = row else {
            continue;
        };
        if args.replace_only && decision.action != "replace" {
            continue;
        }
        let modeled_prob = decision
            .selected_modeled_prob
            .to_f64()
            .context("selected_modeled_prob could not convert to f64")?;
        let key = (decision.selected_question.clone(), decision.selected_is_yes);
        let asset = decision.asset;
        let asset_class = infer_asset_class(&asset);
        let seed = CandidateSampleSeed {
            question: decision.selected_question,
            asset,
            asset_class,
            market_type: decision.selected_market_type,
            event_subtype: decision
                .event_subtype
                .map(|value| value.trim().to_ascii_lowercase())
                .filter(|value| !value.is_empty()),
            observed_at: decision.recorded_at,
            modeled_prob,
            is_yes: decision.selected_is_yes,
            days_to_resolution: decision.selected_days_to_resolution,
        };
        match seeds.get(&key) {
            Some(existing) if existing.observed_at <= seed.observed_at => {}
            _ => {
                seeds.insert(key, seed);
            }
        }
    }

    Ok(seeds)
}

fn infer_asset_class(asset: &str) -> String {
    match asset.trim().to_ascii_uppercase().as_str() {
        "BTCUSDT" | "ETHUSDT" => "major".to_string(),
        _ => "alt".to_string(),
    }
}

fn load_labels(path: &PathBuf) -> Result<BTreeMap<String, ResolutionLabel>> {
    let file = File::open(path)
        .with_context(|| format!("failed to open labels file {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut labels = BTreeMap::new();

    for (line_no, line) in reader.lines().enumerate() {
        let line = line.with_context(|| format!("failed to read line {}", line_no + 1))?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let label: ResolutionLabel = serde_json::from_str(trimmed)
            .with_context(|| format!("invalid labels JSON on line {}", line_no + 1))?;
        labels.insert(label.question.clone(), label);
    }

    Ok(labels)
}

fn held_side_resolved_value(is_yes: bool, label: &ResolutionLabel) -> Result<f64> {
    if let Some(value) = label.resolved_value {
        validate_probability(value)?;
        return Ok(if is_yes { value } else { 1.0 - value });
    }
    if let Some(yes) = label.resolved_yes {
        let yes_value = if yes { 1.0 } else { 0.0 };
        return Ok(if is_yes { yes_value } else { 1.0 - yes_value });
    }
    anyhow::bail!("missing resolved_yes or resolved_value")
}

fn validate_probability(value: f64) -> Result<()> {
    if !value.is_finite() || !(0.0..=1.0).contains(&value) {
        anyhow::bail!("probability must be between 0 and 1");
    }
    Ok(())
}

trait DecimalToF64 {
    fn to_f64(&self) -> Option<f64>;
}

impl DecimalToF64 for rust_decimal::Decimal {
    fn to_f64(&self) -> Option<f64> {
        rust_decimal::prelude::ToPrimitive::to_f64(self)
    }
}

fn print_summary(summary: &PreparationSummary) {
    println!(
        "# crypto_prepare_calibration summary total_candidates={} matched_labels={} emitted_samples={} missing_labels={} invalid_labels={}",
        summary.total_candidate_rows,
        summary.matched_labels,
        summary.emitted_samples,
        summary.missing_label_rows,
        summary.invalid_label_rows
    );
    println!("# by_asset");
    for (asset, count) in &summary.by_asset {
        println!("#   {}={}", asset, count);
    }
    println!("# by_asset_class");
    for (asset_class, count) in &summary.by_asset_class {
        println!("#   {}={}", asset_class, count);
    }
    println!("# by_market_type");
    for (market_type, count) in &summary.by_market_type {
        println!("#   {}={}", market_type, count);
    }
    println!("# by_event_subtype");
    for (event_subtype, count) in &summary.by_event_subtype {
        println!("#   {}={}", event_subtype, count);
    }
}

fn write_summary_json(summary: &PreparationSummary, path: Option<&PathBuf>) -> Result<()> {
    let Some(path) = path else {
        return Ok(());
    };
    let file = File::create(path)
        .with_context(|| format!("failed to create summary output {}", path.display()))?;
    let writer = BufWriter::new(file);
    serde_json::to_writer_pretty(writer, summary).context("failed to serialize summary JSON")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flips_resolution_for_no_side() {
        let label = ResolutionLabel {
            question: "q".into(),
            resolved_yes: Some(true),
            resolved_value: None,
            resolution_at: None,
        };
        assert_eq!(held_side_resolved_value(false, &label).unwrap(), 0.0);
    }

    #[test]
    fn keeps_resolution_for_yes_side() {
        let label = ResolutionLabel {
            question: "q".into(),
            resolved_yes: None,
            resolved_value: Some(0.25),
            resolution_at: None,
        };
        assert_eq!(held_side_resolved_value(true, &label).unwrap(), 0.25);
    }

    #[test]
    fn summary_tracks_asset_and_market_type_counts() {
        let mut summary = PreparationSummary::default();
        summary.emitted_samples = 2;
        *summary.by_asset.entry("BTCUSDT".into()).or_default() += 1;
        *summary.by_asset.entry("ETHUSDT".into()).or_default() += 1;
        *summary.by_asset_class.entry("major".into()).or_default() += 2;
        *summary.by_market_type.entry("binary".into()).or_default() += 2;
        *summary.by_event_subtype.entry("unlock".into()).or_default() += 1;

        assert_eq!(summary.by_asset.get("BTCUSDT"), Some(&1));
        assert_eq!(summary.by_asset_class.get("major"), Some(&2));
        assert_eq!(summary.by_market_type.get("binary"), Some(&2));
        assert_eq!(summary.by_event_subtype.get("unlock"), Some(&1));
    }

    #[test]
    fn summary_json_is_skipped_when_path_missing() {
        let summary = PreparationSummary::default();
        assert!(write_summary_json(&summary, None).is_ok());
    }
}
