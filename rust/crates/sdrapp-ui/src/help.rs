#![forbid(unsafe_code)]

//! Integrated help panel: tabbed reference covering antennas, signals,
//! keyboard controls, and a live MIDI binding map.

use egui::{RichText, Ui};

use crate::theme;

// ── Content types ────────────────────────────────────────────────────────────

/// One row in the antenna frequency guide.
struct AntennaRow {
    band: &'static str,
    freq_range: &'static str,
    antenna: &'static str,
    notes: &'static str,
}

const ANTENNA_TABLE: &[AntennaRow] = &[
    AntennaRow {
        band: "AM Broadcast",
        freq_range: "530 kHz – 1.7 MHz",
        antenna: "Long wire (10–30 m) or ferrite loop",
        notes: "Long horizontal wire on RSPdx Antenna A. Indoor ferrite loop also works well.",
    },
    AntennaRow {
        band: "Shortwave / HF",
        freq_range: "1.7 – 30 MHz",
        antenna: "Long wire, dipole, or end-fed half-wave",
        notes: "Longer is better. Avoid coax runs > 10 m without a balun.",
    },
    AntennaRow {
        band: "Aviation (VHF)",
        freq_range: "108 – 137 MHz",
        antenna: "1/4-wave vertical (~52 cm), mag-mount",
        notes: "AM-mode for VOR/ILS below 118 MHz. NFM for voice above.",
    },
    AntennaRow {
        band: "FM Broadcast",
        freq_range: "87.5 – 108 MHz",
        antenna: "Dipole or discone",
        notes: "1/2-wave dipole = 138 cm total length. WBFM mode, 200 kHz channel spacing.",
    },
    AntennaRow {
        band: "Marine VHF",
        freq_range: "156 – 174 MHz",
        antenna: "1/4-wave vertical (~46 cm) or discone",
        notes: "NFM, 25 kHz channels. Channel 16 (156.8 MHz) = distress calling channel.",
    },
    AntennaRow {
        band: "2m Amateur",
        freq_range: "144 – 148 MHz",
        antenna: "1/4-wave vertical (~49 cm) or Yagi",
        notes: "NFM. Most repeaters on 144.3–147.99 MHz. Squelch around -70 dBFS.",
    },
    AntennaRow {
        band: "PMR / Business",
        freq_range: "446 – 470 MHz",
        antenna: "1/4-wave vertical (~16 cm) or discone",
        notes: "NFM, 12.5 kHz channels. Includes emergency services and PMR446.",
    },
    AntennaRow {
        band: "70cm Amateur / ADS-B",
        freq_range: "430 – 440 MHz / 1090 MHz",
        antenna: "Vertical or discone",
        notes: "ADS-B at 1090 MHz benefits from a collinear or helical antenna.",
    },
    AntennaRow {
        band: "GPS / GNSS",
        freq_range: "1575 MHz (L1)",
        antenna: "Active patch antenna with LNA",
        notes: "Requires antenna with built-in LNA. RSPdx Antenna C preferred for L-band.",
    },
];

/// One keyboard shortcut row.
struct KeyRow {
    key: &'static str,
    action: &'static str,
}

const KEY_TABLE: &[KeyRow] = &[
    KeyRow { key: "↑ / ↓", action: "Tune frequency by step size" },
    KeyRow { key: "← / →", action: "Tune frequency by 10× step size" },
    KeyRow { key: "Scroll on spectrum", action: "Tune frequency by step size" },
    KeyRow { key: "Ctrl+Scroll", action: "Zoom spectrum in/out" },
    KeyRow { key: "Click spectrum", action: "Click-to-tune: retune to clicked frequency" },
    KeyRow { key: "Click waterfall", action: "Click-to-tune on waterfall history" },
    KeyRow { key: "?", action: "Open/close this help panel" },
];

const PAGE_NAMES: &[&str] = &["Tune", "Display", "Record"];

// ── Panel widget ─────────────────────────────────────────────────────────────

/// Help panel state: which tab is active.
#[derive(Default, Clone, Copy, PartialEq, Eq)]
pub enum HelpTab {
    #[default]
    Overview,
    Antennas,
    Signals,
    Controls,
    MidiMap,
}

pub struct HelpPanel {
    pub tab: HelpTab,
}

impl Default for HelpPanel {
    fn default() -> Self {
        Self { tab: HelpTab::Overview }
    }
}

impl HelpPanel {
    /// Draw the help panel as a floating egui::Window.
    ///
    /// `open` is read/written to control visibility.
    /// `midi_bindings` is a list of (page, key_name, action_name) from the live config.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        open: &mut bool,
        midi_bindings: &[(usize, String, String)],
    ) {
        if !*open {
            return;
        }

        egui::Window::new("SDR App Help")
            .id(egui::Id::new("help_panel"))
            .resizable(true)
            .default_size([700.0, 500.0])
            .show(ctx, |ui| {
                // Close button in top-right
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button(RichText::new("✕").color(theme::TEXT_MUTED)).clicked() {
                            *open = false;
                        }
                    });
                });

                // Tab bar
                ui.horizontal(|ui| {
                    for (tab, label) in [
                        (HelpTab::Overview, "Overview"),
                        (HelpTab::Antennas, "Antennas"),
                        (HelpTab::Signals, "Signals"),
                        (HelpTab::Controls, "Controls"),
                        (HelpTab::MidiMap, "MIDI Map"),
                    ] {
                        let selected = self.tab == tab;
                        let text = RichText::new(label).small();
                        let text = if selected {
                            text.color(theme::ACCENT).strong()
                        } else {
                            text.color(theme::TEXT_MUTED)
                        };
                        if ui.selectable_label(selected, text).clicked() {
                            self.tab = tab;
                        }
                        ui.add_space(4.0);
                    }
                });

                ui.separator();
                ui.add_space(4.0);

                egui::ScrollArea::vertical().show(ui, |ui| match self.tab {
                    HelpTab::Overview => self.tab_overview(ui),
                    HelpTab::Antennas => self.tab_antennas(ui),
                    HelpTab::Signals => self.tab_signals(ui),
                    HelpTab::Controls => self.tab_controls(ui),
                    HelpTab::MidiMap => self.tab_midi_map(ui, midi_bindings),
                });
            });
    }

    fn tab_overview(&self, ui: &mut Ui) {
        ui.label(
            RichText::new("SDR App")
                .color(theme::ACCENT)
                .size(16.0)
                .strong(),
        );
        ui.add_space(4.0);
        ui.label(
            RichText::new(
                "A software-defined radio receiver for the SDRplay RSPdx-R2. \
                 Demodulates AM, narrow-band FM, and wide-band FM broadcast. \
                 RDS data (station name, programme type, RadioText) is decoded \
                 from WBFM broadcasts.",
            )
            .color(theme::TEXT_PRIMARY),
        );
        ui.add_space(8.0);

        ui.label(RichText::new("Quick Start").color(theme::ACCENT_DIM).strong());
        ui.add_space(4.0);
        let steps = [
            "1. Connect antenna to the appropriate port (see Antennas tab).",
            "2. Press ▶ Start in the left panel to begin receiving.",
            "3. Tune to a frequency: click the frequency display and type, \
               scroll the spectrum, or use ↑↓ arrow keys.",
            "4. Select demod mode: WBFM for FM broadcasts (88–108 MHz), \
               NFM for voice (aviation, marine, amateur), AM for broadcast below 1.7 MHz.",
            "5. Adjust squelch in NFM mode to gate out noise between transmissions.",
            "6. Press ● Record in the right panel to save audio.",
        ];
        for step in &steps {
            ui.label(RichText::new(*step).color(theme::TEXT_PRIMARY).small());
            ui.add_space(2.0);
        }

        ui.add_space(8.0);
        ui.label(
            RichText::new("Press ? or use MIDI to toggle this panel.")
                .color(theme::TEXT_MUTED)
                .small(),
        );
    }

    fn tab_antennas(&self, ui: &mut Ui) {
        ui.label(
            RichText::new("Antenna Selection by Frequency Band")
                .color(theme::ACCENT_DIM)
                .strong(),
        );
        ui.add_space(4.0);
        ui.label(
            RichText::new(
                "The RSPdx-R2 has three antenna ports. Antenna A covers \
                 the widest range (1 kHz – 2 GHz). Antenna B/C are for \
                 specific use cases per the SDRplay documentation.",
            )
            .color(theme::TEXT_MUTED)
            .small(),
        );
        ui.add_space(6.0);

        egui::Grid::new("antenna_grid")
            .num_columns(4)
            .striped(true)
            .spacing([8.0, 4.0])
            .show(ui, |ui| {
                // Header
                for header in ["Band", "Frequency", "Antenna", "Notes"] {
                    ui.label(RichText::new(header).color(theme::ACCENT_DIM).small().strong());
                }
                ui.end_row();

                for row in ANTENNA_TABLE {
                    ui.label(RichText::new(row.band).color(theme::TEXT_PRIMARY).small());
                    ui.label(RichText::new(row.freq_range).color(theme::ACCENT_DIM).small());
                    ui.label(RichText::new(row.antenna).color(theme::TEXT_PRIMARY).small());
                    ui.label(RichText::new(row.notes).color(theme::TEXT_MUTED).small());
                    ui.end_row();
                }
            });

        ui.add_space(8.0);
        ui.label(
            RichText::new(
                "Tip: A discone antenna covers 25 MHz – 1.3 GHz omnidirectionally \
                 and is the best all-round choice for scanning across multiple bands.",
            )
            .color(theme::TEXT_MUTED)
            .small(),
        );
    }

    fn tab_signals(&self, ui: &mut Ui) {
        ui.label(
            RichText::new("Demodulation Modes").color(theme::ACCENT_DIM).strong(),
        );
        ui.add_space(4.0);

        let modes = [
            ("WBFM — Wideband FM", "FM broadcast stations (88–108 MHz). 75 kHz deviation, \
              200 kHz channel spacing. Includes stereo pilot (19 kHz) and RDS data \
              subcarrier (57 kHz). Expect: music, speech, RDS station name and RadioText."),
            ("NFM — Narrow FM", "Voice communications: aviation (118–137 MHz, AM!), marine \
              VHF (156–174 MHz), amateur 2m/70cm, PMR446, emergency services. 12.5–25 kHz \
              channel spacing. Use squelch to gate out noise between transmissions."),
            ("AM — Amplitude Modulation", "AM broadcast (530 kHz – 1.7 MHz), shortwave, \
              aviation voice (108–137 MHz). Envelope detector — signals appear as two \
              sidebands symmetric around the carrier on the spectrum."),
        ];
        for (title, desc) in &modes {
            ui.label(RichText::new(*title).color(theme::TEXT_PRIMARY).strong().small());
            ui.label(RichText::new(*desc).color(theme::TEXT_MUTED).small());
            ui.add_space(6.0);
        }

        ui.separator();
        ui.add_space(4.0);

        ui.label(
            RichText::new("Reading the Spectrum & Waterfall").color(theme::ACCENT_DIM).strong(),
        );
        ui.add_space(4.0);
        let waterfall_tips = [
            "The spectrum (top) shows signal power in dBFS at each frequency. Higher = stronger signal.",
            "The waterfall (bottom) shows time history — each row is one FFT frame. Brighter = stronger signal.",
            "A carrier appears as a sharp vertical spike. Modulated audio creates a band of width proportional to deviation.",
            "FM broadcast stations occupy ~±100 kHz around the carrier (visible as a wide band on the spectrum).",
            "RDS data creates a small cluster of peaks at ±57 kHz from the carrier.",
            "Scroll the spectrum to tune frequency. Ctrl+Scroll to zoom. Click anywhere to retune to that frequency.",
        ];
        for tip in &waterfall_tips {
            ui.label(RichText::new(format!("• {tip}")).color(theme::TEXT_PRIMARY).small());
            ui.add_space(2.0);
        }

        ui.add_space(6.0);
        ui.separator();
        ui.add_space(4.0);

        ui.label(
            RichText::new("RDS (Radio Data System)").color(theme::ACCENT_DIM).strong(),
        );
        ui.add_space(4.0);
        let rds_info = [
            "PS Name (Programme Service): up to 8 characters — the station's short name (e.g. 'BBC R4  ').",
            "PTY (Programme Type): genre code 0–31. Code 1 = News, 4 = Sport, 10 = Pop, etc.",
            "TA (Traffic Announcement): amber badge appears when a live traffic report is in progress.",
            "RT (RadioText): up to 64 characters of scrolling text — song title, show name, phone-in numbers.",
            "RDS data is reset when you retune or change demod mode. Allow 2–3 seconds for reassembly.",
        ];
        for info in &rds_info {
            ui.label(RichText::new(format!("• {info}")).color(theme::TEXT_PRIMARY).small());
            ui.add_space(2.0);
        }
    }

    fn tab_controls(&self, ui: &mut Ui) {
        ui.label(
            RichText::new("Keyboard Shortcuts").color(theme::ACCENT_DIM).strong(),
        );
        ui.add_space(4.0);

        egui::Grid::new("key_grid")
            .num_columns(2)
            .striped(true)
            .spacing([16.0, 4.0])
            .show(ui, |ui| {
                for row in KEY_TABLE {
                    ui.label(RichText::new(row.key).color(theme::ACCENT).small().strong());
                    ui.label(RichText::new(row.action).color(theme::TEXT_PRIMARY).small());
                    ui.end_row();
                }
            });

        ui.add_space(8.0);
        ui.label(RichText::new("Tuning Tips").color(theme::ACCENT_DIM).strong());
        ui.add_space(4.0);
        let tips = [
            "Scroll step size is shown below the frequency display — click to cycle: 100 Hz → 1 kHz → 10 kHz → 100 kHz.",
            "Left/Right arrows step at 10× the current step size for faster tuning.",
            "Band presets (right panel) jump to common broadcast bands with appropriate span.",
            "Bookmarks (left panel) save and recall your favourite frequencies with demod mode.",
            "The MIDI nanoKontrol2 gives hardware control over all tuning, zoom, and recording functions.",
        ];
        for tip in &tips {
            ui.label(RichText::new(format!("• {tip}")).color(theme::TEXT_PRIMARY).small());
            ui.add_space(2.0);
        }
    }

    fn tab_midi_map(&self, ui: &mut Ui, bindings: &[(usize, String, String)]) {
        ui.label(
            RichText::new("nanoKontrol2 MIDI Binding Map").color(theme::ACCENT_DIM).strong(),
        );
        ui.add_space(2.0);
        ui.label(
            RichText::new(
                "Three pages share all 47 controls. Press CYCLE (transport bar) to advance the page.",
            )
            .color(theme::TEXT_MUTED)
            .small(),
        );
        ui.add_space(6.0);

        if bindings.is_empty() {
            ui.label(
                RichText::new("No MIDI bindings loaded.")
                    .color(theme::TEXT_MUTED)
                    .small(),
            );
            return;
        }

        for (page_idx, page_name) in PAGE_NAMES.iter().enumerate() {
            let page_bindings: Vec<_> = bindings
                .iter()
                .filter(|(p, _, _)| *p == page_idx)
                .collect();

            ui.collapsing(
                RichText::new(format!("Page {}: {}", page_idx, page_name))
                    .color(theme::ACCENT)
                    .strong()
                    .small(),
                |ui| {
                    egui::Grid::new(format!("midi_grid_{page_idx}"))
                        .num_columns(2)
                        .striped(true)
                        .spacing([16.0, 3.0])
                        .show(ui, |ui| {
                            for (_, key_name, action_name) in &page_bindings {
                                ui.label(
                                    RichText::new(key_name.as_str())
                                        .color(theme::ACCENT_DIM)
                                        .small()
                                        .strong(),
                                );
                                ui.label(
                                    RichText::new(action_name.as_str())
                                        .color(theme::TEXT_PRIMARY)
                                        .small(),
                                );
                                ui.end_row();
                            }
                        });
                },
            );
            ui.add_space(4.0);
        }

        ui.add_space(4.0);
        ui.label(
            RichText::new(
                "Transport buttons (Play, Stop, Cycle, Record) apply on all pages.",
            )
            .color(theme::TEXT_MUTED)
            .small(),
        );
    }
}
