# Crypto Calibration Workflow

This document describes the lightweight offline workflow for generating
`crypto_alpha.calibration_overrides` suggestions from historical labeled samples.

## Goal

Turn historical crypto market observations into table-driven calibration rows keyed by:

- `asset`
- `horizon`
- `market_type`

The generated rows currently target:

- `probability_calibration`

The existing runtime config can still layer manual `sigma_multiplier` and
`size_multiplier` values onto the emitted rows if desired.

## Input Format

The CLI consumes newline-delimited JSON (`.jsonl`). Each line must contain:

- `modeled_prob`: raw model probability before calibration, between `0` and `1`
- `resolved_yes` or `resolved_value`: realized outcome as `true/false` or `0/1`

Each line must also provide enough metadata to infer the segment:

Option A:

- `asset`
- `market_type`
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
  --medium-horizon-max-days 7
```

## Output

The command prints TOML-ready `[[crypto_alpha.calibration_overrides]]` blocks with comments:

```toml
# BTCUSDT / long / binary | samples=42 | brier_before=0.221944 | brier_after=0.208311
[[crypto_alpha.calibration_overrides]]
asset = "BTCUSDT"
horizon = "long"
market_type = "binary"
probability_calibration = 0.9132
```

## Fitting Rule

The generated factor uses the current runtime shrink form:

`p_calibrated = 0.5 + (p_raw - 0.5) * factor`

For each segment, the CLI fits `factor` by minimizing squared error against the
realized binary outcome, then clamps the result to the requested bounds.

This is intentionally lightweight. It is meant to produce a practical first-pass
override table, not a full research pipeline.
