#![forbid(unsafe_code)]

//! Section 1 — Welcome
//!
//! Introduces SDR, the app, and how to use the handbook.
//! Written for readers with zero prior radio experience.

use egui::Color32;

use crate::handbook::content::{ContentBlock, HandbookPage, HandbookSection};

pub const TAB_COLOR: Color32 = Color32::from_rgb(160, 50, 50);

pub fn section() -> HandbookSection {
    HandbookSection::new(
        "Welcome",
        "Welcome to SDRApp",
        TAB_COLOR,
        vec![page_what_is_sdr(), page_what_does_this_app_do(), page_how_to_use_handbook()],
    )
}

fn page_what_is_sdr() -> HandbookPage {
    HandbookPage::new(vec![
        ContentBlock::Heading("What is Software Defined Radio?"),
        ContentBlock::Body(
            "A radio receiver has three jobs: pick up a signal from the air, filter out \
             everything you don't want, and turn what's left into audio (or data). \
             Traditionally, dedicated hardware chips handled each step.",
        ),
        ContentBlock::Body(
            "Software Defined Radio (SDR) replaces most of that hardware with a small USB \
             stick and a computer. The stick digitises a wide slice of the radio spectrum — \
             hundreds of MHz at a time — and streams raw numbers to your computer. The \
             software then decides what to listen to and how to decode it.",
        ),
        ContentBlock::Callout {
            icon: "💡",
            text: "Think of it like this: a traditional radio is a point-and-shoot camera with \
                   a fixed lens. An SDR is a digital camera — the sensor captures everything, \
                   and software decides what to keep.",
        },
        ContentBlock::Subheading("What can you receive?"),
        ContentBlock::BulletList(&[
            "FM and AM broadcast radio",
            "Aircraft position beacons (ADS-B at 1090 MHz)",
            "Weather satellite images (NOAA APT at 137 MHz)",
            "Amateur radio contacts (HF, VHF, UHF)",
            "Trunked emergency service radio (with appropriate software)",
            "Pager traffic, ship AIS, and much more",
        ]),
        ContentBlock::Callout {
            icon: "⚠",
            text: "Receiving and decoding signals for personal use is generally legal. \
                   Transmitting is not — the SDRplay hardware is receive-only.",
        },
    ])
}

fn page_what_does_this_app_do() -> HandbookPage {
    HandbookPage::new(vec![
        ContentBlock::Heading("What Does This App Do?"),
        ContentBlock::Body(
            "SDRApp is a full-featured SDR receiver for the SDRplay RSPdx-R2. It shows you \
             the radio spectrum in real time, lets you tune to any frequency, and demodulates \
             signals so you can hear them through your speakers.",
        ),
        ContentBlock::Subheading("Key capabilities"),
        ContentBlock::BulletList(&[
            "Spectrum + waterfall display — see every signal in a 2 MHz+ slice of spectrum",
            "Wide-band FM stereo (WBFM) — listen to broadcast FM with full RDS station data",
            "Narrow-band FM (NFM) — scan local repeaters and public safety channels",
            "AM / SSB / CW demodulation — shortwave broadcasts, aviation, amateur radio",
            "MIDI control — map the Korg nanoKontrol2 to every knob and slider",
            "Frequency bookmarks and scanner — save favourites and auto-scan a list",
            "IQ and WAV recorder — capture signals to file for later analysis",
        ]),
        ContentBlock::Subheading("What you need to get started"),
        ContentBlock::NumberedList(&[
            "An SDRplay RSPdx-R2 receiver (or RTL-SDR for basic use)",
            "A wideband antenna — a simple telescopic whip works for FM and VHF",
            "The SDRplay API installed from sdrplay.com/api/",
            "This app running on macOS",
        ]),
        ContentBlock::Callout {
            icon: "💡",
            text: "For your first session, just plug in your SDRplay, connect an antenna, \
                   and tune to an FM broadcast station between 87.5–108 MHz. \
                   See the 'Your First Signal' section for a step-by-step guide.",
        },
    ])
}

fn page_how_to_use_handbook() -> HandbookPage {
    HandbookPage::new(vec![
        ContentBlock::Heading("How to Use This Handbook"),
        ContentBlock::Body(
            "This handbook is designed to stay open alongside the app. Keep it on a second \
             monitor or in a corner of your screen while you operate.",
        ),
        ContentBlock::Subheading("Navigating the binder"),
        ContentBlock::BulletList(&[
            "Click a coloured tab on the right side to jump to that section",
            "Use the ← → arrows at the bottom to move between pages within a section",
            "The page counter (e.g. '2 / 5') shows your position in the current section",
        ]),
        ContentBlock::Subheading("Sections at a glance"),
        ContentBlock::NumberedList(&[
            "Welcome — you are here",
            "App Layout — a map of every panel and control",
            "Your First Signal — step-by-step: plug in, tune, hear audio",
            "Signals & Modes — recognise common signal types on the waterfall",
            "Controls Reference — every knob, slider, and button explained",
            "MIDI & Shortcuts — nanoKontrol2 map and keyboard shortcuts",
        ]),
        ContentBlock::Spacer,
        ContentBlock::KeyBinding { key: "F1", action: "Open / close this handbook" },
        ContentBlock::KeyBinding { key: "?", action: "Keyboard shortcut overlay" },
        ContentBlock::Callout {
            icon: "ℹ",
            text: "Screenshots in this handbook show the actual app UI. \
                   Numbered callout circles (①②③) match the numbered items in the text.",
        },
    ])
}
