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
        Self { factor: factor.clamp(0.0, 2.0) }
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
        frames.iter().map(|f| StereoFrame {
            left: f.left * self.factor,
            right: f.right * self.factor,
        }).collect()
    }
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
}
