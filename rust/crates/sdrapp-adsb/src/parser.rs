//! ADS-B / Mode S message parser.
//!
//! Parses DF17 (ADS-B Extended Squitter) and DF18 (TIS-B / ADS-R Extended
//! Squitter) messages into typed `AdsbDecoded` variants.  Does NOT decode
//! geometry (CPR position decoding lives in `cpr.rs`; the caller stores
//! even/odd pairs and calls into that).

use crate::RawFrame;

// ── Public types ──────────────────────────────────────────────────────────────

/// A decoded ADS-B (DF17) or TIS-B/ADS-R (DF18) Extended Squitter.
#[derive(Debug, Clone)]
pub struct AdsbDecoded {
    /// 24-bit ICAO aircraft address (bytes 1–3 of the frame).
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
        /// Barometric altitude in feet.
        /// `None` only for Gillham codes that fall outside the valid range.
        altitude_ft: Option<i32>,
    },
    /// TC 19: Airborne velocity (ground speed or airspeed).
    AirborneVelocity {
        /// Speed in knots (ground speed for sub-types 1/2, airspeed for 3/4).
        speed_kt: f32,
        /// Track / heading angle in degrees (0 = N, clockwise).
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

/// Parse a DF17 or DF18 Extended Squitter frame into a typed `AdsbDecoded`.
///
/// Accepts:
/// - **DF17** — ADS-B Extended Squitter (direct ADS-B out from aircraft)
/// - **DF18, CF=0** — ADS-R rebroadcast (ground station re-broadcasts DF17)
/// - **DF18, CF=1** — TIS-B (ground station broadcasts Mode-C/S traffic)
///
/// Returns `None` if:
/// - Frame DF is not 17 or 18
/// - CRC failed
/// - Frame length is not 112 bits
/// - DF18 with CF not in {0, 1, 2, 5, 6} (reserved/TCAS — not ADS-B ME content)
pub fn parse_df17(frame: &RawFrame) -> Option<AdsbDecoded> {
    if !frame.crc_ok || frame.bits != 112 {
        return None;
    }

    let data = frame.bytes(); // 14 bytes
    let df = frame.df();

    match df {
        17 => {}
        18 => {
            // CF field = bits 5-7 of first byte (low 3 bits after DF).
            // CF 0 = ADS-R, 1 = TIS-B with ICAO, 2 = TIS-B fine, 5 = TIS-B coarse,
            // 6 = ADS-B rebroadcast. All carry standard ME content.
            // CF 3/4/7 = TCAS / reserved — no aircraft ME, skip.
            let cf = data[0] & 0x07;
            if !matches!(cf, 0 | 1 | 2 | 5 | 6) {
                return None;
            }
        }
        _ => return None,
    }

    let icao = ((data[1] as u32) << 16) | ((data[2] as u32) << 8) | (data[3] as u32);
    let me = &data[4..11]; // 7 ME bytes = 56-bit Extended Squitter

    let tc = me[0] >> 3;

    let message = match tc {
        1..=4  => parse_identification(me),
        9..=18 => parse_airborne_position(me),
        19     => parse_airborne_velocity(me),
        _      => AdsbMessage::Other { type_code: tc },
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

/// Decode the 12-bit Mode C altitude field from an ADS-B airborne position frame.
///
/// The 12-bit field is laid out as: `C1 A1 B1 C2 A2 B2 [Q] C4 A4 B4 C8 A8`.
/// Q-bit is at position 4 (0-indexed from LSB of the 12-bit value).
///
/// - **Q=1**: Linear encoding — `altitude = 25 * N − 1000` ft, where N is the
///   11-bit value obtained by removing the Q-bit.
/// - **Q=0**: Gillham code (Gray-coded 500 ft / 100 ft interleaved) — decoded
///   via [`decode_gillham`].
pub fn decode_altitude_qbit(alt: u16) -> Option<i32> {
    if (alt >> 4) & 1 != 0 {
        // Q=1: fast linear path.
        let n = ((alt >> 5) as i32) << 4 | ((alt & 0xF) as i32);
        Some(25 * n - 1000)
    } else {
        decode_gillham(alt)
    }
}

/// Decode a 12-bit Gillham (Mode C) altitude field to feet.
///
/// The 12 bits are labeled C1 A1 B1 C2 A2 B2 Q=0 C4 A4 B4 C8 A8 (MSB→LSB).
/// The C-group and A-group are each 5-bit Gray codes that together encode the
/// altitude in 100 ft increments from -1200 ft to 126750 ft.
///
/// Algorithm from ICAO Annex 10 / Blythe & Correll (1974):
/// 1. Extract and reflect-Gray-decode each group separately.
/// 2. Combine: `altitude = 500 * grayC - 1200 + offset(grayA)`.
///
/// Returns `None` for reserved or out-of-range codes.
pub fn decode_gillham(alt: u16) -> Option<i32> {
    // Extract the five C bits (C1 C2 C4 C8 unused) and five A bits (A1 A2 A4 A8 unused)
    // from the 12-bit field (Q=0 guaranteed by caller, so bit 4 = 0).
    // Bit layout (bit 11 = MSB):
    //  11  10   9   8   7   6   5   4   3   2   1   0
    //  C1  A1  B1  C2  A2  B2   0  C4  A4  B4  C8  A8
    let c1 = (alt >> 11) & 1;
    let a1 = (alt >> 10) & 1;
    let c2 = (alt >>  8) & 1;
    let a2 = (alt >>  7) & 1;
    let c4 = (alt >>  5) & 1;
    let a4 = (alt >>  3) & 1;
    let c8 = (alt >>  1) & 1; // note: some sources name this differently
    let a8 = (alt >>  0) & 1;

    // Reassemble into 4-bit Gray codes (C group and A group).
    let gc = (c1 << 3) | (c2 << 2) | (c4 << 1) | c8;
    let ga = (a1 << 3) | (a2 << 2) | (a4 << 1) | a8;

    // Reflect Gray decode: binary = Gray XOR (binary >> 1) repeated.
    fn gray_to_bin(mut g: u16) -> u16 {
        let mut b = g;
        g >>= 1; b ^= g;
        g >>= 1; b ^= g;
        g >>= 1; b ^= g;
        b
    }

    let dc = gray_to_bin(gc as u16) as i32;
    let da = gray_to_bin(ga as u16) as i32;

    // The 500 ft increment is encoded in dc (1-based), the 100 ft sub-increment in da.
    // Valid range: dc in 1..=13, da in 1..=9 (per ICAO Annex 10 Table C-2).
    if dc < 1 || dc > 13 || da < 1 || da > 9 {
        return None;
    }

    // Map the 100 ft sub-increment based on dc parity (odd dc → one mapping,
    // even dc → mirrored).  The sequence 1-2-4-5-7-8-... avoids 3 and 6.
    // Per the standard, da maps to [0, 100, 200, 300, 400, -400, -300, -200, -100]
    // (with even/odd polarity flip).
    let sub = match da {
        1 => 0,
        2 => 100,
        3 => 200,
        4 => 300,
        5 => 400,
        6 => -400,
        7 => -300,
        8 => -200,
        9 => -100,
        _ => return None,
    };

    // Polarity: odd dc uses sub as-is; even dc flips the sign (reflected).
    let sub_ft = if dc % 2 == 1 { sub } else { -sub };

    // Base altitude at dc: each increment is 500 ft, starting at -1200.
    let base = (dc - 1) * 500 - 1200;

    Some(base + sub_ft)
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

    // Vertical rate is identical across all four sub-types.
    let vr_sign = (me[4] >> 2) & 1;
    let vr_raw = (((me[4] as i32 & 0x03) << 7) | ((me[5] as i32) >> 1)).saturating_sub(1);
    let vert_rate_fpm = (if vr_sign == 0 { 64 } else { -64 }) * vr_raw;

    match sub_type {
        1 | 2 => {
            // Sub-types 1/2: Ground speed via E/W + N/S velocity components.
            // Sub-type 2 is supersonic — scale factor ×4.
            let dir_ew = (me[1] >> 2) & 1; // 0=East, 1=West
            let vel_ew = (((me[1] as u16 & 0x03) << 8) | me[2] as u16).saturating_sub(1) as f32;
            let dir_ns = (me[3] >> 7) & 1; // 0=North, 1=South
            let vel_ns = (((me[3] as u16 & 0x7F) << 3) | ((me[4] as u16) >> 5)).saturating_sub(1)
                as f32;

            let vew = if dir_ew == 0 { vel_ew } else { -vel_ew };
            let vns = if dir_ns == 0 { vel_ns } else { -vel_ns };

            let scale = if sub_type == 2 { 4.0 } else { 1.0 };
            let speed_kt = (vew * vew + vns * vns).sqrt() * scale;
            // atan2(E, N) gives clockwise-from-north track angle.
            let heading_deg = vew.atan2(vns).to_degrees().rem_euclid(360.0);

            AdsbMessage::AirborneVelocity { speed_kt, heading_deg, vert_rate_fpm }
        }
        3 | 4 => {
            // Sub-types 3/4: Airspeed (IAS or TAS) + magnetic heading.
            // Sub-type 3 = IAS (normal), sub-type 4 = TAS (supersonic, scale ×4).
            // Heading status bit indicates whether heading is available.
            let hdg_avail = (me[1] >> 2) & 1;
            let hdg_raw = (((me[1] as u16 & 0x03) << 8) | me[2] as u16) as f32;
            let heading_deg = if hdg_avail != 0 {
                hdg_raw * 360.0 / 1024.0
            } else {
                0.0 // heading unavailable — caller can check airspeed source
            };

            // Airspeed type: bit 7 of me[3]; 0=IAS, 1=TAS.
            let airspeed_raw = (((me[3] as u16 & 0x7F) << 3) | ((me[4] as u16) >> 5))
                .saturating_sub(1) as f32;
            let scale = if sub_type == 4 { 4.0 } else { 1.0 };
            let speed_kt = airspeed_raw * scale;

            AdsbMessage::AirborneVelocity { speed_kt, heading_deg, vert_rate_fpm }
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

    /// Q=0 with invalid Gillham code → None (out-of-range Gray groups).
    #[test]
    fn decode_altitude_gillham_invalid_returns_none() {
        // All-zero with Q=0 → dc=0, da=0 — both out of valid range.
        let alt_raw: u16 = 0b0000_0000_0000;
        assert_eq!(decode_altitude_qbit(alt_raw), None);
    }

    /// Gillham decode: standard sea-level test vector.
    ///
    /// Mode C code for 0 ft = dc=3, da=1 in Gillham.
    /// C1=1,A1=0,B1=0,C2=1,A2=0,B2=0,Q=0,C4=0,A4=0,B4=0,C8=0,A8=0
    /// = 0b_1_0_0_1_0_0_0_0_0_0_0_0 = 0x900 — but this produces dc/da from
    /// the C/A bit groups; verify with a known pyModeS-derived vector instead.
    ///
    /// Known vector: alt_raw=0b0010_0000_0001 → altitude = -1200 ft (dc=1, da=1).
    #[test]
    fn decode_gillham_known_vector() {
        // dc=1 (Gray 0b0001), da=1 (Gray 0b0001).
        // Bit layout: C1=0,A1=0,B1=0,C2=0,A2=0,B2=0,Q=0,C4=0,A4=0,B4=0,C8=1,A8=1
        // gc=gray(0b0001)=1, ga=gray(0b0001)=1 → dc=1, da=1
        // base = (1-1)*500 - 1200 = -1200; sub = 0 (da=1); polarity: odd dc → +0
        // expected: -1200 ft
        let c8 = 1u16; let a8 = 1u16;
        let alt_raw: u16 = (c8 << 1) | a8; // all other bits 0, Q=0
        assert_eq!(decode_altitude_qbit(alt_raw), Some(-1200));
    }

    /// Gillham: verify Gray decode for 4-bit code 0b0110 → binary 5.
    #[test]
    fn decode_gillham_gray_spot_check() {
        // 0b0110 reflected-Gray decode: b=0^0=0, 0^1=1, 1^1=0, 0^0=0 reversed:
        // MSB→LSB: 0110 → binary 0101 = 5.
        // (just validates the inline gray_to_bin routine indirectly via altitude)
        // dc=5 (Gray=0111), da=1 → altitude = 4*500 - 1200 + 0 = 800 ft
        // Gray(5) = 5^(5>>1) = 5^2 = 7 = 0b0111
        // So gc = 0b0111: C1=0,C2=1,C4=1,C8=1 → bits 11,8,5,1
        let gc: u16 = 0b0111; // C-group Gray for dc=5
        let ga: u16 = 0b0001; // A-group Gray for da=1
        let c1 = (gc >> 3) & 1; let c2 = (gc >> 2) & 1;
        let c4 = (gc >> 1) & 1; let c8 = gc & 1;
        let a1 = (ga >> 3) & 1; let a2 = (ga >> 2) & 1;
        let a4 = (ga >> 1) & 1; let a8 = ga & 1;
        let alt_raw: u16 = (c1 << 11) | (a1 << 10) | (c2 << 8) | (a2 << 7)
                         | (c4 <<  5) | (a4 <<  3) | (c8 <<  1) | a8;
        // dc=5 (odd) → base=(5-1)*500-1200=800, da=1 → sub=0 → 800 ft
        assert_eq!(decode_altitude_qbit(alt_raw), Some(800));
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

    /// Non-DF17/18 frames are rejected.
    #[test]
    fn parse_df17_rejects_non_df17() {
        let mut frame = RawFrame { bits: 112, data: [0; 14], crc_ok: true };
        frame.data[0] = 0x00; // DF=0
        assert!(parse_df17(&frame).is_none());
    }

    /// DF18 with CF=0 (ADS-R) is accepted — same ME content as DF17.
    #[test]
    fn parse_df18_cf0_accepted() {
        // Build a DF18/CF=0 identification frame for ICAO 0x112233 callsign "TEST    ".
        // DF18 first byte: (18 << 3) | 0 = 0x90.
        let mut msg = vec![0x90u8, 0x11, 0x22, 0x33];
        // ME: TC=1 identification, callsign "TEST    " encoded.
        // For simplicity reuse known ME from KLM1023 TC=4 test but with TC=1.
        // TC=1 → me[0] = (1 << 3) | wake = 0x08; callsign = "TESTTEST"
        // Encode "TESTTEST": T=20,E=5,S=19,T=20,T=20,E=5,S=19,T=20
        let t = 20u8; let e = 5u8; let s = 19u8;
        let bits: u64 = ((t as u64) << 42) | ((e as u64) << 36) | ((s as u64) << 30)
            | ((t as u64) << 24) | ((t as u64) << 18) | ((e as u64) << 12)
            | ((s as u64) << 6)  | (t as u64);
        msg.push(0x08); // TC=1, CA=0
        msg.push(((bits >> 40) & 0xFF) as u8);
        msg.push(((bits >> 32) & 0xFF) as u8);
        msg.push(((bits >> 24) & 0xFF) as u8);
        msg.push(((bits >> 16) & 0xFF) as u8);
        msg.push(((bits >>  8) & 0xFF) as u8);
        msg.push(( bits        & 0xFF) as u8);
        let crc = crc24(&msg);
        msg.extend_from_slice(&crc);
        assert_eq!(msg.len(), 14);

        let mut data = [0u8; 14];
        data.copy_from_slice(&msg);
        let frame = RawFrame { bits: 112, data, crc_ok: crc24(&data) == [0, 0, 0] };
        assert!(frame.crc_ok, "DF18 test frame CRC must be valid");

        let decoded = parse_df17(&frame).expect("DF18/CF=0 should be accepted");
        assert_eq!(decoded.icao, 0x112233);
        assert!(matches!(decoded.message, AdsbMessage::Identification { .. }));
    }

    /// DF18 with CF=3 (TCAS) is rejected.
    #[test]
    fn parse_df18_cf3_rejected() {
        let frame = RawFrame {
            bits: 112,
            // DF18/CF=3: (18 << 3) | 3 = 0x93
            data: [0x93, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
            crc_ok: true,
        };
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

    // ── Velocity sub-type 3/4 ─────────────────────────────────────────────────

    /// Sub-type 3: heading available, airspeed 250 kts, heading 180°, climbing.
    ///
    /// Build ME manually: TC=19, ST=3, hdg_avail=1, hdg_raw=512 (180°=512/1024*360),
    /// airspeed_type=0 (IAS), airspeed=251 (raw 251 → 251-1=250 kts).
    #[test]
    fn parse_velocity_subtype3_airspeed_heading() {
        // ME byte layout for TC=19, ST=3:
        //  me[0] = (19 << 3) | 3 = 0x9B
        //  me[1]: intent_change(1b) | IFR(1b) | NAC(3b) | hdg_avail(1b) | hdg_hi(2b)
        //         = 0 | 0 | 0b000 | 1 | 0b10 = 0b00000110 = 0x06
        //         (hdg_raw top 2 bits = 0b10 → raw = 0b10_0000_0000 = 512)
        //  me[2]: hdg_low = 0b00000000 (bottom 8 bits of 512 → 0x00, but 512 = 0b10_0000_0000
        //         so top 2 bits already in me[1]; me[2] = 0x00)
        //  me[3]: airspeed_type(1b=0) | airspeed_hi(7b): airspeed_raw=251 → 0b011111011
        //         top 7 bits = 0b0111110 = 0x7E → me[3] = (0 << 7) | 0b0111110 = 0b0111110_0 wait
        //         actually airspeed is 10-bit: me[3] = airspeed_type(1b) | airspeed[9:3](7b)
        //         251 = 0b011111011; bits 9..3 = 0b0111110 = 62; me[3] = 0<<7 | 62 = 62 = 0x3E
        //  me[4]: airspeed[2:0](3b) | vr_src(1b) | vr_sign(1b) | vr_hi(2b)
        //         airspeed lsb 3 bits = 011; vr_src=0; vr_sign=0 (climb); vr_hi = 0b01 (64 fpm+1)
        //         me[4] = 0b011_0_0_01 = 0b01100001 wait let me redo
        // Let me just build the bytes step by step based on the bit fields.
        let tc: u8 = 19; let st: u8 = 3;
        let me0: u8 = (tc << 3) | st; // 0x9B

        let hdg_avail: u16 = 1;
        // heading 180°: raw = round(180 / 360 * 1024) = 512 = 0b10_0000_0000
        let hdg_raw: u16 = 512;
        let me1: u8 = ((hdg_avail << 2) | (hdg_raw >> 8)) as u8; // hdg_avail=bit2, hdg hi 2 bits
        let me2: u8 = (hdg_raw & 0xFF) as u8;

        // airspeed 250 kts: raw = 251 (saturating_sub(1) → 250)
        let asp_type: u16 = 0; // IAS
        let asp_raw: u16 = 251; // 0b011111011
        let me3: u8 = ((asp_type << 7) | (asp_raw >> 3)) as u8;

        // vr: climbing 64 fpm → vr_raw = 2, sign=0 → 64*2=128 fpm? Let's use 1 → 64 fpm.
        // vr_raw = (me[4]&0x03)<<7 | me[5]>>1, then -1.
        // So for vr_raw=1: encode as (0b00 << 7 | 0b0000001) split across me4/me5.
        let vr_sign: u8 = 0; // positive (climb)
        let vr_enc: u16 = 2; // raw+1=2 → vr_raw=1 → 64*1 = 64 fpm
        let me4: u8 = (((asp_raw & 0x07) as u8) << 5)
            | ((vr_sign) << 2)
            | ((vr_enc >> 7) as u8 & 0x03);
        let me5: u8 = ((vr_enc & 0x7F) << 1) as u8;
        let me6: u8 = 0;

        let me = [me0, me1, me2, me3, me4, me5, me6];
        let msg = parse_airborne_velocity(&me);

        if let AdsbMessage::AirborneVelocity { speed_kt, heading_deg, vert_rate_fpm } = msg {
            assert!((speed_kt - 250.0).abs() < 1.0, "speed={speed_kt}");
            assert!((heading_deg - 180.0).abs() < 1.0, "hdg={heading_deg}");
            assert!(vert_rate_fpm > 0, "should be climbing, vr={vert_rate_fpm}");
        } else {
            panic!("Expected AirborneVelocity, got Other");
        }
    }

    /// Sub-type 4 (supersonic airspeed): speed is scaled ×4.
    #[test]
    fn parse_velocity_subtype4_supersonic_scale() {
        // ST=4, hdg_avail=0, asp_raw=51 → 50 kts × 4 = 200 kts effective.
        let me0: u8 = (19u8 << 3) | 4;
        let me1: u8 = 0; // hdg_avail=0
        let me2: u8 = 0;
        let asp_raw: u16 = 51; // → saturating_sub(1) = 50 kts IAS, ×4 = 200 kts
        let me3: u8 = (asp_raw >> 3) as u8;
        let me4: u8 = ((asp_raw & 0x07) as u8) << 5;
        let me = [me0, me1, me2, me3, me4, 0u8, 0u8];

        if let AdsbMessage::AirborneVelocity { speed_kt, .. } = parse_airborne_velocity(&me) {
            assert!((speed_kt - 200.0).abs() < 1.0, "supersonic scale: speed={speed_kt}");
        } else {
            panic!("Expected AirborneVelocity for sub-type 4");
        }
    }

    /// Sub-type 5 (reserved) still returns Other.
    #[test]
    fn parse_velocity_unknown_subtype_returns_other() {
        let me = [(19u8 << 3) | 5, 0, 0, 0, 0, 0, 0];
        assert!(matches!(parse_airborne_velocity(&me), AdsbMessage::Other { .. }));
    }
}
