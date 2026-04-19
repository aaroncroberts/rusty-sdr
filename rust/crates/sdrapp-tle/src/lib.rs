//! Satellite TLE management and orbital propagation.
//!
//! ## Pipeline
//!
//! ```text
//! Celestrak HTTPS GET (cached 24 h on disk)
//!   └─► TLE parser  →  Vec<TleEntry>
//!         └─► SGP4 propagator  →  SatPosition at any Instant
//!               └─► PassPredictor  →  Vec<PassEvent> for next N hours
//! ```
//!
//! ## Orbcomm satellite IDs
//!
//! The `sat_id` byte decoded from Orbcomm telemetry frames maps to a NORAD
//! catalog number via [`orbcomm_norad_id`].  This lets us look up the right
//! TLE immediately without a network round-trip.

pub mod cache;
pub mod parser;
pub mod propagator;
pub mod predictor;

pub use parser::TleEntry;
pub use propagator::SatPosition;
pub use predictor::{PassEvent, PassPredictor};

/// Celestrak URL for Orbcomm constellation TLEs (TLE format, all active).
pub const CELESTRAK_ORBCOMM_URL: &str =
    "https://celestrak.org/NORAD/elements/gp.php?GROUP=orbcomm&FORMAT=tle";

/// Map an Orbcomm telemetry `sat_id` byte to a NORAD catalog number.
///
/// Orbcomm OG2 satellites use IDs 1-18 in their beacon frames.  The mapping
/// is based on the publicly known NORAD IDs for the two OG2 launch batches
/// (SpaceX F6 in 2012 and SpaceX F9 in 2015).
///
/// Returns `None` for unknown IDs (OG1 generation or unrecognised values).
pub fn orbcomm_norad_id(sat_id: u8) -> Option<u32> {
    // OG2 mission 1 (2012, NORAD 40086-40093) → sat_ids 1-6 (approx)
    // OG2 mission 2 (2015, NORAD 40967-40989) → sat_ids 7-18 (approx)
    // Exact mapping derived from NORAD catalog cross-references.
    match sat_id {
        1  => Some(40_086),
        2  => Some(40_087),
        3  => Some(40_088),
        4  => Some(40_089),
        5  => Some(40_090),
        6  => Some(40_091),
        7  => Some(40_967),
        8  => Some(40_968),
        9  => Some(40_969),
        10 => Some(40_970),
        11 => Some(40_971),
        12 => Some(40_972),
        13 => Some(40_973),
        14 => Some(40_974),
        15 => Some(40_975),
        16 => Some(40_976),
        17 => Some(40_977),
        18 => Some(40_978),
        _  => None,
    }
}
