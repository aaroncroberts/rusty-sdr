#![forbid(unsafe_code)]

//! Audio bandpass filter for narrowband FM voice audio.
//!
//! Standard voice bandpass: 300 Hz high-pass + 3 kHz low-pass.
//! Implemented as a cascade of two second-order biquad IIR filters
//! (RBJ Audio EQ Cookbook).
//!
//! Both filters use Q = 0.707 (Butterworth response — maximally flat passband).

/// Transposed direct form II biquad section.
struct Biquad {
    // Normalised coefficients (a0 = 1)
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    // Delay-line state
    z1: f32,
    z2: f32,
}

impl Biquad {
    /// Second-order Butterworth high-pass filter.
    ///
    /// Uses the RBJ cookbook HPF with Q = 1/√2.
    fn highpass(f0_hz: f32, fs_hz: f32) -> Self {
        let q = std::f32::consts::FRAC_1_SQRT_2; // 0.7071 = Butterworth
        let w0 = 2.0 * std::f32::consts::PI * f0_hz / fs_hz;
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

    /// Second-order Butterworth low-pass filter.
    fn lowpass(f0_hz: f32, fs_hz: f32) -> Self {
        let q = std::f32::consts::FRAC_1_SQRT_2;
        let w0 = 2.0 * std::f32::consts::PI * f0_hz / fs_hz;
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

    #[inline]
    fn process_sample(&mut self, x: f32) -> f32 {
        // Transposed Direct Form II
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

/// Voice audio bandpass: HP @ 300 Hz then LP @ 3 kHz.
///
/// Suitable for narrowband FM voice (aviation, marine, amateur, PMR446).
/// Removes sub-audible CTCSS tones and high-frequency noise/hiss.
pub struct AudioBandpass {
    hp: Biquad,
    lp: Biquad,
}

impl AudioBandpass {
    /// Create the standard 300 Hz – 3 kHz voice bandpass.
    pub fn voice(fs_hz: f32) -> Self {
        Self {
            hp: Biquad::highpass(300.0, fs_hz),
            lp: Biquad::lowpass(3_000.0, fs_hz),
        }
    }

    /// Process a buffer of audio samples in-place.
    pub fn process_inplace(&mut self, samples: &mut [f32]) {
        for s in samples.iter_mut() {
            *s = self.lp.process_sample(self.hp.process_sample(*s));
        }
    }

    pub fn reset(&mut self) {
        self.hp.reset();
        self.lp.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    fn sine_buf(freq_hz: f32, fs_hz: f32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| (2.0 * PI * freq_hz / fs_hz * i as f32).sin())
            .collect()
    }

    #[test]
    fn voice_passband_passes_1khz() {
        let fs = 48_000.0;
        let mut bp = AudioBandpass::voice(fs);
        let mut buf = sine_buf(1_000.0, fs, 4800);
        bp.process_inplace(&mut buf);
        // After settling, RMS should be close to 1/√2 (passband gain ≈ 0 dB)
        let rms: f32 = buf[2400..].iter().map(|&x| x * x).sum::<f32>() / 2400.0;
        let rms = rms.sqrt();
        assert!(rms > 0.6, "1 kHz should pass through voice bandpass (rms={rms:.3})");
    }

    #[test]
    fn voice_bandpass_attenuates_ctcss_range() {
        let fs = 48_000.0;
        let mut bp = AudioBandpass::voice(fs);
        let mut buf = sine_buf(100.0, fs, 4800); // 100 Hz = CTCSS range
        bp.process_inplace(&mut buf);
        let rms: f32 = buf[2400..].iter().map(|&x| x * x).sum::<f32>() / 2400.0;
        let rms = rms.sqrt();
        assert!(rms < 0.1, "100 Hz should be attenuated (rms={rms:.3})");
    }

    #[test]
    fn voice_bandpass_attenuates_high_freq() {
        let fs = 48_000.0;
        let mut bp = AudioBandpass::voice(fs);
        let mut buf = sine_buf(8_000.0, fs, 4800); // 8 kHz = above voice range
        bp.process_inplace(&mut buf);
        let rms: f32 = buf[2400..].iter().map(|&x| x * x).sum::<f32>() / 2400.0;
        let rms = rms.sqrt();
        assert!(rms < 0.1, "8 kHz should be attenuated (rms={rms:.3})");
    }
}
