use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::PathBuf;

use alloy::primitives::B256;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use clap::Parser;
use serde::{Deserialize, Serialize};

/// Fill resolved crypto labels by querying CLOB market status via condition_id.
#[derive(Debug, Parser)]
#[command(
    name = "crypto-autolabel-resolved",
    about = "Auto-fill resolved crypto label rows from CLOB market winner status"
)]
struct Args {
    /// Input label JSONL path.
    #[arg(long)]
    labels: PathBuf,

    /// Output label JSONL path.
    #[arg(long)]
    output: PathBuf,

    /// Base URL of the Polymarket CLOB API.
    #[arg(long, default_value = "https://clob.polymarket.com")]
    clob_host: String,

    /// Overwrite rows that already have resolution fields.
    #[arg(long, default_value_t = false)]
    overwrite: bool,

    /// Optional JSONL output for rows that still need manual handling.
    #[arg(long)]
    unresolved_output: Option<PathBuf>,

    /// Optional JSON summary output path.
    #[arg(long)]
    summary_output: Option<PathBuf>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct LabelRow {
    question: String,
    asset: String,
    asset_class: Option<String>,
    market_type: String,
    event_subtype: Option<String>,
    condition_id: B256,
    resolved_yes: Option<bool>,
    resolved_value: Option<String>,
    resolution_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ClobMarketResponse {
    closed: bool,
    #[serde(default)]
    tokens: Vec<ClobToken>,
    end_date_iso: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
struct ClobToken {
    outcome: String,
    winner: bool,
}

#[derive(Debug, Serialize)]
struct UnresolvedLabelRow {
    question: String,
    asset: String,
    asset_class: String,
    market_type: String,
    event_subtype: Option<String>,
    condition_id: B256,
    reason: String,
    detail: Option<String>,
}

#[derive(Debug, Default, Serialize)]
struct AutolabelSummary {
    already_labeled: usize,
    filled: usize,
    open_market: usize,
    missing_winner: usize,
    request_error: usize,
    by_asset_class: std::collections::BTreeMap<String, usize>,
    by_event_subtype: std::collections::BTreeMap<String, usize>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let mut rows = load_label_rows(&args.labels)?;
    let client = reqwest::Client::builder()
        .build()
        .context("failed to build HTTP client")?;
    let mut unresolved = Vec::new();
    let mut summary = AutolabelSummary::default();

    for row in &mut rows {
        if !args.overwrite && (row.resolved_yes.is_some() || row.resolution_at.is_some()) {
            summary.already_labeled += 1;
            continue;
        }
        match fetch_resolution_update(&client, &args.clob_host, row.condition_id).await {
            Ok(AutolabelOutcome::Filled(update)) => {
                row.resolved_yes = Some(update.resolved_yes);
                row.resolution_at = update.resolution_at.map(|ts| ts.to_rfc3339());
                summary.filled += 1;
            }
            Ok(AutolabelOutcome::Unresolved { reason, detail }) => {
                match reason {
                    "open_market" => summary.open_market += 1,
                    "missing_winner" => summary.missing_winner += 1,
                    _ => {}
                }
                *summary
                    .by_asset_class
                    .entry(infer_asset_class(row.asset_class.as_deref(), &row.asset))
                    .or_default() += 1;
                *summary
                    .by_event_subtype
                    .entry(
                        row.event_subtype
                            .clone()
                            .unwrap_or_else(|| "any".to_string()),
                    )
                    .or_default() += 1;
                unresolved.push(UnresolvedLabelRow {
                    question: row.question.clone(),
                    asset: row.asset.clone(),
                    asset_class: infer_asset_class(row.asset_class.as_deref(), &row.asset),
                    market_type: row.market_type.clone(),
                    event_subtype: row.event_subtype.clone(),
                    condition_id: row.condition_id,
                    reason: reason.to_string(),
                    detail,
                });
            }
            Err(error) => {
                summary.request_error += 1;
                *summary
                    .by_asset_class
                    .entry(infer_asset_class(row.asset_class.as_deref(), &row.asset))
                    .or_default() += 1;
                *summary
                    .by_event_subtype
                    .entry(
                        row.event_subtype
                            .clone()
                            .unwrap_or_else(|| "any".to_string()),
                    )
                    .or_default() += 1;
                unresolved.push(UnresolvedLabelRow {
                    question: row.question.clone(),
                    asset: row.asset.clone(),
                    asset_class: infer_asset_class(row.asset_class.as_deref(), &row.asset),
                    market_type: row.market_type.clone(),
                    event_subtype: row.event_subtype.clone(),
                    condition_id: row.condition_id,
                    reason: "request_error".into(),
                    detail: Some(error.to_string()),
                });
            }
        }
    }

    let file = File::create(&args.output)
        .with_context(|| format!("failed to create output file {}", args.output.display()))?;
    let mut writer = BufWriter::new(file);
    for row in rows {
        serde_json::to_writer(&mut writer, &row).context("failed to serialize label row")?;
        writer
            .write_all(b"\n")
            .context("failed to write label newline")?;
    }
    writer.flush().context("failed to flush output")?;

    if let Some(path) = &args.unresolved_output {
        let file = File::create(path)
            .with_context(|| format!("failed to create unresolved output {}", path.display()))?;
        let mut writer = BufWriter::new(file);
        for row in unresolved {
            serde_json::to_writer(&mut writer, &row)
                .context("failed to serialize unresolved label row")?;
            writer
                .write_all(b"\n")
                .context("failed to write unresolved label newline")?;
        }
        writer
            .flush()
            .context("failed to flush unresolved output")?;
    }

    write_summary_json(&summary, args.summary_output.as_ref())?;
    tracing::info!(summary = ?summary, "crypto_autolabel_resolved summary");
    Ok(())
}

fn infer_asset_class(explicit: Option<&str>, asset: &str) -> String {
    if let Some(value) = explicit {
        let normalized = value.trim().to_ascii_lowercase();
        if !normalized.is_empty() {
            return normalized;
        }
    }
    match asset.trim().to_ascii_uppercase().as_str() {
        "BTCUSDT" | "ETHUSDT" => "major".to_string(),
        _ => "alt".to_string(),
    }
}

async fn fetch_resolution_update(
    client: &reqwest::Client,
    clob_host: &str,
    condition_id: B256,
) -> Result<AutolabelOutcome> {
    let url = format!(
        "{}/markets/{}",
        clob_host.trim_end_matches('/'),
        condition_id
    );
    let response: ClobMarketResponse = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("request failed for {url}"))?
        .error_for_status()
        .with_context(|| format!("request returned error status for {url}"))?
        .json()
        .await
        .with_context(|| format!("failed to decode JSON from {url}"))?;

    if !response.closed {
        return Ok(AutolabelOutcome::Unresolved {
            reason: "open_market",
            detail: None,
        });
    }
    match resolve_yes_from_tokens(&response.tokens) {
        Ok(resolved_yes) => Ok(AutolabelOutcome::Filled(ResolutionUpdate {
            resolved_yes,
            resolution_at: response.end_date_iso,
        })),
        Err(error) => Ok(AutolabelOutcome::Unresolved {
            reason: "missing_winner",
            detail: Some(error.to_string()),
        }),
    }
}

fn load_label_rows(path: &PathBuf) -> Result<Vec<LabelRow>> {
    let file = File::open(path)
        .with_context(|| format!("failed to open labels file {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut rows = Vec::new();

    for (line_no, line) in reader.lines().enumerate() {
        let line = line.with_context(|| format!("failed to read line {}", line_no + 1))?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let row: LabelRow = serde_json::from_str(trimmed)
            .with_context(|| format!("invalid labels JSON on line {}", line_no + 1))?;
        rows.push(row);
    }

    Ok(rows)
}

struct ResolutionUpdate {
    resolved_yes: bool,
    resolution_at: Option<DateTime<Utc>>,
}

enum AutolabelOutcome {
    Filled(ResolutionUpdate),
    Unresolved {
        reason: &'static str,
        detail: Option<String>,
    },
}

fn write_summary_json(summary: &AutolabelSummary, path: Option<&PathBuf>) -> Result<()> {
    let Some(path) = path else {
        return Ok(());
    };
    let file = File::create(path)
        .with_context(|| format!("failed to create summary output {}", path.display()))?;
    let writer = BufWriter::new(file);
    serde_json::to_writer_pretty(writer, summary).context("failed to serialize summary JSON")?;
    Ok(())
}

fn resolve_yes_from_tokens(tokens: &[ClobToken]) -> Result<bool> {
    let mut winners = tokens.iter().filter(|token| token.winner);
    let winner = winners
        .next()
        .context("closed market did not expose a winning token")?;
    if winners.next().is_some() {
        anyhow::bail!("closed market exposed multiple winning tokens");
    }
    Ok(winner.outcome.eq_ignore_ascii_case("yes"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_yes_winner() {
        let tokens = vec![
            ClobToken {
                outcome: "Yes".into(),
                winner: true,
            },
            ClobToken {
                outcome: "No".into(),
                winner: false,
            },
        ];
        assert!(resolve_yes_from_tokens(&tokens).unwrap());
    }

    #[test]
    fn rejects_missing_winner() {
        let tokens = vec![
            ClobToken {
                outcome: "Yes".into(),
                winner: false,
            },
            ClobToken {
                outcome: "No".into(),
                winner: false,
            },
        ];
        assert!(resolve_yes_from_tokens(&tokens).is_err());
    }

    #[test]
    fn unresolved_variant_preserves_reason() {
        let outcome = AutolabelOutcome::Unresolved {
            reason: "open_market",
            detail: None,
        };
        match outcome {
            AutolabelOutcome::Unresolved { reason, .. } => assert_eq!(reason, "open_market"),
            AutolabelOutcome::Filled(_) => panic!("unexpected filled outcome"),
        }
    }

    #[test]
    fn summary_json_is_skipped_when_path_missing() {
        let summary = AutolabelSummary::default();
        assert!(write_summary_json(&summary, None).is_ok());
    }

    #[test]
    fn infers_major_asset_class_from_asset_symbol() {
        assert_eq!(infer_asset_class(None, "BTCUSDT"), "major");
        assert_eq!(infer_asset_class(None, "SOLUSDT"), "alt");
        assert_eq!(infer_asset_class(Some("alt"), "BTCUSDT"), "alt");
    }
}
