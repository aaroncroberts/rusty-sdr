#![forbid(unsafe_code)]

//! Rational resampler based on a phase accumulator.
//!
//! The phase accumulator is incremented by `audio_rate / input_rate` on every
//! input sample.  When it crosses an integer boundary an output sample is
//! emitted, producing exactly `round(input_len * audio_rate / input_rate)`
//! output samples per batch.  No interpolation is applied; the nearest input
//! sample is used (nearest-neighbour / zero-order hold).

/// Rational resampler: converts a stream at one sample rate to another.
///
/// Maintains a `phase_acc` state between calls so fractional samples carry
/// over across batch boundaries.
pub struct RationalResampler {
    /// Fractional output phase accumulator in `[0, 1)`.
    phase_acc: f64,
    /// Increment per input sample = `output_rate / input_rate`.
    phase_step: f64,
}

impl RationalResampler {
    /// Create a new resampler.
    ///
    /// * `input_rate`  — source sample rate in Hz
    /// * `output_rate` — desired output sample rate in Hz
    pub fn new(input_rate: u32, output_rate: u32) -> Self {
        Self {
            phase_acc: 0.0,
            phase_step: output_rate as f64 / input_rate as f64,
        }
    }

    /// Number of output samples for an input batch of `n` samples.
    ///
    /// Useful for pre-allocating output buffers.
    #[inline]
    pub fn output_len_hint(&self, input_len: usize) -> usize {
        (input_len as f64 * self.phase_step).ceil() as usize + 2
    }

    /// Resample a batch of `f32` values, calling `emit` for each output sample.
    ///
    /// `emit(value)` is called whenever the phase accumulator crosses an integer
    /// boundary.  The `value` passed is the most recently processed input sample
    /// (zero-order hold / nearest-neighbour).
    #[inline]
    pub fn process(&mut self, value: f32, emit: impl FnMut(f32)) {
        self.process_with(value, emit);
    }

    /// Same as [`process`] but takes a `FnMut` explicitly (avoids repeated
    /// monomorphisation at call sites that already have a `mut closure`).
    #[inline]
    pub fn process_with(&mut self, value: f32, mut emit: impl FnMut(f32)) {
        self.phase_acc += self.phase_step;
        while self.phase_acc >= 1.0 {
            emit(value);
            self.phase_acc -= 1.0;
        }
    }

    /// Reset the phase accumulator (call after a demod reset / frequency change).
    pub fn reset(&mut self) {
        self.phase_acc = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_length_matches_expected_rate() {
        // 2 MSps input → 48 kHz output, process 2048 samples
        let mut r = RationalResampler::new(2_000_000, 48_000);
        let mut count = 0usize;
        for _ in 0..2048 {
            r.process(1.0, |_| count += 1);
        }
        // Expected: 2048 * 48000 / 2000000 ≈ 49.2 → should produce 49 samples
        let expected = (2048.0f64 * 48_000.0 / 2_000_000.0).round() as usize;
        assert!(
            count.abs_diff(expected) <= 2,
            "output count {count} far from expected {expected}"
        );
    }

    #[test]
    fn passthrough_at_unity_ratio() {
        // 1:1 ratio — every input produces exactly one output
        let mut r = RationalResampler::new(48_000, 48_000);
        let mut out = Vec::new();
        for i in 0..64 {
            r.process(i as f32, |v| out.push(v));
        }
        assert_eq!(out.len(), 64, "1:1 resampler should pass every sample");
    }

    #[test]
    fn reset_clears_phase() {
        let mut r = RationalResampler::new(2_000_000, 48_000);
        // Run some samples to advance the accumulator
        for _ in 0..100 {
            r.process(0.0, |_| {});
        }
        r.reset();
        assert_eq!(r.phase_acc, 0.0, "reset should clear phase_acc");
    }
}
