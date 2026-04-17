#![forbid(unsafe_code)]

//! FFT processor for spectrum/waterfall display.
//!
//! Takes a batch of IQ samples, applies a configurable window function,
//! runs FFT via `rustfft`, and returns log-magnitude bins.

use serde::{Deserialize, Serialize};

use crate::sample::IqSample;
use rustfft::{num_complex::Complex, FftPlanner};

/// Window function applied before the FFT.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum FftWindow {
    /// Rectangular (no windowing). Best time resolution, worst leakage.
    Rectangular,
    /// Hann — good general-purpose window. Low leakage, moderate main-lobe width.
    #[default]
    Hann,
    /// Hamming — slightly less sidelobe attenuation than Hann, marginally narrower main lobe.
    Hamming,
    /// Blackman-Harris — very low sidelobes (~92 dB). Best for adjacent-channel rejection.
    BlackmanHarris,
}

impl FftWindow {
    /// Human-readable label for UI display.
    pub fn label(self) -> &'static str {
        match self {
            FftWindow::Rectangular => "Rect",
            FftWindow::Hann => "Hann",
            FftWindow::Hamming => "Hamming",
            FftWindow::BlackmanHarris => "Blkm-Har",
        }
    }
}

/// Computes a windowed FFT from IQ samples and returns magnitude in dBFS.
///
/// Output length == fft_size. Bins are ordered DC-first (0..fft_size).
/// For display, bins are usually reordered to center-DC: swap halves.
pub struct FftProcessor {
    fft_size: usize,
    window: Vec<f32>,
    planner: FftPlanner<f32>,
}

impl FftProcessor {
    pub fn new(fft_size: usize, window_fn: FftWindow) -> Self {
        assert!(
            fft_size.is_power_of_two(),
            "fft_size must be a power of two"
        );
        let window = make_window(fft_size, window_fn);
        Self {
            fft_size,
            window,
            planner: FftPlanner::new(),
        }
    }

    /// Process one block of IQ samples.
    ///
    /// Returns `fft_size` magnitude values in dBFS (negative = below full scale).
    /// Returns `None` if `samples.len() < fft_size`.
    pub fn process(&mut self, samples: &[IqSample]) -> Option<Vec<f32>> {
        if samples.len() < self.fft_size {
            return None;
        }

        let fft = self.planner.plan_fft_forward(self.fft_size);
        let mut buf: Vec<Complex<f32>> = samples[..self.fft_size]
            .iter()
            .zip(self.window.iter())
            .map(|(s, w)| Complex::new(s.re * w, s.im * w))
            .collect();

        fft.process(&mut buf);

        // Convert to dBFS, center-dc ordered
        let mags = fftshift_dbfs(&buf);
        Some(mags)
    }

    pub fn fft_size(&self) -> usize {
        self.fft_size
    }
}

/// Generate window coefficients for the given function and size.
pub fn make_window(n: usize, window_fn: FftWindow) -> Vec<f32> {
    match window_fn {
        FftWindow::Rectangular => vec![1.0; n],
        FftWindow::Hann => hann_window(n),
        FftWindow::Hamming => hamming_window(n),
        FftWindow::BlackmanHarris => blackman_harris_window(n),
    }
}

fn hann_window(n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| 0.5 * (1.0 - (2.0 * std::f32::consts::PI * i as f32 / (n as f32 - 1.0)).cos()))
        .collect()
}

fn hamming_window(n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| 0.54 - 0.46 * (2.0 * std::f32::consts::PI * i as f32 / (n as f32 - 1.0)).cos())
        .collect()
}

fn blackman_harris_window(n: usize) -> Vec<f32> {
    let a0 = 0.35875_f32;
    let a1 = 0.48829_f32;
    let a2 = 0.14128_f32;
    let a3 = 0.01168_f32;
    (0..n)
        .map(|i| {
            let t = 2.0 * std::f32::consts::PI * i as f32 / (n as f32 - 1.0);
            a0 - a1 * t.cos() + a2 * (2.0 * t).cos() - a3 * (3.0 * t).cos()
        })
        .collect()
}

/// Reorder FFT output to center-DC and convert power to dBFS.
///
/// Normalises by N² so that 0 dBFS corresponds to a full-scale complex
/// phasor (|I+jQ| = 1) regardless of FFT size.  Without this, doubling the
/// FFT size would shift displayed levels by +6 dB.
///
/// The normalisation factor is `1 / N²` because:
///   - A full-scale DC phasor gives |X[k]| = N (with rectangular window)
///   - We want |X[k]| / N = 1 → 0 dBFS, so divide power by N²
fn fftshift_dbfs(buf: &[Complex<f32>]) -> Vec<f32> {
    let n = buf.len();
    let half = n / 2;
    // Normalise by N² so dBFS is consistent across FFT sizes.
    let norm = 1.0 / (n as f32 * n as f32);
    // Concatenate second half (negative freqs) + first half (positive freqs)
    buf[half..]
        .iter()
        .chain(buf[..half].iter())
        .map(|c| {
            let power = (c.re * c.re + c.im * c.im) * norm;
            if power > 0.0 {
                10.0 * power.log10()
            } else {
                -120.0
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_abs_diff_eq;

    #[test]
    fn dc_tone_peaks_at_center_bin() {
        let fft_size = 1024;
        let mut proc = FftProcessor::new(fft_size, FftWindow::Hann);
        // Pure DC: real=1, imag=0
        let samples: Vec<IqSample> = (0..fft_size).map(|_| IqSample::new(1.0, 0.0)).collect();
        let mags = proc.process(&samples).unwrap();
        assert_eq!(mags.len(), fft_size);
        // DC bin is at index fft_size/2 after fftshift
        let dc_bin = fft_size / 2;
        let max_bin = mags
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .map(|(i, _)| i)
            .unwrap();
        assert_eq!(max_bin, dc_bin);
    }

    #[test]
    fn returns_none_for_insufficient_samples() {
        let mut proc = FftProcessor::new(1024, FftWindow::Hann);
        let samples: Vec<IqSample> = (0..512).map(|_| IqSample::new(0.0, 0.0)).collect();
        assert!(proc.process(&samples).is_none());
    }

    #[test]
    fn silence_returns_low_dbfs() {
        let fft_size = 256;
        let mut proc = FftProcessor::new(fft_size, FftWindow::Hann);
        let samples: Vec<IqSample> = (0..fft_size).map(|_| IqSample::new(0.0, 0.0)).collect();
        let mags = proc.process(&samples).unwrap();
        assert!(mags.iter().all(|&m| m <= -100.0));
    }

    #[test]
    fn hann_window_has_correct_endpoints() {
        let w = hann_window(8);
        assert_abs_diff_eq!(w[0], 0.0, epsilon = 1e-5);
        assert_abs_diff_eq!(w[7], 0.0, epsilon = 1e-5);
        // Peak near center
        assert!(w[3] > 0.9 || w[4] > 0.9);
    }

    #[test]
    fn rectangular_window_is_all_ones() {
        let w = make_window(16, FftWindow::Rectangular);
        assert!(w.iter().all(|&v| (v - 1.0).abs() < 1e-6));
    }

    #[test]
    fn blackman_harris_near_zero_at_endpoints() {
        let w = blackman_harris_window(64);
        assert!(w[0].abs() < 0.01);
        assert!(w[63].abs() < 0.01);
    }

    #[test]
    fn all_windows_produce_correct_length() {
        for &wf in &[
            FftWindow::Rectangular,
            FftWindow::Hann,
            FftWindow::Hamming,
            FftWindow::BlackmanHarris,
        ] {
            let w = make_window(512, wf);
            assert_eq!(w.len(), 512);
        }
    }

    /// The dBFS of a signal's peak bin must not shift when the FFT size changes.
    ///
    /// Before the N² normalization fix, doubling the FFT size shifted the
    /// displayed level by +6 dB (for N) or +12 dB (for 2N), which caused
    /// calibration drift whenever the user changed the FFT size setting.
    #[test]
    fn dbfs_consistent_across_fft_sizes() {
        // Build a rectangular-windowed full-scale DC phasor (I=1, Q=0) for each
        // FFT size, then find the peak bin level.  The difference must be < 1 dB.
        let make_peak_db = |size: usize| {
            let mut proc = FftProcessor::new(size, FftWindow::Rectangular);
            let samples: Vec<IqSample> = (0..size).map(|_| IqSample::new(1.0, 0.0)).collect();
            let mags = proc.process(&samples).unwrap();
            mags.iter().cloned().fold(f32::NEG_INFINITY, f32::max)
        };

        let db_1024 = make_peak_db(1024);
        let db_2048 = make_peak_db(2048);
        let db_4096 = make_peak_db(4096);

        assert!(
            (db_1024 - db_2048).abs() < 1.0,
            "1024 vs 2048 peak level differs: {db_1024:.1} vs {db_2048:.1} dBFS"
        );
        assert!(
            (db_1024 - db_4096).abs() < 1.0,
            "1024 vs 4096 peak level differs: {db_1024:.1} vs {db_4096:.1} dBFS"
        );
        // Rectangular window, full-scale DC → 0 dBFS (allow ±1 dB for float)
        assert!(
            db_1024 > -1.0,
            "rectangular full-scale DC should be near 0 dBFS, got {db_1024:.1}"
        );
    }
}
