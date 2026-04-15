#![forbid(unsafe_code)]

use crate::sample::StereoFrame;

/// Linear volume scaler.
///
/// Pure struct — no threading. Apply to a mutable slice in-place.
#[derive(Debug, Clone)]
pub struct Volume {
    /// Linear gain, 0.0–1.0 (or beyond for amplification).
    factor: f32,
}

impl Volume {
    pub fn new(factor: f32) -> Self {
        Self {
            factor: factor.clamp(0.0, 2.0),
        }
    }

    pub fn set(&mut self, factor: f32) {
        self.factor = factor.clamp(0.0, 2.0);
    }

    pub fn get(&self) -> f32 {
        self.factor
    }

    /// Scale all frames in-place.
    pub fn apply(&self, frames: &mut [StereoFrame]) {
        for f in frames {
            f.left *= self.factor;
            f.right *= self.factor;
        }
    }

    /// Return a new vec of scaled frames (non-mutating version).
    pub fn process(&self, frames: &[StereoFrame]) -> Vec<StereoFrame> {
        frames
            .iter()
            .map(|f| StereoFrame {
                left: f.left * self.factor,
                right: f.right * self.factor,
            })
            .collect()
    }
}

/// Piecewise soft-knee limiter: identity below the knee, exponential approach
/// to ±1.0 above it.
///
/// Below `KNEE` (0.95) the signal passes through unchanged — no colouration
/// at normal listening levels.  Above the knee it applies a smooth exponential
/// that asymptotically approaches ±1.0, completely eliminating hard digital
/// clipping.  The transition is C¹-continuous (no derivative jump at the knee).
///
/// This is a stateless per-sample function; call it after volume scaling.
#[inline]
pub fn soft_limit(x: f32) -> f32 {
    const KNEE: f32 = 0.95;
    const HEADROOM: f32 = 1.0 - KNEE; // 0.05
    let ax = x.abs();
    if ax <= KNEE {
        return x;
    }
    // Exponential approach: KNEE + HEADROOM * (1 − exp(−excess/HEADROOM))
    // → asymptotes to 1.0, never exceeds it.
    let excess = ax - KNEE;
    let limited = KNEE + HEADROOM * (1.0 - (-excess / HEADROOM).exp());
    if x >= 0.0 { limited } else { -limited }
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_abs_diff_eq;

    #[test]
    fn unity_gain_unchanged() {
        let vol = Volume::new(1.0);
        let frames = vec![StereoFrame::new(0.5, -0.3)];
        let out = vol.process(&frames);
        assert_abs_diff_eq!(out[0].left, 0.5, epsilon = 1e-7);
        assert_abs_diff_eq!(out[0].right, -0.3, epsilon = 1e-7);
    }

    #[test]
    fn zero_gain_silences() {
        let vol = Volume::new(0.0);
        let frames = vec![StereoFrame::new(1.0, 1.0)];
        let out = vol.process(&frames);
        assert_abs_diff_eq!(out[0].left, 0.0, epsilon = 1e-7);
        assert_abs_diff_eq!(out[0].right, 0.0, epsilon = 1e-7);
    }

    #[test]
    fn half_gain_halves() {
        let vol = Volume::new(0.5);
        let mut frames = vec![StereoFrame::new(1.0, 0.8)];
        vol.apply(&mut frames);
        assert_abs_diff_eq!(frames[0].left, 0.5, epsilon = 1e-7);
        assert_abs_diff_eq!(frames[0].right, 0.4, epsilon = 1e-7);
    }

    #[test]
    fn gain_clamped_to_range() {
        let vol = Volume::new(5.0);
        assert_abs_diff_eq!(vol.get(), 2.0, epsilon = 1e-7);

        let vol = Volume::new(-1.0);
        assert_abs_diff_eq!(vol.get(), 0.0, epsilon = 1e-7);
    }

    #[test]
    fn soft_limit_bounds_strong_signal() {
        // Output magnitude must never exceed 1.0.
        // (At extreme x the exponential term underflows to 0, so output reaches
        // exactly 1.0 in f32 — that is acceptable; the hard clipping case we
        // want to prevent is output > 1.0.)
        for &x in &[1.0_f32, 1.5, 2.0, 5.0, 10.0, -1.0, -2.0] {
            let out = soft_limit(x);
            assert!(
                out.abs() <= 1.0,
                "soft_limit({x}) = {out} should be ≤ 1.0"
            );
        }
        // Values strictly greater than the knee must compress below the knee level.
        for &x in &[1.1_f32, 1.5, 2.0] {
            let out = soft_limit(x);
            assert!(
                out.abs() < x.abs(),
                "soft_limit({x}) = {out} should compress (out < in)"
            );
        }
    }

    #[test]
    fn soft_limit_near_linear_at_low_amplitude() {
        // At ±0.5 the soft limiter should change gain by less than 0.5 dB
        // (i.e. output is between 0.473 and 0.527 for input 0.5).
        let out = soft_limit(0.5_f32);
        assert!(
            (0.47..=0.53).contains(&out),
            "soft_limit(0.5) = {out} — expected near-linear"
        );
    }

    #[test]
    fn soft_limit_is_odd_function() {
        // soft_limit(-x) == -soft_limit(x) for any x.
        for x in [0.1_f32, 0.5, 1.0, 2.0] {
            assert_abs_diff_eq!(soft_limit(-x), -soft_limit(x), epsilon = 1e-6);
        }
    }
}
