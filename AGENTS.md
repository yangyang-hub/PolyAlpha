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

### 2026-03-21
- Area: `crates/pa-strategy/src/crypto_alpha.rs`, `crates/pa-monitor/src/api.rs`, `frontend/src/api.ts`, `frontend/src/pages/CryptoMarkets.tsx`
- Change: Refined adaptive crypto gate-scale feedback so repeated pre-sizing now weights exact-asset matches, reason-specific friction, recency, and shrink severity instead of using a flat bucket count; preserved retained-size severity on `gate_scale` diagnostics; replaced generic entry override suggestions with per-bucket single-action suggestions; and added post-entry tuning/override suggestions derived from recent crypto exits to both the status API and the CryptoMarkets page.
- Why: The remaining crypto strategy gaps were no longer missing control surfaces but overly coarse feedback loops: repeated pre-sizing still reacted too bluntly, override suggestions could remain too generic to apply safely, and holding/exit diagnostics were not yet feeding back into actionable post-entry parameter guidance.

### 2026-03-21
- Area: `crates/pa-monitor/src/api.rs`, `crates/pa-risk/src/manager.rs`, `src/app/bootstrap.rs`, `src/app/market_runtime.rs`, `src/app/tasks.rs`, `src/main.rs`, `frontend/src/api.ts`, `frontend/src/pages/WeatherStrategy.tsx`, `frontend/src/pages/CryptoMarkets.tsx`, `frontend/src/pages/SmartMoney.tsx`
- Change: Added `strategy_financials` to the runtime status API by aggregating wallet cash, marked-to-market position value, and process-lifetime realized PnL per strategy from strategy-assigned accounts, switched the weather/crypto/smart-money pages to use strategy-scoped balances instead of global portfolio totals, relabeled strategy realized PnL as process-local instead of pretending to match Polymarket's full historical ledger, and updated the `pa-risk` test fixture for the newer `RiskConfig` execution-quality fields.
- Why: Market-specific frontend pages were showing misleading all-account cash and portfolio values, while their realized-PnL cards were still pulling a global in-process gauge that does not match Polymarket's full historical收益口径.

### 2026-03-21
- Area: `crates/pa-monitor/src/api.rs`, `crates/pa-strategy/src/crypto_alpha.rs`
- Change: Added bucket-level arbitration and deduplication for `crypto_override_suggestions`, refined adaptive crypto gate-scale sizing feedback to prefer exact-asset matches before falling back to `asset_class × event_subtype`, and downgraded post-pre-sizing depth/size-retention failures to final sanity guards instead of re-emitting them as front-door `gate_reject` diagnostics.
- Why: The remaining crypto strategy polish gaps were that override suggestions could still emit conflicting actions for the same tuning bucket, adaptive pre-sizing feedback was too coarse to react differently for a specific asset versus the whole class, and post-scale guard failures were still being represented like primary front-door friction even though the actionable signal had already been captured as `gate_scale`.

### 2026-03-21
- Area: `crates/pa-strategy/src/weather.rs`
- Change: Added lightweight in-memory city feedback for the weather strategy so capital-efficiency exits reward a city's future entry edge/size slightly while relative stop-loss and model-reversal exits penalize it, and applied that feedback on top of the existing preferred/conservative city overlays with focused regression coverage.
- Why: Static city tiers are useful, but the strategy still needed a simple runtime mechanism to lean further into cities that are converting efficiently and back away from cities that are producing poor exits without waiting for manual retuning.

### 2026-03-21
- Area: `crates/pa-strategy/src/weather.rs`
- Change: Added a preferred-city overlay for higher-confidence weather cities (`Atlanta`, `Miami`, `New York`, `Dallas`, `Seattle`) that slightly lowers effective entry edge and slightly increases per-trade size, while restricting NegRisk surround entries to validated non-conservative cities and requiring an extra surround-specific edge buffer on top of the normal city/resolution threshold.
- Why: The weather strategy should lean more aggressively into the cities with the best live/replay confidence while preventing the more speculative surround pattern from extending into conservative or gray-rollout cities such as London, Chicago, and still-protected locations.

### 2026-03-21
- Area: `crates/pa-core/src/config.rs`, `crates/pa-strategy/src/weather.rs`, `config/default.toml`, `README.md`, `CLAUDE.md`
- Change: Relaxed the weather entry profile by lowering the base `min_edge_bps` default from `500` to `450`, raising the base `max_entry_price` default from `0.35` to `0.38`, reducing the conservative-city edge overlay from `+100bps` to `+50bps`, and shrinking the short-dated edge overlays from `+100/+200bps` to `+50/+100bps`, with updated regression expectations and operator-facing docs.
- Why: Live weather trading had become too sparse after city overlays and short-dated tightening stacked on top of the already conservative base thresholds, so the entry profile needs a modest reopening before judging the newer routing and WS changes.

### 2026-03-21
- Area: `crates/pa-core/src/config.rs`, `crates/pa-strategy/src/engine.rs`, `crates/pa-strategy/src/crypto_alpha.rs`, `src/app/account_runtime.rs`, `src/bin/backtest.rs`, `config/default.toml`, `frontend/src/components/ConfigSection.tsx`, `README.md`
- Change: Added configurable execution-quality weights to risk settings, switched both engine-side prepared-opportunity scoring and crypto same-asset candidate scoring to a weighted geometric mean of profit retention, size retention, and slippage quality, and threaded the new weights through runtime/backtest wiring with focused regression coverage for weight-sensitive ordering.
- Why: Crypto execution-quality ranking had evolved into a major strategy decision surface, but it still treated retained profit, retained size, and slippage quality as permanently equal; configurable weights let operators bias selection toward the execution durability dimension that matters most for a given market regime without discarding the existing multiplicative semantics.

### 2026-03-21
- Area: `crates/pa-core/src/config.rs`, `crates/pa-monitor/src/diagnostics.rs`, `crates/pa-strategy/src/crypto_alpha.rs`, `config/default.toml`, `frontend/src/components/ConfigSection.tsx`, `README.md`
- Change: Added adaptive crypto gate-scale feedback settings (`gate_scale_feedback_lookback/trigger_count/step_multiplier/max_steps`), taught `size_entry()` to shrink target size when the same `asset_class × event_subtype` bucket has recently produced repeated `gate_scale` decisions, and added test-only recent-decision clearing helpers plus focused regression coverage for the new pre-sizing feedback path.
- Why: Once crypto diagnostics separated true front-door rejects from pre-sizing friction, the remaining runtime gap was that buckets with repeated `gate_scale` events still kept generating over-optimistic target sizes and only got trimmed later; feeding that friction back into sizing itself reduces repeated “generate then scale down” churn.

### 2026-03-21
- Area: `crates/pa-core/src/config.rs`
- Change: Extended direct `PA_` environment-variable backfills to include `PA_WEATHER__KMA_API_KEY`, `PA_WEATHER__MET_OFFICE_API_KEY`, and `PA_WEATHER__MET_OFFICE_OBS_API_KEY`, and added focused regression coverage for the weather API key backfill path.
- Why: Nested weather API key environment variables were still vulnerable to `config` env-deserialization misses even after `.env` loading, which left London/Seoul provider clients seeing empty credentials despite operators configuring the expected `PA_WEATHER__...` variables.

### 2026-03-21
- Area: `crates/pa-strategy/src/weather.rs`
- Change: Aligned NegRisk surround sizing with the same city/resolution-aware USDC caps used by normal weather entries before splitting size across surround bins, and updated London/Seoul provider archive error text to describe the current PostgreSQL snapshot-backed replay path instead of claiming the cities must remain blocked because official historical-forecast APIs are missing.
- Why: Surround entries should not bypass the conservative city and short-dated risk overlays that already govern the rest of the weather strategy, and the old international archive error strings had drifted behind the actual replay architecture.

### 2026-03-21
- Area: `crates/pa-strategy/src/weather.rs`, `src/app/account_runtime.rs`
- Change: Removed the now-unused weather-strategy `get_balance` dependency and cleaned up the related runtime/test wiring after surround sizing stopped using raw wallet-balance-based budgets.
- Why: The weather strategy no longer sizes off total wallet balance directly, so keeping an unused balance callback around only produced warnings and extra constructor noise without affecting behavior.

### 2026-03-21
- Area: `crates/pa-monitor/src/api.rs`, `frontend/src/api.ts`, `frontend/src/pages/CryptoMarkets.tsx`
- Change: Extended the backend-owned crypto tuning summary to emit structured `crypto_override_suggestions` with selector scope, target override field, direction, source reason, and rationale, and exposed those suggestions on the crypto diagnostics page as a dedicated override-adjustment block.
- Why: Parameter hints alone still left operators manually translating friction summaries into concrete override-table edits; surfacing structured field-level suggestions makes the bridge from runtime friction to `calibration_overrides` much more direct.

### 2026-03-21
- Area: `crates/pa-monitor/src/api.rs`, `frontend/src/api.ts`, `frontend/src/pages/CryptoMarkets.tsx`
- Change: Added backend-generated `crypto_entry_tuning_hints` to `/api/status`, deriving prioritized tuning suggestions from the combined gate-reject and gate-scale summaries, and surfaced those structured hints on the CryptoMarkets diagnostics page as a dedicated “当前参数建议” block.
- Why: Once the crypto page could distinguish outright front-door rejects from pre-sizing friction, the remaining operational gap was turning those summaries into consistent next-step advice; moving hint generation to the backend keeps tuning guidance reusable and aligned across clients.

### 2026-03-21
- Area: `crates/pa-monitor/src/api.rs`, `frontend/src/api.ts`, `frontend/src/pages/CryptoMarkets.tsx`
- Change: Added a backend-owned `crypto_gate_scale_summary` to `/api/status`, carrying the same reason/asset/subtype breakdown shape as `crypto_gate_reject_summary`, and surfaced it on the crypto page as a dedicated “最近前门缩量” block so operators can distinguish front-door rejects from event/depth-driven pre-sizing.
- Why: After separating `gate_scale` from true `gate_reject` in the crypto strategy, the remaining observability gap was that the page still summarized only outright rejects, which under-reported execution-aware front-door friction whenever candidates were heavily scaled down but still allowed through.

### 2026-03-21
- Area: `crates/pa-strategy/src/crypto_alpha.rs`
- Change: Fixed crypto front-door override routing so entry-threshold overrides now use the true market type and known asset context on binary versus range paths, extended edge-decay confirmation-window cleanup to include the maximum event-aware override multiplier, and added explicit `gate_scale` candidate-decision records when entry sizing is pre-cut by depth-buffer or retained-size constraints instead of being fully rejected.
- Why: The crypto strategy still had three structural gaps after the event-aware override rollout: range/NegRisk entry thresholds were querying binary rows, edge-decay confirmation cleanup could prune states too early when event-aware window multipliers exceeded the global baseline, and execution-aware front-door diagnostics still conflated genuine rejects with candidates that were only being pre-sized down to fit the book.

### 2026-03-21
- Area: `crates/pa-core/src/config.rs`, `crates/pa-strategy/src/crypto_alpha.rs`, `config/default.toml`, `frontend/src/components/ConfigSection.tsx`, `README.md`
- Change: Added runtime guardrails for non-probability crypto calibration multipliers via `override_multiplier_blend` and `override_multiplier_max_delta_bps`, applied those guardrails across table-driven sigma/size/entry/exit/execution multiplier lookups, and added focused regression coverage for blended and clamped multiplier overrides.
- Why: The crypto strategy had already moved most event-aware behavior into `calibration_overrides`, but unlike probability calibration those live multiplier rows still applied at full strength; adding a shared runtime blend/clamp layer makes table-driven tuning materially safer to roll into production.

### 2026-03-21
- Area: `crates/pa-monitor/src/api.rs`, `frontend/src/api.ts`, `frontend/src/pages/CryptoMarkets.tsx`
- Change: Added `asset_windows` to `/api/status` `crypto_gate_reject_summary`, exposing recent-8 and recent-24 asset breakdowns for gate rejects, and surfaced those short/long asset windows in the CryptoMarkets gate-friction summary.
- Why: Short and long gate-reject windows for reasons and event subtypes still left ambiguity about whether the same friction was concentrated in a single coin or broadly distributed; asset windows make sudden symbol-specific entry friction easier to spot.

### 2026-03-21
- Area: `crates/pa-monitor/src/api.rs`, `frontend/src/api.ts`, `frontend/src/pages/CryptoMarkets.tsx`
- Change: Added `subtype_windows` to `/api/status` `crypto_gate_reject_summary`, exposing recent-8 and recent-24 event-subtype breakdowns for gate rejects, and surfaced those short/long subtype windows in the CryptoMarkets gate-friction summary.
- Why: Reason windows alone still hide whether the same entry friction is concentrated in a specific event bucket like `unlock` or `regulatory`; adding subtype windows makes sudden event-class-specific front-door friction easier to spot.

### 2026-03-21
- Area: `crates/pa-monitor/src/api.rs`, `frontend/src/api.ts`, `frontend/src/pages/CryptoMarkets.tsx`
- Change: Added `reason_windows` to `/api/status` `crypto_gate_reject_summary`, exposing ordered recent-8 and recent-24 gate-reject reason counts, and surfaced those short/long window breakdowns in the CryptoMarkets gate-friction summary.
- Why: A single aggregated reason distribution hides whether crypto front-door friction is stable or has just shifted; exposing both short and longer gate-reject windows makes it easier to see if the dominant entry failure mode is persistent or newly emerging.

### 2026-03-21
- Area: `crates/pa-strategy/src/crypto_alpha.rs`
- Change: Upgraded the periodic crypto scan diagnostics log so recent `gate_reject` output now includes top per-reason `asset/subtype` pairings instead of only three independent global top fields, exposing a compact top-three `reason -> asset/subtype` summary alongside the existing gate-reject counts.
- Why: Once `/api/status` and the frontend summarized gate friction as per-reason pairings, the runtime log still lagged behind with only global top reason/asset/subtype fields; matching the log output to the same diagnostic shape makes crypto entry friction readable even without the UI.

### 2026-03-21
- Area: `crates/pa-monitor/src/api.rs`, `frontend/src/api.ts`, `frontend/src/pages/CryptoMarkets.tsx`
- Change: Expanded `/api/status` `crypto_gate_reject_summary` with ordered `reason_details` entries that pair each leading gate-reject reason with its dominant asset and event subtype, and surfaced those per-reason pairings in the CryptoMarkets gate-friction summary.
- Why: Global top-1 reason/asset/subtype chips still left ambiguity about whether the dominant asset or subtype actually belonged to the same gate reason; pairing them per reason makes front-door crypto friction diagnostics more actionable.

### 2026-03-21
- Area: `crates/pa-monitor/src/api.rs`, `frontend/src/api.ts`, `frontend/src/pages/CryptoMarkets.tsx`
- Change: Expanded `/api/status` `crypto_gate_reject_summary` with ordered `asset_counts` and `subtype_counts` lists, and taught the CryptoMarkets gate-friction asset/subtype chips to prefer those backend-owned distributions over local recomputation.
- Why: After moving the recent gate-reject reason breakdown into the status API, the remaining mismatch was that asset and event-subtype chips were still derived locally; exposing the full backend counts keeps the entire front-door friction summary aligned across status consumers.

### 2026-03-21
- Area: `crates/pa-monitor/src/api.rs`, `frontend/src/api.ts`, `frontend/src/pages/CryptoMarkets.tsx`
- Change: Expanded `/api/status` `crypto_gate_reject_summary` with a full backend-owned `reason_counts` breakdown and taught the CryptoMarkets page to prefer that status payload when rendering recent gate-friction reason chips.
- Why: The status API previously exposed only the dominant gate-reject reason, which left frontend and other consumers recomputing the visible reason distribution locally instead of sharing one consistent runtime snapshot.

### 2026-03-21
- Area: `crates/pa-monitor/src/api.rs`, `frontend/src/api.ts`, `frontend/src/pages/CryptoMarkets.tsx`
- Change: Added a lightweight `crypto_gate_reject_summary` to `/api/status`, exposing the recent gate-reject count plus dominant reason/asset/subtype, and taught the CryptoMarkets page to prefer that backend summary over ad hoc client-side derivation when rendering the gate-friction priority hint.
- Why: Gate-friction diagnostics had reached logs and frontend-local summaries, but the status API still lacked a compact backend-owned snapshot of the same runtime picture; exposing it through `/api/status` keeps the gate-friction headline consistent across consumers.

### 2026-03-21
- Area: `crates/pa-strategy/src/crypto_alpha.rs`
- Change: Extended the periodic crypto scan diagnostics log to summarize recent `gate_reject` decisions, including the dominant gate reason, top affected asset, and top event subtype from the recent entry-gate failures.
- Why: The frontend already exposed gate-friction summaries, but operators also need the same high-level crypto entry-failure picture in runtime logs when monitoring the bot without the UI.

### 2026-03-21
- Area: `frontend/src/pages/CryptoMarkets.tsx`
- Change: Extended the gate-friction priority summary to incorporate the dominant visible event subtype, so the hint line now distinguishes whether the current front-door friction is concentrated in `unlock`, `regulatory`, or generic crypto markets rather than only naming the dominant gate reason.
- Why: A gate-reject reason alone still leaves ambiguity about whether the problem is broad-based liquidity/threshold pressure or concentrated in a specific event class; surfacing the dominant subtype makes the next crypto tuning step more precise.

### 2026-03-21
- Area: `frontend/src/pages/CryptoMarkets.tsx`
- Change: Added a lightweight gate-friction priority summary that maps the dominant visible gate-reject reason plus top affected asset to a short “look at risk vs look at entry parameters” recommendation above the recent candidate decisions table.
- Why: Gate-reject counts and asset/subtype chips are useful, but operators still needed one more compression step to decide whether the next tuning move should target exposure/budget controls or the front-door entry thresholds themselves.

### 2026-03-21
- Area: `frontend/src/pages/CryptoMarkets.tsx`
- Change: Extended the crypto gate-reject summary with top-by-asset and top-by-event-subtype breakdown chips so the page now highlights which assets and event buckets dominate recent front-door entry friction, not just which raw gate reason is most common.
- Why: Once the UI summarized gate rejects by reason and showed a high-level tuning hint, the remaining operational gap was distinguishing whether the friction was concentrated in a specific asset or event class, which is often more actionable for crypto tuning than a reason count alone.

### 2026-03-21
- Area: `frontend/src/pages/CryptoMarkets.tsx`
- Change: Added a short “当前最常见前门摩擦” summary above the recent crypto gate-reject breakdown, mapping the dominant visible gate-reject reason to a concise parameter-tuning hint (edge, spread, depth, size retention, exposure, or budget).
- Why: The gate-reject aggregation already showed counts by reason, but operators still had to translate those counts into likely next tuning actions; a lightweight hint makes it faster to decide whether to inspect edge thresholds, spread limits, retained-size/depth gates, or exposure/budget constraints.

### 2026-03-21
- Area: `frontend/src/pages/CryptoMarkets.tsx`
- Change: Added a compact gate-reject breakdown summary above the recent crypto candidate decisions table, aggregating visible `gate_reject` rows by structured reason and surfacing the most common front-door failure buckets before the detailed per-row diagnostics.
- Why: Once gate-reject events covered the main crypto entry failure modes, operators still had to read them row by row; a small grouped summary makes it much faster to see whether the current strategy is mostly missing on edge, spread, depth, retained size, exposure, or budget.

### 2026-03-21
- Area: `crates/pa-strategy/src/crypto_alpha.rs`
- Change: Extended crypto gate-reject diagnostics so `asset_exposure_cap` and `min_order_or_budget` now emit explicit `gate_reject` candidate-decision records through the same gate context path used by the other entry-gate failures.
- Why: After adding gate-reject coverage for spread, edge, depth-buffer, and retained-size failures, the remaining major front-door crypto entry exits were still invisible in the candidate decision stream even though they often explain why a candidate never reaches bucket competition.

### 2026-03-21
- Area: `crates/pa-strategy/src/crypto_alpha.rs`
- Change: Extended crypto gate-reject diagnostics so `insufficient_size_retention` now emits an explicit `gate_reject` candidate-decision record through the same lightweight gate context used for other entry-gate failures.
- Why: Entry-gate diagnostics already split out spread, edge, and depth-buffer failures, but retained-size rejects were still invisible in the candidate decision stream even though they are one of the main execution-aware reasons a crypto entry can be discarded before bucket competition.

### 2026-03-21
- Area: `crates/pa-strategy/src/crypto_alpha.rs`, `frontend/src/pages/CryptoMarkets.tsx`
- Change: Extended crypto candidate gate diagnostics so `spread_too_wide`, `edge_below_threshold`, and `insufficient_depth_buffer` now emit explicit `gate_reject` decision records with event-aware context, threaded lightweight gate context through `size_entry()` for depth-buffer rejects, and updated the CryptoMarkets decision table to highlight gate rejects separately from same-asset bucket competition.
- Why: After adding `replace` and `reject` records for same-asset candidate competition, operators still could not distinguish bucket-level losses from front-door entry-gate failures; the crypto diagnostics path now separates those two strategy failure modes.

### 2026-03-21
- Area: `crates/pa-strategy/src/crypto_alpha.rs`, `frontend/src/pages/CryptoMarkets.tsx`
- Change: Added explicit `gate_reject` candidate-decision records for crypto entry gates, recording `edge_below_threshold` rejects alongside the existing same-asset `replace/reject/seed` decisions, and updated the CryptoMarkets decision table to distinguish `gate_reject` actions visually from bucket-competition outcomes.
- Why: Crypto candidate diagnostics could already explain same-asset competition inside a bucket, but operators still could not tell whether a missed opportunity lost to another candidate or never cleared the front-door entry gate in the first place.

### 2026-03-21
- Area: `crates/pa-monitor/src/diagnostics.rs`, `crates/pa-strategy/src/crypto_alpha.rs`, `frontend/src/api.ts`, `frontend/src/pages/CryptoMarkets.tsx`
- Change: Added explicit `reject` candidate-decision records for same-asset crypto buckets, reused the ordered execution-quality comparison chain to produce a structured `reason` for both `replace` and `reject`, and updated the CryptoMarkets decision table to surface those reasons and distinguish reject actions visually.
- Why: Same-asset crypto diagnostics previously explained only the winning replacement path; operators also need to see why an incoming candidate lost the bucket altogether, using the same ordered runtime comparison logic the strategy itself applies.

### 2026-03-21
- Area: `crates/pa-monitor/src/diagnostics.rs`, `crates/pa-strategy/src/crypto_alpha.rs`, `frontend/src/api.ts`, `frontend/src/pages/CryptoMarkets.tsx`
- Change: Changed same-asset crypto candidate dedupe to prefer composite executable quality before falling back to retention/slippage/depth tie-breaks, recorded a structured candidate-decision `reason` alongside executable quality scores, and surfaced that reason in the CryptoMarkets diagnostics table so replacements now explain whether they were driven by execution quality, retention, slippage, depth, or later tie-breaks.
- Why: Once candidate diagnostics exposed the underlying execution-retention signals, the next strategy gap was that operators still had to infer why a replacement happened; the runtime and diagnostics should agree on a single ordered reason chain for same-asset candidate selection.

### 2026-03-21
- Area: `crates/pa-monitor/src/diagnostics.rs`, `crates/pa-strategy/src/crypto_alpha.rs`, `src/app/account_runtime.rs`, `frontend/src/api.ts`, `frontend/src/pages/CryptoMarkets.tsx`
- Change: Added crypto-local executable quality scoring for candidate diagnostics by threading the global buy-side slippage budget into `CryptoAlphaStrategy`, computing a same-book `profit_retention × size_retention × slippage_quality` score for candidate entries, recording selected/replaced quality scores in recent crypto candidate decisions, and exposing the new “执行质量” comparison column in the CryptoMarkets diagnostics table.
- Why: After the engine began prioritizing prepared crypto opportunities by a composite execution-quality score, operators still could not see that same combined signal in same-asset candidate decisions; the diagnostics path should expose the same quality lens the runtime is increasingly using.

### 2026-03-21
- Area: `crates/pa-strategy/src/engine.rs`
- Change: Added a composite prepared-opportunity `execution_quality_score` that combines buy-side profit retention, size retention, and slippage-quality signals, changed non-weather execution ordering to prefer that score before retention/efficiency/absolute-profit tie-breaks, and added focused regression coverage for quality-driven crypto ordering.
- Why: Engine freshness now judges crypto buy opportunities on multiple execution-durability axes at once, so non-weather execution order should rank prepared opportunities by the same combined execution quality instead of over-weighting a single retention metric or raw profit.

### 2026-03-21
- Area: `crates/pa-strategy/src/crypto_alpha.rs`
- Change: Unified single-market and grouped-binary crypto event-aware sizing inputs to use the full `event_title + question` market text when computing entry thresholds, sigma, and size multipliers, and added focused regression coverage proving a single-market `event_title` now tightens event-aware sizing.
- Why: Crypto event subtype and event-calendar overlays were already designed to react to full market context, but parts of the single-market/grouped-binary path still only passed the bare question or group title into event-aware sigma/size logic, which could under-apply event overlays when the decisive event context only lived in `event_title`.

### 2026-03-21
- Area: `crates/pa-monitor/src/diagnostics.rs`, `crates/pa-strategy/src/crypto_alpha.rs`, `frontend/src/api.ts`, `frontend/src/pages/CryptoMarkets.tsx`
- Change: Extended recent crypto candidate decision diagnostics with `selected/replaced executable_size_retention`, recorded that signal from same-asset candidate dedupe, and exposed it in the CryptoMarkets decision table as a dedicated “数量保真” comparison column.
- Why: Same-asset crypto candidate selection now considers how much of the intended size survives execution on current book depth, so operators need that retained-size signal visible when reviewing why one candidate replaced another.

### 2026-03-21
- Area: `crates/pa-strategy/src/crypto_alpha.rs`
- Change: Reordered same-asset crypto candidate dedupe so, after executable profit retention and slippage checks tie, the strategy now prefers higher post-sizing depth headroom before executable efficiency and static profit tie-breaks; also added focused regression coverage for deeper-book candidate selection.
- Why: Crypto candidate sizing now bakes in more event-aware execution constraints up front, so same-asset candidate choice should also prefer the opportunity with more residual depth cushion after sizing instead of over-optimizing for slightly higher executable efficiency on a thinner book.

### 2026-03-20
- Area: `crates/pa-strategy/src/crypto_alpha.rs`, `src/app/account_runtime.rs`
- Change: Threaded the global risk `min_size_retention_ratio` into `CryptoAlphaStrategy`, applied event-aware `size_retention_multiplier` during `size_entry()` so crypto candidates pre-cap target size against current book depth before execution freshness, and added focused regression coverage for pre-execution size capping by size retention.
- Why: Crypto event-aware execution already tightened retained-size requirements at freshness time, but candidate sizing still targeted quantities that risky event buckets were likely to shrink later; applying the same retained-size semantics earlier makes crypto entry sizing more realistic and reduces avoidable execution-stage trim.

### 2026-03-20
- Area: `crates/pa-core/src/config.rs`, `crates/pa-strategy/src/crypto_alpha.rs`, `src/bin/crypto_calibrate.rs`, `config/default.toml`, `frontend/src/components/ConfigSection.tsx`, `README.md`
- Change: Added `depth_ratio_multiplier` to crypto calibration overrides, applied event-aware depth-ratio tightening during `size_entry()` so risky crypto buckets pre-cap entry size by a stricter depth buffer before execution freshness, preserved/rendered the new field across calibrate/config/docs, and added focused regression coverage for pre-execution size capping by depth ratio.
- Why: Crypto event-aware execution had already tightened slippage, profit retention, and retained size at freshness time, but sizing still aimed for the same target size up front; risky event buckets should also shrink earlier when available book depth cannot support the original target with enough buffer.

### 2026-03-20
- Area: `crates/pa-strategy/src/crypto_alpha.rs`
- Change: Added an `entry_executable_slippage_bps` comparison signal to same-asset candidate dedupe so, after executable profit retention ties, the strategy now prefers lower book-walk slippage before falling back to executable efficiency and static profit tie-breaks; also added focused regression coverage for lower-slippage candidate selection.
- Why: Profit retention alone still leaves cases where two candidates keep the same total edge but one consumes much more of the order book to do so; crypto candidate selection should prefer the path with lower executable slippage when durability is otherwise equal.

### 2026-03-20
- Area: `crates/pa-core/src/types.rs`, `crates/pa-core/src/config.rs`, `crates/pa-strategy/src/engine.rs`, `crates/pa-strategy/src/crypto_alpha.rs`, `src/app/account_runtime.rs`, `src/bin/backtest.rs`, `src/bin/crypto_calibrate.rs`, `crates/pa-risk/src/manager.rs`, `crates/pa-strategy/src/weather.rs`, `crates/pa-strategy/src/smart_money.rs`, `config/default.toml`, `frontend/src/components/ConfigSection.tsx`, `README.md`
- Change: Added global `risk.min_size_retention_ratio`, added `TradingOpportunity.min_size_retention_ratio_multiplier`, extended crypto calibration overrides with `size_retention_multiplier`, wired engine freshness to reject buy opportunities whose scaled size falls below the effective retained-size floor, emitted event-aware size-retention multipliers from crypto entry generation, preserved/rendered the new field through calibrate merge output and config/docs/default override rows, and added focused engine regression coverage proving an opportunity-local size-retention multiplier can reject a trade that otherwise passes global freshness checks.
- Why: Crypto execution freshness already guarded repricing via slippage budget and preserved edge via profit-retention thresholds, but it still accepted heavily shrunken fills as long as the remainder stayed profitable; risky event buckets like unlock/regulatory should also be able to require that a materially larger fraction of the original intended size survives depth scaling before the trade remains worth executing.

### 2026-03-20
- Area: `crates/pa-core/src/types.rs`, `crates/pa-core/src/config.rs`, `crates/pa-strategy/src/engine.rs`, `crates/pa-strategy/src/crypto_alpha.rs`, `src/bin/crypto_calibrate.rs`, `config/default.toml`, `frontend/src/components/ConfigSection.tsx`, `README.md`
- Change: Added `TradingOpportunity.max_slippage_bps_multiplier`, extended crypto calibration overrides with `slippage_multiplier`, wired crypto event-aware entry generation to emit opportunity-local slippage-budget tightening into execution freshness, updated the engine to apply opportunity-local slippage multipliers on top of the global `risk.max_slippage_bps`, taught the calibrate renderer/merge path plus config/docs/default override rows to preserve the new field, and added focused engine regression coverage for stricter opportunity-local slippage rejection.
- Why: Crypto event-aware tuning had already tightened entry thresholds, holding exits, profit retention, and post-entry management, but execution slippage budget was still one global knob; risky event buckets like unlock/regulatory should also be able to demand a narrower repricing window before a buy stays executable.

### 2026-03-20
- Area: `crates/pa-core/src/types.rs`, `crates/pa-core/src/config.rs`, `crates/pa-strategy/src/engine.rs`, `crates/pa-strategy/src/crypto_alpha.rs`, `src/bin/crypto_calibrate.rs`, `config/default.toml`, `frontend/src/components/ConfigSection.tsx`, `README.md`
- Change: Added `TradingOpportunity.min_profit_retention_ratio_multiplier`, taught engine freshness to apply it on top of the global risk `min_profit_retention_ratio`, extended crypto calibration overrides with `profit_retention_multiplier`, wired crypto entry generation to emit event-aware retention multipliers into buy opportunities, updated the calibrate renderer/merge path plus config/docs/default override rows, and added focused engine regression coverage for opportunity-local retention tightening.
- Why: Crypto event-aware tuning had already reached entry, sigma, size, and post-entry exits, but execution freshness still used one global profit-retention threshold; risky event buckets like unlock/regulatory should also be able to demand stronger edge preservation before a repriced buy is still allowed through execution.

### 2026-03-20
- Area: `crates/pa-monitor/src/diagnostics.rs`, `crates/pa-strategy/src/crypto_alpha.rs`, `frontend/src/api.ts`, `frontend/src/pages/CryptoMarkets.tsx`
- Change: Extended recent crypto candidate diagnostics with `selected/replaced executable_profit_retention`, recorded that signal from same-asset candidate dedupe, and exposed it in the CryptoMarkets decision table as a dedicated “利润保真” comparison column.
- Why: After candidate selection began preferring executable profit retention over raw executable efficiency alone, operators also need to see that retention signal directly when reviewing why one same-asset crypto candidate replaced another.

### 2026-03-20
- Area: `crates/pa-strategy/src/crypto_alpha.rs`
- Change: Added an execution-side `entry_executable_profit_retention` signal to same-asset candidate dedupe, changed `keep_better_entry()` to prefer higher executable profit retention before executable efficiency, and added focused regression coverage for partial-fill/high-retention versus higher-efficiency candidate competition.
- Why: After the engine began prioritizing prepared opportunities whose edge survives repricing better, crypto candidate dedupe still only optimized for executable efficiency; same-asset buckets should also avoid preferring entries whose expected profit is likely to decay more severely before execution.

### 2026-03-20
- Area: `crates/pa-strategy/src/engine.rs`
- Change: Wrapped non-weather prepared opportunities with a buy-side `profit_retention_ratio`, changed prepared execution ordering to prefer higher retention before refreshed `profit / cost`, and added regression coverage using dynamic order-book repricing to prove the engine now favors more durable crypto entries over slightly higher-efficiency but more decayed ones.
- Why: Once buy freshness began enforcing slippage budgets and minimum profit retention, execution ordering still ignored how much edge survived repricing; crypto execution should prioritize opportunities whose expected profit remains more intact through the preparation path.

### 2026-03-20
- Area: `crates/pa-core/src/config.rs`, `crates/pa-strategy/src/engine.rs`, `src/app/account_runtime.rs`, `src/bin/backtest.rs`, `config/default.toml`, `frontend/src/components/ConfigSection.tsx`
- Change: Added `risk.min_profit_retention_ratio`, wired it into buy-side execution freshness so repriced entries must retain a configurable fraction of their original estimated profit after slippage-budget repricing, updated runtime/backtest engine wiring to pass the new risk setting through, and added focused freshness regression coverage for the new rejection path.
- Why: Allowing small buy-side reprices within a slippage budget improves crypto execution realism, but without a profit-retention guard the engine could still accept trades whose edge has decayed to “barely positive” during freshness refresh.

### 2026-03-20
- Area: `crates/pa-strategy/src/engine.rs`, `src/app/account_runtime.rs`
- Change: Wired the engine's freshness validation to use the configured risk `max_slippage_bps` as an explicit buy-side repricing budget, allowing small upward ask moves within budget during execution freshness checks, repricing the order to the walked worst ask when still profitable, and rejecting only when the refreshed ask or walked fill exceeds the allowed slippage window.
- Why: Crypto execution quality needed a real slippage-budget semantic rather than an all-or-nothing stale-price check; small ask jumps should be survivable if they remain within the configured risk budget and the refreshed trade still has positive profit.

### 2026-03-20
- Area: `crates/pa-core/src/config.rs`, `crates/pa-strategy/src/crypto_alpha.rs`, `src/bin/crypto_calibrate.rs`, `config/default.toml`, `frontend/src/components/ConfigSection.tsx`, `README.md`
- Change: Extended crypto calibration overrides with optional `capital_efficiency_multiplier` and `model_reversal_buffer_multiplier`, applied those fields through event-aware exit-threshold computation so subtype buckets can tighten capital-efficiency exits and model-reversal buffers after entry, updated default crypto subtype override rows to carry the new exit-management tuning, and taught the calibrate renderer plus config UI table to preserve/display the extra override fields.
- Why: Crypto event awareness already influenced entry, sigma, sizing, hold-edge, and edge-decay trim cadence, but capital-efficiency and model-reversal exits were still only horizon-aware; subtype buckets should also be able to shorten holding time by tightening those two post-entry thresholds.

### 2026-03-20
- Area: `crates/pa-core/src/config.rs`, `crates/pa-strategy/src/crypto_alpha.rs`, `src/bin/crypto_calibrate.rs`, `config/default.toml`, `frontend/src/components/ConfigSection.tsx`, `README.md`
- Change: Extended crypto calibration overrides with optional `edge_decay_confirmation_scan_multiplier`, `edge_decay_confirmation_window_multiplier`, and `edge_decay_cooldown_multiplier`, wired those fields into event-aware edge-decay confirmation and cooldown logic, updated default crypto subtype override rows to tune confirmation speed/window/cooldown by event bucket, and taught the calibrate renderer plus config UI table to preserve/display the new override fields.
- Why: Crypto event-aware post-entry management had already reached hold-edge thresholds and trim size, but the strategy still confirmed and revisited trims with one global cadence; event subtype buckets should also influence how quickly edge decay is confirmed and how soon the strategy is willing to trim again.

### 2026-03-20
- Area: `crates/pa-core/src/config.rs`, `crates/pa-strategy/src/crypto_alpha.rs`, `src/bin/crypto_calibrate.rs`, `config/default.toml`, `frontend/src/components/ConfigSection.tsx`, `README.md`
- Change: Extended crypto calibration overrides with optional `hold_edge_multiplier` and `edge_decay_exit_multiplier`, applied those fields through field-specific specificity matching so event-aware overrides now influence hold-edge thresholds and edge-decay trim sizing during exit management, updated default crypto subtype override rows to carry post-entry management tuning, and taught the calibrate renderer plus config UI table to preserve/display the new override fields.
- Why: Crypto event awareness previously stopped at entry thresholds, sigma, and size; once subtype tuning moved to the calibration table, the next strategy gap was that holding and trim behavior still ignored those same event buckets.

### 2026-03-20
- Area: `crates/pa-core/src/config.rs`, `crates/pa-strategy/src/crypto_alpha.rs`, `config/default.toml`, `frontend/src/components/ConfigSection.tsx`, `README.md`
- Change: Added runtime guardrails for crypto probability calibration overrides via `override_probability_blend` and `override_probability_max_delta_bps`, blended override probability factors back toward the default baseline before applying them, clamped runtime deviation from the baseline factor, and added focused regression coverage plus operator-facing config labels/docs.
- Why: The crypto strategy's calibration table is now data-driven enough that newly generated probability factors need a conservative rollout path; blending and clamping keep fresh overrides from over-rotating the live model before operators have confidence in the new bucket calibration.

### 2026-03-20
- Area: `src/bin/crypto_pipeline_report.rs`, `docs/crypto-pipeline-report.html`, `docs/crypto-pipeline-report.md`, `README.md`
- Change: Extended the offline crypto pipeline report so calibrate sections now preserve and render `merge_diff_summary`, including `new / updated / unchanged` override row counts plus representative selector rows in markdown, aggregate JSON, and the static HTML viewer.
- Why: Once `crypto_calibrate` began emitting machine-readable merge diffs, the next operational gap was that downstream reports still only showed sample coverage; operators also need to see what a calibration batch would actually change in the runtime override table.

### 2026-03-20
- Area: `src/bin/crypto_calibrate.rs`, `src/bin/crypto_pipeline_report.rs`, `docs/crypto-pipeline-report.html`, `docs/crypto-pipeline-report.md`, `docs/crypto-calibration-workflow.md`, `README.md`
- Change: Added optional `--summary-output` JSON emission to `crypto_calibrate` with emitted and skipped segment details, extended `crypto_pipeline_report` plus the static offline viewer to ingest `crypto_calibrate_summary.json`, and surfaced skipped calibration buckets so underfilled segments show up directly in offline markdown/JSON/HTML reports.
- Why: Once the offline pipeline could split samples by `asset_class` and `event_subtype`, operators needed visibility into which calibration buckets still lacked enough samples instead of only seeing the segments that emitted override suggestions.

### 2026-03-20
- Area: `src/bin/crypto_calibrate.rs`, `src/bin/crypto_pipeline_report.rs`, `docs/crypto-pipeline-report.html`, `docs/crypto-pipeline-report.md`, `docs/crypto-calibration-workflow.md`, `README.md`
- Change: Added aggregated `underfilled_buckets` to the calibrate summary keyed by `asset_class × horizon × event_subtype`, then surfaced those grouped warnings in the offline pipeline report and static viewer alongside the raw skipped-segment list.
- Why: Raw skipped segments are too granular for deciding where more labels are needed, so the offline report should also expose higher-level sample gaps in the same grouping language used by the crypto override model.

### 2026-03-20
- Area: `src/bin/crypto_calibrate.rs`, `src/bin/crypto_pipeline_report.rs`, `docs/crypto-pipeline-report.html`, `docs/crypto-pipeline-report.md`, `docs/crypto-calibration-workflow.md`, `README.md`
- Change: Extended grouped `underfilled_buckets` with `gap_to_min_samples`, and updated the offline report plus static viewer to show how many additional labeled rows each aggregated bucket still needs to reach the calibrate step's `min_samples` threshold.
- Why: A bucket being underfilled is not enough to prioritize label work; explicit sample-gap counts make it obvious which grouped crypto segments are closest to becoming calibration-ready.

### 2026-03-20
- Area: `src/bin/crypto_calibrate.rs`, `src/bin/crypto_pipeline_report.rs`, `docs/crypto-pipeline-report.html`, `docs/crypto-pipeline-report.md`, `docs/crypto-calibration-workflow.md`, `README.md`
- Change: Added `threshold_band` to grouped `underfilled_buckets`, classifying each bucket as `near-threshold` or `far-from-threshold`, and surfaced that band in the offline report and static viewer alongside the remaining-sample gap.
- Why: After adding raw sample-gap counts, operators still needed a simpler prioritization cue that immediately separates buckets that are close to becoming calibration-ready from those that are still far away.

### 2026-03-20
- Area: `src/bin/crypto_pipeline_report.rs`, `docs/crypto-pipeline-report.html`, `docs/crypto-pipeline-report.md`, `docs/crypto-calibration-workflow.md`, `README.md`
- Change: Added a headline view for the top three `near-threshold` calibration buckets so both the markdown report and the static offline viewer surface the most actionable near-ready `asset_class × horizon × event_subtype` buckets before the full underfilled-bucket table.
- Why: Once grouped underfilled buckets had both gap counts and threshold bands, the next operational step was highlighting the very closest calibration candidates without forcing operators to scan the full breakdown.

### 2026-03-20
- Area: `src/bin/crypto_pipeline_report.rs`, `docs/crypto-pipeline-report.html`, `docs/crypto-pipeline-report.md`, `docs/crypto-calibration-workflow.md`, `README.md`
- Change: Added lightweight action hints (`top-up-now`, `ready-soon`, `defer`) for the headline `near-threshold` buckets in both the markdown report and the static offline viewer, based on each bucket's remaining sample gap.
- Why: After surfacing the top near-ready buckets, the remaining usability gap was telling operators what to do next, not just which buckets existed, so the report now translates sample gaps into immediate labeling priorities.

### 2026-03-20
- Area: `src/bin/crypto_pipeline_report.rs`, `docs/crypto-pipeline-report.html`, `docs/crypto-pipeline-report.md`, `docs/crypto-calibration-workflow.md`, `README.md`
- Change: Added headline-level action counts for the visible top-three `near-threshold` buckets so the markdown report and static viewer now summarize how many of those buckets are `top-up-now`, `ready-soon`, or `defer`.
- Why: Once individual action hints existed, the next operator need was a compact roll-up that shows the current mix of immediate versus lower-priority calibration work without reading each bucket row.

### 2026-03-20
- Area: `docs/crypto-pipeline-report.html`, `docs/crypto-pipeline-report.md`, `README.md`
- Change: Updated the static offline viewer so the headline status pill escalates to a more prominent warning when any visible top-three near-threshold bucket is marked `top-up-now`, and documented that behavior alongside the existing action-count summary.
- Why: After adding action counts, the most important remaining usability improvement was surfacing urgent label top-ups with a stronger visual status instead of leaving them buried in table text.

### 2026-03-20
- Area: `docs/crypto-pipeline-report.html`, `docs/crypto-pipeline-report.md`, `README.md`
- Change: Strengthened the offline viewer status escalation so `top-up-now >= 2` now upgrades both `headline-status` and `near-threshold-status` to `Urgent`, while a single `top-up-now` still shows `Action Needed`.
- Why: Once urgent buckets were visible, the remaining gap was distinguishing “one useful top-up” from “multiple immediate label actions,” so the viewer now reserves a stronger status for batches with more than one urgent near-threshold bucket.

### 2026-03-20
- Area: `docs/crypto-pipeline-report.html`, `docs/crypto-pipeline-report.md`, `README.md`
- Change: Added a short explanatory line beneath the headline section that spells out why the current batch is `Urgent`, `Action Needed`, or `Complete`, using the visible top-three near-threshold buckets as the explanation source.
- Why: Status colors and labels alone still forced operators to infer the reason for urgency, so the offline viewer now explains the status directly at the point of attention.

### 2026-03-20
- Area: `src/bin/crypto_pipeline_report.rs`, `docs/crypto-pipeline-report.md`, `README.md`
- Change: Added `headline_explainer` to the markdown/JSON pipeline report headline summary so the report artifacts now carry the same plain-language urgency explanation as the static offline viewer.
- Why: Once the viewer had a human-readable urgency explanation, the remaining mismatch was that markdown and aggregate JSON still only exposed raw counts; carrying the same explainer into the report keeps the offline artifacts aligned.

### 2026-03-20
- Area: `docs/crypto-pipeline-report.html`, `docs/crypto-pipeline-report.md`, `README.md`
- Change: Mirrored the near-threshold urgency explainer into the static viewer's hero section so the top-of-page status area now shows the same explanation as the headline panel without requiring scrolling.
- Why: After adding the explainer below the headline table, the remaining UX gap was discoverability on long reports; operators should see the urgency reason immediately at the top of the artifact.

### 2026-03-20
- Area: `docs/crypto-pipeline-report.html`, `docs/crypto-pipeline-report.md`, `README.md`
- Change: Styled the hero-level urgency explainer to follow the current status state, using warning color for `Urgent`/`Action Needed` and success color for `Complete`.
- Why: Once the hero section carried the same urgency explanation, it still looked like neutral body copy; matching the explainer color to the current status makes the top-of-page signal much easier to scan.

### 2026-03-20
- Area: `docs/crypto-pipeline-report.html`, `docs/crypto-pipeline-report.md`, `README.md`
- Change: Prefixed the hero urgency explainer with a compact `top-up-now` count badge so the top-of-page explanation now immediately shows how many urgent calibration buckets are currently visible.
- Why: After surfacing the urgency explanation at the top of the page, the next compression step was letting operators read the urgent bucket count from the first few words instead of parsing the whole sentence.

### 2026-03-20
- Area: `crates/pa-core/src/config.rs`, `config/default.toml`, `README.md`
- Change: Relaxed the default crypto `unlock / vesting` subtype entry overlays by lowering `unlock_event_min_edge_multiplier` from `1.15` to `1.08` and widening `unlock_event_max_spread_multiplier` from `0.90` to `0.95`, while leaving the existing unlock sigma and size penalties unchanged.
- Why: The previous default stack made short-dated high-impact unlock markets close to default-reject territory, so the strategy should still treat unlocks conservatively but remain capable of participating when the market is genuinely attractive.

### 2026-03-20
- Area: `crates/pa-core/src/config.rs`, `config/default.toml`, `README.md`
- Change: Relaxed the default crypto `upgrade / mainnet / fork` subtype entry overlays by lowering `upgrade_event_min_edge_multiplier` from `1.08` to `1.04` and widening `upgrade_event_max_spread_multiplier` from `0.95` to `0.98`, while keeping the existing upgrade sigma and size overlays unchanged.
- Why: The previous upgrade defaults were too close to the now-relaxed unlock profile and too weakly differentiated from generic crypto events, so the subtype should remain a mild cautionary overlay rather than another near-duplicate heavy gate.

### 2026-03-20
- Area: `crates/pa-core/src/config.rs`, `config/default.toml`, `crates/pa-strategy/src/crypto_alpha.rs`, `frontend/src/components/ConfigSection.tsx`, `README.md`
- Change: Split crypto event subtype overlays into major-asset and alt-asset paths by keeping the existing subtype multipliers for BTC/ETH, adding explicit `alt_*` subtype config fields for sigma/size/entry overlays, and wiring the strategy to apply the lighter alt regulatory defaults while preserving focused regression coverage.
- Why: A single subtype policy was still too blunt across all crypto assets; unlocks and regulatory events should not penalize majors and alts in the same way, and operators need those paths configurable without more hardcoded asset heuristics.

### 2026-03-20
- Area: `frontend/src/components/ConfigSection.tsx`
- Change: Added a derived crypto subtype profile summary table to the read-only configuration page so operators can see the current major/alt `unlock / upgrade / regulatory` sigma, size, min-edge, and max-spread overlays without manually scanning individual fields.
- Why: After splitting subtype controls into major and alt paths, the configuration became harder to reason about at a glance, so the UI should expose the effective per-subtype profile directly instead of forcing hand calculation from scattered numeric fields.

### 2026-03-20
- Area: `crates/pa-core/src/config.rs`, `config/default.toml`, `README.md`
- Change: Relaxed the default `alt unlock` subtype profile relative to the major-asset path by lowering sigma/min-edge pressure and widening size/max-spread tolerance (`1.07 sigma`, `0.90 size`, `1.05 min-edge`, `0.98 max-spread`).
- Why: After splitting subtype controls into major and alt paths, leaving alt unlocks identical to BTC/ETH unlocks was still too blunt; smaller alt unlock events should remain cautious but not inherit the full major-asset penalty stack by default.

### 2026-03-20
- Area: `crates/pa-core/src/config.rs`, `crates/pa-strategy/src/crypto_alpha.rs`, `frontend/src/components/ConfigSection.tsx`, `README.md`
- Change: Extended `crypto_alpha.calibration_overrides` with optional `asset_class` and `event_subtype` selectors, wired sigma/size override matching to use those selectors when event context exists, added focused regression coverage for `major` vs `alt` unlock overrides, and expanded the configuration UI table to display the extra selector columns.
- Why: Once static subtype defaults were split into major and alt paths, the calibration table also needed to express those distinctions so operators can tune event-aware sigma and sizing without adding more hardcoded branches or overloading the coarse asset selector.

### 2026-03-20
- Area: `config/default.toml`, `README.md`
- Change: Added two default short-horizon binary unlock calibration overrides, one for `major` assets and one for `alt` assets, so the table-driven override path now actively demonstrates event-aware sigma/size tuning instead of remaining purely a dormant operator feature.
- Why: After extending override matching to understand `asset_class` and `event_subtype`, the next step was to let the default config exercise that capability in a minimal, readable way rather than leaving all subtype tuning in static baseline fields.

### 2026-03-20
- Area: `frontend/src/components/ConfigSection.tsx`
- Change: Added a derived crypto override-coverage summary table that shows, for each `major/alt × unlock/upgrade/regulatory` bucket, whether `sigma` and `size` are still purely static or already covered by one or more calibration overrides.
- Why: Once event-aware overrides began coexisting with static subtype defaults, operators needed a quick way to see which parts of subtype tuning had actually migrated into the calibration table instead of inferring that from raw override rows.

### 2026-03-20
- Area: `crates/pa-core/src/config.rs`, `config/default.toml`, `README.md`, `frontend/src/components/ConfigSection.tsx`
- Change: Migrated the default `alt regulatory` sigma/size tuning from static subtype fields into a calibration override by resetting the static alt-regulatory sigma/size multipliers to neutral values and adding a matching `asset_class=alt + event_subtype=regulatory` override row; also clarified in the UI that subtype profile rows show static baselines while overrides may supersede sigma/size behavior.
- Why: A bucket should not be marked as override-driven while still getting the same tuning from both static fields and calibration rows; moving alt regulatory fully into the table makes the migration state real instead of nominal.

### 2026-03-20
- Area: `crates/pa-core/src/config.rs`, `config/default.toml`, `README.md`
- Change: Migrated the default `alt unlock` sigma/size tuning into calibration overrides by neutralizing the static alt-unlock sigma/size multipliers, adding a general `asset_class=alt + event_subtype=unlock` override for the baseline alt-unlock profile, and keeping the existing short-binary alt-unlock override as a more conservative secondary layer.
- Why: Once alt regulatory had fully moved into the table, leaving alt unlock half-static and half-override would keep the migration state inconsistent; the override path should own alt-unlock sigma/size completely while still allowing a stricter short-binary sub-case.

### 2026-03-20
- Area: `crates/pa-core/src/config.rs`, `config/default.toml`, `README.md`
- Change: Migrated the default `alt upgrade` subtype tuning fully into calibration overrides by neutralizing the static alt-upgrade sigma/size/min-edge/max-spread fields and adding a matching `asset_class=alt + event_subtype=upgrade` override row that carries the previous baseline values.
- Why: After moving alt unlock and alt regulatory onto the table-driven path, leaving alt upgrade on static fields would keep the alt-side subtype model split across two mechanisms instead of converging on one consistent event-aware override system.

### 2026-03-20
- Area: `crates/pa-core/src/config.rs`, `crates/pa-strategy/src/crypto_alpha.rs`, `config/default.toml`, `README.md`
- Change: Changed calibration-override selection to prefer the most specific matching rule rather than the first matching row, then migrated the default `major unlock` tuning into two override rows: a general `major + unlock` baseline and a more specific `short + binary + major + unlock` refinement; the old static major-unlock sigma/size/min-edge/max-spread fields were reset to neutral values.
- Why: Once the config needs both broad and narrow event-aware rules for the same subtype, first-match semantics become unsafe; specificity-based matching is required before major-side tuning can move into the table without broad rows masking narrower refinements.

### 2026-03-20
- Area: `crates/pa-core/src/config.rs`, `config/default.toml`, `README.md`
- Change: Migrated the remaining `major upgrade` and `major regulatory` subtype tuning into calibration overrides by neutralizing their static sigma/size/min-edge/max-spread fields and adding `asset_class=major + event_subtype=upgrade/regulatory` baseline override rows that preserve the previous values.
- Why: After moving every alt-side subtype and major unlock into the override system, leaving major upgrade/regulatory on static fields would keep the migration half-finished; the subtype model is cleaner once all six scope/subtype buckets use the same table-driven path.

### 2026-03-20
- Area: `crates/pa-core/src/config.rs`, `config/default.toml`, `README.md`
- Change: Neutralized the remaining static `alt unlock` and `alt regulatory` entry-threshold baseline fields (`min_edge` / `max_spread`) so those buckets now rely entirely on calibration overrides for front-door behavior instead of mixing static and override control.
- Why: After all six subtype buckets had table-driven rows, these two lingering non-neutral static entry fields were the last source of hidden double-stacking on the alt side and made the migration state misleading.

### 2026-03-20
- Area: `crates/pa-strategy/src/crypto_alpha.rs`, `frontend/src/components/ConfigSection.tsx`
- Change: Removed the runtime use of legacy static crypto subtype multipliers from event entry/sigma/size paths so subtype behavior now comes from calibration overrides, updated subtype-focused regression tests to inject override rows explicitly, and removed the frontend static subtype-profile card in favor of override-coverage only.
- Why: Once all subtype static baselines were neutralized, keeping a second conceptual subtype path in runtime logic and UI was misleading; the system should now reflect that subtype tuning is override-driven.

### 2026-03-20
- Area: `crates/pa-core/src/config.rs`, `crates/pa-strategy/src/crypto_alpha.rs`, `config/default.toml`, `README.md`, `frontend/src/components/ConfigSection.tsx`
- Change: Deleted the legacy crypto subtype static config fields (`major/alt × unlock/upgrade/regulatory` for sigma, size, min-edge, and max-spread), removed them from defaults/docs/frontend hints, and updated crypto strategy test config literals to match the slimmer `CryptoAlphaConfig`.
- Why: After subtype behavior was fully migrated to calibration overrides, the old always-`1.00` static fields were pure compatibility debt that made the config surface larger and more misleading than the runtime actually needed.

### 2026-03-20
- Area: `docs/crypto-pipeline-report.html`, `docs/crypto-pipeline-report.md`, `README.md`
- Change: Replaced the hero urgency explainer's inline `[N top-up-now]` text prefix with a dedicated pill element whose style now follows the same warning/success state as the surrounding explainer copy.
- Why: The offline report header already surfaced urgent calibration buckets, but rendering the top-up count as plain text made the signal visually weaker and harder to scan than the rest of the status UI.

### 2026-03-20
- Area: `docs/crypto-pipeline-report.html`, `docs/crypto-pipeline-report.md`, `README.md`
- Change: Unified the offline report's hero top-up pill and headline action-count badges under one reusable `action-chip` style so `top-up-now`, `ready-soon`, and `defer` counts render with consistent compact status chips instead of mixed plain text and one-off badge rules.
- Why: The headline action counts and hero urgency pill were expressing the same calibration-priority vocabulary through different UI treatments, which made the static report harder to scan and maintain.

### 2026-03-20
- Area: `docs/crypto-pipeline-report.html`, `docs/crypto-pipeline-report.md`, `README.md`
- Change: Added a separate `soft-warn` action-chip style for `ready-soon` buckets so the static offline report now visually distinguishes `top-up-now`, `ready-soon`, and `defer` instead of treating the middle tier like neutral text.
- Why: Once action counts were rendered as consistent chips, the remaining readability gap was that `ready-soon` still looked too close to the default state even though it represents a distinct near-ready calibration priority.

### 2026-03-20
- Area: `docs/crypto-pipeline-report.html`, `docs/crypto-pipeline-report.md`, `README.md`
- Change: Switched the near-threshold table's `Action` column from plain text to the same reusable `action-chip` rendering used by the hero urgency pill and headline action counts.
- Why: Keeping the table on raw text while the summary areas used chips made the offline report visually inconsistent and harder to scan when comparing the same `top-up-now / ready-soon / defer` actions across sections.

### 2026-03-20
- Area: `docs/crypto-pipeline-report.html`, `docs/crypto-pipeline-report.md`, `README.md`
- Change: Added chip rendering to the calibrate breakdown's `Skip Reason` table and highlighted `insufficient_samples` with the warning style so skipped calibration reasons now use the same lightweight status language as the near-threshold action summaries.
- Why: Once the offline viewer used chips for urgency and action hints, leaving calibrate skip reasons as plain text made the most important skipped reason harder to spot during sample-gap triage.

### 2026-03-20
- Area: `docs/crypto-pipeline-report.html`, `docs/crypto-pipeline-report.md`, `README.md`
- Change: Split the calibrate breakdown's top-underfilled `threshold_band` out of the plain `Need` text and render it as a dedicated action chip, so `near-threshold` and `far-from-threshold` are visually distinct inside the bucket summary table.
- Why: Keeping `threshold_band` buried in the same text blob as row and gap counts made one of the most important prioritization cues harder to scan than the rest of the report's chip-based urgency signals.

### 2026-03-20
- Area: `docs/crypto-pipeline-report.html`, `docs/crypto-pipeline-report.md`, `README.md`
- Change: Split `gap_to_min_samples` out of the calibrate breakdown's top-underfilled `Need` text and render it as a dedicated `gap N` chip alongside the existing `threshold_band` chip.
- Why: Once threshold bands became chips, leaving the sample gap embedded in prose still made the top-underfilled table slower to scan than the rest of the report's priority indicators.

### 2026-03-20
- Area: `docs/crypto-pipeline-report.html`, `docs/crypto-pipeline-report.md`, `README.md`
- Change: Switched the headline table's per-bucket near-threshold rows from `gap X (action)` text to the same `gap` badge plus `action-chip` rendering used elsewhere in the offline report.
- Why: The headline table was the last place still showing near-threshold bucket urgency as plain text instead of the chip-based visual language used across the rest of the report.

### 2026-03-20
- Area: `src/bin/crypto_pipeline_report.rs`, `docs/crypto-pipeline-report.html`, `docs/crypto-pipeline-report.md`, `README.md`
- Change: Added a top-level `ui_priority_summary` to the aggregate pipeline report JSON and taught the static offline viewer to prefer that precomputed headline/near-threshold status, badge text, and explainer data over re-deriving the same UI state client-side.
- Why: The offline report page had accumulated a parallel copy of the same urgency logic that already exists in the report generator, so the aggregate JSON should carry one canonical UI-priority summary instead of forcing the browser to recompute it.

### 2026-03-20
- Area: `src/bin/crypto_pipeline_report.rs`, `docs/crypto-pipeline-report.html`, `docs/crypto-pipeline-report.md`, `README.md`
- Change: Extended `ui_priority_summary` with structured `priority_source` and `headline_status_reason` fields, added regression coverage for the new fields, and let the static viewer attach those reasons to the status pills as tooltip metadata.
- Why: Once UI-priority state moved into aggregate JSON, the next gap was that consumers still had to infer the reason from free-form explainer text instead of reading an explicit structured cause.

### 2026-03-20
- Area: `src/bin/crypto_pipeline_report.rs`, `docs/crypto-pipeline-report.html`, `docs/crypto-pipeline-report.md`, `README.md`
- Change: Extended `ui_priority_summary` with explicit `top_up_now_labels` and `near_threshold_bucket_labels`, added regression coverage for the emitted label lists, and let the static viewer fold those labels into the status-pill tooltips.
- Why: Structured status reasons are still incomplete if consumers cannot see which concrete `asset_class / horizon / event_subtype` buckets triggered them without re-deriving that list from the rendered tables.

### 2026-03-20
- Area: `docs/crypto-pipeline-report.html`, `docs/crypto-pipeline-report.md`, `README.md`
- Change: Surfaced `ui_priority_summary` trigger labels as short `Triggered by ...` lines in both the hero and headline sections so the static offline report now exposes the active bucket labels directly instead of hiding them only in pill tooltips.
- Why: Once aggregate JSON carried the exact triggering bucket labels, leaving them only in tooltips still made the report less scannable than necessary for quick offline triage.

### 2026-03-20
- Area: `docs/crypto-pipeline-report.html`, `docs/crypto-pipeline-report.md`, `README.md`
- Change: Tightened the new trigger-label rows into compact `Triggered by` chip lines and collapsed overflow labels into a `+N more` chip instead of leaving them as plain prose.
- Why: After surfacing triggering bucket labels inline, the remaining issue was that raw text labels were still visually heavier and less consistent with the report's badge/chip language than they needed to be.

### 2026-03-20
- Area: `src/bin/crypto_pipeline_report.rs`, `docs/crypto-pipeline-report.md`, `README.md`
- Change: Extended markdown rendering to include the full `ui_priority_summary` block, including structured status fields and trigger label lists, and added regression coverage for the new markdown output.
- Why: Once the aggregate JSON and static viewer shared a canonical UI-priority summary, the markdown artifact still lagged behind and forced offline readers to infer status reasons from the headline section alone.

### 2026-03-20
- Area: `src/bin/crypto_prepare_calibration.rs`, `src/bin/crypto_calibrate.rs`, `README.md`, `docs/crypto-calibration-workflow.md`
- Change: Extended the offline crypto calibration pipeline so prepared samples now carry `asset_class` and `event_subtype`, preparation summaries count samples by asset class and event subtype, and `crypto_calibrate` can optionally group emitted override suggestions by `asset_class` and `event_subtype` via new CLI flags.
- Why: Once runtime subtype tuning moved fully into calibration overrides, the offline research flow also needed to express the same major/alt and event-subtype segmentation instead of only emitting per-asset probability shrink suggestions.

### 2026-03-20
- Area: `src/bin/crypto_seed_labels.rs`, `src/bin/crypto_pipeline_report.rs`, `docs/crypto-pipeline-report.html`, `docs/crypto-pipeline-report.md`, `docs/crypto-calibration-workflow.md`
- Change: Extended `crypto_seed_labels` summaries with `by_asset_class` and `by_event_subtype`, propagated the same richer distribution shape through `crypto_pipeline_report`, and updated the static offline viewer so seed/prepare breakdowns now show asset class and event subtype alongside the existing asset and market-type tables.
- Why: After `prepare` and `calibrate` gained asset-class and subtype awareness, the upstream seed step and the downstream offline report needed the same dimensions so the full calibration pipeline exposes one consistent segmentation model instead of only the middle stages understanding subtype buckets.

### 2026-03-20
- Area: `src/bin/crypto_autolabel_resolved.rs`, `src/bin/crypto_pipeline_report.rs`, `docs/crypto-pipeline-report.html`, `docs/crypto-pipeline-report.md`, `docs/crypto-calibration-workflow.md`
- Change: Extended `crypto_autolabel_resolved` summaries and unresolved rows with `asset_class` and `event_subtype`, then surfaced those distributions in the offline pipeline report so the autolabel stage now shows which subtype buckets are still blocked by open markets, missing winners, or request failures.
- Why: Once seed, prepare, and calibrate all understood major/alt and subtype buckets, the remaining blind spot was autolabel; operators need to know which event buckets are missing labels before they can trust or expand the override-generation workflow.

### 2026-03-20
- Area: `src/bin/crypto_calibrate.rs`, `src/bin/crypto_pipeline_report.rs`, `docs/crypto-calibration-workflow.md`, `docs/crypto-pipeline-report.md`
- Change: Added machine-readable segment grouping to `crypto_calibrate` via `--group-by-asset-class` and `--group-by-event-subtype`, documented the richer sample/report shape, and kept the offline reporting path aligned with the same major/alt and subtype dimensions.
- Why: The runtime crypto override table now tunes behavior by asset class and event subtype, so the calibration and reporting workflow should expose those same grouping knobs instead of stopping at per-asset probability shrink suggestions.

### 2026-03-20
- Area: `crates/pa-core/src/config.rs`, `crates/pa-strategy/src/crypto_alpha.rs`, `config/default.toml`, `README.md`, `frontend/src/components/ConfigSection.tsx`
- Change: Extended crypto calibration overrides with optional `min_edge_multiplier` and `max_spread_multiplier`, applied those overrides inside crypto entry-threshold calculation after the existing horizon/event static layers, surfaced the new columns in the config UI table, and moved the default alt unlock/regulatory entry-front-door tuning examples into the override table.
- Why: Event-aware calibration should not stop at sigma and size; if subtype tuning is migrating into the table, entry gating should be able to move with it so `alt unlock` and `alt regulatory` no longer depend on scattered static threshold fields for their front-door behavior.

### 2026-03-20
- Area: `frontend/src/components/ConfigSection.tsx`
- Change: Extended the derived crypto override-coverage summary so it now reports override-vs-static status for `min_edge` and `max_spread` in addition to `sigma` and `size`.
- Why: Once calibration overrides could influence entry thresholds, the migration-status UI needed to show front-door coverage too; otherwise operators would still have to infer whether a bucket's entry gating was table-driven or static.

### 2026-03-20
- Area: `frontend/src/components/ConfigSection.tsx`
- Change: Added a headline summary to the crypto override-coverage panel showing how many subtype buckets are fully migrated and listing the remaining `scope/subtype` buckets that still retain any static control path.
- Why: The detailed coverage table is useful, but once several buckets have already moved into calibration overrides operators need a faster summary of what migration work is left without reading every row.

### 2026-03-20
- Area: `docs/crypto-pipeline-report.html`, `docs/crypto-pipeline-report.md`, `README.md`, `docs/crypto-calibration-workflow.md`
- Change: Added a fully static offline HTML viewer for the `crypto_seed_labels`, `crypto_autolabel_resolved`, and `crypto_prepare_calibration` summary JSON files, and documented how to load the three local summaries directly in a browser without running the live monitor frontend.
- Why: The offline calibration pipeline already emits machine-readable JSON at each stage, so operators need a lightweight visual report path that does not require another CLI render step or a running web service.

### 2026-03-20
- Area: `src/bin/crypto_pipeline_report.rs`, `docs/crypto-pipeline-report.html`, `docs/crypto-pipeline-report.md`, `README.md`, `docs/crypto-calibration-workflow.md`
- Change: Extended `crypto_pipeline_report` with `--json-output` so it can emit one aggregate report JSON alongside markdown, added headline ratio data to the aggregate structure, and taught the static HTML viewer to accept that combined JSON directly while remaining backward-compatible with the original three separate summary files.
- Why: Once the offline browser viewer existed, the next step was to stop forcing operators to hand-load three separate summaries when the CLI can already assemble one canonical aggregate report for both human-readable and client-side consumption.

### 2026-03-20
- Area: `src/bin/crypto_pipeline_report.rs`, `docs/crypto-pipeline-report.html`, `docs/crypto-pipeline-report.md`, `README.md`, `docs/crypto-calibration-workflow.md`
- Change: Added `--html-output` to `crypto_pipeline_report`, which embeds the aggregate report JSON directly into the existing static viewer template to produce a single self-contained HTML artifact; the viewer now also auto-loads embedded report data when present.
- Why: After adding aggregate JSON output, the most practical next step was a one-command shareable report artifact so operators can hand around a finished offline calibration report without separately bundling JSON files.

### 2026-03-20
- Area: `src/bin/crypto_pipeline_report.rs`, `docs/crypto-pipeline-report.html`, `docs/crypto-pipeline-report.md`, `README.md`, `docs/crypto-calibration-workflow.md`
- Change: Added optional `--title` and `--subtitle` metadata to `crypto_pipeline_report`, propagated that metadata through markdown/JSON/HTML outputs, and taught the static viewer to render custom embedded report titles and subtitles when present.
- Why: Once the pipeline report became a shareable offline artifact, operators needed a way to label batches and date ranges consistently across every output format instead of renaming files and losing context inside the report itself.

### 2026-03-20
- Area: `src/bin/crypto_pipeline_report.rs`, `docs/crypto-pipeline-report.html`, `docs/crypto-pipeline-report.md`, `README.md`, `docs/crypto-calibration-workflow.md`
- Change: Added an automatic RFC3339 `generated_at_utc` field to pipeline report metadata, rendered it in markdown/JSON/HTML outputs, and surfaced the timestamp on the static offline viewer header.
- Why: Once report artifacts became shareable and batch-labeled, they also needed an explicit generation timestamp inside the content itself so archive history does not depend on external file metadata.

### 2026-03-20
- Area: `src/bin/crypto_pipeline_report.rs`, `docs/crypto-pipeline-report.html`, `docs/crypto-pipeline-report.md`, `README.md`, `docs/crypto-calibration-workflow.md`
- Change: Added optional `--notes` and `--notes-file` support to `crypto_pipeline_report`, stored the selected notes in report metadata, rendered a notes section in markdown, and surfaced batch notes on the static offline viewer when present.
- Why: Once reports carried titles, subtitles, and timestamps, the remaining gap was operator context such as label caveats or sample-selection notes that should travel inside the archived artifact instead of in a separate ad hoc text file.

### 2026-03-20
- Area: `src/bin/crypto_pipeline_report.rs`, `docs/crypto-pipeline-report.html`, `docs/crypto-pipeline-report.md`, `README.md`, `docs/crypto-calibration-workflow.md`
- Change: Added repeatable `--tag` support to `crypto_pipeline_report`, normalized/deduplicated tags into report metadata, rendered them in markdown output, and surfaced them as header pills on the static offline viewer.
- Why: After adding titles, timestamps, and notes, the remaining lightweight classification need was batch tagging so archived reports can carry compact machine-readable labels such as `replace-only`, `majors`, or `manual-review` inside the artifact itself.

### 2026-03-20
- Area: `src/bin/crypto_pipeline_report.rs`, `README.md`, `docs/crypto-calibration-workflow.md`, `docs/crypto-pipeline-report.md`
- Change: Added optional `--input-dir` support to `crypto_pipeline_report`, allowing it to auto-discover `crypto_seed_summary.json`, `crypto_autolabel_summary.json`, and `crypto_prepare_summary.json` from one batch directory while still letting explicit file arguments override the defaults.
- Why: Once the report CLI had accumulated more batch metadata flags, repeatedly typing three separate summary paths became unnecessary friction for the common case where all summary files already live in the same batch directory.

### 2026-03-20
- Area: `src/bin/crypto_pipeline_report.rs`, `README.md`, `docs/crypto-calibration-workflow.md`, `docs/crypto-pipeline-report.md`
- Change: Added optional `--output-dir` support to `crypto_pipeline_report`, which creates the target directory if needed and fills in standard `crypto_pipeline_report.{md,json,html}` filenames for any output paths not explicitly provided.
- Why: After `--input-dir` reduced repeated input-path typing, the matching output-side friction was still forcing operators to repeat three output filenames for the common batch-report case.

### 2026-03-20
- Area: `src/bin/crypto_pipeline_report.rs`, `crates/pa-strategy/src/crypto_alpha.rs`, `README.md`, `docs/crypto-calibration-workflow.md`
- Change: Made `crypto_pipeline_report` fail fast when all summary inputs are missing instead of silently emitting empty artifacts, and fixed crypto edge-decay confirmation cleanup to retain states for the true configured maximum confirmation window across horizon and severity multipliers rather than only the default/zero-severity paths.
- Why: The offline calibration pipeline should not let a missing-input batch masquerade as a valid empty report, and the edge-decay state cache should respect future non-default confirmation-window multipliers instead of pruning confirmations too aggressively.

### 2026-03-20
- Area: `crates/pa-core/src/config.rs`, `config/default.toml`, `crates/pa-monitor/src/diagnostics.rs`, `crates/pa-strategy/src/crypto_alpha.rs`, `frontend/src/api.ts`, `frontend/src/components/ConfigSection.tsx`, `frontend/src/pages/CryptoMarkets.tsx`, `README.md`
- Change: Moved crypto event subtype overlays (`unlock / upgrade / regulatory`) out of hardcoded strategy branches into explicit config fields for entry edge/spread, sigma, and size, and changed crypto candidate/exit diagnostics to prefer the real matched calendar event context (with title and source) while falling back to heuristic inference only when no calendar event matches.
- Why: Event subtype tuning should be adjustable from config instead of requiring code edits, and operator diagnostics should show the actual event context that drove strategy overlays instead of a potentially divergent text-only guess.

### 2026-03-19
- Area: `src/bin/crypto_pipeline_report.rs`, `README.md`, `docs/crypto-calibration-workflow.md`
- Change: Added a standalone `crypto_pipeline_report` CLI that reads the seed/autolabel/prepare summary JSON files and renders one combined markdown report, including a simple emitted-vs-seed headline ratio when both seed and prepare summaries are present.
- Why: Once each offline stage exposes machine-readable summaries, operators need a lightweight way to view the full calibration pipeline state in one place without manually comparing three separate JSON files.

### 2026-03-19
- Area: `src/bin/crypto_seed_labels.rs`, `README.md`, `docs/crypto-calibration-workflow.md`
- Change: Added an optional `--summary-output` JSON file to `crypto_seed_labels` so the deduplicated question count and by-asset/by-market-type distribution can be consumed programmatically, not just inferred from the emitted label skeleton.
- Why: With summary JSON already available for autolabel and prepare stages, the seed-label stage should expose the same machine-readable visibility to keep the offline workflow consistent end to end.

### 2026-03-19
- Area: `src/bin/crypto_autolabel_resolved.rs`, `README.md`, `docs/crypto-calibration-workflow.md`
- Change: Added an optional `--summary-output` JSON file to `crypto_autolabel_resolved` so its existing per-reason autolabel counts can be consumed programmatically as well as logged.
- Why: Once the autolabel step becomes part of a repeatable offline pipeline, the remaining unresolved-label counts should be easy to parse mechanically instead of only reading tracing output.

### 2026-03-19
- Area: `src/bin/crypto_prepare_calibration.rs`, `README.md`, `docs/crypto-calibration-workflow.md`
- Change: Added an optional `--summary-output` JSON file to `crypto_prepare_calibration` so the same coverage summary printed to stdout can also be consumed programmatically by notebooks or future UI tooling.
- Why: The preparation summary is useful operationally, but once calibration becomes a repeated workflow it should also be easy to parse mechanically instead of only scraping console text.

### 2026-03-19
- Area: `src/bin/crypto_prepare_calibration.rs`, `README.md`, `docs/crypto-calibration-workflow.md`
- Change: Added a preparation summary to `crypto_prepare_calibration` so it now reports total candidate rows, matched labels, emitted samples, missing/invalid labels, and emitted-sample counts by asset and market type after writing the JSONL output.
- Why: Before running `crypto_calibrate`, operators need a quick read on sample coverage and label quality instead of inferring it from output file size alone.

### 2026-03-19
- Area: `src/bin/crypto_autolabel_resolved.rs`, `README.md`, `docs/crypto-calibration-workflow.md`
- Change: Extended `crypto_autolabel_resolved` with per-reason summary accounting and an optional unresolved-output JSONL so open markets, missing-winner cases, and request failures can be reviewed separately instead of silently staying unlabeled.
- Why: Once auto-labeling starts covering part of the dataset, the remaining manual workload needs to be explicit and actionable rather than hidden inside an unchanged output file.

### 2026-03-19
- Area: `crates/pa-monitor/src/diagnostics.rs`, `crates/pa-strategy/src/crypto_alpha.rs`, `src/bin/crypto_seed_labels.rs`, `src/bin/crypto_autolabel_resolved.rs`, `README.md`, `docs/crypto-calibration-workflow.md`
- Change: Added `condition_id` to recent crypto candidate diagnostics and seeded label skeletons, then added a standalone `crypto_autolabel_resolved` CLI that queries the single-market CLOB endpoint by `condition_id` and auto-fills `resolved_yes` plus `resolution_at` for already closed markets.
- Why: The offline calibration flow should not require reconstructing market identity from question text when the runtime already knows the exact condition ID, and resolved markets should be labelable automatically instead of by hand.

### 2026-03-19
- Area: `src/bin/crypto_seed_labels.rs`, `README.md`, `docs/crypto-calibration-workflow.md`
- Change: Added a standalone `crypto_seed_labels` CLI that reads exported crypto diagnostics, de-duplicates them at question granularity, and writes a prefilled label-skeleton JSONL file with `question / asset / market_type` plus empty resolution fields.
- Why: The calibration-preparation workflow still assumed operators already had a label file, so the practical next step was generating a clean question list to annotate instead of manually reconstructing one from diagnostics output.

### 2026-03-19
- Area: `crates/pa-monitor/src/diagnostics.rs`, `crates/pa-strategy/src/crypto_alpha.rs`, `src/bin/crypto_prepare_calibration.rs`, `README.md`, `docs/crypto-calibration-workflow.md`
- Change: Enriched recent crypto candidate/exit diagnostics with calibration-relevant metadata such as modeled probability, held-side YES/NO, market type, and horizon, then added a standalone `crypto_prepare_calibration` CLI that joins exported candidate diagnostics with per-question resolution labels into `crypto_calibrate`-ready JSONL samples.
- Why: Exporting recent crypto diagnostics is only the first half of an offline calibration workflow; the research path also needs a deterministic way to turn runtime candidate records plus realized outcomes into actual `crypto_calibrate` input rows.

### 2026-03-19
- Area: `src/bin/crypto_export_diagnostics.rs`, `README.md`, `docs/crypto-calibration-workflow.md`
- Change: Added a standalone `crypto_export_diagnostics` CLI that fetches recent crypto candidate/exit diagnostics from the local monitor API and writes them as time-ordered JSONL, then documented how to use the exporter as the first step of an offline replay/calibration workflow.
- Why: The strategy now emits useful live crypto diagnostics, but operators still needed a simple way to preserve those buffered rows for later analysis without scraping logs or modifying the runtime path.

### 2026-03-19
- Area: `crates/pa-market-data/src/event_calendar.rs`, `crates/pa-core/src/config.rs`, `config/default.toml`, `crates/pa-strategy/src/crypto_alpha.rs`, `frontend/src/components/ConfigSection.tsx`, `README.md`
- Change: Added `matching_event()` to the event calendar and extended crypto event handling with category-aware `macro / crypto` sigma and size multipliers layered on top of the existing impact-tier controls, with focused regression coverage for the new category overlays.
- Why: Crypto event risk should not be driven only by `low/medium/high` impact because macro releases and crypto-native events have different implications for volatility and sizing even at the same nominal impact level.

### 2026-03-19
- Area: `crates/pa-strategy/src/crypto_alpha.rs`
- Change: Restricted the extra `macro` event-category sigma/size overlay to major crypto assets (`BTC` / `ETH`) while leaving alt assets on the existing impact-tier logic, and added focused regression coverage showing Solana no longer inherits the extra macro overlay.
- Why: Broad macro events should influence majors more directly than smaller alt markets, so applying the same extra macro penalty to every crypto asset was too blunt.

### 2026-03-19
- Area: `crates/pa-strategy/src/crypto_alpha.rs`
- Change: Added a lightweight crypto-event subtype layer inside the existing crypto category handling, so titles matching `unlock / vesting`, `upgrade / mainnet / fork`, or `ETF / listing / regulatory` now apply different extra sigma/size overlays on top of the generic crypto-event multiplier, with focused regression coverage for the unlock path.
- Why: Not all crypto-native events carry the same risk profile, and treating token unlocks exactly like generic ecosystem events was still too coarse once category-aware event handling was in place.

### 2026-03-19
- Area: `crates/pa-monitor/src/diagnostics.rs`, `crates/pa-strategy/src/crypto_alpha.rs`, `frontend/src/api.ts`, `frontend/src/pages/CryptoMarkets.tsx`
- Change: Extended recent crypto candidate decisions with lightweight inferred event context (`Macro / Crypto` plus subtype such as `unlock / upgrade / regulatory`) derived from candidate text, and surfaced the extra event column on the crypto decision table.
- Why: Once event-aware crypto logic became more nuanced, the frontend needed at least a lightweight explanation of what kind of event context the recent candidate decisions were reacting to instead of only showing pure execution metrics.

### 2026-03-19
- Area: `crates/pa-strategy/src/crypto_alpha.rs`
- Change: Extended crypto entry-threshold tightening so `unlock`, `upgrade`, and `regulatory` crypto-event subtypes now further adjust `min_edge` and `max_spread` on top of the existing impact-tier event controls, with focused regression coverage for the unlock path.
- Why: Event subtypes were already affecting `sigma` and sizing, but leaving entry thresholds unchanged still let some higher-risk crypto-native windows pass too easily at the front door.

### 2026-03-19
- Area: `crates/pa-monitor/src/diagnostics.rs`, `crates/pa-monitor/src/api.rs`, `crates/pa-strategy/src/crypto_alpha.rs`, `frontend/src/api.ts`, `frontend/src/pages/CryptoMarkets.tsx`
- Change: Added a recent crypto exit-decision buffer and `/api/crypto/exits`, recorded `capital_efficiency / relative_stop_loss / model_reversal / edge_decay` exits with lightweight event context, and surfaced a small recent-exits table on the crypto page.
- Why: Entry-side event diagnostics were already visible, but operators still lacked a compact way to see which exit reasons were firing lately and whether those exits were happening inside macro or crypto-native event windows.

### 2026-03-19
- Area: `frontend/src/pages/CryptoMarkets.tsx`
- Change: Added a lightweight toggle to the recent crypto candidate decision table so the page defaults to showing only `replace` decisions, with an option to reveal `seed` rows when operators want the full same-asset bucket history.
- Why: Once the frontend started showing recent candidate decisions, `seed` rows could drown out the more interesting competitive replacements, so the page should default to the higher-signal subset while keeping full visibility one click away.

### 2026-03-19
- Area: `frontend/src/pages/CryptoMarkets.tsx`
- Change: Added an asset filter to the recent crypto candidate decision table, populated from the assets present in the buffered decision rows, so operators can narrow the view to `BTC`, `ETH`, or any other asset that has recent same-asset competition activity.
- Why: Even after hiding `seed` rows by default, the decision table can still mix multiple assets together, so a quick asset-level filter makes it much faster to inspect one coin's candidate replacement behavior at a time.

### 2026-03-19
- Area: `frontend/src/pages/CryptoMarkets.tsx`
- Change: Added a direction filter to the recent crypto candidate decision table so operators can isolate `Up`, `Down`, `InsideRange`, or `OutsideRange` replacement activity independently of the asset filter.
- Why: Same-asset candidate competition is often direction-specific, so narrowing the table by direction makes it easier to inspect exactly one risk bucket instead of mixing opposite-side opportunities together.

### 2026-03-19
- Area: `frontend/src/pages/CryptoMarkets.tsx`
- Change: Added a sort-mode selector to the recent crypto candidate decision table so operators can switch between `最新优先` and `效率差值`, where the latter ranks rows by the executable-efficiency advantage of the kept candidate over the replaced one.
- Why: Time order is useful for live monitoring, but post-trade diagnosis often benefits more from seeing the largest execution-quality wins first instead of only the most recent replacements.

### 2026-03-19
- Area: `frontend/src/pages/CryptoMarkets.tsx`
- Change: Added row-level highlighting for recent crypto `replace` decisions based on executable-efficiency delta, plus a small legend describing the current high/medium thresholds.
- Why: Once the table can sort by efficiency delta, a matching visual cue makes the most meaningful candidate replacements immediately obvious without forcing operators to read each row's numbers first.

### 2026-03-19
- Area: `frontend/src/pages/CryptoMarkets.tsx`
- Change: Added a compact summary strip above the recent crypto candidate decision table, showing the number of visible decisions, the number of `replace` rows, and the average executable-efficiency delta for the visible replacements.
- Why: After adding filters and ranking to the decision table, operators need a quick aggregate read on how much meaningful candidate competition is happening without scanning every row.

### 2026-03-19
- Area: `frontend/src/pages/CryptoMarkets.tsx`
- Change: Extended the recent crypto candidate decision summary strip with a `最大效率差值` badge alongside the existing average, so the table now exposes both the typical and the strongest visible execution-quality replacement.
- Why: Average efficiency delta is useful but can hide one especially meaningful candidate replacement, so operators also need the peak delta to quickly spot the strongest recent execution-quality win.

### 2026-03-19
- Area: `frontend/src/pages/CryptoMarkets.tsx`
- Change: Added click-through focus from the recent crypto candidate decision table into the asset-aggregation and current-position tables, automatically highlighting matching `asset + direction` rows and expanding the related aggregate bucket until the focus is cleared.
- Why: Once the frontend can explain why a candidate won, the next useful step is linking that decision back to current exposure so operators can immediately see which live positions sit in the same risk bucket.

### 2026-03-19
- Area: `crates/pa-strategy/src/engine.rs`
- Change: Changed execution-freshness validation for buy orders so opportunities that still price within limit but no longer have full visible ask depth are now scaled down to current executable depth and repriced conservatively before profit refresh, instead of being rejected outright.
- Why: Crypto and weather opportunities can remain attractive even when only part of the original size is still executable at the limit, so outright rejection was unnecessarily wasting still-viable fills.

### 2026-03-19
- Area: `crates/pa-strategy/src/engine.rs`
- Change: For non-weather strategies, changed the engine to run cooldown/event/depth/freshness preparation before ranking opportunities, then sort the prepared opportunities by refreshed profit efficiency before execution; added focused regression coverage for the prepared-ordering path and for avoiding budget consumption on risk rejection.
- Why: Scan-time ordering can become stale once execution-time depth and price adjustments shrink or reprice a crypto candidate, so non-weather opportunities should compete using their post-freshness economics instead of their original raw scan output.

### 2026-03-19
- Area: `crates/pa-strategy/src/crypto_alpha.rs`
- Change: Changed same-asset crypto entry dedupe to prefer the candidate with better execution-adjusted profit efficiency based on current order-book walk, depth, and slippage before falling back to raw estimated profit, and added focused regression coverage for the new executable-ranking path.
- Why: Even before the engine revalidates freshness, two same-asset crypto candidates can have very different real execution quality, so strategy-local dedupe should not keep the higher raw-profit candidate when its live book makes it materially less executable.

### 2026-03-19
- Area: `crates/pa-strategy/src/crypto_alpha.rs`
- Change: Added debug logs for same-asset crypto candidate seeding and replacement decisions, including raw profit, static efficiency, execution-adjusted efficiency, and depth-buffer comparisons whenever one candidate displaces another.
- Why: Once crypto dedupe started using execution-quality signals, operators needed a direct way to see why a BTC/ETH candidate survived or was replaced without reverse-engineering the ranking from raw opportunity snapshots.

### 2026-03-19
- Area: `crates/pa-monitor/src/diagnostics.rs`, `crates/pa-monitor/src/api.rs`, `crates/pa-strategy/src/crypto_alpha.rs`, `frontend/src/api.ts`, `frontend/src/pages/CryptoMarkets.tsx`
- Change: Added an in-process recent crypto candidate decision buffer, exposed it via `/api/crypto/decisions`, recorded same-asset `seed` / `replace` decisions directly from `crypto_alpha`, and surfaced the latest decision rows on the crypto page with selected/replaced questions plus efficiency and depth-buffer comparisons.
- Why: Debug logs alone explain candidate replacement one line at a time, but operators also need a lightweight frontend view of the most recent crypto dedupe decisions without depending on Prometheus or digging through logs.

### 2026-03-19
- Area: `crates/pa-core/src/config.rs`, `config/default.toml`, `README.md`, `frontend/src/components/ConfigSection.tsx`, `crates/pa-strategy/src/crypto_alpha.rs`
- Change: Added `crypto_alpha.min_entry_depth_ratio` and enforced it in crypto entry sizing so a candidate now needs enough order-book depth at the chosen limit price before it can pass into execution, with focused regression coverage for thin-book rejection.
- Why: Discovery quality had improved, but some crypto candidates were still only attractive on paper because the visible ask depth was too thin relative to the intended order size.

### 2026-03-19
- Area: `crates/pa-market-data/src/gamma_feed.rs`
- Change: Tagged merged crypto search terms with `default` vs `custom` sources, included the source in per-term search completion logs, and added a discovery-pass summary showing how many new markets each custom search term contributed.
- Why: Once discovery terms became configurable, operators needed feedback on whether their custom phrases were actually expanding crypto market coverage or just duplicating the built-in defaults.

### 2026-03-19
- Area: `crates/pa-core/src/config.rs`, `config/default.toml`, `README.md`, `frontend/src/components/ConfigSection.tsx`, `crates/pa-market-data/src/gamma_feed.rs`
- Change: Added `crypto_alpha.discovery_search_terms` so operators can append custom Gamma search phrases on top of the shared crypto discovery term set, wired the merged list into `GammaFeed`, and documented the new read-only config field.
- Why: Crypto discovery keywords should be extensible at runtime because new tradable assets and nonstandard market titles appear faster than code deploys.

### 2026-03-19
- Area: `crates/pa-market-data/src/gamma_feed.rs`
- Change: Added a per-discovery-pass crypto relevance summary log that counts how many markets were admitted via each relevance path, while keeping the detailed per-market debug reasons for title/category-driven matches.
- Why: Discovery tuning needs both per-market explanations and a compact roll-up showing whether the scan is mostly succeeding via direct question matches or through the newer event-title/category fallback paths.

### 2026-03-19
- Area: `crates/pa-market-data/src/gamma_feed.rs`
- Change: Added explicit crypto discovery relevance reasons (`question`, `event_title`, `category+crypto_text`) and debug logging when crypto markets are admitted through title/category paths instead of only the raw question text.
- Why: After broadening crypto market discovery coverage, operators need a direct way to see why a market was included so discovery tuning does not turn into guesswork.

### 2026-03-19
- Area: `crates/pa-core/src/crypto.rs`, `crates/pa-core/src/lib.rs`, `crates/pa-market-data/src/gamma_feed.rs`
- Change: Added shared crypto discovery metadata plus reusable search-term and text-matching helpers, expanded Gamma crypto search coverage to include major and alt asset keywords, and taught market relevance filtering to consider `event_title` and `category` in addition to the market question.
- Why: Crypto market discovery was still biased toward a few BTC/ETH search phrases and question-only matching, which risked missing alt markets and grouped crypto events whose asset signal lives mainly in the event title.

### 2026-03-19
- Area: `src/app/tasks.rs`
- Change: Changed the weather forecast snapshot archiver to archive all non-NOAA cities by provider instead of filtering on `!trade_enabled`, so London continues to persist PostgreSQL weather snapshots after being enabled for conservative live trading while Seoul remains archived as before.
- Why: The previous archive filter silently stopped snapshotting London as soon as it became trade-enabled, which broke the intended London/Seoul international weather replay path even though both cities still need persisted forecast archives.

### 2026-03-19
- Area: `crates/pa-core/src/weather.rs`, `crates/pa-strategy/src/weather.rs`, `crates/pa-monitor/src/api.rs`
- Change: Enabled London as a trade-enabled weather city, kept Seoul audit-only, expanded the conservative city overlay to include London so its live entry thresholds and sizing stay tighter than the standard validated NOAA cities, and aligned shared trade-enabled city metadata plus tests/UI config metadata with the new London status.
- Why: London now has a higher-confidence Met Office audit stack than before, so it can be opened as a small-step gray rollout, but it still needs more conservative live trading behavior than the long-running U.S. weather cities.

### 2026-03-19
- Area: `crates/pa-strategy/src/weather.rs`, `src/app/helpers.rs`
- Change: Removed the now-dead weather entry-window gating branches from `WeatherAlphaStrategy::scan()` after reverting to all-day weather entries, and changed WS token dedupe inside `build_ws_token_list()` from repeated `Vec::contains` scans to a `HashSet` while preserving the same ordering semantics.
- Why: Once the weather entry window was removed, the scan path no longer needed to pretend there was a runtime gate, and the tightened WS ranking logic should use stable O(1) dedupe instead of repeated linear membership checks as weather market counts keep growing.

### 2026-03-19
- Area: `crates/pa-core/src/weather.rs`, `crates/pa-monitor/src/api.rs`, `frontend/src/pages/WeatherStrategy.tsx`, `frontend/src/components/ConfigSection.tsx`, `frontend/src/api.ts`
- Change: Removed the UTC+8 weather-entry session restriction by making the shared `weather_entry_window_open_*` helpers unconditional, deleting the weather-entry-window runtime status field from `/api/status`, and removing the related frontend trading-window messaging and run-context display.
- Why: The user decided to stop constraining new weather entries to the previous midnight-to-morning UTC+8 session, so the strategy, API, and UI all need to return to an always-open weather entry model instead of continuing to expose stale window semantics.

### 2026-03-19
- Area: `src/app/helpers.rs`
- Change: Split NegRisk weather tokens into a separate WS subscription budget instead of letting them continue to spill into the main weather subscription pool, capping NegRisk additions to the smaller of `80` tokens or `25%` of `ws_max_instruments`, with focused regression coverage for the cap.
- Why: Even after ranking lower-quality markets down, NegRisk tokens were still being re-added wholesale at the end of the WS list, which let them crowd out higher-value standard weather binaries and kept unnecessary pressure on the Polymarket WebSocket connection.

### 2026-03-19
- Area: `config/default.toml`, `README.md`, `CLAUDE.md`, `frontend/src/components/ConfigSection.tsx`
- Change: Lowered the default `market_filter.ws_max_instruments` setting from `500` to `350` and updated operator-facing docs/UI hints to match the new recommended WebSocket subscription ceiling.
- Why: After prioritizing higher-quality weather markets in the WS token list, the next practical step to reduce Polymarket WebSocket reset pressure is to stop filling the connection all the way to 500 instruments by default.

### 2026-03-19
- Area: `src/app/helpers.rs`
- Change: Reworked weather WS token ordering so subscriptions still prioritize held tokens first, but discovered weather markets are now ranked by settlement-validation status, Chicago/default-protected conservatism, non-NegRisk preference, mid-range price quality, and higher liquidity before truncating to `ws_max_instruments`; added focused regression coverage showing validated weather cities outrank default-protected ones.
- Why: The old WS builder mostly filled the 500-token cap by midpoint distance, which over-subscribed lower-quality weather markets and made the WebSocket feed carry more low-value load than necessary during weather-only trading.

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

### 2026-03-20
- Area: `src/bin/crypto_calibrate.rs`, `docs/crypto-calibration-workflow.md`, `README.md`
- Change: Added `--override-output` to `crypto_calibrate`, extracted override TOML rendering into a reusable helper, and enabled the CLI to write merge-ready `crypto_alpha.calibration_overrides` fragments directly to disk in addition to stdout.
- Why: The crypto strategy's next bottleneck is parameter data flow, and requiring operators to manually copy TOML blocks from stdout slows the handoff from offline calibration into runtime config review.

### 2026-03-20
- Area: `src/bin/crypto_calibrate.rs`, `docs/crypto-calibration-workflow.md`, `README.md`
- Change: Added `--existing-overrides-input` to `crypto_calibrate`, implemented exact-selector merge logic against existing `crypto_alpha.calibration_overrides` rows, preserved non-probability fields during merges, and added regression coverage for the merge behavior.
- Why: Generating a fresh override fragment is useful, but the practical operator flow is usually “update probability calibration inside an existing config without clobbering hand-tuned sigma/size/entry multipliers.”

### 2026-03-20
- Area: `src/bin/crypto_calibrate.rs`, `docs/crypto-calibration-workflow.md`, `README.md`
- Change: Added explicit `--merge-mode` support to `crypto_calibrate` with `probability-only`, `replace-row`, and `append-only` behaviors, kept `probability-only` as the default, and added focused regression tests for all three modes.
- Why: Once merge-against-existing-config existed, operators still needed a deliberate way to choose whether calibration should patch only probability factors, replace entire rows, or append review-only duplicates.

### 2026-03-20
- Area: `src/bin/crypto_calibrate.rs`, `docs/crypto-calibration-workflow.md`, `README.md`
- Change: Added merge diff summaries to `crypto_calibrate` output, including `new_rows`, `updated_rows`, and `unchanged_rows` counts plus selector comments, and added regression coverage for the new merge-review output.
- Why: Merge-ready override output is still cumbersome to review if operators must visually diff the whole TOML fragment instead of first seeing which selectors actually changed.

### 2026-03-20
- Area: `src/bin/crypto_calibrate.rs`, `docs/crypto-calibration-workflow.md`, `README.md`
- Change: Extended `crypto_calibrate_summary.json` with an optional machine-readable `merge_diff_summary` section so merge runs expose the same `new/updated/unchanged` row summary through JSON as well as TOML comments.
- Why: Once merge output carried a human-readable diff header, the next step was making that same review signal available to downstream report tooling without scraping comments from the generated TOML.
