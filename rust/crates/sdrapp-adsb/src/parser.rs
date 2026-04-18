//! DF17 ADS-B message parser.
//!
//! Parses Mode S Downlink Format 17 Extended Squitter messages into typed
//! `AdsbDecoded` variants.  Does NOT decode geometry (CPR position decoding
//! lives in `cpr.rs`; the caller stores even/odd pairs and calls into that).

use crate::RawFrame;

// ── Public types ──────────────────────────────────────────────────────────────

/// A decoded DF17 ADS-B Extended Squitter.
#[derive(Debug, Clone)]
pub struct AdsbDecoded {
    /// 24-bit ICAO aircraft address (bytes 1–3 of the DF17 frame).
    pub icao: u32,
    /// Parsed message payload.
    pub message: AdsbMessage,
}

/// ADS-B message variants produced by the parser.
#[derive(Debug, Clone)]
pub enum AdsbMessage {
    /// TC 1–4: Aircraft identification and wake-vortex category.
    Identification {
        /// 8-character callsign, space-padded, ASCII printable.
        callsign: [u8; 8],
    },
    /// TC 9–18: Airborne position (altitude + CPR lat/lon fragment).
    AirbornePosition {
        /// `false` = even CPR frame, `true` = odd CPR frame.
        cpr_odd: bool,
        /// 17-bit encoded latitude fraction.
        lat_cpr: u32,
        /// 17-bit encoded longitude fraction.
        lon_cpr: u32,
        /// Barometric altitude in feet, `None` if Q=0 (Gillham code).
        altitude_ft: Option<i32>,
    },
    /// TC 19: Airborne velocity.
    AirborneVelocity {
        /// Ground speed in knots.
        speed_kt: f32,
        /// Track angle in degrees (0 = N, clockwise).
        heading_deg: f32,
        /// Vertical rate in feet per minute (positive = climb).
        vert_rate_fpm: i32,
    },
    /// TC not handled by this parser.
    Other {
        type_code: u8,
    },
}

// ── Entry point ───────────────────────────────────────────────────────────────

/// Parse a DF17 ADS-B frame into a typed `AdsbDecoded`.
///
/// Returns `None` if:
/// - Frame DF ≠ 17
/// - CRC failed
/// - Frame length is not 112 bits
pub fn parse_df17(frame: &RawFrame) -> Option<AdsbDecoded> {
    if frame.df() != 17 || !frame.crc_ok || frame.bits != 112 {
        return None;
    }

    let data = frame.bytes(); // 14 bytes
    let icao = ((data[1] as u32) << 16) | ((data[2] as u32) << 8) | (data[3] as u32);
    let me = &data[4..11]; // 7 ME bytes = 56-bit Extended Squitter

    let tc = me[0] >> 3;

    let message = match tc {
        1..=4 => parse_identification(me),
        9..=18 => parse_airborne_position(me),
        19 => parse_airborne_velocity(me),
        _ => AdsbMessage::Other { type_code: tc },
    };

    Some(AdsbDecoded { icao, message })
}

// ── Identification (TC 1–4) ───────────────────────────────────────────────────

/// ADS-B 6-bit character set → ASCII.
/// 1–26 = 'A'–'Z', 32 = ' ', 48–57 = '0'–'9', else '#' (invalid/reserved).
fn adsb_char(code: u8) -> u8 {
    match code {
        1..=26 => b'A' + code - 1,
        32 => b' ',
        48..=57 => b'0' + code - 48,
        _ => b'#',
    }
}

fn parse_identification(me: &[u8]) -> AdsbMessage {
    // ME bits 8–55 (6 bytes): 8 × 6-bit character codes.
    let bits: u64 = ((me[1] as u64) << 40)
        | ((me[2] as u64) << 32)
        | ((me[3] as u64) << 24)
        | ((me[4] as u64) << 16)
        | ((me[5] as u64) << 8)
        | (me[6] as u64);

    let mut callsign = [b' '; 8];
    for (i, slot) in callsign.iter_mut().enumerate() {
        let code = ((bits >> (42 - i * 6)) & 0x3F) as u8;
        *slot = adsb_char(code);
    }

    AdsbMessage::Identification { callsign }
}

// ── Airborne position (TC 9–18) ───────────────────────────────────────────────

/// Decode the Q-bit altitude encoding from a 12-bit altitude field.
///
/// Q-bit is at bit 4 (0-indexed from LSB).
/// When Q=1: `altitude = 25 * N − 1000` feet, where N is the 11-bit value
/// obtained by removing the Q-bit.
/// When Q=0 (Gillham code): returns `None`.
pub fn decode_altitude_qbit(alt: u16) -> Option<i32> {
    if (alt >> 4) & 1 == 0 {
        return None; // Gillham code — not handled
    }
    let n = ((alt >> 5) as i32) << 4 | ((alt & 0xF) as i32);
    Some(25 * n - 1000)
}

fn parse_airborne_position(me: &[u8]) -> AdsbMessage {
    // ALT: me[1] (high 8 bits) and me[2] bits 7..4 (low 4 bits)
    let alt_raw = ((me[1] as u16) << 4) | ((me[2] as u16) >> 4);
    let altitude_ft = decode_altitude_qbit(alt_raw);

    // F (CPR format): me[2] bit 2
    let cpr_odd = (me[2] >> 2) & 1 != 0;

    // LAT-CPR: me[2] bits 1..0 (2 MSBs), me[3] (8 bits), me[4] bits 7..1 (7 bits)
    let lat_cpr = ((me[2] as u32 & 0x03) << 15) | ((me[3] as u32) << 7) | ((me[4] as u32) >> 1);

    // LON-CPR: me[4] bit 0 (1 MSB), me[5] (8 bits), me[6] (8 bits)
    let lon_cpr = ((me[4] as u32 & 0x01) << 16) | ((me[5] as u32) << 8) | (me[6] as u32);

    AdsbMessage::AirbornePosition {
        cpr_odd,
        lat_cpr,
        lon_cpr,
        altitude_ft,
    }
}

// ── Airborne velocity (TC 19) ─────────────────────────────────────────────────

fn parse_airborne_velocity(me: &[u8]) -> AdsbMessage {
    let sub_type = me[0] & 0x07;

    match sub_type {
        1 | 2 => {
            // Ground speed, sub-type 1 (normal) or 2 (supersonic)
            let dir_ew = (me[1] >> 2) & 1; // 0=East, 1=West
            let vel_ew = (((me[1] as u16 & 0x03) << 8) | me[2] as u16).saturating_sub(1) as f32;
            let dir_ns = (me[3] >> 7) & 1; // 0=North, 1=South
            let vel_ns = (((me[3] as u16 & 0x7F) << 3) | ((me[4] as u16) >> 5)).saturating_sub(1)
                as f32;

            let vew = if dir_ew == 0 { vel_ew } else { -vel_ew };
            let vns = if dir_ns == 0 { vel_ns } else { -vel_ns };

            let scale = if sub_type == 2 { 4.0 } else { 1.0 };
            let speed_kt = (vew * vew + vns * vns).sqrt() * scale;
            let heading_deg = vew.atan2(vns).to_degrees().rem_euclid(360.0);

            let vr_src = (me[4] >> 3) & 1; // 0 = geometric, 1 = barometric
            let _ = vr_src;
            let vr_sign = (me[4] >> 2) & 1;
            let vr_raw = (((me[4] as i32 & 0x03) << 7) | ((me[5] as i32) >> 1)).saturating_sub(1);
            let vert_rate_fpm = (if vr_sign == 0 { 64 } else { -64 }) * vr_raw;

            AdsbMessage::AirborneVelocity {
                speed_kt,
                heading_deg,
                vert_rate_fpm,
            }
        }
        _ => AdsbMessage::Other { type_code: 19 },
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{crc24, RawFrame};

    /// Helper: build a RawFrame from 14 known-good bytes (CRC must be valid).
    fn frame_from_bytes(bytes: &[u8; 14]) -> RawFrame {
        let mut data = [0u8; 14];
        data.copy_from_slice(bytes);
        // Append correct CRC: validate full frame
        let crc_ok = crc24(bytes) == [0, 0, 0];
        RawFrame {
            bits: 112,
            data,
            crc_ok,
        }
    }

    // ── Identification ────────────────────────────────────────────────────────

    /// Known frame: "8D4840D6202CC371C32CE0576098"
    /// Source: widely-cited ADS-B test vector (KLM1023, ICAO 4840D6).
    #[test]
    fn parse_identification_klm1023() {
        let bytes: [u8; 14] = [
            0x8D, 0x48, 0x40, 0xD6, // DF=17, ICAO=4840D6
            0x20, 0x2C, 0xC3, 0x71, 0xC3, 0x2C, 0xE0, // ME (TC=4, "KLM1023 ")
            0x57, 0x60, 0x98, // CRC
        ];
        let frame = frame_from_bytes(&bytes);
        assert!(frame.crc_ok, "CRC should be valid for known test vector");

        let decoded = parse_df17(&frame).expect("Should parse DF17");
        assert_eq!(decoded.icao, 0x4840D6);

        if let AdsbMessage::Identification { callsign } = decoded.message {
            let cs = std::str::from_utf8(&callsign).unwrap();
            assert_eq!(cs, "KLM1023 ", "Callsign mismatch: got {cs:?}");
        } else {
            panic!("Expected Identification message");
        }
    }

    /// Callsign charset: letters A–Z.
    #[test]
    fn adsb_char_letters() {
        for (i, &c) in (1u8..=26).zip(b"ABCDEFGHIJKLMNOPQRSTUVWXYZ".iter()) {
            assert_eq!(adsb_char(i), c);
        }
    }

    /// Callsign charset: digits 0–9.
    #[test]
    fn adsb_char_digits() {
        for (i, &c) in (48u8..=57).zip(b"0123456789".iter()) {
            assert_eq!(adsb_char(i), c);
        }
    }

    /// Callsign charset: space.
    #[test]
    fn adsb_char_space() {
        assert_eq!(adsb_char(32), b' ');
    }

    // ── Altitude decoding ─────────────────────────────────────────────────────

    /// Q=1 encoding: altitude 38000 ft → N=1560, alt_raw=0xC38.
    #[test]
    fn decode_altitude_38000ft() {
        let alt_raw = 0xC38u16;
        assert_eq!((alt_raw >> 4) & 1, 1, "Q-bit should be 1");
        let alt = decode_altitude_qbit(alt_raw).unwrap();
        assert_eq!(alt, 38000);
    }

    /// Q=1 encoding: N=1 → altitude = 25*1 - 1000 = -975 ft.
    #[test]
    fn decode_altitude_minimum() {
        // N=1: smallest positive N.
        // alt_raw with Q=1 and N=1: N=1 → upper bits = (1>>4)=0, lower bits = 1&0xF=1
        // alt_raw = (0 << 5) | (1 << 4) | 1 = 0b0000_0001_0001 = 0x011
        let alt_raw: u16 = (1 << 4) | 1; // Q=1 at bit 4, N lower = 1
        let n = ((alt_raw >> 5) << 4) | (alt_raw & 0xF); // should be 1
        assert_eq!(n, 1);
        let alt = decode_altitude_qbit(alt_raw).unwrap();
        assert_eq!(alt, 25 * 1 - 1000);
    }

    /// Q=0 → Gillham code, returns None.
    #[test]
    fn decode_altitude_gillham_returns_none() {
        let alt_raw: u16 = 0b0000_1100_0000; // Q-bit (bit 4) = 0
        assert_eq!(decode_altitude_qbit(alt_raw), None);
    }

    // ── Airborne position ─────────────────────────────────────────────────────

    /// Known frame "8D40621D58C382D690C8AC2863A7" (even position, TC=11, 38000 ft)
    /// Source: pyModeS test suite.
    #[test]
    fn parse_position_even_38000ft() {
        let bytes: [u8; 14] = [
            0x8D, 0x40, 0x62, 0x1D, // DF=17, ICAO=40621D
            0x58, 0xC3, 0x82, 0xD6, 0x90, 0xC8, 0xAC, // ME TC=11
            0x28, 0x63, 0xA7, // CRC
        ];
        let frame = frame_from_bytes(&bytes);
        assert!(frame.crc_ok, "CRC should be valid");

        let decoded = parse_df17(&frame).expect("Should parse DF17");
        assert_eq!(decoded.icao, 0x40621D);

        if let AdsbMessage::AirbornePosition {
            cpr_odd,
            lat_cpr,
            lon_cpr,
            altitude_ft,
        } = decoded.message
        {
            assert!(!cpr_odd, "Should be even frame (F=0)");
            assert_eq!(lat_cpr, 93000, "LAT-CPR mismatch");
            assert_eq!(lon_cpr, 51372, "LON-CPR mismatch");
            assert_eq!(altitude_ft, Some(38000), "Altitude should be 38000 ft");
        } else {
            panic!("Expected AirbornePosition");
        }
    }

    // ── parse_df17 rejects invalid inputs ────────────────────────────────────

    /// Non-DF17 frames are rejected.
    #[test]
    fn parse_df17_rejects_non_df17() {
        let mut frame = RawFrame {
            bits: 112,
            data: [0; 14],
            crc_ok: true,
        };
        frame.data[0] = 0x00; // DF=0
        assert!(parse_df17(&frame).is_none());
    }

    /// CRC-failed frames are rejected.
    #[test]
    fn parse_df17_rejects_bad_crc() {
        let frame = RawFrame {
            bits: 112,
            data: [0x8D, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
            crc_ok: false,
        };
        assert!(parse_df17(&frame).is_none());
    }

    /// Short frames (56 bits) are rejected.
    #[test]
    fn parse_df17_rejects_short_frame() {
        let frame = RawFrame {
            bits: 56,
            data: [0x8D, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
            crc_ok: true,
        };
        assert!(parse_df17(&frame).is_none());
    }
}
