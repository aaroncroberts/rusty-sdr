#![forbid(unsafe_code)]

/// Every action the MIDI controller can trigger.
///
/// Using a typed enum instead of string dispatch means the compiler
/// catches missing cases, and tests can exhaustively cover every action.
#[derive(Debug, Clone, PartialEq)]
pub enum MidiAction {
    // Tuning
    TuneCoarseUp,
    TuneCoarseDown,
    TuneMediumUp,
    TuneMediumDown,
    TuneFineUp,
    TuneFineDown,
    // Transport
    PlayToggle,
    Stop,
    RecordStart,
    RecordStop,
    // Display
    ZoomIn,
    ZoomOut,
    // Page cycling (nanoKontrol2 CYCLE button)
    PageNext,
    // Volume
    VolumeSet(f32),
    // Passthrough for unmapped controls
    Unmapped,
}
