use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use pa_monitor::diagnostics::{CryptoCandidateDecision, CryptoExitDecision};
use reqwest::Url;
use serde::Serialize;

/// Export recent crypto diagnostics from the local monitor API as JSONL.
#[derive(Debug, Parser)]
#[command(
    name = "crypto-export-diagnostics",
    about = "Export recent crypto decisions/exits from pa-monitor into JSONL"
)]
struct Args {
    /// Base URL of the local pa-monitor API.
    #[arg(long, default_value = "http://127.0.0.1:8080")]
    base_url: String,

    /// Output JSONL path.
    #[arg(long)]
    output: PathBuf,

    /// Skip recent candidate-decision rows.
    #[arg(long, default_value_t = false)]
    skip_decisions: bool,

    /// Skip recent exit-decision rows.
    #[arg(long, default_value_t = false)]
    skip_exits: bool,
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum DiagnosticRow {
    CandidateDecision(CryptoCandidateDecision),
    ExitDecision(CryptoExitDecision),
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    if args.skip_decisions && args.skip_exits {
        anyhow::bail!("at least one of decisions/exits must remain enabled");
    }

    let client = reqwest::Client::builder()
        .build()
        .context("failed to build HTTP client")?;
    let mut rows = Vec::new();

    if !args.skip_decisions {
        let decisions: Vec<CryptoCandidateDecision> =
            fetch_json(&client, &args.base_url, "/api/crypto/decisions").await?;
        rows.extend(decisions.into_iter().map(DiagnosticRow::CandidateDecision));
    }

    if !args.skip_exits {
        let exits: Vec<CryptoExitDecision> =
            fetch_json(&client, &args.base_url, "/api/crypto/exits").await?;
        rows.extend(exits.into_iter().map(DiagnosticRow::ExitDecision));
    }

    rows.sort_by_key(row_recorded_at);

    let file = File::create(&args.output)
        .with_context(|| format!("failed to create output file {}", args.output.display()))?;
    let mut writer = BufWriter::new(file);
    for row in rows {
        serde_json::to_writer(&mut writer, &row).context("failed to serialize JSONL row")?;
        writer
            .write_all(b"\n")
            .context("failed to write JSONL newline")?;
    }
    writer.flush().context("failed to flush output")?;

    Ok(())
}

async fn fetch_json<T>(client: &reqwest::Client, base_url: &str, path: &str) -> Result<T>
where
    T: serde::de::DeserializeOwned,
{
    let url = api_url(base_url, path)?;
    let response = client
        .get(url.clone())
        .send()
        .await
        .with_context(|| format!("request failed for {url}"))?
        .error_for_status()
        .with_context(|| format!("request returned error status for {url}"))?;
    response
        .json()
        .await
        .with_context(|| format!("failed to decode JSON from {url}"))
}

fn api_url(base_url: &str, path: &str) -> Result<Url> {
    let base = Url::parse(base_url).with_context(|| format!("invalid base URL `{base_url}`"))?;
    base.join(path)
        .with_context(|| format!("failed to join `{path}` onto `{base_url}`"))
}

fn row_recorded_at(row: &DiagnosticRow) -> chrono::DateTime<chrono::Utc> {
    match row {
        DiagnosticRow::CandidateDecision(entry) => entry.recorded_at,
        DiagnosticRow::ExitDecision(entry) => entry.recorded_at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_api_path_onto_base_url() {
        let url = api_url("http://127.0.0.1:8080", "/api/crypto/decisions").unwrap();
        assert_eq!(url.as_str(), "http://127.0.0.1:8080/api/crypto/decisions");
    }

    #[test]
    fn rejects_invalid_base_url() {
        assert!(api_url("not a url", "/api/crypto/decisions").is_err());
    }
}
