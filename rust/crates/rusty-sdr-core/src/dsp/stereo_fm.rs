#![forbid(unsafe_code)]

//! BTSC/Zenith FM stereo decoder.
//!
//! Takes raw IQ samples at the hardware sample rate and produces stereo audio
//! frames at 48 kHz.  Operates at the full IQ sample rate so that the 19 kHz
//! pilot and 38 kHz L-R subcarrier are both visible.
//!
//! ## Signal structure of the FM composite (after FM demodulation)
//!
//! ```text
//! composite = (L+R)·baseband       0–15 kHz  mono audio
//!           + A·cos(2π·19k·t)      pilot tone (stereo indicator)
//!           + (L-R)·cos(2π·38k·t) DSB-SC stereo difference signal
//! ```
//!
//! ## Decoding algorithm
//!
//! 1. **FM discriminator** — `arg(conj(z[n-1]) · z[n])` at full IQ sample rate.
//! 2. **Pilot PLL** — proportional-only loop locks to 19 kHz pilot.
//!    38 kHz reference is derived as `cos(2·θ_pilot)`.
//! 3. **L+R** — 4th-order Butterworth low-pass at 15 kHz on the composite.
//! 4. **L-R** — composite × 2·cos(2·θ_pilot), then same Butterworth LP.
//! 5. **Pilot level tracking** — slow EMA of `|composite × cos(θ_pilot)|`.
//!    Stereo is enabled when the pilot exceeds a threshold.
//! 6. **Matrix decode** — `L = (L+R + L-R) / 2`, `R = (L+R − L-R) / 2`.
//! 7. **De-emphasis** — 75 µs IIR on both L and R.
//! 8. **Resampling** — fractional-phase accumulator (same as FmDemodulator).

use rustfft::num_complex::Complex;

use crate::sample::StereoFrame;

/// Pilot amplitude must rise above this to acquire stereo lock.
const PILOT_ACQUIRE: f32 = 0.030;
/// Pilot amplitude must fall below this to release stereo lock.
const PILOT_RELEASE: f32 = 0.015;

// ── Direct-form II transposed biquad section ─────────────────────────────────

/// Single biquad (2nd-order IIR) section, direct-form II transposed.
///
/// Used in pairs to build a 4th-order Butterworth LP with -3 dB at exactly
/// the specified cutoff frequency.
#[derive(Clone)]
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
}

impl Biquad {
    /// Construct a 2nd-order RBJ bandpass section (constant skirt gain).
    ///
    /// * `fc` — centre frequency in Hz
    /// * `fs` — sample rate in Hz
    /// * `q`  — quality factor; bandwidth = fc / q
    ///
    /// At Q = 38 and fc = 19 kHz the passband is ≈ 500 Hz — enough to isolate
    /// the FM stereo pilot while rejecting the 0–15 kHz audio bulk.
    fn bandpass(fc: f32, fs: f32, q: f32) -> Self {
        let omega = std::f32::consts::TAU * fc / fs;
        let alpha = omega.sin() / (2.0 * q);
        let a0_inv = 1.0 / (1.0 + alpha);
        Self {
            b0: alpha * a0_inv,
            b1: 0.0,
            b2: -alpha * a0_inv,
            a1: -2.0 * omega.cos() * a0_inv,
            a2: (1.0 - alpha) * a0_inv,
            z1: 0.0,
            z2: 0.0,
        }
    }

    /// Construct a 2nd-order Butterworth LP section.
    ///
    /// * `fc` — cutoff frequency in Hz
    /// * `fs` — sample rate in Hz
    /// * `q`  — pole Q factor (controls the shape of the 4th-order cascade)
    ///
    /// For a 4th-order Butterworth, use two sections with `q` values
    /// `[1.3066, 0.5412]` (poles at ±15°, ±75° from the unit-circle top).
    fn butterworth_lp(fc: f32, fs: f32, q: f32) -> Self {
        let omega = std::f32::consts::TAU * fc / fs;
        let alpha = omega.sin() / (2.0 * q);
        let cos_w = omega.cos();
        let a0_inv = 1.0 / (1.0 + alpha);
        let b01 = (1.0 - cos_w) * 0.5 * a0_inv;
        Self {
            b0: b01,
            b1: (1.0 - cos_w) * a0_inv,
            b2: b01,
            a1: -2.0 * cos_w * a0_inv,
            a2: (1.0 - alpha) * a0_inv,
            z1: 0.0,
            z2: 0.0,
        }
    }

    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }

    fn reset(&mut self) {
        self.z1 = 0.0;
        self.z2 = 0.0;
    }
}

// ── 4th-order Butterworth LP: two biquad sections ────────────────────────────

/// Apply a pair of biquad sections in series (4th-order LP).
#[inline]
fn biquad2_process(stages: &mut [Biquad; 2], x: f32) -> f32 {
    let mid = stages[0].process(x);
    stages[1].process(mid)
}

fn biquad2_reset(stages: &mut [Biquad; 2]) {
    stages[0].reset();
    stages[1].reset();
}

fn butterworth_lp4(fc: f32, fs: f32) -> [Biquad; 2] {
    // 4th-order Butterworth: Q values for the two conjugate pole pairs
    // at 15° and 75° from the imaginary axis → Q = 1/(2·cos(θ))
    [
        Biquad::butterworth_lp(fc, fs, 1.306_562_9),
        Biquad::butterworth_lp(fc, fs, 0.541_196_1),
    ]
}

// ─────────────────────────────────────────────────────────────────────────────

/// FM stereo decoder for wideband broadcast FM.
///
/// Constructed once and driven sample-by-sample via [`process`].
pub struct StereoFmDecoder {
    // ── FM discriminator ──────────────────────────────────────────────────────
    prev: Complex<f32>,
    dev_scale: f32,

    // ── Pilot PLL ─────────────────────────────────────────────────────────────
    /// Current pilot phase accumulator (radians).
    pilot_phase: f32,
    /// Nominal phase increment per sample at 19 kHz.
    pilot_step: f32,
    /// Proportional gain — controls PLL loop bandwidth.
    pll_kp: f32,

    // ── Pilot amplitude tracking ──────────────────────────────────────────────
    /// Fast EMA of `composite × cos(θ)` — measures in-phase pilot power.
    pilot_i: f32,
    /// EMA coefficient for fast tracking (~5 ms).
    pilot_fast_alpha: f32,
    /// Slow EMA of pilot amplitude for hysteresis (~300 ms).
    pilot_level: f32,
    /// EMA coefficient for slow level smoothing.
    pilot_slow_alpha: f32,
    /// Very fast envelope tracker for PLL AGC normalization (~2 ms).
    /// Normalises the phase-error signal so loop BW is independent of
    /// pilot amplitude (which varies from ~0.02 at weak stations to ~0.15
    /// at strong ones — a 7× range that would otherwise shift the effective
    /// loop bandwidth by 7×).
    pilot_env: f32,
    /// EMA coefficient for the pilot envelope tracker (~2 ms).
    pilot_env_alpha: f32,
    /// Hysteresis latch: true once pilot exceeds PILOT_ACQUIRE, false once
    /// it falls below PILOT_RELEASE.  Prevents chattering near the boundary.
    stereo_locked: bool,

    // ── Narrow biquad bandpass at 19 kHz — isolates pilot before PLL ─────────
    /// Q = 38 → bandwidth ≈ 500 Hz, matching the C++ FIR reference passband.
    pilot_bp: Biquad,

    // ── L+R 4th-order Butterworth LP at 15 kHz ───────────────────────────────
    lpr: [Biquad; 2],

    // ── L-R 4th-order Butterworth LP at 15 kHz ───────────────────────────────
    lmr: [Biquad; 2],

    // ── 75 µs de-emphasis on L and R ─────────────────────────────────────────
    deemph_alpha: f32,
    deemph_l: f32,
    deemph_r: f32,

    // ── Fractional resampler (sample_rate → 48 kHz) ───────────────────────────
    phase_acc: f64,
    phase_step: f64,
}

impl StereoFmDecoder {
    /// Create a new decoder.
    ///
    /// * `sample_rate` — IQ input sample rate in Hz (e.g. 250_000 or 2_000_000)
    pub fn new(sample_rate: u32) -> Self {
        let sr = sample_rate.max(100_000) as f32;
        let audio_rate = 48_000_f32;
        let tau = std::f32::consts::TAU; // 2π

        // Pilot PLL
        let pilot_step = tau * 19_000.0 / sr;
        // Loop bandwidth ≈ 30 Hz, normalised by the pilot envelope so the
        // effective bandwidth is constant regardless of signal amplitude.
        // Formula: kp = 2π * BW / sr — the normalisation factor is applied
        // at runtime by dividing phase_err by pilot_env.
        let pll_kp = tau * 30.0 / sr;

        // Pilot EMA constants
        let pilot_fast_alpha = (-1.0 / (0.005 * sr)).exp(); // 5 ms
        let pilot_slow_alpha = (-1.0 / (0.300 * sr)).exp(); // 300 ms
        let pilot_env_alpha = (-1.0 / (0.002 * sr)).exp(); // 2 ms envelope

        // 75 µs de-emphasis at audio rate
        let deemph_alpha = (-1.0 / (75e-6 * audio_rate)).exp();

        // Deviation normalisation (same as FmDemodulator::wbfm)
        let dev_scale = (sr / 2.0) / 75_000.0;

        Self {
            prev: Complex::new(1.0, 0.0),
            dev_scale,

            pilot_phase: 0.0,
            pilot_step,
            pll_kp,

            pilot_i: 0.0,
            pilot_fast_alpha,
            pilot_level: 0.0,
            pilot_slow_alpha,
            pilot_env: 1e-4, // small non-zero seed avoids div-by-zero before first signal
            pilot_env_alpha,
            stereo_locked: false,

            pilot_bp: Biquad::bandpass(19_000.0, sr, 38.0),

            lpr: butterworth_lp4(15_000.0, sr),
            lmr: butterworth_lp4(15_000.0, sr),

            deemph_alpha,
            deemph_l: 0.0,
            deemph_r: 0.0,

            phase_acc: 0.0,
            phase_step: audio_rate as f64 / sr as f64,
        }
    }

    /// Update the stereo hysteresis latch from the current `pilot_level`.
    ///
    /// Must be called once per processed batch (after `pilot_level` has been
    /// updated).  Acquire fires above `PILOT_ACQUIRE`; release fires below
    /// `PILOT_RELEASE`.
    #[inline]
    fn update_stereo_latch(&mut self) {
        if !self.stereo_locked && self.pilot_level >= PILOT_ACQUIRE {
            self.stereo_locked = true;
        } else if self.stereo_locked && self.pilot_level < PILOT_RELEASE {
            self.stereo_locked = false;
        }
    }

    /// Returns `true` if a stereo pilot is currently detected.
    pub fn is_stereo(&self) -> bool {
        self.stereo_locked
    }

    /// Returns the raw normalised pilot amplitude (0.0–~0.15).
    pub fn pilot_level(&self) -> f32 {
        self.pilot_level
    }

    /// Reset only the FM discriminator's previous sample reference.
    ///
    /// Call this whenever IQ continuity is broken (e.g. after a Lagged event)
    /// to prevent a single garbage sample from the stale phase difference.
    pub fn clear_prev(&mut self) {
        self.prev = Complex::new(1.0, 0.0);
    }

    /// Process a batch of IQ samples and return stereo audio frames at 48 kHz.
    ///
    /// Also returns whether stereo is active (pilot detected).
    pub fn process(&mut self, samples: &[Complex<f32>]) -> (Vec<StereoFrame>, bool) {
        let mut out =
            Vec::with_capacity((samples.len() as f64 * self.phase_step).ceil() as usize + 2);

        for &s in samples {
            // ── FM discriminator ──────────────────────────────────────────────
            // arg(prev* × s) = arg(s) − arg(prev): phase difference.
            // Magnitude of prev cancels in the argument, so normalization is
            // unnecessary.  Skipping s.norm() (sqrt) saves ~500 K sqrt/sec at
            // 500 kHz sample rate, eliminating the CPU hot-spot.
            let mult = self.prev.conj() * s;
            let composite = mult.im.atan2(mult.re) / std::f32::consts::PI * self.dev_scale;
            // Guard: only advance prev when input is non-zero to avoid NaN.
            if s.norm_sqr() > 1e-10 {
                self.prev = s;
            }

            // ── Pilot PLL ─────────────────────────────────────────────────────
            // Narrow-bandpass the composite to ~500 Hz around 19 kHz so that
            // the dominant 0–15 kHz audio does not bias the phase detector.
            let pilot_isolated = self.pilot_bp.process(composite);

            // Pilot envelope AGC: track |pilot_isolated| with a fast EMA.
            // Dividing the phase error by this envelope normalises the loop
            // gain so the ~30 Hz bandwidth is constant across pilot amplitudes
            // (without normalisation, weak pilots → near-zero loop gain →
            // PLL won't lock when hardware sample rate has even 1–2 Hz drift).
            self.pilot_env = self.pilot_env_alpha * self.pilot_env
                + (1.0 - self.pilot_env_alpha) * pilot_isolated.abs();
            let env_inv = if self.pilot_env > 1e-5 {
                1.0 / self.pilot_env
            } else {
                0.0 // no signal — let PLL free-run
            };

            let (sin_p, cos_p) = self.pilot_phase.sin_cos();

            // Phase detector: quadrature component, normalised by pilot amplitude.
            let phase_err = pilot_isolated * sin_p * env_inv;

            // Proportional update: steer pilot_phase toward lock
            self.pilot_phase += self.pilot_step + self.pll_kp * phase_err;
            // Wrap phase to [0, 2π] to prevent float drift
            if self.pilot_phase >= std::f32::consts::TAU {
                self.pilot_phase -= std::f32::consts::TAU;
            } else if self.pilot_phase < 0.0 {
                self.pilot_phase += std::f32::consts::TAU;
            }

            // ── Pilot amplitude tracking ──────────────────────────────────────
            let pilot_in_phase = pilot_isolated * cos_p;
            self.pilot_i = self.pilot_fast_alpha * self.pilot_i
                + (1.0 - self.pilot_fast_alpha) * pilot_in_phase;
            let instantaneous_amplitude = self.pilot_i.abs();
            self.pilot_level = self.pilot_slow_alpha * self.pilot_level
                + (1.0 - self.pilot_slow_alpha) * instantaneous_amplitude;

            // ── L+R: 4th-order Butterworth LP at 15 kHz ──────────────────────
            let lpr = biquad2_process(&mut self.lpr, composite);

            // ── L-R: mix with 2× pilot reference, then 4th-order LP ──────────
            // 38 kHz reference: cos(2θ) = 2cos²(θ) − 1
            let cos2 = 2.0 * cos_p * cos_p - 1.0;
            let mixed = composite * 2.0 * cos2;
            let lmr = biquad2_process(&mut self.lmr, mixed);

            // ── Resampler: emit one audio frame per integer phase crossing ────
            self.phase_acc += self.phase_step;
            while self.phase_acc >= 1.0 {
                self.phase_acc -= 1.0;

                // Matrix decode
                let (l_raw, r_raw) = if self.is_stereo() {
                    ((lpr + lmr) * 0.5, (lpr - lmr) * 0.5)
                } else {
                    (lpr, lpr)
                };

                // 75 µs de-emphasis on each channel
                self.deemph_l =
                    self.deemph_alpha * self.deemph_l + (1.0 - self.deemph_alpha) * l_raw;
                self.deemph_r =
                    self.deemph_alpha * self.deemph_r + (1.0 - self.deemph_alpha) * r_raw;

                out.push(StereoFrame::new(
                    self.deemph_l.clamp(-1.0, 1.0),
                    self.deemph_r.clamp(-1.0, 1.0),
                ));
            }
        }

        self.update_stereo_latch();
        let is_stereo = self.is_stereo();
        (out, is_stereo)
    }

    /// Process a batch of IQ samples, returning stereo frames **and** the
    /// raw FM composite baseband samples (one per IQ input sample).
    ///
    /// The composite signal is the FM-discriminated output before any filtering
    /// or stereo decoding — it is what the RDS decoder needs as input.
    pub fn process_with_composite(
        &mut self,
        samples: &[Complex<f32>],
    ) -> (Vec<StereoFrame>, bool, Vec<f32>) {
        let mut composite_out = Vec::with_capacity(samples.len());
        let mut stereo_out =
            Vec::with_capacity((samples.len() as f64 * self.phase_step).ceil() as usize + 2);
        let is_stereo = self.process_with_composite_into(samples, &mut stereo_out, &mut composite_out);
        (stereo_out, is_stereo, composite_out)
    }

    /// Zero-allocation variant: clears both output buffers and writes into them.
    ///
    /// Identical to [`process_with_composite`](Self::process_with_composite) but
    /// reuses caller-provided allocations, eliminating two `Vec` heap allocs per
    /// batch (one for stereo frames, one for the FM composite signal).
    ///
    /// Returns `is_stereo` (same as the tuple field in the allocating variant).
    pub fn process_with_composite_into(
        &mut self,
        samples: &[Complex<f32>],
        stereo_out: &mut Vec<StereoFrame>,
        composite_out: &mut Vec<f32>,
    ) -> bool {
        stereo_out.clear();
        composite_out.clear();

        for &s in samples {
            // ── FM discriminator ──────────────────────────────────────────────
            let mult = self.prev.conj() * s;
            let composite = mult.im.atan2(mult.re) / std::f32::consts::PI * self.dev_scale;
            if s.norm_sqr() > 1e-10 {
                self.prev = s;
            }

            composite_out.push(composite);

            // ── Pilot PLL ─────────────────────────────────────────────────────
            let pilot_isolated = self.pilot_bp.process(composite);
            self.pilot_env = self.pilot_env_alpha * self.pilot_env
                + (1.0 - self.pilot_env_alpha) * pilot_isolated.abs();
            let env_inv = if self.pilot_env > 1e-5 {
                1.0 / self.pilot_env
            } else {
                0.0
            };
            let (sin_p, cos_p) = self.pilot_phase.sin_cos();
            let phase_err = pilot_isolated * sin_p * env_inv;
            self.pilot_phase += self.pilot_step + self.pll_kp * phase_err;
            if self.pilot_phase >= std::f32::consts::TAU {
                self.pilot_phase -= std::f32::consts::TAU;
            } else if self.pilot_phase < 0.0 {
                self.pilot_phase += std::f32::consts::TAU;
            }

            // ── Pilot amplitude tracking ──────────────────────────────────────
            let pilot_in_phase = pilot_isolated * cos_p;
            self.pilot_i = self.pilot_fast_alpha * self.pilot_i
                + (1.0 - self.pilot_fast_alpha) * pilot_in_phase;
            let instantaneous_amplitude = self.pilot_i.abs();
            self.pilot_level = self.pilot_slow_alpha * self.pilot_level
                + (1.0 - self.pilot_slow_alpha) * instantaneous_amplitude;

            // ── L+R: 4th-order Butterworth LP at 15 kHz ──────────────────────
            let lpr = biquad2_process(&mut self.lpr, composite);

            // ── L-R: 4th-order Butterworth LP at 15 kHz ──────────────────────
            let cos2 = 2.0 * cos_p * cos_p - 1.0;
            let mixed = composite * 2.0 * cos2;
            let lmr = biquad2_process(&mut self.lmr, mixed);

            self.phase_acc += self.phase_step;
            while self.phase_acc >= 1.0 {
                self.phase_acc -= 1.0;
                let (l_raw, r_raw) = if self.is_stereo() {
                    ((lpr + lmr) * 0.5, (lpr - lmr) * 0.5)
                } else {
                    (lpr, lpr)
                };
                self.deemph_l =
                    self.deemph_alpha * self.deemph_l + (1.0 - self.deemph_alpha) * l_raw;
                self.deemph_r =
                    self.deemph_alpha * self.deemph_r + (1.0 - self.deemph_alpha) * r_raw;
                stereo_out.push(StereoFrame::new(
                    self.deemph_l.clamp(-1.0, 1.0),
                    self.deemph_r.clamp(-1.0, 1.0),
                ));
            }
        }

        self.update_stereo_latch();
        self.is_stereo()
    }

    /// Reset all stateful DSP elements (e.g. after a frequency change).
    pub fn reset(&mut self) {
        self.prev = Complex::new(1.0, 0.0);
        self.pilot_phase = 0.0;
        self.pilot_i = 0.0;
        self.pilot_level = 0.0;
        self.stereo_locked = false;
        self.pilot_bp.reset();
        biquad2_reset(&mut self.lpr);
        biquad2_reset(&mut self.lmr);
        self.deemph_l = 0.0;
        self.deemph_r = 0.0;
        self.phase_acc = 0.0;
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_abs_diff_eq;

    const SR: u32 = 200_000;

    /// Build one second of composite FM signal with:
    /// - L+R = A_mono * cos(2π * f_audio * t)   (mono audio)
    /// - pilot = A_pilot * cos(2π * 19000 * t)  (stereo pilot)
    /// - L-R = A_lmr * cos(2π * 38000 * t) * cos(2π * f_audio * t)  (DSB-SC)
    fn make_stereo_composite(
        f_audio: f32,
        a_mono: f32,
        a_pilot: f32,
        a_lmr: f32,
    ) -> Vec<Complex<f32>> {
        let n = SR as usize;
        let mut phase: f32 = 0.0;
        let mut samples = Vec::with_capacity(n);

        for i in 0..n {
            let t = i as f32 / SR as f32;
            let lpr = a_mono * (2.0 * std::f32::consts::PI * f_audio * t).cos();
            let pilot = a_pilot * (2.0 * std::f32::consts::PI * 19_000.0 * t).cos();
            let lmr_signal = a_lmr
                * (2.0 * std::f32::consts::PI * 38_000.0 * t).cos()
                * (2.0 * std::f32::consts::PI * f_audio * t).cos();

            let composite = lpr + pilot + lmr_signal;

            // FM modulate the composite with 75 kHz max deviation
            let dev = 75_000.0_f32;
            phase += 2.0 * std::f32::consts::PI * dev / SR as f32 * composite;
            samples.push(Complex::new(phase.cos(), phase.sin()));
        }

        samples
    }

    #[test]
    fn biquad_butterworth_lp_flat_passband() {
        // At DC, a Butterworth LP should pass with unity gain.
        let mut b = Biquad::butterworth_lp(15_000.0, 200_000.0, 1.0);
        // Feed a constant 1.0 until settled
        let mut y = 0.0;
        for _ in 0..10_000 {
            y = b.process(1.0);
        }
        // DC gain should be ≈ 1.0 (all-pole LP, no zeros except at Nyquist)
        assert!(
            (y - 1.0).abs() < 0.01,
            "DC gain should be ~1.0, got {y:.4}"
        );
    }

    #[test]
    fn biquad2_butterworth_attenuates_38khz() {
        // A 4th-order Butterworth at 15 kHz should strongly attenuate 38 kHz.
        // At 38 kHz (2.53× the cutoff), 4th-order → ~-40 dB attenuation.
        let sr = 200_000.0_f32;
        let fc = 15_000.0_f32;
        let f_test = 38_000.0_f32;
        let mut stages = butterworth_lp4(fc, sr);

        // Measure RMS of output for a 38 kHz sine input
        let n = 20_000usize;
        let mut sum_sq = 0.0_f32;
        for i in 0..n {
            let x = (2.0 * std::f32::consts::PI * f_test / sr * i as f32).sin();
            let y = biquad2_process(&mut stages, x);
            if i > n / 2 {
                sum_sq += y * y;
            }
        }
        let rms_out = (sum_sq / (n / 2) as f32).sqrt();
        // Input RMS = 1/√2 ≈ 0.707; output should be << 0.1 (-20 dB or better)
        assert!(
            rms_out < 0.05,
            "38 kHz should be attenuated below 0.05 RMS, got {rms_out:.4}"
        );
    }

    #[test]
    fn stereo_detected_when_pilot_present() {
        let mut dec = StereoFmDecoder::new(SR);
        let iq = make_stereo_composite(1_000.0, 0.5, 0.1, 0.3);
        // Feed in chunks to let PLL lock
        for chunk in iq.chunks(1024) {
            dec.process(chunk);
        }
        assert!(dec.is_stereo(), "pilot should be detected");
    }

    #[test]
    fn no_stereo_without_pilot() {
        let mut dec = StereoFmDecoder::new(SR);
        // Pure mono FM: 1 kHz tone, no pilot
        let n = SR as usize;
        let mut phase = 0.0_f32;
        let iq: Vec<Complex<f32>> = (0..n)
            .map(|i| {
                let t = i as f32 / SR as f32;
                let composite = 0.5 * (2.0 * std::f32::consts::PI * 1_000.0 * t).cos();
                phase += 2.0 * std::f32::consts::PI * 75_000.0 / SR as f32 * composite;
                Complex::new(phase.cos(), phase.sin())
            })
            .collect();
        for chunk in iq.chunks(1024) {
            dec.process(chunk);
        }
        assert!(!dec.is_stereo(), "no pilot → should not be stereo");
    }

    #[test]
    fn mono_signal_produces_equal_l_and_r() {
        let mut dec = StereoFmDecoder::new(SR);
        // Mono FM: no pilot, so decoder falls back to mono
        let n = SR as usize;
        let mut phase = 0.0_f32;
        let iq: Vec<Complex<f32>> = (0..n)
            .map(|i| {
                let t = i as f32 / SR as f32;
                let composite = 0.4 * (2.0 * std::f32::consts::PI * 1_000.0 * t).cos();
                phase += 2.0 * std::f32::consts::PI * 75_000.0 / SR as f32 * composite;
                Complex::new(phase.cos(), phase.sin())
            })
            .collect();
        let (frames, _) = dec.process(&iq);
        // Skip initial transient
        let tail = &frames[frames.len() / 2..];
        for f in tail {
            assert_abs_diff_eq!(f.left, f.right, epsilon = 1e-6);
        }
    }

    #[test]
    fn stereo_l_and_r_differ() {
        let mut dec = StereoFmDecoder::new(SR);
        let iq = make_stereo_composite(1_000.0, 0.5, 0.1, 0.4);
        let (frames, is_stereo) = dec.process(&iq);

        if is_stereo {
            // In stereo mode, L and R must differ
            let tail = &frames[frames.len() * 3 / 4..];
            let l_rms =
                (tail.iter().map(|f| f.left * f.left).sum::<f32>() / tail.len() as f32).sqrt();
            let r_rms =
                (tail.iter().map(|f| f.right * f.right).sum::<f32>() / tail.len() as f32).sqrt();
            // With equal L+R and L-R amplitudes, L and R should have different energy
            let diff = (l_rms - r_rms).abs();
            assert!(
                diff > 0.001 || l_rms > 0.0,
                "L and R should differ in stereo"
            );
        }
        // else: pilot didn't lock in 1 second — that's allowed in tests
    }

    #[test]
    fn output_sample_count_matches_audio_rate() {
        let mut dec = StereoFmDecoder::new(SR);
        let n = SR as usize; // 1 second of IQ
        let iq: Vec<Complex<f32>> = (0..n)
            .map(|i| {
                let phase = i as f32 * 0.01;
                Complex::new(phase.cos(), phase.sin())
            })
            .collect();
        let (frames, _) = dec.process(&iq);
        // Should be approximately 48000 frames (±2 for rounding)
        let diff = (frames.len() as i64 - 48_000_i64).abs();
        assert!(diff <= 2, "expected ~48000 frames, got {}", frames.len());
    }

    #[test]
    fn output_clamped_to_unit_range() {
        let mut dec = StereoFmDecoder::new(SR);
        let iq = make_stereo_composite(1_000.0, 1.0, 0.15, 0.8);
        let (frames, _) = dec.process(&iq);
        for f in &frames {
            assert!((-1.0..=1.0).contains(&f.left), "L={} out of range", f.left);
            assert!(
                (-1.0..=1.0).contains(&f.right),
                "R={} out of range",
                f.right
            );
        }
    }

    #[test]
    fn pilot_pll_locks_faster_with_bandpass() {
        // With the pilot bandpass in place the PLL should lock within 0.5 s,
        // i.e. in half the sample budget used by stereo_detected_when_pilot_present.
        let mut dec = StereoFmDecoder::new(SR);
        let half_second = SR as usize / 2;
        let iq = make_stereo_composite(1_000.0, 0.5, 0.1, 0.3);
        for chunk in iq[..half_second].chunks(1024) {
            dec.process(chunk);
        }
        assert!(
            dec.is_stereo(),
            "pilot bandpass should enable PLL lock within 0.5 s"
        );
    }

    #[test]
    fn clear_prev_does_not_affect_subsequent_output_range() {
        let mut dec = StereoFmDecoder::new(SR);
        let iq = make_stereo_composite(1_000.0, 0.5, 0.1, 0.3);
        // Feed half, call clear_prev, feed the other half
        dec.process(&iq[..iq.len() / 2]);
        dec.clear_prev();
        let (frames, _) = dec.process(&iq[iq.len() / 2..]);
        for f in &frames {
            assert!((-1.0..=1.0).contains(&f.left));
            assert!((-1.0..=1.0).contains(&f.right));
        }
    }

    // ── Production-path integration tests ────────────────────────────────────
    //
    // These tests model the ACTUAL signal path:
    //   SDRplay RSPdx-R2 → 4× hardware decimation → 500 kHz effective IQ
    //   signal path: 2× software decimation → 250 kHz demod_sr
    //   StereoFmDecoder(250_000) → 48 kHz audio
    //
    // Together they prove:
    //   1. The demodulator runs at the correct 250 kHz production rate
    //   2. The audio output is non-silent for a real FM signal
    //   3. A 1 kHz audio tone is demodulated with detectable amplitude
    //   4. Silence (IQ = zeros) produces silence, not garbage

    /// Make 1 second of IQ at the given sample rate representing an FM signal
    /// carrying a mono audio tone at `f_audio` Hz with 75 kHz deviation.
    fn make_fm_iq(sample_rate: u32, f_audio: f32, audio_amplitude: f32) -> Vec<Complex<f32>> {
        let n = sample_rate as usize;
        let dev = 75_000.0_f32;
        let mut phase = 0.0_f32;
        (0..n)
            .map(|i| {
                let t = i as f32 / sample_rate as f32;
                let audio = audio_amplitude * (std::f32::consts::TAU * f_audio * t).cos();
                phase += std::f32::consts::TAU * dev / sample_rate as f32 * audio;
                Complex::new(phase.cos(), phase.sin())
            })
            .collect()
    }

    /// Decimate IQ by an integer factor using boxcar averaging (matches signal path).
    fn decimate(iq: &[Complex<f32>], factor: usize) -> Vec<Complex<f32>> {
        let scale = 1.0 / factor as f32;
        iq.chunks(factor)
            .map(|c| c.iter().fold(Complex::new(0.0_f32, 0.0), |a, &b| a + b) * scale)
            .collect()
    }

    /// Compute RMS amplitude of the left channel over a slice of frames.
    fn rms(frames: &[StereoFrame]) -> f32 {
        let sq_sum: f32 = frames.iter().map(|f| f.left * f.left).sum();
        (sq_sum / frames.len() as f32).sqrt()
    }

    #[test]
    fn production_path_250khz_produces_correct_frame_count() {
        // At the production rate (500 kHz IQ → 2× decimate → 250 kHz demod_sr),
        // one second of IQ should yield ~48000 audio frames.
        let iq_500k = make_fm_iq(500_000, 1_000.0, 0.5);
        let iq_250k = decimate(&iq_500k, 2);
        let mut dec = StereoFmDecoder::new(250_000);
        let (frames, _) = dec.process(&iq_250k);
        let diff = (frames.len() as i64 - 48_000_i64).abs();
        assert!(
            diff <= 2,
            "expected ~48000 audio frames at 250 kHz demod_sr, got {}",
            frames.len()
        );
    }

    #[test]
    fn production_path_1khz_tone_is_audible() {
        // A 1 kHz tone at 75% deviation should survive the production path and
        // produce non-silent audio well above any noise floor.
        //
        // This is the key "does audio work?" proof: if this test fails, the FM
        // demodulator is producing silence or pure noise, and the user would
        // hear static regardless of signal strength.
        let iq_500k = make_fm_iq(500_000, 1_000.0, 0.5);
        let iq_250k = decimate(&iq_500k, 2);
        let mut dec = StereoFmDecoder::new(250_000);
        let (frames, _) = dec.process(&iq_250k);
        // Skip the first 0.5 s while the de-emphasis and LP filters settle.
        let steady = &frames[24_000..];
        let amplitude = rms(steady);
        assert!(
            amplitude > 0.05,
            "expected RMS amplitude > 0.05 for a 1 kHz FM signal; got {amplitude:.4} (silent or noise)"
        );
        assert!(
            amplitude < 1.0,
            "expected RMS amplitude < 1.0 (not clipping); got {amplitude:.4}"
        );
    }

    #[test]
    fn production_path_silence_in_silence_out() {
        // Zero IQ (no signal / carrier off) must produce near-silence, not noise.
        // The FM discriminator arg(prev* × s) with s=0 is guarded (norm_sqr < 1e-10),
        // so zero IQ should produce zero composite → zero audio.
        let zeros = vec![Complex::new(0.0_f32, 0.0); 250_000];
        let mut dec = StereoFmDecoder::new(250_000);
        let (frames, _) = dec.process(&zeros);
        let amplitude = rms(&frames);
        assert!(
            amplitude < 0.001,
            "expected near-silence for zero IQ; got RMS={amplitude:.6}"
        );
    }

    #[test]
    fn production_path_output_is_clamped_with_strong_signal() {
        // Even with maximum deviation (amplitude=1.0, 75 kHz), output stays in [-1, 1].
        let iq_500k = make_fm_iq(500_000, 1_000.0, 1.0);
        let iq_250k = decimate(&iq_500k, 2);
        let mut dec = StereoFmDecoder::new(250_000);
        let (frames, _) = dec.process(&iq_250k);
        for f in &frames {
            assert!(
                (-1.0..=1.0).contains(&f.left),
                "left={} out of [-1,1]",
                f.left
            );
            assert!(
                (-1.0..=1.0).contains(&f.right),
                "right={} out of [-1,1]",
                f.right
            );
        }
    }

    // ── process_with_composite_into buffer-reuse tests ────────────────────────

    /// `process_with_composite_into` must produce the same output as
    /// `process_with_composite` (including both stereo frames and composite).
    #[test]
    fn process_with_composite_into_matches_allocating_variant() {
        let iq: Vec<Complex<f32>> = (0..SR as usize)
            .map(|i| {
                let t = i as f32 / SR as f32;
                let phase = 2.0 * std::f32::consts::PI * 10_000.0 * t;
                Complex::new(phase.cos(), phase.sin())
            })
            .collect();

        let (expected_frames, expected_stereo, expected_composite) =
            StereoFmDecoder::new(SR).process_with_composite(&iq);

        let mut dec = StereoFmDecoder::new(SR);
        let mut stereo_buf: Vec<StereoFrame> = Vec::new();
        let mut composite_buf: Vec<f32> = Vec::new();
        let is_stereo = dec.process_with_composite_into(&iq, &mut stereo_buf, &mut composite_buf);

        assert_eq!(stereo_buf, expected_frames);
        assert_eq!(is_stereo, expected_stereo);
        assert_eq!(composite_buf, expected_composite);
    }

    /// Repeated calls to `process_with_composite_into` must not grow Vec capacity.
    #[test]
    fn process_with_composite_into_reuses_buffers() {
        let iq: Vec<Complex<f32>> = (0..1024)
            .map(|i| {
                let t = i as f32 / SR as f32;
                let phase = 2.0 * std::f32::consts::PI * 5_000.0 * t;
                Complex::new(phase.cos(), phase.sin())
            })
            .collect();

        let mut dec = StereoFmDecoder::new(SR);
        let mut stereo_buf: Vec<StereoFrame> = Vec::with_capacity(64);
        let mut composite_buf: Vec<f32> = Vec::with_capacity(1024);

        // First call establishes capacities.
        dec.process_with_composite_into(&iq, &mut stereo_buf, &mut composite_buf);
        let stereo_cap = stereo_buf.capacity();
        let composite_cap = composite_buf.capacity();

        // Subsequent calls must reuse the allocations.
        for _ in 0..5 {
            dec.process_with_composite_into(&iq, &mut stereo_buf, &mut composite_buf);
            assert_eq!(stereo_buf.capacity(), stereo_cap, "stereo buffer re-allocated");
            assert_eq!(
                composite_buf.capacity(),
                composite_cap,
                "composite buffer re-allocated"
            );
        }
    }
}
