#![forbid(unsafe_code)]

//! CTCSS (Continuous Tone-Coded Squelch System) tone detector.
//!
//! CTCSS uses sub-audible tones (67.0 – 254.1 Hz) to gate the audio squelch
//! on repeaters and radios.  When enabled, audio is only passed when the
//! expected tone is present on the received signal.
//!
//! Detection: Goertzel algorithm on 100 ms blocks of 48 kHz audio.
//! The 4800-sample block gives 10 Hz resolution — sufficient to distinguish
//! adjacent CTCSS tones (minimum spacing ≈ 2.7 Hz).
//!
//! Mode: "any tone" — squelch opens when ANY standard CTCSS tone is
//! detected above the threshold.  Specific tone selection can be added later.

/// The 50 standard CTCSS tone frequencies (Hz), EIA RS-220.
pub const CTCSS_TONES: &[f32] = &[
    67.0, 71.9, 74.4, 77.0, 79.7, 82.5, 85.4, 88.5, 91.5, 94.8, 97.4, 100.0, 103.5, 107.2, 110.9,
    114.8, 118.8, 123.0, 127.3, 131.8, 136.5, 141.3, 146.2, 151.4, 156.7, 162.2, 167.9, 173.8,
    179.9, 186.2, 192.8, 203.5, 210.7, 218.1, 225.7, 233.6, 241.8, 250.3,
];

/// Goertzel coefficients for a single tone frequency.
struct GoertzelBin {
    coeff: f32, // 2 * cos(2π * k / N)
    q1: f32,
    q2: f32,
}

impl GoertzelBin {
    /// Tune the bin to the exact frequency (not rounded to an integer DFT bin).
    /// This avoids leakage errors for frequencies that fall between integer bins.
    fn new(tone_hz: f32, sample_rate: f32, _block_size: usize) -> Self {
        let omega = 2.0 * std::f32::consts::PI * tone_hz / sample_rate;
        Self {
            coeff: 2.0 * omega.cos(),
            q1: 0.0,
            q2: 0.0,
        }
    }

    #[inline]
    fn push(&mut self, x: f32) {
        let q0 = self.coeff * self.q1 - self.q2 + x;
        self.q2 = self.q1;
        self.q1 = q0;
    }

    /// Compute squared magnitude at end of block, then reset state.
    fn power(&mut self) -> f32 {
        let p = self.q1 * self.q1 + self.q2 * self.q2 - self.q1 * self.q2 * self.coeff;
        self.q1 = 0.0;
        self.q2 = 0.0;
        p
    }
}

/// Detects the presence of any standard CTCSS tone in the audio stream.
///
/// Processes 48 kHz audio in 100 ms (4800-sample) blocks.  Returns
/// `true` from `is_tone_present()` after a block is complete and at least
/// one CTCSS tone exceeds the detection threshold.
pub struct CtcssDetector {
    bins: Vec<GoertzelBin>,
    block_size: usize,
    buf_pos: usize,
    /// Detection threshold relative to block energy (default 0.005).
    threshold: f32,
    /// Result of last completed block.
    tone_detected: bool,
    /// Normalisation factor: 1 / (block_size^2 / 4) to make threshold scale-independent.
    norm: f32,
}

impl CtcssDetector {
    /// Create a CTCSS detector for 48 kHz audio.
    ///
    /// `threshold` is the minimum relative power that counts as tone detection
    /// (0.0–1.0, default 0.005 = 0.5% of full-scale block energy).
    pub fn new(sample_rate: f32, threshold: f32) -> Self {
        let block_size = (sample_rate * 0.1).round() as usize; // 100 ms
        let bins = CTCSS_TONES
            .iter()
            .map(|&f| GoertzelBin::new(f, sample_rate, block_size))
            .collect();
        let norm = 1.0 / (block_size as f32 * block_size as f32 / 4.0);
        Self {
            bins,
            block_size,
            buf_pos: 0,
            threshold,
            tone_detected: false,
            norm,
        }
    }

    pub fn with_default_threshold(sample_rate: f32) -> Self {
        Self::new(sample_rate, 0.005)
    }

    /// Feed one audio sample. After each complete 100 ms block, the detection
    /// result is updated.
    pub fn push(&mut self, sample: f32) {
        for bin in &mut self.bins {
            bin.push(sample);
        }
        self.buf_pos += 1;
        if self.buf_pos >= self.block_size {
            self.buf_pos = 0;
            let norm = self.norm;
            let threshold = self.threshold;
            self.tone_detected = self.bins.iter_mut().any(|b| b.power() * norm > threshold);
        }
    }

    /// Process a batch of audio samples.
    pub fn process_batch(&mut self, samples: &[f32]) {
        for &s in samples {
            self.push(s);
        }
    }

    /// Returns `true` if a CTCSS tone was detected in the most recent block.
    pub fn is_tone_present(&self) -> bool {
        self.tone_detected
    }

    pub fn reset(&mut self) {
        for bin in &mut self.bins {
            bin.q1 = 0.0;
            bin.q2 = 0.0;
        }
        self.buf_pos = 0;
        self.tone_detected = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn generate_tone(freq_hz: f32, fs: f32, n_samples: usize) -> Vec<f32> {
        (0..n_samples)
            .map(|i| (2.0 * std::f32::consts::PI * freq_hz / fs * i as f32).sin() * 0.1)
            .collect()
    }

    #[test]
    fn detects_standard_ctcss_tone_100hz() {
        let fs = 48_000.0;
        let mut det = CtcssDetector::with_default_threshold(fs);
        // 100.0 Hz is a standard CTCSS tone — send 200 ms of it
        let samples = generate_tone(100.0, fs, (fs * 0.2) as usize);
        det.process_batch(&samples);
        assert!(
            det.is_tone_present(),
            "100.0 Hz CTCSS tone should be detected"
        );
    }

    #[test]
    fn rejects_non_ctcss_tone() {
        let fs = 48_000.0;
        let mut det = CtcssDetector::with_default_threshold(fs);
        // 440 Hz is NOT a CTCSS tone (it's voice band) — send 200 ms
        let samples = generate_tone(440.0, fs, (fs * 0.2) as usize);
        det.process_batch(&samples);
        assert!(
            !det.is_tone_present(),
            "440 Hz should NOT be detected as CTCSS"
        );
    }

    #[test]
    fn detects_250hz_tone() {
        let fs = 48_000.0;
        let mut det = CtcssDetector::with_default_threshold(fs);
        let samples = generate_tone(250.3, fs, (fs * 0.2) as usize);
        det.process_batch(&samples);
        assert!(
            det.is_tone_present(),
            "250.3 Hz CTCSS tone should be detected"
        );
    }

    #[test]
    fn silence_produces_no_detection() {
        let fs = 48_000.0;
        let mut det = CtcssDetector::with_default_threshold(fs);
        let samples = vec![0.0f32; (fs * 0.2) as usize];
        det.process_batch(&samples);
        assert!(
            !det.is_tone_present(),
            "Silence should not trigger CTCSS detection"
        );
    }

    #[test]
    fn all_standard_tones_detected() {
        let fs = 48_000.0;
        for &tone in CTCSS_TONES {
            let mut det = CtcssDetector::with_default_threshold(fs);
            let samples = generate_tone(tone, fs, (fs * 0.25) as usize); // 250 ms
            det.process_batch(&samples);
            assert!(
                det.is_tone_present(),
                "Standard CTCSS tone {tone} Hz should be detected"
            );
        }
    }
}
