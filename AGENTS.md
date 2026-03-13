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
  4. `PA_` environment variables are re-applied last as highest priority
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
- Area: `src/app/bootstrap.rs`, `src/app/market_runtime.rs`, `src/main.rs`, `crates/pa-monitor/src/api.rs`, `crates/pa-monitor/Cargo.toml`, `crates/pa-storage/src/lib.rs`
- Change: Removed the disabled config-store/config watch plumbing from bootstrap, runtime wiring, API state, and storage exports, inlined config section extraction into `pa-monitor`, and dropped the now-unused `pa-storage` dependency from `pa-monitor`.
- Why: Finish the transition to read-only `TOML + environment variable` configuration and eliminate dead persistence scaffolding that could mislead future maintenance.

### 2026-03-13
- Area: `crates/pa-monitor/src/api.rs`, `crates/pa-monitor/src/lib.rs`, `crates/pa-monitor/Cargo.toml`, `crates/pa-storage/Cargo.toml`
- Change: Removed the dead `alerts` and legacy `health` modules from `pa-monitor`, inlined the shared `HealthCheck` alias into the active API module, and pruned unused crate dependencies from `pa-monitor` and `pa-storage`.
- Why: Reduce dead code and dependency surface after the monitoring stack converged on the unified Axum API server and storage config persistence was removed.

### 2026-03-13
- Area: `src/app/types.rs`, `src/app/helpers.rs`, `src/app/liquidity_rewards.rs`, `Cargo.toml`
- Change: Inlined LR-only type aliases back into the liquidity-rewards module and removed unused root-crate dependencies on `config` and `sqlx`.
- Why: Reduce cross-module indirection in `src/app` and shrink the dependency surface of the main binary crate without changing runtime behavior.

### 2026-03-13
- Area: `src/bin/weather_audit.rs`
- Change: Switched the audit CLI to the explicit `parse_target_date_server_local()` helper after weather date parsing APIs were tightened.
- Why: Restore full workspace compilation for auxiliary weather audit tooling without weakening production weather date semantics.

### 2026-03-13
- Area: `crates/pa-monitor/src/api.rs`, `crates/pa-strategy/src/weather.rs`
- Change: Removed the inert config history API route and fully eliminated the public server-local `parse_target_date()` entrypoint in favor of explicit production/test call sites.
- Why: Remove no-op surface area and reduce the chance of future regressions back to server-local date semantics in weather logic.

### 2026-03-13
- Area: `crates/pa-strategy/src/weather.rs`, `crates/pa-strategy/src/crypto_alpha.rs`
- Change: Added an explicit `parse_target_date_server_local()` helper and switched crypto strategy parsing to use it instead of depending on the weather test-oriented wrapper.
- Why: Fix the compile regression introduced after tightening weather date parsing semantics without reintroducing server-local date handling into weather production paths.

### 2026-03-13
- Area: `crates/pa-strategy/src/weather.rs`, `frontend/src/api.ts`, `frontend/src/components/HistoryModal.tsx`
- Change: Restricted the server-local `parse_target_date()` wrapper to tests and removed dead frontend config-mutation/history client code.
- Why: Avoid future production regressions to server-local date parsing and remove stale configuration-editing code after the UI/API became read-only.

### 2026-03-13
- Area: `crates/pa-strategy/src/weather.rs`
- Change: Switched remaining weather `days_to_event`, surround gating, and relative date parsing paths from server-local time to market-local dates.
- Why: Keep weather forecasting, stale-liquidity, and international-city date handling consistent with market-local settlement boundaries.

### 2026-03-13
- Area: `crates/pa-monitor/src/api.rs`
- Change: Removed the writable `/api/config/{section}` update route so the config API is now read-only.
- Why: Runtime configuration should be controlled only by TOML and environment variables, not by ad-hoc API mutation.

### 2026-03-13
- Area: `frontend/src/components/ConfigSection.tsx`, `frontend/src/pages/Configuration.tsx`
- Change: Converted the configuration UI to read-only display mode and removed frontend save/history actions.
- Why: Configuration should now be managed via TOML and environment variables only, not edited or persisted from the web UI.

### 2026-03-13
- Area: `src/app/bootstrap.rs`
- Change: Disabled database-backed config-store loading and persistence so runtime config changes are now in-memory only.
- Why: Configuration should no longer be persisted or restored from PostgreSQL overrides.

### 2026-03-13
- Area: `config/default.toml`, `crates/pa-core/src/config.rs`
- Change: Relaxed weather trading defaults to improve order generation by lowering `min_edge_bps` to `500`, widening `max_spread_bps` to `1700`, raising `max_entry_price` to `0.35`, and lowering `profit_take_threshold` to `0.34`.
- Why: The live weather strategy was consistently filtered out by spread and edge thresholds and needed a moderately less conservative default profile.

### 2026-03-13
- Area: `AGENTS.md`
- Change: Added repository overview, module layout, runtime/config notes, common commands, and maintenance expectations.
- Why: Make the file useful as an operational and engineering guide instead of only a change ledger.

### 2026-03-13
- Area: `crates/pa-core/src/weather.rs`, `crates/pa-strategy/src/weather.rs`
- Change: Added shared weather location timezones to `WeatherLocation`, introduced shared `weather_timezone(location)`, and removed the duplicate strategy-local timezone mapping from `market_local_today()`.
- Why: Prevent future drift between shared city metadata and stale-liquidity local-day logic.
