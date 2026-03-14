# Weather Settlement Validation Plan

## Goal
- Expand weather-market settlement consistency handling from the currently validated subset of cities to the remaining trade-enabled cities.
- Promote cities from `DefaultProtected` to `Validated` only after their Polymarket settlement source, timezone, and station mapping have been checked.
- Keep strategy behavior conservative for unvalidated cities until their settlement path is confirmed.

## Current State

Validated cities today:
- `New York`
- `Chicago`
- `Miami`
- `Seattle`
- `Dallas`
- `Denver`
- `Atlanta`

Default-protected trade-enabled cities today:
- `Los Angeles`
- `Houston`
- `Phoenix`
- `Philadelphia`
- `San Antonio`
- `San Diego`
- `Austin`
- `San Francisco`
- `Nashville`
- `Portland`
- `Las Vegas`
- `Minneapolis`
- `Tampa`
- `New Orleans`
- `Cleveland`

Audit-only international cities:
- `London`
- `Seoul`

## Validation Outcome Definition

A city can move from `DefaultProtected` to `Validated` only when all of the following are true:
- A recent Polymarket market page has been checked manually.
- The exact settlement wording has been copied or summarized.
- The final settlement station has been identified.
- The station timezone matches the city metadata.
- The settlement note in shared weather metadata has been updated.
- The residual mismatch risk is understood and acceptable for the current strategy.

## Required Checks Per City

For each city, collect and record:
- One or more recent Polymarket weather market URLs.
- The exact market type:
  - highest temperature
  - lowest temperature
  - other
- The settlement source wording from Polymarket.
- The implied observation station and code.
- The station timezone.
- Whether the current shared weather metadata matches:
  - timezone
  - settlement note
  - provider
- Main mismatch risk:
  - station vs NOAA grid
  - local-day window
  - airport microclimate
  - coastal/elevation effects

## Rollout Batches

Batch ordering is a planning default, not a hard gate.

Operational rule:
- If a city outside the current batch has a direct Polymarket rules page available and can be verified cleanly, it may be promoted ahead of batch order.
- If a current-batch city cannot be verified because the rules page is not publicly discoverable, it should remain `DefaultProtected` and validation effort should move to the next city with better primary-source availability.
- Primary-source availability takes precedence over the original batch order.

### Batch 1
Highest expected value based on liquidity and likely station clarity.

- `Philadelphia`
- `San Francisco`
- `Las Vegas`
- `Austin`
- `Phoenix`
- `Minneapolis`

Expected output:
- Fill checklist rows
- Add settlement notes
- Reclassify any fully verified city to `Validated`

Batch 1 current investigation targets:
- `Philadelphia` -> candidate station `KPHL`
- `San Francisco` -> candidate station `KSFO`
- `Las Vegas` -> candidate station `KLAS`
- `Austin` -> candidate station `KAUS`
- `Minneapolis` -> candidate station `KMSP`

Batch 1 verified progress:
- `Phoenix` -> verified against direct Polymarket rules page; settlement station `KPHX`

Rule for this phase:
- Candidate stations may be recorded in docs as investigation targets.
- No city should be promoted to `Validated` until a direct Polymarket rules page confirms the station and resolution wording.

Current blocker:
- Public Polymarket/Gamma search does not reliably surface historical weather pages for several first-batch cities.
- If direct rules pages cannot be found from public search, the city must remain `DefaultProtected` until a live or archived primary-source market page is captured manually.

Fallback execution rule:
- Continue promoting cities opportunistically whenever a directly verifiable rules page is available, even if that means validating a later-batch city before an earlier-batch city.

Observed active-market snapshot on 2026-03-14:
- Active weather pages surfaced by `weather_audit` currently cover:
  - `Atlanta`
  - `Chicago`
  - `Dallas`
  - `Miami`
  - `New York`
  - `Seattle`
  - plus audit-only `London` and `Seoul`
- The remaining first-batch default-protected NOAA cities did not appear in the active sample on that date.
- As a result, no additional first-batch city could be promoted from active primary-source evidence during that pass.

### Batch 2
Useful follow-up cities with moderate expected strategy value.

- `Nashville`
- `Portland`
- `Tampa`
- `New Orleans`
- `Cleveland`
- `San Diego`

### Batch 3
Lower priority or more ambiguous station-mapping cities.

- `Los Angeles`
- `Houston`
- `San Antonio`

## Code Changes Required Per Verified City

When a city is fully verified, update:
- `crates/pa-core/src/weather.rs`
  - `settlement_note`
  - `settlement_validation_status`
  - timezone only if needed
- `docs/weather-noaa-settlement-checklist.md`
  - checklist row
  - source URL
  - risk assessment
- `AGENTS.md`
  - add a brief change-log entry

If verification changes strategy behavior, also check:
- `crates/pa-strategy/src/weather.rs`
  - no extra city-local hardcoding was introduced
- `frontend/src/components/ConfigSection.tsx`
  - badges/rendering still match backend metadata semantics

## Strategy Policy While Validation Is Incomplete

- Keep unvalidated cities in `DefaultProtected`.
- Continue applying the extra settlement edge buffer to those cities.
- Do not remove the protection buffer before documentation and metadata are updated.
- If a city shows repeated mismatch risk after review, keep it `DefaultProtected` even if a station is identified.

## Suggested Execution Loop

For each batch:
1. Collect 1 to 3 recent market pages per city.
2. Update the checklist with settlement wording and station code.
3. Update shared metadata for cities that are clearly verified.
4. Run:
   - `cargo test -q -p pa-core`
   - `cargo test -q -p pa-strategy weather`
   - `cargo test -q -p pa-monitor --no-run`
5. Observe live weather rejection and order behavior for 1 to 3 days before promoting the next batch.

## Exit Criteria

This plan is complete when:
- Every trade-enabled NOAA city has either:
  - `Validated`, or
  - an explicit documented reason to remain `DefaultProtected`
- The checklist is filled for all trade-enabled cities.
- Frontend metadata for each city reflects its final validation state.
