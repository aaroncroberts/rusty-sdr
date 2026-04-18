#![forbid(unsafe_code)]

//! Section 5 — Controls Reference
//!
//! Systematic walkthrough of every significant control in the app.

use egui::Color32;

use crate::handbook::content::{ContentBlock, HandbookPage, HandbookSection};

pub const TAB_COLOR: Color32 = Color32::from_rgb(100, 50, 160);

pub fn section() -> HandbookSection {
    HandbookSection::new(
        "Controls",
        "Controls Reference",
        TAB_COLOR,
        vec![
            page_frequency_widget(),
            page_spectrum_controls(),
            page_gain_controls(),
            page_audio_controls(),
            page_recorder_controls(),
            page_bookmark_scanner(),
        ],
    )
}

fn page_frequency_widget() -> HandbookPage {
    HandbookPage::new(vec![
        ContentBlock::Heading("Frequency Widget"),
        ContentBlock::Image {
            key: "frequency_widget",
            caption: Some("The frequency display — click any digit to edit, scroll to tune"),
        },
        ContentBlock::Body(
            "The large frequency display is the most-used control in the app. \
             It shows the current centre frequency in MHz and supports several \
             input methods.",
        ),
        ContentBlock::Subheading("Ways to tune"),
        ContentBlock::BulletList(&[
            "Mouse scroll wheel — hover over any digit and scroll to change it",
            "Click a digit then type a number — replaces the digit and advances the cursor",
            "Arrow keys — Up/Down step by the configured tune step; Left/Right by 10×",
            "Click the spectrum or waterfall — tunes to the clicked frequency",
            "Band presets (right panel) — jump directly to a band centre",
        ]),
        ContentBlock::Subheading("Tune step"),
        ContentBlock::Body(
            "The tune step determines how much the frequency changes per arrow key press \
             or mouse wheel click. Set it in the demodulation section. Common values: \
             100 Hz for SSB fine-tuning, 25 kHz for NFM scanning, 200 kHz for FM browsing.",
        ),
        ContentBlock::Callout {
            icon: "💡",
            text: "For broadcast FM (200 kHz channel spacing) set the tune step to 100 kHz \
                   so Left/Right arrows step one full channel at a time.",
        },
    ])
}

fn page_spectrum_controls() -> HandbookPage {
    HandbookPage::new(vec![
        ContentBlock::Heading("Spectrum Display Controls"),
        ContentBlock::Subheading("Reference Level"),
        ContentBlock::Body(
            "Sets the top of the spectrum's Y axis in dBFS (dB relative to full scale). \
             Drag the top ruler in the spectrum view, or use the Ref Level knob. \
             Default is −30 dBFS. Lower values reveal weak signals at the cost of \
             clipping strong ones at the top.",
        ),
        ContentBlock::Subheading("Dynamic Range"),
        ContentBlock::Body(
            "How many decibels of signal range the spectrum Y axis spans. \
             A wider range (e.g. 80 dB) shows the noise floor and strong signals together; \
             a tighter range (e.g. 40 dB) expands the detail around the signals of interest.",
        ),
        ContentBlock::Subheading("FFT Size"),
        ContentBlock::BulletList(&[
            "512 — fastest update, coarsest frequency resolution (~4 kHz/bin at 2 MSps)",
            "1024 — good balance for most use",
            "2048 — default; clear enough to see 25 kHz NFM channels",
            "4096 — high resolution; useful for spotting narrow digital modes",
            "8192 — maximum detail; significant CPU cost at high sample rates",
        ]),
        ContentBlock::Subheading("FFT Averaging"),
        ContentBlock::Body(
            "Exponential moving average applied to the FFT output. Higher values produce \
             a smoother, less noisy trace at the expense of response time. \
             1 = no averaging (raw, noisy); 8 = heavily smoothed (default 4).",
        ),
        ContentBlock::Subheading("Waterfall Speed"),
        ContentBlock::Body(
            "Controls how fast new rows scroll into the waterfall. \
             At 1.0× one FFT frame = one row. At 2.0× rows are added twice as fast. \
             Slow down to observe rapid transmissions; speed up for continuous monitoring.",
        ),
        ContentBlock::Subheading("Waterfall Level"),
        ContentBlock::Body(
            "The dBFS floor for waterfall colour mapping — the value that maps to the \
             darkest colour in the palette. Drag left to reveal weaker signals; \
             drag right if the background is too bright.",
        ),
    ])
}

fn page_gain_controls() -> HandbookPage {
    HandbookPage::new(vec![
        ContentBlock::Heading("Gain Controls"),
        ContentBlock::Body(
            "Gain controls appear in the left panel's device section. \
             They are only relevant when AGC is disabled.",
        ),
        ContentBlock::Subheading("AGC (Automatic Gain Control)"),
        ContentBlock::Body(
            "When enabled, the receiver automatically adjusts its amplification to \
             maintain a target signal level. This is the recommended setting for \
             general use — it prevents both overloading on strong signals and losing \
             weak signals when the antenna is repositioned.",
        ),
        ContentBlock::Subheading("LNA State"),
        ContentBlock::Body(
            "Controls the Low Noise Amplifier at the antenna input. \
             The RSPdx-R2 has states 0 through 9. Lower state number = higher gain. \
             Typical starting point: state 4.",
        ),
        ContentBlock::BulletList(&[
            "State 0–2: maximum gain — use for very weak/distant signals",
            "State 3–5: moderate gain — good for most local signals",
            "State 6–9: minimum gain — for strong local transmitters or rooftop antenna",
        ]),
        ContentBlock::Subheading("IF Gain"),
        ContentBlock::Body(
            "Fine-tunes the intermediate frequency amplifier. Range is −59 to 0 dBFS. \
             Leave at 0 unless you need to fine-tune the signal level after setting LNA state.",
        ),
        ContentBlock::Subheading("AGC Setpoint"),
        ContentBlock::Body(
            "When AGC is enabled, this is the target signal level in dBFS. \
             Default −30 dBFS. Lower values (more negative) cause the AGC to run \
             at higher gain, which can increase noise but helps with weak signals.",
        ),
        ContentBlock::Callout {
            icon: "⚠",
            text: "The ADC SAT warning in the status bar means the signal is overloading \
                   the analogue-to-digital converter. Increase the LNA State number \
                   immediately — ADC clipping causes wideband interference that affects \
                   every frequency, not just the strong one.",
        },
    ])
}

fn page_audio_controls() -> HandbookPage {
    HandbookPage::new(vec![
        ContentBlock::Heading("Audio Controls"),
        ContentBlock::Subheading("Volume Slider"),
        ContentBlock::Body(
            "Linear 0–100% volume control in the right panel. This is independent of \
             macOS system volume — you can mute the system and still control app volume \
             separately, or use MIDI CC 7 on the nanoKontrol2 to adjust it remotely.",
        ),
        ContentBlock::Subheading("VU Meter"),
        ContentBlock::Body(
            "The bar above the volume slider shows the instantaneous audio peak level. \
             It should bounce rhythmically with speech or music. If it stays at zero, \
             check that a signal is received, the mode is correct, and volume is not zero.",
        ),
        ContentBlock::Subheading("De-emphasis"),
        ContentBlock::Body(
            "FM broadcast uses a 75 µs pre-emphasis on audio at the transmitter to \
             reduce noise on high frequencies. The app applies a matching 75 µs \
             de-emphasis filter. This is automatic and cannot be disabled.",
        ),
        ContentBlock::Subheading("Demodulation Modes — Quick Reference"),
        ContentBlock::BulletList(&[
            "WBFM — Wide-Band FM: broadcast radio, stereo, RDS. 200 kHz bandwidth.",
            "NFM — Narrow-Band FM: repeaters, commercial two-way. 12.5 or 25 kHz.",
            "AM — Amplitude Modulation: aviation, shortwave broadcast, MW.",
            "USB — Upper Single Sideband: HF amateur 10–20m, marine, aeronautical.",
            "LSB — Lower Single Sideband: HF amateur 40–160m.",
            "CW — Morse code with adjustable BFO offset.",
        ]),
    ])
}

fn page_recorder_controls() -> HandbookPage {
    HandbookPage::new(vec![
        ContentBlock::Heading("Recorder Controls"),
        ContentBlock::Image {
            key: "right_panel",
            caption: Some("Recorder section — in the right panel below band presets"),
        },
        ContentBlock::Subheading("Manual recording"),
        ContentBlock::Body(
            "Click Record to start capturing the demodulated audio to a WAV file. \
             The file is saved in your Documents/SDRApp Recordings/ folder with a \
             timestamp filename. Click Stop Recording to end the capture.",
        ),
        ContentBlock::Subheading("Scheduled recording"),
        ContentBlock::Body(
            "Set a delay (seconds until start) and a duration (seconds to record), \
             then click Schedule. The app will begin recording after the delay and \
             automatically stop when the duration elapses. Useful for capturing a \
             scheduled broadcast.",
        ),
        ContentBlock::BulletList(&[
            "Delay: 0 seconds starts immediately",
            "Duration: 0 seconds records until you manually stop",
            "The REC badge in the status bar flashes red during recording",
        ]),
        ContentBlock::Callout {
            icon: "💡",
            text: "Recordings capture the demodulated audio — the same signal you hear. \
                   For IQ recordings (raw spectrum data), a future version of the app \
                   will add an IQ capture mode.",
        },
    ])
}

fn page_bookmark_scanner() -> HandbookPage {
    HandbookPage::new(vec![
        ContentBlock::Heading("Bookmarks & Scanner"),
        ContentBlock::Subheading("Bookmarks"),
        ContentBlock::Body(
            "Bookmarks save a frequency, demodulation mode, and optional name so you can \
             return to favourite stations instantly. NFM bookmarks also store bandwidth, \
             squelch threshold, and CTCSS setting — recalling one fully restores the \
             receiver without any manual adjustment.",
        ),
        ContentBlock::BulletList(&[
            "Click Add Bookmark to save the current frequency, mode, and (for NFM) receiver settings",
            "Click a bookmark entry to tune directly to it — NFM settings are applied automatically",
            "Right-click a bookmark to rename, edit, or delete it",
            "Filter the list by category using the filter box at the top",
            "Export/Import bookmarks as CSV for sharing or backup",
            "Set 'Bookmarks file' in config to load bookmarks from an external CSV at startup",
        ]),
        ContentBlock::Subheading("Scanner"),
        ContentBlock::Body(
            "The scanner automatically cycles through your bookmarks, pausing on each \
             one for the configured dwell time and stopping when squelch opens \
             (a signal is detected above the threshold).",
        ),
        ContentBlock::BulletList(&[
            "Dwell time: how long to listen on each bookmark before moving to the next",
            "Category filter: restrict scanning to bookmarks in a specific category",
            "Start/Stop Scanner buttons appear in the scan section of the left panel",
        ]),
        ContentBlock::Callout {
            icon: "💡",
            text: "Build a bookmark list of your local NFM repeaters and run the scanner \
                   to monitor activity across all of them simultaneously — it stops \
                   automatically whenever someone transmits.",
        },
    ])
}
