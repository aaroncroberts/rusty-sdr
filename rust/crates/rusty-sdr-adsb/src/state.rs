//! Aircraft state store with 60-second expiry.
//!
//! Maintains a `HashMap<u32, AircraftState>` keyed by ICAO address.
//! Call [`AircraftStore::update`] with decoded messages, then
//! [`AircraftStore::prune_expired`] periodically (e.g. once per second).

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::parser::{AdsbDecoded, AdsbMessage};
use crate::ShortFrameDecoded;
use crate::cpr::{decode_global, decode_local, CprFrame};

/// Expiry window for aircraft entries.  5 min keeps aircraft visible long
/// after they leave your coverage zone.
pub const EXPIRY: Duration = Duration::from_secs(300);

/// Aircraft are marked "stale" (signal lost) after this many seconds.
pub const STALE_AFTER: Duration = Duration::from_secs(60);

/// CPR frame window: pair must arrive within this interval to be decoded.
/// 10 s matches dump1090/readsb — prevents pairing frames where the aircraft
/// has crossed an NL zone boundary, which would produce a bad fix.
const CPR_WINDOW: Duration = Duration::from_secs(10);

/// Decoded state for one aircraft.
#[derive(Debug, Clone)]
pub struct AircraftState {
    /// 24-bit ICAO address.
    pub icao: u32,
    /// 8-char callsign, if received.
    pub callsign: Option<String>,
    /// Decoded latitude (degrees), if a CPR pair has been received.
    pub lat: Option<f64>,
    /// Decoded longitude (degrees).
    pub lon: Option<f64>,
    /// Barometric altitude in feet.
    pub altitude_ft: Option<i32>,
    /// Ground speed in knots.
    pub speed_kt: Option<f32>,
    /// Track heading in degrees (0 = N, clockwise).
    pub heading_deg: Option<f32>,
    /// Vertical rate in feet per minute.
    pub vert_rate_fpm: Option<i32>,
    /// Squawk code (Mode A identity), if received via DF5/DF21.
    pub squawk: Option<u16>,
    /// `true` when this aircraft has been seen via Mode-S short frames
    /// (DF5/11/21) but has never sent a DF17/18 ADS-B extended squitter.
    /// Such aircraft are tracked by ICAO only — no callsign or position.
    pub mode_s_only: bool,
    /// Wall-clock time of the most-recent message from this aircraft.
    pub last_seen: Instant,
    // Pending CPR even/odd frames for position decoding.
    pending_even: Option<(CprFrame, Instant)>,
    pending_odd: Option<(CprFrame, Instant)>,
}

impl AircraftState {
    /// True when no message has been received for [`STALE_AFTER`] seconds.
    /// Stale aircraft remain in the store until [`EXPIRY`] (5 min) and are
    /// displayed as dimmed ghosts on the map.
    pub fn is_stale(&self) -> bool {
        self.last_seen.elapsed() >= STALE_AFTER
    }

    fn new(icao: u32) -> Self {
        Self {
            icao,
            callsign: None,
            lat: None,
            lon: None,
            altitude_ft: None,
            speed_kt: None,
            heading_deg: None,
            vert_rate_fpm: None,
            squawk: None,
            mode_s_only: false,
            last_seen: Instant::now(),
            pending_even: None,
            pending_odd: None,
        }
    }
}

/// Thread-local aircraft state store.
///
/// Not `Send` or `Sync` by itself — wrap in `Arc<Mutex<AircraftStore>>` or
/// `Arc<RwLock<AircraftStore>>` for shared access.
#[derive(Debug, Default)]
pub struct AircraftStore {
    map: HashMap<u32, AircraftState>,
    /// Observer home position — used for single-frame local CPR bootstrap.
    /// When set, any airborne position frame produces a position immediately
    /// (no need to wait for an even+odd pair) for aircraft within ~300 nm.
    home_lat: Option<f64>,
    home_lon: Option<f64>,
}

impl AircraftStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the observer home position for single-frame CPR bootstrap.
    ///
    /// Call this once on startup with the configured home lat/lon.
    /// Any subsequent airborne position frame will immediately produce a
    /// decoded lat/lon using local CPR (accurate within ~300 nm of home).
    pub fn set_home_position(&mut self, lat: f64, lon: f64) {
        self.home_lat = Some(lat);
        self.home_lon = Some(lon);
    }

    /// Apply a decoded short-frame (DF5/DF11/DF21) to the store.
    ///
    /// **DF11** (CRC verified, `icao_recovered = false`): creates a new entry if
    /// the ICAO is unknown — the ICAO is directly readable and trustworthy.
    ///
    /// **DF5/DF21** (`icao_recovered = true`): only updates an *existing* entry
    /// (adds squawk).  Never creates a new entry — ICAO recovery is computed from
    /// an unverified payload, so it would create spurious entries from noise frames.
    pub fn update_short(&mut self, msg: &ShortFrameDecoded) {
        let now = Instant::now();
        if msg.icao_recovered {
            // DF5/DF21: only enrich a known aircraft; don't create phantom entries.
            if let Some(entry) = self.map.get_mut(&msg.icao) {
                entry.last_seen = now;
                if let Some(sq) = msg.squawk {
                    entry.squawk = Some(sq);
                }
            }
            return;
        }
        // DF11 (CRC-verified): safe to create a new entry.
        let entry = self.map.entry(msg.icao).or_insert_with(|| AircraftState::new(msg.icao));
        entry.last_seen = now;
        if let Some(sq) = msg.squawk {
            entry.squawk = Some(sq);
        }
        if entry.callsign.is_none() && entry.lat.is_none() {
            entry.mode_s_only = true;
        }
    }

    /// Apply a decoded ADS-B message to the store.
    pub fn update(&mut self, msg: &AdsbDecoded) {
        let now = Instant::now();
        let entry = self.map.entry(msg.icao).or_insert_with(|| AircraftState::new(msg.icao));
        entry.last_seen = now;
        // Receiving a DF17/18 frame proves this is an ADS-B aircraft.
        entry.mode_s_only = false;

        match &msg.message {
            AdsbMessage::Identification { callsign } => {
                let cs = callsign
                    .iter()
                    .map(|&b| b as char)
                    .collect::<String>()
                    .trim_end()
                    .to_string();
                entry.callsign = Some(cs);
            }
            AdsbMessage::AirbornePosition { cpr_odd, lat_cpr, lon_cpr, altitude_ft } => {
                if let Some(alt) = altitude_ft {
                    entry.altitude_ft = Some(*alt);
                }

                let frame = CprFrame {
                    odd: *cpr_odd,
                    lat_cpr: *lat_cpr,
                    lon_cpr: *lon_cpr,
                };

                if *cpr_odd {
                    entry.pending_odd = Some((frame, now));
                } else {
                    entry.pending_even = Some((frame, now));
                }

                // If we already have a fix, use local CPR for a smooth single-frame
                // update — no need to wait for a fresh even+odd pair.
                if let (Some(lat_ref), Some(lon_ref)) = (entry.lat, entry.lon) {
                    let (lat, lon) = decode_local(frame, lat_ref, lon_ref);
                    entry.lat = Some(lat);
                    entry.lon = Some(lon);
                } else {
                    // No fix yet — first try global decode from the pending even+odd pair.
                    let global_pos = match (entry.pending_even, entry.pending_odd) {
                        (Some((even, t_even)), Some((odd, t_odd))) => {
                            let age = if t_even > t_odd { t_even - t_odd } else { t_odd - t_even };
                            if age <= CPR_WINDOW { decode_global(even, odd) } else { None }
                        }
                        _ => None,
                    };

                    if let Some((lat, lon)) = global_pos {
                        // Sanity-check the global decode against home position.
                        // Global CPR can produce geographically impossible results
                        // when frames arrive from noisy or ambiguous receptions.
                        // Accept the result only if it is within a generous box
                        // around home (≈2000 nm); reject silently and wait for
                        // home-bootstrap on the next frame.
                        let accepted = match (self.home_lat, self.home_lon) {
                            (Some(hlat), Some(hlon)) => {
                                (lat - hlat).abs() < 25.0 && (lon - hlon).abs() < 35.0
                            }
                            _ => true, // no home set: accept unconditionally
                        };
                        if accepted {
                            entry.lat = Some(lat);
                            entry.lon = Some(lon);
                        }
                    } else if let (Some(hlat), Some(hlon)) = (self.home_lat, self.home_lon) {
                        // Bootstrap from home position: decode this single frame locally.
                        // Accept the result if it falls within ~350 nm of home (5° lat / 8° lon).
                        // This gives an immediate position on first contact without waiting
                        // for an even+odd pair — identical to dump1090 --lat/--lon behaviour.
                        let (lat, lon) = decode_local(frame, hlat, hlon);
                        if (lat - hlat).abs() < 5.0 && (lon - hlon).abs() < 8.0 {
                            entry.lat = Some(lat);
                            entry.lon = Some(lon);
                        }
                    }
                }
            }
            AdsbMessage::AirborneVelocity { speed_kt, heading_deg, vert_rate_fpm } => {
                entry.speed_kt = Some(*speed_kt);
                entry.heading_deg = Some(*heading_deg);
                entry.vert_rate_fpm = Some(*vert_rate_fpm);
            }
            AdsbMessage::Other { .. } => {}
        }
    }

    /// Remove aircraft not heard from in the last 60 seconds.
    pub fn prune_expired(&mut self) {
        self.prune_expired_at(Instant::now());
    }

    /// Prune expired entries relative to an explicit `now` (for testing).
    pub fn prune_expired_at(&mut self, now: Instant) {
        self.map.retain(|_, v| now.duration_since(v.last_seen) < EXPIRY);
    }

    /// All currently tracked aircraft (sorted by ICAO for determinism).
    pub fn aircraft(&self) -> Vec<&AircraftState> {
        let mut v: Vec<&AircraftState> = self.map.values().collect();
        v.sort_by_key(|a| a.icao);
        v
    }

    /// Look up a single aircraft by ICAO address.
    pub fn get(&self, icao: u32) -> Option<&AircraftState> {
        self.map.get(&icao)
    }

    /// Number of tracked aircraft.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{AdsbDecoded, AdsbMessage};

    fn ident_msg(icao: u32, callsign: &str) -> AdsbDecoded {
        let mut cs = [b' '; 8];
        for (i, &b) in callsign.as_bytes().iter().take(8).enumerate() {
            cs[i] = b;
        }
        AdsbDecoded {
            icao,
            message: AdsbMessage::Identification { callsign: cs },
        }
    }

    fn pos_msg(icao: u32, cpr_odd: bool, lat_cpr: u32, lon_cpr: u32, alt: i32) -> AdsbDecoded {
        AdsbDecoded {
            icao,
            message: AdsbMessage::AirbornePosition {
                cpr_odd,
                lat_cpr,
                lon_cpr,
                altitude_ft: Some(alt),
            },
        }
    }

    fn vel_msg(icao: u32, speed: f32, hdg: f32, vr: i32) -> AdsbDecoded {
        AdsbDecoded {
            icao,
            message: AdsbMessage::AirborneVelocity {
                speed_kt: speed,
                heading_deg: hdg,
                vert_rate_fpm: vr,
            },
        }
    }

    /// Identification message stores trimmed callsign.
    #[test]
    fn store_callsign() {
        let mut store = AircraftStore::new();
        store.update(&ident_msg(0x4840D6, "KLM1023 "));
        let ac = store.get(0x4840D6).unwrap();
        assert_eq!(ac.callsign.as_deref(), Some("KLM1023"));
    }

    /// Velocity message stores speed/heading/vert-rate.
    #[test]
    fn store_velocity() {
        let mut store = AircraftStore::new();
        store.update(&vel_msg(0x111111, 450.0, 270.0, -512));
        let ac = store.get(0x111111).unwrap();
        assert_eq!(ac.speed_kt, Some(450.0));
        assert_eq!(ac.heading_deg, Some(270.0));
        assert_eq!(ac.vert_rate_fpm, Some(-512));
    }

    /// CPR even+odd pair decodes to position.
    #[test]
    fn store_cpr_position_decodes() {
        let mut store = AircraftStore::new();
        // Known pair: 52.2572°N, 3.9194°E
        store.update(&pos_msg(0x40621D, false, 93000, 51372, 38000)); // even
        store.update(&pos_msg(0x40621D, true, 74158, 50194, 38000));  // odd
        let ac = store.get(0x40621D).unwrap();
        let lat = ac.lat.expect("Lat should be set after even+odd pair");
        let lon = ac.lon.expect("Lon should be set after even+odd pair");
        assert!((lat - 52.2572).abs() < 0.01, "lat={lat:.4}");
        assert!((lon - 3.9194).abs() < 0.01, "lon={lon:.4}");
    }

    /// Single CPR frame (no pair yet) → no position when home is not set.
    #[test]
    fn store_single_cpr_no_position_without_home() {
        let mut store = AircraftStore::new();
        store.update(&pos_msg(0x111111, false, 93000, 51372, 38000)); // even only
        let ac = store.get(0x111111).unwrap();
        assert!(ac.lat.is_none(), "Should not have position from single frame without home");
    }

    /// Single CPR frame with home position set → immediate local CPR decode.
    ///
    /// Uses the known Netherlands test vector (lat 52.26°, lon 3.92°) and seeds
    /// home at (52.0, 4.0) — within 25 nm so the 5°/8° validation box accepts it.
    #[test]
    fn store_single_cpr_with_home_gives_position() {
        let mut store = AircraftStore::new();
        store.set_home_position(52.0, 4.0); // close to the test vector
        store.update(&pos_msg(0x40621D, false, 93000, 51372, 38000)); // even only
        let ac = store.get(0x40621D).unwrap();
        let lat = ac.lat.expect("Home-seeded local CPR should give position on first frame");
        let lon = ac.lon.expect("Home-seeded local CPR should give longitude");
        assert!((lat - 52.2572).abs() < 0.05, "lat={lat:.4}");
        assert!((lon - 3.9194).abs() < 0.05, "lon={lon:.4}");
    }

    /// Aircraft far from home (outside 5°/8° box) does not get a bogus position.
    #[test]
    fn store_single_cpr_far_from_home_no_bogus_position() {
        let mut store = AircraftStore::new();
        store.set_home_position(41.5, -81.7); // Cleveland OH — far from Netherlands
        store.update(&pos_msg(0x40621D, false, 93000, 51372, 38000)); // Netherlands aircraft
        let ac = store.get(0x40621D).unwrap();
        // Local CPR from Cleveland for a Netherlands aircraft is >100° off — must be rejected.
        assert!(ac.lat.is_none(), "Far-away aircraft should not get bogus local CPR position");
    }

    /// Prune removes entries older than 60 s.
    #[test]
    fn prune_expired_removes_old_entries() {
        let mut store = AircraftStore::new();
        store.update(&ident_msg(0xAABBCC, "OLD     "));

        // Fast-forward past the 5-minute expiry window.
        let ac = store.get(0xAABBCC).unwrap();
        let past_expiry = ac.last_seen + Duration::from_secs(301);
        store.prune_expired_at(past_expiry);

        assert!(store.is_empty(), "Expired aircraft should be pruned");
    }

    /// Prune keeps entries seen recently.
    #[test]
    fn prune_keeps_fresh_entries() {
        let mut store = AircraftStore::new();
        store.update(&ident_msg(0xAABBCC, "FRESH   "));

        // Only 10 s elapsed: should survive
        let ac = store.get(0xAABBCC).unwrap();
        let still_fresh = ac.last_seen + Duration::from_secs(10);
        store.prune_expired_at(still_fresh);

        assert_eq!(store.len(), 1, "Fresh aircraft should not be pruned");
    }

    /// Multiple aircraft tracked independently.
    #[test]
    fn tracks_multiple_aircraft() {
        let mut store = AircraftStore::new();
        for i in 0u32..5 {
            store.update(&ident_msg(i, &format!("FLT{i:05}")));
        }
        assert_eq!(store.len(), 5);
        assert_eq!(store.get(2).unwrap().callsign.as_deref(), Some("FLT00002"));
    }
}
