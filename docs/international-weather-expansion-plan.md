# International Weather Expansion Plan

This document defines a staged plan to expand the weather strategy beyond the
current US/NOAA-only market set. The international audit path now uses
`MetOffice` for London and `Kma` for Seoul, with both cities still remaining
audit-only.

## Goal

- Expand weather market coverage beyond NOAA-supported US cities
- Preserve the current conservative execution standard
- Keep settlement-source mismatch explicit in the model
- Roll out one provider and one or two cities first, not a global switch

## Current State

- Runtime weather strategy only trades cities supported by
  [crates/pa-core/src/weather.rs](/home/yangyang/workspace/polygon/PolyAlpha/crates/pa-core/src/weather.rs)
- Settlement-risk adjustments currently exist only for NOAA-supported cities
- Frontend and backend weather metadata are built around canonical NOAA city names
- Rejection metrics show `unsupported_city` is still the dominant reason

## Recommendation

Use provider-specific official forecast sources for the initial international
audit layer.

Why:

- Better alignment with local national weather agencies
- Stronger audit confidence before any future trade enablement
- Still compatible with PostgreSQL forecast snapshot archiving for replay

## Rollout Scope

Phase 1:

- London
- Seoul
- Highest temperature markets only
- Binary and NegRisk range markets only

Out of scope for phase 1:

- Rainfall
- Snowfall
- Wind
- Storm / extreme-event markets
- Any city without a verified settlement-rule sample

## Architecture Changes

### 1. Introduce provider abstraction

Create a provider-neutral weather source interface in strategy code:

- `supports_location(location) -> bool`
- `fetch_forecast(location, metric, target_date) -> ForecastSnapshot`
- `historical_forecast(...)` optional / best-effort
- `provider_name()`

Then implement:

- `NoaaWeatherProvider`
- `MetOfficeWeatherProvider`
- `KmaWeatherProvider`

The strategy should choose the provider by city, not by market.

### 2. Move location metadata out of NOAA-only naming

Current shared weather metadata should evolve into a provider-aware registry,
for example:

- canonical city name
- aliases
- provider
- lat/lon
- settlement risk tier
- settlement notes

Example conceptual shape:

```text
CityWeatherConfig {
  canonical_name,
  aliases[],
  provider: Noaa | MetOffice | Kma,
  lat,
  lon,
  risk_tier,
  enabled,
}
```

### 3. Keep settlement-aware sigma, but make it provider-aware

Current settlement risk is only:

- `Low`
- `Medium`
- `High`

That structure is still fine, but it should no longer be named only around NOAA.
It should become generic settlement/model mismatch risk.

Suggested phase-1 defaults:

- Verified US NOAA cities: `Medium`
- London: `High`
- Seoul: `High`

After settlement-rule verification:

- London can move to `Medium`
- Seoul can move to `Medium`

### 4. Restrict international rollout to temperature markets first

Temperature markets are the cleanest to map from forecast mean + sigma into
event probability. Do not expand the first international provider to rain/snow
until settlement-source alignment is checked.

## Settlement-Risk Rules

Phase-1 international cities must not be enabled for trading until all of the
following are true:

1. A real Polymarket rules page sample is captured
2. Settlement source is identified
3. Local-day window is understood
4. We know whether the market settles to airport observation, city station, or
   another weather source

Until then:

- city may appear in audit tooling
- city should not be trade-enabled by default

## Implementation Plan

### Stage 0. Audit-only metadata

- Add `London` and `Seoul` to a new provider-aware city registry
- Mark them `enabled = false`
- Add provider metadata to frontend configuration meta
- Do not let strategy trade them yet

Deliverables:

- provider-aware city registry
- frontend risk/provider display
- audit doc updated with `London` / `Seoul`

### Stage 1. Official provider clients

- Add `MetOfficeClient` for London
- Add `KmaClient` for Seoul
- Support:
  - daily max temperature
  - daily min temperature
  - daily average temperature where available
- Normalize units to the same internal representation as current weather logic
- Reuse the existing probability path after forecast normalization

Deliverables:

- provider client implementations
- tests for location support and response normalization

### Stage 2. Provider routing

- Choose provider by canonical city metadata
- Keep NOAA path unchanged for existing US cities
- Route London to `MetOffice`
- Route Seoul to `Kma`

Deliverables:

- strategy path can fetch from either provider
- metrics include provider label

### Stage 3. Settlement-aware rollout

- Keep `London` and `Seoul` disabled-by-default cities
- Maintain audit-only replay for:
  - forecast
  - archived forecast snapshot
  - actuals
- Revisit trade enablement only after:
  - checklist sample verified
  - sigma multiplier assigned
  - replay evidence shows acceptable forecast-vs-actual behavior

Deliverables:

- config flag per city or provider
- frontend labels showing disabled-for-trading vs supported-for-audit

## Metrics and Observability

Add provider labels to weather diagnostics where useful:

- `weather_rejections_total{provider="noaa|met_office|kma",reason="..."}`
- forecast fetch success/failure counters by provider
- opportunities generated by provider
- executions by provider

Add frontend weather-page context:

- provider for each supported city
- whether city is audit-only or trade-enabled

## Risk Controls

For phase 1 international rollout:

- keep `max_position_usdc` unchanged or lower for international cities
- do not relax `max_spread_bps`
- require verified bid support on tradable side
- keep settlement mismatch sigma conservative

Suggested initial international overrides if trading is ever enabled later:

- `max_position_usdc = 3.0`
- `min_edge_bps = 800`
- `max_entry_price = 0.25`

These can later be moved into city/provider-specific overrides if results are
acceptable.

## Frontend Changes

Configuration page should eventually show:

- city
- provider
- settlement risk tier
- trade-enabled vs audit-only

Weather page should eventually show:

- opportunities by provider
- rejections by provider
- current positions grouped by provider

## Suggested Execution Order

1. Build provider-aware city registry
2. Add official provider clients
3. Route forecast fetch by provider
4. Keep London/Seoul audit-only
5. Add actuals and PostgreSQL archive replay
6. Verify settlement rules
7. Observe replay and live rejection behavior
8. Only then consider enabling one city first

## Practical Recommendation

Do not enable both London and Seoul for live trading immediately.

Recommended order:

1. London audit-only with Met Office forecast/actual + PostgreSQL archive
2. Seoul audit-only with KMA forecast/actual + PostgreSQL archive
3. Only discuss live trading after replay evidence is strong
4. Seoul live

This keeps the expansion controlled and makes it easier to separate:

- provider integration bugs
- settlement mismatch
- liquidity quality problems

## London Upgrade Path

London is now the first international city on the official-provider audit path.

Recommended provider:

- `Met Office Weather DataHub`

Reason:

- Most NOAA-like official forecast source for the UK
- Better long-term fit than keeping London on an aggregator
- Cleaner separation between:
  - official UK forecast source
  - airport-station settlement truth (`EGLC`)

Suggested rollout steps:

1. Keep London `audit-only`
2. Use `MetOfficeClient` for live temperature forecast
3. Use `Met Office Land Observations` for actuals
4. Persist forecast snapshots in PostgreSQL for archive replay
5. Compare:
   - live Met Office forecast
   - archived forecast snapshot
   - airport/site actuals
6. Re-score London settlement/model mismatch risk
7. Only then consider `trade_enabled = true`

Trading gate for London:

- settlement rule verified
- `EGLC` station mapping stable
- Met Office forecast and observations paths implemented
- replay evidence shows stable archive-vs-actual behavior
- conservative city override applied:
  - `max_position_usdc = 3.0`
  - `min_edge_bps = 800`
  - `max_entry_price = 0.25`

## Seoul Upgrade Path

Seoul should remain the second international rollout city.

Recommended provider:

- `KMA API Hub`
- `Open MET Data Portal`

Reason:

- Most NOAA-like official source for Korea
- Better local forecast authority than staying on an aggregator
- PostgreSQL snapshot archive fills the historical-forecast gap until an
  official KMA archive path is confirmed

Extra caution:

- The current settlement sample points to `RKSI` (Incheon), not central Seoul
- Spatial mismatch is likely larger than London

So even after a KMA client exists, Seoul should still be treated as higher-risk
than London until replay and settlement comparison show otherwise.
