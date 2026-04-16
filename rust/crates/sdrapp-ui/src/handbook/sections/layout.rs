#![forbid(unsafe_code)]

//! Section 2 — App Layout
//!
//! Annotated overview of every panel and region.

use egui::Color32;

use crate::handbook::content::{ContentBlock, HandbookPage, HandbookSection};

pub const TAB_COLOR: Color32 = Color32::from_rgb(180, 100, 30);

pub fn section() -> HandbookSection {
    HandbookSection::new(
        "Layout",
        "App Layout",
        TAB_COLOR,
        vec![page_overview(), page_left_panel(), page_center_panel(), page_right_panel(), page_status_bar()],
    )
}

fn page_overview() -> HandbookPage {
    HandbookPage::new(vec![
        ContentBlock::Heading("App Layout Overview"),
        ContentBlock::Body(
            "The app is divided into four main areas: a left control panel, a centre spectrum \
             display, a right information panel, and a thin status bar along the bottom.",
        ),
        ContentBlock::Image {
            key: "full_layout",
            caption: Some("Full app layout — ① Left Panel  ② Spectrum  ③ Waterfall  ④ Right Panel  ⑤ Status Bar"),
        },
        ContentBlock::Subheading("The four regions"),
        ContentBlock::NumberedList(&[
            "Left Panel — source selection, frequency, demodulation mode, scanner",
            "Spectrum — real-time FFT power plot; drag to set reference level",
            "Waterfall — scrolling time-vs-frequency heat map",
            "Right Panel — volume, band presets, recorder, MIDI, RDS",
            "Status Bar — device info, buffer health, recording indicator",
        ]),
        ContentBlock::Callout {
            icon: "💡",
            text: "Both side panels scroll vertically — if a control is not visible, \
                   try scrolling down inside the panel.",
        },
    ])
}

fn page_left_panel() -> HandbookPage {
    HandbookPage::new(vec![
        ContentBlock::Heading("Left Panel"),
        ContentBlock::Image {
            key: "left_panel",
            caption: Some("Left panel — source selector at top, frequency in the middle, demod and scanner below"),
        },
        ContentBlock::Subheading("Source section"),
        ContentBlock::Body(
            "At the top you choose which hardware device to use (SDRplay RSPdx-R2 or \
             RTL-SDR), the antenna port (A, B, or C), and the sample rate. Higher sample \
             rates show a wider slice of spectrum but use more CPU.",
        ),
        ContentBlock::Subheading("Frequency section"),
        ContentBlock::Body(
            "The large frequency display shows the centre frequency in MHz. Click any digit \
             and scroll the mouse wheel to tune, or type a frequency directly. The arrow \
             keys also tune up and down in configurable steps.",
        ),
        ContentBlock::Subheading("Demodulation section"),
        ContentBlock::Body(
            "Select the demodulation mode — WBFM for broadcast FM, NFM for narrow-band, \
             AM for shortwave and aviation, SSB for single-sideband, or CW for Morse code. \
             Mode-specific controls appear below the selector.",
        ),
        ContentBlock::Subheading("Scanner / Bookmarks"),
        ContentBlock::Body(
            "Save favourite frequencies as bookmarks. The scanner automatically cycles \
             through bookmarks, stopping when a signal exceeds the squelch threshold.",
        ),
    ])
}

fn page_center_panel() -> HandbookPage {
    HandbookPage::new(vec![
        ContentBlock::Heading("Spectrum & Waterfall"),
        ContentBlock::Image {
            key: "spectrum_waterfall",
            caption: Some("Spectrum (top) and waterfall (bottom) — bright peaks are signals"),
        },
        ContentBlock::Subheading("Spectrum trace"),
        ContentBlock::Body(
            "The top section shows a live FFT power plot — frequency along the X axis, \
             signal strength (dBFS) on the Y axis. Each bright spike is a signal. \
             The smooth average is overlaid in a lighter colour.",
        ),
        ContentBlock::BulletList(&[
            "Drag the top ruler up/down to set the reference level",
            "Scroll the mouse wheel on the spectrum to zoom in/out",
            "Click any frequency in the display to tune there instantly",
        ]),
        ContentBlock::Subheading("Waterfall display"),
        ContentBlock::Body(
            "The waterfall scrolls time downward. Colour represents signal strength: \
             bright/yellow = strong, dark blue = weak or noise floor. Continuous signals \
             appear as vertical stripes; brief transmissions appear as horizontal dashes.",
        ),
        ContentBlock::Callout {
            icon: "💡",
            text: "To find a busy frequency, watch the waterfall for a minute without tuning. \
                   Strong local signals will appear as bright persistent stripes.",
        },
        ContentBlock::Subheading("Spectrum controls"),
        ContentBlock::BulletList(&[
            "Ref Level — top of the Y axis; drag down to see weaker signals",
            "Dyn Range — how many dB the display spans (vertical scale)",
            "Averaging — smooths the trace; higher = less noise, slower response",
            "FFT Size — more bins = better frequency resolution, more CPU cost",
        ]),
    ])
}

fn page_right_panel() -> HandbookPage {
    HandbookPage::new(vec![
        ContentBlock::Heading("Right Panel"),
        ContentBlock::Image {
            key: "right_panel",
            caption: Some("Right panel — volume at top, bands, recorder, MIDI status, RDS at bottom"),
        },
        ContentBlock::Subheading("Volume & VU meter"),
        ContentBlock::Body(
            "The volume slider controls audio output level. The VU bar above it shows \
             instantaneous audio peak level — keep it below the top to avoid clipping.",
        ),
        ContentBlock::Subheading("Band presets"),
        ContentBlock::Body(
            "Quick-jump buttons for common frequency bands: FM broadcast, aircraft, \
             marine VHF, amateur 2m/70cm, weather satellites, and others. Clicking a band \
             tunes to the centre of that range and sets the appropriate demod mode.",
        ),
        ContentBlock::Subheading("Recorder"),
        ContentBlock::Body(
            "Record the demodulated audio as a WAV file. You can also schedule a timed \
             recording — set a delay and duration, then click Schedule.",
        ),
        ContentBlock::Subheading("MIDI status"),
        ContentBlock::Body(
            "Shows the name of the connected MIDI controller and which CYCLE page is active \
             on the nanoKontrol2. The ▶ Mapper button opens the visual control map.",
        ),
        ContentBlock::Subheading("RDS (Radio Data System)"),
        ContentBlock::Body(
            "When tuned to a WBFM broadcast station that carries RDS, the station name, \
             programme type (PTY), and RadioText scrolling message appear here. \
             RDS is a European/international standard — not all stations transmit it.",
        ),
    ])
}

fn page_status_bar() -> HandbookPage {
    HandbookPage::new(vec![
        ContentBlock::Heading("Status Bar"),
        ContentBlock::Image {
            key: "status_bar",
            caption: Some("Status bar — device · MIDI page · buffer health · recording indicator"),
        },
        ContentBlock::Body(
            "The thin bar at the very bottom of the window shows a quick summary of the \
             app's current state at all times.",
        ),
        ContentBlock::BulletList(&[
            "Green dot + device name — hardware is connected and running",
            "Red dot — hardware stopped or not connected; click Start in the left panel",
            "MIDI page indicator — shows the current nanoKontrol2 CYCLE page (1, 2, or 3)",
            "Buffer bar — shows how full the IQ receive buffer is; should stay below 50%",
            "REC badge — flashes red while a recording is in progress",
            "ADC SAT — appears if the ADC is clipping (signal too strong — reduce gain)",
        ]),
        ContentBlock::Callout {
            icon: "⚠",
            text: "If the buffer bar is consistently above 80% or you see 'Lagged' warnings \
                   in the log, your CPU may not be keeping up. Try reducing the FFT size or \
                   disabling spectrum averaging.",
        },
    ])
}
