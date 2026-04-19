#![forbid(unsafe_code)]

//! Section 4 — Signals & Modes
//!
//! How to recognise common signal types and when to use each demodulation mode.

use egui::Color32;

use crate::handbook::content::{ContentBlock, HandbookPage, HandbookSection};

pub const TAB_COLOR: Color32 = Color32::from_rgb(50, 80, 170);

pub fn section() -> HandbookSection {
    HandbookSection::new(
        "Signals",
        "Signals & Modes",
        TAB_COLOR,
        vec![
            page_reading_waterfall(),
            page_fm_signals(),
            page_am_ssb_cw(),
            page_common_patterns(),
        ],
    )
}

fn page_reading_waterfall() -> HandbookPage {
    HandbookPage::new(vec![
        ContentBlock::Heading("Reading the Waterfall"),
        ContentBlock::Body(
            "The waterfall display is the single most powerful tool for understanding \
             what is happening in the radio spectrum. Learning to read it is worth the \
             time investment.",
        ),
        ContentBlock::Image {
            key: "spectrum_waterfall",
            caption: Some("Waterfall — time flows downward; frequency left-right; brightness = signal strength"),
        },
        ContentBlock::Subheading("What the axes mean"),
        ContentBlock::BulletList(&[
            "Horizontal (X) axis — frequency, increasing left to right",
            "Vertical (Y) axis — time, with the most recent moment at the top and older moments scrolling down",
            "Colour — signal power; bright yellow/white is strong; dark blue is the noise floor",
        ]),
        ContentBlock::Subheading("Basic shapes and what they mean"),
        ContentBlock::BulletList(&[
            "Wide bright vertical stripe — continuous FM broadcast station",
            "Narrow dim vertical stripe — narrowband FM (NFM) repeater or commercial radio",
            "Short bright horizontal dash — brief transmission (e.g. a push-to-talk radio)",
            "Two symmetric sidebands around a centre — AM carrier with upper and lower sidebands",
            "Single sideband on one side only — SSB voice (amateur radio, HF)",
            "Dashed or dotted vertical line — digital data mode or pager",
            "Ladder pattern — NOAA weather satellite APT image",
        ]),
        ContentBlock::Callout {
            icon: "💡",
            text: "Slow down the waterfall speed (right-click the spectrum area > options) \
                   if signals are passing too quickly to identify. A slower scroll reveals \
                   more detail in each transmission.",
        },
    ])
}

fn page_fm_signals() -> HandbookPage {
    HandbookPage::new(vec![
        ContentBlock::Heading("FM Signals"),
        ContentBlock::Subheading("Wide-Band FM (WBFM) — Broadcast Radio"),
        ContentBlock::Body(
            "Broadcast FM stations occupy about 200 kHz of spectrum each. On the waterfall \
             they look like a wide, very bright band. The audio channel is modulated with \
             up to ±75 kHz of deviation, and a 19 kHz stereo pilot tone is often visible \
             as a faint thin line at the edge of the main signal.",
        ),
        ContentBlock::BulletList(&[
            "Frequency range: 87.5 – 108.0 MHz",
            "Bandwidth: ~200 kHz per station",
            "App mode: WBFM",
            "Typical use: music, talk radio, news",
        ]),
        ContentBlock::Subheading("Narrow-Band FM (NFM) — Two-way Radio"),
        ContentBlock::Body(
            "NFM is used by handheld radios, repeaters, commercial radio systems, and \
             aviation control. The signal is much narrower — typically 12.5 kHz or 25 kHz — \
             and appears as a thin, faint vertical line on the waterfall. You will \
             often only see it when someone is actively transmitting.",
        ),
        ContentBlock::BulletList(&[
            "Common frequencies: 144–148 MHz (amateur 2m), 430–450 MHz (amateur 70cm), \
             462–467 MHz (FRS/GMRS), 806–870 MHz (trunked)",
            "Bandwidth: 12.5 kHz or 25 kHz",
            "App mode: NFM",
            "Typical use: local repeaters, emergency services, business radio",
        ]),
        ContentBlock::Callout {
            icon: "💡",
            text: "For NFM, enable the squelch slider to silence the receiver between \
                   transmissions. Set the threshold just above the noise floor.",
        },
    ])
}

fn page_am_ssb_cw() -> HandbookPage {
    HandbookPage::new(vec![
        ContentBlock::Heading("AM, SSB, and CW Signals"),
        ContentBlock::Subheading("Amplitude Modulation (AM)"),
        ContentBlock::Body(
            "AM is the oldest broadcast modulation. It uses a centre carrier flanked by \
             two identical sidebands. On the waterfall, look for a bright central stripe \
             with symmetric 'wings' on both sides. AM is used for medium-wave broadcast \
             (540–1700 kHz), aviation (108–137 MHz), and some shortwave.",
        ),
        ContentBlock::BulletList(&[
            "Aviation voice: 108.0 – 136.975 MHz (narrow AM)",
            "Shortwave broadcast: 3 – 30 MHz",
            "App mode: AM",
            "Bandwidth: 10 kHz (broadcast), 6 kHz (aviation)",
        ]),
        ContentBlock::Subheading("Single-Sideband (SSB)"),
        ContentBlock::Body(
            "SSB transmits only one sideband — either Upper (USB) or Lower (LSB). \
             This is more efficient than AM and is the dominant voice mode on HF amateur \
             radio. On the waterfall, SSB looks like a single smeared band, offset to one \
             side of the dial frequency.",
        ),
        ContentBlock::BulletList(&[
            "Lower sideband (LSB): 40m, 80m, 160m amateur bands",
            "Upper sideband (USB): 10m, 15m, 17m, 20m amateur bands; also HF marine/aero",
            "App mode: LSB or USB",
        ]),
        ContentBlock::Subheading("CW — Morse Code"),
        ContentBlock::Body(
            "CW (Continuous Wave) is Morse code — a single tone keyed on and off. \
             On the waterfall it appears as a dotted/dashed vertical line. \
             Listen for the characteristic dit-dah sound.",
        ),
        ContentBlock::BulletList(&[
            "All HF amateur bands have CW sub-bands near the low end",
            "App mode: CW",
            "BFO offset: the app generates an audio beat tone against the carrier",
        ]),
    ])
}

fn page_common_patterns() -> HandbookPage {
    HandbookPage::new(vec![
        ContentBlock::Heading("Common Signal Patterns"),
        ContentBlock::Body(
            "As you spend time watching the waterfall, certain patterns recur. \
             Here are some you are likely to encounter.",
        ),
        ContentBlock::Subheading("Aircraft ADS-B (1090 MHz)"),
        ContentBlock::Body(
            "Modern airliners broadcast their GPS position, altitude, speed, and callsign \
             on 1090.0 MHz. On the waterfall this looks like a forest of short pulsed bursts \
             appearing whenever an aircraft is within range. This app has a built-in ADS-B \
             decoder — click ✈ Map in the right panel to tune, decode, and show live \
             aircraft on a map in one click.",
        ),
        ContentBlock::Subheading("Weather satellites (137 MHz)"),
        ContentBlock::Body(
            "NOAA weather satellites pass overhead several times a day on 137.1, 137.5, \
             and 137.9125 MHz. Their APT image transmission looks like a distinctive \
             ladder or barcode pattern on the waterfall — two interleaved scan lines \
             with a clear 2400 Hz subcarrier frequency.",
        ),
        ContentBlock::Subheading("Pagers (~152 MHz / ~462 MHz)"),
        ContentBlock::Body(
            "FLEX and POCSAG pagers appear as wide-bandwidth bursts with a characteristic \
             blocky shape — strong signal, very flat top, abrupt edges.",
        ),
        ContentBlock::Subheading("Unidentified / interference"),
        ContentBlock::BulletList(&[
            "Perfectly horizontal stripe across the entire spectrum — internal to your computer (USB noise, CPU noise)",
            "Spurs at regular intervals — mixing products from nearby oscillators",
            "DC spike at dead centre — normal IQ imbalance artefact; tune slightly off-centre",
        ]),
        ContentBlock::Callout {
            icon: "💡",
            text: "A great resource for identifying unknown signals is the Signal ID Wiki at \
                   sigidwiki.com — upload a screenshot of your waterfall to compare.",
        },
    ])
}
