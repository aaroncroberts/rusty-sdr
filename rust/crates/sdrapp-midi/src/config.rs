#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};

/// A MIDI key: channel + message type + note/CC number.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MidiKey {
    pub channel: u8,
    pub kind: MidiKeyKind,
    pub number: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MidiKeyKind {
    ControlChange,
    NoteOn,
}

/// Serializable action tag for the config file.
/// Mirrors MidiAction but is serde-friendly as a string.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MidiActionTag {
    // ── Tuning (button) ───────────────────────────────────────────────────────
    TuneCoarseUp,
    TuneCoarseDown,
    TuneMediumUp,
    TuneMediumDown,
    TuneFineUp,
    TuneFineDown,
    TuneUltraFineUp,
    TuneUltraFineDown,

    // ── Tuning (knob/fader — relative delta) ─────────────────────────────────
    TuneKnobCoarse,    // 1 MHz / CC unit
    TuneKnobMedium,    // 100 kHz / CC unit
    TuneKnobFine,      // 10 kHz / CC unit
    TuneKnobUltraFine, // 1 kHz / CC unit

    // ── Demod & signal ────────────────────────────────────────────────────────
    DemodModeCycle,
    StepSizeCycle,

    // ── Volume & squelch (absolute, value 0-127) ──────────────────────────────
    VolumeSet,
    SquelchSet,

    // ── Display ───────────────────────────────────────────────────────────────
    ZoomIn,
    ZoomOut,
    ZoomSet,
    WaterfallSpeedUp,
    WaterfallSpeedDown,
    WaterfallSpeedSet,

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

/// A single MIDI binding entry — one row in the binding table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindingEntry {
    pub page: usize,
    pub key: MidiKey,
    pub action: MidiActionTag,
}

/// Persistent MIDI config: device selection and binding list.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MidiConfig {
    /// MIDI port name to connect to. None = first available port.
    pub port_name: Option<String>,
    /// Current active page (0-based, wraps at page_count).
    pub current_page: usize,
    pub page_count: usize,
    /// Binding list serialized as an array of entries (JSON-safe).
    pub bindings: Vec<BindingEntry>,
}

impl MidiConfig {
    pub fn with_nanokontrol2_defaults() -> Self {
        use super::nanokontrol2::default_bindings;
        let bindings = default_bindings()
            .into_iter()
            .map(|((page, key), action)| BindingEntry { page, key, action })
            .collect();
        Self {
            port_name: Some("nanoKONTROL2".into()),
            current_page: 0,
            page_count: 3,
            bindings,
        }
    }

    /// Look up the action for a given (page, key) pair.
    pub fn lookup(&self, page: usize, key: &MidiKey) -> Option<&MidiActionTag> {
        self.bindings
            .iter()
            .find(|e| e.page == page && &e.key == key)
            .map(|e| &e.action)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_round_trips_json() {
        let cfg = MidiConfig::with_nanokontrol2_defaults();
        let json = serde_json::to_string(&cfg).unwrap();
        let restored: MidiConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.page_count, 3);
        assert_eq!(restored.port_name.as_deref(), Some("nanoKONTROL2"));
        assert!(!restored.bindings.is_empty());
    }
}
