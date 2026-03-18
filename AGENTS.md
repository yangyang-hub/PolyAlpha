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

### 2026-03-18
- Area: `crates/pa-strategy/src/engine.rs`, `crates/pa-strategy/src/weather.rs`
- Change: Added a shared weather-event-key helper for opportunity questions and taught the strategy engine to process weather entry candidates per event in profit order, falling back to the next candidate when the current best candidate fails pre-execution validation (depth/freshness/budget) instead of dropping the whole event for that scan; added focused engine regression coverage for the fallback path.
- Why: Same-event weather dedupe was correctly avoiding duplicate entries, but a single thin-book or stale best bin could still suppress a tradable second-choice bin in the same scan, which was wasting valid weather opportunities.

### 2026-03-17
- Area: `crates/pa-strategy/src/weather.rs`
- Change: Restricted new weather buy scanning to the `UTC+8 00:00-08:00` window by gating binary, NegRisk, stale-liquidity, and surround entry generation behind a shared time-window check while leaving exit scanning active at all times, and added focused regression coverage for the UTC-to-UTC+8 hour mapping.
- Why: Recent live observations showed the highest weather-market win rate during the UTC+8 midnight-to-morning session, so new weather entries should be concentrated into that operator-selected time window without delaying risk-reducing exits.

### 2026-03-17
- Area: `frontend/src/pages/WeatherStrategy.tsx`, `frontend/src/components/ConfigSection.tsx`
- Change: Added explicit frontend copy showing that new weather buys are only generated during the `UTC+8 00:00-08:00` window, both on the weather strategy page and in the read-only weather config hints.
- Why: Once the entry window became strategy behavior rather than just an implementation detail, operators need the UI to explain why new weather opportunities disappear outside that session instead of inferring it from logs.

### 2026-03-17
- Area: `crates/pa-monitor/src/api.rs`, `frontend/src/api.ts`, `frontend/src/pages/WeatherStrategy.tsx`
- Change: Added a runtime `weather_entry_window_open` field to `/api/status` and surfaced it in the weather page's run-context card so operators can see whether the current moment is inside the `UTC+8 00:00-08:00` weather entry window.
- Why: Static UI copy explains the configured trading window, but operators also need live status to distinguish “no opportunities” from “entry window currently closed”.

### 2026-03-17
- Area: `crates/pa-core/src/weather.rs`, `crates/pa-strategy/src/weather.rs`, `crates/pa-monitor/src/api.rs`
- Change: Moved the `UTC+8 00:00-08:00` weather entry-window rule into shared weather metadata helpers and switched both the strategy layer and `/api/status` to reuse that single implementation, with the canonical regression test living in `pa-core`.
- Why: The strategy and monitoring API had drift-prone duplicate copies of the same time-window rule, so the weather entry session should be defined once and consumed everywhere.

### 2026-03-16
- Area: `yangyang_resume.md`
- Change: Replaced the resume's `StoryChain` project section with a `PolyAlpha` project entry focused on Rust-based quantitative trading, strategy/risk infrastructure, Polymarket integration, and monitoring/observability.
- Why: The user wanted this repository experience reflected in the resume instead of the previous Web3 storytelling project, so the resume now better matches the work done in this codebase.

### 2026-03-16
- Area: `crates/pa-strategy/src/weather.rs`
- Change: Added a conservative city overlay for weather entries so `Chicago` and all `DefaultProtected` cities now require an extra `+100bps` edge, cap entry price at `0.30`, and size at `75%` of the normal per-trade USDC limit across both main and stale-liquidity entry paths, with focused regression coverage for validated vs protected cities.
- Why: Recent live weather trades showed better consistency in cities like Atlanta/Miami than in Chicago and still-unvalidated cities, so the strategy should lean harder into validated locations and automatically be more selective where settlement/behavior confidence is weaker.

### 2026-03-16
- Area: `crates/pa-strategy/src/weather.rs`
- Change: Added short-dated weather entry overlays keyed off each market's `end_date`, so positions resolving within `24h` or `12h` now require higher edge, lower entry-price ceilings, and smaller per-trade size caps on the normal binary and NegRisk entry paths, with focused regression coverage for validated and conservative cities.
- Why: Recent weather churn was concentrated in near-resolution bins, so entry logic should get materially stricter as the resolution window approaches instead of treating same-day markets like longer-dated setups.

### 2026-03-16
- Area: `crates/pa-strategy/src/weather.rs`
- Change: Reworked same-event weather dedupe to rank binary and NegRisk opportunities together by `location + metric + target_date`, keeping only the single highest-`estimated_profit` normal-entry candidate per event per scan and removing the earlier persistent 30-minute strategy-local cooldown.
- Why: A strategy-local cooldown was occupying event slots before execution success was known and also let binary scans win by ordering rather than expected value, so same-event throttling should stay scan-local until there is an execution-aware hook.

### 2026-03-16
- Area: `config/default.toml`, `README.md`
- Change: Moved the crypto horizon/exit/edge-decay defaults back to the top-level `[crypto_alpha]` section instead of leaving them underneath the last `[[crypto_alpha.calibration_overrides]]` sample row, and corrected the README example to match.
- Why: TOML table-array scoping had caused those fields to deserialize as part of the final override row rather than as live strategy defaults, so the documented crypto risk profile was not actually the one loaded at runtime.

### 2026-03-16
- Area: `src/bin/crypto_calibrate.rs`, `docs/crypto-calibration-workflow.md`, `README.md`
- Change: Added an offline `crypto_calibrate` CLI that reads historical JSONL observations, infers `asset + horizon + market_type`, fits `probability_calibration` overrides for each sufficiently sampled segment, and prints TOML-ready `crypto_alpha.calibration_overrides` blocks with Brier-score comments.
- Why: The crypto strategy already supports table-driven calibration overrides, so the next missing piece was a practical workflow for turning labeled historical samples into operator-usable override drafts instead of hand-tuning every segment.

### 2026-03-16
- Area: `frontend/src/components/ConfigSection.tsx`
- Change: Grouped the crypto calibration override table by `market_type`, rendering separate `binary` / `range` / wildcard sections while still keeping specificity ordering within each section.
- Why: Once override rows became more numerous, operators needed a faster way to scan one market family at a time instead of mentally filtering a single mixed table.

### 2026-03-16
- Area: `frontend/src/components/ConfigSection.tsx`
- Change: Sorted the crypto calibration override table by selector specificity so exact asset/horizon/type rows render before broader wildcard rules, with stable lexical fallback ordering for ties.
- Why: Operators should see the most targeted calibration rules first instead of scanning past catch-all entries that are less likely to explain a specific strategy behavior.

### 2026-03-16
- Area: `frontend/src/components/ConfigSection.tsx`
- Change: Added badge-style scope rendering for `crypto_alpha.calibration_overrides` table cells so wildcard selectors such as `*` and `any` stand out visually from exact asset/horizon/type matches.
- Why: Once override rows are shown as a table, operators still need to quickly distinguish broad catch-all rules from specific targeted rules without reading every cell character-by-character.

### 2026-03-16
- Area: `frontend/src/components/ConfigSection.tsx`
- Change: Added a dedicated read-only table renderer for `crypto_alpha.calibration_overrides`, showing `asset / horizon / market_type / probability / sigma / size` columns with explicit empty-state placeholders instead of falling back to raw JSON.
- Why: Calibration overrides now drive multiple layers of crypto behavior, so operators need a compact human-readable view rather than an opaque JSON blob in the configuration page.

### 2026-03-16
- Area: `crates/pa-strategy/src/crypto_alpha.rs`, `config/default.toml`, `README.md`
- Change: Extended the new crypto calibration override table so `sigma_multiplier` and `size_multiplier` are now actively read by the strategy, updated grouped-binary sigma handling to use each market's own horizon when applying override-aware multipliers, and expanded focused regression coverage for override sigma/size lookups.
- Why: A table-driven calibration layer should control more than probability shrinkage, and grouped binary markets should not apply a fixed 30-day override context when a more precise per-market horizon is available.

### 2026-03-16
- Area: `crates/pa-core/src/config.rs`, `config/default.toml`, `crates/pa-strategy/src/crypto_alpha.rs`, `frontend/src/components/ConfigSection.tsx`, `README.md`
- Change: Added table-driven `crypto_alpha.calibration_overrides` entries keyed by `asset + horizon + market_type`, taught the strategy to prefer override probability calibration factors before falling back to the static defaults, and documented sample override rows plus focused regression coverage.
- Why: The growing set of static crypto calibration fields was becoming too rigid, so operators need an explicit override table that can target specific model segments without adding another top-level config field each time.

### 2026-03-16
- Area: `crates/pa-core/src/config.rs`, `config/default.toml`, `crates/pa-strategy/src/crypto_alpha.rs`, `frontend/src/components/ConfigSection.tsx`, `README.md`
- Change: Split crypto probability calibration by market type with separate `binary` and `range` shrink factors, and wired the new dimension through both entry and exit probability calibration so NegRisk/range markets can be calibrated more conservatively than plain binary markets.
- Why: Binary and range markets have materially different probability-shape errors, so they should not share one identical post-model shrinkage factor.

### 2026-03-16
- Area: `crates/pa-core/src/config.rs`, `config/default.toml`, `crates/pa-strategy/src/crypto_alpha.rs`, `frontend/src/components/ConfigSection.tsx`, `README.md`
- Change: Added lightweight crypto probability calibration factors for `BTC / ETH / alt` assets and `short / medium` horizon buckets, then applied the combined shrinkage to GBM probabilities on both entry and exit paths so extreme model probabilities are pulled back toward 50% before edge and sizing decisions.
- Why: The baseline GBM model was still too confident in tails, especially on shorter-dated and non-major crypto markets, so a conservative post-model calibration layer is a pragmatic first step before any larger distribution rewrite.

### 2026-03-16
- Area: `crates/pa-core/src/config.rs`, `config/default.toml`, `crates/pa-strategy/src/crypto_alpha.rs`, `frontend/src/components/ConfigSection.tsx`, `README.md`
- Change: Added horizon-based crypto entry size multipliers and wired short/medium expiry buckets directly into Kelly sizing so near-dated markets now open smaller positions even after passing the tighter threshold stack, with focused regression coverage for the horizon sizing helper.
- Why: Expiry buckets were already making short-dated crypto markets stricter on entry and faster on exit, so position sizing should also be structurally lighter for the same short-term jump risk.

### 2026-03-16
- Area: `crates/pa-core/src/config.rs`, `config/default.toml`, `crates/pa-strategy/src/crypto_alpha.rs`, `frontend/src/components/ConfigSection.tsx`, `README.md`
- Change: Added impact-tiered crypto event size multipliers and wired matched `low/medium/high` events into entry sizing so event windows now shrink Kelly-based position sizes in addition to tightening thresholds and inflating sigma, with focused regression coverage for the sizing helper.
- Why: Event risk should affect not only whether a crypto market passes filters but also how much capital is put at risk once it does pass.

### 2026-03-16
- Area: `crates/pa-core/src/config.rs`, `config/default.toml`, `crates/pa-strategy/src/crypto_alpha.rs`, `frontend/src/components/ConfigSection.tsx`, `README.md`
- Change: Added impact-tiered crypto event sigma multipliers and wired matched `low/medium/high` calendar events into the effective GBM volatility used by both crypto entry evaluation and exit probability recomputation, with focused regression coverage for the sigma scaling helper.
- Why: Event windows should change the model distribution itself, not only tighten edge/spread gates, so crypto probabilities become more conservative when known macro or token events raise uncertainty.

### 2026-03-16
- Area: `crates/pa-core/src/config.rs`, `config/default.toml`, `crates/pa-strategy/src/crypto_alpha.rs`, `frontend/src/components/ConfigSection.tsx`, `README.md`
- Change: Added severity-scaled `edge_decay` confirmation-scan multipliers so moderate and severe thin-edge states can require fewer repeated confirmations before trimming, with regression coverage for the updated confirmation ladder.
- Why: Once edge decay is materially worse, holding the same confirmation count as a mild thin-edge state delays de-risking more than intended.

### 2026-03-16
- Area: `crates/pa-core/src/config.rs`, `config/default.toml`, `crates/pa-strategy/src/crypto_alpha.rs`, `frontend/src/components/ConfigSection.tsx`, `README.md`
- Change: Added severity-scaled `edge_decay` confirmation-window multipliers so moderate and severe thin-edge states can complete their repeated-confirmation sequence inside a shorter wall-clock gap, with updated regression checks for the new window ladder.
- Why: Once edge decay is materially worse, requiring the same long confirmation spacing as a mild thin-edge state slows down de-risking unnecessarily.

### 2026-03-16
- Area: `crates/pa-core/src/config.rs`, `config/default.toml`, `crates/pa-strategy/src/crypto_alpha.rs`, `frontend/src/components/ConfigSection.tsx`, `README.md`
- Change: Added severity-scaled `edge_decay` cooldown multipliers so moderate and severe thin-edge states shorten the next trim cooldown in addition to already increasing trim size, with regression coverage for the new cooldown ladder.
- Why: When model edge has already decayed materially, waiting the full normal cooldown before allowing another trim is too slow, so repeated de-risking should accelerate alongside severity.

### 2026-03-16
- Area: `crates/pa-core/src/config.rs`, `config/default.toml`, `crates/pa-strategy/src/crypto_alpha.rs`, `frontend/src/components/ConfigSection.tsx`, `README.md`
- Change: Added moderate/severe `edge_decay` gap bands with configurable gap thresholds and trim multipliers, and made crypto partial exits scale not only with repeated confirmations but also with how far the held-side model edge has decayed below the keep-holding threshold.
- Why: Thin-edge states are not all equally dangerous, so materially worse edge decay should de-risk faster even before many more confirmation windows accumulate.

### 2026-03-16
- Area: `crates/pa-core/src/config.rs`, `config/default.toml`, `crates/pa-strategy/src/crypto_alpha.rs`, `frontend/src/components/ConfigSection.tsx`, `README.md`
- Change: Added an `edge_decay` confirmation time window plus horizon-scaled window multipliers, changed crypto confirmation state from a bare scan counter to `(count, last_seen)` tracking, and reset progressive trims when thin-edge confirmations are too far apart in wall-clock time.
- Why: Pure scan-count confirmation was sensitive to runtime scan frequency, so `edge_decay` needed a time-bounded sequence to keep trim behavior stable across slower or faster polling loops.

### 2026-03-16
- Area: `src/app/account_runtime.rs`, `crates/pa-strategy/src/crypto_alpha.rs`, `frontend/src/components/ConfigSection.tsx`, `README.md`
- Change: Threaded the shared event calendar into `crypto_alpha` and made crypto entry filtering event-aware so active matched events now raise the effective `min_edge_bps` and tighten the effective `max_spread_bps`, with focused regression coverage for event-window entry rejection.
- Why: Engine-level event scaling only reduced size after an opportunity was already generated, so crypto markets still entered too easily around token or macro event windows instead of requiring better edge and cleaner books up front.

### 2026-03-16
- Area: `crates/pa-core/src/config.rs`, `crates/pa-market-data/src/event_calendar.rs`, `crates/pa-strategy/src/crypto_alpha.rs`, `config/default.toml`, `frontend/src/components/ConfigSection.tsx`, `README.md`
- Change: Split crypto event-window entry controls into low/medium/high impact edge and spread multipliers, added event-calendar impact lookup support, kept the old single crypto event keys as medium-impact aliases, and added regression coverage for impact-tier threshold scaling.
- Why: A single crypto event tightening profile was too coarse once event-aware entry filtering was live, so the strategy now needs materially stricter thresholds for high-impact events without over-penalizing lower-impact crypto windows.

### 2026-03-16
- Area: `crates/pa-core/src/config.rs`, `crates/pa-strategy/src/crypto_alpha.rs`, `config/default.toml`, `frontend/src/components/ConfigSection.tsx`, `README.md`
- Change: Added short/medium horizon entry buckets to `crypto_alpha`, so near-expiry markets now automatically require higher edge and tighter spreads with configurable day cutoffs and multiplier defaults, plus regression coverage for horizon-tier threshold scaling.
- Why: Crypto markets close to resolution behave materially differently from longer-dated contracts, so entry filtering should become stricter as expiry approaches instead of sharing one static edge/spread profile.

### 2026-03-16
- Area: `crates/pa-core/src/config.rs`, `crates/pa-strategy/src/crypto_alpha.rs`, `config/default.toml`, `frontend/src/components/ConfigSection.tsx`, `README.md`
- Change: Added short/medium horizon exit buckets to `crypto_alpha`, lowering capital-efficiency thresholds and shrinking model-reversal exit buffers for near-expiry positions, with focused regression tests for earlier short-dated exits.
- Why: Entry filtering alone was not enough for near-expiry crypto contracts, so held positions now also unwind more aggressively as resolution approaches instead of waiting on the same long-dated exit settings.

### 2026-03-16
- Area: `crates/pa-core/src/config.rs`, `crates/pa-strategy/src/crypto_alpha.rs`, `config/default.toml`, `frontend/src/components/ConfigSection.tsx`, `README.md`
- Change: Added a configurable crypto `edge_decay` exit with short/medium horizon hold-edge multipliers and partial-exit sizing, so positions now trim only part of the size when residual model edge over the bid becomes too thin, with regression coverage for both base and short-dated edge decay behavior.
- Why: Near-fair-value crypto positions were lingering until full reversal or stop-loss, but immediately fully exiting on small edge decay would be too blunt, so the strategy now frees capital progressively as remaining edge deteriorates.

### 2026-03-16
- Area: `crates/pa-core/src/config.rs`, `crates/pa-strategy/src/crypto_alpha.rs`, `config/default.toml`, `frontend/src/components/ConfigSection.tsx`, `README.md`
- Change: Added token-level `edge_decay` cooldown state and `edge_decay_cooldown_secs` config so repeated thin-edge scans no longer fire partial exit orders every cycle on the same crypto position, with focused regression coverage for cooldown suppression.
- Why: Once edge-decay became a partial-exit path, the strategy needed a local debounce layer to avoid repeatedly trimming the same token in a tight loop while conditions remain only marginally changed.

### 2026-03-16
- Area: `crates/pa-core/src/config.rs`, `crates/pa-strategy/src/crypto_alpha.rs`, `config/default.toml`, `frontend/src/components/ConfigSection.tsx`, `README.md`
- Change: Made crypto `edge_decay` cooldown horizon-aware by adding short/medium horizon cooldown multipliers, so near-expiry positions re-arm for further trims sooner than long-dated ones, with regression coverage for cooldown scaling.
- Why: A single 30-minute debounce was too blunt once edge-decay became the main progressive-exit path, because short-dated markets need to keep trimming faster as expiry approaches.

### 2026-03-16
- Area: `crates/pa-core/src/config.rs`, `crates/pa-strategy/src/crypto_alpha.rs`, `config/default.toml`, `frontend/src/components/ConfigSection.tsx`, `README.md`
- Change: Added `edge_decay_confirmation_scans` and token-level consecutive-confirmation tracking so crypto edge-decay exits now require repeated thin-edge scans before trimming, with updated regression coverage for first-scan confirmation vs second-scan execution.
- Why: Even with cooldowns, one noisy scan could still trigger an unnecessary trim, so edge-decay now waits for repeated confirmation before acting on marginal residual-edge deterioration.

### 2026-03-16
- Area: `crates/pa-core/src/config.rs`, `crates/pa-strategy/src/crypto_alpha.rs`, `config/default.toml`, `frontend/src/components/ConfigSection.tsx`, `README.md`
- Change: Made crypto `edge_decay` confirmation counts horizon-aware by adding short/medium confirmation overrides, so near-expiry positions can trim after fewer repeated confirmations than long-dated ones, with regression coverage for confirmation scaling.
- Why: Once edge-decay confirmation existed, using the same count for all maturities was still too blunt because short-dated contracts should react faster to sustained thin-edge conditions than long-dated ones.

### 2026-03-16
- Area: `crates/pa-core/src/config.rs`, `crates/pa-strategy/src/crypto_alpha.rs`, `config/default.toml`, `frontend/src/components/ConfigSection.tsx`, `README.md`
- Change: Changed crypto `edge_decay` sizing from a fixed trim fraction to a progressive schedule driven by consecutive confirmations, adding `edge_decay_exit_fraction_step` so repeated confirmed thin-edge states now cut larger portions of the remaining position.
- Why: Once edge-decay required confirmation, the next useful improvement was making later trims more decisive than the first one, so the strategy can scale out progressively instead of repeating the same small reduction every time.

### 2026-03-16
- Area: `crates/pa-core/src/config.rs`, `config/default.toml`, `crates/pa-strategy/src/crypto_alpha.rs`, `frontend/src/components/ConfigSection.tsx`, `README.md`
- Change: Refactored the crypto strategy to split spot/history/IV refresh intervals, added a configurable relative stop-loss and per-asset exposure cap, updated crypto entry sizing to enforce aggregate asset limits across related markets, and documented the new config fields in the monitor UI and README.
- Why: The previous crypto path used one coarse cache TTL, a hardcoded 50% loss cut, and only token-level sizing, which made fast markets stale and allowed correlated BTC/ETH exposure to stack across multiple contracts without an asset-level cap.

### 2026-03-16
- Area: `crates/pa-monitor/src/metrics.rs`, `crates/pa-strategy/src/crypto_alpha.rs`
- Change: Added dedicated crypto strategy Prometheus metrics for cache hit/refresh events, rejection reasons, per-asset aggregate exposure, and exit reasons, and wired the strategy to emit those metrics during scans and exit checks.
- Why: The crypto refactor added asset-level sizing and split cache refresh paths, so operators need direct observability into whether the strategy is being limited by stale data, spread/edge filters, or per-asset exposure caps.

### 2026-03-16
- Area: `crates/pa-monitor/src/api.rs`, `src/app/helpers.rs`, `src/app/tasks.rs`, `frontend/src/api.ts`, `frontend/src/pages/CryptoMarkets.tsx`
- Change: Added an `asset` field to API position snapshots by inferring crypto exposure from market questions/event titles, and updated the crypto frontend page to show the live crypto config parameters plus per-asset aggregated exposure and per-position asset labels.
- Why: Operators wanted the crypto refactor to be inspectable directly in the frontend without depending on Prometheus, so the monitoring UI now surfaces the new refresh/stop-loss/exposure behavior in the crypto page itself.

### 2026-03-16
- Area: `frontend/src/pages/CryptoMarkets.tsx`
- Change: Removed the crypto page's dependency on `/metrics` and switched the page summary cards to use only config/status APIs plus live position snapshots, including derived position market value and runtime context display.
- Why: The crypto workflow should be inspectable directly from frontend/API data without requiring Prometheus scraping or metrics parsing just to understand the current strategy state.

### 2026-03-16
- Area: `frontend/src/pages/CryptoMarkets.tsx`
- Change: Turned the crypto page's per-asset exposure table into an expandable grouped view so each asset row can reveal the underlying positions with direction, size, cost basis, and unrealized PnL inline.
- Why: Asset-level aggregation alone was too coarse to debug stacked BTC/ETH exposure, so operators need one-click drilldown from asset totals to the exact positions contributing to that exposure.

### 2026-03-16
- Area: `frontend/src/pages/CryptoMarkets.tsx`
- Change: Added a plain-language risk explanation block under the crypto strategy parameter card, summarizing how the current refresh cadence, edge/spread gates, per-asset exposure cap, and relative stop-loss/model-reversal exits affect live behavior.
- Why: The crypto config fields are now richer after the refactor, so operators need the UI to explain the effective trading behavior directly instead of mentally translating raw numeric parameters.

### 2026-03-16
- Area: `frontend/src/pages/CryptoMarkets.tsx`
- Change: Added per-position exposure share labels inside each expanded asset group, showing how much of that asset bucket and of the overall crypto strategy cost basis each position represents.
- Why: Once the asset table became expandable, operators still needed a quick way to spot which single positions dominate the BTC/ETH exposure instead of manually comparing raw cost figures.

### 2026-03-16
- Area: `frontend/src/pages/CryptoMarkets.tsx`
- Change: Added front-end concentration highlighting to the asset aggregation table, including per-asset strategy-share percentages plus `高集中` / `中集中` badges and row tinting when one asset dominates the crypto strategy cost basis.
- Why: The crypto page needed a faster visual warning when strategy exposure becomes too concentrated in one asset, even without relying on Prometheus or extra backend state.

### 2026-03-16
- Area: `frontend/src/pages/CryptoMarkets.tsx`
- Change: Added an explicit UI note clarifying that the asset-table concentration percentages are computed against current crypto-strategy cost basis only, not against total account assets.
- Why: Once the page started surfacing concentration badges and percentages, operators needed a clear statement of scope to avoid misreading strategy-level exposure concentration as whole-portfolio concentration.

### 2026-03-16
- Area: `frontend/src/pages/CryptoMarkets.tsx`
- Change: Tidied the crypto page layout by removing duplicated snapshot-time display, shortening helper copy, and changing the summary stat grid to a 5-card-friendly responsive layout.
- Why: After several frontend additions, the crypto page had started repeating context and wrapping awkwardly, so the final layout needed a small cleanup pass to stay easy to scan.

### 2026-03-16
- Area: `crates/pa-strategy/src/crypto_alpha.rs`
- Change: Changed crypto scan selection so each asset keeps only its single best entry candidate per scan across grouped binary, standalone binary, and NegRisk paths, using estimated profit first and edge/size as tie-breakers, with regression coverage.
- Why: The crypto strategy could previously surface multiple simultaneous BTC/ETH entries in one scan, which stacked correlated exposure and forced the execution layer to arbitrate redundant candidates instead of the strategy picking the highest-value one upfront.

### 2026-03-16
- Area: `crates/pa-strategy/src/crypto_alpha.rs`
- Change: Refined the per-asset entry ranking to compare net profit first, then profit-per-cost efficiency, then lower capital usage, with spread and size only as later tie-breakers, and added focused tests for the ranking helper.
- Why: Absolute profit alone can prefer a bulkier but less capital-efficient candidate, so the dedupe step should favor entries that deliver the same expected value with better capital efficiency and less additional asset concentration.

### 2026-03-16
- Area: `crates/pa-strategy/src/crypto_alpha.rs`
- Change: Extended the per-asset crypto entry ranking to prefer candidates with larger executable depth buffer at the quoted limit price before falling back to capital usage, and added regression coverage for equal-profit/equal-efficiency but shallower books.
- Why: Two BTC/ETH opportunities can look equally good on model PnL while one sits on much thinner liquidity, so same-asset dedupe should prefer the candidate with more order-book headroom instead of the one that is more likely to fail freshness/depth validation.

### 2026-03-16
- Area: `crates/pa-strategy/src/crypto_alpha.rs`
- Change: Upgraded crypto candidate dedupe from `asset` scope to `asset + direction bucket` scope by deriving token-level up/down/range buckets from binary and NegRisk questions, so opposite-direction candidates can coexist while same-direction duplicates are still collapsed to the best one.
- Why: A pure asset-only dedupe is too blunt once the strategy is ranking opportunities more carefully, because it suppresses legitimate opposite-direction setups on the same asset instead of only removing redundant same-direction candidates.

### 2026-03-16
- Area: `crates/pa-core/src/config.rs`, `config/default.toml`, `crates/pa-strategy/src/crypto_alpha.rs`, `frontend/src/components/ConfigSection.tsx`, `README.md`
- Change: Added a separate `max_exposure_per_asset_direction_pct` crypto config, wired strategy sizing to enforce both asset-level and asset-direction-level exposure caps using the same direction buckets as candidate dedupe, and added regression coverage for same-direction cap blocking.
- Why: Even after same-asset candidates were deduped more intelligently, repeated scans could still accumulate too much BTC-up or ETH-down exposure over time, so the crypto strategy needed a first-class directional exposure ceiling rather than relying only on total asset caps.

### 2026-03-16
- Area: `crates/pa-strategy/src/crypto_alpha.rs`
- Change: Refined the NegRisk direction buckets from the coarse `Range/Other` pair to explicit `InsideRange/OutsideRange` buckets while keeping `Up/Down` for one-sided thresholds, so range outcomes now reuse clearer semantics across dedupe and directional exposure caps.
- Why: The original `Other` bucket was too ambiguous once directional risk limits were introduced, and range markets needed a more precise inside-vs-outside split to avoid mixing unrelated NegRisk exposures under one fallback label.

### 2026-03-16
- Area: `crates/pa-monitor/src/api.rs`, `src/app/helpers.rs`, `src/app/tasks.rs`, `frontend/src/api.ts`, `frontend/src/pages/CryptoMarkets.tsx`
- Change: Added per-position crypto `direction` labels to the monitoring snapshot pipeline and updated the crypto frontend page to aggregate exposure by `asset + direction`, display the direction bucket inline, and surface the new single-direction exposure cap in the config summary/risk explanation.
- Why: The crypto strategy now reasons and limits risk at the `asset + direction` level, so the frontend needed the same dimension to show whether live exposure is concentrated in `BTC-Up`, `BTC-Down`, `InsideRange`, or `OutsideRange` instead of flattening everything into one asset total.

### 2026-03-16
- Area: `crates/pa-monitor/src/api.rs`, `src/app/bootstrap.rs`, `src/app/market_runtime.rs`, `src/app/tasks.rs`, `src/main.rs`, `frontend/src/api.ts`, `frontend/src/pages/CryptoMarkets.tsx`
- Change: Added a non-Prometheus `wallet_balance` field to `/api/status`, kept it refreshed alongside shared position snapshots, and used it in the crypto frontend page to highlight `asset + direction` rows that are at or near the configured single-direction exposure cap.
- Why: Directional exposure limits are defined as a fraction of wallet balance, so the frontend needed the same live balance denominator to warn accurately when `BTC-Up` or similar buckets are approaching their configured cap without depending on metrics scraping.

### 2026-03-16
- Area: `frontend/src/pages/CryptoMarkets.tsx`
- Change: Changed the `asset + direction` exposure table to sort primarily by proximity to the configured single-direction cap, with cost basis and PnL only as secondary tie-breakers, and updated the UI copy to explain that ordering.
- Why: Once direction-cap highlighting was added, the most useful default view is to surface the buckets closest to their limit first instead of forcing operators to scan manually through lower-risk rows.

### 2026-03-16
- Area: `frontend/src/pages/CryptoMarkets.tsx`
- Change: Added an explicit `已用/上限` numeric readout to each `asset + direction` exposure row, showing the current cost basis against the configured single-direction dollar cap derived from live wallet balance.
- Why: Color and badge cues were useful but still approximate, so operators needed the exact current-usage-vs-limit numbers inline to judge how close a direction bucket really is to its cap.

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
