#![forbid(unsafe_code)]

//! Demodulators for common SDR modulation schemes.
//!
//! Each demodulator is a stateful struct that processes IQ batches and emits
//! audio samples at the configured `audio_rate`.  The input sample rate is
//! set at construction; the demodulator internally handles rate conversion so
//! callers always receive exactly `round(input_len * audio_rate / sample_rate)`
//! audio samples per call.

use rustfft::num_complex::Complex;

/// FM discriminator + de-emphasis + rational resampler.
///
/// Converts wideband FM (IQ at `sample_rate` Hz) to audio at `audio_rate` Hz.
///
/// Algorithm:
///   1. Phase derivative: `d[n] = arg(conj(z[n-1]) * z[n]) / π`  — this is
///      proportional to instantaneous frequency deviation.
///   2. Rational resampling: phase accumulator stepped by `audio_rate /
///      sample_rate`; emit one output sample per integer crossing.  Produces
///      *exactly* the right number of samples over any integer multiple of the
///      input rate.
///   3. 75 µs de-emphasis IIR (North America/Asia standard):
///      `y[n] = α * y[n-1] + (1 - α) * x[n]`  where α = exp(-1/(τ*fs))
pub struct FmDemodulator {
    prev: Complex<f32>,
    /// Fractional output phase accumulator (0..1)
    phase_acc: f64,
    /// Increment per input sample = audio_rate / sample_rate
    phase_step: f64,
    /// De-emphasis state
    deemph_y: f32,
    /// De-emphasis coefficient α
    deemph_alpha: f32,
    /// FM deviation scale: `max_deviation / (sample_rate / 2)`
    deviation_scale: f32,
}

impl FmDemodulator {
    /// Create a new FM demodulator.
    ///
    /// * `sample_rate` — IQ input sample rate in Hz (e.g. 2_000_000)
    /// * `audio_rate`  — desired audio output rate in Hz (e.g. 48_000)
    /// * `max_dev_hz`  — FM channel max deviation in Hz (75_000 for broadcast)
    /// * `tau_us`      — de-emphasis time constant in µs (75.0 for NA, 50.0 for EU)
    pub fn new(sample_rate: u32, audio_rate: u32, max_dev_hz: f32, tau_us: f32) -> Self {
        let phase_step = audio_rate as f64 / sample_rate as f64;

        // De-emphasis: α = exp(-1 / (τ * fs_audio))
        let tau = tau_us * 1e-6;
        let deemph_alpha = (-1.0 / (tau * audio_rate as f32)).exp();

        // Scale: arg(conj * z) / π  is in [-1, 1] for ±sample_rate/2 Hz deviation.
        // Normalise so that max_dev maps to full scale (~0.9 to leave headroom).
        let deviation_scale = (sample_rate as f32 / 2.0) / max_dev_hz;

        Self {
            prev: Complex::new(1.0, 0.0),
            phase_acc: 0.0,
            phase_step,
            deemph_y: 0.0,
            deemph_alpha,
            deviation_scale,
        }
    }

    /// Convenience constructor for standard wideband FM broadcast (NA).
    pub fn wbfm(sample_rate: u32) -> Self {
        Self::new(sample_rate, 48_000, 75_000.0, 75.0)
    }

    /// Process a batch of IQ samples and return resampled audio.
    ///
    /// Output length ≈ `input_len * audio_rate / sample_rate`.
    pub fn process(&mut self, samples: &[Complex<f32>]) -> Vec<f32> {
        let mut out = Vec::with_capacity(
            (samples.len() as f64 * self.phase_step).ceil() as usize + 2,
        );

        for &s in samples {
            // Phase derivative (FM discriminator)
            let mult = self.prev.conj() * s;
            // atan2 gives [-π, π]; divide by π → [-1, 1]
            let demod = mult.im.atan2(mult.re) / std::f32::consts::PI * self.deviation_scale;
            self.prev = if s.norm_sqr() > 1e-10 {
                // Normalise to unit circle to prevent drift
                s / s.norm()
            } else {
                Complex::new(1.0, 0.0)
            };

            // Rational resampler: emit when phase crosses integer boundary
            self.phase_acc += self.phase_step;
            while self.phase_acc >= 1.0 {
                // De-emphasis IIR
                self.deemph_y = self.deemph_alpha * self.deemph_y + (1.0 - self.deemph_alpha) * demod;
                out.push(self.deemph_y.clamp(-1.0, 1.0));
                self.phase_acc -= 1.0;
            }
        }

        out
    }

    /// Reset demodulator state (e.g. after a frequency change).
    pub fn reset(&mut self) {
        self.prev = Complex::new(1.0, 0.0);
        self.phase_acc = 0.0;
        self.deemph_y = 0.0;
    }
}

// ── AM demodulator ────────────────────────────────────────────────────────────

/// AM envelope detector with rational resampler.
///
/// Algorithm:
///   1. Envelope: `|z[n]| = sqrt(I²+Q²)` — no phase computation needed.
///   2. DC removal: `y[n] = x[n] - x[n-1] + 0.999 * y[n-1]` (single-pole HP).
///   3. Rational resampling: same phase accumulator approach as FmDemodulator.
pub struct AmDemodulator {
    /// Previous raw envelope sample (for DC removal)
    prev_env: f32,
    /// Previous HP filter output (for DC removal)
    dc_y: f32,
    /// HP filter coefficient (≈ 0.999 for ~20 Hz cutoff at 48 kHz)
    dc_coeff: f32,
    phase_acc: f64,
    phase_step: f64,
}

impl AmDemodulator {
    /// Create AM demodulator.
    ///
    /// * `sample_rate` — IQ input sample rate in Hz
    /// * `audio_rate`  — desired audio output rate in Hz
    pub fn new(sample_rate: u32, audio_rate: u32) -> Self {
        let phase_step = audio_rate as f64 / sample_rate as f64;
        // HP coefficient: α ≈ exp(-2π * f_cutoff / fs_audio), f_cutoff = 20 Hz
        let dc_coeff = (-2.0 * std::f32::consts::PI * 20.0 / audio_rate as f32).exp();
        Self {
            prev_env: 0.0,
            dc_y: 0.0,
            dc_coeff,
            phase_acc: 0.0,
            phase_step,
        }
    }

    /// Convenience constructor for standard SDR audio output (48 kHz).
    pub fn standard(sample_rate: u32) -> Self {
        Self::new(sample_rate, 48_000)
    }

    /// Process a batch of IQ samples and return resampled audio.
    pub fn process(&mut self, samples: &[Complex<f32>]) -> Vec<f32> {
        let mut out = Vec::with_capacity(
            (samples.len() as f64 * self.phase_step).ceil() as usize + 2,
        );

        for &s in samples {
            let env = s.norm(); // envelope = |IQ|

            // DC-blocking high-pass filter
            let dc_filtered =
                env - self.prev_env + self.dc_coeff * self.dc_y;
            self.prev_env = env;
            self.dc_y = dc_filtered;

            // Rational resampler
            self.phase_acc += self.phase_step;
            while self.phase_acc >= 1.0 {
                out.push(dc_filtered.clamp(-1.0, 1.0));
                self.phase_acc -= 1.0;
            }
        }

        out
    }

    pub fn reset(&mut self) {
        self.prev_env = 0.0;
        self.dc_y = 0.0;
        self.phase_acc = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_abs_diff_eq;

    #[test]
    fn output_count_matches_expected_rate() {
        let sr = 2_000_000u32;
        let ar = 48_000u32;
        let mut demod = FmDemodulator::wbfm(sr);

        // Feed exactly 1 second of IQ data at sample_rate
        let samples: Vec<Complex<f32>> = (0..sr as usize)
            .map(|i| {
                // 100 kHz tone (well within FM bandwidth)
                let t = i as f32 / sr as f32;
                let phase = 2.0 * std::f32::consts::PI * 100_000.0 * t;
                Complex::new(phase.cos(), phase.sin())
            })
            .collect();

        let out = demod.process(&samples);

        // Should be very close to audio_rate samples (within ±2 for rounding)
        let expected = ar as usize;
        let diff = (out.len() as i64 - expected as i64).abs();
        assert!(diff <= 2, "expected ~{expected} samples, got {}", out.len());
    }

    #[test]
    fn silence_produces_near_zero_audio() {
        let mut demod = FmDemodulator::wbfm(2_000_000);
        // DC signal at 0 Hz deviation should produce ~0 after de-emphasis
        let samples: Vec<Complex<f32>> = (0..2048)
            .map(|_| Complex::new(1.0, 0.0))
            .collect();
        let out = demod.process(&samples);
        for &s in &out {
            assert_abs_diff_eq!(s, 0.0, epsilon = 0.05);
        }
    }

    #[test]
    fn demodulated_tone_is_non_trivial() {
        let sr = 2_000_000u32;
        let mut demod = FmDemodulator::wbfm(sr);
        // Frequency-modulated signal: 1 kHz audio tone, 10 kHz deviation
        let samples: Vec<Complex<f32>> = (0..sr as usize)
            .map(|i| {
                let t = i as f32 / sr as f32;
                // Instantaneous phase: integral of 2π*(fc + Δf*sin(2π*fa*t))
                // Simplified: just the modulating sine's phase
                let phase = 2.0 * std::f32::consts::PI * 10_000.0 / 1_000.0
                    * (2.0 * std::f32::consts::PI * 1_000.0 * t).sin();
                Complex::new(phase.cos(), phase.sin())
            })
            .collect();
        let out = demod.process(&samples);
        // Should have non-trivial values (not all zero)
        let rms = (out.iter().map(|&v| v * v).sum::<f32>() / out.len() as f32).sqrt();
        assert!(rms > 0.001, "FM demod output should be non-trivial, rms={rms}");
    }

    #[test]
    fn output_is_clamped_to_unit_range() {
        let mut demod = FmDemodulator::wbfm(2_000_000);
        // Strong input signal
        let samples: Vec<Complex<f32>> = (0..10_000)
            .map(|i| {
                let phase = i as f32 * 0.3;
                Complex::new(phase.cos() * 10.0, phase.sin() * 10.0)
            })
            .collect();
        let out = demod.process(&samples);
        for &s in &out {
            assert!((-1.0..=1.0).contains(&s), "sample {s} out of [-1, 1]");
        }
    }
}
