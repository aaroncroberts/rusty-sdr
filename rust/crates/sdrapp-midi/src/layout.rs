#![forbid(unsafe_code)]

//! Controller layout model for the interactive MIDI mapper.
//!
//! A [`ControllerLayout`] describes the physical controls on a MIDI controller —
//! their positions, types, and MIDI keys — so the mapper window can render any
//! controller without knowing device-specific details.
//!
//! The [`NanoKontrol2Layout`] provides the full Korg nanoKONTROL2 layout with all
//! 51 controls (8 channel strips × 5 controls + 11 transport buttons).

use crate::config::{MidiKey, MidiKeyKind};

// ── Types ─────────────────────────────────────────────────────────────────────

/// The physical type of a controller element, used by the renderer to choose shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlType {
    /// Rotary knob (CC, typically absolute 0-127).
    Knob,
    /// Vertical fader / slider (CC, absolute 0-127).
    Fader,
    /// Momentary push button (Note On/Off or CC toggle).
    Button,
}

/// Position and size of a control in the controller's reference canvas.
///
/// All values are in the coordinate units returned by
/// [`ControllerLayout::canvas_size`]. The mapper renderer scales them to screen
/// pixels using the window size / canvas size ratio.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ControlRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl ControlRect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }
}

/// Description of one physical control on a controller.
pub struct ControlDef {
    /// Unique stable identifier within the controller (e.g. `"fader_0"`, `"transport_play"`).
    pub id: &'static str,
    /// Short display label rendered inside the control widget (e.g. `"F1"`, `"PLAY"`).
    pub label: &'static str,
    /// Physical type of the control — determines rendered shape.
    pub control_type: ControlType,
    /// MIDI key (channel + message type + number) that this control transmits.
    pub midi_key: MidiKey,
    /// Bounding rect in reference canvas coordinates.
    pub rect: ControlRect,
}

// ── Trait ─────────────────────────────────────────────────────────────────────

/// A hardware MIDI controller's physical layout.
///
/// Implement this trait to make a controller renderable in the mapper window.
/// The mapper window calls [`controls`] to enumerate controls, [`canvas_size`] to
/// know the coordinate space, and [`page_names`] to populate the page tabs.
pub trait ControllerLayout: Send + Sync {
    /// Display name of the controller (e.g. `"Korg nanoKONTROL2"`).
    fn name(&self) -> &str;

    /// All physical controls, in any stable order.
    fn controls(&self) -> &[ControlDef];

    /// Reference canvas size `(width, height)` in arbitrary units.
    ///
    /// All [`ControlDef`] rects are expressed in these units. The renderer
    /// computes `scale = window_pixels / canvas_units` and applies it uniformly.
    fn canvas_size(&self) -> (f32, f32);

    /// Ordered page names for the page-selector tabs in the mapper window.
    ///
    /// For single-page controllers return a one-element slice.
    fn page_names(&self) -> &[&str];
}

// ── nanoKONTROL2 implementation ───────────────────────────────────────────────

fn cc(number: u8) -> MidiKey {
    MidiKey { channel: 0, kind: MidiKeyKind::ControlChange, number }
}

fn note(number: u8) -> MidiKey {
    MidiKey { channel: 0, kind: MidiKeyKind::NoteOn, number }
}

// Reference canvas: 576 × 110 units.
// Transport section = 0..112; 8 channel strips × 58 units each = 464; total = 576.
//
// Physical nanoKontrol2 layout (left → right):
//   Transport: Prev/Next/Cycle/Set/Markers (nav row) + Rew/FF/Stop/Play/Rec (playback row)
//   Channel strips: 8 × (Knob top · S/M/R buttons middle · Fader bottom)
const CANVAS_W: f32 = 576.0;
const CANVAS_H: f32 = 110.0;

// Channel strips begin after the transport section.
const STRIP_OFFSET_X: f32 = 114.0;
const STRIP_W: f32 = 57.0;

const fn knob_rect(col: u8) -> ControlRect {
    ControlRect::new(STRIP_OFFSET_X + col as f32 * STRIP_W + 8.0, 5.0, 40.0, 40.0)
}

/// SMR row button: col = track 0-7, smr = 0 (S) / 1 (M) / 2 (R)
const fn smr_rect(col: u8, smr: u8) -> ControlRect {
    ControlRect::new(STRIP_OFFSET_X + col as f32 * STRIP_W + 5.0 + smr as f32 * 17.0, 50.0, 14.0, 14.0)
}

const fn fader_rect(col: u8) -> ControlRect {
    ControlRect::new(STRIP_OFFSET_X + col as f32 * STRIP_W + 24.0, 68.0, 10.0, 36.0)
}

/// Korg nanoKONTROL2 physical layout.
///
/// Reference canvas: 560 × 110 units.
///
/// Layout (left→right):
/// - 8 channel strips: knob (row 0) · S/M/R buttons (row 1-3) · fader (row 4)
/// - Transport section: navigation row + playback row (right of strips)
pub struct NanoKontrol2Layout {
    controls: Vec<ControlDef>,
}

impl NanoKontrol2Layout {
    pub fn new() -> Self {
        let mut controls: Vec<ControlDef> = Vec::with_capacity(51);

        // ── 8 channel strips ─────────────────────────────────────────────────

        const KNOB_IDS:   [&str; 8] = ["knob_0","knob_1","knob_2","knob_3","knob_4","knob_5","knob_6","knob_7"];
        const KNOB_LBL:   [&str; 8] = ["K1","K2","K3","K4","K5","K6","K7","K8"];
        const S_IDS:      [&str; 8] = ["s_0","s_1","s_2","s_3","s_4","s_5","s_6","s_7"];
        const S_LBL:      [&str; 8] = ["S1","S2","S3","S4","S5","S6","S7","S8"];
        const M_IDS:      [&str; 8] = ["m_0","m_1","m_2","m_3","m_4","m_5","m_6","m_7"];
        const M_LBL:      [&str; 8] = ["M1","M2","M3","M4","M5","M6","M7","M8"];
        const R_IDS:      [&str; 8] = ["r_0","r_1","r_2","r_3","r_4","r_5","r_6","r_7"];
        const R_LBL:      [&str; 8] = ["R1","R2","R3","R4","R5","R6","R7","R8"];
        const FADER_IDS:  [&str; 8] = ["fader_0","fader_1","fader_2","fader_3","fader_4","fader_5","fader_6","fader_7"];
        const FADER_LBL:  [&str; 8] = ["F1","F2","F3","F4","F5","F6","F7","F8"];

        for col in 0u8..8 {
            let c = col as usize;
            controls.push(ControlDef { id: KNOB_IDS[c],  label: KNOB_LBL[c],  control_type: ControlType::Knob,   midi_key: cc(16 + col),    rect: knob_rect(col) });
            controls.push(ControlDef { id: S_IDS[c],     label: S_LBL[c],     control_type: ControlType::Button, midi_key: note(32 + col),   rect: smr_rect(col, 0) });
            controls.push(ControlDef { id: M_IDS[c],     label: M_LBL[c],     control_type: ControlType::Button, midi_key: note(48 + col),   rect: smr_rect(col, 1) });
            controls.push(ControlDef { id: R_IDS[c],     label: R_LBL[c],     control_type: ControlType::Button, midi_key: note(64 + col),   rect: smr_rect(col, 2) });
            controls.push(ControlDef { id: FADER_IDS[c], label: FADER_LBL[c], control_type: ControlType::Fader,  midi_key: cc(col),          rect: fader_rect(col) });
        }

        // ── Transport: navigation row (y=5) ──────────────────────────────────
        // Physical nanoKontrol2: transport is on the LEFT of the device.
        // Prev Track · Next Track · Cycle · Set · ◄ Marker · Marker ►
        let nav: &[(&str, &str, MidiKey, f32, f32, f32, f32)] = &[
            ("transport_prev",   "◄◄",   note(58),  2.0, 5.0, 17.0, 14.0),
            ("transport_next",   "►►",   note(59), 21.0, 5.0, 17.0, 14.0),
            ("transport_cycle",  "CYC",  note(46), 40.0, 5.0, 17.0, 14.0),
            ("transport_set",    "SET",  note(60), 59.0, 5.0, 14.0, 14.0),
            ("transport_mark_l", "◄",    note(61), 75.0, 5.0, 14.0, 14.0),
            ("transport_mark_r", "►",    note(62), 91.0, 5.0, 14.0, 14.0),
        ];
        for &(id, label, ref key, x, y, w, h) in nav {
            controls.push(ControlDef { id, label, control_type: ControlType::Button, midi_key: key.clone(), rect: ControlRect::new(x, y, w, h) });
        }

        // ── Transport: playback row (y=24) ────────────────────────────────────
        // Rewind · Fast-Forward · Stop · Play · Record
        let playback: &[(&str, &str, MidiKey, f32, f32, f32, f32)] = &[
            ("transport_rew",  "<<",  note(43),  2.0, 24.0, 20.0, 16.0),
            ("transport_ff",   ">>",  note(44), 24.0, 24.0, 20.0, 16.0),
            ("transport_stop", "STP", note(42), 46.0, 24.0, 20.0, 16.0),
            ("transport_play", "PLY", note(41), 68.0, 24.0, 20.0, 16.0),
            ("transport_rec",  "REC", note(45), 90.0, 24.0, 20.0, 16.0),
        ];
        for &(id, label, ref key, x, y, w, h) in playback {
            controls.push(ControlDef { id, label, control_type: ControlType::Button, midi_key: key.clone(), rect: ControlRect::new(x, y, w, h) });
        }

        Self { controls }
    }
}

impl Default for NanoKontrol2Layout {
    fn default() -> Self {
        Self::new()
    }
}

impl ControllerLayout for NanoKontrol2Layout {
    fn name(&self) -> &str { "Korg nanoKONTROL2" }
    fn controls(&self) -> &[ControlDef] { &self.controls }
    fn canvas_size(&self) -> (f32, f32) { (CANVAS_W, CANVAS_H) }
    fn page_names(&self) -> &[&str] { &["Tune", "Display", "Record"] }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn nk() -> NanoKontrol2Layout { NanoKontrol2Layout::new() }

    #[test]
    fn nanokontrol2_control_count() {
        // 8 strips × 5 controls (knob + S + M + R + fader) + 11 transport = 51
        assert_eq!(nk().controls().len(), 51);
    }

    #[test]
    fn no_duplicate_control_ids() {
        let layout = nk();
        let ids: HashSet<&str> = layout.controls().iter().map(|c| c.id).collect();
        assert_eq!(ids.len(), layout.controls().len(), "duplicate control IDs");
    }

    #[test]
    fn no_duplicate_midi_keys() {
        let layout = nk();
        // nanoKontrol2 uses channel 0 throughout; (kind, number) must be unique.
        let pairs: HashSet<(MidiKeyKind, u8)> = layout.controls().iter()
            .map(|c| (c.midi_key.kind.clone(), c.midi_key.number))
            .collect();
        assert_eq!(pairs.len(), layout.controls().len(), "duplicate (kind, number) MIDI keys");
    }

    #[test]
    fn faders_are_cc_0_to_7() {
        let layout = nk();
        let mut ccs: Vec<u8> = layout.controls().iter()
            .filter(|c| c.control_type == ControlType::Fader)
            .map(|c| c.midi_key.number)
            .collect();
        ccs.sort_unstable();
        assert_eq!(ccs, vec![0,1,2,3,4,5,6,7]);
    }

    #[test]
    fn knobs_are_cc_16_to_23() {
        let layout = nk();
        let mut ccs: Vec<u8> = layout.controls().iter()
            .filter(|c| c.control_type == ControlType::Knob)
            .map(|c| c.midi_key.number)
            .collect();
        ccs.sort_unstable();
        assert_eq!(ccs, vec![16,17,18,19,20,21,22,23]);
    }

    #[test]
    fn s_buttons_are_note_32_to_39() {
        let layout = nk();
        let mut ns: Vec<u8> = layout.controls().iter()
            .filter(|c| c.id.starts_with("s_"))
            .map(|c| c.midi_key.number)
            .collect();
        ns.sort_unstable();
        assert_eq!(ns, vec![32,33,34,35,36,37,38,39]);
    }

    #[test]
    fn m_buttons_are_note_48_to_55() {
        let layout = nk();
        let mut ns: Vec<u8> = layout.controls().iter()
            .filter(|c| c.id.starts_with("m_"))
            .map(|c| c.midi_key.number)
            .collect();
        ns.sort_unstable();
        assert_eq!(ns, vec![48,49,50,51,52,53,54,55]);
    }

    #[test]
    fn r_buttons_are_note_64_to_71() {
        let layout = nk();
        let mut ns: Vec<u8> = layout.controls().iter()
            .filter(|c| c.id.starts_with("r_"))
            .map(|c| c.midi_key.number)
            .collect();
        ns.sort_unstable();
        assert_eq!(ns, vec![64,65,66,67,68,69,70,71]);
    }

    #[test]
    fn transport_note_numbers_match_nanokontrol2_spec() {
        let layout = nk();
        let find = |id: &str| -> u8 {
            layout.controls().iter().find(|c| c.id == id)
                .unwrap_or_else(|| panic!("control '{id}' not found"))
                .midi_key.number
        };
        assert_eq!(find("transport_play"),   41);
        assert_eq!(find("transport_stop"),   42);
        assert_eq!(find("transport_rew"),    43);
        assert_eq!(find("transport_ff"),     44);
        assert_eq!(find("transport_rec"),    45);
        assert_eq!(find("transport_cycle"),  46);
        assert_eq!(find("transport_prev"),   58);
        assert_eq!(find("transport_next"),   59);
        assert_eq!(find("transport_set"),    60);
        assert_eq!(find("transport_mark_l"), 61);
        assert_eq!(find("transport_mark_r"), 62);
    }

    #[test]
    fn transport_has_11_buttons() {
        let layout = nk();
        let n = layout.controls().iter()
            .filter(|c| c.id.starts_with("transport_"))
            .count();
        assert_eq!(n, 11);
    }

    #[test]
    fn canvas_size_is_576x110() {
        assert_eq!(nk().canvas_size(), (576.0, 110.0));
    }

    #[test]
    fn page_names_are_tune_display_record() {
        assert_eq!(nk().page_names(), &["Tune", "Display", "Record"]);
    }

    #[test]
    fn all_rects_within_canvas() {
        let layout = nk();
        let (cw, ch) = layout.canvas_size();
        for ctrl in layout.controls() {
            let r = ctrl.rect;
            assert!(
                r.x >= 0.0 && r.x + r.w <= cw,
                "control '{}' x out of bounds: x={} w={} canvas_w={}",
                ctrl.id, r.x, r.w, cw
            );
            assert!(
                r.y >= 0.0 && r.y + r.h <= ch,
                "control '{}' y out of bounds: y={} h={} canvas_h={}",
                ctrl.id, r.y, r.h, ch
            );
        }
    }

    #[test]
    fn name_is_korg_nanokontrol2() {
        assert_eq!(nk().name(), "Korg nanoKONTROL2");
    }
}
