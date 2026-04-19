//! ATC frequency database built from OurAirports open-data CSVs.
//!
//! On first call to [`AtcDb::load`] the two CSVs are downloaded from
//! ourairports.com and cached in `$data_dir/rusty-sdr/atc/`.  Subsequent
//! calls use the cache unless it is older than [`CACHE_MAX_AGE`] (7 days).
//!
//! [`AtcDb::query_nearby`] returns a ranked list of nearby ATC frequencies
//! filtered by altitude:
//! - Below 18,000 ft → TWR / APP / GND
//! - At or above 18,000 ft → CTR / ENRT

mod cache;

pub use cache::CACHE_MAX_AGE;

// OurAirports CSV endpoints.
const AIRPORTS_URL: &str = "https://davidmegginson.github.io/ourairports-data/airports.csv";
const FREQUENCIES_URL: &str =
    "https://davidmegginson.github.io/ourairports-data/airport-frequencies.csv";

/// Transition altitude (ft) separating approach/tower from center/enroute.
pub const TRANSITION_ALT_FT: f32 = 18_000.0;

// Frequency types used for low-altitude (< 18 000 ft) operations.
const LOW_ALT_TYPES: &[&str] = &["TWR", "APP", "GND", "ATIS", "CLNC DEL", "DEP"];
// Frequency types used for high-altitude (>= 18 000 ft) operations.
const HIGH_ALT_TYPES: &[&str] = &["CTR", "ENRT", "FSS", "AFIS"];

/// VHF ATC band (Hz).  Only frequencies in this range are returned.
const VHF_ATC_MIN_HZ: u64 = 118_000_000;
const VHF_ATC_MAX_HZ: u64 = 136_975_000;

// ── Public types ──────────────────────────────────────────────────────────────

/// One airport record from airports.csv.
#[derive(Debug, Clone)]
pub struct Airport {
    pub ident: String,
    pub name: String,
    pub lat: f64,
    pub lon: f64,
}

/// One frequency record from airport-frequencies.csv.
#[derive(Debug, Clone)]
pub struct AirportFrequency {
    pub airport_ident: String,
    /// e.g. "TWR", "APP", "GND", "CTR"
    pub freq_type: String,
    /// Megahertz, e.g. 118.3
    pub freq_mhz: f64,
}

/// A frequency result returned from [`AtcDb::query_nearby`].
#[derive(Debug, Clone)]
pub struct AtcFrequency {
    pub freq_hz: u64,
    pub freq_type: String,
    pub airport_name: String,
    pub airport_ident: String,
    pub distance_nm: f32,
}

/// In-memory ATC frequency database.
pub struct AtcDb {
    airports: Vec<Airport>,
    frequencies: Vec<AirportFrequency>,
}

impl AtcDb {
    /// Load the database, using the on-disk cache when fresh.
    ///
    /// Never returns `Err` — on failure returns an empty database so the UI
    /// degrades gracefully.
    pub fn load() -> Self {
        let airports = cache::load_airports(AIRPORTS_URL, "airports.csv");
        let frequencies = cache::load_frequencies(FREQUENCIES_URL, "airport-frequencies.csv");
        tracing::info!(
            airports = airports.len(),
            frequencies = frequencies.len(),
            "ATC database loaded"
        );
        Self {
            airports,
            frequencies,
        }
    }

    /// Build directly from pre-parsed data (useful for tests).
    pub fn from_data(airports: Vec<Airport>, frequencies: Vec<AirportFrequency>) -> Self {
        Self {
            airports,
            frequencies,
        }
    }

    /// Return nearby ATC frequencies for a given position and altitude.
    ///
    /// * `lat`, `lon` — aircraft position in decimal degrees
    /// * `alt_ft` — altitude in feet (determines TWR/APP vs CTR/ENRT)
    /// * `radius_nm` — search radius in nautical miles
    ///
    /// Results are sorted by `distance_nm` ascending.
    pub fn query_nearby(
        &self,
        lat: f64,
        lon: f64,
        alt_ft: f32,
        radius_nm: f32,
    ) -> Vec<AtcFrequency> {
        let allowed_types: &[&str] = if alt_ft >= TRANSITION_ALT_FT {
            HIGH_ALT_TYPES
        } else {
            LOW_ALT_TYPES
        };

        let mut results: Vec<AtcFrequency> = Vec::new();

        for airport in &self.airports {
            let dist = haversine_nm(lat, lon, airport.lat, airport.lon);
            if dist > radius_nm {
                continue;
            }

            for freq in self
                .frequencies
                .iter()
                .filter(|f| f.airport_ident == airport.ident)
            {
                let ftype_upper = freq.freq_type.to_uppercase();
                let matched = allowed_types
                    .iter()
                    .any(|t| ftype_upper == *t || ftype_upper.starts_with(t));
                if !matched {
                    continue;
                }

                let freq_hz = (freq.freq_mhz * 1_000_000.0).round() as u64;
                if !(VHF_ATC_MIN_HZ..=VHF_ATC_MAX_HZ).contains(&freq_hz) {
                    continue;
                }

                results.push(AtcFrequency {
                    freq_hz,
                    freq_type: ftype_upper,
                    airport_name: airport.name.clone(),
                    airport_ident: airport.ident.clone(),
                    distance_nm: dist,
                });
            }
        }

        results.sort_by(|a, b| a.distance_nm.partial_cmp(&b.distance_nm).unwrap());
        results
    }

    pub fn airport_count(&self) -> usize {
        self.airports.len()
    }

    pub fn frequency_count(&self) -> usize {
        self.frequencies.len()
    }
}

// ── Geometry ──────────────────────────────────────────────────────────────────

/// Great-circle distance in nautical miles between two lat/lon points.
pub fn haversine_nm(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f32 {
    const EARTH_RADIUS_NM: f64 = 3_440.065;
    let dlat = (lat2 - lat1).to_radians();
    let dlon = (lon2 - lon1).to_radians();
    let lat1r = lat1.to_radians();
    let lat2r = lat2.to_radians();
    let a =
        (dlat / 2.0).sin().powi(2) + lat1r.cos() * lat2r.cos() * (dlon / 2.0).sin().powi(2);
    let c = 2.0 * a.sqrt().atan2((1.0 - a).sqrt());
    (EARTH_RADIUS_NM * c) as f32
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_airports() -> Vec<Airport> {
        vec![
            Airport {
                ident: "KSFO".into(),
                name: "San Francisco Intl".into(),
                lat: 37.6189,
                lon: -122.3750,
            },
            Airport {
                ident: "KOAK".into(),
                name: "Oakland Intl".into(),
                lat: 37.7213,
                lon: -122.2208,
            },
            Airport {
                ident: "KDNV".into(), // Danville, IL — far away
                name: "Vermilion Regional".into(),
                lat: 40.1997,
                lon: -87.5959,
            },
        ]
    }

    fn sample_frequencies() -> Vec<AirportFrequency> {
        vec![
            AirportFrequency {
                airport_ident: "KSFO".into(),
                freq_type: "TWR".into(),
                freq_mhz: 120.5,
            },
            AirportFrequency {
                airport_ident: "KSFO".into(),
                freq_type: "APP".into(),
                freq_mhz: 135.1,
            },
            AirportFrequency {
                airport_ident: "KSFO".into(),
                freq_type: "CTR".into(),
                freq_mhz: 132.35,
            },
            AirportFrequency {
                airport_ident: "KSFO".into(),
                freq_type: "GND".into(),
                freq_mhz: 121.9,
            },
            AirportFrequency {
                airport_ident: "KOAK".into(),
                freq_type: "TWR".into(),
                freq_mhz: 118.3,
            },
            AirportFrequency {
                airport_ident: "KDNV".into(),
                freq_type: "TWR".into(),
                freq_mhz: 122.8,
            },
        ]
    }

    #[test]
    fn low_altitude_returns_twr_app_gnd_not_ctr() {
        let db = AtcDb::from_data(sample_airports(), sample_frequencies());
        // Position right at SFO, altitude 5000 ft
        let results = db.query_nearby(37.6189, -122.375, 5_000.0, 50.0);
        let types: Vec<&str> = results.iter().map(|r| r.freq_type.as_str()).collect();
        assert!(types.contains(&"TWR"), "should include TWR at low alt");
        assert!(types.contains(&"APP"), "should include APP at low alt");
        assert!(types.contains(&"GND"), "should include GND at low alt");
        assert!(!types.contains(&"CTR"), "should NOT include CTR at low alt");
    }

    #[test]
    fn high_altitude_returns_ctr_not_twr() {
        let db = AtcDb::from_data(sample_airports(), sample_frequencies());
        // Position right at SFO, altitude 35000 ft
        let results = db.query_nearby(37.6189, -122.375, 35_000.0, 50.0);
        let types: Vec<&str> = results.iter().map(|r| r.freq_type.as_str()).collect();
        assert!(types.contains(&"CTR"), "should include CTR at high alt");
        assert!(!types.contains(&"TWR"), "should NOT include TWR at high alt");
        assert!(!types.contains(&"APP"), "should NOT include APP at high alt");
    }

    #[test]
    fn results_sorted_by_distance_ascending() {
        let db = AtcDb::from_data(sample_airports(), sample_frequencies());
        // Between SFO and OAK, closer to SFO
        let results = db.query_nearby(37.65, -122.31, 3_000.0, 50.0);
        assert!(!results.is_empty());
        for window in results.windows(2) {
            assert!(
                window[0].distance_nm <= window[1].distance_nm,
                "results not sorted: {} > {}",
                window[0].distance_nm,
                window[1].distance_nm
            );
        }
    }

    #[test]
    fn radius_excludes_distant_airports() {
        let db = AtcDb::from_data(sample_airports(), sample_frequencies());
        // At SFO with 50 nm radius — KDNV is in Illinois, >1000 nm away
        let results = db.query_nearby(37.6189, -122.375, 3_000.0, 50.0);
        assert!(
            results.iter().all(|r| r.airport_ident != "KDNV"),
            "KDNV should be excluded by radius"
        );
    }

    #[test]
    fn freq_hz_conversion_correct() {
        let db = AtcDb::from_data(sample_airports(), sample_frequencies());
        let results = db.query_nearby(37.6189, -122.375, 3_000.0, 10.0);
        let twr = results
            .iter()
            .find(|r| r.airport_ident == "KSFO" && r.freq_type == "TWR");
        assert!(twr.is_some());
        assert_eq!(twr.unwrap().freq_hz, 120_500_000);
    }

    #[test]
    fn non_vhf_frequencies_excluded() {
        let mut freqs = sample_frequencies();
        // Add a HF frequency that should be filtered out
        freqs.push(AirportFrequency {
            airport_ident: "KSFO".into(),
            freq_type: "TWR".into(),
            freq_mhz: 5.680, // HF — below VHF ATC band
        });
        let db = AtcDb::from_data(sample_airports(), freqs);
        let results = db.query_nearby(37.6189, -122.375, 3_000.0, 10.0);
        assert!(
            results.iter().all(|r| r.freq_hz >= VHF_ATC_MIN_HZ),
            "HF frequency should be excluded"
        );
    }

    #[test]
    fn haversine_sfo_to_lax_approx() {
        // SFO to LAX is ~293 nautical miles (~337 statute miles)
        let d = haversine_nm(37.6189, -122.375, 33.9425, -118.4081);
        assert!(
            (d - 293.0).abs() < 5.0,
            "SFO→LAX should be ~293 nm, got {d}"
        );
    }

    #[test]
    fn empty_db_returns_empty_results() {
        let db = AtcDb::from_data(vec![], vec![]);
        let results = db.query_nearby(37.6189, -122.375, 3_000.0, 50.0);
        assert!(results.is_empty());
    }

    #[test]
    fn transition_altitude_boundary() {
        let db = AtcDb::from_data(sample_airports(), sample_frequencies());
        // Exactly at transition altitude
        let results_at = db.query_nearby(37.6189, -122.375, TRANSITION_ALT_FT, 10.0);
        let results_below = db.query_nearby(37.6189, -122.375, TRANSITION_ALT_FT - 1.0, 10.0);

        let at_types: Vec<&str> = results_at.iter().map(|r| r.freq_type.as_str()).collect();
        let below_types: Vec<&str> = results_below.iter().map(|r| r.freq_type.as_str()).collect();

        // At exactly 18000 → high alt (CTR), below → low alt (TWR/APP)
        assert!(at_types.contains(&"CTR"));
        assert!(!at_types.contains(&"TWR"));
        assert!(below_types.contains(&"TWR"));
        assert!(!below_types.contains(&"CTR"));
    }
}
