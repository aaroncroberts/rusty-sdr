//! Orbcomm frame synchronizer.
//!
//! The Orbcomm downlink frame format uses a 24-bit sync word `0x65A8F9`
//! (transmitted MSB-first) that we correlate against the bit stream coming
//! out of the clock recovery stage.
//!
//! ## Frame structure (after sync word)
//!
//! ```text
//! Offset  Length  Field
//! ------  ------  -----
//!  0      24 b    Sync word       0x65_A8_F9
//! 24      16 b    Type word
//! 40     144 b    Data words      9 × 16 b
//! 184     32 b    Fletcher FCS    2 × 16 b  (type + data words covered)
//! ------
//! 216 b total after sync word
//! ```
//!
//! The sync word is *not* included in the FCS calculation.
//!
//! ## Inverted-polarity handling
//!
//! DPSK has a phase ambiguity: a 180° carrier error inverts all bits.  We
//! handle this by also correlating against the bitwise complement of the sync
//! word (`0x9A_57_06`).  When that matches we set the [`FrameSync::inverted`]
//! flag and the parser will flip all data bits before processing.

/// Orbcomm 24-bit sync word (MSB-first).
pub const SYNC_WORD: u32 = 0x65_A8_F9;

/// Bitwise complement of the sync word for inverted-polarity detection.
pub const SYNC_WORD_INV: u32 = (!SYNC_WORD) & 0x00_FF_FF_FF;

/// Number of bits in the sync word.
pub const SYNC_BITS: usize = 24;

/// Number of payload bits following the sync word (type + 9 data + 2 FCS words).
pub const PAYLOAD_BITS: usize = 12 * 16; // 192

/// Total frame size including sync word.
pub const FRAME_BITS: usize = SYNC_BITS + PAYLOAD_BITS;

/// Maximum allowed bit errors in sync correlation (Hamming distance threshold).
pub const MAX_SYNC_ERRORS: u32 = 1;

/// Frame synchronizer state machine.
///
/// Call [`FrameSync::push_bit`] for each recovered bit.  When a full frame is
/// detected, [`FrameSync::take_frame`] returns `Some(Vec<u8>)` containing the
/// payload bytes (i.e. everything *after* the sync word: type + data + FCS).
#[derive(Debug)]
pub struct FrameSync {
    /// Shift register holding the most recent 24 bits for sync correlation.
    sync_sr: u32,
    /// Payload bit buffer (filled after sync acquisition).
    payload: Vec<u8>,
    /// Number of payload bits collected so far.
    payload_bits: usize,
    /// True when we are collecting payload bits.
    acquiring: bool,
    /// True when the frame was acquired with inverted polarity.
    pub inverted: bool,
}

impl FrameSync {
    pub fn new() -> Self {
        Self {
            sync_sr: 0,
            payload: vec![0u8; PAYLOAD_BITS / 8],
            payload_bits: 0,
            acquiring: false,
            inverted: false,
        }
    }

    /// Push one recovered bit (0 or 1).  Returns `true` if a frame is ready
    /// to be retrieved with [`take_frame`].
    pub fn push_bit(&mut self, bit: u8) -> bool {
        let bit = bit & 1;

        if self.acquiring {
            // Accumulate payload bits MSB-first into bytes.
            let byte_idx = self.payload_bits / 8;
            let bit_idx = 7 - (self.payload_bits % 8);
            self.payload[byte_idx] |= (bit as u8) << bit_idx;
            self.payload_bits += 1;

            if self.payload_bits >= PAYLOAD_BITS {
                self.acquiring = false;
                return true;
            }
            return false;
        }

        // Shift new bit into the 24-bit sync shift register.
        self.sync_sr = ((self.sync_sr << 1) | bit as u32) & 0x00_FF_FF_FF;

        // Check normal and inverted sync.
        let dist_normal = hamming_distance_24(self.sync_sr, SYNC_WORD);
        let dist_inv = hamming_distance_24(self.sync_sr, SYNC_WORD_INV);

        if dist_normal <= MAX_SYNC_ERRORS {
            self.start_payload(false);
        } else if dist_inv <= MAX_SYNC_ERRORS {
            self.start_payload(true);
        }

        false
    }

    /// Called when sync correlation fires.
    fn start_payload(&mut self, inverted: bool) {
        self.inverted = inverted;
        self.acquiring = true;
        self.payload_bits = 0;
        self.payload.fill(0);
    }

    /// Take the collected payload bytes out of the synchronizer, resetting it
    /// for the next frame.  Returns `None` if no frame is ready.
    pub fn take_frame(&mut self) -> Option<Vec<u8>> {
        if self.acquiring || self.payload_bits < PAYLOAD_BITS {
            // Still collecting — check if we actually just finished.
            return None;
        }
        let mut out = self.payload.clone();
        if self.inverted {
            for b in &mut out {
                *b = !*b;
            }
        }
        Some(out)
    }
}

impl Default for FrameSync {
    fn default() -> Self {
        Self::new()
    }
}

/// Count differing bits between two 24-bit values.
fn hamming_distance_24(a: u32, b: u32) -> u32 {
    (a ^ b).count_ones()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn push_bits(fs: &mut FrameSync, word: u32, n_bits: usize) -> bool {
        let mut done = false;
        for i in (0..n_bits).rev() {
            let bit = ((word >> i) & 1) as u8;
            if fs.push_bit(bit) {
                done = true;
            }
        }
        done
    }

    fn push_bytes(fs: &mut FrameSync, bytes: &[u8]) -> bool {
        let mut done = false;
        for &b in bytes {
            for i in (0..8).rev() {
                let bit = (b >> i) & 1;
                if fs.push_bit(bit) {
                    done = true;
                }
            }
        }
        done
    }

    /// Sync word detection: exact match fires after 24 bits.
    #[test]
    fn sync_detected_exact() {
        let mut fs = FrameSync::new();
        push_bits(&mut fs, SYNC_WORD, 24);
        assert!(fs.acquiring, "Should be acquiring after exact sync match");
        assert!(!fs.inverted);
    }

    /// Sync word with one bit error still triggers (within MAX_SYNC_ERRORS=1).
    #[test]
    fn sync_detected_one_error() {
        let mut fs = FrameSync::new();
        // Flip the LSB of the sync word.
        push_bits(&mut fs, SYNC_WORD ^ 1, 24);
        assert!(fs.acquiring, "Should acquire with 1 bit error in sync");
    }

    /// Inverted sync word detected and `inverted` flag set.
    #[test]
    fn sync_detects_inverted_polarity() {
        let mut fs = FrameSync::new();
        push_bits(&mut fs, SYNC_WORD_INV, 24);
        assert!(fs.acquiring, "Should acquire on inverted sync");
        assert!(fs.inverted, "Inverted flag should be set");
    }

    /// A completely wrong pattern does not trigger sync.
    #[test]
    fn sync_not_triggered_on_noise() {
        let mut fs = FrameSync::new();
        // Push 24 zero bits — far from the sync word.
        push_bits(&mut fs, 0x00_00_00, 24);
        assert!(!fs.acquiring, "Should not acquire on all-zero pattern");
    }

    /// Full frame: sync → payload → take_frame returns expected bytes.
    #[test]
    fn full_frame_roundtrip() {
        let mut fs = FrameSync::new();

        // Build a payload: 12 words × 2 bytes = 24 bytes.
        let payload_bytes: Vec<u8> = (0u8..24).collect();

        // Push sync word then payload.
        push_bits(&mut fs, SYNC_WORD, 24);
        let done = push_bytes(&mut fs, &payload_bytes);
        assert!(done, "Expected frame-ready signal");

        let frame = fs.take_frame().expect("Expected a frame");
        assert_eq!(frame, payload_bytes, "Payload bytes should match");
    }

    /// Inverted frame: bytes are de-inverted when taken.
    #[test]
    fn inverted_frame_deinverted_on_take() {
        let mut fs = FrameSync::new();
        let original: Vec<u8> = (0u8..24).collect();
        let inverted: Vec<u8> = original.iter().map(|&b| !b).collect();

        push_bits(&mut fs, SYNC_WORD_INV, 24);
        push_bytes(&mut fs, &inverted);
        let frame = fs.take_frame().expect("Expected a frame");
        assert_eq!(frame, original, "Should have de-inverted the payload");
    }

    /// take_frame returns None when no frame is ready.
    #[test]
    fn take_frame_returns_none_when_not_ready() {
        let mut fs = FrameSync::new();
        assert!(fs.take_frame().is_none());
    }

    /// Hamming distance helper works correctly.
    #[test]
    fn hamming_distance_correctness() {
        assert_eq!(hamming_distance_24(0x00_00_00, 0x00_00_00), 0);
        assert_eq!(hamming_distance_24(0xFF_FF_FF, 0x00_00_00), 24);
        assert_eq!(hamming_distance_24(0x00_00_01, 0x00_00_00), 1);
    }
}
