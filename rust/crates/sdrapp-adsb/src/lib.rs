//! ADS-B 1090 MHz PPM demodulator and raw Mode S frame extractor.
//!
//! Implements:
//! - CRC-24 validation (Mode S polynomial 0xFFF409)
//! - PPM preamble detection (16-sample pattern at 2 Msps)
//! - Bit slicing: 2 samples/bit, first-half vs second-half amplitude
//! - Raw frame extraction (56 or 112 data bits)
//!
//! Does NOT perform semantic ADS-B parsing — that lives in the layer above.

pub mod cpr;
pub mod parser;
pub mod state;

pub use cpr::{decode_global, CprFrame};
pub use parser::{parse_df17, AdsbDecoded, AdsbMessage};
pub use state::{AircraftState, AircraftStore};

// ── Constants ─────────────────────────────────────────────────────────────────

/// Mode S CRC-24 generator polynomial: x^24+x^23+x^10+x^3+x+1 = 0xFFF409
const MODES_POLY: u32 = 0xFFF409;

/// Preamble length in magnitude samples at 2 Msps.
pub const PREAMBLE_LEN: usize = 16;

/// Magnitude samples per data bit at 2 Msps (1 µs/bit × 2 samples/µs).
pub const SAMPLES_PER_BIT: usize = 2;

/// Short Mode S frame length in bits (DF 0–10, 12–16).
pub const SHORT_MSG_BITS: usize = 56;

/// Long Mode S frame length in bits (DF 11, 17–19).
pub const LONG_MSG_BITS: usize = 112;

/// Preamble high-sample positions (indices into a 16-sample window).
/// Correspond to pulses at 0, 1, 3.5, 4.5 µs (ICAO Annex 10).
const PREAMBLE_HIGH: [usize; 4] = [0, 2, 7, 9];

/// Preamble low-sample positions.
const PREAMBLE_LOW: [usize; 12] = [1, 3, 4, 5, 6, 8, 10, 11, 12, 13, 14, 15];

// ── CRC-24 ────────────────────────────────────────────────────────────────────

/// Compute Mode S CRC-24 over `data`.
///
/// For a valid complete frame (message bytes **including** the appended CRC
/// field), this returns `[0, 0, 0]`.
///
/// To compute the CRC to append: pass the message bytes **excluding** the
/// last 3 bytes; append the returned 3 bytes.
pub fn crc24(data: &[u8]) -> [u8; 3] {
    let mut crc: u32 = 0;
    for &byte in data {
        crc ^= (byte as u32) << 16;
        for _ in 0..8 {
            crc <<= 1;
            if crc & 0x0100_0000 != 0 {
                crc ^= MODES_POLY;
            }
        }
    }
    crc &= 0x00FF_FFFF;
    [(crc >> 16) as u8, ((crc >> 8) & 0xFF) as u8, (crc & 0xFF) as u8]
}

// ── Preamble detection ────────────────────────────────────────────────────────

/// Detect a Mode S preamble in a 16-sample magnitude window.
///
/// Returns `true` when the mean of the four high-position samples is at
/// least 3× the mean of the twelve low-position samples and above a minimum
/// absolute threshold, indicating a valid PPM preamble.
pub fn detect_preamble(samples: &[f32]) -> bool {
    if samples.len() < PREAMBLE_LEN {
        return false;
    }

    let high_mean: f32 =
        PREAMBLE_HIGH.iter().map(|&i| samples[i]).sum::<f32>() / PREAMBLE_HIGH.len() as f32;
    let low_mean: f32 =
        PREAMBLE_LOW.iter().map(|&i| samples[i]).sum::<f32>() / PREAMBLE_LOW.len() as f32;

    // Require: high positions are at least 3× the low positions, and above noise floor.
    high_mean > low_mean * 3.0 && high_mean > 0.01
}

// ── Bit / byte extraction ─────────────────────────────────────────────────────

/// Decode one byte (8 bits) from 16 consecutive magnitude samples.
///
/// Each bit occupies 2 samples:
/// - `samples[2k] > samples[2k+1]` → bit k = 1
/// - `samples[2k] ≤ samples[2k+1]` → bit k = 0
fn decode_byte(samples: &[f32]) -> u8 {
    let mut byte = 0u8;
    for bit_idx in 0..8 {
        if samples[bit_idx * 2] > samples[bit_idx * 2 + 1] {
            byte |= 1 << (7 - bit_idx);
        }
    }
    byte
}

/// Decode `num_bits` bits from `samples` into a byte array (up to 14 bytes).
///
/// Caller must ensure `samples.len() >= num_bits * SAMPLES_PER_BIT`.
fn decode_bits(samples: &[f32], num_bits: usize) -> [u8; 14] {
    let mut data = [0u8; 14];
    let num_bytes = num_bits / 8;
    for (byte_idx, slot) in data.iter_mut().enumerate().take(num_bytes) {
        let offset = byte_idx * 8 * SAMPLES_PER_BIT;
        *slot = decode_byte(&samples[offset..offset + 8 * SAMPLES_PER_BIT]);
    }
    data
}

// ── Raw frame type ────────────────────────────────────────────────────────────

/// A raw decoded Mode S frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawFrame {
    /// Number of data bits: 56 (short) or 112 (long).
    pub bits: usize,
    /// Decoded frame bytes, including the 3-byte CRC/PI field.
    /// Valid indices: `0..bits/8`.
    pub data: [u8; 14],
    /// `true` if CRC-24 validation passed.
    pub crc_ok: bool,
}

impl RawFrame {
    /// Return only the valid data bytes (length = `self.bits / 8`).
    pub fn bytes(&self) -> &[u8] {
        &self.data[..self.bits / 8]
    }

    /// Downlink Format field (5 MSBs of first byte).
    pub fn df(&self) -> u8 {
        self.data[0] >> 3
    }
}

// ── PPM demodulator ───────────────────────────────────────────────────────────

/// PPM demodulator for 1090 MHz Mode S at 2 Msps.
///
/// Feed interleaved IQ samples (f32 pairs, ±1.0 range) via [`process`].
/// Maintains an internal sample buffer for streaming across packet boundaries.
pub struct PpmDemodulator {
    /// Buffered magnitude samples not yet consumed by frame search.
    buf: Vec<f32>,
}

impl Default for PpmDemodulator {
    fn default() -> Self {
        Self::new()
    }
}

impl PpmDemodulator {
    pub fn new() -> Self {
        Self {
            buf: Vec::with_capacity(512),
        }
    }

    /// Feed interleaved IQ samples and return any complete frames found.
    ///
    /// Input layout: `[I0, Q0, I1, Q1, ...]`.
    /// Magnitude is computed as `sqrt(I² + Q²)`.
    pub fn process(&mut self, iq: &[f32]) -> Vec<RawFrame> {
        // Convert IQ pairs to magnitude.
        self.buf.extend(iq.chunks_exact(2).map(|c| {
            let (i, q) = (c[0], c[1]);
            (i * i + q * q).sqrt()
        }));

        let mut frames = Vec::new();
        let min_needed = PREAMBLE_LEN + LONG_MSG_BITS * SAMPLES_PER_BIT;
        let mut pos = 0;

        while pos + min_needed <= self.buf.len() {
            match self.try_decode_at(pos) {
                Some(frame) => {
                    let advance = PREAMBLE_LEN + frame.bits * SAMPLES_PER_BIT;
                    frames.push(frame);
                    pos += advance;
                }
                None => {
                    pos += 1;
                }
            }
        }

        self.buf.drain(..pos);
        frames
    }

    /// Attempt to decode a frame at `buf[pos..]`.
    fn try_decode_at(&self, pos: usize) -> Option<RawFrame> {
        let window = &self.buf[pos..];

        if !detect_preamble(&window[..PREAMBLE_LEN]) {
            return None;
        }

        let data_samples = &window[PREAMBLE_LEN..];
        if data_samples.len() < LONG_MSG_BITS * SAMPLES_PER_BIT {
            return None;
        }

        // Peek at first byte to determine frame length from DF.
        let first_byte = decode_byte(&data_samples[..8 * SAMPLES_PER_BIT]);
        let df = first_byte >> 3;

        let num_bits = if matches!(df, 11 | 17 | 18 | 19) {
            LONG_MSG_BITS
        } else {
            SHORT_MSG_BITS
        };

        if data_samples.len() < num_bits * SAMPLES_PER_BIT {
            return None;
        }

        let data = decode_bits(data_samples, num_bits);
        let num_bytes = num_bits / 8;
        let crc_ok = crc24(&data[..num_bytes]) == [0, 0, 0];

        Some(RawFrame {
            bits: num_bits,
            data,
            crc_ok,
        })
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── CRC-24 ────────────────────────────────────────────────────────────────

    /// CRC-24 round-trip: compute CRC, append, verify remainder = [0,0,0].
    #[test]
    fn crc24_round_trip() {
        let msg = [0x8D_u8, 0x48, 0x40, 0xD6, 0x20, 0x2C, 0xC3, 0x71, 0xC3, 0x2C, 0xE0];
        let crc = crc24(&msg);
        let mut full = msg.to_vec();
        full.extend_from_slice(&crc);
        assert_eq!(crc24(&full), [0, 0, 0], "CRC-24 round-trip failed");
    }

    /// Single-byte CRC round-trip.
    #[test]
    fn crc24_single_byte_round_trip() {
        let msg = [0xAB_u8];
        let crc = crc24(&msg);
        let mut full = msg.to_vec();
        full.extend_from_slice(&crc);
        assert_eq!(crc24(&full), [0, 0, 0]);
    }

    /// Zero message round-trip.
    #[test]
    fn crc24_all_zeros_round_trip() {
        let msg = [0u8; 7];
        let crc = crc24(&msg);
        let mut full = msg.to_vec();
        full.extend_from_slice(&crc);
        assert_eq!(crc24(&full), [0, 0, 0]);
    }

    /// Different data produces different CRCs.
    #[test]
    fn crc24_different_data_different_crc() {
        let a = crc24(&[0x01, 0x02, 0x03]);
        let b = crc24(&[0x01, 0x02, 0x04]);
        assert_ne!(a, b);
    }

    // ── Preamble detection ────────────────────────────────────────────────────

    /// Ideal preamble: high only at positions 0,2,7,9.
    #[test]
    fn detect_preamble_ideal() {
        let mut s = [0.0f32; 16];
        for &p in &PREAMBLE_HIGH {
            s[p] = 1.0;
        }
        assert!(detect_preamble(&s), "Ideal preamble not detected");
    }

    /// All-zero signal → no preamble.
    #[test]
    fn detect_preamble_all_zeros() {
        assert!(!detect_preamble(&[0.0f32; 16]));
    }

    /// Uniform signal (no contrast) → no preamble.
    #[test]
    fn detect_preamble_uniform() {
        assert!(!detect_preamble(&[0.5f32; 16]));
    }

    /// Inverted pattern (high at low positions) → no preamble.
    #[test]
    fn detect_preamble_inverted() {
        let mut s = [0.0f32; 16];
        for &p in &PREAMBLE_LOW {
            s[p] = 1.0;
        }
        assert!(!detect_preamble(&s), "Inverted preamble should not be detected");
    }

    /// Noisy preamble (small high offset) → no preamble below noise floor.
    #[test]
    fn detect_preamble_noise_floor() {
        let mut s = [0.0f32; 16];
        for &p in &PREAMBLE_HIGH {
            s[p] = 0.005; // below 0.01 threshold
        }
        assert!(!detect_preamble(&s));
    }

    // ── Byte / bit decoding ───────────────────────────────────────────────────

    /// Decode all-one bits: each bit is [high, low] → byte = 0xFF.
    #[test]
    fn decode_byte_all_ones() {
        let mut s = [0.0f32; 16];
        for bit in 0..8 {
            s[bit * 2] = 1.0;
            s[bit * 2 + 1] = 0.0;
        }
        assert_eq!(decode_byte(&s), 0xFF);
    }

    /// Decode all-zero bits: each bit is [low, high] → byte = 0x00.
    #[test]
    fn decode_byte_all_zeros() {
        let mut s = [0.0f32; 16];
        for bit in 0..8 {
            s[bit * 2] = 0.0;
            s[bit * 2 + 1] = 1.0;
        }
        assert_eq!(decode_byte(&s), 0x00);
    }

    /// Decode alternating bits: 10101010 = 0xAA.
    #[test]
    fn decode_byte_alternating() {
        let mut s = [0.0f32; 16];
        for bit in 0..8 {
            if bit % 2 == 0 {
                s[bit * 2] = 1.0;
                s[bit * 2 + 1] = 0.0;
            } else {
                s[bit * 2] = 0.0;
                s[bit * 2 + 1] = 1.0;
            }
        }
        assert_eq!(decode_byte(&s), 0xAA);
    }

    // ── End-to-end demodulator ────────────────────────────────────────────────

    /// Helper: build a synthetic magnitude buffer from preamble + bit pattern.
    fn make_preamble_mag() -> Vec<f32> {
        let mut v = vec![0.0f32; PREAMBLE_LEN];
        for &p in &PREAMBLE_HIGH {
            v[p] = 1.0;
        }
        v
    }

    /// Encode a byte as 16 magnitude samples (2 samples/bit, PPM).
    fn encode_byte_mag(byte: u8) -> Vec<f32> {
        let mut v = Vec::with_capacity(16);
        for bit in 0..8 {
            let is_one = (byte >> (7 - bit)) & 1 == 1;
            if is_one {
                v.push(1.0);
                v.push(0.0);
            } else {
                v.push(0.0);
                v.push(1.0);
            }
        }
        v
    }

    /// Build synthetic magnitude buffer for a complete frame.
    fn encode_frame_mag(frame_bytes: &[u8]) -> Vec<f32> {
        let mut v = make_preamble_mag();
        for &b in frame_bytes {
            v.extend(encode_byte_mag(b));
        }
        v
    }

    /// Convert a magnitude buffer to synthetic IQ (mag as I, Q=0).
    fn mag_to_iq(mags: &[f32]) -> Vec<f32> {
        let mut iq = Vec::with_capacity(mags.len() * 2);
        for &m in mags {
            iq.push(m);
            iq.push(0.0);
        }
        iq
    }

    /// Demodulator on empty input → no frames.
    #[test]
    fn demodulator_empty_input() {
        let mut demod = PpmDemodulator::new();
        assert!(demod.process(&[]).is_empty());
    }

    /// Demodulator decodes a synthetic valid short frame (DF=0, 56 bits).
    #[test]
    fn demodulator_decodes_synthetic_short_frame() {
        // DF=0 → first byte = 0b00000XXX = 0x00..0x07
        // Use [0x02, data..., crc0, crc1, crc2]
        let mut msg = vec![0x02u8, 0x48, 0x40, 0xD6];
        let crc = crc24(&msg);
        msg.extend_from_slice(&crc);
        assert_eq!(msg.len(), 7); // 56 bits

        // Verify round-trip CRC
        assert_eq!(crc24(&msg), [0, 0, 0]);

        let mags = encode_frame_mag(&msg);
        // Pad with trailing silence so demodulator has enough lookahead
        let mut iq = mag_to_iq(&mags);
        iq.extend(vec![0.0f32; PREAMBLE_LEN * 2 * 2 + LONG_MSG_BITS * SAMPLES_PER_BIT * 2]);

        let mut demod = PpmDemodulator::new();
        let frames = demod.process(&iq);

        assert!(!frames.is_empty(), "Expected at least one frame");
        let f = &frames[0];
        assert_eq!(f.bits, SHORT_MSG_BITS);
        assert!(f.crc_ok, "CRC should pass for synthetic frame");
        assert_eq!(f.bytes()[..4], msg[..4]);
        assert_eq!(f.df(), 0);
    }

    /// Demodulator decodes a synthetic valid long frame (DF=17, 112 bits).
    #[test]
    fn demodulator_decodes_synthetic_long_frame() {
        // DF=17 → first byte = 0b10001XXX = 0x88..0x8F
        // Use 0x8D (DF=17, some spare bits)
        let mut msg = vec![
            0x8D_u8, 0x40, 0x62, 0x1D, 0x58, 0xC3, 0x82, 0xD6, 0x90, 0xC8, 0xAC,
        ];
        // Pad to 11 bytes (need 14 total with 3 CRC)
        while msg.len() < 11 {
            msg.push(0x00);
        }
        let crc = crc24(&msg);
        msg.extend_from_slice(&crc);
        assert_eq!(msg.len(), 14); // 112 bits

        assert_eq!(crc24(&msg), [0, 0, 0]);

        let mags = encode_frame_mag(&msg);
        let mut iq = mag_to_iq(&mags);
        iq.extend(vec![0.0f32; PREAMBLE_LEN * 2 * 2 + LONG_MSG_BITS * SAMPLES_PER_BIT * 2]);

        let mut demod = PpmDemodulator::new();
        let frames = demod.process(&iq);

        assert!(!frames.is_empty(), "Expected at least one frame");
        let f = &frames[0];
        assert_eq!(f.bits, LONG_MSG_BITS);
        assert!(f.crc_ok, "CRC should pass for synthetic long frame");
        assert_eq!(f.df(), 17);
    }

    /// Demodulator rejects frame with bit-flipped CRC (crc_ok = false).
    #[test]
    fn demodulator_rejects_bad_crc() {
        let mut msg = vec![0x02u8, 0x48, 0x40, 0xD6];
        let crc = crc24(&msg);
        msg.extend_from_slice(&crc);
        // Corrupt one CRC byte
        let last = msg.len() - 1;
        msg[last] ^= 0xFF;

        let mags = encode_frame_mag(&msg);
        let mut iq = mag_to_iq(&mags);
        iq.extend(vec![0.0f32; PREAMBLE_LEN * 2 * 2 + LONG_MSG_BITS * SAMPLES_PER_BIT * 2]);

        let mut demod = PpmDemodulator::new();
        let frames = demod.process(&iq);

        // Frame may or may not be found (preamble is valid), but if found CRC must fail
        for f in &frames {
            if f.bytes()[..4] == msg[..4] {
                assert!(!f.crc_ok, "Corrupted CRC frame should fail validation");
            }
        }
    }

    /// Demodulator processes IQ in multiple chunks (streaming behavior).
    #[test]
    fn demodulator_streaming_chunks() {
        let mut msg = vec![0x02u8, 0x10, 0x20, 0x30];
        let crc = crc24(&msg);
        msg.extend_from_slice(&crc);

        let mags = encode_frame_mag(&msg);
        let mut iq = mag_to_iq(&mags);
        iq.extend(vec![0.0f32; PREAMBLE_LEN * 2 * 2 + LONG_MSG_BITS * SAMPLES_PER_BIT * 2]);

        let mut demod = PpmDemodulator::new();
        let mut all_frames = Vec::new();

        // Feed in 32-sample IQ chunks (16 magnitude samples per chunk)
        for chunk in iq.chunks(32) {
            all_frames.extend(demod.process(chunk));
        }

        assert!(
            all_frames.iter().any(|f| f.crc_ok && f.bits == SHORT_MSG_BITS),
            "Streaming decoding should find the frame"
        );
    }
}
