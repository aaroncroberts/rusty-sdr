#![forbid(unsafe_code)]

//! Demodulators for common SDR modulation schemes.
//!
//! Each demodulator is a stateful struct that processes IQ batches and emits
//! audio samples at the configured `audio_rate`.  The input sample rate is
//! set at construction; the demodulator internally handles rate conversion so
//! callers always receive exactly `round(input_len * audio_rate / sample_rate)`
//! audio samples per call.

use rustfft::num_complex::Complex;

use super::bandpass::AudioBandpass;
use super::resampler::RationalResampler;

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
    resampler: RationalResampler,
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
        // De-emphasis: α = exp(-1 / (τ * fs_audio))
        let tau = tau_us * 1e-6;
        let deemph_alpha = (-1.0 / (tau * audio_rate as f32)).exp();

        // Scale: arg(conj * z) / π  is in [-1, 1] for ±sample_rate/2 Hz deviation.
        // Normalise so that max_dev maps to full scale (~0.9 to leave headroom).
        let deviation_scale = (sample_rate as f32 / 2.0) / max_dev_hz;

        Self {
            prev: Complex::new(1.0, 0.0),
            resampler: RationalResampler::new(sample_rate, audio_rate),
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
        let mut out = Vec::with_capacity(self.resampler.output_len_hint(samples.len()));

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

            // Split borrows so the closure can mutate deemph_y while resampler is borrowed.
            let deemph_alpha = self.deemph_alpha;
            let resampler = &mut self.resampler;
            let deemph_y = &mut self.deemph_y;
            resampler.process_with(demod, |v| {
                *deemph_y = deemph_alpha * *deemph_y + (1.0 - deemph_alpha) * v;
                out.push((*deemph_y).clamp(-1.0, 1.0));
            });
        }

        out
    }

    /// Reset demodulator state (e.g. after a frequency change).
    pub fn reset(&mut self) {
        self.prev = Complex::new(1.0, 0.0);
        self.resampler.reset();
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
    resampler: RationalResampler,
}

impl AmDemodulator {
    /// Create AM demodulator.
    ///
    /// * `sample_rate` — IQ input sample rate in Hz
    /// * `audio_rate`  — desired audio output rate in Hz
    pub fn new(sample_rate: u32, audio_rate: u32) -> Self {
        // HP coefficient: α ≈ exp(-2π * f_cutoff / fs_audio), f_cutoff = 20 Hz
        let dc_coeff = (-2.0 * std::f32::consts::PI * 20.0 / audio_rate as f32).exp();
        Self {
            prev_env: 0.0,
            dc_y: 0.0,
            dc_coeff,
            resampler: RationalResampler::new(sample_rate, audio_rate),
        }
    }

    /// Convenience constructor for standard SDR audio output (48 kHz).
    pub fn standard(sample_rate: u32) -> Self {
        Self::new(sample_rate, 48_000)
    }

    /// Process a batch of IQ samples and return resampled audio.
    pub fn process(&mut self, samples: &[Complex<f32>]) -> Vec<f32> {
        let mut out = Vec::with_capacity(self.resampler.output_len_hint(samples.len()));

        for &s in samples {
            let env = s.norm(); // envelope = |IQ|

            // DC-blocking high-pass filter
            let dc_filtered = env - self.prev_env + self.dc_coeff * self.dc_y;
            self.prev_env = env;
            self.dc_y = dc_filtered;

            self.resampler
                .process(dc_filtered, |v| out.push(v.clamp(-1.0, 1.0)));
        }

        out
    }

    pub fn reset(&mut self) {
        self.prev_env = 0.0;
        self.dc_y = 0.0;
        self.resampler.reset();
    }
}

// ── Internal biquad for SSB and CW demodulators ───────────────────────────────
//
// Mirrors the private `Biquad` in bandpass.rs (which is inaccessible here).
// Transposed Direct Form II; RBJ Audio EQ Cookbook coefficients.

struct DspBiquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
}

impl DspBiquad {
    fn lowpass(cutoff_hz: f32, fs_hz: f32) -> Self {
        let q = std::f32::consts::FRAC_1_SQRT_2;
        let w0 = 2.0 * std::f32::consts::PI * cutoff_hz / fs_hz;
        let alpha = w0.sin() / (2.0 * q);
        let cos_w0 = w0.cos();
        let b0 = (1.0 - cos_w0) / 2.0;
        let b1 = 1.0 - cos_w0;
        let b2 = (1.0 - cos_w0) / 2.0;
        let a0 = 1.0 + alpha;
        let a1 = -2.0 * cos_w0;
        let a2 = 1.0 - alpha;
        Self {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
            z1: 0.0,
            z2: 0.0,
        }
    }

    fn highpass(cutoff_hz: f32, fs_hz: f32) -> Self {
        let q = std::f32::consts::FRAC_1_SQRT_2;
        let w0 = 2.0 * std::f32::consts::PI * cutoff_hz / fs_hz;
        let alpha = w0.sin() / (2.0 * q);
        let cos_w0 = w0.cos();
        let b0 = (1.0 + cos_w0) / 2.0;
        let b1 = -(1.0 + cos_w0);
        let b2 = (1.0 + cos_w0) / 2.0;
        let a0 = 1.0 + alpha;
        let a1 = -2.0 * cos_w0;
        let a2 = 1.0 - alpha;
        Self {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
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

// ── SSB / DSB demodulator (Weaver phasing method) ─────────────────────────────

/// Sideband selection for [`SsbDemodulator`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SsbMode {
    /// Upper sideband — demodulates positive-frequency (above-carrier) content.
    Usb,
    /// Lower sideband — demodulates negative-frequency (below-carrier) content.
    Lsb,
    /// Double sideband — passes both sidebands; takes I channel directly.
    Dsb,
}

/// SSB and DSB demodulator using the Weaver phasing method.
///
/// Algorithm (USB / LSB):
///   1. **Mix down** input IQ by fc = 1650 Hz (midpoint of the 300–3 kHz voice
///      band): `z_bb[n] = z[n] · exp(-j·2π·fc/fs·n)`.
///      For LSB the input Q is negated first (conjugates the spectrum), so the
///      upper sideband of the original signal shifts into the passband.
///   2. **Lowpass filter** I and Q of `z_bb` with a 4th-order Butterworth at
///      1350 Hz (two cascaded 2nd-order sections ≈ 25 dB rejection at 2650 Hz).
///   3. **Mix back up** and take the real part:
///      `audio = I_filt · cos(θ) − Q_filt · sin(θ)`.
///   4. **Rational resample** to `audio_rate` via a phase accumulator; apply a
///      300–3 kHz voice bandpass filter at audio rate.
///
/// Algorithm (DSB):
///   Takes the real part (I channel) of the baseband IQ directly, lowpass-
///   filtered at input rate, resampled, then voice-bandpassed at audio rate.
///
/// The Weaver oscillator advances at input rate; the mix-up step uses the same
/// phase as mix-down, making the two stages coherent.
pub struct SsbDemodulator {
    mode: SsbMode,
    resampler: RationalResampler,
    /// Weaver oscillator phase (radians), advanced at input rate
    lo_phase: f32,
    /// Weaver oscillator step per input sample = 2π · fc / sample_rate
    lo_step: f32,
    /// 4th-order input-rate Butterworth LPF for I channel (2 cascaded sections)
    lpf_i1: DspBiquad,
    lpf_i2: DspBiquad,
    /// 4th-order input-rate Butterworth LPF for Q channel
    lpf_q1: DspBiquad,
    lpf_q2: DspBiquad,
    /// Voice bandpass (300–3000 Hz) applied at audio rate
    audio_bp: AudioBandpass,
}

impl SsbDemodulator {
    /// Weaver carrier frequency in Hz (midpoint of the voice passband).
    const WEAVER_FC: f32 = 1_650.0;
    /// Weaver LPF cutoff in Hz — half the voice bandwidth (3000−300)/2 ≈ 1350 Hz.
    const WEAVER_LPF: f32 = 1_350.0;

    /// Create an SSB/DSB demodulator.
    ///
    /// * `mode`        — [`SsbMode::Usb`], [`SsbMode::Lsb`], or [`SsbMode::Dsb`]
    /// * `sample_rate` — IQ input sample rate in Hz
    /// * `audio_rate`  — desired audio output rate in Hz (typically 48 000)
    pub fn new(mode: SsbMode, sample_rate: u32, audio_rate: u32) -> Self {
        let lo_step = 2.0 * std::f32::consts::PI * Self::WEAVER_FC / sample_rate as f32;
        let fs = sample_rate as f32;
        Self {
            mode,
            resampler: RationalResampler::new(sample_rate, audio_rate),
            lo_phase: 0.0,
            lo_step,
            lpf_i1: DspBiquad::lowpass(Self::WEAVER_LPF, fs),
            lpf_i2: DspBiquad::lowpass(Self::WEAVER_LPF, fs),
            lpf_q1: DspBiquad::lowpass(Self::WEAVER_LPF, fs),
            lpf_q2: DspBiquad::lowpass(Self::WEAVER_LPF, fs),
            audio_bp: AudioBandpass::voice(audio_rate as f32),
        }
    }

    /// Convenience constructor for standard SDR audio output (48 kHz).
    pub fn standard(mode: SsbMode, sample_rate: u32) -> Self {
        Self::new(mode, sample_rate, 48_000)
    }

    /// Process a batch of IQ samples and return resampled audio.
    pub fn process(&mut self, samples: &[Complex<f32>]) -> Vec<f32> {
        let mut out = Vec::with_capacity(self.resampler.output_len_hint(samples.len()));

        for &s in samples {
            // Snapshot oscillator phase before advancing (used for both
            // mix-down and mix-up to keep the two stages coherent).
            let lo_cos = self.lo_phase.cos();
            let lo_sin = self.lo_phase.sin();
            self.lo_phase += self.lo_step;
            if self.lo_phase >= std::f32::consts::TAU {
                self.lo_phase -= std::f32::consts::TAU;
            }

            let audio_raw = match self.mode {
                SsbMode::Dsb => {
                    // DSB: take I channel through input-rate LPF (both sidebands contribute)
                    self.lpf_i2.process(self.lpf_i1.process(s.re))
                }
                SsbMode::Usb | SsbMode::Lsb => {
                    // LSB: conjugate input (negate Q) to mirror spectrum into USB path
                    let (i_in, q_in) = if self.mode == SsbMode::Lsb {
                        (s.re, -s.im)
                    } else {
                        (s.re, s.im)
                    };
                    // Weaver mix-down: multiply by exp(-j·lo_phase)
                    let i_bb = i_in * lo_cos + q_in * lo_sin;
                    let q_bb = q_in * lo_cos - i_in * lo_sin;
                    // 4th-order Butterworth LPF (two cascaded 2nd-order sections)
                    let i_filt = self.lpf_i2.process(self.lpf_i1.process(i_bb));
                    let q_filt = self.lpf_q2.process(self.lpf_q1.process(q_bb));
                    // Weaver mix-up: Re(z_filt · exp(+j·lo_phase))
                    i_filt * lo_cos - q_filt * lo_sin
                }
            };

            self.resampler.process(audio_raw, |v| out.push(v));
        }

        // Voice bandpass at audio rate then clamp
        self.audio_bp.process_inplace(&mut out);
        for s in &mut out {
            *s = s.clamp(-1.0, 1.0);
        }

        out
    }

    /// Reset demodulator state (e.g. after a frequency change).
    pub fn reset(&mut self) {
        self.resampler.reset();
        self.lo_phase = 0.0;
        self.lpf_i1.reset();
        self.lpf_i2.reset();
        self.lpf_q1.reset();
        self.lpf_q2.reset();
        self.audio_bp.reset();
    }
}

// ── CW demodulator ────────────────────────────────────────────────────────────

/// CW (Morse code) demodulator with narrow audio bandpass.
///
/// Algorithm:
///   1. Take the real part (I channel) of the baseband IQ.
///   2. Rational resample to `audio_rate` via phase accumulator.
///   3. Apply a narrow CW bandpass (HP @ 400 Hz, LP @ 900 Hz) at audio rate.
///      This centres on the conventional 700 Hz CW sidetone and rejects voice
///      and other off-frequency signals.
pub struct CwDemodulator {
    resampler: RationalResampler,
    /// HP at 400 Hz (audio rate) — removes low-frequency noise and voice
    hp: DspBiquad,
    /// LP at 900 Hz (audio rate) — narrow CW passband centred on 700 Hz sidetone
    lp: DspBiquad,
}

impl CwDemodulator {
    /// Create a CW demodulator.
    ///
    /// * `sample_rate` — IQ input sample rate in Hz
    /// * `audio_rate`  — desired audio output rate in Hz (typically 48 000)
    pub fn new(sample_rate: u32, audio_rate: u32) -> Self {
        let fs = audio_rate as f32;
        Self {
            resampler: RationalResampler::new(sample_rate, audio_rate),
            hp: DspBiquad::highpass(400.0, fs),
            lp: DspBiquad::lowpass(900.0, fs),
        }
    }

    /// Convenience constructor for standard SDR audio output (48 kHz).
    pub fn standard(sample_rate: u32) -> Self {
        Self::new(sample_rate, 48_000)
    }

    /// Process a batch of IQ samples and return resampled audio.
    pub fn process(&mut self, samples: &[Complex<f32>]) -> Vec<f32> {
        let mut out = Vec::with_capacity(self.resampler.output_len_hint(samples.len()));

        for &s in samples {
            // Split borrows so the closure can mutate hp/lp while resampler is borrowed.
            let resampler = &mut self.resampler;
            let hp = &mut self.hp;
            let lp = &mut self.lp;
            resampler.process_with(s.re, |v| {
                // I channel → narrow bandpass (HP 400 Hz, LP 900 Hz)
                let filtered = lp.process(hp.process(v));
                out.push(filtered.clamp(-1.0, 1.0));
            });
        }

        out
    }

    /// Reset demodulator state.
    pub fn reset(&mut self) {
        self.resampler.reset();
        self.hp.reset();
        self.lp.reset();
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
        let samples: Vec<Complex<f32>> = (0..2048).map(|_| Complex::new(1.0, 0.0)).collect();
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
        assert!(
            rms > 0.001,
            "FM demod output should be non-trivial, rms={rms}"
        );
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

    // ── SSB / DSB tests ───────────────────────────────────────────────────────

    fn iq_tone(freq_hz: f32, fs: f32, n: usize) -> Vec<Complex<f32>> {
        (0..n)
            .map(|i| {
                let phase = 2.0 * std::f32::consts::PI * freq_hz / fs * i as f32;
                Complex::new(phase.cos(), phase.sin())
            })
            .collect()
    }

    fn rms_second_half(samples: &[f32]) -> f32 {
        let half = samples.len() / 2;
        let count = samples.len() - half;
        (samples[half..].iter().map(|&v| v * v).sum::<f32>() / count as f32).sqrt()
    }

    /// USB should pass a +1 kHz baseband tone (above-carrier content).
    #[test]
    fn ssb_usb_passes_upper_sideband() {
        let sr = 48_000u32;
        let mut demod = SsbDemodulator::standard(SsbMode::Usb, sr);
        let samples = iq_tone(1_000.0, sr as f32, 24_000);
        let out = demod.process(&samples);
        let rms = rms_second_half(&out);
        assert!(rms > 0.1, "USB +1 kHz should produce audio (rms={rms:.4})");
    }

    /// USB should reject a -1 kHz tone (lower sideband).
    #[test]
    fn ssb_usb_rejects_lower_sideband() {
        let sr = 48_000u32;
        let mut demod = SsbDemodulator::standard(SsbMode::Usb, sr);
        // -1 kHz: conjugate of +1 kHz phasor
        let samples: Vec<Complex<f32>> = iq_tone(1_000.0, sr as f32, 24_000)
            .into_iter()
            .map(|s| Complex::new(s.re, -s.im))
            .collect();
        let out = demod.process(&samples);
        let rms = rms_second_half(&out);
        assert!(
            rms < 0.15,
            "USB should reject -1 kHz LSB tone (rms={rms:.4})"
        );
    }

    /// LSB should pass a -1 kHz baseband tone (below-carrier content).
    #[test]
    fn ssb_lsb_passes_lower_sideband() {
        let sr = 48_000u32;
        let mut demod = SsbDemodulator::standard(SsbMode::Lsb, sr);
        let samples: Vec<Complex<f32>> = iq_tone(1_000.0, sr as f32, 24_000)
            .into_iter()
            .map(|s| Complex::new(s.re, -s.im))
            .collect();
        let out = demod.process(&samples);
        let rms = rms_second_half(&out);
        assert!(rms > 0.1, "LSB -1 kHz should produce audio (rms={rms:.4})");
    }

    /// LSB should reject a +1 kHz tone (upper sideband).
    #[test]
    fn ssb_lsb_rejects_upper_sideband() {
        let sr = 48_000u32;
        let mut demod = SsbDemodulator::standard(SsbMode::Lsb, sr);
        let samples = iq_tone(1_000.0, sr as f32, 24_000);
        let out = demod.process(&samples);
        let rms = rms_second_half(&out);
        assert!(
            rms < 0.15,
            "LSB should reject +1 kHz USB tone (rms={rms:.4})"
        );
    }

    /// DSB should produce audio for a real (both-sideband) cosine input.
    #[test]
    fn ssb_dsb_produces_audio() {
        let sr = 48_000u32;
        let mut demod = SsbDemodulator::standard(SsbMode::Dsb, sr);
        // Real cosine: I = cos(1000t), Q = 0 (equal energy in both sidebands)
        let samples: Vec<Complex<f32>> = (0..24_000)
            .map(|i| {
                let phase = 2.0 * std::f32::consts::PI * 1_000.0 / sr as f32 * i as f32;
                Complex::new(phase.cos(), 0.0)
            })
            .collect();
        let out = demod.process(&samples);
        let rms = rms_second_half(&out);
        assert!(
            rms > 0.1,
            "DSB should produce audio for real 1 kHz input (rms={rms:.4})"
        );
    }

    /// Output sample count follows the rate-conversion contract.
    #[test]
    fn ssb_output_count_matches_rate() {
        let sr = 240_000u32;
        let ar = 48_000u32;
        let mut demod = SsbDemodulator::new(SsbMode::Usb, sr, ar);
        let n = sr as usize; // 1 second of input
        let samples = iq_tone(1_000.0, sr as f32, n);
        let out = demod.process(&samples);
        let expected = ar as usize;
        let diff = (out.len() as i64 - expected as i64).abs();
        assert!(diff <= 2, "expected ~{expected} samples, got {}", out.len());
    }

    // ── CW tests ──────────────────────────────────────────────────────────────

    /// CW bandpass passes the standard 700 Hz sidetone.
    #[test]
    fn cw_passes_700hz_sidetone() {
        let sr = 48_000u32;
        let ar = 48_000u32;
        let mut demod = CwDemodulator::new(sr, ar);
        let n = 24_000;
        let samples: Vec<Complex<f32>> = (0..n)
            .map(|i| {
                let phase = 2.0 * std::f32::consts::PI * 700.0 / sr as f32 * i as f32;
                Complex::new(phase.cos(), 0.0)
            })
            .collect();
        let out = demod.process(&samples);
        let rms = rms_second_half(&out);
        assert!(rms > 0.1, "CW bandpass should pass 700 Hz (rms={rms:.4})");
    }

    /// CW bandpass rejects 2 kHz (voice range, outside CW sidetone window).
    #[test]
    fn cw_rejects_2khz_voice() {
        let sr = 48_000u32;
        let ar = 48_000u32;
        let mut demod = CwDemodulator::new(sr, ar);
        let n = 24_000;
        let samples: Vec<Complex<f32>> = (0..n)
            .map(|i| {
                let phase = 2.0 * std::f32::consts::PI * 2_000.0 / sr as f32 * i as f32;
                Complex::new(phase.cos(), 0.0)
            })
            .collect();
        let out = demod.process(&samples);
        let rms = rms_second_half(&out);
        assert!(
            rms < 0.15,
            "CW bandpass should reject 2 kHz voice (rms={rms:.4})"
        );
    }
}
