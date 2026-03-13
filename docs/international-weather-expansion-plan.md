# International Weather Expansion Plan

This document defines a staged plan to expand the weather strategy beyond the
current US/NOAA-only market set. The recommended first international provider
is Open-Meteo, with London and Seoul as the initial rollout cities.

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

Use Open-Meteo as the first non-US provider layer.

Why:

- Global coverage with straightforward lat/lon APIs
- Good fit for forecast-based pricing, especially temperature markets
- Easier to integrate than direct model-provider feeds
- Easier to backtest than more opaque or UI-oriented sources

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
- `OpenMeteoWeatherProvider`

The strategy should choose the provider by city, not by market.

### 2. Move location metadata out of NOAA-only naming

Current shared weather metadata should evolve from:

- `NOAA_SUPPORTED_LOCATIONS`

to a provider-aware registry, for example:

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
  provider: Noaa | OpenMeteo,
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

### Stage 1. Open-Meteo client

- Add `OpenMeteoClient`
- Support:
  - daily max temperature
  - daily min temperature
- Normalize units to the same internal representation as current weather logic
- Reuse the existing probability path after forecast normalization

Deliverables:

- client implementation
- tests for location support and response normalization

### Stage 2. Provider routing

- Choose provider by canonical city metadata
- Keep NOAA path unchanged for existing US cities
- Add feature-gated routing for London / Seoul

Deliverables:

- strategy path can fetch from either provider
- metrics include provider label

### Stage 3. Settlement-aware rollout

- Add `London` and `Seoul` as disabled-by-default cities
- Enable only after:
  - checklist sample verified
  - sigma multiplier assigned
  - one dry-run observation window completed

Deliverables:

- config flag per city or provider
- frontend labels showing disabled-for-trading vs supported-for-audit

## Metrics and Observability

Add provider labels to weather diagnostics where useful:

- `weather_rejections_total{provider="noaa|open_meteo",reason="..."}`
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

Suggested initial international overrides:

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
2. Add Open-Meteo client
3. Route forecast fetch by provider
4. Keep London/Seoul audit-only
5. Verify settlement rules
6. Enable one city first
7. Observe real rejection/execution behavior
8. Enable second city

## Practical Recommendation

Do not enable both London and Seoul for live trading immediately.

Recommended order:

1. London audit-only
2. London live with conservative sizing
3. Seoul audit-only
4. Seoul live

This keeps the expansion controlled and makes it easier to separate:

- provider integration bugs
- settlement mismatch
- liquidity quality problems

## London Upgrade Path

After the current Open-Meteo audit-only stage, London should be the first
international city upgraded to a more official source.

Recommended provider:

- `Met Office Weather DataHub`

Reason:

- Most NOAA-like official forecast source for the UK
- Better long-term fit than keeping London on an aggregator
- Cleaner separation between:
  - official UK forecast source
  - airport-station settlement truth (`EGLC`)

Suggested rollout steps:

1. Keep London on `Open-Meteo` for replay and audit only
2. Confirm DataHub access model, quotas, and auth requirements
3. Build `MetOfficeClient` for temperature daily max/min first
4. Compare:
   - live Met Office forecast
   - Open-Meteo forecast
   - airport historical actual (`EGLC`)
5. Re-score London settlement/model mismatch risk
6. Only then consider `trade_enabled = true`

Trading gate for London:

- settlement rule verified
- `EGLC` station mapping stable
- Met Office client implemented
- one dry-run observation window completed
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

Extra caution:

- The current settlement sample points to `RKSI` (Incheon), not central Seoul
- Spatial mismatch is likely larger than London

So even after a KMA client exists, Seoul should still be treated as higher-risk
than London until replay and settlement comparison show otherwise.
