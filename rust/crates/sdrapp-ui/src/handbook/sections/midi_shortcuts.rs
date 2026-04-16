#![forbid(unsafe_code)]

//! Section 6 — MIDI & Shortcuts
//!
//! nanoKontrol2 control map, keyboard shortcuts, and MIDI Learn procedure.

use egui::Color32;

use crate::handbook::content::{ContentBlock, HandbookPage, HandbookSection};

pub const TAB_COLOR: Color32 = Color32::from_rgb(25, 120, 130);

pub fn section() -> HandbookSection {
    HandbookSection::new(
        "MIDI",
        "MIDI & Shortcuts",
        TAB_COLOR,
        vec![
            page_keyboard_shortcuts(),
            page_nanokontrol_overview(),
            page_nanokontrol_pages(),
            page_midi_learn(),
        ],
    )
}

fn page_keyboard_shortcuts() -> HandbookPage {
    HandbookPage::new(vec![
        ContentBlock::Heading("Keyboard Shortcuts"),
        ContentBlock::Body(
            "All keyboard shortcuts work whenever a text field does not have focus. \
             If a shortcut seems unresponsive, click anywhere outside a text field \
             and try again.",
        ),
        ContentBlock::Subheading("Navigation & Tuning"),
        ContentBlock::KeyBinding { key: "↑ / ↓", action: "Tune up / down by one tune step" },
        ContentBlock::KeyBinding { key: "→ / ←", action: "Tune up / down by 10 × tune step" },
        ContentBlock::Divider,
        ContentBlock::Subheading("Display"),
        ContentBlock::KeyBinding { key: "F1", action: "Open / close Operators Handbook" },
        ContentBlock::KeyBinding { key: "?", action: "Keyboard shortcut overlay" },
        ContentBlock::KeyBinding { key: "Ctrl+,", action: "Settings window" },
        ContentBlock::Divider,
        ContentBlock::Subheading("App State"),
        ContentBlock::KeyBinding { key: "Escape", action: "Cancel pending MIDI learn binding" },
        ContentBlock::Divider,
        ContentBlock::Subheading("MIDI (nanoKontrol2)"),
        ContentBlock::Body(
            "The CYCLE button on the nanoKontrol2 rotates through three control pages. \
             The current page is shown in the MIDI section of the right panel and in \
             the status bar.",
        ),
    ])
}

fn page_nanokontrol_overview() -> HandbookPage {
    HandbookPage::new(vec![
        ContentBlock::Heading("Korg nanoKontrol2 Overview"),
        ContentBlock::Body(
            "The Korg nanoKontrol2 is a compact USB MIDI controller with 8 channel strips, \
             each containing a knob, a fader, and three buttons (Solo/Mute/Record). \
             It also has transport controls (Play, Stop, Record, FF, Rew, Loop) and a \
             CYCLE button that SDRApp uses to switch between three control pages.",
        ),
        ContentBlock::Subheading("Physical layout"),
        ContentBlock::BulletList(&[
            "8 × Knob (K1–K8): rotary controllers, 0–127",
            "8 × Fader (F1–F8): linear sliders, 0–127",
            "8 × Solo button (S1–S8): toggle buttons",
            "8 × Mute button (M1–M8): toggle buttons",
            "8 × Record button (R1–R8): toggle buttons",
            "Transport row: Rew, FF, Stop, Play, Record",
            "CYCLE button: cycles the active control page",
            "Track ◀ / ▶: bank navigation",
            "Marker ◀ Set ▶: marker navigation",
        ]),
        ContentBlock::Callout {
            icon: "ℹ",
            text: "The nanoKontrol2 must be in its default factory MIDI configuration. \
                   If you have customised it with the Korg software, the button/CC \
                   assignments below may not match. A factory reset restores defaults.",
        },
        ContentBlock::Subheading("Opening the MIDI Mapper"),
        ContentBlock::Body(
            "Click the ▶ Mapper button in the MIDI section of the right panel. \
             A floating window shows a diagram of the nanoKontrol2 with every control \
             labelled with its current SDRApp assignment.",
        ),
    ])
}

fn page_nanokontrol_pages() -> HandbookPage {
    HandbookPage::new(vec![
        ContentBlock::Heading("nanoKontrol2 — Three CYCLE Pages"),
        ContentBlock::Body(
            "SDRApp maps the nanoKontrol2 across three logical pages, switched by the \
             CYCLE button. The current page is shown in the status bar (P1/P2/P3).",
        ),
        ContentBlock::Subheading("Page 1 — Main Controls"),
        ContentBlock::BulletList(&[
            "K1: Reference level (spectrum top)",
            "K2: Dynamic range (spectrum vertical span)",
            "K3: Waterfall level",
            "K4: Waterfall gain",
            "K5–K8: Tune step / demod mode (varies by context)",
            "F1: Volume",
            "F2–F4: not assigned (available for MIDI Learn)",
            "S1–S4: Start / Stop / Mode cycle / Record",
            "Transport Play/Stop: mirror the Start/Stop buttons",
        ]),
        ContentBlock::Subheading("Page 2 — Gain & Device"),
        ContentBlock::BulletList(&[
            "K1: LNA State (when AGC off)",
            "K2: IF Gain (when AGC off)",
            "K3: AGC Setpoint",
            "K4: Waterfall speed",
            "F1–F4: S-meter range adjustments",
            "S1: Toggle AGC on/off",
            "S2: Toggle FM notch filter",
            "S3: Toggle band plan overlay",
        ]),
        ContentBlock::Subheading("Page 3 — Scanner & Bookmarks"),
        ContentBlock::BulletList(&[
            "K1: Scanner dwell time",
            "S1: Start scanner",
            "S2: Stop scanner",
            "S3: Save bookmark at current frequency",
            "S4: Jump to next bookmark",
            "S5: Jump to previous bookmark",
            "Transport Rew/FF: previous/next bookmark",
        ]),
        ContentBlock::Callout {
            icon: "💡",
            text: "The CYCLE LED on the nanoKontrol2 blinks to indicate the active page: \
                   1 blink = Page 1, 2 blinks = Page 2, 3 blinks = Page 3.",
        },
    ])
}

fn page_midi_learn() -> HandbookPage {
    HandbookPage::new(vec![
        ContentBlock::Heading("MIDI Learn — Custom Mappings"),
        ContentBlock::Body(
            "MIDI Learn lets you re-map any MIDI CC from your controller to any knob \
             or slider in the app. This overrides the default nanoKontrol2 profile \
             and is useful if you use a different controller or prefer a different layout.",
        ),
        ContentBlock::Subheading("To map a MIDI CC to a control"),
        ContentBlock::NumberedList(&[
            "Right-click any knob or slider in the app. A context menu appears.",
            "Select 'MIDI Learn'. The control highlights and a banner appears \
             in the status bar saying 'Waiting for MIDI...'",
            "Move the knob, fader, or button on your MIDI controller.",
            "The app detects the MIDI CC and maps it to the control. The banner disappears.",
            "The mapping is saved automatically to config.json and persists on next launch.",
        ]),
        ContentBlock::Subheading("To remove a MIDI mapping"),
        ContentBlock::NumberedList(&[
            "Right-click the control.",
            "Select 'Clear MIDI Mapping'.",
        ]),
        ContentBlock::Callout {
            icon: "⚠",
            text: "MIDI Learn mappings override the default nanoKontrol2 profile. \
                   If you re-map a CC that was used by the default profile, the default \
                   assignment for that CC will no longer work.",
        },
        ContentBlock::Subheading("Viewing all current mappings"),
        ContentBlock::Body(
            "The Operators Handbook MIDI tab and the ? shortcut overlay both show the \
             complete list of active bindings. The MIDI Mapper window (▶ Mapper button) \
             shows them visually overlaid on the nanoKontrol2 diagram.",
        ),
    ])
}
