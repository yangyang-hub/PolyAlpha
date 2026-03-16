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

### 2026-03-16
- Area: `crates/pa-core/src/config.rs`, `src/app/bootstrap.rs`
- Change: Moved the database configuration log to after environment override reapplication and added a direct `PA_DATABASE__URL` backfill onto `settings.database.url`.
- Why: Local and container runs were logging `No database URL configured` even when `PA_DATABASE__URL` was present, so startup should apply env overrides before logging and should not rely only on nested `config` env deserialization for the database URL.

### 2026-03-16
- Area: `docs/weather-settlement-validation-plan.md`, `docs/weather-noaa-settlement-checklist.md`
- Change: Added an explicit priority order and a fast follow-up checklist for the remaining Batch 1 default-protected cities: Philadelphia, San Francisco, Las Vegas, Austin, and Minneapolis.
- Why: Those cities are still blocked mainly by primary-source rules-page availability, so the validation workflow should have a fixed execution order and a minimal repeatable checklist ready for when any of them reappears in active markets.

### 2026-03-16
- Area: `crates/pa-core/src/weather.rs`, `crates/pa-strategy/src/weather.rs`
- Change: Added a shared observation-site hint helper and pinned London's Met Office historical actual path to the fixed audit geohash `gcptq8` instead of scanning the nearest five candidates at runtime.
- Why: London replay and settlement-audit actuals should stay aligned with one consistent chosen observation site rather than drifting to whichever nearby geohash happens to expose temperature data first.

### 2026-03-16
- Area: `docs/weather-noaa-settlement-checklist.md`, `docs/weather-settlement-validation-plan.md`, `docs/international-weather-expansion-plan.md`
- Change: Updated the international weather audit documentation so London now reflects `MetOffice` forecast plus Met Office Land Observations actuals and PostgreSQL snapshot archive, while Seoul now reflects `Kma` forecast plus KMA monthly actuals and PostgreSQL snapshot archive.
- Why: The docs still described both cities as Open-Meteo audit paths, which no longer matched the live implementation or replay workflow.

### 2026-03-16
- Area: `crates/pa-core/src/weather.rs`, `crates/pa-core/src/config.rs`, `config/default.toml`, `.env.example`, `crates/pa-strategy/src/weather.rs`, `src/app/tasks.rs`, `src/bin/weather_replay.rs`, `crates/pa-monitor/src/api.rs`, `frontend/src/api.ts`, `frontend/src/components/ConfigSection.tsx`
- Change: Added a `MetOffice` weather provider plus `met_office_api_key` config, switched London from `OpenMeteo` to `MetOffice`, implemented audit-only Met Office daily temperature live-forecast routing, and wired the provider through replay output, PostgreSQL forecast snapshots, monitor metadata, and frontend config labels.
- Why: London needs a more trustworthy official UK forecast source before any future trading enablement, and Met Office Weather DataHub is the right upstream provider for that audit path.

### 2026-03-16
- Area: `crates/pa-core/src/config.rs`, `config/default.toml`, `.env.example`, `crates/pa-strategy/src/weather.rs`, `src/app/tasks.rs`, `src/bin/weather_replay.rs`, `frontend/src/components/ConfigSection.tsx`, `README.md`, `CLAUDE.md`
- Change: Added a separate `met_office_obs_api_key` config, split Met Office forecast and land-observations credentials, and wired London's Met Office actuals path to use the documented `nearest -> geohash -> observations` flow for temperature replay while keeping forecast snapshots on the site-specific key.
- Why: Met Office forecast and land observations are separate subscribed products, so London audit replay needs distinct credentials and an explicit observations path instead of assuming one key or one endpoint shape covers both.

### 2026-03-16
- Area: `crates/pa-strategy/src/weather.rs`
- Change: Updated the London Met Office observations path to request up to five nearest land-observation geohashes and automatically use the first candidate that returns real temperature observations for the target day instead of blindly trusting the first geohash.
- Why: The nearest geohash returned for central London (`gcpvj0`) only exposes timestamp indexes, while nearby candidates like `gcptq8` and `gcpsvg` return actual temperature observations; replay should prefer a usable observation site over a metadata-only neighbor.

### 2026-03-16
- Area: `src/bin/weather_replay.rs`
- Change: Added a `--seed-archive-if-missing` replay flag that writes the current live forecast curve into PostgreSQL weather snapshot storage when no archived forecast is available for the requested provider/location/metric/date.
- Why: Audit-only cities such as London and Seoul may not have archived forecast rows yet right after a deploy or key migration, so replay needs a manual one-shot way to backfill snapshot archive data without waiting for the periodic runtime task.

### 2026-03-16
- Area: `src/bin/weather_replay.rs`
- Change: Made manual replay seeding immediately reuse the just-written target-date value as the archive result if the follow-up database read still misses the fresh row.
- Why: Operators need London/Seoul archive-vs-actual comparisons to be usable in the same replay invocation that seeds a missing snapshot, without being blocked by read-after-write timing quirks.

### 2026-03-16
- Area: `src/bin/weather_replay.rs`
- Change: Changed replay archive loading/seeding to prefer `PA_DATABASE__URL` / `DATABASE_URL` directly before falling back to `Settings::load()`.
- Why: Manual replay runs often source `.env` directly, so archive read/write helpers should not depend solely on the higher-level settings loader when a valid database URL is already present in the process environment.

### 2026-03-16
- Area: `crates/pa-market-data/src/ws_feed.rs`
- Change: Stopped the WebSocket feed from flipping out of the "awaiting first message" state after only a 15-second no-data warning, so the 60-second stale watchdog now starts only after a real order-book message is received.
- Why: Quiet subscriptions or slow first packets were being misclassified as stale disconnected sockets, causing reconnect loops even when the underlying stream had not emitted an actual error.

### 2026-03-15
- Area: `src/app/tasks.rs`
- Change: Changed the weather forecast snapshot archiver to run one snapshot pass immediately at startup before entering the 30-minute interval loop.
- Why: Seoul/London audit replay should have archived forecast data available right after a restart instead of waiting for the first periodic tick.

### 2026-03-15
- Area: `migrations/009_create_weather_forecast_snapshots.sql`, `crates/pa-storage/src/models.rs`, `crates/pa-storage/src/repository.rs`, `src/app/tasks.rs`, `src/app/market_runtime.rs`, `src/bin/weather_replay.rs`
- Change: Added a PostgreSQL-backed `weather_forecast_snapshots` archive table plus repository read/write methods, started a shared runtime task that periodically snapshots forecast curves for audit-only weather cities into the database, and taught `weather_replay` to read the latest archived forecast target from PostgreSQL for replay output.
- Why: KMA does not currently expose a clear historical forecast archive API, so Seoul and other audit-only cities need a locally persisted forecast snapshot path to support later archive-vs-actual replay and settlement-audit workflows.

### 2026-03-15
- Area: `crates/pa-core/src/weather.rs`, `crates/pa-core/src/config.rs`, `crates/pa-strategy/src/weather.rs`, `src/bin/weather_replay.rs`, `crates/pa-monitor/src/api.rs`, `frontend/src/api.ts`, `frontend/src/components/ConfigSection.tsx`, `.env.example`, `README.md`, `CLAUDE.md`
- Change: Switched Seoul from `OpenMeteo` to a new `Kma` provider, added shared KMA grid/station metadata plus `kma_api_key` config, implemented KMA short-range forecast routing for Seoul, surfaced KMA in replay/API/frontend metadata, and documented the new audit-only Seoul key path.
- Why: Seoul needs a more trustworthy official forecast source before any future trading enablement, and KMA is the right upstream source for that audit path.

### 2026-03-15
- Area: `crates/pa-strategy/src/weather.rs`
- Change: Added a KMA TMP-series fallback for Seoul so daily max/min forecast values are synthesized from intraday temperature points when `TMX/TMN` are missing for the current day, with focused unit coverage.
- Why: KMA short-range forecast responses can omit same-day daily extrema even when intraday temperature points are available, and replay/audit output should still return a useful Seoul target value in that case.

### 2026-03-15
- Area: `crates/pa-strategy/src/weather.rs`
- Change: Wired Seoul/KMA historical actuals through `SfcMtlyInfoService/getDailyWthrData`, parsing published monthly daily station records for temperature max/min/avg while keeping unpublished months as explicit no-data errors.
- Why: Seoul replay and audit flows need a real historical actual path once the KMA monthly daily-weather API has been authorized, otherwise past-date verification stays stuck at `None`.

### 2026-03-15
- Area: `crates/pa-core/src/config.rs`, `config/default.toml`, `crates/pa-strategy/src/weather.rs`, `frontend/src/components/ConfigSection.tsx`, `README.md`, `CLAUDE.md`
- Change: Removed the fixed absolute weather profit-take threshold, added a configurable `relative_stop_loss_ratio`, switched weather exits to use capital-efficiency + relative stop-loss + model-reversal only, and updated regression tests plus operator-facing labels/docs.
- Why: Recent weather trades were churning around the absolute `profit_take_threshold`, so exits needed to depend on position-relative downside protection instead of a hardcoded price take-profit rule.

### 2026-03-15
- Area: `config/default.toml`, `crates/pa-core/src/config.rs`, `README.md`, `CLAUDE.md`
- Change: Tightened the default weather risk profile by raising `relative_stop_loss_ratio` to `0.80` and lowering `max_position_usdc` to `4.0`.
- Why: Recent weather trades showed repeated churn and avoidable drawdowns in narrow temperature bins, so the default profile should cut losing positions sooner and size them more conservatively.

### 2026-03-13
- Area: `docs/weather-settlement-validation-plan.md`
- Change: Added a phased implementation plan for promoting remaining trade-enabled weather cities from `DefaultProtected` to `Validated`, including verification criteria, rollout batches, and required metadata/documentation updates.
- Why: Turn the settlement-consistency expansion work into an executable repo-level plan instead of leaving it as ad-hoc discussion.

### 2026-03-13
- Area: `docs/weather-noaa-settlement-checklist.md`, `docs/weather-settlement-validation-plan.md`
- Change: Marked the first batch of remaining NOAA cities as in-progress investigation targets and added candidate airport-station hypotheses for Philadelphia, Austin, San Francisco, Las Vegas, and Minneapolis while keeping them unvalidated pending direct Polymarket rule confirmation.
- Why: Make the first execution batch actionable without prematurely promoting cities to validated settlement status before primary-source rule pages are confirmed.

### 2026-03-13
- Area: `docs/weather-noaa-settlement-checklist.md`
- Change: Added Phoenix to the first-batch in-progress settlement investigation set with `KPHX` recorded as the current candidate airport station hypothesis.
- Why: Keep the whole first validation batch documented consistently so direct Polymarket rule checks can proceed city-by-city without missing a planned target.

### 2026-03-13
- Area: `crates/pa-core/src/weather.rs`, `docs/weather-noaa-settlement-checklist.md`, `docs/weather-settlement-validation-plan.md`
- Change: Promoted Phoenix from `DefaultProtected` to `Validated`, added the confirmed `Phoenix Sky Harbor Intl / KPHX` settlement note, and updated the settlement checklist/plan with a direct Polymarket rules-page sample.
- Why: A primary-source market rules page confirmed the expected settlement station and whole-degree temperature resolution, so Phoenix no longer needs the default extra settlement-protection edge buffer.

### 2026-03-13
- Area: `docs/weather-settlement-validation-plan.md`
- Change: Added an execution note that public Polymarket/Gamma search does not reliably surface historical weather pages for several remaining first-batch cities, so they must stay `DefaultProtected` until a primary-source rules page is captured.
- Why: Record the current validation blocker explicitly so future settlement-review work does not mistake missing public search coverage for completed verification.

### 2026-03-13
- Area: `docs/weather-settlement-validation-plan.md`
- Change: Relaxed the settlement-validation rollout plan to allow evidence-first, out-of-batch promotion whenever a direct Polymarket rules page is available for another city.
- Why: Public discoverability of historical weather pages is inconsistent, so the validation workflow should prioritize primary-source availability over rigid batch order.

### 2026-03-13
- Area: `src/bin/weather_audit.rs`
- Change: Extended the weather audit CLI to include event slugs and direct Polymarket event URLs in both text and JSON output.
- Why: Public search coverage for historical weather pages is inconsistent, so the audit tool should surface primary-source URLs immediately whenever a target city reappears in active markets.

### 2026-03-14
- Area: `docs/weather-settlement-validation-plan.md`
- Change: Added an active-market availability snapshot noting that the current weather audit sample still only exposes already-validated NOAA cities plus audit-only London/Seoul, with no new first-batch default-protected cities surfaced on that pass.
- Why: Record why no additional settlement promotions were made despite continuing the evidence-first validation workflow.

### 2026-03-14
- Area: `src/bin/weather_audit.rs`
- Change: Added `--only-trade-enabled` and `--only-unvalidated` filters plus per-entry validation-status output so the audit CLI can act as a direct queue for settlement-validation follow-up, and added retry logic around Gamma public-search calls.
- Why: Make the audit workflow both easier to filter and more resilient to the intermittent EOF errors returned by the Gamma endpoint.

### 2026-03-14
- Area: `src/bin/weather_audit.rs`
- Change: Added filtered result counters to the audit CLI output and JSON payload so filtered runs report both full-scan totals and the actual size of the filtered follow-up queue.
- Why: Prevent confusion where the full supported-city counts remained nonzero even when the filtered settlement-validation candidate set was empty.

### 2026-03-14
- Area: `src/bin/weather_audit.rs`
- Change: Switched the audit CLI's internal unvalidated-city filtering and counting from string matching to `SettlementValidationStatus` enum matching.
- Why: Avoid brittle filtering behavior if the displayed validation-status strings ever change while keeping the CLI output human-readable.

### 2026-03-14
- Area: `src/bin/weather_audit.rs`
- Change: Added `--only-trade-enabled` and `--only-unvalidated` filters plus per-entry validation-status output so the audit CLI can act as a direct queue for settlement-validation follow-up.
- Why: Make it trivial to surface only the remaining default-protected cities when they reappear in active weather markets instead of manually filtering the full audit output.

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
- Area: `crates/pa-core/src/weather.rs`, `crates/pa-monitor/src/api.rs`, `crates/pa-strategy/src/weather.rs`
- Change: Added shared settlement validation status metadata and per-city extra edge buffers, exposed them through weather config meta, and made weather entry thresholds dynamically stricter for default-protected cities while keeping validated cities on the base threshold.
- Why: Apply a consistent settlement-mismatch protection layer to all weather cities without pretending every city has the same level of settlement validation.

### 2026-03-13
- Area: `frontend/src/api.ts`, `frontend/src/components/ConfigSection.tsx`
- Change: Exposed settlement validation status and extra edge buffers in the weather configuration UI so each city now shows whether it is validated or default-protected and how much extra edge is required.
- Why: Make the new city-level settlement protection policy visible to operators instead of hiding it only in backend metadata and strategy logic.

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
