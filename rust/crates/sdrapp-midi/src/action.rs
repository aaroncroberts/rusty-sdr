#![forbid(unsafe_code)]

/// Every action the MIDI controller can trigger.
#[derive(Debug, Clone, PartialEq)]
pub enum MidiAction {
    // ── Tuning (button — fixed step) ─────────────────────────────────────────
    TuneCoarseUp,      // +1 MHz
    TuneCoarseDown,    // -1 MHz
    TuneMediumUp,      // +100 kHz
    TuneMediumDown,    // -100 kHz
    TuneFineUp,        // +10 kHz
    TuneFineDown,      // -10 kHz
    TuneUltraFineUp,   // +1 kHz
    TuneUltraFineDown, // -1 kHz

    // ── Tuning (knob/fader — relative delta × hz_per_unit) ───────────────────
    // Controller tracks last CC value; delta = new − old → tune by delta × scale.
    TuneKnobCoarse,     // 1 000 000 Hz / CC unit
    TuneKnobMedium,     //   100 000 Hz / CC unit
    TuneKnobFine,       //    10 000 Hz / CC unit
    TuneKnobUltraFine,  //     1 000 Hz / CC unit

    // ── Demod & signal ────────────────────────────────────────────────────────
    DemodModeCycle, // WBFM → NFM → AM → WBFM
    StepSizeCycle,  // 100 Hz → 1 kHz → 10 kHz → 100 kHz (keyboard/scroll step)

    // ── Volume & squelch (absolute: raw MIDI value 0-127 carried in variant) ─
    VolumeSet(f32),  // maps 0-127 → 0.0-1.0
    SquelchSet(f32), // maps 0-127 → -80.0-0.0 dBFS

    // ── Display ───────────────────────────────────────────────────────────────
    ZoomIn,
    ZoomOut,
    ZoomSet(f32), // absolute from fader: 0-127 → 1.0-0.05
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
