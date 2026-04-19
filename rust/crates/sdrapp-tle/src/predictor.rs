//! Pass predictor: compute upcoming satellite passes over a ground observer.
//!
//! A "pass" is a continuous arc where the satellite elevation is above
//! [`MIN_ELEVATION_DEG`].  For each pass we report AOS (acquisition of signal),
//! LOS (loss of signal), and the moment of maximum elevation.

use std::time::{Duration, SystemTime};

use crate::{parser::TleEntry, propagator::position_at};

/// Minimum elevation (degrees) for a pass to be counted.
pub const MIN_ELEVATION_DEG: f64 = 5.0;

/// Step size used when scanning for passes.  Smaller = more accurate AOS/LOS
/// times but slower computation.
const SCAN_STEP: Duration = Duration::from_secs(30);

/// Fine-search step for refining AOS/LOS crossing.
const FINE_STEP: Duration = Duration::from_secs(5);

/// One satellite pass event.
#[derive(Debug, Clone)]
pub struct PassEvent {
    /// Satellite name (from TLE).
    pub sat_name: String,
    /// Time the satellite rises above [`MIN_ELEVATION_DEG`].
    pub aos: SystemTime,
    /// Time of maximum elevation during the pass.
    pub max_el_time: SystemTime,
    /// Maximum elevation in degrees.
    pub max_el_deg: f64,
    /// Time the satellite drops below [`MIN_ELEVATION_DEG`].
    pub los: SystemTime,
    /// Azimuth at AOS (degrees, 0=N).
    pub aos_az_deg: f64,
}

impl PassEvent {
    /// Duration of the pass.
    pub fn duration(&self) -> Duration {
        self.los
            .duration_since(self.aos)
            .unwrap_or(Duration::ZERO)
    }
}

/// Predict upcoming passes over an observer location.
pub struct PassPredictor {
    tle: TleEntry,
    observer_lat: f64,
    observer_lon: f64,
}

impl PassPredictor {
    pub fn new(tle: TleEntry, observer_lat_deg: f64, observer_lon_deg: f64) -> Self {
        Self {
            tle,
            observer_lat: observer_lat_deg,
            observer_lon: observer_lon_deg,
        }
    }

    /// Predict up to `max_passes` passes starting from `start` over the
    /// next `window` duration.
    ///
    /// Returns passes sorted by AOS time.
    pub fn predict(
        &self,
        start: SystemTime,
        window: Duration,
        max_passes: usize,
    ) -> Vec<PassEvent> {
        let end = start + window;
        let mut passes = Vec::new();
        let mut t = start;
        let mut prev_el = self.el_at(t);
        let mut in_pass = false;
        let mut pass_start = start;
        let mut max_el = f64::NEG_INFINITY;
        let mut max_el_time = start;
        let mut aos_az = 0.0;

        while t <= end && passes.len() < max_passes {
            t += SCAN_STEP;
            let el = self.el_at(t);

            if !in_pass && prev_el < MIN_ELEVATION_DEG && el >= MIN_ELEVATION_DEG {
                // Rising edge: refine AOS.
                let aos = self.find_crossing(t - SCAN_STEP, t, true);
                pass_start = aos;
                aos_az = self
                    .pos_at(aos)
                    .map(|p| p.az_deg)
                    .unwrap_or(0.0);
                max_el = el;
                max_el_time = t;
                in_pass = true;
            } else if in_pass && el > max_el {
                max_el = el;
                max_el_time = t;
            } else if in_pass && prev_el >= MIN_ELEVATION_DEG && el < MIN_ELEVATION_DEG {
                // Falling edge: refine LOS.
                let los = self.find_crossing(t - SCAN_STEP, t, false);
                passes.push(PassEvent {
                    sat_name: self.tle.name.clone(),
                    aos: pass_start,
                    max_el_time,
                    max_el_deg: max_el,
                    los,
                    aos_az_deg: aos_az,
                });
                in_pass = false;
                max_el = f64::NEG_INFINITY;
            }
            prev_el = el;
        }

        // If still in a pass at `end`, close it there.
        if in_pass && passes.len() < max_passes {
            passes.push(PassEvent {
                sat_name: self.tle.name.clone(),
                aos: pass_start,
                max_el_time,
                max_el_deg: max_el,
                los: end,
                aos_az_deg: aos_az,
            });
        }

        passes
    }

    /// Compute a dense ground track (lat/lon points) for map rendering.
    ///
    /// Returns points at `step` intervals from `t_start` to `t_start + duration`.
    pub fn ground_track(
        &self,
        t_start: SystemTime,
        duration: Duration,
        step: Duration,
    ) -> Vec<(f64, f64)> {
        let mut points = Vec::new();
        let mut t = t_start;
        let end = t_start + duration;
        while t <= end {
            if let Some(pos) = self.pos_at(t) {
                points.push((pos.lat_deg, pos.lon_deg));
            }
            t += step;
        }
        points
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn el_at(&self, t: SystemTime) -> f64 {
        self.pos_at(t).map(|p| p.el_deg).unwrap_or(-90.0)
    }

    fn pos_at(&self, t: SystemTime) -> Option<crate::SatPosition> {
        position_at(&self.tle, t, self.observer_lat, self.observer_lon)
    }

    /// Binary-search for the elevation crossing between `t_lo` and `t_hi`.
    /// `rising = true` → find where el crosses MIN_ELEVATION_DEG upward.
    fn find_crossing(&self, t_lo: SystemTime, t_hi: SystemTime, _rising: bool) -> SystemTime {
        let mut lo = t_lo;
        let mut hi = t_hi;
        for _ in 0..8 {
            if hi.duration_since(lo).unwrap_or(Duration::ZERO) < FINE_STEP {
                break;
            }
            let mid = lo + (hi.duration_since(lo).unwrap_or(Duration::ZERO) / 2);
            let el_mid = self.el_at(mid);
            if el_mid >= MIN_ELEVATION_DEG {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        lo
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::TleEntry;
    use std::time::UNIX_EPOCH;

    // ISS TLE (epoch 2024-01-01 12:00 UTC) — passes frequently over 41°N.
    fn iss_tle() -> TleEntry {
        TleEntry {
            name: "ISS (ZARYA)".to_string(),
            norad_id: 25544,
            line1: "1 25544U 98067A   24001.50000000  .00020351  00000-0  36046-3 0  9995"
                .to_string(),
            line2: "2 25544  51.6416 350.6991 0001349 325.6715 139.9249 15.49563848432182"
                .to_string(),
        }
    }

    #[test]
    fn predictor_returns_passes_over_24h() {
        let tle = iss_tle();
        let predictor = PassPredictor::new(tle, 41.5, -81.7);
        let start = UNIX_EPOCH + Duration::from_secs(1_704_110_400); // 2024-01-01 12:00 UTC
        let window = Duration::from_secs(24 * 3600);

        let passes = predictor.predict(start, window, 20);
        // ISS has ~15 orbits/day; expect ≥3 passes visible from Cleveland in 24 h.
        assert!(
            passes.len() >= 3,
            "Expected ≥3 ISS passes in 24 h, got {}",
            passes.len()
        );
    }

    #[test]
    fn pass_event_has_valid_times() {
        let tle = iss_tle();
        let predictor = PassPredictor::new(tle, 41.5, -81.7);
        let start = UNIX_EPOCH + Duration::from_secs(1_704_110_400);
        let window = Duration::from_secs(6 * 3600);
        let passes = predictor.predict(start, window, 5);

        for p in &passes {
            assert!(p.los >= p.aos, "LOS must be after AOS");
            assert!(p.max_el_time >= p.aos, "max_el_time must be after AOS");
            assert!(p.max_el_time <= p.los, "max_el_time must be before LOS");
            assert!(p.max_el_deg >= MIN_ELEVATION_DEG, "max_el below min: {}", p.max_el_deg);
            assert!(p.max_el_deg <= 90.0, "max_el implausible: {}", p.max_el_deg);
        }
    }

    #[test]
    fn pass_duration_is_positive() {
        let tle = iss_tle();
        let predictor = PassPredictor::new(tle, 41.5, -81.7);
        let start = UNIX_EPOCH + Duration::from_secs(1_704_110_400);
        let passes = predictor.predict(start, Duration::from_secs(6 * 3600), 5);
        for p in &passes {
            assert!(p.duration() > Duration::ZERO, "pass duration should be positive");
        }
    }

    #[test]
    fn ground_track_returns_points() {
        let tle = iss_tle();
        let predictor = PassPredictor::new(tle, 41.5, -81.7);
        let start = UNIX_EPOCH + Duration::from_secs(1_704_110_400);
        let track = predictor.ground_track(start, Duration::from_secs(600), Duration::from_secs(30));
        assert!(!track.is_empty(), "Ground track should have points");
        for (lat, lon) in &track {
            assert!(*lat >= -90.0 && *lat <= 90.0, "lat out of range: {lat}");
            assert!(*lon >= -180.0 && *lon <= 180.0, "lon out of range: {lon}");
        }
    }

    #[test]
    fn max_passes_limit_respected() {
        let tle = iss_tle();
        let predictor = PassPredictor::new(tle, 41.5, -81.7);
        let start = UNIX_EPOCH + Duration::from_secs(1_704_110_400);
        let passes = predictor.predict(start, Duration::from_secs(48 * 3600), 3);
        assert!(passes.len() <= 3, "Should not exceed max_passes=3");
    }
}
