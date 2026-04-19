//! Disk cache for OurAirports CSVs with 7-day TTL.
//!
//! Files are stored in `$data_dir/rusty-sdr/atc/`.  On network failure the
//! stale cache is used so the app keeps working offline after first fetch.

use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use crate::{Airport, AirportFrequency};

/// How long a cached CSV file is considered fresh.
pub const CACHE_MAX_AGE: Duration = Duration::from_secs(7 * 24 * 3600);

// ── Public loaders ────────────────────────────────────────────────────────────

pub(crate) fn load_airports(url: &str, filename: &str) -> Vec<Airport> {
    let csv = fetch_csv(url, filename);
    parse_airports(&csv)
}

pub(crate) fn load_frequencies(url: &str, filename: &str) -> Vec<AirportFrequency> {
    let csv = fetch_csv(url, filename);
    parse_frequencies(&csv)
}

// ── Fetch-with-cache ──────────────────────────────────────────────────────────

fn fetch_csv(url: &str, filename: &str) -> String {
    let cache_path = cache_file_path(filename);

    // Try fresh cache first.
    if let Some(ref p) = cache_path {
        if is_fresh(p) {
            if let Ok(text) = fs::read_to_string(p) {
                tracing::debug!(path = %p.display(), "ATC CSV cache hit");
                return text;
            }
        }
    }

    // Attempt live fetch.
    match ureq::get(url)
        .set("User-Agent", "rusty-sdr ATC db/1.0")
        .call()
    {
        Ok(resp) => {
            if let Ok(text) = resp.into_string() {
                if let Some(ref p) = cache_path {
                    let _ = ensure_parent(p).and_then(|_| fs::write(p, &text).ok());
                }
                tracing::info!(url, "ATC CSV fetch successful");
                return text;
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, url, "ATC CSV fetch failed — trying stale cache");
        }
    }

    // Fall back to stale cache.
    if let Some(ref p) = cache_path {
        if let Ok(text) = fs::read_to_string(p) {
            tracing::info!(path = %p.display(), "Using stale ATC CSV cache");
            return text;
        }
    }

    tracing::warn!(url, "No ATC CSV available — database will be empty");
    String::new()
}

// ── CSV parsers ───────────────────────────────────────────────────────────────

/// Parse airports.csv.
///
/// Expected header (columns that matter):
/// `id,ident,type,name,latitude_deg,longitude_deg,...`
fn parse_airports(csv: &str) -> Vec<Airport> {
    let mut out = Vec::new();
    let mut lines = csv.lines();

    // Parse header to find column indices.
    let header = match lines.next() {
        Some(h) => h,
        None => return out,
    };
    let cols: Vec<&str> = split_csv_row(header);
    let Some(ident_idx) = cols.iter().position(|c| *c == "ident") else {
        tracing::warn!("airports.csv missing 'ident' column");
        return out;
    };
    let Some(name_idx) = cols.iter().position(|c| *c == "name") else {
        tracing::warn!("airports.csv missing 'name' column");
        return out;
    };
    let Some(lat_idx) = cols.iter().position(|c| *c == "latitude_deg") else {
        tracing::warn!("airports.csv missing 'latitude_deg' column");
        return out;
    };
    let Some(lon_idx) = cols.iter().position(|c| *c == "longitude_deg") else {
        tracing::warn!("airports.csv missing 'longitude_deg' column");
        return out;
    };

    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let fields = split_csv_row(line);
        let max_idx = ident_idx.max(name_idx).max(lat_idx).max(lon_idx);
        if fields.len() <= max_idx {
            continue;
        }
        let lat: f64 = match fields[lat_idx].trim_matches('"').parse() {
            Ok(v) => v,
            Err(_) => continue,
        };
        let lon: f64 = match fields[lon_idx].trim_matches('"').parse() {
            Ok(v) => v,
            Err(_) => continue,
        };
        out.push(Airport {
            ident: fields[ident_idx].trim_matches('"').to_string(),
            name: fields[name_idx].trim_matches('"').to_string(),
            lat,
            lon,
        });
    }
    out
}

/// Parse airport-frequencies.csv.
///
/// Expected header (columns that matter):
/// `id,airport_ref,airport_ident,type,description,frequency_mhz`
fn parse_frequencies(csv: &str) -> Vec<AirportFrequency> {
    let mut out = Vec::new();
    let mut lines = csv.lines();

    let header = match lines.next() {
        Some(h) => h,
        None => return out,
    };
    let cols: Vec<&str> = split_csv_row(header);
    let Some(ident_idx) = cols.iter().position(|c| *c == "airport_ident") else {
        tracing::warn!("airport-frequencies.csv missing 'airport_ident' column");
        return out;
    };
    let Some(type_idx) = cols.iter().position(|c| *c == "type") else {
        tracing::warn!("airport-frequencies.csv missing 'type' column");
        return out;
    };
    let Some(mhz_idx) = cols.iter().position(|c| *c == "frequency_mhz") else {
        tracing::warn!("airport-frequencies.csv missing 'frequency_mhz' column");
        return out;
    };

    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let fields = split_csv_row(line);
        let max_idx = ident_idx.max(type_idx).max(mhz_idx);
        if fields.len() <= max_idx {
            continue;
        }
        let freq_mhz: f64 = match fields[mhz_idx].trim_matches('"').parse() {
            Ok(v) => v,
            Err(_) => continue,
        };
        out.push(AirportFrequency {
            airport_ident: fields[ident_idx].trim_matches('"').to_string(),
            freq_type: fields[type_idx].trim_matches('"').to_string(),
            freq_mhz,
        });
    }
    out
}

/// Split a CSV row, handling double-quoted fields (no embedded newlines).
/// This is intentionally simple — OurAirports CSVs are well-formed.
fn split_csv_row(line: &str) -> Vec<&str> {
    let mut fields = Vec::new();
    let mut start = 0;
    let mut in_quotes = false;
    let bytes = line.as_bytes();

    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => in_quotes = !in_quotes,
            b',' if !in_quotes => {
                fields.push(&line[start..i]);
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    fields.push(&line[start..]);
    fields
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn cache_file_path(filename: &str) -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("rusty-sdr").join("atc").join(filename))
}

fn is_fresh(path: &std::path::Path) -> bool {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .map(|mtime| {
            SystemTime::now()
                .duration_since(mtime)
                .map(|age| age < CACHE_MAX_AGE)
                .unwrap_or(false)
        })
        .unwrap_or(false)
}

fn ensure_parent(path: &std::path::Path) -> Option<()> {
    path.parent().and_then(|p| fs::create_dir_all(p).ok())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_AIRPORTS_CSV: &str = "\
id,ident,type,name,latitude_deg,longitude_deg,elevation_ft,continent,iso_country,iso_region,municipality,scheduled_service,gps_code,iata_code,local_code,home_link,wikipedia_link,keywords
6523,00A,heliport,Total Rf Heliport,40.07080078125,-74.93360137939453,11,NA,US,US-PA,Bally,no,00A,,00A,,,
6524,KSFO,large_airport,San Francisco International Airport,37.618999481200005,-122.375,13,NA,US,US-CA,San Francisco,yes,KSFO,SFO,SFO,,,
6525,KOAK,medium_airport,Oakland International Airport,37.721298217773,-122.220802307129,9,NA,US,US-CA,Oakland,yes,KOAK,OAK,OAK,,,
";

    const SAMPLE_FREQUENCIES_CSV: &str = "\
id,airport_ref,airport_ident,type,description,frequency_mhz
1,6524,KSFO,TWR,Tower,120.5
2,6524,KSFO,APP,Approach,135.1
3,6524,KSFO,GND,Ground,121.9
4,6525,KOAK,TWR,Tower,118.3
";

    #[test]
    fn parse_airports_extracts_correct_fields() {
        let airports = parse_airports(SAMPLE_AIRPORTS_CSV);
        assert_eq!(airports.len(), 3);
        let sfo = airports.iter().find(|a| a.ident == "KSFO").unwrap();
        assert_eq!(sfo.name, "San Francisco International Airport");
        assert!((sfo.lat - 37.618999).abs() < 0.001);
        assert!((sfo.lon - (-122.375)).abs() < 0.001);
    }

    #[test]
    fn parse_frequencies_extracts_correct_fields() {
        let freqs = parse_frequencies(SAMPLE_FREQUENCIES_CSV);
        assert_eq!(freqs.len(), 4);
        let twr = freqs
            .iter()
            .find(|f| f.airport_ident == "KSFO" && f.freq_type == "TWR")
            .unwrap();
        assert!((twr.freq_mhz - 120.5).abs() < 0.001);
    }

    #[test]
    fn parse_airports_skips_malformed_rows() {
        let csv = "id,ident,type,name,latitude_deg,longitude_deg\n\
                   bad,KXYZ,airport,Test,not_a_number,also_bad\n\
                   1,KGOOD,airport,Good,37.0,-122.0\n";
        let airports = parse_airports(csv);
        assert_eq!(airports.len(), 1);
        assert_eq!(airports[0].ident, "KGOOD");
    }

    #[test]
    fn parse_frequencies_skips_malformed_rows() {
        let csv = "id,airport_ref,airport_ident,type,description,frequency_mhz\n\
                   bad,1,KXYZ,TWR,Tower,not_a_number\n\
                   1,1,KGOOD,TWR,Tower,120.5\n";
        let freqs = parse_frequencies(csv);
        assert_eq!(freqs.len(), 1);
        assert_eq!(freqs[0].airport_ident, "KGOOD");
    }

    #[test]
    fn split_csv_row_handles_quoted_commas() {
        let row = r#"1,"Name, with comma",37.0"#;
        let fields = split_csv_row(row);
        assert_eq!(fields.len(), 3);
        assert_eq!(fields[1], r#""Name, with comma""#);
    }

    #[test]
    fn cache_freshness_on_new_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.csv");
        std::fs::write(&path, "x").unwrap();
        assert!(is_fresh(&path));
    }

    #[test]
    fn cache_stale_on_old_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("old.csv");
        std::fs::write(&path, "x").unwrap();
        let old_time = SystemTime::now() - Duration::from_secs(8 * 24 * 3600);
        let ft = filetime::FileTime::from_system_time(old_time);
        filetime::set_file_mtime(&path, ft).unwrap();
        assert!(!is_fresh(&path));
    }

    #[test]
    fn ensure_parent_creates_nested_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a").join("b").join("c.csv");
        ensure_parent(&path).expect("should create dirs");
        assert!(path.parent().unwrap().exists());
    }

    #[test]
    fn empty_csv_returns_empty_vec() {
        assert!(parse_airports("").is_empty());
        assert!(parse_frequencies("").is_empty());
    }
}
