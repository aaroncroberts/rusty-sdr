//! AM envelope detector for the 2400 Hz APT subcarrier.
//!
//! Rectifies the audio (|x|) then applies a single-pole IIR low-pass filter
//! with cutoff at 2400 Hz.  At 48 kHz input this gives a smooth AM envelope
//! suitable for sync detection and pixel extraction.

use std::f32::consts::PI;

const CUTOFF_HZ: f32 = 2_400.0;
const SAMPLE_RATE: f32 = 48_000.0;

pub(crate) struct EnvelopeDetector {
    alpha: f32,
    state: f32,
}

impl EnvelopeDetector {
    pub fn new() -> Self {
        let alpha = 1.0 - (-2.0 * PI * CUTOFF_HZ / SAMPLE_RATE).exp();
        Self { alpha, state: 0.0 }
    }

    pub fn reset(&mut self) {
        self.state = 0.0;
    }

    /// Process a batch of audio samples; returns the AM envelope at the same rate.
    pub fn process(&mut self, audio: &[f32]) -> Vec<f32> {
        let mut out = Vec::with_capacity(audio.len());
        for &s in audio {
            self.state += self.alpha * (s.abs() - self.state);
            out.push(self.state);
        }
        out
    }
}
