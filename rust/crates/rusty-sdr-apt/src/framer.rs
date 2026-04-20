//! APT line sync detector and pixel extractor.
//!
//! Watches the AM envelope for the characteristic Sync A burst (7 cycles of
//! 1040 Hz square wave), then extracts 2080 pixels from the following line.

use std::collections::VecDeque;

use crate::{PIXELS_PER_LINE, SAMPLES_PER_LINE};

/// APT image line rate is 2/sec; at 48 kHz one line = 24000 samples.
const SYNC_WINDOW: usize = 400;
/// Variance threshold above which we consider a region "sync-like".
const SYNC_VAR_THRESH: f32 = 0.008;
/// Minimum samples between successive sync detections (avoid double-trigger).
const MIN_LINE_SAMPLES: usize = 20_000;

/// One decoded APT line: 2080 grayscale pixels (u8, 0=black, 255=white).
#[derive(Debug, Clone)]
pub struct AptLine {
    pub pixels: Vec<u8>,
    /// Line number (0-based within this session).
    pub line_num: usize,
}

pub(crate) struct AptFramer {
    /// Ring buffer of envelope samples used for variance computation.
    win: VecDeque<f32>,
    win_sum: f32,
    win_sum_sq: f32,
    /// All envelope samples collected since the last sync.
    line_buf: Vec<f32>,
    /// Are we currently inside a sync burst?
    in_sync: bool,
    /// Number of samples since the last line was emitted.
    since_last_line: usize,
    /// Number of complete lines emitted.
    line_count: usize,
}

impl AptFramer {
    pub fn new() -> Self {
        Self {
            win: VecDeque::with_capacity(SYNC_WINDOW + 1),
            win_sum: 0.0,
            win_sum_sq: 0.0,
            line_buf: Vec::with_capacity(SAMPLES_PER_LINE + 500),
            in_sync: false,
            since_last_line: MIN_LINE_SAMPLES, // allow first sync immediately
            line_count: 0,
        }
    }

    pub fn reset(&mut self) {
        self.win.clear();
        self.win_sum = 0.0;
        self.win_sum_sq = 0.0;
        self.line_buf.clear();
        self.in_sync = false;
        self.since_last_line = MIN_LINE_SAMPLES;
        self.line_count = 0;
    }

    pub fn line_count(&self) -> usize {
        self.line_count
    }

    /// Push envelope samples; returns any newly completed lines.
    pub fn push_envelope(&mut self, env: &[f32]) -> Vec<AptLine> {
        let mut out = Vec::new();
        for &s in env {
            self.since_last_line += 1;
            self.line_buf.push(s);

            // Update sliding window variance
            self.win.push_back(s);
            self.win_sum += s;
            self.win_sum_sq += s * s;
            if self.win.len() > SYNC_WINDOW {
                let old = self.win.pop_front().unwrap();
                self.win_sum -= old;
                self.win_sum_sq -= old * old;
            }

            if self.win.len() < SYNC_WINDOW {
                continue;
            }

            let n = SYNC_WINDOW as f32;
            let mean = self.win_sum / n;
            let variance = (self.win_sum_sq / n) - (mean * mean);

            let high_var = variance > SYNC_VAR_THRESH;

            if high_var && !self.in_sync && self.since_last_line >= MIN_LINE_SAMPLES {
                // Rising edge of sync burst — start of a new line
                self.in_sync = true;
                // The line_buf contains samples from the previous sync start.
                // Trim to exactly SAMPLES_PER_LINE worth before emitting.
                if self.line_buf.len() >= SAMPLES_PER_LINE {
                    let line_samples: Vec<f32> = self.line_buf
                        [self.line_buf.len() - SAMPLES_PER_LINE..]
                        .to_vec();
                    if let Some(line) = self.extract_line(&line_samples) {
                        out.push(line);
                    }
                }
                // Reset for next line
                self.line_buf.clear();
                self.since_last_line = 0;
            } else if !high_var && self.in_sync {
                // Falling edge of sync burst
                self.in_sync = false;
            }
        }
        out
    }

    fn extract_line(&mut self, samples: &[f32]) -> Option<AptLine> {
        if samples.len() < SAMPLES_PER_LINE {
            return None;
        }
        // Compute per-line min/max for normalization
        let min = samples.iter().cloned().fold(f32::MAX, f32::min);
        let max = samples.iter().cloned().fold(f32::MIN, f32::max);
        let range = (max - min).max(1e-6);

        let mut pixels = Vec::with_capacity(PIXELS_PER_LINE);
        let scale = (SAMPLES_PER_LINE as f32) / (PIXELS_PER_LINE as f32);
        for px in 0..PIXELS_PER_LINE {
            let sample_f = px as f32 * scale;
            let i = sample_f as usize;
            let frac = sample_f - i as f32;
            let a = samples[i.min(samples.len() - 1)];
            let b = samples[(i + 1).min(samples.len() - 1)];
            let v = a + frac * (b - a);
            pixels.push(((v - min) / range * 255.0) as u8);
        }

        let line_num = self.line_count;
        self.line_count += 1;
        Some(AptLine { pixels, line_num })
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn framer_starts_with_zero_lines() {
        let f = AptFramer::new();
        assert_eq!(f.line_count(), 0);
    }

    #[test]
    fn framer_reset_clears_state() {
        let mut f = AptFramer::new();
        f.line_count = 5;
        f.reset();
        assert_eq!(f.line_count(), 0);
        assert!(f.line_buf.is_empty());
    }

    #[test]
    fn empty_push_returns_no_lines() {
        let mut f = AptFramer::new();
        let lines = f.push_envelope(&[]);
        assert!(lines.is_empty());
    }
}
