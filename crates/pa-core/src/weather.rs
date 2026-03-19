use chrono::{DateTime, Utc};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WeatherProvider {
    Noaa,
    OpenMeteo,
    Kma,
    MetOffice,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettlementRiskTier {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettlementValidationStatus {
    Validated,
    DefaultProtected,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WeatherLocation {
    pub canonical_name: &'static str,
    pub provider: WeatherProvider,
    pub lat: f64,
    pub lon: f64,
    pub timezone: &'static str,
    pub settlement_risk_tier: SettlementRiskTier,
    pub settlement_validation_status: SettlementValidationStatus,
    pub trade_enabled: bool,
    pub settlement_note: Option<&'static str>,
}

pub const WEATHER_LOCATIONS: &[WeatherLocation] = &[
    WeatherLocation {
        canonical_name: "New York",
        provider: WeatherProvider::Noaa,
        lat: 40.7128,
        lon: -74.0060,
        timezone: "America/New_York",
        settlement_risk_tier: SettlementRiskTier::Medium,
        settlement_validation_status: SettlementValidationStatus::Validated,
        trade_enabled: true,
        settlement_note: Some("LaGuardia Airport / KLGA"),
    },
    WeatherLocation {
        canonical_name: "Chicago",
        provider: WeatherProvider::Noaa,
        lat: 41.8781,
        lon: -87.6298,
        timezone: "America/Chicago",
        settlement_risk_tier: SettlementRiskTier::Medium,
        settlement_validation_status: SettlementValidationStatus::Validated,
        trade_enabled: true,
        settlement_note: Some("Chicago O'Hare / KORD"),
    },
    WeatherLocation {
        canonical_name: "Los Angeles",
        provider: WeatherProvider::Noaa,
        lat: 34.0522,
        lon: -118.2437,
        timezone: "America/Los_Angeles",
        settlement_risk_tier: SettlementRiskTier::High,
        settlement_validation_status: SettlementValidationStatus::DefaultProtected,
        trade_enabled: true,
        settlement_note: None,
    },
    WeatherLocation {
        canonical_name: "Houston",
        provider: WeatherProvider::Noaa,
        lat: 29.7604,
        lon: -95.3698,
        timezone: "America/Chicago",
        settlement_risk_tier: SettlementRiskTier::High,
        settlement_validation_status: SettlementValidationStatus::DefaultProtected,
        trade_enabled: true,
        settlement_note: None,
    },
    WeatherLocation {
        canonical_name: "Phoenix",
        provider: WeatherProvider::Noaa,
        lat: 33.4484,
        lon: -112.0740,
        timezone: "America/Phoenix",
        settlement_risk_tier: SettlementRiskTier::Medium,
        settlement_validation_status: SettlementValidationStatus::Validated,
        trade_enabled: true,
        settlement_note: Some("Phoenix Sky Harbor Intl / KPHX"),
    },
    WeatherLocation {
        canonical_name: "Miami",
        provider: WeatherProvider::Noaa,
        lat: 25.7617,
        lon: -80.1918,
        timezone: "America/New_York",
        settlement_risk_tier: SettlementRiskTier::Medium,
        settlement_validation_status: SettlementValidationStatus::Validated,
        trade_enabled: true,
        settlement_note: Some("Miami Intl Airport / KMIA"),
    },
    WeatherLocation {
        canonical_name: "Philadelphia",
        provider: WeatherProvider::Noaa,
        lat: 39.9526,
        lon: -75.1652,
        timezone: "America/New_York",
        settlement_risk_tier: SettlementRiskTier::High,
        settlement_validation_status: SettlementValidationStatus::DefaultProtected,
        trade_enabled: true,
        settlement_note: None,
    },
    WeatherLocation {
        canonical_name: "San Antonio",
        provider: WeatherProvider::Noaa,
        lat: 29.4241,
        lon: -98.4936,
        timezone: "America/Chicago",
        settlement_risk_tier: SettlementRiskTier::High,
        settlement_validation_status: SettlementValidationStatus::DefaultProtected,
        trade_enabled: true,
        settlement_note: None,
    },
    WeatherLocation {
        canonical_name: "San Diego",
        provider: WeatherProvider::Noaa,
        lat: 32.7157,
        lon: -117.1611,
        timezone: "America/Los_Angeles",
        settlement_risk_tier: SettlementRiskTier::High,
        settlement_validation_status: SettlementValidationStatus::DefaultProtected,
        trade_enabled: true,
        settlement_note: None,
    },
    WeatherLocation {
        canonical_name: "Dallas",
        provider: WeatherProvider::Noaa,
        lat: 32.7767,
        lon: -96.7970,
        timezone: "America/Chicago",
        settlement_risk_tier: SettlementRiskTier::Medium,
        settlement_validation_status: SettlementValidationStatus::Validated,
        trade_enabled: true,
        settlement_note: Some("Dallas Love Field / KDAL"),
    },
    WeatherLocation {
        canonical_name: "Austin",
        provider: WeatherProvider::Noaa,
        lat: 30.2672,
        lon: -97.7431,
        timezone: "America/Chicago",
        settlement_risk_tier: SettlementRiskTier::High,
        settlement_validation_status: SettlementValidationStatus::DefaultProtected,
        trade_enabled: true,
        settlement_note: None,
    },
    WeatherLocation {
        canonical_name: "San Francisco",
        provider: WeatherProvider::Noaa,
        lat: 37.7749,
        lon: -122.4194,
        timezone: "America/Los_Angeles",
        settlement_risk_tier: SettlementRiskTier::High,
        settlement_validation_status: SettlementValidationStatus::DefaultProtected,
        trade_enabled: true,
        settlement_note: None,
    },
    WeatherLocation {
        canonical_name: "Seattle",
        provider: WeatherProvider::Noaa,
        lat: 47.6062,
        lon: -122.3321,
        timezone: "America/Los_Angeles",
        settlement_risk_tier: SettlementRiskTier::Medium,
        settlement_validation_status: SettlementValidationStatus::Validated,
        trade_enabled: true,
        settlement_note: Some("Seattle-Tacoma Intl / KSEA"),
    },
    WeatherLocation {
        canonical_name: "Denver",
        provider: WeatherProvider::Noaa,
        lat: 39.7392,
        lon: -104.9903,
        timezone: "America/Denver",
        settlement_risk_tier: SettlementRiskTier::Medium,
        settlement_validation_status: SettlementValidationStatus::Validated,
        trade_enabled: true,
        settlement_note: Some("Buckley Space Force Base / KBKF"),
    },
    WeatherLocation {
        canonical_name: "Nashville",
        provider: WeatherProvider::Noaa,
        lat: 36.1627,
        lon: -86.7816,
        timezone: "America/Chicago",
        settlement_risk_tier: SettlementRiskTier::High,
        settlement_validation_status: SettlementValidationStatus::DefaultProtected,
        trade_enabled: true,
        settlement_note: None,
    },
    WeatherLocation {
        canonical_name: "Portland",
        provider: WeatherProvider::Noaa,
        lat: 45.5152,
        lon: -122.6784,
        timezone: "America/Los_Angeles",
        settlement_risk_tier: SettlementRiskTier::High,
        settlement_validation_status: SettlementValidationStatus::DefaultProtected,
        trade_enabled: true,
        settlement_note: None,
    },
    WeatherLocation {
        canonical_name: "Las Vegas",
        provider: WeatherProvider::Noaa,
        lat: 36.1699,
        lon: -115.1398,
        timezone: "America/Los_Angeles",
        settlement_risk_tier: SettlementRiskTier::High,
        settlement_validation_status: SettlementValidationStatus::DefaultProtected,
        trade_enabled: true,
        settlement_note: None,
    },
    WeatherLocation {
        canonical_name: "Atlanta",
        provider: WeatherProvider::Noaa,
        lat: 33.7490,
        lon: -84.3880,
        timezone: "America/New_York",
        settlement_risk_tier: SettlementRiskTier::Medium,
        settlement_validation_status: SettlementValidationStatus::Validated,
        trade_enabled: true,
        settlement_note: Some("Hartsfield-Jackson / KATL"),
    },
    WeatherLocation {
        canonical_name: "Minneapolis",
        provider: WeatherProvider::Noaa,
        lat: 44.9778,
        lon: -93.2650,
        timezone: "America/Chicago",
        settlement_risk_tier: SettlementRiskTier::High,
        settlement_validation_status: SettlementValidationStatus::DefaultProtected,
        trade_enabled: true,
        settlement_note: None,
    },
    WeatherLocation {
        canonical_name: "Tampa",
        provider: WeatherProvider::Noaa,
        lat: 27.9506,
        lon: -82.4572,
        timezone: "America/New_York",
        settlement_risk_tier: SettlementRiskTier::High,
        settlement_validation_status: SettlementValidationStatus::DefaultProtected,
        trade_enabled: true,
        settlement_note: None,
    },
    WeatherLocation {
        canonical_name: "New Orleans",
        provider: WeatherProvider::Noaa,
        lat: 29.9511,
        lon: -90.0715,
        timezone: "America/Chicago",
        settlement_risk_tier: SettlementRiskTier::High,
        settlement_validation_status: SettlementValidationStatus::DefaultProtected,
        trade_enabled: true,
        settlement_note: None,
    },
    WeatherLocation {
        canonical_name: "Cleveland",
        provider: WeatherProvider::Noaa,
        lat: 41.4993,
        lon: -81.6944,
        timezone: "America/New_York",
        settlement_risk_tier: SettlementRiskTier::High,
        settlement_validation_status: SettlementValidationStatus::DefaultProtected,
        trade_enabled: true,
        settlement_note: None,
    },
    WeatherLocation {
        canonical_name: "London",
        provider: WeatherProvider::MetOffice,
        lat: 51.5072,
        lon: -0.1276,
        timezone: "Europe/London",
        settlement_risk_tier: SettlementRiskTier::High,
        settlement_validation_status: SettlementValidationStatus::Validated,
        trade_enabled: false,
        settlement_note: Some("London City Airport / EGLC"),
    },
    WeatherLocation {
        canonical_name: "Seoul",
        provider: WeatherProvider::Kma,
        lat: 37.5665,
        lon: 126.9780,
        timezone: "Asia/Seoul",
        settlement_risk_tier: SettlementRiskTier::High,
        settlement_validation_status: SettlementValidationStatus::Validated,
        trade_enabled: false,
        settlement_note: Some("Incheon Intl Airport / RKSI"),
    },
];

const WEATHER_LOCATION_ALIASES: &[(&str, &str)] = &[
    ("new york city", "New York"),
    ("nyc", "New York"),
    ("los angeles", "Los Angeles"),
    ("l.a.", "Los Angeles"),
    ("la", "Los Angeles"),
    ("philly", "Philadelphia"),
    ("san fran", "San Francisco"),
    ("sf", "San Francisco"),
    ("vegas", "Las Vegas"),
    ("nola", "New Orleans"),
];

/// Backward-compatible NOAA lookup table used by strategy geocoding.
pub const NOAA_SUPPORTED_LOCATIONS: &[(&str, f64, f64)] = &[
    ("New York", 40.7128, -74.0060),
    ("Chicago", 41.8781, -87.6298),
    ("Los Angeles", 34.0522, -118.2437),
    ("Houston", 29.7604, -95.3698),
    ("Phoenix", 33.4484, -112.0740),
    ("Miami", 25.7617, -80.1918),
    ("Philadelphia", 39.9526, -75.1652),
    ("San Antonio", 29.4241, -98.4936),
    ("San Diego", 32.7157, -117.1611),
    ("Dallas", 32.7767, -96.7970),
    ("Austin", 30.2672, -97.7431),
    ("San Francisco", 37.7749, -122.4194),
    ("Seattle", 47.6062, -122.3321),
    ("Denver", 39.7392, -104.9903),
    ("Nashville", 36.1627, -86.7816),
    ("Portland", 45.5152, -122.6784),
    ("Las Vegas", 36.1699, -115.1398),
    ("Atlanta", 33.7490, -84.3880),
    ("Minneapolis", 44.9778, -93.2650),
    ("Tampa", 27.9506, -82.4572),
    ("New Orleans", 29.9511, -90.0715),
    ("Cleveland", 41.4993, -81.6944),
];

pub fn weather_location(location: &str) -> Option<&'static WeatherLocation> {
    let normalized = normalize_weather_location_name(location)?;
    WEATHER_LOCATIONS
        .iter()
        .find(|entry| entry.canonical_name == normalized)
}

pub fn normalize_weather_location_name(location: &str) -> Option<&'static str> {
    let trimmed = location.trim();
    for entry in WEATHER_LOCATIONS {
        if trimmed.eq_ignore_ascii_case(entry.canonical_name) {
            return Some(entry.canonical_name);
        }
    }

    let lower = trimmed.to_lowercase();
    for &(alias, canonical) in WEATHER_LOCATION_ALIASES {
        if lower == alias {
            return Some(canonical);
        }
    }

    None
}

pub fn weather_supported_location_names() -> &'static [&'static str] {
    &[
        "New York",
        "Chicago",
        "Los Angeles",
        "Houston",
        "Phoenix",
        "Miami",
        "Philadelphia",
        "San Antonio",
        "San Diego",
        "Dallas",
        "Austin",
        "San Francisco",
        "Seattle",
        "Denver",
        "Nashville",
        "Portland",
        "Las Vegas",
        "Atlanta",
        "Minneapolis",
        "Tampa",
        "New Orleans",
        "Cleveland",
        "London",
        "Seoul",
    ]
}

pub fn trade_enabled_weather_location_names() -> &'static [&'static str] {
    &[
        "New York",
        "Chicago",
        "Los Angeles",
        "Houston",
        "Phoenix",
        "Miami",
        "Philadelphia",
        "San Antonio",
        "San Diego",
        "Dallas",
        "Austin",
        "San Francisco",
        "Seattle",
        "Denver",
        "Nashville",
        "Portland",
        "Las Vegas",
        "Atlanta",
        "Minneapolis",
        "Tampa",
        "New Orleans",
        "Cleveland",
    ]
}

pub fn settlement_risk_tier(location: &str) -> SettlementRiskTier {
    weather_location(location)
        .map(|entry| entry.settlement_risk_tier)
        .unwrap_or(SettlementRiskTier::High)
}

pub fn settlement_sigma_multiplier(tier: SettlementRiskTier) -> f64 {
    match tier {
        SettlementRiskTier::Low => 1.00,
        SettlementRiskTier::Medium => 1.15,
        SettlementRiskTier::High => 1.35,
    }
}

pub fn settlement_sigma_multiplier_for_location(location: &str) -> f64 {
    settlement_sigma_multiplier(settlement_risk_tier(location))
}

pub fn settlement_validation_status(location: &str) -> SettlementValidationStatus {
    weather_location(location)
        .map(|entry| entry.settlement_validation_status)
        .unwrap_or(SettlementValidationStatus::DefaultProtected)
}

pub fn settlement_extra_edge_bps(status: SettlementValidationStatus) -> u32 {
    match status {
        SettlementValidationStatus::Validated => 0,
        SettlementValidationStatus::DefaultProtected => 150,
    }
}

pub fn settlement_extra_edge_bps_for_location(location: &str) -> u32 {
    settlement_extra_edge_bps(settlement_validation_status(location))
}

pub fn weather_timezone(location: &str) -> &'static str {
    weather_location(location)
        .map(|entry| entry.timezone)
        .unwrap_or("UTC")
}

pub fn weather_observation_site_hint(location: &str) -> Option<&'static str> {
    let canonical = weather_location(location)?.canonical_name;
    match canonical {
        // London Met Office Land Observations audit path is intentionally pinned
        // to a fixed usable observation site near the settlement venue instead of
        // dynamically drifting across nearby geohashes.
        "London" => Some("gcptq8"),
        _ => None,
    }
}

pub fn weather_kma_grid(location: &str) -> Option<(u32, u32)> {
    let canonical = weather_location(location)?.canonical_name;
    match canonical {
        "Seoul" => Some((60, 127)),
        _ => None,
    }
}

pub fn weather_kma_station_id(location: &str) -> Option<u32> {
    let canonical = weather_location(location)?.canonical_name;
    match canonical {
        // Current Seoul settlement audit path uses Incheon as the closest KMA daily station
        // to the Polymarket settlement venue (Incheon Intl / RKSI).
        "Seoul" => Some(112),
        _ => None,
    }
}

pub fn weather_entry_window_open_at(_now_utc: DateTime<Utc>) -> bool {
    true
}

pub fn weather_entry_window_open_now() -> bool {
    weather_entry_window_open_at(Utc::now())
}

pub fn normalize_noaa_location_name(location: &str) -> Option<&'static str> {
    let normalized = normalize_weather_location_name(location)?;
    weather_location(normalized)
        .filter(|entry| entry.provider == WeatherProvider::Noaa)
        .map(|entry| entry.canonical_name)
}

pub fn noaa_supported_location_names() -> &'static [&'static str] {
    trade_enabled_weather_location_names()
}

pub fn noaa_settlement_risk_tier(location: &str) -> SettlementRiskTier {
    normalize_noaa_location_name(location)
        .map(settlement_risk_tier)
        .unwrap_or(SettlementRiskTier::High)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_weather_location_name_aliases() {
        assert_eq!(
            normalize_weather_location_name("New York City"),
            Some("New York")
        );
        assert_eq!(normalize_weather_location_name("SF"), Some("San Francisco"));
        assert_eq!(normalize_weather_location_name("Vegas"), Some("Las Vegas"));
        assert_eq!(normalize_weather_location_name("NOLA"), Some("New Orleans"));
    }

    #[test]
    fn test_weather_supported_location_names_include_audit_only_cities() {
        let names = weather_supported_location_names();
        assert!(names.contains(&"London"));
        assert!(names.contains(&"Seoul"));
    }

    #[test]
    fn test_weather_timezone_uses_shared_metadata() {
        assert_eq!(weather_timezone("Phoenix"), "America/Phoenix");
        assert_eq!(weather_timezone("London"), "Europe/London");
        assert_eq!(weather_timezone("Unknown"), "UTC");
    }

    #[test]
    fn test_trade_enabled_weather_location_names_exclude_audit_only_cities() {
        let names = trade_enabled_weather_location_names();
        assert!(names.contains(&"New York"));
        assert!(!names.contains(&"London"));
        assert!(!names.contains(&"Seoul"));
    }

    #[test]
    fn test_noaa_supported_location_names_are_canonical() {
        let names = noaa_supported_location_names();
        assert!(names.contains(&"New York"));
        assert!(names.contains(&"Los Angeles"));
    }

    #[test]
    fn test_noaa_normalization_rejects_non_noaa_cities() {
        assert_eq!(normalize_noaa_location_name("London"), None);
        assert_eq!(normalize_noaa_location_name("Seoul"), None);
        assert_eq!(normalize_noaa_location_name("NYC"), Some("New York"));
    }

    #[test]
    fn test_weather_location_metadata_exposes_provider_and_trade_flag() {
        let london = weather_location("London").unwrap();
        assert_eq!(london.provider, WeatherProvider::MetOffice);
        assert!(!london.trade_enabled);

        let seoul = weather_location("Seoul").unwrap();
        assert_eq!(seoul.provider, WeatherProvider::Kma);
        assert!(!seoul.trade_enabled);

        let seattle = weather_location("Seattle").unwrap();
        assert_eq!(seattle.provider, WeatherProvider::Noaa);
        assert!(seattle.trade_enabled);
    }

    #[test]
    fn test_weather_kma_metadata_helpers() {
        assert_eq!(weather_kma_grid("Seoul"), Some((60, 127)));
        assert_eq!(weather_kma_station_id("Seoul"), Some(112));
        assert_eq!(weather_kma_grid("London"), None);
    }

    #[test]
    fn test_noaa_settlement_risk_tier_verified_cities_are_medium() {
        assert_eq!(
            noaa_settlement_risk_tier("New York"),
            SettlementRiskTier::Medium
        );
        assert_eq!(
            noaa_settlement_risk_tier("Seattle"),
            SettlementRiskTier::Medium
        );
        assert_eq!(
            noaa_settlement_risk_tier("Dallas"),
            SettlementRiskTier::Medium
        );
    }

    #[test]
    fn test_noaa_settlement_risk_tier_unverified_cities_are_high() {
        assert_eq!(
            noaa_settlement_risk_tier("San Francisco"),
            SettlementRiskTier::High
        );
        assert_eq!(
            noaa_settlement_risk_tier("Las Vegas"),
            SettlementRiskTier::High
        );
    }

    #[test]
    fn test_settlement_sigma_multiplier_for_location_uses_aliases() {
        assert_eq!(settlement_sigma_multiplier_for_location("NYC"), 1.15);
        assert_eq!(settlement_sigma_multiplier_for_location("NOLA"), 1.35);
        assert_eq!(settlement_sigma_multiplier_for_location("London"), 1.35);
    }

    #[test]
    fn test_settlement_validation_status_distinguishes_validated_from_default_protected() {
        assert_eq!(
            settlement_validation_status("New York"),
            SettlementValidationStatus::Validated
        );
        assert_eq!(
            settlement_validation_status("Seattle"),
            SettlementValidationStatus::Validated
        );
        assert_eq!(
            settlement_validation_status("San Francisco"),
            SettlementValidationStatus::DefaultProtected
        );
    }

    #[test]
    fn test_settlement_extra_edge_bps_for_location_uses_aliases() {
        assert_eq!(settlement_extra_edge_bps_for_location("NYC"), 0);
        assert_eq!(settlement_extra_edge_bps_for_location("SF"), 150);
    }

    #[test]
    fn test_weather_entry_window_is_always_open() {
        let early = DateTime::parse_from_rfc3339("2026-03-16T00:30:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let late = DateTime::parse_from_rfc3339("2026-03-16T16:30:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let boundary = DateTime::parse_from_rfc3339("2026-03-17T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        assert!(weather_entry_window_open_at(early));
        assert!(weather_entry_window_open_at(late));
        assert!(weather_entry_window_open_at(boundary));
    }
}
