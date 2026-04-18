//! Scanner state container for the signal-path loop.
//!
//! Holds the mutable state for both bookmark-scan mode and range-sweep mode.
//! The actual scan tick logic lives in `mod.rs` because it needs to reach
//! into demod, frequency atomic, and shared state simultaneously.

use super::shared_state::{Bookmark, DemodMode};

/// Mutable scanner state carried inside the signal-path thread.
///
/// Pack all thirteen previously-scattered `scan_*` locals into one struct
/// so they are easy to spot, zero/reset as a unit, and navigate in an IDE.
pub(super) struct ScanThreadState {
    // ── Common ────────────────────────────────────────────────────────────
    /// True while either bookmark or range scan is active.
    pub running: bool,
    /// Accumulates IQ sample count for the dwell timer.
    /// Compare to `(dwell_secs * sample_rate) as u64` to trigger an advance.
    pub dwell_samples: u64,
    /// Dwell duration in seconds before advancing to the next channel.
    pub dwell_secs: f32,

    // ── Bookmark scan mode ─────────────────────────────────────────────────
    /// Current bookmark index (wraps around).
    pub cursor: usize,
    /// Category filter ("" = all categories).
    pub category: String,

    // ── Range sweep mode ──────────────────────────────────────────────────
    /// True when range sweep (not bookmark) mode is active.
    pub range_mode: bool,
    /// Current sweep frequency (Hz).
    pub range_freq: u64,
    /// Sweep lower bound (Hz).
    pub range_lo: u64,
    /// Sweep upper bound (Hz).
    pub range_hi: u64,
    /// Step size per dwell (Hz).
    pub range_step: u64,
    /// Squelch threshold for locking: stop when `signal_level ≥ range_squelch`.
    pub range_squelch: f32,
    /// When `true`, only lock on channels where WBFM stereo pilot is detected.
    pub range_stereo_only: bool,

    // ── Restore state ──────────────────────────────────────────────────────
    /// Demod mode to restore when the range scan stops (lock or manual stop).
    pub pre_mode: Option<DemodMode>,
}

impl Default for ScanThreadState {
    fn default() -> Self {
        Self {
            running: false,
            dwell_samples: 0,
            dwell_secs: 2.0,
            cursor: 0,
            category: String::new(),
            range_mode: false,
            range_freq: 87_500_000,
            range_lo: 87_500_000,
            range_hi: 108_000_000,
            range_step: 100_000,
            range_squelch: -60.0,
            range_stereo_only: false,
            pre_mode: None,
        }
    }
}

/// Find the next bookmark at or after `start_idx` in `bookmarks` whose
/// `category` matches `filter` (empty filter matches all).
///
/// Returns `(index, freq_hz, mode)` or `None` if no matching bookmark exists.
pub(super) fn scan_next_bookmark(
    bookmarks: &[Bookmark],
    filter: &str,
    start_idx: usize,
) -> Option<(usize, u64, DemodMode)> {
    bookmarks
        .iter()
        .enumerate()
        .skip(start_idx)
        .find(|(_, b)| filter.is_empty() || b.category == filter)
        .map(|(i, b)| (i, b.freq_hz, b.mode))
}
