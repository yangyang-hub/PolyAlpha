use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use chrono::Utc;
use clap::Parser;
use serde::{Deserialize, Serialize};

/// Combine offline crypto calibration summaries into one markdown report.
#[derive(Debug, Parser)]
#[command(
    name = "crypto-pipeline-report",
    about = "Generate a markdown report from seed/autolabel/prepare/calibrate summary JSON files"
)]
struct Args {
    /// Path to crypto_seed_labels summary JSON.
    #[arg(long)]
    seed_summary: Option<PathBuf>,

    /// Path to crypto_autolabel_resolved summary JSON.
    #[arg(long)]
    autolabel_summary: Option<PathBuf>,

    /// Path to crypto_prepare_calibration summary JSON.
    #[arg(long)]
    prepare_summary: Option<PathBuf>,

    /// Path to crypto_calibrate summary JSON.
    #[arg(long)]
    calibrate_summary: Option<PathBuf>,

    /// Optional directory containing standard summary filenames.
    #[arg(long)]
    input_dir: Option<PathBuf>,

    /// Optional markdown output path. Prints to stdout when omitted.
    #[arg(long)]
    output: Option<PathBuf>,

    /// Optional directory for standard report output filenames.
    #[arg(long)]
    output_dir: Option<PathBuf>,

    /// Optional aggregate JSON output path for the same combined report data.
    #[arg(long)]
    json_output: Option<PathBuf>,

    /// Optional self-contained HTML output path with embedded aggregate JSON.
    #[arg(long)]
    html_output: Option<PathBuf>,

    /// Optional report title shared by markdown, JSON, and HTML outputs.
    #[arg(long)]
    title: Option<String>,

    /// Optional report subtitle shared by markdown, JSON, and HTML outputs.
    #[arg(long)]
    subtitle: Option<String>,

    /// Optional free-form notes shared by markdown, JSON, and HTML outputs.
    #[arg(long)]
    notes: Option<String>,

    /// Optional path to a UTF-8 notes file shared by markdown, JSON, and HTML outputs.
    #[arg(long)]
    notes_file: Option<PathBuf>,

    /// Optional repeatable batch tags shared by markdown, JSON, and HTML outputs.
    #[arg(long)]
    tag: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct SeedLabelsSummary {
    question_count: usize,
    replace_only: bool,
    by_asset: BTreeMap<String, usize>,
    by_asset_class: BTreeMap<String, usize>,
    by_market_type: BTreeMap<String, usize>,
    by_event_subtype: BTreeMap<String, usize>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct AutolabelSummary {
    already_labeled: usize,
    filled: usize,
    open_market: usize,
    missing_winner: usize,
    request_error: usize,
    by_asset_class: BTreeMap<String, usize>,
    by_event_subtype: BTreeMap<String, usize>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
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

#[derive(Debug, Deserialize, Serialize, Clone)]
struct CalibrateSummary {
    input_rows: usize,
    emitted_segment_count: usize,
    skipped_segment_count: usize,
    min_samples: usize,
    grouping: CalibrateGroupingSummary,
    emitted_segments: Vec<CalibrateSummaryEntry>,
    skipped_segments: Vec<CalibrateSkippedSegment>,
    underfilled_buckets: Vec<CalibrateUnderfilledBucket>,
    merge_diff_summary: Option<CalibrateMergeDiffSummary>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct CalibrateGroupingSummary {
    group_by_asset_class: bool,
    group_by_event_subtype: bool,
    short_horizon_max_days: u32,
    medium_horizon_max_days: u32,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct CalibrateSummaryEntry {
    key: CalibrateSegmentKey,
    summary: CalibrateSegmentSummary,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct CalibrateSkippedSegment {
    key: CalibrateSegmentKey,
    count: usize,
    reason: String,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct CalibrateSegmentKey {
    asset: String,
    asset_class: String,
    horizon: String,
    market_type: String,
    event_subtype: String,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct CalibrateSegmentSummary {
    count: usize,
    probability_calibration: f64,
    brier_before: f64,
    brier_after: f64,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct CalibrateUnderfilledBucket {
    key: CalibrateUnderfilledBucketKey,
    skipped_segment_count: usize,
    skipped_row_count: usize,
    gap_to_min_samples: usize,
    threshold_band: String,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct CalibrateUnderfilledBucketKey {
    asset_class: String,
    horizon: String,
    event_subtype: String,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct CalibrateMergeDiffSummary {
    new_row_count: usize,
    updated_row_count: usize,
    unchanged_row_count: usize,
    new_rows: Vec<String>,
    updated_rows: Vec<String>,
    unchanged_rows: Vec<String>,
}

#[derive(Debug, Serialize)]
struct PipelineReportSummary {
    metadata: PipelineReportMetadata,
    seed: Option<SeedLabelsSummary>,
    autolabel: Option<AutolabelSummary>,
    prepare: Option<PreparationSummary>,
    calibrate: Option<CalibrateSummary>,
    headline: Option<PipelineHeadlineSummary>,
    ui_priority_summary: Option<UiPrioritySummary>,
}

#[derive(Debug, Serialize)]
struct PipelineReportMetadata {
    title: String,
    subtitle: Option<String>,
    generated_at_utc: String,
    notes: Option<String>,
    tags: Vec<String>,
}

#[derive(Debug, Serialize)]
struct PipelineHeadlineSummary {
    emitted_vs_seed: Option<RatioSummary>,
    matched_vs_seed: Option<RatioSummary>,
    filled_vs_seed: Option<RatioSummary>,
    top_up_now_count: usize,
    ready_soon_count: usize,
    defer_count: usize,
    explainer: String,
    near_threshold_buckets: Vec<NearThresholdHeadlineBucket>,
}

#[derive(Debug, Serialize)]
struct RatioSummary {
    numerator: usize,
    denominator: usize,
    ratio: f64,
}

#[derive(Debug, Serialize)]
struct NearThresholdHeadlineBucket {
    asset_class: String,
    horizon: String,
    event_subtype: String,
    gap_to_min_samples: usize,
    skipped_row_count: usize,
    suggested_action: String,
}

#[derive(Debug, Serialize)]
struct UiPrioritySummary {
    headline_status: String,
    headline_status_level: String,
    headline_status_reason: String,
    near_threshold_status: String,
    near_threshold_status_level: String,
    priority_source: String,
    near_threshold_bucket_labels: Vec<String>,
    top_up_now_labels: Vec<String>,
    top_up_now_count: usize,
    ready_soon_count: usize,
    defer_count: usize,
    hero_badge_text: String,
    hero_badge_level: String,
    headline_explainer: String,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let (seed_path, autolabel_path, prepare_path, calibrate_path) = resolve_summary_paths(
        args.input_dir.as_ref(),
        args.seed_summary,
        args.autolabel_summary,
        args.prepare_summary,
        args.calibrate_summary,
    );
    validate_summary_inputs(
        args.input_dir.as_ref(),
        seed_path.as_ref(),
        autolabel_path.as_ref(),
        prepare_path.as_ref(),
        calibrate_path.as_ref(),
    )?;
    let seed = load_optional::<SeedLabelsSummary>(seed_path.as_ref())?;
    let autolabel = load_optional::<AutolabelSummary>(autolabel_path.as_ref())?;
    let prepare = load_optional::<PreparationSummary>(prepare_path.as_ref())?;
    let calibrate = load_optional::<CalibrateSummary>(calibrate_path.as_ref())?;
    let notes = load_notes(args.notes, args.notes_file.as_ref())?;
    let aggregate = build_summary(
        seed,
        autolabel,
        prepare,
        calibrate,
        args.title,
        args.subtitle,
        notes,
        normalize_tags(args.tag),
    );
    let (markdown_output, json_output, html_output) = resolve_output_paths(
        args.output_dir.as_ref(),
        args.output,
        args.json_output,
        args.html_output,
    )?;

    if let Some(path) = &json_output {
        write_json_output(path, &aggregate)?;
    }

    if let Some(path) = &html_output {
        write_html_output(path, &aggregate)?;
    }

    let report = render_report(&aggregate);
    if let Some(path) = &markdown_output {
        let file = File::create(path)
            .with_context(|| format!("failed to create report output {}", path.display()))?;
        let mut writer = BufWriter::new(file);
        writer
            .write_all(report.as_bytes())
            .context("failed to write report markdown")?;
        writer.flush().context("failed to flush report output")?;
    } else {
        print!("{report}");
    }
    Ok(())
}

fn write_json_output(path: &PathBuf, summary: &PipelineReportSummary) -> Result<()> {
    let file = File::create(path)
        .with_context(|| format!("failed to create json report output {}", path.display()))?;
    let writer = BufWriter::new(file);
    serde_json::to_writer_pretty(writer, summary)
        .with_context(|| format!("failed to write json report output {}", path.display()))?;
    Ok(())
}

fn write_html_output(path: &PathBuf, summary: &PipelineReportSummary) -> Result<()> {
    let html = render_html_document(summary)?;
    let file = File::create(path)
        .with_context(|| format!("failed to create html report output {}", path.display()))?;
    let mut writer = BufWriter::new(file);
    writer
        .write_all(html.as_bytes())
        .with_context(|| format!("failed to write html report output {}", path.display()))?;
    writer
        .flush()
        .with_context(|| format!("failed to flush html report output {}", path.display()))?;
    Ok(())
}

fn load_optional<T>(path: Option<&PathBuf>) -> Result<Option<T>>
where
    T: for<'de> Deserialize<'de>,
{
    let Some(path) = path else {
        return Ok(None);
    };
    let file = File::open(path)
        .with_context(|| format!("failed to open summary file {}", path.display()))?;
    Ok(Some(serde_json::from_reader(file).with_context(|| {
        format!("failed to parse summary JSON {}", path.display())
    })?))
}

fn load_notes(notes: Option<String>, notes_file: Option<&PathBuf>) -> Result<Option<String>> {
    if let Some(path) = notes_file {
        let text = fs::read_to_string(path)
            .with_context(|| format!("failed to read notes file {}", path.display()))?;
        let trimmed = text.trim().to_string();
        if !trimmed.is_empty() {
            return Ok(Some(trimmed));
        }
    }

    Ok(notes.and_then(|value| {
        let trimmed = value.trim().to_string();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    }))
}

fn validate_summary_inputs(
    input_dir: Option<&PathBuf>,
    seed_summary: Option<&PathBuf>,
    autolabel_summary: Option<&PathBuf>,
    prepare_summary: Option<&PathBuf>,
    calibrate_summary: Option<&PathBuf>,
) -> Result<()> {
    if seed_summary.is_some()
        || autolabel_summary.is_some()
        || prepare_summary.is_some()
        || calibrate_summary.is_some()
    {
        return Ok(());
    }

    if let Some(dir) = input_dir {
        bail!(
            "no summary inputs found; {} did not contain any of crypto_seed_summary.json, crypto_autolabel_summary.json, crypto_prepare_summary.json, or crypto_calibrate_summary.json",
            dir.display()
        );
    }

    bail!(
        "no summary inputs provided; pass at least one of --seed-summary/--autolabel-summary/--prepare-summary/--calibrate-summary or use --input-dir"
    );
}

fn resolve_output_paths(
    output_dir: Option<&PathBuf>,
    output: Option<PathBuf>,
    json_output: Option<PathBuf>,
    html_output: Option<PathBuf>,
) -> Result<(Option<PathBuf>, Option<PathBuf>, Option<PathBuf>)> {
    if let Some(dir) = output_dir {
        fs::create_dir_all(dir)
            .with_context(|| format!("failed to create output directory {}", dir.display()))?;
    }

    let markdown = output.or_else(|| output_dir.map(|dir| dir.join("crypto_pipeline_report.md")));
    let json =
        json_output.or_else(|| output_dir.map(|dir| dir.join("crypto_pipeline_report.json")));
    let html =
        html_output.or_else(|| output_dir.map(|dir| dir.join("crypto_pipeline_report.html")));

    Ok((markdown, json, html))
}

fn resolve_summary_paths(
    input_dir: Option<&PathBuf>,
    seed_summary: Option<PathBuf>,
    autolabel_summary: Option<PathBuf>,
    prepare_summary: Option<PathBuf>,
    calibrate_summary: Option<PathBuf>,
) -> (
    Option<PathBuf>,
    Option<PathBuf>,
    Option<PathBuf>,
    Option<PathBuf>,
) {
    let seed =
        seed_summary.or_else(|| resolve_standard_summary(input_dir, "crypto_seed_summary.json"));
    let autolabel = autolabel_summary
        .or_else(|| resolve_standard_summary(input_dir, "crypto_autolabel_summary.json"));
    let prepare = prepare_summary
        .or_else(|| resolve_standard_summary(input_dir, "crypto_prepare_summary.json"));
    let calibrate = calibrate_summary
        .or_else(|| resolve_standard_summary(input_dir, "crypto_calibrate_summary.json"));
    (seed, autolabel, prepare, calibrate)
}

fn resolve_standard_summary(input_dir: Option<&PathBuf>, filename: &str) -> Option<PathBuf> {
    let dir = input_dir?;
    let candidate = dir.join(filename);
    candidate.is_file().then_some(candidate)
}

fn normalize_tags(tags: Vec<String>) -> Vec<String> {
    let mut tags = tags
        .into_iter()
        .map(|tag| tag.trim().to_string())
        .filter(|tag| !tag.is_empty())
        .collect::<Vec<_>>();
    tags.sort();
    tags.dedup();
    tags
}

fn build_summary(
    seed: Option<SeedLabelsSummary>,
    autolabel: Option<AutolabelSummary>,
    prepare: Option<PreparationSummary>,
    calibrate: Option<CalibrateSummary>,
    title: Option<String>,
    subtitle: Option<String>,
    notes: Option<String>,
    tags: Vec<String>,
) -> PipelineReportSummary {
    let headline = build_headline(
        seed.as_ref(),
        autolabel.as_ref(),
        prepare.as_ref(),
        calibrate.as_ref(),
    );
    let ui_priority_summary = headline
        .as_ref()
        .map(build_ui_priority_summary)
        .or_else(|| {
            if seed.is_some() || autolabel.is_some() || prepare.is_some() || calibrate.is_some() {
                Some(build_empty_ui_priority_summary())
            } else {
                None
            }
        });
    PipelineReportSummary {
        metadata: PipelineReportMetadata {
            title: title.unwrap_or_else(|| "Crypto Calibration Pipeline Report".to_string()),
            subtitle,
            generated_at_utc: Utc::now().to_rfc3339(),
            notes,
            tags,
        },
        seed,
        autolabel,
        prepare,
        calibrate,
        headline,
        ui_priority_summary,
    }
}

fn build_headline(
    seed: Option<&SeedLabelsSummary>,
    autolabel: Option<&AutolabelSummary>,
    prepare: Option<&PreparationSummary>,
    calibrate: Option<&CalibrateSummary>,
) -> Option<PipelineHeadlineSummary> {
    let seed_count = seed
        .map(|seed| seed.question_count)
        .filter(|count| *count > 0);
    let emitted_vs_seed = build_ratio(prepare.map(|prepare| prepare.emitted_samples), seed_count);
    let matched_vs_seed = build_ratio(prepare.map(|prepare| prepare.matched_labels), seed_count);
    let filled_vs_seed = build_ratio(
        autolabel.map(|autolabel| autolabel.filled + autolabel.already_labeled),
        seed_count,
    );
    let near_threshold_buckets = top_near_threshold_buckets(calibrate);
    let (top_up_now_count, ready_soon_count, defer_count) =
        count_near_threshold_actions(&near_threshold_buckets);
    let explainer = build_headline_explainer(top_up_now_count, seed, autolabel, prepare, calibrate);

    if emitted_vs_seed.is_none()
        && matched_vs_seed.is_none()
        && filled_vs_seed.is_none()
        && near_threshold_buckets.is_empty()
    {
        return None;
    }

    Some(PipelineHeadlineSummary {
        emitted_vs_seed,
        matched_vs_seed,
        filled_vs_seed,
        top_up_now_count,
        ready_soon_count,
        defer_count,
        explainer,
        near_threshold_buckets,
    })
}

fn top_near_threshold_buckets(
    calibrate: Option<&CalibrateSummary>,
) -> Vec<NearThresholdHeadlineBucket> {
    let Some(calibrate) = calibrate else {
        return Vec::new();
    };

    let mut buckets = calibrate
        .underfilled_buckets
        .iter()
        .filter(|bucket| bucket.threshold_band == "near-threshold")
        .map(|bucket| NearThresholdHeadlineBucket {
            asset_class: bucket.key.asset_class.clone(),
            horizon: bucket.key.horizon.clone(),
            event_subtype: bucket.key.event_subtype.clone(),
            gap_to_min_samples: bucket.gap_to_min_samples,
            skipped_row_count: bucket.skipped_row_count,
            suggested_action: classify_gap_action(bucket.gap_to_min_samples),
        })
        .collect::<Vec<_>>();

    buckets.sort_by(|a, b| {
        a.gap_to_min_samples
            .cmp(&b.gap_to_min_samples)
            .then_with(|| b.skipped_row_count.cmp(&a.skipped_row_count))
            .then_with(|| a.asset_class.cmp(&b.asset_class))
            .then_with(|| a.horizon.cmp(&b.horizon))
            .then_with(|| a.event_subtype.cmp(&b.event_subtype))
    });
    buckets.truncate(3);
    buckets
}

fn classify_gap_action(gap_to_min_samples: usize) -> String {
    if gap_to_min_samples <= 1 {
        "top-up-now".to_string()
    } else if gap_to_min_samples <= 3 {
        "ready-soon".to_string()
    } else {
        "defer".to_string()
    }
}

fn count_near_threshold_actions(buckets: &[NearThresholdHeadlineBucket]) -> (usize, usize, usize) {
    let mut top_up_now_count = 0;
    let mut ready_soon_count = 0;
    let mut defer_count = 0;

    for bucket in buckets {
        match bucket.suggested_action.as_str() {
            "top-up-now" => top_up_now_count += 1,
            "ready-soon" => ready_soon_count += 1,
            _ => defer_count += 1,
        }
    }

    (top_up_now_count, ready_soon_count, defer_count)
}

fn build_headline_explainer(
    top_up_now_count: usize,
    seed: Option<&SeedLabelsSummary>,
    autolabel: Option<&AutolabelSummary>,
    prepare: Option<&PreparationSummary>,
    calibrate: Option<&CalibrateSummary>,
) -> String {
    if top_up_now_count >= 2 {
        format!("{top_up_now_count} buckets are one sample away from calibration readiness.")
    } else if top_up_now_count == 1 {
        "At least one bucket is one sample away from calibration readiness.".to_string()
    } else if seed.is_some() || autolabel.is_some() || prepare.is_some() || calibrate.is_some() {
        "No immediate top-up buckets in the visible top-three near-threshold set.".to_string()
    } else {
        "Load summaries to compute near-threshold urgency.".to_string()
    }
}

fn build_ratio(numerator: Option<usize>, denominator: Option<usize>) -> Option<RatioSummary> {
    let numerator = numerator?;
    let denominator = denominator?;
    if denominator == 0 {
        return None;
    }
    Some(RatioSummary {
        numerator,
        denominator,
        ratio: numerator as f64 / denominator as f64,
    })
}

fn build_ui_priority_summary(headline: &PipelineHeadlineSummary) -> UiPrioritySummary {
    let (
        headline_status,
        headline_status_level,
        headline_status_reason,
        near_threshold_status,
        near_threshold_status_level,
        priority_source,
    ) = if headline.top_up_now_count >= 2 {
        (
            "Urgent",
            "warn",
            "multiple top-up-now buckets in the visible top-three near-threshold set",
            "Urgent",
            "warn",
            "near-threshold-action-counts",
        )
    } else if headline.top_up_now_count > 0 {
        (
            "Action Needed",
            "warn",
            "at least one top-up-now bucket is present in the visible top-three near-threshold set",
            "Action Needed",
            "warn",
            "near-threshold-action-counts",
        )
    } else if headline.near_threshold_buckets.is_empty() {
        (
            "Complete",
            "good",
            "no near-threshold buckets are currently visible",
            "None",
            "warn",
            "near-threshold-presence",
        )
    } else {
        (
            "Complete",
            "good",
            "near-threshold buckets exist, but none are in the immediate top-up-now band",
            "Loaded",
            "good",
            "near-threshold-presence",
        )
    };

    let hero_badge_level = if headline.top_up_now_count > 0 {
        "warn"
    } else if headline.emitted_vs_seed.is_some()
        || headline.matched_vs_seed.is_some()
        || headline.filled_vs_seed.is_some()
        || !headline.near_threshold_buckets.is_empty()
    {
        "good"
    } else {
        ""
    };
    let near_threshold_bucket_labels = headline
        .near_threshold_buckets
        .iter()
        .map(near_threshold_bucket_label)
        .collect::<Vec<_>>();
    let top_up_now_labels = headline
        .near_threshold_buckets
        .iter()
        .filter(|bucket| bucket.suggested_action == "top-up-now")
        .map(near_threshold_bucket_label)
        .collect::<Vec<_>>();

    UiPrioritySummary {
        headline_status: headline_status.to_string(),
        headline_status_level: headline_status_level.to_string(),
        headline_status_reason: headline_status_reason.to_string(),
        near_threshold_status: near_threshold_status.to_string(),
        near_threshold_status_level: near_threshold_status_level.to_string(),
        priority_source: priority_source.to_string(),
        near_threshold_bucket_labels,
        top_up_now_labels,
        top_up_now_count: headline.top_up_now_count,
        ready_soon_count: headline.ready_soon_count,
        defer_count: headline.defer_count,
        hero_badge_text: format!("{} top-up-now", headline.top_up_now_count),
        hero_badge_level: hero_badge_level.to_string(),
        headline_explainer: headline.explainer.clone(),
    }
}

fn build_empty_ui_priority_summary() -> UiPrioritySummary {
    UiPrioritySummary {
        headline_status: "Complete".to_string(),
        headline_status_level: "good".to_string(),
        headline_status_reason:
            "no immediate top-up buckets in the visible top-three near-threshold set".to_string(),
        near_threshold_status: "None".to_string(),
        near_threshold_status_level: "warn".to_string(),
        priority_source: "near-threshold-presence".to_string(),
        near_threshold_bucket_labels: Vec::new(),
        top_up_now_labels: Vec::new(),
        top_up_now_count: 0,
        ready_soon_count: 0,
        defer_count: 0,
        hero_badge_text: "0 top-up-now".to_string(),
        hero_badge_level: "good".to_string(),
        headline_explainer:
            "No immediate top-up buckets in the visible top-three near-threshold set.".to_string(),
    }
}

fn near_threshold_bucket_label(bucket: &NearThresholdHeadlineBucket) -> String {
    format!(
        "{} / {} / {}",
        bucket.asset_class, bucket.horizon, bucket.event_subtype
    )
}

fn render_report(summary: &PipelineReportSummary) -> String {
    let mut out = String::new();
    out.push_str("# ");
    out.push_str(&summary.metadata.title);
    out.push_str("\n\n");

    if let Some(subtitle) = summary.metadata.subtitle.as_ref() {
        out.push_str(subtitle);
        out.push_str("\n\n");
    }

    out.push_str("generated_at_utc: ");
    out.push_str(&summary.metadata.generated_at_utc);
    out.push_str("\n\n");

    if !summary.metadata.tags.is_empty() {
        out.push_str("tags: ");
        out.push_str(&summary.metadata.tags.join(", "));
        out.push_str("\n\n");
    }

    if let Some(notes) = summary.metadata.notes.as_ref() {
        out.push_str("## Notes\n\n");
        out.push_str(notes);
        out.push_str("\n\n");
    }

    if let Some(seed) = summary.seed.as_ref() {
        out.push_str("## Seed Labels\n\n");
        out.push_str(&format!("- question_count: {}\n", seed.question_count));
        out.push_str(&format!("- replace_only: {}\n", seed.replace_only));
        render_distribution(&mut out, "by_asset", &seed.by_asset);
        render_distribution(&mut out, "by_asset_class", &seed.by_asset_class);
        render_distribution(&mut out, "by_market_type", &seed.by_market_type);
        render_distribution(&mut out, "by_event_subtype", &seed.by_event_subtype);
        out.push('\n');
    }

    if let Some(autolabel) = summary.autolabel.as_ref() {
        out.push_str("## Autolabel\n\n");
        out.push_str(&format!(
            "- already_labeled: {}\n",
            autolabel.already_labeled
        ));
        out.push_str(&format!("- filled: {}\n", autolabel.filled));
        out.push_str(&format!("- open_market: {}\n", autolabel.open_market));
        out.push_str(&format!("- missing_winner: {}\n", autolabel.missing_winner));
        out.push_str(&format!("- request_error: {}\n", autolabel.request_error));
        render_distribution(&mut out, "by_asset_class", &autolabel.by_asset_class);
        render_distribution(&mut out, "by_event_subtype", &autolabel.by_event_subtype);
        out.push('\n');
    }

    if let Some(prepare) = summary.prepare.as_ref() {
        out.push_str("## Prepare Calibration\n\n");
        out.push_str(&format!(
            "- total_candidate_rows: {}\n",
            prepare.total_candidate_rows
        ));
        out.push_str(&format!("- matched_labels: {}\n", prepare.matched_labels));
        out.push_str(&format!("- emitted_samples: {}\n", prepare.emitted_samples));
        out.push_str(&format!(
            "- missing_label_rows: {}\n",
            prepare.missing_label_rows
        ));
        out.push_str(&format!(
            "- invalid_label_rows: {}\n",
            prepare.invalid_label_rows
        ));
        render_distribution(&mut out, "by_asset", &prepare.by_asset);
        render_distribution(&mut out, "by_asset_class", &prepare.by_asset_class);
        render_distribution(&mut out, "by_market_type", &prepare.by_market_type);
        render_distribution(&mut out, "by_event_subtype", &prepare.by_event_subtype);
        out.push('\n');
    }

    if let Some(calibrate) = summary.calibrate.as_ref() {
        out.push_str("## Calibrate\n\n");
        out.push_str(&format!("- input_rows: {}\n", calibrate.input_rows));
        out.push_str(&format!(
            "- emitted_segment_count: {}\n",
            calibrate.emitted_segment_count
        ));
        out.push_str(&format!(
            "- skipped_segment_count: {}\n",
            calibrate.skipped_segment_count
        ));
        out.push_str(&format!("- min_samples: {}\n", calibrate.min_samples));
        out.push_str(&format!(
            "- group_by_asset_class: {}\n",
            calibrate.grouping.group_by_asset_class
        ));
        out.push_str(&format!(
            "- group_by_event_subtype: {}\n",
            calibrate.grouping.group_by_event_subtype
        ));
        out.push_str(&format!(
            "- short_horizon_max_days: {}\n",
            calibrate.grouping.short_horizon_max_days
        ));
        out.push_str(&format!(
            "- medium_horizon_max_days: {}\n",
            calibrate.grouping.medium_horizon_max_days
        ));
        let mut skipped_by_reason = BTreeMap::new();
        for skipped in &calibrate.skipped_segments {
            *skipped_by_reason
                .entry(skipped.reason.clone())
                .or_insert(0usize) += skipped.count;
        }
        render_distribution(&mut out, "skipped_by_reason", &skipped_by_reason);
        render_underfilled_buckets(&mut out, &calibrate.underfilled_buckets);
        render_merge_diff_summary(&mut out, calibrate.merge_diff_summary.as_ref());
        render_skipped_segments(&mut out, &calibrate.skipped_segments);
        out.push('\n');
    }

    if let Some(headline) = summary.headline.as_ref() {
        out.push_str("## Headline\n\n");
        if let Some(ui_priority_summary) = summary.ui_priority_summary.as_ref() {
            out.push_str("- ui_priority_summary:\n");
            out.push_str(&format!(
                "  - headline_status: {} ({})\n",
                ui_priority_summary.headline_status, ui_priority_summary.headline_status_level
            ));
            out.push_str(&format!(
                "  - near_threshold_status: {} ({})\n",
                ui_priority_summary.near_threshold_status,
                ui_priority_summary.near_threshold_status_level
            ));
            out.push_str(&format!(
                "  - priority_source: {}\n",
                ui_priority_summary.priority_source
            ));
            out.push_str(&format!(
                "  - headline_status_reason: {}\n",
                ui_priority_summary.headline_status_reason
            ));
            out.push_str(&format!(
                "  - hero_badge: {} ({})\n",
                ui_priority_summary.hero_badge_text, ui_priority_summary.hero_badge_level
            ));
            render_label_list(
                &mut out,
                "top_up_now_labels",
                &ui_priority_summary.top_up_now_labels,
            );
            render_label_list(
                &mut out,
                "near_threshold_bucket_labels",
                &ui_priority_summary.near_threshold_bucket_labels,
            );
        }
        render_ratio_line(
            &mut out,
            "emitted_vs_seed",
            headline.emitted_vs_seed.as_ref(),
        );
        render_ratio_line(
            &mut out,
            "matched_vs_seed",
            headline.matched_vs_seed.as_ref(),
        );
        render_ratio_line(&mut out, "filled_vs_seed", headline.filled_vs_seed.as_ref());
        out.push_str(&format!(
            "- near_threshold_action_counts: top-up-now={}, ready-soon={}, defer={}\n",
            headline.top_up_now_count, headline.ready_soon_count, headline.defer_count
        ));
        out.push_str("- headline_explainer: ");
        out.push_str(&headline.explainer);
        out.push('\n');
        out.push_str("- near_threshold_buckets:\n");
        if headline.near_threshold_buckets.is_empty() {
            out.push_str("  - none\n");
        } else {
            for bucket in &headline.near_threshold_buckets {
                out.push_str(&format!(
                    "  - {} / {} / {}: gap={}, rows={}, action={}\n",
                    bucket.asset_class,
                    bucket.horizon,
                    bucket.event_subtype,
                    bucket.gap_to_min_samples,
                    bucket.skipped_row_count,
                    bucket.suggested_action
                ));
            }
        }
        out.push('\n');
    }

    out
}

fn render_html_document(summary: &PipelineReportSummary) -> Result<String> {
    const PLACEHOLDER: &str =
        "<script id=\"pipeline-report-data\" type=\"application/json\"></script>";
    let template = include_str!("../../docs/crypto-pipeline-report.html");
    let json =
        serde_json::to_string_pretty(summary).context("failed to serialize aggregate json")?;
    let escaped_json = json.replace("</script>", "<\\/script>");
    let injected = format!(
        "<script id=\"pipeline-report-data\" type=\"application/json\">\n{escaped_json}\n</script>"
    );
    Ok(template.replacen(PLACEHOLDER, &injected, 1))
}

fn render_distribution(out: &mut String, title: &str, values: &BTreeMap<String, usize>) {
    out.push_str(&format!("- {}:\n", title));
    if values.is_empty() {
        out.push_str("  - none\n");
        return;
    }
    for (key, count) in values {
        out.push_str(&format!("  - {}: {}\n", key, count));
    }
}

fn render_label_list(out: &mut String, title: &str, values: &[String]) {
    out.push_str(&format!("  - {}:\n", title));
    if values.is_empty() {
        out.push_str("    - none\n");
        return;
    }
    for value in values {
        out.push_str(&format!("    - {}\n", value));
    }
}

fn render_ratio_line(out: &mut String, title: &str, ratio: Option<&RatioSummary>) {
    match ratio {
        Some(ratio) => out.push_str(&format!(
            "- {}: {}/{} ({:.1}%)\n",
            title,
            ratio.numerator,
            ratio.denominator,
            ratio.ratio * 100.0
        )),
        None => out.push_str(&format!("- {}: n/a\n", title)),
    }
}

fn render_skipped_segments(out: &mut String, skipped_segments: &[CalibrateSkippedSegment]) {
    out.push_str("- skipped_segments:\n");
    if skipped_segments.is_empty() {
        out.push_str("  - none\n");
        return;
    }

    for skipped in skipped_segments.iter().take(10) {
        out.push_str(&format!(
            "  - {} / {} / {} / {} / {}: {} ({})\n",
            skipped.key.asset,
            fallback_selector(&skipped.key.asset_class),
            skipped.key.horizon,
            skipped.key.market_type,
            fallback_selector(&skipped.key.event_subtype),
            skipped.count,
            skipped.reason
        ));
    }
    if skipped_segments.len() > 10 {
        out.push_str(&format!(
            "  - ... {} more skipped segments\n",
            skipped_segments.len() - 10
        ));
    }
}

fn render_underfilled_buckets(out: &mut String, buckets: &[CalibrateUnderfilledBucket]) {
    out.push_str("- underfilled_buckets:\n");
    if buckets.is_empty() {
        out.push_str("  - none\n");
        return;
    }

    for bucket in buckets.iter().take(10) {
        out.push_str(&format!(
            "  - {} / {} / {}: segments={}, rows={}, gap={}, band={}\n",
            fallback_selector(&bucket.key.asset_class),
            fallback_selector(&bucket.key.horizon),
            fallback_selector(&bucket.key.event_subtype),
            bucket.skipped_segment_count,
            bucket.skipped_row_count,
            bucket.gap_to_min_samples,
            bucket.threshold_band
        ));
    }
    if buckets.len() > 10 {
        out.push_str(&format!(
            "  - ... {} more underfilled buckets\n",
            buckets.len() - 10
        ));
    }
}

fn render_merge_diff_summary(
    out: &mut String,
    merge_diff_summary: Option<&CalibrateMergeDiffSummary>,
) {
    out.push_str("- merge_diff_summary:\n");
    let Some(merge_diff_summary) = merge_diff_summary else {
        out.push_str("  - none\n");
        return;
    };

    out.push_str(&format!(
        "  - counts: new={}, updated={}, unchanged={}\n",
        merge_diff_summary.new_row_count,
        merge_diff_summary.updated_row_count,
        merge_diff_summary.unchanged_row_count
    ));
    render_merge_diff_rows(out, "new_rows", &merge_diff_summary.new_rows);
    render_merge_diff_rows(out, "updated_rows", &merge_diff_summary.updated_rows);
    render_merge_diff_rows(out, "unchanged_rows", &merge_diff_summary.unchanged_rows);
}

fn render_merge_diff_rows(out: &mut String, title: &str, rows: &[String]) {
    out.push_str(&format!("  - {}:\n", title));
    if rows.is_empty() {
        out.push_str("    - none\n");
        return;
    }

    for row in rows.iter().take(10) {
        out.push_str(&format!("    - {}\n", row));
    }
    if rows.len() > 10 {
        out.push_str(&format!("    - ... {} more rows\n", rows.len() - 10));
    }
}

fn fallback_selector(value: &str) -> &str {
    if value.is_empty() { "any" } else { value }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_includes_headline_ratio_when_seed_and_prepare_exist() {
        let seed = SeedLabelsSummary {
            question_count: 10,
            replace_only: false,
            by_asset: BTreeMap::new(),
            by_asset_class: BTreeMap::new(),
            by_market_type: BTreeMap::new(),
            by_event_subtype: BTreeMap::new(),
        };
        let prepare = PreparationSummary {
            total_candidate_rows: 10,
            matched_labels: 8,
            emitted_samples: 6,
            missing_label_rows: 2,
            invalid_label_rows: 0,
            by_asset: BTreeMap::new(),
            by_asset_class: BTreeMap::new(),
            by_market_type: BTreeMap::new(),
            by_event_subtype: BTreeMap::new(),
        };

        let summary = build_summary(
            Some(seed),
            None,
            Some(prepare),
            None,
            None,
            None,
            None,
            Vec::new(),
        );
        let report = render_report(&summary);
        assert!(report.contains("emitted_vs_seed: 6/10 (60.0%)"));
        assert!(report.contains("matched_vs_seed: 8/10 (80.0%)"));
    }

    #[test]
    fn report_handles_empty_distributions() {
        let seed = SeedLabelsSummary {
            question_count: 1,
            replace_only: true,
            by_asset: BTreeMap::new(),
            by_asset_class: BTreeMap::new(),
            by_market_type: BTreeMap::new(),
            by_event_subtype: BTreeMap::new(),
        };
        let summary = build_summary(Some(seed), None, None, None, None, None, None, Vec::new());
        let report = render_report(&summary);
        assert!(report.contains("none"));
    }

    #[test]
    fn summary_includes_all_headline_ratios() {
        let seed = SeedLabelsSummary {
            question_count: 20,
            replace_only: false,
            by_asset: BTreeMap::new(),
            by_asset_class: BTreeMap::new(),
            by_market_type: BTreeMap::new(),
            by_event_subtype: BTreeMap::new(),
        };
        let autolabel = AutolabelSummary {
            already_labeled: 3,
            filled: 7,
            open_market: 4,
            missing_winner: 1,
            request_error: 0,
            by_asset_class: BTreeMap::new(),
            by_event_subtype: BTreeMap::new(),
        };
        let prepare = PreparationSummary {
            total_candidate_rows: 30,
            matched_labels: 12,
            emitted_samples: 9,
            missing_label_rows: 5,
            invalid_label_rows: 1,
            by_asset: BTreeMap::new(),
            by_asset_class: BTreeMap::new(),
            by_market_type: BTreeMap::new(),
            by_event_subtype: BTreeMap::new(),
        };

        let summary = build_summary(
            Some(seed),
            Some(autolabel),
            Some(prepare),
            None,
            None,
            None,
            None,
            Vec::new(),
        );
        let headline = summary.headline.expect("headline should exist");
        let emitted = headline.emitted_vs_seed.expect("emitted ratio");
        let matched = headline.matched_vs_seed.expect("matched ratio");
        let filled = headline.filled_vs_seed.expect("filled ratio");

        assert_eq!(emitted.numerator, 9);
        assert_eq!(emitted.denominator, 20);
        assert!((emitted.ratio - 0.45).abs() < 1e-9);
        assert_eq!(matched.numerator, 12);
        assert_eq!(filled.numerator, 10);
        assert_eq!(headline.top_up_now_count, 0);
        assert_eq!(headline.ready_soon_count, 0);
        assert_eq!(headline.defer_count, 0);
        assert_eq!(
            headline.explainer,
            "No immediate top-up buckets in the visible top-three near-threshold set."
        );
        assert!(headline.near_threshold_buckets.is_empty());
    }

    #[test]
    fn html_document_embeds_aggregate_json() {
        let summary = build_summary(
            Some(SeedLabelsSummary {
                question_count: 2,
                replace_only: false,
                by_asset: BTreeMap::new(),
                by_asset_class: BTreeMap::new(),
                by_market_type: BTreeMap::new(),
                by_event_subtype: BTreeMap::new(),
            }),
            None,
            None,
            None,
            Some("Batch A".to_string()),
            Some("2026-03-20".to_string()),
            Some("Manual note".to_string()),
            vec!["eth".to_string(), "batch-a".to_string()],
        );

        let html = render_html_document(&summary).expect("html render should succeed");
        assert!(html.contains("\"question_count\": 2"));
        assert!(html.contains("pipeline-report-data"));
        assert!(html.contains("\"title\": \"Batch A\""));
        assert!(html.contains("\"generated_at_utc\""));
    }

    #[test]
    fn report_renders_custom_title_and_subtitle() {
        let summary = build_summary(
            None,
            None,
            None,
            None,
            Some("March Batch".to_string()),
            Some("BTC + ETH sample set".to_string()),
            None,
            vec!["replace-only".to_string()],
        );
        let report = render_report(&summary);
        assert!(report.starts_with("# March Batch"));
        assert!(report.contains("BTC + ETH sample set"));
        assert!(report.contains("generated_at_utc: "));
        assert!(report.contains("tags: replace-only"));
    }

    #[test]
    fn report_renders_notes_section() {
        let summary = build_summary(
            None,
            None,
            None,
            None,
            None,
            None,
            Some("Operator note line 1\nline 2".to_string()),
            Vec::new(),
        );
        let report = render_report(&summary);
        assert!(report.contains("## Notes"));
        assert!(report.contains("Operator note line 1"));
    }

    #[test]
    fn normalize_tags_trims_sorts_and_dedups() {
        let tags = normalize_tags(vec![
            " beta ".to_string(),
            "alpha".to_string(),
            "".to_string(),
            "beta".to_string(),
        ]);
        assert_eq!(tags, vec!["alpha".to_string(), "beta".to_string()]);
    }

    #[test]
    fn resolve_summary_paths_prefers_explicit_over_directory_defaults() {
        let input_dir = PathBuf::from("/tmp/batch");
        let explicit_seed = PathBuf::from("/tmp/custom/seed.json");
        let (seed, autolabel, prepare, calibrate) = resolve_summary_paths(
            Some(&input_dir),
            Some(explicit_seed.clone()),
            None,
            None,
            None,
        );
        assert_eq!(seed, Some(explicit_seed));
        assert_eq!(autolabel, None);
        assert_eq!(prepare, None);
        assert_eq!(calibrate, None);
    }

    #[test]
    fn resolve_output_paths_uses_standard_filenames_inside_output_dir() {
        let output_dir = std::env::temp_dir().join(format!(
            "crypto-pipeline-report-test-{}",
            std::process::id()
        ));
        let (markdown, json, html) = resolve_output_paths(Some(&output_dir), None, None, None)
            .expect("output dir should resolve");
        assert_eq!(markdown, Some(output_dir.join("crypto_pipeline_report.md")));
        assert_eq!(json, Some(output_dir.join("crypto_pipeline_report.json")));
        assert_eq!(html, Some(output_dir.join("crypto_pipeline_report.html")));
        let _ = fs::remove_dir_all(output_dir);
    }

    #[test]
    fn resolve_output_paths_prefers_explicit_paths_over_output_dir_defaults() {
        let output_dir = PathBuf::from("/tmp/batch-output");
        let explicit_html = PathBuf::from("/tmp/custom/report.html");
        let (markdown, json, html) =
            resolve_output_paths(Some(&output_dir), None, None, Some(explicit_html.clone()))
                .expect("output paths should resolve");
        assert_eq!(markdown, Some(output_dir.join("crypto_pipeline_report.md")));
        assert_eq!(json, Some(output_dir.join("crypto_pipeline_report.json")));
        assert_eq!(html, Some(explicit_html));
    }

    #[test]
    fn validate_summary_inputs_rejects_empty_input_set() {
        let err = validate_summary_inputs(None, None, None, None, None)
            .expect_err("expected empty input set to fail");
        assert!(err.to_string().contains("no summary inputs provided"));
    }

    #[test]
    fn report_renders_calibrate_section() {
        let summary = build_summary(
            None,
            None,
            None,
            Some(CalibrateSummary {
                input_rows: 42,
                emitted_segment_count: 2,
                skipped_segment_count: 1,
                min_samples: 20,
                grouping: CalibrateGroupingSummary {
                    group_by_asset_class: true,
                    group_by_event_subtype: true,
                    short_horizon_max_days: 1,
                    medium_horizon_max_days: 7,
                },
                emitted_segments: Vec::new(),
                underfilled_buckets: vec![CalibrateUnderfilledBucket {
                    key: CalibrateUnderfilledBucketKey {
                        asset_class: "alt".to_string(),
                        horizon: "short".to_string(),
                        event_subtype: "unlock".to_string(),
                    },
                    skipped_segment_count: 2,
                    skipped_row_count: 9,
                    gap_to_min_samples: 31,
                    threshold_band: "far-from-threshold".to_string(),
                }],
                merge_diff_summary: Some(CalibrateMergeDiffSummary {
                    new_row_count: 1,
                    updated_row_count: 2,
                    unchanged_row_count: 0,
                    new_rows: vec!["alt / short / unlock".to_string()],
                    updated_rows: vec![
                        "major / medium / regulatory".to_string(),
                        "alt / any / regulatory".to_string(),
                    ],
                    unchanged_rows: Vec::new(),
                }),
                skipped_segments: vec![CalibrateSkippedSegment {
                    key: CalibrateSegmentKey {
                        asset: "*".to_string(),
                        asset_class: "alt".to_string(),
                        horizon: "short".to_string(),
                        market_type: "binary".to_string(),
                        event_subtype: "unlock".to_string(),
                    },
                    count: 9,
                    reason: "insufficient_samples".to_string(),
                }],
            }),
            None,
            None,
            None,
            Vec::new(),
        );
        let report = render_report(&summary);
        assert!(report.contains("## Calibrate"));
        assert!(report.contains("skipped_segment_count: 1"));
        assert!(report.contains("insufficient_samples"));
        assert!(report.contains("* / alt / short / binary / unlock"));
        assert!(report.contains("merge_diff_summary"));
        assert!(report.contains("counts: new=1, updated=2, unchanged=0"));
        assert!(report.contains("major / medium / regulatory"));
        assert!(
            report.contains(
                "alt / short / unlock: segments=2, rows=9, gap=31, band=far-from-threshold"
            )
        );
    }

    #[test]
    fn headline_includes_top_near_threshold_buckets() {
        let summary = build_summary(
            None,
            None,
            None,
            Some(CalibrateSummary {
                input_rows: 100,
                emitted_segment_count: 4,
                skipped_segment_count: 4,
                min_samples: 20,
                grouping: CalibrateGroupingSummary {
                    group_by_asset_class: true,
                    group_by_event_subtype: true,
                    short_horizon_max_days: 1,
                    medium_horizon_max_days: 7,
                },
                emitted_segments: Vec::new(),
                underfilled_buckets: vec![
                    CalibrateUnderfilledBucket {
                        key: CalibrateUnderfilledBucketKey {
                            asset_class: "alt".to_string(),
                            horizon: "short".to_string(),
                            event_subtype: "unlock".to_string(),
                        },
                        skipped_segment_count: 1,
                        skipped_row_count: 18,
                        gap_to_min_samples: 2,
                        threshold_band: "near-threshold".to_string(),
                    },
                    CalibrateUnderfilledBucket {
                        key: CalibrateUnderfilledBucketKey {
                            asset_class: "major".to_string(),
                            horizon: "medium".to_string(),
                            event_subtype: "regulatory".to_string(),
                        },
                        skipped_segment_count: 2,
                        skipped_row_count: 30,
                        gap_to_min_samples: 1,
                        threshold_band: "near-threshold".to_string(),
                    },
                    CalibrateUnderfilledBucket {
                        key: CalibrateUnderfilledBucketKey {
                            asset_class: "alt".to_string(),
                            horizon: "long".to_string(),
                            event_subtype: "upgrade".to_string(),
                        },
                        skipped_segment_count: 1,
                        skipped_row_count: 25,
                        gap_to_min_samples: 3,
                        threshold_band: "near-threshold".to_string(),
                    },
                    CalibrateUnderfilledBucket {
                        key: CalibrateUnderfilledBucketKey {
                            asset_class: "alt".to_string(),
                            horizon: "medium".to_string(),
                            event_subtype: "unlock".to_string(),
                        },
                        skipped_segment_count: 1,
                        skipped_row_count: 12,
                        gap_to_min_samples: 5,
                        threshold_band: "near-threshold".to_string(),
                    },
                ],
                merge_diff_summary: None,
                skipped_segments: Vec::new(),
            }),
            None,
            None,
            None,
            Vec::new(),
        );
        let headline = summary.headline.as_ref().expect("headline should exist");
        assert_eq!(headline.near_threshold_buckets.len(), 3);
        assert_eq!(headline.near_threshold_buckets[0].asset_class, "major");
        assert_eq!(headline.near_threshold_buckets[0].gap_to_min_samples, 1);
        assert_eq!(
            headline.near_threshold_buckets[0].suggested_action,
            "top-up-now"
        );
        assert_eq!(headline.top_up_now_count, 1);
        assert_eq!(headline.ready_soon_count, 2);
        assert_eq!(headline.defer_count, 0);
        assert_eq!(
            headline.explainer,
            "At least one bucket is one sample away from calibration readiness."
        );
        let ui = summary
            .ui_priority_summary
            .as_ref()
            .expect("ui priority summary should exist");
        assert_eq!(ui.headline_status, "Action Needed");
        assert_eq!(ui.priority_source, "near-threshold-action-counts");
        assert_eq!(
            ui.headline_status_reason,
            "at least one top-up-now bucket is present in the visible top-three near-threshold set"
        );
        assert_eq!(
            ui.top_up_now_labels,
            vec!["major / medium / regulatory".to_string()]
        );
        assert_eq!(
            ui.near_threshold_bucket_labels,
            vec![
                "major / medium / regulatory".to_string(),
                "alt / short / unlock".to_string(),
                "alt / long / upgrade".to_string()
            ]
        );

        let report = render_report(&summary);
        assert!(report.contains("ui_priority_summary"));
        assert!(report.contains("priority_source: near-threshold-action-counts"));
        assert!(report.contains("top_up_now_labels"));
        assert!(report.contains("major / medium / regulatory"));
        assert!(
            report.contains("near_threshold_action_counts: top-up-now=1, ready-soon=2, defer=0")
        );
        assert!(report.contains(
            "headline_explainer: At least one bucket is one sample away from calibration readiness."
        ));
        assert!(report.contains("near_threshold_buckets"));
        assert!(report.contains("major / medium / regulatory: gap=1, rows=30, action=top-up-now"));
    }
}
