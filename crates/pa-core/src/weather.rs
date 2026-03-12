/// Hardcoded NOAA-supported weather locations and coordinates.
pub const NOAA_SUPPORTED_LOCATIONS: &[(&str, f64, f64)] = &[
    ("New York", 40.7128, -74.0060),
    ("NYC", 40.7128, -74.0060),
    ("Chicago", 41.8781, -87.6298),
    ("Los Angeles", 34.0522, -118.2437),
    ("LA", 34.0522, -118.2437),
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

const NOAA_LOCATION_ALIASES: &[(&str, &str)] = &[
    ("new york city", "New York"),
    ("nyc", "NYC"),
    ("los angeles", "Los Angeles"),
    ("l.a.", "LA"),
    ("la", "LA"),
    ("philly", "Philadelphia"),
    ("san fran", "San Francisco"),
    ("sf", "San Francisco"),
    ("vegas", "Las Vegas"),
    ("nola", "New Orleans"),
];

pub fn noaa_supported_location_names() -> &'static [&'static str] {
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

pub fn normalize_noaa_location_name(location: &str) -> Option<&'static str> {
    for &(name, _, _) in NOAA_SUPPORTED_LOCATIONS {
        if location.eq_ignore_ascii_case(name) {
            return Some(name);
        }
    }

    let lower = location.trim().to_lowercase();
    for &(alias, canonical) in NOAA_LOCATION_ALIASES {
        if lower == alias {
            return Some(canonical);
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_noaa_location_name_aliases() {
        assert_eq!(
            normalize_noaa_location_name("New York City"),
            Some("New York")
        );
        assert_eq!(normalize_noaa_location_name("SF"), Some("San Francisco"));
        assert_eq!(normalize_noaa_location_name("Vegas"), Some("Las Vegas"));
        assert_eq!(normalize_noaa_location_name("NOLA"), Some("New Orleans"));
    }

    #[test]
    fn test_noaa_supported_location_names_are_canonical() {
        let names = noaa_supported_location_names();
        assert!(names.contains(&"New York"));
        assert!(names.contains(&"Los Angeles"));
        assert!(!names.contains(&"NYC"));
        assert!(!names.contains(&"LA"));
    }
}
