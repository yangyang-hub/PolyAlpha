use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::PathBuf;

use alloy::primitives::B256;
use anyhow::{Context, Result};
use clap::Parser;
use pa_monitor::diagnostics::{CryptoCandidateDecision, CryptoExitDecision};
use serde::{Deserialize, Serialize};

/// Generate a de-duplicated label skeleton file from exported crypto diagnostics.
#[derive(Debug, Parser)]
#[command(
    name = "crypto-seed-labels",
    about = "Generate question-level label skeleton JSONL from exported crypto diagnostics"
)]
struct Args {
    /// Path to JSONL exported by `crypto_export_diagnostics`.
    #[arg(long)]
    diagnostics: PathBuf,

    /// Output label skeleton JSONL path.
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

#[derive(Debug, Serialize, PartialEq, Eq)]
struct LabelSkeleton {
    question: String,
    asset: String,
    asset_class: String,
    market_type: String,
    event_subtype: Option<String>,
    condition_id: B256,
    resolved_yes: Option<bool>,
    resolved_value: Option<String>,
    resolution_at: Option<String>,
}

#[derive(Debug, Default, Serialize)]
struct SeedLabelsSummary {
    question_count: usize,
    replace_only: bool,
    by_asset: BTreeMap<String, usize>,
    by_asset_class: BTreeMap<String, usize>,
    by_market_type: BTreeMap<String, usize>,
    by_event_subtype: BTreeMap<String, usize>,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let labels = load_label_skeletons(&args)?;
    let summary = build_summary(&labels, args.replace_only);

    let file = File::create(&args.output)
        .with_context(|| format!("failed to create output file {}", args.output.display()))?;
    let mut writer = BufWriter::new(file);
    for label in labels.values() {
        serde_json::to_writer(&mut writer, label).context("failed to serialize label row")?;
        writer
            .write_all(b"\n")
            .context("failed to write label newline")?;
    }
    writer.flush().context("failed to flush output")?;
    write_summary_json(&summary, args.summary_output.as_ref())?;
    Ok(())
}

fn load_label_skeletons(args: &Args) -> Result<BTreeMap<String, LabelSkeleton>> {
    let file = File::open(&args.diagnostics).with_context(|| {
        format!(
            "failed to open diagnostics file {}",
            args.diagnostics.display()
        )
    })?;
    let reader = BufReader::new(file);
    let mut labels: BTreeMap<String, LabelSkeleton> = BTreeMap::new();

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
        labels
            .entry(decision.selected_question.clone())
            .or_insert_with(|| LabelSkeleton {
                question: decision.selected_question,
                asset_class: infer_asset_class(&decision.asset),
                asset: decision.asset,
                market_type: decision.selected_market_type,
                event_subtype: decision.event_subtype,
                condition_id: decision.selected_condition_id,
                resolved_yes: None,
                resolved_value: None,
                resolution_at: None,
            });
    }

    Ok(labels)
}

fn build_summary(
    labels: &BTreeMap<String, LabelSkeleton>,
    replace_only: bool,
) -> SeedLabelsSummary {
    let mut summary = SeedLabelsSummary {
        question_count: labels.len(),
        replace_only,
        ..SeedLabelsSummary::default()
    };
    for label in labels.values() {
        *summary.by_asset.entry(label.asset.clone()).or_default() += 1;
        *summary
            .by_asset_class
            .entry(label.asset_class.clone())
            .or_default() += 1;
        *summary
            .by_market_type
            .entry(label.market_type.clone())
            .or_default() += 1;
        *summary
            .by_event_subtype
            .entry(
                label
                    .event_subtype
                    .clone()
                    .unwrap_or_else(|| "any".to_string()),
            )
            .or_default() += 1;
    }
    summary
}

fn infer_asset_class(asset: &str) -> String {
    match asset.trim().to_ascii_uppercase().as_str() {
        "BTCUSDT" | "ETHUSDT" => "major".to_string(),
        _ => "alt".to_string(),
    }
}

fn write_summary_json(summary: &SeedLabelsSummary, path: Option<&PathBuf>) -> Result<()> {
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
    fn keeps_first_question_only_once() {
        let mut labels = BTreeMap::new();
        labels
            .entry("q".to_string())
            .or_insert_with(|| LabelSkeleton {
                question: "q".into(),
                asset: "BTCUSDT".into(),
                asset_class: "major".into(),
                market_type: "binary".into(),
                event_subtype: None,
                condition_id: B256::ZERO,
                resolved_yes: None,
                resolved_value: None,
                resolution_at: None,
            });
        labels
            .entry("q".to_string())
            .or_insert_with(|| LabelSkeleton {
                question: "q".into(),
                asset: "ETHUSDT".into(),
                asset_class: "major".into(),
                market_type: "range".into(),
                event_subtype: None,
                condition_id: B256::from([1u8; 32]),
                resolved_yes: None,
                resolved_value: None,
                resolution_at: None,
            });

        assert_eq!(labels.len(), 1);
        assert_eq!(labels.get("q").unwrap().asset, "BTCUSDT");
        assert_eq!(labels.get("q").unwrap().condition_id, B256::ZERO);
    }

    #[test]
    fn skeleton_defaults_resolution_fields_to_none() {
        let skeleton = LabelSkeleton {
            question: "q".into(),
            asset: "BTCUSDT".into(),
            asset_class: "major".into(),
            market_type: "binary".into(),
            event_subtype: None,
            condition_id: B256::ZERO,
            resolved_yes: None,
            resolved_value: None,
            resolution_at: None,
        };
        assert_eq!(
            skeleton,
            LabelSkeleton {
                question: "q".into(),
                asset: "BTCUSDT".into(),
                asset_class: "major".into(),
                market_type: "binary".into(),
                event_subtype: None,
                condition_id: B256::ZERO,
                resolved_yes: None,
                resolved_value: None,
                resolution_at: None,
            }
        );
    }

    #[test]
    fn summary_counts_assets_and_market_types() {
        let labels = BTreeMap::from([
            (
                "q1".to_string(),
                LabelSkeleton {
                    question: "q1".into(),
                    asset: "BTCUSDT".into(),
                    asset_class: "major".into(),
                    market_type: "binary".into(),
                    event_subtype: Some("unlock".into()),
                    condition_id: B256::ZERO,
                    resolved_yes: None,
                    resolved_value: None,
                    resolution_at: None,
                },
            ),
            (
                "q2".to_string(),
                LabelSkeleton {
                    question: "q2".into(),
                    asset: "BTCUSDT".into(),
                    asset_class: "major".into(),
                    market_type: "range".into(),
                    event_subtype: None,
                    condition_id: B256::from([2u8; 32]),
                    resolved_yes: None,
                    resolved_value: None,
                    resolution_at: None,
                },
            ),
        ]);

        let summary = build_summary(&labels, true);
        assert_eq!(summary.question_count, 2);
        assert!(summary.replace_only);
        assert_eq!(summary.by_asset.get("BTCUSDT"), Some(&2));
        assert_eq!(summary.by_asset_class.get("major"), Some(&2));
        assert_eq!(summary.by_market_type.get("binary"), Some(&1));
        assert_eq!(summary.by_market_type.get("range"), Some(&1));
        assert_eq!(summary.by_event_subtype.get("unlock"), Some(&1));
        assert_eq!(summary.by_event_subtype.get("any"), Some(&1));
    }

    #[test]
    fn summary_json_is_skipped_when_path_missing() {
        let summary = SeedLabelsSummary::default();
        assert!(write_summary_json(&summary, None).is_ok());
    }
}
