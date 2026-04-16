#![forbid(unsafe_code)]

//! Kaiser-windowed FIR lowpass filter for complex IQ data.
//!
//! Used for anti-aliasing before integer IQ decimation in the WBFM path.
//! Applies the same real-valued coefficient set to both I and Q channels.
//!
//! # Design
//! Coefficients are computed as a windowed-sinc: `h[n] = sinc(2fc·(n-M/2)) · w[n]`
//! where `w[n]` is the Kaiser window.  Kaiser beta=6.0 gives ~60 dB stopband
//! rejection, sufficient to suppress adjacent FM stations 200 kHz away from
//! folding into a 200 kHz WBFM passband after 4× decimation.

use std::f64::consts::PI;

use rustfft::num_complex::Complex;

/// Stateful FIR lowpass filter operating on complex (IQ) input.
pub struct FirLowpass {
    /// Filter coefficients (Kaiser-windowed sinc), length M.
    coeffs: Vec<f32>,
    /// State buffer: last `M-1` input samples, stored newest-first.
    /// `state[0]` = x[-1], `state[1]` = x[-2], ..., `state[M-2]` = x[-(M-1)].
    state: Vec<Complex<f32>>,
}

impl FirLowpass {
    /// Construct a Kaiser-windowed FIR lowpass filter.
    ///
    /// - `cutoff_hz`: –6 dB cutoff in Hz
    /// - `sample_rate_hz`: input sample rate in Hz
    /// - `num_taps`: filter length (prefer odd for linear phase)
    /// - `beta`: Kaiser window shape (6.0 → ~60 dB stopband rejection)
    pub fn new(cutoff_hz: f32, sample_rate_hz: f32, num_taps: usize, beta: f32) -> Self {
        let coeffs = kaiser_sinc_coeffs(
            cutoff_hz as f64,
            sample_rate_hz as f64,
            num_taps,
            beta as f64,
        );
        let state_len = num_taps.saturating_sub(1);
        Self {
            coeffs,
            state: vec![Complex::new(0.0_f32, 0.0); state_len],
        }
    }

    /// Reset internal state to zero (use on source reconnect or demod reset).
    pub fn reset(&mut self) {
        self.state.iter_mut().for_each(|s| *s = Complex::new(0.0, 0.0));
    }

    /// Filter `input` into `output` (same length), maintaining state across calls.
    pub fn process(&mut self, input: &[Complex<f32>], output: &mut Vec<Complex<f32>>) {
        let n = input.len();
        let m = self.coeffs.len();
        let state_len = m.saturating_sub(1);

        output.clear();
        output.reserve(n);

        // Direct-form FIR: y[i] = sum_{j=0}^{M-1}  h[j] * x[i-j]
        // For j <= i: x[i-j] is in `input`.
        // For j >  i: x[i-j] = state[j-i-1]  (newest-first ordering).
        for i in 0..n {
            let mut acc_re = 0.0_f32;
            let mut acc_im = 0.0_f32;
            for (j, &h) in self.coeffs.iter().enumerate() {
                let s = if j <= i {
                    input[i - j]
                } else {
                    // j > i → access state[j-i-1]
                    self.state[j - i - 1]
                };
                acc_re += h * s.re;
                acc_im += h * s.im;
            }
            output.push(Complex::new(acc_re, acc_im));
        }

        // Update state: newest-first, last (M-1) input samples.
        if n >= state_len {
            for k in 0..state_len {
                self.state[k] = input[n - 1 - k];
            }
        } else {
            // Batch shorter than state — shift old state right, prepend new samples.
            self.state.copy_within(0..state_len - n, n);
            for k in 0..n {
                self.state[k] = input[n - 1 - k];
            }
        }
    }
}

// ── Coefficient generation ────────────────────────────────────────────────────

/// Compute Kaiser-windowed sinc FIR lowpass coefficients.
/// Coefficients are normalized to unit DC gain.
fn kaiser_sinc_coeffs(
    cutoff_hz: f64,
    sample_rate_hz: f64,
    num_taps: usize,
    beta: f64,
) -> Vec<f32> {
    let m = num_taps - 1; // filter order
    let fc = cutoff_hz / sample_rate_hz; // normalized cutoff in [0, 0.5]
    let i0_beta = bessel_i0(beta);

    let mut coeffs: Vec<f64> = (0..num_taps)
        .map(|n| {
            let centered = n as f64 - m as f64 / 2.0;
            let sinc = if centered == 0.0 {
                2.0 * fc
            } else {
                (2.0 * PI * fc * centered).sin() / (PI * centered)
            };
            let r = 2.0 * n as f64 / m as f64 - 1.0;
            let window = bessel_i0(beta * (1.0 - r * r).max(0.0).sqrt()) / i0_beta;
            sinc * window
        })
        .collect();

    // Normalize so DC gain = 1.0
    let sum: f64 = coeffs.iter().sum();
    if sum.abs() > 1e-12 {
        for c in &mut coeffs {
            *c /= sum;
        }
    }

    coeffs.iter().map(|&c| c as f32).collect()
}

/// Zeroth-order modified Bessel function of the first kind, I₀(x).
/// Uses the polynomial approximation from Abramowitz & Stegun §9.8.
fn bessel_i0(x: f64) -> f64 {
    let ax = x.abs();
    if ax < 3.75 {
        let y = (x / 3.75) * (x / 3.75);
        1.0 + y * (3.515_622_9
            + y * (3.089_942_4
                + y * (1.206_749_2
                    + y * (0.265_973_2 + y * (0.036_076_8 + y * 0.004_581_3)))))
    } else {
        let y = 3.75 / ax;
        (0.398_942_28
            + y * (0.013_285_92
                + y * (0.002_253_19
                    + y * (-0.001_575_65
                        + y * (0.009_162_81
                            + y * (-0.020_577_06
                                + y * (0.026_355_37
                                    + y * (-0.016_476_33 + y * 0.003_923_77))))))))
            * ax.exp()
            / ax.sqrt()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI as PI32;

    fn tone(freq_hz: f32, sample_rate: f32, n: usize) -> Vec<Complex<f32>> {
        (0..n)
            .map(|i| {
                let theta = 2.0 * PI32 * freq_hz / sample_rate * i as f32;
                Complex::new(theta.cos(), theta.sin())
            })
            .collect()
    }

    fn rms_power(samples: &[Complex<f32>]) -> f32 {
        let sum: f32 = samples.iter().map(|s| s.re * s.re + s.im * s.im).sum();
        (sum / samples.len() as f32).sqrt()
    }

    #[test]
    fn passband_tone_passes() {
        // At 2 MSps with 220 kHz cutoff, a 19 kHz pilot (FM stereo) must pass.
        let sr = 2_000_000.0;
        let mut fir = FirLowpass::new(220_000.0, sr, 127, 6.0);
        let input = tone(19_000.0, sr, 16384);
        let mut output = Vec::new();
        fir.process(&input, &mut output);
        // Skip transient (first num_taps samples), measure steady state.
        let power = rms_power(&output[127..]);
        assert!(
            power > 0.9,
            "Passband tone power too low: {power:.3} (expected >0.9)"
        );
    }

    #[test]
    fn stopband_tone_rejected() {
        // Sample rate 2 MSps, cutoff 220 kHz, tone at 400 kHz.
        // Nyquist = 1 MHz, so 400 kHz is well below Nyquist and above the cutoff.
        // Without the filter, 400 kHz aliases to 100 kHz after 4x decimation to
        // 500 kHz — this is the exact aliasing scenario the filter prevents.
        let sr = 2_000_000.0;
        let mut fir = FirLowpass::new(220_000.0, sr, 127, 6.0);
        let input = tone(400_000.0, sr, 16384);
        let mut output = Vec::new();
        fir.process(&input, &mut output);
        // Skip filter transient, then measure steady-state power.
        let power = rms_power(&output[127..]);
        assert!(
            power < 0.05,
            "Stopband tone not rejected: {power:.4} (expected <0.05)"
        );
    }

    #[test]
    fn state_continuity_across_batches() {
        // Splitting a batch in two must produce the same result as one batch.
        let sr = 2_000_000.0;
        let input = tone(10_000.0, sr, 1024);

        let mut fir1 = FirLowpass::new(220_000.0, sr, 63, 6.0);
        let mut out1 = Vec::new();
        fir1.process(&input, &mut out1);

        let mut fir2 = FirLowpass::new(220_000.0, sr, 63, 6.0);
        let mut out2a = Vec::new();
        let mut out2b = Vec::new();
        fir2.process(&input[..512], &mut out2a);
        fir2.process(&input[512..], &mut out2b);
        let out2: Vec<_> = out2a.iter().chain(out2b.iter()).cloned().collect();

        for (a, b) in out1.iter().zip(out2.iter()) {
            let err = ((a.re - b.re).powi(2) + (a.im - b.im).powi(2)).sqrt();
            assert!(err < 1e-5, "State discontinuity between batches: err={err:.2e}");
        }
    }
}
