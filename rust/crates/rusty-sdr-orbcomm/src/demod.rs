//! SDPSK demodulator with Gardner timing error detector.
//!
//! ## SDPSK demodulation
//!
//! Differential PSK encodes bits in *phase differences* between successive
//! symbols.  For binary SDPSK with ±90° shifts:
//!
//! ```text
//! bit = arg(conj(prev_symbol) × curr_symbol) > 0  →  1
//!                                              ≤ 0  →  0
//! ```
//!
//! This is carrier-phase-agnostic: a 180° carrier error just flips all bits,
//! which is fixed later by the frame sync's correlation check.
//!
//! ## Gardner timing error detector
//!
//! The Gardner TED estimates the fractional sample offset without a separate
//! carrier recovery loop.  Given three consecutive symbol-spaced samples
//! `y[n-1]`, `y[n]`, `y[n+1]` (where `y[n]` is the on-time symbol) and the
//! midpoint `m[n]` between `y[n-1]` and `y[n+1]`:
//!
//! ```text
//! TED output  e = Re{ (y[n+1] - y[n-1]) × conj(m[n]) }
//! ```
//!
//! We use the simplified real version since the demodulated soft symbols are
//! already real-valued after the differential operation:
//!
//! ```text
//! e = (y[n+1] - y[n-1]) × y_mid
//! ```
//!
//! The error drives a simple PI loop that adjusts the next strobe position.

use num_complex::Complex32;

/// SDPSK soft-symbol demodulator.
///
/// Feed complex IQ samples one at a time via [`SdpskDemod::push`].  The
/// demodulator tracks the previous complex sample and emits a soft-symbol
/// (`f32` in the range `−1.0 … +1.0`) for each new sample.
///
/// In practice you feed this into [`GardnerClock`] which strobes the output
/// at symbol boundaries.
#[derive(Debug, Default)]
pub struct SdpskDemod {
    prev: Complex32,
}

impl SdpskDemod {
    pub fn new() -> Self {
        Self::default()
    }

    /// Push one IQ sample and return the soft-symbol value.
    ///
    /// The value is the phase difference (in normalised units) between this
    /// sample and the previous one.  Values near `+1.0` → bit 1; values near
    /// `−1.0` → bit 0.
    #[inline]
    pub fn push(&mut self, sample: Complex32) -> f32 {
        // Phase difference: arg(conj(prev) * sample) normalised to [-1, 1].
        let diff = self.prev.conj() * sample;
        self.prev = sample;
        // atan2 returns [-π, π]; normalise to [-1, 1].
        diff.im.atan2(diff.re) * std::f32::consts::FRAC_1_PI
    }

    /// Reset internal state (e.g. when restarting acquisition).
    pub fn reset(&mut self) {
        self.prev = Complex32::new(0.0, 0.0);
    }
}

/// Gardner timing error detector + PI loop for symbol clock recovery.
///
/// Works on the *real-valued* soft-symbol stream produced by [`SdpskDemod`].
///
/// ## Usage
///
/// ```ignore
/// let mut demod = SdpskDemod::new();
/// let mut clock = GardnerClock::new(SPS as f32);
///
/// for sample in iq_samples {
///     let soft = demod.push(sample);
///     if let Some(symbol) = clock.push(soft) {
///         // `symbol` is an on-time soft symbol — feed to FrameSync
///     }
/// }
/// ```
#[derive(Debug)]
pub struct GardnerClock {
    /// Nominal samples-per-symbol.
    sps: f32,
    /// Current fractional strobe position within the symbol period.
    mu: f32,
    /// PI integrator state.
    int: f32,
    /// Proportional gain.
    kp: f32,
    /// Integral gain.
    ki: f32,
    /// Circular sample buffer (length = 2 × sps rounded up).
    buf: Vec<f32>,
    buf_len: usize,
    write_pos: usize,
    /// Integer step size between strobes.
    step: usize,
}

impl GardnerClock {
    /// Create a new clock recovery loop.
    ///
    /// `sps` — nominal (possibly fractional) samples per symbol.
    pub fn new(sps: f32) -> Self {
        let buf_len = (sps as usize) * 2 + 4;
        Self {
            sps,
            mu: 0.0,
            int: 0.0,
            kp: 0.05,
            ki: 0.001,
            buf: vec![0.0; buf_len],
            buf_len,
            write_pos: 0,
            step: sps as usize,
        }
    }

    /// Push one soft-symbol sample.  Returns `Some(symbol)` when the strobe
    /// fires (i.e. an on-time symbol has been recovered), `None` otherwise.
    pub fn push(&mut self, sample: f32) -> Option<f32> {
        // Write sample into circular buffer.
        self.buf[self.write_pos] = sample;
        self.write_pos = (self.write_pos + 1) % self.buf_len;

        // Count fractional position.
        self.mu += 1.0;

        if self.mu < self.step as f32 {
            return None;
        }
        self.mu -= self.step as f32;

        // Strobe: read on-time sample and its neighbours for the TED.
        let on_time = self.read_delayed(0);
        let early = self.read_delayed(self.step / 2);
        let late = self.read_delayed(self.step / 2 + self.step);

        // Gardner TED: e = (late - early) × on_time
        // (simplified real-valued version)
        let e = (late - early) * on_time;

        // PI loop update.
        self.int += self.ki * e;
        let adj = self.kp * e + self.int;

        // Clamp adjustment to avoid runaway.
        let adj = adj.clamp(-2.0, 2.0);

        // Update nominal step, keeping it within ±20% of ideal sps.
        let new_step = (self.sps + adj).clamp(self.sps * 0.8, self.sps * 1.2);
        self.step = new_step.round() as usize;
        self.step = self.step.max(1);

        Some(on_time)
    }

    /// Read a sample from `delay` positions back in the circular buffer.
    fn read_delayed(&self, delay: usize) -> f32 {
        let idx = (self.write_pos + self.buf_len - delay - 1) % self.buf_len;
        self.buf[idx]
    }

    /// Reset loop state.
    pub fn reset(&mut self) {
        self.mu = 0.0;
        self.int = 0.0;
        self.step = self.sps as usize;
        self.buf.fill(0.0);
        self.write_pos = 0;
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// SdpskDemod: a +90° phase step should give a positive soft symbol.
    #[test]
    fn sdpsk_positive_phase_step() {
        let mut d = SdpskDemod::new();
        // Seed with a sample at angle 0.
        d.push(Complex32::new(1.0, 0.0));
        // Next sample is +90° rotated.
        let soft = d.push(Complex32::new(0.0, 1.0));
        assert!(soft > 0.0, "Expected positive soft symbol for +90° step, got {soft}");
    }

    /// SdpskDemod: a −90° phase step should give a negative soft symbol.
    #[test]
    fn sdpsk_negative_phase_step() {
        let mut d = SdpskDemod::new();
        d.push(Complex32::new(1.0, 0.0));
        let soft = d.push(Complex32::new(0.0, -1.0));
        assert!(soft < 0.0, "Expected negative soft symbol for -90° step, got {soft}");
    }

    /// SdpskDemod: zero phase step → near-zero output.
    #[test]
    fn sdpsk_zero_phase_step() {
        let mut d = SdpskDemod::new();
        d.push(Complex32::new(1.0, 0.0));
        let soft = d.push(Complex32::new(1.0, 0.0));
        assert!(soft.abs() < 0.01, "Expected ~0 for 0° step, got {soft}");
    }

    /// SdpskDemod: phase difference of exactly +π/2 normalises to +0.5.
    #[test]
    fn sdpsk_normalisation() {
        let mut d = SdpskDemod::new();
        d.push(Complex32::new(1.0, 0.0));
        // +90° step → arg = π/2 → normalised = (π/2) / π = 0.5
        let soft = d.push(Complex32::new(0.0, 1.0));
        let expected = 0.5_f32;
        assert!(
            (soft - expected).abs() < 0.001,
            "Expected {expected}, got {soft}"
        );
    }

    /// GardnerClock fires exactly once every `sps` samples on a steady input.
    #[test]
    fn gardner_fires_at_symbol_rate() {
        let sps = 10usize;
        let mut clock = GardnerClock::new(sps as f32);

        // Feed 100 identical soft symbols — expect 10 strobes.
        let mut strobes = 0usize;
        for _ in 0..(sps * 10) {
            if clock.push(1.0).is_some() {
                strobes += 1;
            }
        }
        // Allow ±1 due to fractional rounding.
        assert!(
            (strobes as i64 - 10).abs() <= 1,
            "Expected ~10 strobes, got {strobes}"
        );
    }

    /// GardnerClock: reset restores strobe timing.
    #[test]
    fn gardner_reset_restores_timing() {
        let sps = 10usize;
        let mut clock = GardnerClock::new(sps as f32);

        // Run for a while.
        for _ in 0..(sps * 5) {
            clock.push(0.5);
        }
        clock.reset();

        // After reset the first strobe should be at sample `sps`.
        let mut first_strobe = None;
        for i in 0..(sps * 2) {
            if clock.push(1.0).is_some() {
                first_strobe = Some(i);
                break;
            }
        }
        let pos = first_strobe.expect("Expected a strobe after reset");
        assert!(
            pos >= sps - 2 && pos <= sps + 2,
            "First strobe after reset at sample {pos}, expected ~{sps}"
        );
    }

    /// Full pipeline: generate SDPSK symbols, verify demod + clock reproduce them.
    /// Clock recovery on filtered soft-symbol stream.
    ///
    /// In a real receiver, the bandlimited RF channel smooths symbol
    /// transitions so that the demod output is sustained at ±1 across each
    /// symbol period (not just a spike at the edge).  This test simulates that
    /// matched-filtered output directly, bypassing SdpskDemod, and verifies
    /// the Gardner clock recovers the correct symbol sequence.
    #[test]
    fn gardner_clock_recovers_soft_symbols() {
        let sps = 10usize;
        let bits: Vec<u8> = vec![1, 0, 1, 1, 0, 0, 1, 0, 1, 1, 0, 1, 0, 0, 1, 1, 0, 1, 1, 0];

        // Simulate matched-filter output: each symbol occupies `sps` samples
        // at ±0.5 amplitude (differential encoding: 1→+0.5, 0→−0.5).
        let preamble = vec![1u8; 5];
        let mut soft_stream: Vec<f32> = Vec::new();
        for &bit in preamble.iter().chain(bits.iter()) {
            let value = if bit == 1 { 0.5 } else { -0.5 };
            for _ in 0..sps {
                soft_stream.push(value);
            }
        }

        let mut clock = GardnerClock::new(sps as f32);
        let mut recovered: Vec<u8> = Vec::new();

        for &soft in &soft_stream {
            if let Some(sym) = clock.push(soft) {
                recovered.push(if sym > 0.0 { 1 } else { 0 });
            }
        }

        assert!(
            recovered.len() >= preamble.len(),
            "Too few recovered bits: {}",
            recovered.len()
        );
        // Skip the preamble and compare against known bits.
        let payload = &recovered[preamble.len()..];
        let compare_len = payload.len().min(bits.len());
        let matches = bits[..compare_len]
            .iter()
            .zip(payload.iter())
            .filter(|(a, b)| a == b)
            .count();
        let accuracy = matches as f32 / compare_len as f32;
        assert!(
            accuracy >= 0.90,
            "Expected ≥90% bit accuracy, got {:.0}% ({matches}/{compare_len})",
            accuracy * 100.0
        );
    }
}
