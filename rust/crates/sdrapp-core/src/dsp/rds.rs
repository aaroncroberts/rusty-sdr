#![forbid(unsafe_code)]

//! RDS (Radio Data System) decoder for WBFM broadcast.
//!
//! Extracts the 57 kHz RDS subcarrier from the composite FM baseband,
//! demodulates DBPSK at 1187.5 bps, syncs to 26-bit blocks using
//! CRC-10 syndrome matching, and assembles the 8-character Programme
//! Service (PS) name from Group 0 transmissions.
//!
//! ## Signal chain
//! ```text
//! composite baseband (fs Hz)
//!   → multiply by 57 kHz NCO  (shifts RDS subcarrier to baseband)
//!   → 1st-order IIR LP at ~2.4 kHz  (removes adjacent subcarriers)
//!   → decimate to OVERSAMPLE × bit-rate
//!   → average over OVERSAMPLE chips → one symbol per RDS bit
//!   → differential BPSK decode (sign of current × previous symbol)
//!   → CRC-10 block sync + Group 0 PS name extraction
//! ```
//!
//! The decoder is conservative: it only publishes a PS name after all 4
//! Group-0 segments arrive without CRC error.  Bad blocks lose sync and
//! force a re-hunt without corrupting already-assembled characters.

use std::f32::consts::PI;

// ── Constants ────────────────────────────────────────────────────────────────

/// RDS subcarrier centre frequency (Hz).
const SUBCARRIER_HZ: f32 = 57_000.0;

/// RDS symbol (bit) rate (bps).
const BIT_RATE_HZ: f32 = 1_187.5;

/// Oversampling ratio for the chip stream (samples per bit before averaging).
const OVERSAMPLE: usize = 8;

/// Bits per RDS block (16 data + 10 CRC/offset).
const BLOCK_BITS: usize = 26;

/// CRC-10 feedback polynomial (lower 10 bits of the generator):
/// x^8 + x^7 + x^5 + x^4 + x^3 + 1 = 0x1B9
/// (The leading x^10 term is implicit in the LFSR shift structure.)
const CRC_POLY: u16 = 0x1B9;

/// CRC offset words for the four block positions.
const OFFSET_A: u16 = 0x0FC;
const OFFSET_B: u16 = 0x198;
const OFFSET_C: u16 = 0x168;
const OFFSET_C_PRIME: u16 = 0x1B4;
const OFFSET_D: u16 = 0x0D4;

/// Consecutive valid blocks needed to declare block sync.
const SYNC_THRESHOLD: usize = 4;

// ── Public types ──────────────────────────────────────────────────────────────

/// RDS data extracted by the decoder.
#[derive(Debug, Clone, Default)]
pub struct RdsData {
    /// Programme Service name (8 ASCII characters).
    /// `None` until all four Group-0 segments have been received.
    pub ps_name: Option<String>,
}

/// Stateful RDS decoder.  Feed composite FM baseband (post-discriminator,
/// pre-de-emphasis) mono samples via `process()`.
pub struct RdsDecoder {
    // ── NCO (57 kHz) ─────────────────────────────────────────────────────────
    nco_phase: f32,
    nco_step: f32,

    // ── Baseband lowpass (IIR, ~2.4 kHz cutoff) ──────────────────────────────
    lp_alpha: f32,
    lp_i: f32,

    // ── Chip decimation ───────────────────────────────────────────────────────
    /// Input samples per chip (fractional accumulator).
    samples_per_chip: f32,
    chip_acc: f32,
    /// Chip-level I samples accumulated over one OVERSAMPLE window.
    chip_sum: f32,
    chip_count: usize,

    // ── DBPSK ─────────────────────────────────────────────────────────────────
    /// Previous symbol estimate (for differential decision).
    prev_symbol: f32,

    // ── Block sync / CRC ──────────────────────────────────────────────────────
    /// 26-bit shift register (MSB = oldest bit).
    shift_reg: u32,
    /// Bit counter within the current block (0..BLOCK_BITS).
    bit_pos: usize,
    /// Block counter within the current group (0..4).
    block_pos: usize,
    /// Valid block streak (for sync acquisition).
    sync_count: usize,
    /// True once sync_count >= SYNC_THRESHOLD.
    synced: bool,

    // ── Group data staging ────────────────────────────────────────────────────
    /// Data words for the current group (one entry per block A-D).
    group_words: [u16; 4],

    // ── PS name assembly ──────────────────────────────────────────────────────
    /// 8-byte PS name buffer (2 chars per segment × 4 segments).
    ps_chars: [u8; 8],
    /// Which segments have been received without CRC error.
    ps_received: [bool; 4],

    /// Latest decoded RDS data.
    pub data: RdsData,
}

impl RdsDecoder {
    /// Create a new decoder for the given composite baseband sample rate.
    pub fn new(sample_rate: u32) -> Self {
        let fs = sample_rate as f32;
        let nco_step = 2.0 * PI * SUBCARRIER_HZ / fs;
        let lp_alpha = (-2.0 * PI * 2_400.0_f32 / fs).exp();
        let samples_per_chip = fs / (BIT_RATE_HZ * OVERSAMPLE as f32);

        Self {
            nco_phase: 0.0,
            nco_step,
            lp_alpha,
            lp_i: 0.0,
            samples_per_chip,
            chip_acc: 0.0,
            chip_sum: 0.0,
            chip_count: 0,
            prev_symbol: 0.0,
            shift_reg: 0,
            bit_pos: 0,
            block_pos: 0,
            sync_count: 0,
            synced: false,
            group_words: [0u16; 4],
            ps_chars: [b' '; 8],
            ps_received: [false; 4],
            data: RdsData::default(),
        }
    }

    /// Feed composite baseband samples (post-FM-discriminator, mono, ±1.0).
    ///
    /// Returns `true` if `self.data` was updated (e.g. a new PS name decoded).
    pub fn process(&mut self, samples: &[f32]) -> bool {
        let mut updated = false;

        for &s in samples {
            // ── Frequency-shift 57 kHz subcarrier to baseband ─────────────────
            let (sin_p, cos_p) = self.nco_phase.sin_cos();
            let i = s * cos_p;
            // q = s * (-sin_p)  — not used after lowpass (RDS is in I channel)
            let _ = s * (-sin_p);

            self.nco_phase += self.nco_step;
            if self.nco_phase > PI {
                self.nco_phase -= 2.0 * PI;
            }

            // ── Lowpass filter ────────────────────────────────────────────────
            self.lp_i = self.lp_alpha * self.lp_i + (1.0 - self.lp_alpha) * i;

            // ── Decimate: accumulate chips ────────────────────────────────────
            self.chip_acc += 1.0;
            if self.chip_acc >= self.samples_per_chip {
                self.chip_acc -= self.samples_per_chip;
                self.chip_sum += self.lp_i;
                self.chip_count += 1;

                if self.chip_count >= OVERSAMPLE {
                    // Average → one symbol estimate
                    let symbol = self.chip_sum / OVERSAMPLE as f32;
                    self.chip_sum = 0.0;
                    self.chip_count = 0;

                    // ── DBPSK decision ────────────────────────────────────────
                    // Positive product → same phase → bit 0
                    // Negative product → phase flip → bit 1
                    let bit: u8 = if symbol * self.prev_symbol < 0.0 { 1 } else { 0 };
                    self.prev_symbol = symbol;

                    if self.push_bit(bit) {
                        updated = true;
                    }
                }
            }
        }

        updated
    }

    /// Push one DBPSK-decoded bit into the 26-bit shift register.
    ///
    /// When 26 bits have accumulated, checks CRC against the current block
    /// position's offset word.  A good block advances block_pos; a bad block
    /// resets sync and shifts the bit boundary by one to re-hunt.
    ///
    /// Returns `true` if the PS name was updated.
    fn push_bit(&mut self, bit: u8) -> bool {
        self.shift_reg = ((self.shift_reg << 1) | bit as u32) & 0x03FF_FFFF;
        self.bit_pos += 1;

        if self.bit_pos < BLOCK_BITS {
            return false;
        }
        self.bit_pos = 0;

        // ── CRC check ────────────────────────────────────────────────────────
        let offset = [OFFSET_A, OFFSET_B, OFFSET_C, OFFSET_D][self.block_pos];
        let syn = crc10_syndrome(self.shift_reg, offset);
        // Block C may use either offset C or C' (version B groups)
        let ok = syn == 0
            || (self.block_pos == 2 && crc10_syndrome(self.shift_reg, OFFSET_C_PRIME) == 0);

        if !ok {
            if self.synced {
                self.synced = false;
                self.sync_count = 0;
            }
            // Slide block boundary by one bit
            self.bit_pos = BLOCK_BITS - 1;
            return false;
        }

        // Good block
        self.sync_count += 1;
        if self.sync_count >= SYNC_THRESHOLD {
            self.synced = true;
        }

        let data_word = (self.shift_reg >> 10) as u16;

        if self.synced {
            self.group_words[self.block_pos] = data_word;

            // Dispatch when all four blocks of a group are present
            if self.block_pos == 3 {
                let updated = self.dispatch_group();
                self.block_pos = 0;
                return updated;
            }
        }

        self.block_pos = (self.block_pos + 1) % 4;
        false
    }

    /// Dispatch a complete group (words A–D collected in `group_words`).
    /// Only handles Group 0 (PS name).  Returns true if PS name changed.
    fn dispatch_group(&mut self) -> bool {
        let block_b = self.group_words[1];
        let group_type = (block_b >> 12) & 0x0F;

        if group_type != 0 {
            return false; // Not Group 0 — ignore
        }

        // Group 0A/0B: segment address in bits 1-0 of Block B
        let seg_addr = (block_b & 0x03) as usize;
        if seg_addr >= 4 {
            return false;
        }

        // Block C and Block D each carry one PS character
        // Block C: bits 15-8 = char 0, bits 7-0 = char 1 of segment
        // But in Group 0A/0B the two PS chars are in Block D
        // (Block C in Group 0A contains Programme Item Number or AF codes,
        //  Block D carries the two PS chars)
        let block_d = self.group_words[3];
        let char0 = sanitise_rds_char((block_d >> 8) as u8);
        let char1 = sanitise_rds_char((block_d & 0xFF) as u8);

        let idx = seg_addr * 2;
        self.ps_chars[idx] = char0;
        self.ps_chars[idx + 1] = char1;
        self.ps_received[seg_addr] = true;

        // Publish PS name once all 4 segments received
        if self.ps_received.iter().all(|&r| r) {
            let name: String = self.ps_chars
                .iter()
                .map(|&b| b as char)
                .collect::<String>()
                .trim_end()
                .to_string();
            let changed = self.data.ps_name.as_deref() != Some(&name);
            self.data.ps_name = Some(name);
            return changed;
        }

        false
    }

    /// Reset all decoder state (call on frequency change).
    pub fn reset(&mut self) {
        self.nco_phase = 0.0;
        self.lp_i = 0.0;
        self.chip_acc = 0.0;
        self.chip_sum = 0.0;
        self.chip_count = 0;
        self.prev_symbol = 0.0;
        self.shift_reg = 0;
        self.bit_pos = 0;
        self.block_pos = 0;
        self.sync_count = 0;
        self.synced = false;
        self.group_words = [0u16; 4];
        self.ps_chars = [b' '; 8];
        self.ps_received = [false; 4];
        self.data = RdsData::default();
    }
}

// ── DSP helpers ───────────────────────────────────────────────────────────────

/// CRC-10 syndrome for a received 26-bit block against a given offset word.
///
/// Returns 0 if the block is error-free at that position.
fn crc10_syndrome(received: u32, offset: u16) -> u16 {
    let data = (received >> 10) as u16;
    let received_crc = (received & 0x3FF) as u16;
    crc10(data) ^ received_crc ^ offset
}

/// Compute the CRC-10 remainder of a 16-bit message word.
///
/// Uses the RDS generator polynomial g(x) = x^10 + x^8 + x^7 + x^5 + x^4 + x^3 + 1.
fn crc10(data: u16) -> u16 {
    let mut reg: u16 = 0;
    for i in (0..16).rev() {
        let bit = (data >> i) & 1;
        let feedback = ((reg >> 9) ^ bit) & 1;
        reg = (reg << 1) & 0x3FF;
        if feedback != 0 {
            reg ^= CRC_POLY;
        }
    }
    reg
}

/// Replace non-printable or non-ASCII bytes with a space.
fn sanitise_rds_char(b: u8) -> u8 {
    if b.is_ascii() && !b.is_ascii_control() {
        b
    } else {
        b' '
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc10_all_zeros_is_zero() {
        assert_eq!(crc10(0), 0);
    }

    #[test]
    fn crc10_roundtrip() {
        // Encoding a word and checking its own CRC gives syndrome 0 (no offset).
        for data in [0x1234u16, 0xABCDu16, 0xFFFFu16, 0x0001u16] {
            let crc = crc10(data);
            let word = ((data as u32) << 10) | crc as u32;
            assert_eq!(
                crc10_syndrome(word, 0),
                0,
                "roundtrip failed for data={data:#06x}"
            );
        }
    }

    #[test]
    fn syndrome_detects_single_bit_error() {
        let data: u16 = 0x5A5A;
        let crc = crc10(data);
        let word = ((data as u32) << 10) | crc as u32;
        // Flip bit 5 → syndrome must be non-zero
        let corrupted = word ^ (1 << 5);
        assert_ne!(crc10_syndrome(corrupted, 0), 0);
    }

    #[test]
    fn decoder_constructs_for_common_rates() {
        for &sr in &[200_000u32, 250_000, 1_000_000, 2_000_000] {
            let _ = RdsDecoder::new(sr);
        }
    }

    #[test]
    fn decoder_reset_clears_state() {
        let mut dec = RdsDecoder::new(200_000);
        dec.nco_phase = 1.5;
        dec.sync_count = 10;
        dec.synced = true;
        dec.data.ps_name = Some("TEST    ".to_string());
        dec.reset();
        assert_eq!(dec.nco_phase, 0.0);
        assert!(!dec.synced);
        assert_eq!(dec.sync_count, 0);
        assert!(dec.data.ps_name.is_none());
    }

    #[test]
    fn sanitise_keeps_printable_ascii() {
        assert_eq!(sanitise_rds_char(b'A'), b'A');
        assert_eq!(sanitise_rds_char(b' '), b' ');
        assert_eq!(sanitise_rds_char(b'9'), b'9');
    }

    #[test]
    fn sanitise_replaces_control_chars() {
        assert_eq!(sanitise_rds_char(0x00), b' ');
        assert_eq!(sanitise_rds_char(0x0D), b' ');
        assert_eq!(sanitise_rds_char(0x7F), b' ');
    }

    #[test]
    fn process_silence_does_not_panic_or_produce_name() {
        let mut dec = RdsDecoder::new(200_000);
        let silence = vec![0.0f32; 200_000];
        let updated = dec.process(&silence);
        assert!(!updated, "silence should not produce an RDS name");
        assert!(dec.data.ps_name.is_none());
    }

    #[test]
    fn encode_decode_group0_ps_name() {
        // Build a synthetic RDS bitstream with a known PS name and verify
        // the decoder extracts it correctly.
        //
        // PS name "TESTFM  " split into 4 segments:
        //   seg 0 → "TE", seg 1 → "ST", seg 2 → "FM", seg 3 → "  "
        //
        // We use a sample rate of BIT_RATE_HZ * OVERSAMPLE ≈ 9500 Hz so that
        // samples_per_chip == 1.0 and each input sample is exactly one chip.
        // We set nco_step = 0 (private field, accessible within this module) so
        // cos(nco_phase) == 1.0 always, effectively bypassing the frequency shift.
        //
        // We transmit the 4 groups 8 times so the decoder can:
        //   (a) acquire block sync (needs SYNC_THRESHOLD=4 valid blocks), and
        //   (b) collect all 4 PS segments after lock.

        let ps_segments: [(&str, u16); 4] =
            [("TE", 0), ("ST", 1), ("FM", 2), ("  ", 3)];

        // Build the 4-group bit pattern once and repeat it
        let mut group_bits: Vec<u8> = Vec::new();
        for (chars, seg) in &ps_segments {
            let pi: u16 = 0x1234;
            append_block(&mut group_bits, pi, OFFSET_A);
            // Group 0, seg_addr in bits 1-0; group type 0 → upper nibble = 0
            append_block(&mut group_bits, *seg, OFFSET_B);
            // Block C: dummy (AF / Programme Item Number)
            append_block(&mut group_bits, 0x0000, OFFSET_C);
            // Block D: two PS chars
            let c0 = chars.as_bytes()[0] as u16;
            let c1 = chars.as_bytes()[1] as u16;
            append_block(&mut group_bits, (c0 << 8) | c1, OFFSET_D);
        }

        // Repeat 8 times to guarantee sync + segment collection
        let mut all_bits: Vec<u8> = Vec::new();
        for _ in 0..8 {
            all_bits.extend_from_slice(&group_bits);
        }

        // Create decoder at the chip rate; bypass the 57 kHz NCO by zeroing its step
        let sr = (BIT_RATE_HZ * OVERSAMPLE as f32).ceil() as u32; // ≈ 9500 Hz
        let mut dec = RdsDecoder::new(sr);
        dec.nco_step = 0.0; // cos(0) == 1.0 always → no frequency shift

        // Build DBPSK waveform: bit 0 = same phase, bit 1 = flip phase
        let mut phase: f32 = 1.0;
        let mut samples: Vec<f32> = Vec::with_capacity(all_bits.len() * OVERSAMPLE);
        for &bit in &all_bits {
            if bit == 1 {
                phase *= -1.0;
            }
            for _ in 0..OVERSAMPLE {
                samples.push(phase);
            }
        }

        dec.process(&samples);

        assert_eq!(
            dec.data.ps_name.as_deref(),
            Some("TESTFM"),
            "decoded PS name mismatch (got {:?})",
            dec.data.ps_name
        );
    }

    /// Append a 26-bit encoded RDS block (16-bit data + 10-bit CRC XOR offset)
    /// as individual bits (MSB first) to `out`.
    fn append_block(out: &mut Vec<u8>, data: u16, offset: u16) {
        let crc = crc10(data) ^ offset;
        let word: u32 = ((data as u32) << 10) | crc as u32;
        for i in (0..26).rev() {
            out.push(((word >> i) & 1) as u8);
        }
    }
}
