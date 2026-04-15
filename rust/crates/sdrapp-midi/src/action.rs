#![forbid(unsafe_code)]

/// Every action the MIDI controller can trigger.
#[derive(Debug, Clone, PartialEq)]
pub enum MidiAction {
    // ── Tuning ────────────────────────────────────────────────────────────────
    TuneCoarseUp,    // +1 MHz
    TuneCoarseDown,  // -1 MHz
    TuneMediumUp,    // +100 kHz
    TuneMediumDown,  // -100 kHz
    TuneFineUp,      // +10 kHz
    TuneFineDown,    // -10 kHz
    TuneUltraFineUp,   // +1 kHz
    TuneUltraFineDown, // -1 kHz

    // ── Demod & signal ────────────────────────────────────────────────────────
    DemodModeCycle, // WBFM → NFM → AM → WBFM
    StepSizeCycle,  // 100 Hz → 1 kHz → 10 kHz → 100 kHz (keyboard/scroll step)

    // ── Volume & squelch (absolute: raw MIDI value 0-127 carried in variant) ─
    VolumeSet(f32),   // maps 0-127 → 0.0-1.0
    SquelchSet(f32),  // maps 0-127 → -80.0-0.0 dBFS

    // ── Display ───────────────────────────────────────────────────────────────
    ZoomIn,
    ZoomOut,
    ZoomSet(f32),          // absolute from fader: 0-127 → 1.0-0.05
    WaterfallSpeedUp,
    WaterfallSpeedDown,
    WaterfallSpeedSet(f32), // absolute from fader: 0-127 → 0.1-5.0

    // ── Bookmarks ─────────────────────────────────────────────────────────────
    BookmarkNext,
    BookmarkPrev,
    BookmarkSave,

    // ── Recording ─────────────────────────────────────────────────────────────
    RecordStart,
    RecordStop,
    RecordingToggle,

    // ── Transport / system ────────────────────────────────────────────────────
    PlayToggle,
    Stop,
    HelpPanelToggle,

    // ── Page cycling ──────────────────────────────────────────────────────────
    PageNext,

    // ── Passthrough ───────────────────────────────────────────────────────────
    Unmapped,
}
