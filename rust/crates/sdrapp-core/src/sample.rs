#![forbid(unsafe_code)]

/// Complex IQ sample — the fundamental type flowing from SDR sources.
pub type IqSample = num_complex::Complex<f32>;

/// Stereo audio frame for output to speakers or recording.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StereoFrame {
    pub left: f32,
    pub right: f32,
}

impl StereoFrame {
    pub fn new(left: f32, right: f32) -> Self {
        Self { left, right }
    }

    pub fn mono(sample: f32) -> Self {
        Self { left: sample, right: sample }
    }

    /// Mix to mono: (L + R) / 2
    pub fn to_mono(self) -> f32 {
        (self.left + self.right) * 0.5
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_abs_diff_eq;

    #[test]
    fn stereo_to_mono_averages() {
        let frame = StereoFrame::new(1.0, 0.0);
        assert_abs_diff_eq!(frame.to_mono(), 0.5, epsilon = 1e-7);
    }

    #[test]
    fn mono_constructor_is_symmetric() {
        let frame = StereoFrame::mono(0.75);
        assert_eq!(frame.left, frame.right);
        assert_abs_diff_eq!(frame.to_mono(), 0.75, epsilon = 1e-7);
    }
}
