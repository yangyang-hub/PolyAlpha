# Crypto Calibration Workflow

This document describes the lightweight offline workflow for generating
`crypto_alpha.calibration_overrides` suggestions from historical labeled samples.

If you first need to preserve recent live diagnostics, the repository also
includes a small exporter:

```bash
cargo run --bin crypto_export_diagnostics -- \
  --base-url http://127.0.0.1:8080 \
  --output tmp/crypto_diagnostics.jsonl
```

That exporter writes `candidate_decision` and `exit_decision` rows from the
local monitor API into JSONL. Those rows are useful as a raw replay/labeling
input, but they are not yet valid `crypto_calibrate` samples until you enrich
them with realized outcomes and `modeled_prob`.

If your exported diagnostics already include the runtime candidate-side modeled
probabilities, you can join them with a small label file and emit
`crypto_calibrate` samples directly:

If you do not have the label file yet, generate a de-duplicated skeleton first:

```bash
cargo run --bin crypto_seed_labels -- \
  --diagnostics tmp/crypto_diagnostics.jsonl \
  --output tmp/crypto_labels.jsonl \
  --summary-output tmp/crypto_seed_summary.json
```

That command pre-fills:

- `question`
- `asset`
- `asset_class`
- `market_type`
- `event_subtype`

If `--summary-output` is provided, it also writes a small JSON summary with the
deduplicated question count and the resulting distribution by asset, asset class,
market type, and event subtype.

You then fill in:

- `resolved_yes` or `resolved_value`
- optional `resolution_at`

If some of those markets are already resolved, you can auto-fill them first:

```bash
cargo run --bin crypto_autolabel_resolved -- \
  --labels tmp/crypto_labels.jsonl \
  --output tmp/crypto_labels_filled.jsonl \
  --unresolved-output tmp/crypto_labels_unresolved.jsonl \
  --summary-output tmp/crypto_autolabel_summary.json
```

That command uses each row's `condition_id` to query the single-market CLOB
endpoint and fills:

- `resolved_yes`
- `resolution_at`

Rows that still need manual work are written to the unresolved file with a
machine-readable reason such as `open_market`, `missing_winner`, or
`request_error`. If `--summary-output` is provided, the same per-reason counts
plus unresolved distributions by `asset_class` and `event_subtype` are also
written as JSON.

```bash
cargo run --bin crypto_prepare_calibration -- \
  --diagnostics tmp/crypto_diagnostics.jsonl \
  --labels tmp/crypto_labels_filled.jsonl \
  --output tmp/crypto_samples.jsonl \
  --summary-output tmp/crypto_prepare_summary.json
```

At the end, the command prints a small preparation summary with:

- `total_candidates`
- `matched_labels`
- `emitted_samples`
- `missing_labels`
- `invalid_labels`
- `by_asset`
- `by_asset_class`
- `by_market_type`
- `by_event_subtype`

If `--summary-output` is provided, the same summary is also written as JSON for
machine-readable follow-up analysis.

You can then fit segment-level calibration suggestions and emit a matching
machine-readable summary:

```bash
cargo run --bin crypto_calibrate -- \
  --input tmp/crypto_samples.jsonl \
  --min-samples 20 \
  --short-horizon-max-days 1 \
  --medium-horizon-max-days 7 \
  --group-by-asset-class \
  --group-by-event-subtype \
  --override-output tmp/crypto_calibration_overrides.toml \
  --summary-output tmp/crypto_calibrate_summary.json
```

The calibrate summary includes:

- `input_rows`
- `emitted_segment_count`
- `skipped_segment_count`
- emitted segments
- skipped segments with reasons such as `insufficient_samples`
- aggregated `underfilled_buckets` keyed by `asset_class × horizon × event_subtype`
- `gap_to_min_samples` for each underfilled bucket, showing how many more labeled rows are still needed to bring the skipped segments in that bucket up to `min_samples`
- `threshold_band` for each underfilled bucket, currently `near-threshold` or `far-from-threshold`

The pipeline report then lifts the top `near-threshold` buckets into a headline
view so the next labeling targets are visible without scanning the full skipped
bucket list, and annotates them with a lightweight action hint such as
`top-up-now`, `ready-soon`, or `defer`. The same headline also summarizes the
action counts across the visible top-3 near-threshold buckets.

To combine the four summary files into one readable report:

```bash
cargo run --bin crypto_pipeline_report -- \
  --input-dir tmp \
  --output-dir tmp/report \
  --title "March 2026 Crypto Calibration Batch" \
  --subtitle "BTC/ETH replace-only run" \
  --notes-file tmp/crypto_pipeline_notes.txt \
  --tag replace-only \
  --tag majors \
```

Without `--output`, the report is printed to stdout.

If you prefer a local browser view instead of generating markdown, open
[`crypto-pipeline-report.html`](./crypto-pipeline-report.html) and load:

- `tmp/crypto_pipeline_report.html`
- `tmp/crypto_pipeline_report.json`
- or:
- `tmp/crypto_seed_summary.json`
- `tmp/crypto_autolabel_summary.json`
- `tmp/crypto_prepare_summary.json`
- `tmp/crypto_calibrate_summary.json`

The page is fully static and renders everything client-side.
Every output format also includes an automatic `generated_at_utc` timestamp plus optional batch notes and repeatable tags from `--notes`, `--notes-file`, and `--tag`.
`--input-dir` can be used to auto-discover the four standard summary filenames inside one batch directory.
`--output-dir` can be used to emit the standard markdown/JSON/HTML report trio into one batch output directory.
If all four summaries are missing, `crypto_pipeline_report` now fails fast instead of emitting an empty report artifact.

Each `tmp/crypto_labels.jsonl` row should contain:

- `question`
- `resolved_yes` or `resolved_value`
- optional `resolution_at`

## Goal

Turn historical crypto market observations into table-driven calibration rows keyed by:

- `asset`
- optional `asset_class`
- `horizon`
- `market_type`
- optional `event_subtype`

The generated rows currently target:

- `probability_calibration`

When grouped more coarsely, the emitted override rows can also use
`asset = "*"` plus `asset_class`, and can optionally split on `event_subtype`.

## Input Format

The CLI consumes newline-delimited JSON (`.jsonl`). Each line must contain:

- `modeled_prob`: raw model probability before calibration, between `0` and `1`
- `resolved_yes` or `resolved_value`: realized outcome as `true/false` or `0/1`

Each line must also provide enough metadata to infer the segment:

Option A:

- `asset`
- optional `asset_class`
- `market_type`
- optional `event_subtype`
- `days_to_resolution`

Option B:

- `question`
- `days_to_resolution`

Option C:

- `question`
- `observed_at`
- `resolution_at`

Example:

```json
{"question":"Will Bitcoin reach $200,000 by December 31, 2026?","modeled_prob":0.64,"resolved_yes":false,"days_to_resolution":30}
{"question":"Will Ethereum reach $5,000 by June 30, 2026?","modeled_prob":0.58,"resolved_yes":true,"days_to_resolution":12}
{"asset":"BTCUSDT","market_type":"range","modeled_prob":0.72,"resolved_value":1.0,"days_to_resolution":1}
```

## Command

```bash
cargo run --bin crypto_calibrate -- \
  --input tmp/crypto_samples.jsonl \
  --min-samples 20 \
  --short-horizon-max-days 1 \
  --medium-horizon-max-days 7 \
  --group-by-asset-class \
  --group-by-event-subtype \
  --summary-output tmp/crypto_calibrate_summary.json
```

## Output

The command prints TOML-ready `[[crypto_alpha.calibration_overrides]]` blocks with comments:

```toml
# * / alt / long / binary / unlock | samples=42 | brier_before=0.221944 | brier_after=0.208311
[[crypto_alpha.calibration_overrides]]
asset = "*"
asset_class = "alt"
horizon = "long"
market_type = "binary"
event_subtype = "unlock"
probability_calibration = 0.9132
```

If `--summary-output` is provided, the same run also writes
`tmp/crypto_calibrate_summary.json`, including emitted segments and skipped
underfilled segments so downstream reports can highlight buckets that do not yet
have enough samples. When merge mode is active, that JSON now also includes a
machine-readable `merge_diff_summary` with `new/updated/unchanged` row counts
and selector lists.

If `--override-output` is provided, the TOML-ready override blocks are also
written to a standalone merge-ready file instead of requiring manual copy from
stdout.

If `--existing-overrides-input` is provided, `crypto_calibrate` first loads the
existing TOML config/fragment, matches rows by exact selector
(`asset / asset_class / horizon / market_type / event_subtype`), and then only
updates or appends `probability_calibration`. Existing `sigma_multiplier`,
`size_multiplier`, `min_edge_multiplier`, and `max_spread_multiplier` values are
preserved.

`--merge-mode` controls that behavior:

- `probability-only` (default): update only `probability_calibration` on exact selector match
- `replace-row`: replace the full matching row with the freshly generated minimal row
- `append-only`: never overwrite matches; append a new row even if the selector already exists

When merge mode is used, the generated TOML fragment also includes a comment
header summarizing:

- `new_rows`
- `updated_rows`
- `unchanged_rows`

along with the concrete selector labels in each bucket, so review can start from
the diff summary before reading the full override fragment.

## Fitting Rule

The generated factor uses the current runtime shrink form:

`p_calibrated = 0.5 + (p_raw - 0.5) * factor`

For each segment, the CLI fits `factor` by minimizing squared error against the
realized binary outcome, then clamps the result to the requested bounds.

This is intentionally lightweight. It is meant to produce a practical first-pass
override table, not a full research pipeline.
