# Weather Settlement Checklist

Generated from the current canonical NOAA-supported city list in [crates/pa-core/src/weather.rs](/home/yangyang/workspace/polygon/PolyAlpha/crates/pa-core/src/weather.rs).

Purpose:
- Track how Polymarket weather markets settle for each NOAA-supported city
- Compare settlement source/rules vs the strategy's current NOAA forecast input
- Flag where model risk comes from settlement-source mismatch rather than forecast quality
- Track international audit-only cities before enabling a non-NOAA provider in trading

Current strategy NOAA input:
- Source: `api.weather.gov`
- Path: `/points/{lat},{lon}` -> `/gridpoints/{office}/{x},{y}`
- Data type: NOAA grid forecast, not final observed settlement value
- Current canonical city coverage: 22 US cities

Risk rubric:
- `Low`: temperature market, settlement source clear, station/window close to current NOAA usage
- `Medium`: settlement source mostly clear, but station/window not fully aligned
- `High`: precipitation/wind/extreme threshold or settlement source unclear

Suggested review steps for each city:
1. Open one or more recent Polymarket weather markets for the city
2. Copy the exact resolution source / rule text
3. Identify whether settlement uses station observation, airport observation, or another official source
4. Compare that source to the current NOAA grid forecast path
5. Record whether mismatch risk is mostly spatial, temporal, or metric-definition related

## Checklist

| City | Sample market title | Metric | Polymarket settlement rule | Expected settlement source | Current NOAA input | Main mismatch risk | Risk | Status | Notes |
|---|---|---|---|---|---|---|---|---|---|
| New York | Highest temperature in NYC on March 11? | Highest temperature (F) | Highest temperature recorded at the LaGuardia Airport Station for the full local day; Wunderground final daily history; whole-degree F resolution | Wunderground daily history for LaGuardia Airport Station (`KLGA`) | NOAA grid forecast for New York | Station vs grid mismatch; local-day/high-temp window mismatch | Medium | Verified | Source: https://polymarket.com/event/highest-temperature-in-nyc-on-march-11/will-the-highest-temperature-in-nyc-be-between-63-64f-on-march-11 |
| Chicago | Highest temperature in Chicago on March 5? | Highest temperature (F) | Highest temperature recorded at the Chicago O'Hare Intl Airport Station for the full local day; Wunderground final daily history; whole-degree F resolution | Wunderground daily history for Chicago O'Hare Intl Airport Station (`KORD`) | NOAA grid forecast for Chicago | Station vs grid mismatch; airport microclimate vs city grid | Medium | Verified | Source: https://polymarket.com/zh/event/highest-temperature-in-chicago-on-march-5-2026 |
| Los Angeles |  |  |  |  | NOAA grid forecast for Los Angeles |  |  | Not started |  |
| Houston |  |  |  |  | NOAA grid forecast for Houston |  |  | Not started |  |
| Phoenix | Highest temperature in Phoenix on December 4? | Highest temperature (F) | Highest temperature recorded at the Phoenix Sky Harbor Intl Airport Station for the full local day; Wunderground final daily history; whole-degree F resolution | Wunderground daily history for Phoenix Sky Harbor Intl Airport (`KPHX`) | NOAA grid forecast for Phoenix | Airport-station settlement vs grid forecast; desert-airport microclimate risk | Medium | Verified | Source: https://polymarket.com/event/highest-temperature-in-phoenix-on-december-4 |
| Miami | Highest temperature in Miami on March 12? | Highest temperature (F) | Highest temperature recorded at the Miami Intl Airport Station for the full local day; Wunderground final daily history; whole-degree F resolution | Wunderground daily history for Miami Intl Airport Station (`KMIA`) | NOAA grid forecast for Miami | Airport-station settlement vs grid forecast; coastal/local-day mismatch | Medium | Verified | Source: https://polymarket.com/zh/event/highest-temperature-in-miami-on-march-12-2026/highest-temperature-in-miami-on-march-12-2026-82-83f |
| Philadelphia |  | Highest temperature (F) | Need direct Polymarket rules page sample | Candidate: Wunderground daily history for Philadelphia Intl Airport (`KPHL`) | NOAA grid forecast for Philadelphia | Airport-station settlement vs grid forecast; local-day mismatch | High | In progress | Batch 1 priority; candidate station needs confirmation from direct market rules page |
| San Antonio |  |  |  |  | NOAA grid forecast for San Antonio |  |  | Not started |  |
| San Diego |  |  |  |  | NOAA grid forecast for San Diego |  |  | Not started |  |
| Dallas | Highest temperature in Dallas on December 7? | Highest temperature (F) | Highest temperature recorded at the Dallas Love Field Station for the full local day; Wunderground final daily history; whole-degree F resolution | Wunderground daily history for Dallas Love Field Station (`KDAL`) | NOAA grid forecast for Dallas | Airport-station settlement vs grid forecast; airport microclimate mismatch | Medium | Verified | Source: https://polymarket.com/event/highest-temperature-in-dallas-on-december-7-785 |
| Austin |  | Highest temperature (F) | Need direct Polymarket rules page sample | Candidate: Wunderground daily history for Austin-Bergstrom Intl Airport (`KAUS`) | NOAA grid forecast for Austin | Airport-station settlement vs grid forecast; heat-island mismatch | High | In progress | Batch 1 priority; candidate station needs confirmation from direct market rules page |
| San Francisco |  | Highest temperature (F) | Need direct Polymarket rules page sample | Candidate: Wunderground daily history for San Francisco Intl Airport (`KSFO`) | NOAA grid forecast for San Francisco | Airport-station settlement vs grid forecast; airport vs city-core microclimate risk | High | In progress | Batch 1 priority; direct rule sample still needed because downtown-vs-airport mismatch risk is high |
| Seattle | Highest temperature in Seattle on March 3? | Highest temperature (F) | Highest temperature recorded at the Seattle-Tacoma International Airport Station for the full local day; Wunderground final daily history; whole-degree F resolution | Wunderground daily history for Seattle-Tacoma International Airport Station (`KSEA`) | NOAA grid forecast for Seattle | Airport-station settlement vs grid forecast; Puget Sound microclimate risk | Medium | Verified | Source: https://polymarket.com/event/highest-temperature-in-seattle-on-march-3-2026 |
| Denver | Highest temperature in Denver on December 4? | Highest temperature (F) | Highest temperature recorded at the Buckley Space Force Base Station for the full local day; Wunderground final daily history; whole-degree F resolution | Wunderground daily history for Buckley Space Force Base Station (`KBKF`) | NOAA grid forecast for Denver | Specific base station settlement vs grid forecast; elevation/local microclimate risk | Medium | Verified | Source: https://polymarket.com/event/highest-temperature-in-denver-on-december-4 |
| Nashville |  |  |  |  | NOAA grid forecast for Nashville |  |  | Not started |  |
| Portland |  |  |  |  | NOAA grid forecast for Portland |  |  | Not started |  |
| Las Vegas |  | Highest temperature (F) | Need direct Polymarket rules page sample | Candidate: Wunderground daily history for Harry Reid Intl Airport (`KLAS`) | NOAA grid forecast for Las Vegas | Airport-station settlement vs grid forecast; desert-airport microclimate risk | High | In progress | Batch 1 priority; candidate station needs confirmation from direct market rules page |
| Atlanta | Highest temperature in Atlanta on March 5? | Highest temperature (F) | Highest temperature recorded at the Hartsfield-Jackson International Airport Station for the full local day; Wunderground final daily history; whole-degree F resolution | Wunderground daily history for Hartsfield-Jackson International Airport Station (`KATL`) | NOAA grid forecast for Atlanta | Airport-station settlement vs grid forecast; local-day/high-temp window mismatch | Medium | Verified | Source: https://polymarket.com/zh/event/highest-temperature-in-atlanta-on-march-5-2026 |
| Minneapolis |  | Highest temperature (F) | Need direct Polymarket rules page sample | Candidate: Wunderground daily history for Minneapolis-St. Paul Intl Airport (`KMSP`) | NOAA grid forecast for Minneapolis | Airport-station settlement vs grid forecast; local-day mismatch | High | In progress | Batch 1 priority; candidate station needs confirmation from direct market rules page |
| Tampa |  |  |  |  | NOAA grid forecast for Tampa |  |  | Not started |  |
| New Orleans |  |  |  |  | NOAA grid forecast for New Orleans |  |  | Not started |  |
| Cleveland |  |  |  |  | NOAA grid forecast for Cleveland |  |  | Not started |  |

## International Audit-Only Samples

These cities are currently in the provider-aware registry as audit-only
locations. They are not trade-enabled yet.

| City | Sample market title | Metric | Polymarket settlement rule | Expected settlement source | Current strategy input | Main mismatch risk | Risk | Status | Notes |
|---|---|---|---|---|---|---|---|---|---|
| London | Highest temperature in London on March 12? | Highest temperature (C) | Highest temperature recorded at the London City Airport Station for the full local day; Wunderground final daily history; whole-degree C resolution | Wunderground daily history for London City Airport Station (`EGLC`) | Met Office daily forecast + Met Office Land Observations actual + PostgreSQL forecast snapshot archive | Airport-station settlement vs city forecast; observation-site selection can differ from the nearest geohash metadata node | High | Verified | Source: https://polymarket.com/event/highest-temperature-in-london-on-march-12-2026 |
| Seoul | Highest temperature in Seoul on March 11? | Highest temperature (C) | Highest temperature recorded at the Incheon Intl Airport Station for the full local day; Wunderground final daily history; whole-degree C resolution | Wunderground daily history for Incheon Intl Airport Station (`RKSI`) | KMA short-range forecast + KMA monthly daily actual + PostgreSQL forecast snapshot archive | Airport-station settlement vs city forecast; airport-to-city spatial mismatch; no official historical forecast archive confirmed yet | High | Verified | Source: https://polymarket.com/zh/event/highest-temperature-in-seoul-on-march-11-2026 |

## Quick Summary Template

Use this after the table starts filling out.

```md
- Verified cities:
- Low-risk cities:
- Medium-risk cities:
- High-risk cities:
- Common settlement sources:
- Common mismatch patterns:
  - Spatial mismatch:
  - Temporal mismatch:
  - Metric-definition mismatch:
```
