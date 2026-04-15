#![forbid(unsafe_code)]

//! RDS (Radio Data System) decoder for WBFM broadcast.
//!
//! Extracts the 57 kHz RDS subcarrier from the composite FM baseband,
//! demodulates DBPSK at 1187.5 bps, syncs to 26-bit blocks using
//! CRC-10 syndrome matching, and decodes:
//! - Group 0: PS name (8 chars), PTY, TP, TA flags
//! - Group 2: RadioText RT (64 chars, A/B flag-aware reassembly)
//!
//! ## Signal chain
//! ```text
//! composite baseband (fs Hz)
//!   → multiply by 57 kHz NCO  (shifts RDS subcarrier to baseband)
//!   → 1st-order IIR LP at ~2.4 kHz  (removes adjacent subcarriers)
//!   → decimate to OVERSAMPLE × bit-rate
//!   → average over OVERSAMPLE chips → one symbol per RDS bit
//!   → differential BPSK decode (sign of current × previous symbol)
//!   → CRC-10 block sync + group dispatch
//! ```

use std::f32::consts::PI;

// ── Constants ────────────────────────────────────────────────────────────────

const SUBCARRIER_HZ: f32 = 57_000.0;
const BIT_RATE_HZ: f32 = 1_187.5;
const OVERSAMPLE: usize = 8;
const BLOCK_BITS: usize = 26;
const CRC_POLY: u16 = 0x1B9;
const OFFSET_A: u16 = 0x0FC;
const OFFSET_B: u16 = 0x198;
const OFFSET_C: u16 = 0x168;
const OFFSET_C_PRIME: u16 = 0x1B4;
const OFFSET_D: u16 = 0x0D4;
const SYNC_THRESHOLD: usize = 4;

/// Number of Group-2 segments needed for a complete RadioText (16 for 2A, 8 for 2B).
const RT_SEGMENTS_2A: usize = 16;

/// Number of consecutive non-Group-2 group dispatches after which a partial
/// RadioText is considered stale and flushed.  At ~11.25 groups/s this is ≈ 5 s.
const RT_STALE_GROUP_COUNT: usize = 56;

// ── PTY table ─────────────────────────────────────────────────────────────────

/// RDS Programme Type (PTY) code → genre string (RBDS/RDS-Europe).
pub fn pty_to_str(pty: u8) -> &'static str {
    match pty {
        0 => "No programme type",
        1 => "News",
        2 => "Current affairs",
        3 => "Information",
        4 => "Sport",
        5 => "Education",
        6 => "Drama",
        7 => "Cultures",
        8 => "Science",
        9 => "Varied speech",
        10 => "Pop music",
        11 => "Rock music",
        12 => "Easy listening",
        13 => "Light classics",
        14 => "Serious classics",
        15 => "Other music",
        16 => "Weather",
        17 => "Finance",
        18 => "Children's progs",
        19 => "Social affairs",
        20 => "Religion",
        21 => "Phone in",
        22 => "Travel",
        23 => "Leisure",
        24 => "Jazz music",
        25 => "Country music",
        26 => "National music",
        27 => "Oldies music",
        28 => "Folk music",
        29 => "Documentary",
        30 => "Alarm test",
        31 => "Alarm",
        _ => "Unknown",
    }
}

// ── Public types ──────────────────────────────────────────────────────────────

/// All RDS data extracted by the decoder.
#[derive(Debug, Clone, Default)]
pub struct RdsData {
    /// Programme Service name (8 ASCII characters).
    pub ps_name: Option<String>,

    /// Programme Type code (0-31).
    pub pty: Option<u8>,

    /// Traffic Programme flag — station carries traffic info regularly.
    pub tp: bool,

    /// Traffic Announcement flag — traffic bulletin being broadcast now.
    pub ta: bool,

    /// RadioText (up to 64 chars for Group 2A, up to 32 for Group 2B).
    pub rt: Option<String>,
}

/// Stateful RDS decoder.
pub struct RdsDecoder {
    // NCO
    nco_phase: f32,
    nco_step: f32,

    // Baseband lowpass
    lp_alpha: f32,
    lp_i: f32,

    // Decimation
    samples_per_chip: f32,
    chip_acc: f32,
    chip_sum: f32,
    chip_count: usize,

    // DBPSK
    prev_symbol: f32,

    // Block sync
    shift_reg: u32,
    bit_pos: usize,
    block_pos: usize,
    sync_count: usize,
    synced: bool,

    // Group staging
    group_words: [u16; 4],

    // PS assembly
    ps_chars: [u8; 8],
    ps_received: [bool; 4],

    // RT assembly
    rt_chars: [u8; 64],
    rt_received: [bool; RT_SEGMENTS_2A],
    rt_ab_flag: Option<bool>, // current A/B flag — flip = clear and restart

    // RT stale-text detection: counts group dispatches since the last Group 2
    // segment.  At ~11.25 groups/s, 56 dispatches ≈ 5 s of no RT activity.
    // When the count exceeds the threshold, partial RT is flushed.
    rt_groups_since_last_seg: usize,

    /// Latest decoded RDS data.
    pub data: RdsData,
}

impl RdsDecoder {
    pub fn new(sample_rate: u32) -> Self {
        let fs = sample_rate as f32;
        Self {
            nco_phase: 0.0,
            nco_step: 2.0 * PI * SUBCARRIER_HZ / fs,
            lp_alpha: (-2.0 * PI * 2_400.0_f32 / fs).exp(),
            lp_i: 0.0,
            samples_per_chip: fs / (BIT_RATE_HZ * OVERSAMPLE as f32),
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
            rt_chars: [b' '; 64],
            rt_received: [false; RT_SEGMENTS_2A],
            rt_ab_flag: None,
            rt_groups_since_last_seg: 0,
            data: RdsData::default(),
        }
    }

    /// Feed composite FM baseband samples (post-discriminator, mono, ±1.0).
    /// Returns `true` if any `RdsData` field was updated.
    pub fn process(&mut self, samples: &[f32]) -> bool {
        let mut updated = false;
        for &s in samples {
            let (_sin_p, cos_p) = self.nco_phase.sin_cos();
            let i = s * cos_p;
            self.nco_phase += self.nco_step;
            if self.nco_phase > PI {
                self.nco_phase -= 2.0 * PI;
            }
            self.lp_i = self.lp_alpha * self.lp_i + (1.0 - self.lp_alpha) * i;

            self.chip_acc += 1.0;
            if self.chip_acc >= self.samples_per_chip {
                self.chip_acc -= self.samples_per_chip;
                self.chip_sum += self.lp_i;
                self.chip_count += 1;

                if self.chip_count >= OVERSAMPLE {
                    let symbol = self.chip_sum / OVERSAMPLE as f32;
                    self.chip_sum = 0.0;
                    self.chip_count = 0;
                    let bit: u8 = if symbol * self.prev_symbol < 0.0 {
                        1
                    } else {
                        0
                    };
                    self.prev_symbol = symbol;
                    if self.push_bit(bit) {
                        updated = true;
                    }
                }
            }
        }
        updated
    }

    fn push_bit(&mut self, bit: u8) -> bool {
        self.shift_reg = ((self.shift_reg << 1) | bit as u32) & 0x03FF_FFFF;
        self.bit_pos += 1;
        if self.bit_pos < BLOCK_BITS {
            return false;
        }
        self.bit_pos = 0;

        let offset = [OFFSET_A, OFFSET_B, OFFSET_C, OFFSET_D][self.block_pos];
        let syn = crc10_syndrome(self.shift_reg, offset);
        let ok = syn == 0
            || (self.block_pos == 2 && crc10_syndrome(self.shift_reg, OFFSET_C_PRIME) == 0);

        if !ok {
            if self.synced {
                self.synced = false;
                self.sync_count = 0;
            }
            self.bit_pos = BLOCK_BITS - 1;
            return false;
        }

        self.sync_count += 1;
        if self.sync_count >= SYNC_THRESHOLD {
            self.synced = true;
        }

        if self.synced {
            self.group_words[self.block_pos] = (self.shift_reg >> 10) as u16;
            if self.block_pos == 3 {
                let updated = self.dispatch_group();
                self.block_pos = 0;
                return updated;
            }
        }

        self.block_pos = (self.block_pos + 1) % 4;
        false
    }

    fn dispatch_group(&mut self) -> bool {
        let block_b = self.group_words[1];
        let group_type = (block_b >> 12) & 0x0F;
        let _version_b = (block_b >> 11) & 0x01; // 0=A, 1=B

        // TP flag is in bit 10 of Block B for all groups
        let tp = (block_b >> 10) & 0x01 != 0;
        if tp != self.data.tp {
            self.data.tp = tp;
        }

        // PTY is bits 9-5 of Block B for all groups
        let pty = ((block_b >> 5) & 0x1F) as u8;
        let pty_changed = self.data.pty != Some(pty);
        if pty_changed {
            self.data.pty = Some(pty);
        }

        let mut stale_rt_cleared = false;
        if group_type == 2 {
            // Group 2 received — reset stale counter.
            self.rt_groups_since_last_seg = 0;
        } else {
            // Non-Group-2 group: increment stale counter and flush if threshold reached.
            self.rt_groups_since_last_seg =
                self.rt_groups_since_last_seg.saturating_add(1);
            if self.rt_groups_since_last_seg >= RT_STALE_GROUP_COUNT
                && (self.rt_received.iter().any(|&r| r) || self.data.rt.is_some())
            {
                // No Group 2 segment for ~5 s on a weak signal — discard stale RT.
                self.rt_chars = [b' '; 64];
                self.rt_received = [false; RT_SEGMENTS_2A];
                self.rt_ab_flag = None;
                self.data.rt = None;
                self.rt_groups_since_last_seg = 0;
                stale_rt_cleared = true;
            }
        }

        match group_type {
            0 => {
                let changed = self.dispatch_group0(block_b);
                changed || pty_changed || stale_rt_cleared
            }
            2 => {
                let changed = self.dispatch_group2(block_b);
                changed || pty_changed
            }
            _ => pty_changed || stale_rt_cleared,
        }
    }

    /// Group 0A/0B: PS name (Block D), TA flag (bit 4 of Block B).
    fn dispatch_group0(&mut self, block_b: u16) -> bool {
        let ta = (block_b >> 4) & 0x01 != 0;
        let ta_changed = ta != self.data.ta;
        self.data.ta = ta;

        let seg_addr = (block_b & 0x03) as usize;
        if seg_addr >= 4 {
            return ta_changed;
        }

        let block_d = self.group_words[3];
        let char0 = sanitise_rds_char((block_d >> 8) as u8);
        let char1 = sanitise_rds_char((block_d & 0xFF) as u8);

        let idx = seg_addr * 2;
        self.ps_chars[idx] = char0;
        self.ps_chars[idx + 1] = char1;
        self.ps_received[seg_addr] = true;

        let mut changed = ta_changed;
        if self.ps_received.iter().all(|&r| r) {
            let name: String = self
                .ps_chars
                .iter()
                .map(|&b| b as char)
                .collect::<String>()
                .trim_end()
                .to_string();
            if self.data.ps_name.as_deref() != Some(&name) {
                self.data.ps_name = Some(name);
                changed = true;
            }
        }
        changed
    }

    /// Group 2A: RadioText, 4 chars per group, 16 segments = 64 chars.
    fn dispatch_group2(&mut self, block_b: u16) -> bool {
        let ab_flag = (block_b >> 4) & 0x01 != 0;
        let seg_addr = (block_b & 0x0F) as usize;

        // A/B flag toggle: new text incoming — clear buffer
        if let Some(prev_ab) = self.rt_ab_flag {
            if prev_ab != ab_flag {
                self.rt_chars = [b' '; 64];
                self.rt_received = [false; RT_SEGMENTS_2A];
                self.data.rt = None;
            }
        }
        self.rt_ab_flag = Some(ab_flag);

        if seg_addr >= RT_SEGMENTS_2A {
            return false;
        }

        let block_c = self.group_words[2];
        let block_d = self.group_words[3];

        let idx = seg_addr * 4;
        self.rt_chars[idx] = sanitise_rds_char((block_c >> 8) as u8);
        self.rt_chars[idx + 1] = sanitise_rds_char((block_c & 0xFF) as u8);
        self.rt_chars[idx + 2] = sanitise_rds_char((block_d >> 8) as u8);
        self.rt_chars[idx + 3] = sanitise_rds_char((block_d & 0xFF) as u8);
        self.rt_received[seg_addr] = true;

        // Publish when all 16 segments received
        if self.rt_received.iter().all(|&r| r) {
            let rt: String = self
                .rt_chars
                .iter()
                .map(|&b| b as char)
                .collect::<String>()
                .trim_end()
                .to_string();
            if self.data.rt.as_deref() != Some(&rt) {
                self.data.rt = Some(rt);
                return true;
            }
        }
        false
    }

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
        self.rt_chars = [b' '; 64];
        self.rt_received = [false; RT_SEGMENTS_2A];
        self.rt_ab_flag = None;
        self.rt_groups_since_last_seg = 0;
        self.data = RdsData::default();
    }
}

// ── DSP helpers ───────────────────────────────────────────────────────────────

fn crc10_syndrome(received: u32, offset: u16) -> u16 {
    let data = (received >> 10) as u16;
    let received_crc = (received & 0x3FF) as u16;
    crc10(data) ^ received_crc ^ offset
}

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

    fn append_block(out: &mut Vec<u8>, data: u16, offset: u16) {
        let crc = crc10(data) ^ offset;
        let word: u32 = ((data as u32) << 10) | crc as u32;
        for i in (0..26).rev() {
            out.push(((word >> i) & 1) as u8);
        }
    }

    fn make_decoder_at_chip_rate() -> RdsDecoder {
        let sr = (BIT_RATE_HZ * OVERSAMPLE as f32).ceil() as u32;
        let mut dec = RdsDecoder::new(sr);
        dec.nco_step = 0.0; // bypass NCO for test
        dec
    }

    fn bits_to_samples(bits: &[u8]) -> Vec<f32> {
        let mut phase: f32 = 1.0;
        let mut out = Vec::with_capacity(bits.len() * OVERSAMPLE);
        for &bit in bits {
            if bit == 1 {
                phase *= -1.0;
            }
            for _ in 0..OVERSAMPLE {
                out.push(phase);
            }
        }
        out
    }

    // ── CRC tests ────────────────────────────────────────────────────────────

    #[test]
    fn crc10_all_zeros_is_zero() {
        assert_eq!(crc10(0), 0);
    }

    #[test]
    fn crc10_roundtrip() {
        for data in [0x1234u16, 0xABCDu16, 0xFFFFu16, 0x0001u16] {
            let crc = crc10(data);
            let word = ((data as u32) << 10) | crc as u32;
            assert_eq!(
                crc10_syndrome(word, 0),
                0,
                "roundtrip failed for {data:#06x}"
            );
        }
    }

    #[test]
    fn syndrome_detects_single_bit_error() {
        let data: u16 = 0x5A5A;
        let word = ((data as u32) << 10) | crc10(data) as u32;
        assert_ne!(crc10_syndrome(word ^ (1 << 5), 0), 0);
    }

    // ── Decoder construction / reset ─────────────────────────────────────────

    #[test]
    fn decoder_constructs_for_common_rates() {
        for &sr in &[200_000u32, 250_000, 1_000_000, 2_000_000] {
            let _ = RdsDecoder::new(sr);
        }
    }

    #[test]
    fn decoder_reset_clears_state() {
        let mut dec = make_decoder_at_chip_rate();
        dec.sync_count = 10;
        dec.synced = true;
        dec.data.ps_name = Some("TEST    ".into());
        dec.data.rt = Some("Hello".into());
        dec.reset();
        assert!(!dec.synced);
        assert!(dec.data.ps_name.is_none());
        assert!(dec.data.rt.is_none());
        assert!(dec.data.pty.is_none());
        assert!(!dec.data.ta);
    }

    #[test]
    fn sanitise_keeps_printable_ascii() {
        assert_eq!(sanitise_rds_char(b'A'), b'A');
        assert_eq!(sanitise_rds_char(b' '), b' ');
    }

    #[test]
    fn sanitise_replaces_control_chars() {
        assert_eq!(sanitise_rds_char(0x00), b' ');
        assert_eq!(sanitise_rds_char(0x7F), b' ');
    }

    #[test]
    fn process_silence_does_not_produce_data() {
        let mut dec = make_decoder_at_chip_rate();
        dec.process(&vec![0.0f32; 200_000]);
        assert!(dec.data.ps_name.is_none());
        assert!(dec.data.rt.is_none());
    }

    // ── PTY string table ─────────────────────────────────────────────────────

    #[test]
    fn pty_to_str_known_codes() {
        assert_eq!(pty_to_str(1), "News");
        assert_eq!(pty_to_str(10), "Pop music");
        assert_eq!(pty_to_str(0), "No programme type");
        assert_eq!(pty_to_str(31), "Alarm");
    }

    // ── Group 0 end-to-end ───────────────────────────────────────────────────

    #[test]
    fn encode_decode_group0_ps_name_with_pty_and_ta() {
        // PS "TESTFM  ", PTY=10 (Pop music), TA=true
        let pi: u16 = 0x1234;
        let pty: u16 = 10;
        let ta_bit: u16 = 1;
        let tp_bit: u16 = 1;

        let mut all_bits: Vec<u8> = Vec::new();
        let segs: [(&str, u16); 4] = [("TE", 0), ("ST", 1), ("FM", 2), ("  ", 3)];

        // 8 repetitions to ensure sync + full assembly
        for _ in 0..8 {
            for (chars, seg) in &segs {
                append_block(&mut all_bits, pi, OFFSET_A);
                // Block B: group=0, version=0, TP, PTY, TA, seg_addr
                let block_b: u16 = (tp_bit << 10) | (pty << 5) | (ta_bit << 4) | seg;
                append_block(&mut all_bits, block_b, OFFSET_B);
                append_block(&mut all_bits, 0x0000, OFFSET_C); // AF dummy
                let c0 = chars.as_bytes()[0] as u16;
                let c1 = chars.as_bytes()[1] as u16;
                append_block(&mut all_bits, (c0 << 8) | c1, OFFSET_D);
            }
        }

        let mut dec = make_decoder_at_chip_rate();
        dec.process(&bits_to_samples(&all_bits));

        assert_eq!(
            dec.data.ps_name.as_deref(),
            Some("TESTFM"),
            "PS name mismatch"
        );
        assert_eq!(dec.data.pty, Some(10), "PTY mismatch");
        assert!(dec.data.tp, "TP should be set");
        assert!(dec.data.ta, "TA should be set");
    }

    // ── Group 2 RadioText end-to-end ─────────────────────────────────────────

    #[test]
    fn encode_decode_group2_radiotext() {
        // Build a 64-char RT message padded to exactly 64 chars
        let rt_msg = "Now Playing: Test Signal FM - Long RadioText Message Here  Pad!!";
        assert_eq!(rt_msg.len(), 64);

        let pi: u16 = 0x1234;
        let pty: u16 = 10;
        let tp_bit: u16 = 1;
        let ab_flag: u16 = 0; // version A

        let mut all_bits: Vec<u8> = Vec::new();

        // 4 repetitions: 1st to gain sync, rest to receive all 16 segments
        for _ in 0..4 {
            for seg in 0u16..16 {
                let idx = (seg as usize) * 4;
                append_block(&mut all_bits, pi, OFFSET_A);
                // Group 2A Block B: group=2, version=0, TP, PTY, A/B, seg_addr
                let block_b: u16 = (2 << 12) | (tp_bit << 10) | (pty << 5) | (ab_flag << 4) | seg;
                append_block(&mut all_bits, block_b, OFFSET_B);
                let c: &[u8] = rt_msg.as_bytes();
                let block_c: u16 = ((c[idx] as u16) << 8) | c[idx + 1] as u16;
                let block_d: u16 = ((c[idx + 2] as u16) << 8) | c[idx + 3] as u16;
                append_block(&mut all_bits, block_c, OFFSET_C);
                append_block(&mut all_bits, block_d, OFFSET_D);
            }
        }

        let mut dec = make_decoder_at_chip_rate();
        dec.process(&bits_to_samples(&all_bits));

        assert_eq!(
            dec.data.rt.as_deref(),
            Some(rt_msg.trim_end()),
            "RadioText mismatch: got {:?}",
            dec.data.rt
        );
    }

    #[test]
    fn ab_flag_toggle_clears_radiotext() {
        let mut dec = make_decoder_at_chip_rate();
        // Manually set a partial RT buffer and simulate an A/B toggle
        dec.synced = true;
        dec.sync_count = SYNC_THRESHOLD;
        dec.rt_ab_flag = Some(false);
        dec.rt_chars[0] = b'X';

        // Call dispatch_group2 with opposite AB flag
        let block_b_new_ab: u16 = (2 << 12) | (1 << 4); // group=2, AB=1, seg=0
        dec.dispatch_group2(block_b_new_ab);

        assert_eq!(
            dec.rt_chars[0], b' ',
            "buffer should be cleared on A/B flip"
        );
    }

    // ── Stale RadioText timeout ──────────────────────────────────────────────

    #[test]
    fn stale_partial_rt_is_cleared_after_timeout() {
        let mut dec = make_decoder_at_chip_rate();
        dec.synced = true;
        dec.sync_count = SYNC_THRESHOLD;

        // Plant partial RT: mark 3 of 16 segments received and set data.rt.
        dec.rt_chars[0] = b'H';
        dec.rt_chars[1] = b'i';
        dec.rt_received[0] = true;
        dec.data.rt = Some("Hi".into());

        // Simulate RT_STALE_GROUP_COUNT non-Group-2 group dispatches.
        // Group 0 Block B: group=0, no TP, PTY=0, TA=0, seg=0.
        let group0_block_b: u16 = 0x0000; // group=0, everything else 0
        for _ in 0..RT_STALE_GROUP_COUNT {
            dec.group_words[1] = group0_block_b;
            dec.group_words[0] = 0; // PI
            dec.group_words[2] = 0; // Block C
            dec.group_words[3] = 0; // Block D (PS seg chars)
            dec.dispatch_group();
        }

        assert!(
            dec.data.rt.is_none(),
            "stale partial RT should be flushed after timeout"
        );
        assert!(
            dec.rt_received.iter().all(|&r| !r),
            "rt_received flags should be cleared after timeout"
        );
    }

    #[test]
    fn complete_rt_not_cleared_by_ongoing_group2() {
        // A station that consistently sends Group 2 should never trigger the flush.
        let mut dec = make_decoder_at_chip_rate();
        dec.synced = true;
        dec.sync_count = SYNC_THRESHOLD;
        dec.data.rt = Some("Test Station RT".into());

        // Alternate Group 2 and Group 0 dispatches; Group 2 resets the counter.
        let group2_b: u16 = (2 << 12); // group=2, seg=0, AB=0
        let group0_b: u16 = 0x0000;

        for i in 0..(RT_STALE_GROUP_COUNT * 4) {
            dec.group_words[1] = if i % 2 == 0 { group2_b } else { group0_b };
            dec.group_words[0] = 0;
            dec.group_words[2] = 0;
            dec.group_words[3] = 0;
            dec.dispatch_group();
        }

        // RT should still be set — Group 2 kept arriving to reset the counter.
        assert!(
            dec.data.rt.is_some(),
            "RT should not be cleared when Group 2 keeps arriving"
        );
    }
}
