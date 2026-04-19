//! SGP4 orbital propagator wrapper.
//!
//! Wraps the `sgp4` crate to produce [`SatPosition`] values (lat/lon/alt and
//! observer-relative az/el) from a [`TleEntry`] at any given time.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::parser::TleEntry;

/// Observer-centred satellite position snapshot.
#[derive(Debug, Clone, PartialEq)]
pub struct SatPosition {
    /// Sub-satellite latitude in degrees (−90..+90).
    pub lat_deg: f64,
    /// Sub-satellite longitude in degrees (−180..+180).
    pub lon_deg: f64,
    /// Altitude above the WGS-84 ellipsoid in kilometres.
    pub alt_km: f64,
    /// Azimuth from true north, degrees (0-360), relative to `observer`.
    pub az_deg: f64,
    /// Elevation above the observer's horizon, degrees (−90..+90).
    pub el_deg: f64,
}

/// Compute the satellite position at `time` from `tle`, as seen from
/// `observer_lat_deg` / `observer_lon_deg` (WGS-84 degrees).
///
/// Returns `None` if SGP4 propagation fails (satellite has decayed,
/// or TLE epoch is too far from the requested time).
pub fn position_at(
    tle: &TleEntry,
    time: SystemTime,
    observer_lat_deg: f64,
    observer_lon_deg: f64,
) -> Option<SatPosition> {
    // Parse TLE into sgp4 elements.
    let elements = sgp4::Elements::from_tle(
        Some(tle.name.clone()),
        tle.line1.as_bytes(),
        tle.line2.as_bytes(),
    )
    .ok()?;

    // Compute minutes since the TLE epoch.
    // elements.datetime is a chrono::NaiveDateTime (UTC).
    let epoch_unix_secs = elements.datetime.and_utc().timestamp() as f64;

    let t_unix_secs = time
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs_f64();

    let minutes_since_epoch = (t_unix_secs - epoch_unix_secs) / 60.0;

    // Build SGP4 constants (WGS-84, aerospace-convention).
    let constants = sgp4::Constants::from_elements(&elements).ok()?;

    // SGP4 propagation → TEME state vector (km, km/s).
    let prediction = constants
        .propagate(sgp4::MinutesSinceEpoch(minutes_since_epoch))
        .ok()?;
    let pos_teme = prediction.position; // [x, y, z] km in TEME frame

    // GMST at the target time (to rotate TEME → ECEF).
    let gmst = gmst_rad(t_unix_secs);

    // Convert TEME to geodetic (lat/lon/alt).
    let (lat_rad, lon_rad, alt_km) = teme_to_geodetic(pos_teme, gmst);

    // Compute az/el from observer.
    let (az_rad, el_rad) = az_el(
        lat_rad,
        lon_rad,
        alt_km,
        observer_lat_deg.to_radians(),
        observer_lon_deg.to_radians(),
    );

    Some(SatPosition {
        lat_deg: lat_rad.to_degrees(),
        lon_deg: lon_rad.to_degrees(),
        alt_km,
        az_deg: az_rad.to_degrees().rem_euclid(360.0),
        el_deg: el_rad.to_degrees(),
    })
}

// ── Coordinate helpers ────────────────────────────────────────────────────────

/// Greenwich Mean Sidereal Time in radians from Unix seconds.
fn gmst_rad(t_unix: f64) -> f64 {
    // Days since J2000.0 (2000-01-01 12:00 ≈ Unix 946728000).
    let d = (t_unix - 946_728_000.0) / 86400.0;
    // IAU 1982 GMST formula (degrees).
    let gmst_deg = 280.460_618_37 + 360.985_647_366_29 * d;
    gmst_deg.to_radians()
}

/// Convert TEME position vector [km] to geodetic (lat rad, lon rad, alt km).
fn teme_to_geodetic(pos: [f64; 3], gmst: f64) -> (f64, f64, f64) {
    // Rotate TEME (Earth-centred inertial) → ECEF by GMST angle.
    let x_ecef = pos[0] * gmst.cos() + pos[1] * gmst.sin();
    let y_ecef = -pos[0] * gmst.sin() + pos[1] * gmst.cos();
    let z_ecef = pos[2];

    // WGS-84 constants.
    const A: f64 = 6_378.137;
    const F: f64 = 1.0 / 298.257_223_563;
    const B: f64 = A * (1.0 - F);
    const E2: f64 = 1.0 - (B / A) * (B / A);

    let lon = y_ecef.atan2(x_ecef);
    let p = (x_ecef * x_ecef + y_ecef * y_ecef).sqrt();

    // Iterative Bowring method for latitude.
    let mut lat = (z_ecef / (p * (1.0 - E2))).atan();
    for _ in 0..10 {
        let sin_lat = lat.sin();
        let n = A / (1.0 - E2 * sin_lat * sin_lat).sqrt();
        lat = ((z_ecef + E2 * n * sin_lat) / p).atan();
    }
    let sin_lat = lat.sin();
    let cos_lat = lat.cos();
    let n = A / (1.0 - E2 * sin_lat * sin_lat).sqrt();
    let alt = if cos_lat.abs() > 1e-10 {
        p / cos_lat - n
    } else {
        z_ecef.abs() / sin_lat.abs() - n * (1.0 - E2)
    };

    (lat, lon, alt)
}

/// Compute azimuth and elevation (radians) from observer to satellite.
fn az_el(
    sat_lat_rad: f64,
    sat_lon_rad: f64,
    sat_alt_km: f64,
    obs_lat_rad: f64,
    obs_lon_rad: f64,
) -> (f64, f64) {
    const A: f64 = 6_378.137;
    const F: f64 = 1.0 / 298.257_223_563;
    const E2: f64 = 2.0 * F - F * F;

    // Observer ECEF (sea level).
    let sin_obs = obs_lat_rad.sin();
    let cos_obs = obs_lat_rad.cos();
    let n_obs = A / (1.0 - E2 * sin_obs * sin_obs).sqrt();
    let ox = n_obs * cos_obs * obs_lon_rad.cos();
    let oy = n_obs * cos_obs * obs_lon_rad.sin();
    let oz = n_obs * (1.0 - E2) * sin_obs;

    // Satellite ECEF.
    let sin_sat = sat_lat_rad.sin();
    let cos_sat = sat_lat_rad.cos();
    let n_sat = A / (1.0 - E2 * sin_sat * sin_sat).sqrt();
    let sx = (n_sat + sat_alt_km) * cos_sat * sat_lon_rad.cos();
    let sy = (n_sat + sat_alt_km) * cos_sat * sat_lon_rad.sin();
    let sz = (n_sat * (1.0 - E2) + sat_alt_km) * sin_sat;

    // ECEF range vector.
    let dx = sx - ox;
    let dy = sy - oy;
    let dz = sz - oz;
    let range = (dx * dx + dy * dy + dz * dz).sqrt();
    if range < 1.0 {
        return (0.0, std::f64::consts::FRAC_PI_2); // observer == satellite (degenerate)
    }

    // Rotate to local topocentric (South, East, Up).
    let sin_lat = obs_lat_rad.sin();
    let cos_lat = obs_lat_rad.cos();
    let sin_lon = obs_lon_rad.sin();
    let cos_lon = obs_lon_rad.cos();

    let south = sin_lat * cos_lon * dx + sin_lat * sin_lon * dy - cos_lat * dz;
    let east = -sin_lon * dx + cos_lon * dy;
    let up = cos_lat * cos_lon * dx + cos_lat * sin_lon * dy + sin_lat * dz;

    let el = (up / range).asin();
    let az = (-south).atan2(east);

    (az, el)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ISS TLE (epoch 2024-01-01 12:00 UTC).
    fn iss_entry() -> TleEntry {
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
    fn propagation_returns_position_at_epoch() {
        let tle = iss_entry();
        // 2024-01-01 12:00:00 UTC
        let t = UNIX_EPOCH + Duration::from_secs(1_704_110_400);
        let pos = position_at(&tle, t, 41.5, -81.7);
        assert!(pos.is_some(), "SGP4 should produce a position at epoch");
        let p = pos.unwrap();
        assert!(p.lat_deg >= -90.0 && p.lat_deg <= 90.0, "lat={}", p.lat_deg);
        assert!(p.lon_deg >= -180.0 && p.lon_deg <= 180.0, "lon={}", p.lon_deg);
        assert!(p.alt_km > 200.0 && p.alt_km < 1_000.0, "alt={}", p.alt_km);
    }

    #[test]
    fn propagation_is_deterministic() {
        let tle = iss_entry();
        let t = UNIX_EPOCH + Duration::from_secs(1_704_110_400);
        let p1 = position_at(&tle, t, 41.5, -81.7).unwrap();
        let p2 = position_at(&tle, t, 41.5, -81.7).unwrap();
        assert_eq!(p1.lat_deg, p2.lat_deg);
        assert_eq!(p1.lon_deg, p2.lon_deg);
    }

    #[test]
    fn elevation_when_directly_overhead_is_near_90() {
        // If the observer is at the sub-satellite point, elevation ≈ 90°.
        let tle = iss_entry();
        let t = UNIX_EPOCH + Duration::from_secs(1_704_110_400);
        let pos_under = position_at(&tle, t, 41.5, -81.7).unwrap();
        // Now query from the sub-satellite point as observer.
        let pos_from_below = position_at(&tle, t, pos_under.lat_deg, pos_under.lon_deg).unwrap();
        assert!(
            pos_from_below.el_deg > 85.0,
            "elevation from below should be ~90°, got {}",
            pos_from_below.el_deg
        );
    }

    #[test]
    fn gmst_increases_by_2pi_per_sidereal_day() {
        let t0 = 1_704_110_400.0;
        let sidereal_day = 86164.1; // seconds
        let g0 = gmst_rad(t0);
        let g1 = gmst_rad(t0 + sidereal_day);
        let delta = g1 - g0;
        assert!((delta - std::f64::consts::TAU).abs() < 0.01, "GMST delta={delta:.4}");
    }

    #[test]
    fn teme_to_geodetic_near_equator() {
        // A point on the equator at 0° lon, 6778 km from Earth centre → alt ≈ 400 km.
        let (lat, lon, alt) = teme_to_geodetic([6_778.0, 0.0, 0.0], 0.0);
        assert!(lat.abs() < 0.01, "lat≈0, got {lat}");
        assert!(lon.abs() < 0.01, "lon≈0, got {lon}");
        assert!((alt - 399.9).abs() < 5.0, "alt≈400, got {alt}");
    }
}
