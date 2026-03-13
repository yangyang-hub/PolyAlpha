# AGENTS.md

## Purpose
This file records repository-specific working agreements, high-level project context, and a lightweight change log for code changes made in this repository.

## Project Overview
- `PolyAlpha` is a Polymarket quantitative directional trading bot.
- The repository is a Rust workspace with a web frontend and monitoring API.
- The current production focus is weather trading, with additional infrastructure for other strategies and liquidity-rewards workflows.

## Repository Layout
- `src/`
  - Binary entrypoint and application orchestration.
  - `src/app/` contains bootstrapping, runtime wiring, account setup, task spawning, and liquidity-rewards orchestration.
- `crates/pa-core`
  - Shared domain types, configuration, weather location metadata, and strategy/risk enums.
- `crates/pa-market-data`
  - Gamma discovery, Data API access, order book websocket feeds, wallet tracking.
- `crates/pa-strategy`
  - Trading logic for weather and other strategies, execution planning, strategy-engine flow.
- `crates/pa-execution`
  - CLOB order placement, orchestration, fill handling.
- `crates/pa-risk`
  - Risk checks, position limits, cooldowns, circuit breakers.
- `crates/pa-monitor`
  - Metrics, status/config API, frontend static serving.
- `frontend/`
  - Monitoring/configuration UI.
- `config/`
  - Default runtime configuration and environment-specific overrides.
- `docs/`
  - Operational notes, audit checklists, and design docs.

## Runtime Model
- The bot loads config from:
  1. `config/default.toml`
  2. `config/{RUN_MODE}.toml` if present
  3. `PA_` prefixed environment variables
  4. persisted config-store overrides if enabled
  5. `PA_` environment variables are re-applied last as highest priority
- Trading is multi-account only.
- Accounts must be explicitly configured through `[[accounts]]` or `PA_ACCOUNT_<N>_*` environment variables.
- The monitoring API and frontend are served by `pa-monitor`.

## Weather Strategy Notes
- Weather city/provider metadata lives in `crates/pa-core/src/weather.rs`.
- Trade-enabled cities are currently NOAA-backed; international cities such as London and Seoul are audit-only unless explicitly enabled later.
- Settlement-risk adjustments and provider-aware routing are part of the live weather strategy path.
- Any weather-market logic change should be checked against:
  - order book side validation
  - stale-liquidity date handling
  - provider/timezone handling
  - execution freshness checks
  - metrics and frontend observability

## Common Commands
- Compile main binary only:
  - `cargo test -q -p polyalpha --bin polyalpha --no-run`
- Run weather strategy tests:
  - `cargo test -q -p pa-strategy weather`
- Run engine tests:
  - `cargo test -q -p pa-strategy engine`
- Run monitor compilation/tests:
  - `cargo test -q -p pa-monitor --no-run`
- Build frontend:
  - `npm run build --prefix frontend`

## Change Recording Rules
- Every code change must be accompanied by an update to this file.
- Entries should be brief and high-signal.
- Each code-change entry should include:
  - Date
  - Changed area
  - What changed
  - Why it changed
- Documentation-only changes may be omitted unless they affect engineering workflow or agent behavior.

## Maintenance Expectations
- Prefer updating shared metadata instead of duplicating strategy-local lookup tables.
- Keep frontend monitoring labels aligned with backend metric semantics.
- When changing config behavior, update both runtime code and operator-facing docs/examples.
- When changing execution or risk behavior, add or update focused regression tests.

## Change Log

### 2026-03-13
- Area: `AGENTS.md`
- Change: Added repository overview, module layout, runtime/config notes, common commands, and maintenance expectations.
- Why: Make the file useful as an operational and engineering guide instead of only a change ledger.

### 2026-03-13
- Area: `crates/pa-core/src/weather.rs`, `crates/pa-strategy/src/weather.rs`
- Change: Added shared weather location timezones to `WeatherLocation`, introduced shared `weather_timezone(location)`, and removed the duplicate strategy-local timezone mapping from `market_local_today()`.
- Why: Prevent future drift between shared city metadata and stale-liquidity local-day logic.
