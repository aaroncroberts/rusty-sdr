#![forbid(unsafe_code)]

//! RMS-based squelch gate for narrow-band FM.
//!
//! The squelch estimates short-term signal power using an exponential moving
//! average (EMA), converts it to dBFS, and gates the audio output to silence
//! when the power is below a configurable threshold.
//!
//! Asymmetric time constants (fast attack, slow release) prevent chattering at
//! the squelch boundary: the gate opens quickly when a signal appears and stays
//! open briefly after it fades, so short signal drops don't produce clicks.

/// RMS-based squelch gate.
///
/// Maintains a smoothed power estimate and gates audio to silence when power
/// falls below the configured threshold. Designed to be applied to mono audio
/// at 48 kHz after FM demodulation.
pub struct Squelch {
    /// EMA power estimate in linear scale (amplitude²).
    avg_power: f32,

    /// EMA coefficient for rising power (attack — fast, ~5 ms at 48 kHz).
    alpha_attack: f32,

    /// EMA coefficient for falling power (release — slow, ~150 ms at 48 kHz).
    alpha_release: f32,

    /// Squelch threshold in linear power scale (= 10^(threshold_dbfs/10)).
    threshold_power: f32,
}

impl Squelch {
    /// Create a new squelch with the given audio rate and initial threshold.
    ///
    /// * `audio_rate`      — audio sample rate in Hz (typically 48_000)
    /// * `threshold_dbfs`  — squelch threshold in dBFS (e.g. -50.0)
    pub fn new(audio_rate: u32, threshold_dbfs: f32) -> Self {
        let fs = audio_rate as f32;

        // Attack: ~5 ms time constant
        let alpha_attack = (-1.0 / (0.005 * fs)).exp();

        // Release: ~150 ms time constant — hold squelch open after signal fades
        let alpha_release = (-1.0 / (0.150 * fs)).exp();

        Self {
            avg_power: 0.0,
            alpha_attack,
            alpha_release,
            threshold_power: dbfs_to_power(threshold_dbfs),
        }
    }

    /// Update the squelch threshold.
    ///
    /// `threshold_dbfs` is in dBFS (negative, e.g. -50.0 for -50 dBFS).
    pub fn set_threshold_dbfs(&mut self, threshold_dbfs: f32) {
        self.threshold_power = dbfs_to_power(threshold_dbfs);
    }

    /// Returns the current estimated signal level in dBFS.
    ///
    /// Useful for displaying a signal-strength indicator alongside the slider.
    pub fn level_dbfs(&self) -> f32 {
        power_to_dbfs(self.avg_power)
    }

    /// Returns `true` if the squelch is currently open (signal above threshold).
    pub fn is_open(&self) -> bool {
        self.avg_power >= self.threshold_power
    }

    /// Process a slice of mono audio samples.
    ///
    /// Returns a `Vec<f32>` of the same length as `samples`:
    /// - If the squelch is open, samples pass through unchanged.
    /// - If the squelch is closed, samples are replaced with 0.0 (silence).
    ///
    /// The power estimate is updated sample-by-sample using an asymmetric EMA,
    /// so the open/close decision can change mid-batch.
    pub fn process(&mut self, samples: &[f32]) -> Vec<f32> {
        let mut out = Vec::with_capacity(samples.len());

        for &s in samples {
            let power = s * s;

            // Asymmetric EMA: use fast coefficient when power is rising,
            // slow coefficient when it is falling.
            let alpha = if power > self.avg_power {
                self.alpha_attack
            } else {
                self.alpha_release
            };
            self.avg_power = alpha * self.avg_power + (1.0 - alpha) * power;

            // Gate: pass audio when open, silence when closed.
            out.push(if self.avg_power >= self.threshold_power {
                s
            } else {
                0.0
            });
        }

        out
    }

    /// Reset internal state (e.g. after a demod mode change).
    pub fn reset(&mut self) {
        self.avg_power = 0.0;
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// dBFS amplitude → linear power: `10^(db/10)`.
/// dBFS is amplitude-referenced (0 dBFS = full scale amplitude),
/// so power = amplitude² = 10^(db/10).
fn dbfs_to_power(dbfs: f32) -> f32 {
    10.0_f32.powf(dbfs / 10.0)
}

/// Linear power → dBFS: `10 * log10(power)`.
fn power_to_dbfs(power: f32) -> f32 {
    if power <= 0.0 {
        return -120.0;
    }
    10.0 * power.log10()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_abs_diff_eq;

    #[test]
    fn silence_closes_squelch() {
        let mut sq = Squelch::new(48_000, -50.0);
        // Feed enough silent samples for the EMA to settle near zero
        let silence: Vec<f32> = vec![0.0; 48_000];
        let out = sq.process(&silence);

        // All output should be zero (squelch closed)
        assert!(out.iter().all(|&s| s == 0.0), "silence should be gated");
        assert!(!sq.is_open(), "squelch should be closed on silence");
    }

    #[test]
    fn strong_signal_opens_squelch() {
        let mut sq = Squelch::new(48_000, -50.0);
        // Feed 1 kHz sine at 0.5 amplitude — about -6 dBFS RMS
        let signal: Vec<f32> = (0..48_000)
            .map(|i| {
                let t = i as f32 / 48_000.0;
                0.5 * (2.0 * std::f32::consts::PI * 1_000.0 * t).sin()
            })
            .collect();
        let out = sq.process(&signal);

        // After the attack settles (skip first 500 samples), output should pass through
        let tail = &out[500..];
        let rms_in = (signal[500..].iter().map(|&v| v * v).sum::<f32>() / tail.len() as f32).sqrt();
        let rms_out = (tail.iter().map(|&v| v * v).sum::<f32>() / tail.len() as f32).sqrt();
        // Output RMS should be close to input RMS (gate open)
        assert_abs_diff_eq!(rms_out, rms_in, epsilon = 0.05);
        assert!(sq.is_open(), "squelch should be open on strong signal");
    }

    #[test]
    fn threshold_above_noise_silences_weak_signal() {
        let mut sq = Squelch::new(48_000, -10.0); // high threshold: -10 dBFS
        let weak: Vec<f32> = (0..48_000)
            .map(|i| {
                let t = i as f32 / 48_000.0;
                0.001 * (2.0 * std::f32::consts::PI * 1_000.0 * t).sin()
            })
            .collect();
        let out = sq.process(&weak);
        // Weak signal (-60 dBFS) should be gated by -10 dBFS threshold
        let tail = &out[1000..];
        assert!(tail.iter().all(|&s| s == 0.0), "weak signal should be gated");
    }

    #[test]
    fn set_threshold_updates_behaviour() {
        let mut sq = Squelch::new(48_000, -10.0); // Start closed (high threshold)
        // Lower the threshold to open the gate
        sq.set_threshold_dbfs(-120.0);
        // Feed moderate signal
        let sig: Vec<f32> = (0..2000)
            .map(|i| {
                let t = i as f32 / 48_000.0;
                0.1 * (2.0 * std::f32::consts::PI * 1_000.0 * t).sin()
            })
            .collect();
        let out = sq.process(&sig);
        // With threshold at -120 dBFS, signal should always pass
        let rms_out = (out.iter().map(|&v| v * v).sum::<f32>() / out.len() as f32).sqrt();
        assert!(rms_out > 0.0, "signal should pass after lowering threshold");
    }

    #[test]
    fn level_dbfs_reflects_signal_strength() {
        let mut sq = Squelch::new(48_000, -50.0);
        // -20 dBFS sine (amplitude = 0.1, power = 0.01)
        let sig: Vec<f32> = (0..48_000)
            .map(|i| {
                let t = i as f32 / 48_000.0;
                0.1 * (2.0 * std::f32::consts::PI * 1_000.0 * t).sin()
            })
            .collect();
        sq.process(&sig);
        let level = sq.level_dbfs();
        // RMS of 0.1-amplitude sine is ~0.0707 → power ~0.005 → ~-23 dBFS
        // (≠ -20 because that's peak, not RMS) — just check it's in the right ballpark
        assert!(level > -40.0 && level < -10.0, "level {level} not in expected range");
    }
}
