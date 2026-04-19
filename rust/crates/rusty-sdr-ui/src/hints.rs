#![forbid(unsafe_code)]

//! Contextual hint system — evaluates known problem conditions each frame and
//! returns the highest-priority active hint for display in the hint strip.
//!
//! Adding a new hint: append one entry to `ALL_HINTS` with a priority, message
//! closure, and optional action label + variant.  No other files need changing.

use rusty_sdr_core::signal_path::DemodMode;

/// Everything the hint evaluators need, assembled once per frame before calling
/// [`evaluate`].  All values are cheap copies — no locks held during evaluation.
pub struct HintCtx {
    pub is_running: bool,
    pub demod_mode: DemodMode,
    /// Half-span in Hz (full displayed bandwidth = span_hz × 2).
    pub span_hz: u64,
    pub snr_db: Option<f32>,
    /// Post-volume audio peak [0.0, 1.0].
    pub audio_level: f32,
    pub volume: f32,
    pub fft_clipping: bool,
    pub fm_notch_enabled: bool,
    pub frequency_hz: u64,
    pub scanner_running: bool,
}

/// What the hint strip should do when the user clicks the action button.
#[derive(Debug, Clone, PartialEq)]
pub enum HintAction {
    ZoomOut,
    SetVolume(f32),
    SetDemodMode(DemodMode),
    DisableFmNotch,
    MaxAttenuation,
}

/// A single active hint: message text + optional labelled action button.
#[derive(Debug, Clone)]
pub struct Hint {
    pub priority: u8,
    pub message: &'static str,
    /// If Some, render a small button with this label that fires the action.
    pub action: Option<(&'static str, HintAction)>,
}

// ── Hint definitions ─────────────────────────────────────────────────────────
// Lower priority number = shown first (highest urgency).

struct HintDef {
    priority: u8,
    message: &'static str,
    action: Option<(&'static str, HintAction)>,
    /// Returns true when this hint should be shown.
    condition: fn(&HintCtx) -> bool,
}

static ALL_HINTS: &[HintDef] = &[
    // P0 — ADC saturation: hardware is clipping (no action button — prevents accidental max-atten)
    HintDef {
        priority: 0,
        message: "⚡ ADC saturated — reduce LNA State in Device Settings",
        action: None,
        condition: |c| c.is_running && c.fft_clipping,
    },
    // P1 — FM notch active while tuned to FM broadcast band
    HintDef {
        priority: 1,
        message: "⚠ FM notch active — filter is cutting your listening band",
        action: Some(("Disable Notch", HintAction::DisableFmNotch)),
        condition: |c| {
            c.is_running
                && c.fm_notch_enabled
                && c.frequency_hz >= 87_000_000
                && c.frequency_hz <= 108_000_000
        },
    },
    // P2 — WBFM requires at least ~150 kHz visible span
    HintDef {
        priority: 2,
        message: "WBFM needs ~200 kHz visible — zoom out to hear stereo audio",
        action: Some(("Zoom Out", HintAction::ZoomOut)),
        condition: |c| {
            c.is_running
                && c.demod_mode == DemodMode::Wbfm
                && (c.span_hz * 2) < 150_000
        },
    },
    // P3 — (volume muted hint removed — dedicated Mute button handles this)
    // P4 — Very weak signal
    HintDef {
        priority: 4,
        message: "Weak signal — try tuning ±5 kHz or adjusting gain",
        action: None,
        condition: |c| {
            c.is_running
                && c.snr_db.map(|s| s < 5.0).unwrap_or(false)
        },
    },
    // P5 — Scanner is running (informational, low priority)
    HintDef {
        priority: 5,
        message: "Scanner active — frequency changing automatically",
        action: None,
        condition: |c| c.is_running && c.scanner_running,
    },
];

/// Evaluate all hint conditions against `ctx`.
/// Returns the single highest-priority active hint, or `None` if all is well.
pub fn evaluate(ctx: &HintCtx) -> Option<Hint> {
    ALL_HINTS
        .iter()
        .filter(|def| (def.condition)(ctx))
        .min_by_key(|def| def.priority)
        .map(|def| Hint {
            priority: def.priority,
            message: def.message,
            action: def.action.clone(),
        })
}
