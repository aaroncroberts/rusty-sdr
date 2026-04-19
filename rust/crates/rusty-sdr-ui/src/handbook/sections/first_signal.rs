#![forbid(unsafe_code)]

//! Section 3 — Your First Signal
//!
//! Step-by-step tutorial for brand-new operators.  No SDR knowledge assumed.

use egui::Color32;

use crate::handbook::content::{ContentBlock, HandbookPage, HandbookSection};

pub const TAB_COLOR: Color32 = Color32::from_rgb(60, 130, 70);

pub fn section() -> HandbookSection {
    HandbookSection::new(
        "1st Signal",
        "Your First Signal",
        TAB_COLOR,
        vec![
            page_connect(),
            page_tune_fm(),
            page_adjust_gain(),
            page_hear_audio(),
        ],
    )
}

fn page_connect() -> HandbookPage {
    HandbookPage::new(vec![
        ContentBlock::Heading("Step 1 — Connect Your Hardware"),
        ContentBlock::Body(
            "Before you can hear anything, the hardware needs to be connected and the \
             software needs to start acquiring samples. Follow these steps exactly.",
        ),
        ContentBlock::NumberedList(&[
            "Plug the SDRplay RSPdx-R2 into a USB port on your Mac. \
             The device needs USB 3.0 — use a blue port if available.",
            "Attach your antenna to the port labelled 'A' (the SMA connector on the end). \
             For FM broadcast (88–108 MHz) a simple telescopic whip antenna works well.",
            "Launch Rusty SDR. You should see the spectrum and waterfall displays.",
            "In the Left Panel, make sure the source selector shows 'SDRplay RSPdx-R2' \
             and the antenna is set to 'A'.",
            "Click the green Start button. The status bar should show a green dot \
             and the waterfall should begin scrolling.",
        ]),
        ContentBlock::Callout {
            icon: "⚠",
            text: "If the Start button is greyed out or shows an error, check that the \
                   SDRplay API is installed. Download it free from sdrplay.com/api/",
        },
        ContentBlock::Callout {
            icon: "💡",
            text: "You can also launch the app with --auto-start to have it begin \
                   capturing immediately on startup.",
        },
    ])
}

fn page_tune_fm() -> HandbookPage {
    HandbookPage::new(vec![
        ContentBlock::Heading("Step 2 — Tune to an FM Station"),
        ContentBlock::Body(
            "FM broadcast stations transmit between 87.5 and 108.0 MHz. They appear as \
             wide, bright peaks in the spectrum and as thick bright stripes in the waterfall.",
        ),
        ContentBlock::Image {
            key: "spectrum_waterfall",
            caption: Some("Look for wide bright peaks like this — each one is an FM broadcast station"),
        },
        ContentBlock::Subheading("Finding a station"),
        ContentBlock::NumberedList(&[
            "Look at the waterfall. FM stations are typically 200–300 kHz wide and appear \
             as very bright (yellow/white) vertical bands.",
            "Click on the centre of one of those bright bands. The frequency display will \
             update to the clicked frequency.",
            "Alternatively, type a known station frequency into the frequency display. \
             For example, type '105.7' and press Enter for 105.7 MHz.",
            "In the Demodulation section, confirm the mode is set to WBFM \
             (Wide-Band FM). This is the correct mode for broadcast FM.",
        ]),
        ContentBlock::Callout {
            icon: "💡",
            text: "Use the arrow keys to fine-tune. Up/Down steps by the configured tune step, \
                   Left/Right steps by 10×. Try holding Down until you're centred on the \
                   loudest part of the signal.",
        },
        ContentBlock::Subheading("How to confirm you're on the right frequency"),
        ContentBlock::BulletList(&[
            "The spectrum peak should be centred in the middle of the display",
            "The waterfall stripe should run through the dead centre of the view",
            "The signal strength meter (S-meter) in the left panel should be well above the noise floor",
        ]),
    ])
}

fn page_adjust_gain() -> HandbookPage {
    HandbookPage::new(vec![
        ContentBlock::Heading("Step 3 — Adjust Your Gain"),
        ContentBlock::Body(
            "Gain controls how much the receiver amplifies the incoming signal. \
             Too little gain and the signal is buried in noise; too much and the ADC \
             overloads (clips), causing distortion and interference.",
        ),
        ContentBlock::Subheading("The easy way — use AGC"),
        ContentBlock::Body(
            "By default, Automatic Gain Control (AGC) is enabled. It continuously adjusts \
             the gain to keep the signal level near a target. For most situations, \
             leave AGC on and skip the manual steps below.",
        ),
        ContentBlock::Subheading("Manual gain adjustment"),
        ContentBlock::Body(
            "If you want manual control, disable AGC in the left panel device section \
             and adjust the LNA State and IF Gain sliders.",
        ),
        ContentBlock::BulletList(&[
            "LNA State — controls the first stage amplifier. Lower numbers = more gain. \
             Start at state 4 and adjust from there.",
            "IF Gain — fine-tunes the intermediate frequency amplifier. \
             Typically set to 0 dB and left alone.",
        ]),
        ContentBlock::Callout {
            icon: "⚠",
            text: "If you see 'ADC SAT' flash in the status bar, your gain is too high. \
                   Increase the LNA State number (less gain) or enable AGC.",
        },
        ContentBlock::Callout {
            icon: "💡",
            text: "For a very strong local FM transmitter (within a few km), you may need \
                   LNA State 8 or 9 to avoid overloading. For a distant station or weak \
                   signal, try state 1 or 2.",
        },
        ContentBlock::Subheading("What good gain looks like"),
        ContentBlock::BulletList(&[
            "The FM signal peak stands clearly above the noise floor",
            "The waterfall shows the signal as clearly brighter than the background",
            "No 'ADC SAT' warning in the status bar",
            "Audio sounds clean — no buzzing or crackling",
        ]),
    ])
}

fn page_hear_audio() -> HandbookPage {
    HandbookPage::new(vec![
        ContentBlock::Heading("Step 4 — Hear Your Station"),
        ContentBlock::Body(
            "With the signal tuned and gain set, the final step is confirming you can hear \
             the audio through your Mac's speakers or headphones.",
        ),
        ContentBlock::NumberedList(&[
            "In the Right Panel, find the Volume slider at the top. Make sure it is not \
             at zero — drag it to about 50%.",
            "Confirm the demodulation mode is WBFM in the Left Panel.",
            "You should hear audio. For a broadcast FM station, this will be music or speech.",
            "If you see the RDS section at the bottom of the Right Panel showing a station \
             name (like 'WMJI' or 'NPR'), your RDS decoding is working too.",
        ]),
        ContentBlock::Callout {
            icon: "⚠",
            text: "If you hear clicking or static, try this: press Stop, wait 2 seconds, \
                   press Start again. This clears any stale samples that accumulated while \
                   the demodulator was not running.",
        },
        ContentBlock::Callout {
            icon: "💡",
            text: "The VU meter in the right panel shows audio peak level. The bar should \
                   bounce with the music — if it stays at zero, the demod mode may be wrong \
                   or the volume is muted at the OS level.",
        },
        ContentBlock::Subheading("Stereo indicator"),
        ContentBlock::Body(
            "If the station broadcasts in stereo and your signal is strong enough, a \
             'Stereo' indicator appears in the demodulation section. You should hear \
             distinct left and right channels through headphones.",
        ),
        ContentBlock::Subheading("You did it!"),
        ContentBlock::Body(
            "Congratulations — you are now a software-defined radio operator. \
             From here, explore the band presets in the right panel to jump to other \
             frequency ranges, or read the Signals & Modes section to learn how to \
             identify different types of transmissions on the waterfall.",
        ),
    ])
}
