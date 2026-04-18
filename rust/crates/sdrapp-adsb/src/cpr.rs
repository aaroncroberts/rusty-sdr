//! Compact Position Reporting (CPR) global position decoder.
//!
//! ADS-B broadcasts positions as 17-bit fractional zone codes alternating
//! between "even" (NZ=15, 60 latitude zones) and "odd" (NZ=15, 59 zones)
//! frames.  A globally-unambiguous position requires one of each, received
//! within a short window (typically 10 s).
//!
//! Algorithm: ICAO Doc 9684, Appendix C; also documented in pyModeS.

const NZ: f64 = 15.0;

/// CPR even/odd frame stored for pairing.
#[derive(Debug, Clone, Copy)]
pub struct CprFrame {
    /// `false` = even, `true` = odd.
    pub odd: bool,
    /// 17-bit encoded latitude (0..=131071).
    pub lat_cpr: u32,
    /// 17-bit encoded longitude (0..=131071).
    pub lon_cpr: u32,
}

/// Number of longitude zones at latitude `lat` (degrees).
///
/// Returns 1 at extreme latitudes (poles).
fn nl(lat: f64) -> f64 {
    if lat.abs() >= 87.0 {
        return 1.0;
    }
    let a = 1.0 - (std::f64::consts::PI / (2.0 * NZ)).cos();
    let b = lat.to_radians().cos().powi(2);
    (2.0 * std::f64::consts::PI / (1.0 - a / b).acos())
        .floor()
        .max(1.0)
}

/// Decode a globally-unambiguous position from one even and one odd CPR frame.
///
/// `a` and `b` may be supplied in either order — the function identifies them
/// by `CprFrame::odd`.  Returns `(latitude_deg, longitude_deg)` or `None` if
/// the frames are inconsistent (different NL zones).
///
/// **Caller responsibility**: ensure frames are from the same aircraft and
/// received within 10 s of each other.
pub fn decode_global(a: CprFrame, b: CprFrame) -> Option<(f64, f64)> {
    // Normalise so `even` is always the even frame.
    let (even, odd) = if !a.odd && b.odd {
        (a, b)
    } else if a.odd && !b.odd {
        (b, a)
    } else {
        return None; // both same parity
    };

    let lat_e = even.lat_cpr as f64 / 131072.0;
    let lat_o = odd.lat_cpr as f64 / 131072.0;
    let lon_e = even.lon_cpr as f64 / 131072.0;
    let lon_o = odd.lon_cpr as f64 / 131072.0;

    let d_lat_even = 360.0 / (4.0 * NZ);        // 6.0°
    let d_lat_odd = 360.0 / (4.0 * NZ - 1.0);   // ≈6.1017°

    // Latitude zone index.
    let j = (59.0 * lat_e - 60.0 * lat_o + 0.5).floor();

    // Decoded latitudes for each frame.
    let mut lat_even_dec = d_lat_even * (j.rem_euclid(60.0) + lat_e);
    let mut lat_odd_dec = d_lat_odd * (j.rem_euclid(59.0) + lat_o);

    // Normalise to [-90, 90].
    if lat_even_dec >= 270.0 {
        lat_even_dec -= 360.0;
    }
    if lat_odd_dec >= 270.0 {
        lat_odd_dec -= 360.0;
    }

    // Both frames must fall in the same NL zone.
    if nl(lat_even_dec) != nl(lat_odd_dec) {
        return None;
    }

    // Use the even frame latitude (convention: use the more-recently-received
    // frame; callers should pass the later frame as the even one when possible).
    let lat = lat_even_dec;
    let nl_lat = nl(lat);

    // Longitude zone index: formula uses (NL-1) for even, NL for odd.
    let m = (lon_e * (nl_lat - 1.0) - lon_o * nl_lat + 0.5).floor();

    let lon = if nl_lat > 0.0 {
        let lon_raw = (360.0 / nl_lat) * (m.rem_euclid(nl_lat) + lon_e);
        // Normalise to [-180, 180).
        if lon_raw >= 180.0 {
            lon_raw - 360.0
        } else {
            lon_raw
        }
    } else {
        0.0
    };

    Some((lat, lon))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Test vectors from frames 8D40621D58C382D690C8AC and 8D40621D58C386435CC412.
    /// even: lat_cpr=93000, lon_cpr=51372  (ICAO 40621D, TC=11, 38000 ft)
    /// odd:  lat_cpr=74158, lon_cpr=50194
    /// Expected: lat ≈ 52.2572°, lon ≈ 3.9194°  (Netherlands)
    #[test]
    fn cpr_global_decode_known_pair() {
        let even = CprFrame { odd: false, lat_cpr: 93000, lon_cpr: 51372 };
        let odd = CprFrame { odd: true, lat_cpr: 74158, lon_cpr: 50194 };

        let (lat, lon) = decode_global(even, odd).expect("Should decode successfully");

        assert!(
            (lat - 52.2572).abs() < 0.002,
            "Latitude mismatch: got {lat:.4}, expected 52.2572"
        );
        assert!(
            (lon - 3.9194).abs() < 0.002,
            "Longitude mismatch: got {lon:.4}, expected 3.9194"
        );
    }

    /// Swapping even/odd argument order should give the same result.
    #[test]
    fn cpr_global_decode_order_independent() {
        let even = CprFrame { odd: false, lat_cpr: 93000, lon_cpr: 51372 };
        let odd = CprFrame { odd: true, lat_cpr: 74158, lon_cpr: 50194 };

        let r1 = decode_global(even, odd).unwrap();
        let r2 = decode_global(odd, even).unwrap();

        assert!((r1.0 - r2.0).abs() < 1e-9);
        assert!((r1.1 - r2.1).abs() < 1e-9);
    }

    /// Two frames of the same parity → None.
    #[test]
    fn cpr_global_decode_same_parity_returns_none() {
        let frame = CprFrame { odd: false, lat_cpr: 93000, lon_cpr: 51372 };
        assert!(decode_global(frame, frame).is_none());
    }

    /// NL zones: spot-check known values.
    #[test]
    fn nl_spot_checks() {
        assert_eq!(nl(0.0), 59.0, "NL(0°) should be 59");
        assert_eq!(nl(87.9), 1.0, "NL(87.9°) should be 1");
        // nl(60°) should be 29 per pyModeS reference
        assert_eq!(nl(60.0), 29.0, "NL(60°) should be 29");
        // Symmetric around equator
        assert_eq!(nl(-52.0), nl(52.0));
    }
}
