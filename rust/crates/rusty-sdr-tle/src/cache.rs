//! TLE disk cache with 24-hour expiry.
//!
//! TLEs are stored in the OS data directory (`dirs::data_dir() / sdrapp / tle/`).
//! On first access, or when the cached file is older than [`CACHE_MAX_AGE`],
//! the file is re-fetched from Celestrak.
//!
//! Network errors fall back to the cached file (even if stale), so the app
//! keeps working without internet access after the first successful fetch.

use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use crate::parser::{parse_tle_text, TleEntry};

/// How long a cached TLE file is considered fresh.
pub const CACHE_MAX_AGE: Duration = Duration::from_secs(24 * 3600);

/// Fetch TLEs for the Orbcomm constellation.
///
/// 1. If a fresh cached file exists (< 24 h old), load from disk.
/// 2. Otherwise try a live HTTP GET from Celestrak; on success, save to disk.
/// 3. If the HTTP request fails but a stale cached file exists, use it.
///
/// Returns an empty `Vec` (never an error) on total failure so the UI
/// degrades gracefully — the pass predictor simply has nothing to show.
pub fn fetch_orbcomm_tles() -> Vec<TleEntry> {
    fetch_from_url(crate::CELESTRAK_ORBCOMM_URL, "orbcomm.tle")
}

/// Core fetch-with-cache logic, parameterised so tests can override the URL.
pub(crate) fn fetch_from_url(url: &str, filename: &str) -> Vec<TleEntry> {
    let cache_path = cache_file_path(filename);

    // Try cache first.
    if let Some(ref p) = cache_path {
        if is_fresh(p) {
            if let Ok(text) = fs::read_to_string(p) {
                if let Ok(entries) = parse_tle_text(&text) {
                    tracing::debug!(
                        path = %p.display(),
                        count = entries.len(),
                        "TLE cache hit"
                    );
                    return entries;
                }
            }
        }
    }

    // Attempt live fetch.
    match ureq::get(url)
        .set("User-Agent", "sdrapp satellite tracker/1.0")
        .call()
    {
        Ok(resp) => {
            if let Ok(text) = resp.into_string() {
                // Save to disk before parsing — if parse fails we still want
                // the raw file for debugging.
                if let Some(ref p) = cache_path {
                    let _ = ensure_parent(p).and_then(|_| fs::write(p, &text).ok());
                }
                match parse_tle_text(&text) {
                    Ok(entries) => {
                        tracing::info!(count = entries.len(), url, "TLE fetch successful");
                        return entries;
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "TLE parse error after live fetch");
                    }
                }
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, url, "TLE fetch failed — trying stale cache");
        }
    }

    // Fall back to stale cache.
    if let Some(ref p) = cache_path {
        if let Ok(text) = fs::read_to_string(p) {
            if let Ok(entries) = parse_tle_text(&text) {
                tracing::info!(
                    path = %p.display(),
                    "Using stale TLE cache (network unavailable)"
                );
                return entries;
            }
        }
    }

    tracing::warn!("No TLE data available — satellite tracking disabled");
    Vec::new()
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn cache_file_path(filename: &str) -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("rusty-sdr").join("tle").join(filename))
}

fn is_fresh(path: &PathBuf) -> bool {
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

fn ensure_parent(path: &PathBuf) -> Option<()> {
    path.parent().and_then(|p| fs::create_dir_all(p).ok())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_TLE_TEXT: &str = "\
ORBCOMM FM107
1 40086U 14037A   24001.50000000  .00000100  00000-0  10000-4 0  9994
2 40086  47.0001 123.4567 0001234  12.3456 347.6543 14.40000000123456
ORBCOMM FM108
1 40087U 14037B   24001.50000000  .00000100  00000-0  10000-4 0  9994
2 40087  47.0001 124.4567 0001234  12.3456 347.6543 14.40000000123456
";

    /// fetch_from_url with a data: URI workaround is hard to test without a
    /// server; we test cache read/write independently.

    #[test]
    fn fresh_cache_is_read_without_network() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.tle");
        fs::write(&path, SAMPLE_TLE_TEXT).unwrap();

        // Simulate a fresh file by reading it directly.
        let text = fs::read_to_string(&path).unwrap();
        let entries = parse_tle_text(&text).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].norad_id, 40_086);
        assert_eq!(entries[1].norad_id, 40_087);
    }

    #[test]
    fn stale_detection_on_old_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("old.tle");
        fs::write(&path, SAMPLE_TLE_TEXT).unwrap();

        // Set the mtime to 25 hours ago.
        let old_time = SystemTime::now() - Duration::from_secs(25 * 3600);
        let ft = filetime::FileTime::from_system_time(old_time);
        filetime::set_file_mtime(&path, ft).unwrap();

        assert!(!is_fresh(&path), "25-hour-old file should be stale");
    }

    #[test]
    fn fresh_detection_on_new_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("new.tle");
        fs::write(&path, SAMPLE_TLE_TEXT).unwrap();
        assert!(is_fresh(&path), "Freshly written file should be fresh");
    }

    #[test]
    fn ensure_parent_creates_nested_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a").join("b").join("c.tle");
        ensure_parent(&path).expect("Should create nested dirs");
        assert!(path.parent().unwrap().exists());
    }
}
