use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use clap::{Parser, ValueEnum};
use pa_core::config::CryptoCalibrationOverride;
use pa_strategy::crypto_alpha::{find_asset, parse_crypto_outcome_range, parse_crypto_question};
use serde::{Deserialize, Serialize};

/// Generate crypto calibration override suggestions from historical labeled samples.
///
/// Input is newline-delimited JSON. Each line should provide:
/// - `modeled_prob`: raw model probability before calibration, between 0 and 1
/// - `resolved_yes` or `resolved_value`: realized outcome as 0/1
/// - either explicit `asset`, `market_type`, `days_to_resolution`
///   or enough fields to infer them from `question` and timestamps
#[derive(Debug, Parser)]
#[command(
    name = "crypto-calibrate",
    about = "Generate crypto calibration_overrides suggestions from JSONL samples"
)]
struct Args {
    /// Path to newline-delimited JSON input samples.
    #[arg(long)]
    input: PathBuf,

    /// Minimum samples required before a segment is emitted.
    #[arg(long, default_value_t = 20)]
    min_samples: usize,

    /// Days-to-resolution threshold for the `short` horizon bucket.
    #[arg(long, default_value_t = 1)]
    short_horizon_max_days: u32,

    /// Days-to-resolution threshold for the `medium` horizon bucket.
    #[arg(long, default_value_t = 7)]
    medium_horizon_max_days: u32,

    /// Lower clamp for fitted probability calibration factors.
    #[arg(long, default_value_t = 0.60)]
    min_factor: f64,

    /// Upper clamp for fitted probability calibration factors.
    #[arg(long, default_value_t = 1.10)]
    max_factor: f64,

    /// Collapse per-asset grouping into `asset_class` (`major` / `alt`) selectors.
    #[arg(long, default_value_t = false)]
    group_by_asset_class: bool,

    /// Split segments by event subtype (`unlock` / `upgrade` / `regulatory` / `other`).
    #[arg(long, default_value_t = false)]
    group_by_event_subtype: bool,

    /// Optional JSON summary output path.
    #[arg(long)]
    summary_output: Option<PathBuf>,

    /// Optional TOML output path for merge-ready calibration_overrides suggestions.
    #[arg(long)]
    override_output: Option<PathBuf>,

    /// Optional existing TOML config/fragment used to merge probability_calibration updates.
    #[arg(long)]
    existing_overrides_input: Option<PathBuf>,

    /// Merge behavior when `--existing-overrides-input` is provided.
    #[arg(long, value_enum, default_value_t = MergeMode::ProbabilityOnly)]
    merge_mode: MergeMode,
}

#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
enum MergeMode {
    ProbabilityOnly,
    AppendOnly,
    ReplaceRow,
}

#[derive(Debug, Deserialize)]
struct CalibrationSampleLine {
    question: Option<String>,
    asset: Option<String>,
    asset_class: Option<String>,
    market_type: Option<String>,
    event_subtype: Option<String>,
    days_to_resolution: Option<u32>,
    observed_at: Option<DateTime<Utc>>,
    resolution_at: Option<DateTime<Utc>>,
    modeled_prob: f64,
    resolved_yes: Option<bool>,
    resolved_value: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
struct SegmentKey {
    asset: String,
    asset_class: String,
    horizon: String,
    market_type: String,
    event_subtype: String,
}

#[derive(Debug, Clone)]
struct SampleObservation {
    modeled_prob: f64,
    resolved_value: f64,
}

#[derive(Debug, Clone, Serialize)]
struct SegmentSummary {
    count: usize,
    probability_calibration: f64,
    brier_before: f64,
    brier_after: f64,
}

#[derive(Debug, Clone, Serialize)]
struct SkippedSegmentSummary {
    key: SegmentKey,
    count: usize,
    reason: String,
}

#[derive(Debug, Serialize)]
struct CalibrationSummaryJson {
    input_rows: usize,
    emitted_segment_count: usize,
    skipped_segment_count: usize,
    min_samples: usize,
    grouping: CalibrationGroupingSummary,
    emitted_segments: Vec<CalibrationSummaryEntry>,
    skipped_segments: Vec<SkippedSegmentSummary>,
    underfilled_buckets: Vec<UnderfilledBucketSummary>,
    merge_diff_summary: Option<MergeDiffSummaryJson>,
}

#[derive(Debug, Serialize)]
struct CalibrationGroupingSummary {
    group_by_asset_class: bool,
    group_by_event_subtype: bool,
    short_horizon_max_days: u32,
    medium_horizon_max_days: u32,
}

#[derive(Debug, Clone, Serialize)]
struct CalibrationSummaryEntry {
    key: SegmentKey,
    summary: SegmentSummary,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
struct UnderfilledBucketKey {
    asset_class: String,
    horizon: String,
    event_subtype: String,
}

#[derive(Debug, Clone, Serialize)]
struct UnderfilledBucketSummary {
    key: UnderfilledBucketKey,
    skipped_segment_count: usize,
    skipped_row_count: usize,
    gap_to_min_samples: usize,
    threshold_band: String,
}

#[derive(Debug, Deserialize, Default)]
struct ExistingConfigDoc {
    #[serde(default)]
    crypto_alpha: ExistingCryptoAlphaSection,
}

#[derive(Debug, Deserialize, Default)]
struct ExistingCryptoAlphaSection {
    #[serde(default)]
    calibration_overrides: Vec<CryptoCalibrationOverride>,
}

#[derive(Debug, Default)]
struct MergeDiffSummary {
    new_rows: Vec<String>,
    updated_rows: Vec<String>,
    unchanged_rows: Vec<String>,
}

#[derive(Debug, Serialize)]
struct MergeDiffSummaryJson {
    new_row_count: usize,
    updated_row_count: usize,
    unchanged_row_count: usize,
    new_rows: Vec<String>,
    updated_rows: Vec<String>,
    unchanged_rows: Vec<String>,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let samples = load_samples(&args)?;
    let (emitted, skipped) = group_samples(&samples, &args);
    let merge_diff_summary = if let Some(path) = args.existing_overrides_input.as_ref() {
        let (merged, diff) = merge_existing_overrides(path, &emitted, args.merge_mode)?;
        let override_toml = render_merged_override_toml(&merged, &emitted, &args, &diff);
        print!("{override_toml}");
        write_override_output(&override_toml, args.override_output.as_ref())?;
        Some(diff)
    } else {
        let override_toml = render_override_toml(&emitted, &args);
        print!("{override_toml}");
        write_override_output(&override_toml, args.override_output.as_ref())?;
        None
    };

    write_summary_json(
        &CalibrationSummaryJson {
            input_rows: samples.len(),
            emitted_segment_count: emitted.len(),
            skipped_segment_count: skipped.len(),
            min_samples: args.min_samples,
            grouping: CalibrationGroupingSummary {
                group_by_asset_class: args.group_by_asset_class,
                group_by_event_subtype: args.group_by_event_subtype,
                short_horizon_max_days: args.short_horizon_max_days,
                medium_horizon_max_days: args.medium_horizon_max_days,
            },
            emitted_segments: emitted
                .into_iter()
                .map(|(key, summary)| CalibrationSummaryEntry { key, summary })
                .collect(),
            underfilled_buckets: build_underfilled_buckets(&skipped, args.min_samples),
            skipped_segments: skipped,
            merge_diff_summary: merge_diff_summary
                .as_ref()
                .map(MergeDiffSummaryJson::from_diff),
        },
        args.summary_output.as_ref(),
    )?;

    Ok(())
}

fn render_override_toml(emitted: &BTreeMap<SegmentKey, SegmentSummary>, args: &Args) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "# Generated by `cargo run --bin crypto_calibrate -- --input {}`\n",
        args.input.display()
    ));
    out.push_str(&format!(
        "# Segments require at least {} samples. Horizons: short<= {}d, medium<= {}d, else long.\n",
        args.min_samples, args.short_horizon_max_days, args.medium_horizon_max_days
    ));

    for (key, summary) in emitted {
        out.push('\n');
        out.push_str(&format!(
            "# {} / {} / {} / {} / {} | samples={} | brier_before={:.6} | brier_after={:.6}\n",
            key.asset,
            if key.asset_class.is_empty() {
                "asset"
            } else {
                key.asset_class.as_str()
            },
            key.horizon,
            key.market_type,
            if key.event_subtype.is_empty() {
                "any"
            } else {
                key.event_subtype.as_str()
            },
            summary.count,
            summary.brier_before,
            summary.brier_after
        ));
        out.push_str("[[crypto_alpha.calibration_overrides]]\n");
        out.push_str(&format!("asset = {:?}\n", key.asset));
        if !key.asset_class.is_empty() {
            out.push_str(&format!("asset_class = {:?}\n", key.asset_class));
        }
        out.push_str(&format!("horizon = {:?}\n", key.horizon));
        out.push_str(&format!("market_type = {:?}\n", key.market_type));
        if !key.event_subtype.is_empty() {
            out.push_str(&format!("event_subtype = {:?}\n", key.event_subtype));
        }
        out.push_str(&format!(
            "probability_calibration = {:.4}\n",
            round_to(summary.probability_calibration, 4)
        ));
    }

    out
}

impl MergeDiffSummaryJson {
    fn from_diff(diff: &MergeDiffSummary) -> Self {
        Self {
            new_row_count: diff.new_rows.len(),
            updated_row_count: diff.updated_rows.len(),
            unchanged_row_count: diff.unchanged_rows.len(),
            new_rows: diff.new_rows.clone(),
            updated_rows: diff.updated_rows.clone(),
            unchanged_rows: diff.unchanged_rows.clone(),
        }
    }
}

fn merge_existing_overrides(
    path: &PathBuf,
    emitted: &BTreeMap<SegmentKey, SegmentSummary>,
    merge_mode: MergeMode,
) -> Result<(Vec<CryptoCalibrationOverride>, MergeDiffSummary)> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read existing override input {}", path.display()))?;
    let parsed: ExistingConfigDoc = toml::from_str(&raw)
        .with_context(|| format!("failed to parse TOML from {}", path.display()))?;
    let mut merged = parsed.crypto_alpha.calibration_overrides;
    let mut diff = MergeDiffSummary::default();

    for (key, summary) in emitted {
        let rounded_probability = format!("{:.4}", round_to(summary.probability_calibration, 4));
        let new_probability = rounded_probability
            .parse::<rust_decimal::Decimal>()
            .context("failed to convert fitted probability_calibration into Decimal")?;

        let new_row = CryptoCalibrationOverride {
            asset: key.asset.clone(),
            asset_class: key.asset_class.clone(),
            horizon: key.horizon.clone(),
            market_type: key.market_type.clone(),
            event_subtype: key.event_subtype.clone(),
            probability_calibration: Some(new_probability),
            ..Default::default()
        };
        let label = segment_label(key);

        if let Some(existing) = merged
            .iter_mut()
            .find(|entry| override_matches_segment(entry, key))
        {
            match merge_mode {
                MergeMode::ProbabilityOnly => {
                    if existing.probability_calibration == Some(new_probability) {
                        diff.unchanged_rows.push(label);
                    } else {
                        existing.probability_calibration = Some(new_probability);
                        diff.updated_rows.push(label);
                    }
                }
                MergeMode::ReplaceRow => {
                    let unchanged = existing.probability_calibration
                        == new_row.probability_calibration
                        && existing.sigma_multiplier == new_row.sigma_multiplier
                        && existing.size_multiplier == new_row.size_multiplier
                        && existing.depth_ratio_multiplier == new_row.depth_ratio_multiplier
                        && existing.min_edge_multiplier == new_row.min_edge_multiplier
                        && existing.max_spread_multiplier == new_row.max_spread_multiplier
                        && existing.hold_edge_multiplier == new_row.hold_edge_multiplier
                        && existing.edge_decay_exit_multiplier
                            == new_row.edge_decay_exit_multiplier
                        && existing.edge_decay_confirmation_scan_multiplier
                            == new_row.edge_decay_confirmation_scan_multiplier
                        && existing.edge_decay_confirmation_window_multiplier
                            == new_row.edge_decay_confirmation_window_multiplier
                        && existing.edge_decay_cooldown_multiplier
                            == new_row.edge_decay_cooldown_multiplier
                        && existing.capital_efficiency_multiplier
                            == new_row.capital_efficiency_multiplier
                        && existing.model_reversal_buffer_multiplier
                            == new_row.model_reversal_buffer_multiplier
                        && existing.profit_retention_multiplier
                            == new_row.profit_retention_multiplier
                        && existing.slippage_multiplier == new_row.slippage_multiplier
                        && existing.size_retention_multiplier == new_row.size_retention_multiplier;
                    *existing = new_row;
                    if unchanged {
                        diff.unchanged_rows.push(label);
                    } else {
                        diff.updated_rows.push(label);
                    }
                }
                MergeMode::AppendOnly => {
                    merged.push(new_row);
                    diff.new_rows.push(label);
                }
            }
        } else {
            merged.push(new_row);
            diff.new_rows.push(label);
        }
    }

    Ok((merged, diff))
}

fn override_matches_segment(entry: &CryptoCalibrationOverride, key: &SegmentKey) -> bool {
    entry.asset == key.asset
        && normalize_selector(&entry.asset_class) == normalize_selector(&key.asset_class)
        && normalize_selector(&entry.horizon) == normalize_selector(&key.horizon)
        && normalize_selector(&entry.market_type) == normalize_selector(&key.market_type)
        && normalize_selector(&entry.event_subtype) == normalize_selector(&key.event_subtype)
}

fn normalize_selector(value: &str) -> &str {
    if value.is_empty() { "any" } else { value }
}

fn render_merged_override_toml(
    overrides: &[CryptoCalibrationOverride],
    emitted: &BTreeMap<SegmentKey, SegmentSummary>,
    args: &Args,
    diff: &MergeDiffSummary,
) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "# Generated by `cargo run --bin crypto_calibrate -- --input {}`\n",
        args.input.display()
    ));
    out.push_str(&format!(
        "# Existing overrides were merged by exact selector match using mode={:?}.\n",
        args.merge_mode
    ));
    out.push_str(&format!(
        "# Diff summary: new_rows={}, updated_rows={}, unchanged_rows={}\n",
        diff.new_rows.len(),
        diff.updated_rows.len(),
        diff.unchanged_rows.len()
    ));
    if !diff.new_rows.is_empty() {
        out.push_str("# New rows:\n");
        for row in &diff.new_rows {
            out.push_str(&format!("#   - {row}\n"));
        }
    }
    if !diff.updated_rows.is_empty() {
        out.push_str("# Updated rows:\n");
        for row in &diff.updated_rows {
            out.push_str(&format!("#   - {row}\n"));
        }
    }
    if !diff.unchanged_rows.is_empty() {
        out.push_str("# Unchanged rows:\n");
        for row in &diff.unchanged_rows {
            out.push_str(&format!("#   - {row}\n"));
        }
    }
    out.push_str(&format!(
        "# Segments require at least {} samples. Horizons: short<= {}d, medium<= {}d, else long.\n",
        args.min_samples, args.short_horizon_max_days, args.medium_horizon_max_days
    ));

    for override_row in overrides {
        out.push('\n');
        if let Some((key, summary)) = emitted
            .iter()
            .find(|(key, _)| override_matches_segment(override_row, key))
        {
            out.push_str(&format!(
                "# {} / {} / {} / {} / {} | samples={} | brier_before={:.6} | brier_after={:.6}\n",
                key.asset,
                if key.asset_class.is_empty() {
                    "asset"
                } else {
                    key.asset_class.as_str()
                },
                key.horizon,
                key.market_type,
                if key.event_subtype.is_empty() {
                    "any"
                } else {
                    key.event_subtype.as_str()
                },
                summary.count,
                summary.brier_before,
                summary.brier_after
            ));
        }
        out.push_str(&render_single_override_block(override_row));
    }

    out
}

fn segment_label(key: &SegmentKey) -> String {
    format!(
        "{} / {} / {} / {} / {}",
        key.asset,
        if key.asset_class.is_empty() {
            "any"
        } else {
            key.asset_class.as_str()
        },
        key.horizon,
        key.market_type,
        if key.event_subtype.is_empty() {
            "any"
        } else {
            key.event_subtype.as_str()
        }
    )
}

fn render_single_override_block(override_row: &CryptoCalibrationOverride) -> String {
    let mut out = String::new();
    out.push_str("[[crypto_alpha.calibration_overrides]]\n");
    out.push_str(&format!("asset = {:?}\n", override_row.asset));
    if !override_row.asset_class.is_empty() {
        out.push_str(&format!("asset_class = {:?}\n", override_row.asset_class));
    }
    if !override_row.horizon.is_empty() {
        out.push_str(&format!("horizon = {:?}\n", override_row.horizon));
    }
    if !override_row.market_type.is_empty() {
        out.push_str(&format!("market_type = {:?}\n", override_row.market_type));
    }
    if !override_row.event_subtype.is_empty() {
        out.push_str(&format!(
            "event_subtype = {:?}\n",
            override_row.event_subtype
        ));
    }
    if let Some(value) = override_row.probability_calibration {
        out.push_str(&format!(
            "probability_calibration = {}\n",
            value.normalize()
        ));
    }
    if let Some(value) = override_row.sigma_multiplier {
        out.push_str(&format!("sigma_multiplier = {}\n", value.normalize()));
    }
    if let Some(value) = override_row.size_multiplier {
        out.push_str(&format!("size_multiplier = {}\n", value.normalize()));
    }
    if let Some(value) = override_row.depth_ratio_multiplier {
        out.push_str(&format!("depth_ratio_multiplier = {}\n", value.normalize()));
    }
    if let Some(value) = override_row.min_edge_multiplier {
        out.push_str(&format!("min_edge_multiplier = {}\n", value.normalize()));
    }
    if let Some(value) = override_row.max_spread_multiplier {
        out.push_str(&format!("max_spread_multiplier = {}\n", value.normalize()));
    }
    if let Some(value) = override_row.hold_edge_multiplier {
        out.push_str(&format!("hold_edge_multiplier = {}\n", value.normalize()));
    }
    if let Some(value) = override_row.edge_decay_exit_multiplier {
        out.push_str(&format!(
            "edge_decay_exit_multiplier = {}\n",
            value.normalize()
        ));
    }
    if let Some(value) = override_row.edge_decay_confirmation_scan_multiplier {
        out.push_str(&format!(
            "edge_decay_confirmation_scan_multiplier = {}\n",
            value.normalize()
        ));
    }
    if let Some(value) = override_row.edge_decay_confirmation_window_multiplier {
        out.push_str(&format!(
            "edge_decay_confirmation_window_multiplier = {}\n",
            value.normalize()
        ));
    }
    if let Some(value) = override_row.edge_decay_cooldown_multiplier {
        out.push_str(&format!(
            "edge_decay_cooldown_multiplier = {}\n",
            value.normalize()
        ));
    }
    if let Some(value) = override_row.capital_efficiency_multiplier {
        out.push_str(&format!(
            "capital_efficiency_multiplier = {}\n",
            value.normalize()
        ));
    }
    if let Some(value) = override_row.model_reversal_buffer_multiplier {
        out.push_str(&format!(
            "model_reversal_buffer_multiplier = {}\n",
            value.normalize()
        ));
    }
    if let Some(value) = override_row.profit_retention_multiplier {
        out.push_str(&format!(
            "profit_retention_multiplier = {}\n",
            value.normalize()
        ));
    }
    if let Some(value) = override_row.slippage_multiplier {
        out.push_str(&format!("slippage_multiplier = {}\n", value.normalize()));
    }
    if let Some(value) = override_row.size_retention_multiplier {
        out.push_str(&format!(
            "size_retention_multiplier = {}\n",
            value.normalize()
        ));
    }
    out
}

fn load_samples(args: &Args) -> Result<Vec<(SegmentKey, SampleObservation)>> {
    let file = File::open(&args.input)
        .with_context(|| format!("failed to open input file {}", args.input.display()))?;
    let reader = BufReader::new(file);
    let mut parsed = Vec::new();

    for (line_no, line) in reader.lines().enumerate() {
        let line = line.with_context(|| format!("failed to read line {}", line_no + 1))?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let sample: CalibrationSampleLine = serde_json::from_str(trimmed)
            .with_context(|| format!("invalid JSON on line {}", line_no + 1))?;
        let key = infer_segment_key(&sample, args)
            .with_context(|| format!("could not infer segment on line {}", line_no + 1))?;
        let resolved_value = infer_resolved_value(&sample)
            .with_context(|| format!("invalid realized outcome on line {}", line_no + 1))?;
        validate_probability(sample.modeled_prob)
            .with_context(|| format!("invalid modeled_prob on line {}", line_no + 1))?;

        parsed.push((
            key,
            SampleObservation {
                modeled_prob: sample.modeled_prob,
                resolved_value,
            },
        ));
    }

    Ok(parsed)
}

fn infer_segment_key(sample: &CalibrationSampleLine, args: &Args) -> Result<SegmentKey> {
    let question = sample.question.as_deref().unwrap_or("");
    let asset = if let Some(asset) = sample.asset.as_deref() {
        normalize_asset(asset)
    } else if let Some(parsed) = parse_crypto_question(question) {
        parsed.asset.binance_symbol.to_string()
    } else if parse_crypto_outcome_range(question).is_some() {
        find_asset(question)
            .map(|asset| asset.binance_symbol.to_string())
            .context("range question matched price format but not a known crypto asset")?
    } else if let Some(asset) = find_asset(question) {
        asset.binance_symbol.to_string()
    } else {
        anyhow::bail!("missing asset and question did not match a known crypto asset");
    };

    let market_type = if let Some(market_type) = sample.market_type.as_deref() {
        normalize_market_type(market_type)?
    } else if parse_crypto_outcome_range(question).is_some() {
        "range".to_string()
    } else if parse_crypto_question(question).is_some() {
        "binary".to_string()
    } else {
        anyhow::bail!("missing market_type and question did not match a supported crypto market");
    };

    let days_to_resolution = if let Some(days) = sample.days_to_resolution {
        days
    } else if let (Some(observed_at), Some(resolution_at)) =
        (sample.observed_at, sample.resolution_at)
    {
        let duration = resolution_at.signed_duration_since(observed_at);
        if duration.num_seconds() < 0 {
            anyhow::bail!("resolution_at must not be earlier than observed_at");
        }
        let days = duration.num_seconds() as f64 / 86_400.0;
        days.ceil() as u32
    } else {
        anyhow::bail!("missing days_to_resolution and timestamp pair");
    };

    let asset_class = if args.group_by_asset_class {
        sample
            .asset_class
            .as_deref()
            .map(normalize_asset_class)
            .transpose()?
            .unwrap_or_else(|| infer_asset_class(&asset))
    } else {
        String::new()
    };

    let event_subtype = if args.group_by_event_subtype {
        sample
            .event_subtype
            .as_deref()
            .map(normalize_event_subtype)
            .transpose()?
            .unwrap_or_else(|| "any".to_string())
    } else {
        String::new()
    };

    Ok(SegmentKey {
        asset: if args.group_by_asset_class {
            "*".to_string()
        } else {
            asset
        },
        asset_class,
        horizon: horizon_bucket(
            days_to_resolution,
            args.short_horizon_max_days,
            args.medium_horizon_max_days,
        )
        .to_string(),
        market_type,
        event_subtype,
    })
}

fn normalize_asset(asset: &str) -> String {
    asset.trim().to_ascii_uppercase()
}

fn normalize_market_type(value: &str) -> Result<String> {
    let normalized = value.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "binary" | "range" | "any" => Ok(normalized),
        _ => anyhow::bail!("unsupported market_type `{value}`"),
    }
}

fn normalize_asset_class(value: &str) -> Result<String> {
    let normalized = value.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "major" | "alt" | "any" => Ok(normalized),
        _ => anyhow::bail!("unsupported asset_class `{value}`"),
    }
}

fn normalize_event_subtype(value: &str) -> Result<String> {
    let normalized = value.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "unlock" | "upgrade" | "regulatory" | "other" | "any" => Ok(normalized),
        _ => anyhow::bail!("unsupported event_subtype `{value}`"),
    }
}

fn infer_asset_class(asset: &str) -> String {
    match asset.trim().to_ascii_uppercase().as_str() {
        "BTCUSDT" | "ETHUSDT" => "major".to_string(),
        _ => "alt".to_string(),
    }
}

fn infer_resolved_value(sample: &CalibrationSampleLine) -> Result<f64> {
    if let Some(value) = sample.resolved_value {
        validate_probability(value)?;
        return Ok(value);
    }
    if let Some(yes) = sample.resolved_yes {
        return Ok(if yes { 1.0 } else { 0.0 });
    }
    anyhow::bail!("missing resolved_yes or resolved_value")
}

fn validate_probability(value: f64) -> Result<()> {
    if !value.is_finite() || !(0.0..=1.0).contains(&value) {
        anyhow::bail!("probability must be between 0 and 1");
    }
    Ok(())
}

fn horizon_bucket(
    days_to_resolution: u32,
    short_max_days: u32,
    medium_max_days: u32,
) -> &'static str {
    if days_to_resolution <= short_max_days {
        "short"
    } else if days_to_resolution <= medium_max_days {
        "medium"
    } else {
        "long"
    }
}

fn group_samples(
    samples: &[(SegmentKey, SampleObservation)],
    args: &Args,
) -> (
    BTreeMap<SegmentKey, SegmentSummary>,
    Vec<SkippedSegmentSummary>,
) {
    let mut grouped: BTreeMap<SegmentKey, Vec<SampleObservation>> = BTreeMap::new();
    for (key, sample) in samples {
        grouped.entry(key.clone()).or_default().push(sample.clone());
    }

    let mut emitted = BTreeMap::new();
    let mut skipped = Vec::new();

    for (key, samples) in grouped {
        if samples.len() < args.min_samples {
            skipped.push(SkippedSegmentSummary {
                key,
                count: samples.len(),
                reason: "insufficient_samples".to_string(),
            });
            continue;
        }

        let factor = fit_probability_calibration(&samples, args.min_factor, args.max_factor);
        let brier_before = brier_score(&samples, 1.0);
        let brier_after = brier_score(&samples, factor);
        emitted.insert(
            key,
            SegmentSummary {
                count: samples.len(),
                probability_calibration: factor,
                brier_before,
                brier_after,
            },
        );
    }

    (emitted, skipped)
}

fn fit_probability_calibration(
    samples: &[SampleObservation],
    min_factor: f64,
    max_factor: f64,
) -> f64 {
    let mut numerator = 0.0;
    let mut denominator = 0.0;

    for sample in samples {
        let centered_prob = sample.modeled_prob - 0.5;
        let centered_outcome = sample.resolved_value - 0.5;
        numerator += centered_prob * centered_outcome;
        denominator += centered_prob * centered_prob;
    }

    if denominator <= f64::EPSILON {
        return 1.0_f64.clamp(min_factor, max_factor);
    }

    (numerator / denominator).clamp(min_factor, max_factor)
}

fn brier_score(samples: &[SampleObservation], factor: f64) -> f64 {
    let total: f64 = samples
        .iter()
        .map(|sample| {
            let calibrated = 0.5 + (sample.modeled_prob - 0.5) * factor;
            let diff = calibrated - sample.resolved_value;
            diff * diff
        })
        .sum();
    total / samples.len() as f64
}

fn round_to(value: f64, decimals: u32) -> f64 {
    let scale = 10f64.powi(decimals as i32);
    (value * scale).round() / scale
}

fn write_summary_json(summary: &CalibrationSummaryJson, path: Option<&PathBuf>) -> Result<()> {
    let Some(path) = path else {
        return Ok(());
    };
    let file = File::create(path)
        .with_context(|| format!("failed to create summary output {}", path.display()))?;
    let writer = BufWriter::new(file);
    serde_json::to_writer_pretty(writer, summary).context("failed to serialize summary JSON")?;
    Ok(())
}

fn write_override_output(rendered: &str, path: Option<&PathBuf>) -> Result<()> {
    let Some(path) = path else {
        return Ok(());
    };
    let file = File::create(path)
        .with_context(|| format!("failed to create override output {}", path.display()))?;
    let mut writer = BufWriter::new(file);
    writer
        .write_all(rendered.as_bytes())
        .context("failed to write override TOML")?;
    writer
        .flush()
        .context("failed to flush override TOML output")?;
    Ok(())
}

fn build_underfilled_buckets(
    skipped_segments: &[SkippedSegmentSummary],
    min_samples: usize,
) -> Vec<UnderfilledBucketSummary> {
    let mut aggregated: BTreeMap<UnderfilledBucketKey, (usize, usize, usize)> = BTreeMap::new();

    for skipped in skipped_segments {
        if skipped.reason != "insufficient_samples" {
            continue;
        }

        let key = UnderfilledBucketKey {
            asset_class: selector_or_any(&skipped.key.asset_class),
            horizon: selector_or_any(&skipped.key.horizon),
            event_subtype: selector_or_any(&skipped.key.event_subtype),
        };
        let gap_to_min_samples = min_samples.saturating_sub(skipped.count);
        let entry = aggregated.entry(key).or_insert((0, 0, 0));
        entry.0 += 1;
        entry.1 += skipped.count;
        entry.2 += gap_to_min_samples;
    }

    aggregated
        .into_iter()
        .map(
            |(key, (skipped_segment_count, skipped_row_count, gap_to_min_samples))| {
                UnderfilledBucketSummary {
                    key,
                    skipped_segment_count,
                    skipped_row_count,
                    gap_to_min_samples,
                    threshold_band: classify_underfilled_bucket(gap_to_min_samples, min_samples),
                }
            },
        )
        .collect()
}

fn classify_underfilled_bucket(gap_to_min_samples: usize, min_samples: usize) -> String {
    let near_threshold_gap = usize::max(3, (min_samples + 3) / 4);
    if gap_to_min_samples <= near_threshold_gap {
        "near-threshold".to_string()
    } else {
        "far-from-threshold".to_string()
    }
}

fn selector_or_any(value: &str) -> String {
    if value.is_empty() {
        "any".to_string()
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fits_shrink_factor_below_one_for_overconfident_samples() {
        let samples = vec![
            SampleObservation {
                modeled_prob: 0.90,
                resolved_value: 1.0,
            },
            SampleObservation {
                modeled_prob: 0.88,
                resolved_value: 0.0,
            },
            SampleObservation {
                modeled_prob: 0.85,
                resolved_value: 1.0,
            },
            SampleObservation {
                modeled_prob: 0.92,
                resolved_value: 0.0,
            },
        ];

        let factor = fit_probability_calibration(&samples, 0.60, 1.10);
        assert!(factor < 1.0);
        assert!(brier_score(&samples, factor) < brier_score(&samples, 1.0));
    }

    #[test]
    fn horizon_bucket_matches_strategy_layout() {
        assert_eq!(horizon_bucket(1, 1, 7), "short");
        assert_eq!(horizon_bucket(7, 1, 7), "medium");
        assert_eq!(horizon_bucket(30, 1, 7), "long");
    }

    #[test]
    fn infers_segment_from_question_and_timestamps() {
        let sample = CalibrationSampleLine {
            question: Some("Will Bitcoin reach $200,000 by December 31, 2026?".to_string()),
            asset: None,
            asset_class: None,
            market_type: None,
            event_subtype: None,
            days_to_resolution: Some(30),
            observed_at: None,
            resolution_at: None,
            modeled_prob: 0.62,
            resolved_yes: Some(true),
            resolved_value: None,
        };

        let args = Args {
            input: PathBuf::from("samples.jsonl"),
            min_samples: 20,
            short_horizon_max_days: 1,
            medium_horizon_max_days: 7,
            min_factor: 0.60,
            max_factor: 1.10,
            group_by_asset_class: false,
            group_by_event_subtype: false,
            summary_output: None,
            override_output: None,
            existing_overrides_input: None,
            merge_mode: MergeMode::ProbabilityOnly,
        };

        let key = infer_segment_key(&sample, &args).unwrap();
        assert_eq!(key.asset, "BTCUSDT");
        assert_eq!(key.horizon, "long");
        assert_eq!(key.market_type, "binary");
    }

    #[test]
    fn can_group_segment_by_asset_class_and_event_subtype() {
        let sample = CalibrationSampleLine {
            question: Some("Will Solana reach $500 by December 31, 2026?".to_string()),
            asset: Some("SOLUSDT".to_string()),
            asset_class: Some("alt".to_string()),
            market_type: Some("binary".to_string()),
            event_subtype: Some("unlock".to_string()),
            days_to_resolution: Some(3),
            observed_at: None,
            resolution_at: None,
            modeled_prob: 0.62,
            resolved_yes: Some(true),
            resolved_value: None,
        };

        let args = Args {
            input: PathBuf::from("samples.jsonl"),
            min_samples: 20,
            short_horizon_max_days: 1,
            medium_horizon_max_days: 7,
            min_factor: 0.60,
            max_factor: 1.10,
            group_by_asset_class: true,
            group_by_event_subtype: true,
            summary_output: None,
            override_output: None,
            existing_overrides_input: None,
            merge_mode: MergeMode::ProbabilityOnly,
        };

        let key = infer_segment_key(&sample, &args).unwrap();
        assert_eq!(key.asset, "*");
        assert_eq!(key.asset_class, "alt");
        assert_eq!(key.horizon, "medium");
        assert_eq!(key.market_type, "binary");
        assert_eq!(key.event_subtype, "unlock");
    }

    #[test]
    fn group_samples_reports_underfilled_segments() {
        let key = SegmentKey {
            asset: "BTCUSDT".to_string(),
            asset_class: String::new(),
            horizon: "long".to_string(),
            market_type: "binary".to_string(),
            event_subtype: String::new(),
        };
        let samples = vec![(
            key.clone(),
            SampleObservation {
                modeled_prob: 0.6,
                resolved_value: 1.0,
            },
        )];
        let args = Args {
            input: PathBuf::from("samples.jsonl"),
            min_samples: 2,
            short_horizon_max_days: 1,
            medium_horizon_max_days: 7,
            min_factor: 0.60,
            max_factor: 1.10,
            group_by_asset_class: false,
            group_by_event_subtype: false,
            summary_output: None,
            override_output: None,
            existing_overrides_input: None,
            merge_mode: MergeMode::ProbabilityOnly,
        };

        let (emitted, skipped) = group_samples(&samples, &args);
        assert!(emitted.is_empty());
        assert_eq!(skipped.len(), 1);
        assert_eq!(skipped[0].key, key);
        assert_eq!(skipped[0].count, 1);
        assert_eq!(skipped[0].reason, "insufficient_samples");
    }

    #[test]
    fn builds_underfilled_bucket_summary() {
        let buckets = build_underfilled_buckets(
            &[
                SkippedSegmentSummary {
                    key: SegmentKey {
                        asset: "*".to_string(),
                        asset_class: "alt".to_string(),
                        horizon: "short".to_string(),
                        market_type: "binary".to_string(),
                        event_subtype: "unlock".to_string(),
                    },
                    count: 3,
                    reason: "insufficient_samples".to_string(),
                },
                SkippedSegmentSummary {
                    key: SegmentKey {
                        asset: "*".to_string(),
                        asset_class: "alt".to_string(),
                        horizon: "short".to_string(),
                        market_type: "range".to_string(),
                        event_subtype: "unlock".to_string(),
                    },
                    count: 5,
                    reason: "insufficient_samples".to_string(),
                },
            ],
            10,
        );

        assert_eq!(buckets.len(), 1);
        assert_eq!(buckets[0].key.asset_class, "alt");
        assert_eq!(buckets[0].key.horizon, "short");
        assert_eq!(buckets[0].key.event_subtype, "unlock");
        assert_eq!(buckets[0].skipped_segment_count, 2);
        assert_eq!(buckets[0].skipped_row_count, 8);
        assert_eq!(buckets[0].gap_to_min_samples, 12);
        assert_eq!(buckets[0].threshold_band, "far-from-threshold");
    }

    #[test]
    fn builds_near_threshold_bucket_summary() {
        let buckets = build_underfilled_buckets(
            &[SkippedSegmentSummary {
                key: SegmentKey {
                    asset: "*".to_string(),
                    asset_class: "major".to_string(),
                    horizon: "medium".to_string(),
                    market_type: "binary".to_string(),
                    event_subtype: "regulatory".to_string(),
                },
                count: 18,
                reason: "insufficient_samples".to_string(),
            }],
            20,
        );

        assert_eq!(buckets.len(), 1);
        assert_eq!(buckets[0].gap_to_min_samples, 2);
        assert_eq!(buckets[0].threshold_band, "near-threshold");
    }

    #[test]
    fn renders_merge_ready_override_toml() {
        let mut emitted = BTreeMap::new();
        emitted.insert(
            SegmentKey {
                asset: "*".to_string(),
                asset_class: "alt".to_string(),
                horizon: "short".to_string(),
                market_type: "binary".to_string(),
                event_subtype: "unlock".to_string(),
            },
            SegmentSummary {
                count: 24,
                probability_calibration: 0.82341,
                brier_before: 0.211234,
                brier_after: 0.190001,
            },
        );
        let args = Args {
            input: PathBuf::from("tmp/crypto_samples.jsonl"),
            min_samples: 20,
            short_horizon_max_days: 1,
            medium_horizon_max_days: 7,
            min_factor: 0.60,
            max_factor: 1.10,
            group_by_asset_class: true,
            group_by_event_subtype: true,
            summary_output: None,
            override_output: None,
            existing_overrides_input: None,
            merge_mode: MergeMode::ProbabilityOnly,
        };

        let rendered = render_override_toml(&emitted, &args);
        assert!(rendered.contains("[[crypto_alpha.calibration_overrides]]"));
        assert!(rendered.contains("asset = \"*\""));
        assert!(rendered.contains("asset_class = \"alt\""));
        assert!(rendered.contains("event_subtype = \"unlock\""));
        assert!(rendered.contains("probability_calibration = 0.8234"));
    }

    #[test]
    fn merged_override_keeps_existing_non_probability_fields() {
        let existing = std::env::temp_dir().join(format!(
            "crypto-calibrate-merge-{}.toml",
            std::process::id()
        ));
        std::fs::write(
            &existing,
            r#"
[[crypto_alpha.calibration_overrides]]
asset = "*"
asset_class = "alt"
horizon = "short"
market_type = "binary"
event_subtype = "unlock"
probability_calibration = 0.91
sigma_multiplier = 1.10
size_multiplier = 0.82
"#,
        )
        .unwrap();

        let mut emitted = BTreeMap::new();
        emitted.insert(
            SegmentKey {
                asset: "*".to_string(),
                asset_class: "alt".to_string(),
                horizon: "short".to_string(),
                market_type: "binary".to_string(),
                event_subtype: "unlock".to_string(),
            },
            SegmentSummary {
                count: 24,
                probability_calibration: 0.8012,
                brier_before: 0.2,
                brier_after: 0.19,
            },
        );

        let (merged, diff) =
            merge_existing_overrides(&existing, &emitted, MergeMode::ProbabilityOnly).unwrap();
        let row = merged.first().unwrap();
        assert_eq!(row.sigma_multiplier.unwrap().normalize().to_string(), "1.1");
        assert_eq!(row.size_multiplier.unwrap().normalize().to_string(), "0.82");
        assert_eq!(
            row.probability_calibration.unwrap().normalize().to_string(),
            "0.8012"
        );
        assert_eq!(diff.updated_rows.len(), 1);
        assert!(diff.new_rows.is_empty());

        let _ = std::fs::remove_file(existing);
    }

    #[test]
    fn replace_row_merge_mode_resets_non_probability_fields() {
        let existing = std::env::temp_dir().join(format!(
            "crypto-calibrate-replace-{}.toml",
            std::process::id()
        ));
        std::fs::write(
            &existing,
            r#"
[[crypto_alpha.calibration_overrides]]
asset = "*"
asset_class = "alt"
horizon = "short"
market_type = "binary"
event_subtype = "unlock"
probability_calibration = 0.91
sigma_multiplier = 1.10
size_multiplier = 0.82
"#,
        )
        .unwrap();

        let mut emitted = BTreeMap::new();
        emitted.insert(
            SegmentKey {
                asset: "*".to_string(),
                asset_class: "alt".to_string(),
                horizon: "short".to_string(),
                market_type: "binary".to_string(),
                event_subtype: "unlock".to_string(),
            },
            SegmentSummary {
                count: 24,
                probability_calibration: 0.8012,
                brier_before: 0.2,
                brier_after: 0.19,
            },
        );

        let (merged, diff) =
            merge_existing_overrides(&existing, &emitted, MergeMode::ReplaceRow).unwrap();
        let row = merged.first().unwrap();
        assert!(row.sigma_multiplier.is_none());
        assert!(row.size_multiplier.is_none());
        assert_eq!(
            row.probability_calibration.unwrap().normalize().to_string(),
            "0.8012"
        );
        assert_eq!(diff.updated_rows.len(), 1);

        let _ = std::fs::remove_file(existing);
    }

    #[test]
    fn append_only_merge_mode_keeps_existing_and_adds_new_row() {
        let existing = std::env::temp_dir().join(format!(
            "crypto-calibrate-append-{}.toml",
            std::process::id()
        ));
        std::fs::write(
            &existing,
            r#"
[[crypto_alpha.calibration_overrides]]
asset = "*"
asset_class = "alt"
horizon = "short"
market_type = "binary"
event_subtype = "unlock"
probability_calibration = 0.91
sigma_multiplier = 1.10
"#,
        )
        .unwrap();

        let mut emitted = BTreeMap::new();
        emitted.insert(
            SegmentKey {
                asset: "*".to_string(),
                asset_class: "alt".to_string(),
                horizon: "short".to_string(),
                market_type: "binary".to_string(),
                event_subtype: "unlock".to_string(),
            },
            SegmentSummary {
                count: 24,
                probability_calibration: 0.8012,
                brier_before: 0.2,
                brier_after: 0.19,
            },
        );

        let (merged, diff) =
            merge_existing_overrides(&existing, &emitted, MergeMode::AppendOnly).unwrap();
        assert_eq!(merged.len(), 2);
        assert_eq!(
            merged[0]
                .probability_calibration
                .unwrap()
                .normalize()
                .to_string(),
            "0.91"
        );
        assert_eq!(
            merged[1]
                .probability_calibration
                .unwrap()
                .normalize()
                .to_string(),
            "0.8012"
        );
        assert_eq!(diff.new_rows.len(), 1);

        let _ = std::fs::remove_file(existing);
    }

    #[test]
    fn merged_output_includes_diff_summary_comments() {
        let mut emitted = BTreeMap::new();
        emitted.insert(
            SegmentKey {
                asset: "*".to_string(),
                asset_class: "alt".to_string(),
                horizon: "short".to_string(),
                market_type: "binary".to_string(),
                event_subtype: "unlock".to_string(),
            },
            SegmentSummary {
                count: 24,
                probability_calibration: 0.8012,
                brier_before: 0.2,
                brier_after: 0.19,
            },
        );
        let args = Args {
            input: PathBuf::from("tmp/crypto_samples.jsonl"),
            min_samples: 20,
            short_horizon_max_days: 1,
            medium_horizon_max_days: 7,
            min_factor: 0.60,
            max_factor: 1.10,
            group_by_asset_class: true,
            group_by_event_subtype: true,
            summary_output: None,
            override_output: None,
            existing_overrides_input: Some(PathBuf::from("config/default.toml")),
            merge_mode: MergeMode::ProbabilityOnly,
        };
        let diff = MergeDiffSummary {
            new_rows: vec!["* / alt / short / binary / unlock".to_string()],
            updated_rows: vec!["* / major / any / any / unlock".to_string()],
            unchanged_rows: vec!["* / alt / any / any / regulatory".to_string()],
        };

        let rendered = render_merged_override_toml(&[], &emitted, &args, &diff);
        assert!(rendered.contains("# Diff summary: new_rows=1, updated_rows=1, unchanged_rows=1"));
        assert!(rendered.contains("# New rows:"));
        assert!(rendered.contains("# Updated rows:"));
        assert!(rendered.contains("# Unchanged rows:"));
    }

    #[test]
    fn merge_diff_summary_json_copies_counts_and_labels() {
        let diff = MergeDiffSummary {
            new_rows: vec!["a".to_string()],
            updated_rows: vec!["b".to_string(), "c".to_string()],
            unchanged_rows: vec!["d".to_string()],
        };

        let json = MergeDiffSummaryJson::from_diff(&diff);
        assert_eq!(json.new_row_count, 1);
        assert_eq!(json.updated_row_count, 2);
        assert_eq!(json.unchanged_row_count, 1);
        assert_eq!(json.updated_rows, vec!["b".to_string(), "c".to_string()]);
    }
}
