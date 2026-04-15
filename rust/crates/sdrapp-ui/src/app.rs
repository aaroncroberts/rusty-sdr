#![forbid(unsafe_code)]

//! Top-level eframe application.
//!
//! Layout:
//!   ┌────────────────┬────────────────────────────────┬──────────────────┐
//!   │  Left panel    │   Spectrum (center)             │  Right panel     │
//!   │  · Source      │   Waterfall                     │  · Volume / VU   │
//!   │  · Frequency   │                                 │  · Band presets  │
//!   │  · Start/Stop  │                                 │  · Recorder      │
//!   │  · Status      │                                 │  · MIDI status   │
//!   ├────────────────┴────────────────────────────────┴──────────────────┤
//!   │  Status bar: device info · MIDI page · buffer health · recording   │
//!   └────────────────────────────────────────────────────────────────────┘

use parking_lot::RwLock;
use std::sync::Arc;

use egui::{Color32, RichText, Stroke, Ui, Vec2};

use sdrapp_core::{
    config::{AppConfig, BookmarkConfig},
    registry::ModuleRegistry,
    signal_path::{DemodMode, RecordingMode, SharedState, SignalPathCommand},
};
use sdrapp_recorder::RecorderCommand;

use crate::{
    frequency::FrequencyWidget,
    help::HelpPanel,
    spectrum::SpectrumWidget,
    theme,
    waterfall::WaterfallWidget,
};

/// Band preset: name, center frequency in Hz, span in Hz.
struct BandPreset {
    name: &'static str,
    center_hz: u64,
    span_hz: u64,
}

const BAND_PRESETS: &[BandPreset] = &[
    BandPreset {
        name: "FM Broadcast",
        center_hz: 97_500_000,
        span_hz: 10_500_000,
    },
    BandPreset {
        name: "Aviation VOR",
        center_hz: 113_000_000,
        span_hz: 5_000_000,
    },
    BandPreset {
        name: "Air Traffic",
        center_hz: 127_500_000,
        span_hz: 9_500_000,
    },
    BandPreset {
        name: "NOAA Weather",
        center_hz: 162_400_000,
        span_hz: 500_000,
    },
    BandPreset {
        name: "AIS Marine",
        center_hz: 161_975_000,
        span_hz: 500_000,
    },
    BandPreset {
        name: "Ham 2m",
        center_hz: 146_000_000,
        span_hz: 4_000_000,
    },
    BandPreset {
        name: "ISM 433 MHz",
        center_hz: 433_920_000,
        span_hz: 2_000_000,
    },
];

pub struct SdrApp {
    config: AppConfig,
    registry: ModuleRegistry,
    shared: Arc<RwLock<SharedState>>,
    cmd_tx: crossbeam_channel::Sender<SignalPathCommand>,
    recorder_cmd_tx: tokio::sync::mpsc::Sender<RecorderCommand>,
    frequency_widget: FrequencyWidget,
    waterfall: WaterfallWidget,
    /// Dirty flag — save config at next opportunity
    config_dirty: bool,
    /// Simulated audio peak level [0.0, 1.0] for VU meter (updated each frame)
    vu_peak: f32,
    /// Peak-hold buffer: tracks per-bin maximum with slow decay
    peak_hold: Vec<f32>,
    // ── Display range ─────────────────────────────────────────────────────────
    /// Top of the spectrum display in dBFS (like "max" or "ref level" in SDR# / GQRX).
    /// db_ceil = ref_level;  db_floor = ref_level - dyn_range.
    ref_level: f32,
    /// How many dB of range to show (vertical span of spectrum/waterfall).
    dyn_range: f32,
    /// When true, ref_level tracks the signal ceiling automatically.
    auto_ref: bool,
    /// Waterfall brightness offset (positive = brighter / more sensitive).
    /// Applied only to waterfall colourmap; does not affect spectrum.
    wf_gain: f32,
    /// Slow EMA of 10th-percentile FFT bin — noise floor estimate for auto-ref.
    noise_floor_ema: f32,
    /// Slow EMA of 99th-percentile FFT bin — signal ceiling estimate for auto-ref.
    signal_ceil_ema: f32,
    /// Fractional row accumulator for waterfall speed control.
    /// Incremented by waterfall_speed each frame; push_row fires once per integer crossed.
    waterfall_row_frac: f32,
    /// Help panel widget (tabs state).
    help_panel: HelpPanel,
    /// Live MIDI bindings for the help panel MIDI Map tab.
    /// Populated at construction from the nanoKontrol2 default profile.
    midi_bindings: Vec<(usize, String, String)>,
    /// Scheduled recording: delay before start (seconds).
    sched_delay_secs: u32,
    /// Scheduled recording: duration (seconds).
    sched_duration_secs: u32,
}

impl SdrApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        config: AppConfig,
        shared: Arc<RwLock<SharedState>>,
        cmd_tx: crossbeam_channel::Sender<SignalPathCommand>,
        recorder_cmd_tx: tokio::sync::mpsc::Sender<RecorderCommand>,
        midi_bindings: Vec<(usize, String, String)>,
    ) -> Self {
        // Apply our beautiful dark theme
        theme::apply(&cc.egui_ctx);

        let freq = config.ui.frequency_hz;
        Self {
            registry: ModuleRegistry::new(),
            frequency_widget: FrequencyWidget::new(freq),
            waterfall: WaterfallWidget::new_with_colormap(1024, (-120.0, 0.0)),
            config,
            shared,
            cmd_tx,
            recorder_cmd_tx,
            config_dirty: false,
            vu_peak: 0.0,
            peak_hold: Vec::new(),
            ref_level: -30.0,
            dyn_range: 60.0,
            auto_ref: true,
            wf_gain: 10.0,
            noise_floor_ema: -85.0,
            signal_ceil_ema: -40.0,
            waterfall_row_frac: 0.0,
            help_panel: HelpPanel::default(),
            midi_bindings,
            sched_delay_secs: 0,
            sched_duration_secs: 60,
        }
    }

    // ── Left Panel ───────────────────────────────────────────────────────────

    fn left_panel(&mut self, ui: &mut Ui) {
        ui.add_space(4.0);

        // App title with version
        ui.horizontal(|ui| {
            ui.add_space(2.0);
            ui.label(
                RichText::new("◉  SDR App")
                    .color(theme::ACCENT)
                    .size(15.0)
                    .strong(),
            );
        });

        ui.add_space(4.0);
        ui.separator();
        ui.add_space(4.0);

        // ── Source section ────────────────────────────────────────────────────
        ui.label(RichText::new("SOURCE").color(theme::TEXT_MUTED).small());
        ui.add_space(4.0);

        let source_name = self.shared.read().source_name.clone();
        let is_demo = source_name
            .as_deref()
            .map(|n| n.contains("Demo"))
            .unwrap_or(false);

        let display_name = source_name
            .as_deref()
            .or_else(|| {
                self.registry
                    .sources
                    .first()
                    .map(|s| s.display_name)
            })
            .unwrap_or("No device");

        ui.horizontal(|ui| {
            let (icon, color) = if is_demo {
                ("⚠", theme::DANGER)
            } else {
                ("◈", theme::ACCENT_DIM)
            };
            ui.label(RichText::new(icon).color(color));
            ui.label(RichText::new(display_name).color(theme::TEXT_PRIMARY));
        });

        if is_demo {
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                ui.add_space(14.0);
                ui.label(
                    RichText::new("no hardware connected")
                        .color(theme::DANGER)
                        .small(),
                );
            });
        }

        ui.add_space(6.0);

        // Start / Stop button
        let is_running = self.shared.read().is_running;
        let (btn_text, btn_color) = if is_running {
            ("■  Stop", theme::DANGER)
        } else {
            ("▶  Start", theme::STATUS_OK)
        };

        let btn = egui::Button::new(RichText::new(btn_text).color(btn_color).strong())
            .fill(theme::WIDGET_BG)
            .stroke(Stroke::new(1.0, btn_color));

        if ui
            .add_sized(Vec2::new(ui.available_width(), 28.0), btn)
            .clicked()
            && is_running
        {
            let _ = self.cmd_tx.try_send(SignalPathCommand::Stop);
            // Start is handled by main.rs wiring the signal path; button is a placeholder
        }

        ui.add_space(8.0);
        ui.separator();
        ui.add_space(6.0);

        // ── Frequency section ─────────────────────────────────────────────────
        ui.label(RichText::new("FREQUENCY").color(theme::TEXT_MUTED).small());
        ui.add_space(4.0);

        let (_, new_freq) = self.frequency_widget.show(ui);
        if let Some(freq) = new_freq {
            let _ = self.cmd_tx.try_send(SignalPathCommand::SetFrequency(freq));
            self.config.ui.frequency_hz = freq;
            self.config_dirty = true;
        }

        // ── Tuning step controls ──────────────────────────────────────────────
        let step_hz = self.shared.read().tune_step_hz;
        ui.add_space(4.0);

        // Step size selector row
        ui.label(RichText::new("STEP").color(theme::TEXT_MUTED).small());
        ui.add_space(2.0);
        ui.horizontal_wrapped(|ui| {
            for (hz, label) in [
                (100_u64,       "100 Hz"),
                (1_000,         "1 kHz"),
                (10_000,        "10 kHz"),
                (100_000,       "100 kHz"),
                (1_000_000,     "1 MHz"),
                (10_000_000,    "10 MHz"),
            ] {
                let selected = step_hz == hz;
                let text = RichText::new(label).small();
                let text = if selected { text.color(theme::ACCENT).strong() } else { text.color(theme::TEXT_MUTED) };
                if ui.selectable_label(selected, text).clicked() {
                    let _ = self.cmd_tx.try_send(SignalPathCommand::SetTuneStep(hz));
                }
            }
        });

        // Nudge buttons: ◄◄ ◄ ► ►► (×10 / ×1 step)
        ui.add_space(4.0);
        let freq = self.config.ui.frequency_hz;
        ui.horizontal(|ui| {
            let btn_w = (ui.available_width() - 16.0) / 4.0;
            for (label, delta, tip) in [
                ("◄◄", -(step_hz as i64 * 10), "−10 × step"),
                ("◄",  -(step_hz as i64),       "−1 × step  (or ↓ / ↑ arrow keys)"),
                ("►",   step_hz as i64,          "+1 × step  (or ↑ arrow key)"),
                ("►►",  step_hz as i64 * 10,    "+10 × step"),
            ] {
                if ui.add_sized(
                    Vec2::new(btn_w, 22.0),
                    egui::Button::new(RichText::new(label).color(theme::TEXT_PRIMARY))
                        .fill(theme::WIDGET_BG),
                ).on_hover_text(tip).clicked() {
                    let new_freq = (freq as i64 + delta).max(1) as u64;
                    let _ = self.cmd_tx.try_send(SignalPathCommand::SetFrequency(new_freq));
                    self.config.ui.frequency_hz = new_freq;
                    self.frequency_widget = FrequencyWidget::new(new_freq);
                    self.config_dirty = true;
                }
            }
        });

        ui.add_space(8.0);
        ui.separator();
        ui.add_space(6.0);

        // ── Demod mode ────────────────────────────────────────────────────────
        ui.label(RichText::new("DEMOD MODE").color(theme::TEXT_MUTED).small());
        ui.add_space(4.0);

        let current_mode = self.shared.read().demod_mode;
        ui.horizontal(|ui| {
            for (mode, label, tooltip) in [
                (DemodMode::Wbfm, "WBFM", "Wideband FM — FM broadcast stations (88–108 MHz). 75 kHz deviation, stereo, RDS."),
                (DemodMode::Nfm, "NFM", "Narrow FM — voice comms (aviation, marine, amateur, PMR). 12.5–25 kHz channels. Enable squelch."),
                (DemodMode::Am, "AM", "Amplitude Modulation — AM broadcast (530 kHz–1.7 MHz), shortwave, aviation voice."),
            ] {
                let selected = current_mode == mode;
                let text = RichText::new(label).small();
                let text = if selected {
                    text.color(theme::ACCENT).strong()
                } else {
                    text.color(theme::TEXT_MUTED)
                };
                if ui.selectable_label(selected, text).on_hover_text(tooltip).clicked() && !selected {
                    let _ = self.cmd_tx.try_send(SignalPathCommand::SetDemodMode(mode));
                }
            }
        });

        // ── NFM squelch & settings ────────────────────────────────────────────
        if current_mode == DemodMode::Nfm {
            ui.add_space(8.0);
            ui.separator();
            ui.add_space(6.0);

            ui.label(RichText::new("NFM SETTINGS").color(theme::TEXT_MUTED).small());
            ui.add_space(4.0);

            // Channel bandwidth selector (12.5 / 25 kHz)
            let nfm_bw = self.shared.read().nfm_bandwidth_hz;
            ui.horizontal(|ui| {
                ui.label(RichText::new("BW").color(theme::TEXT_MUTED).small());
                for (bw, label) in [(12_500u32, "12.5k"), (25_000u32, "25k")] {
                    let selected = nfm_bw == bw;
                    let text = RichText::new(label).small();
                    let text = if selected { text.color(theme::ACCENT).strong() } else { text.color(theme::TEXT_MUTED) };
                    if ui.selectable_label(selected, text)
                        .on_hover_text("NFM channel bandwidth. 12.5 kHz for modern PMR/amateur, 25 kHz for legacy systems.")
                        .clicked() && !selected
                    {
                        let _ = self.cmd_tx.try_send(SignalPathCommand::SetNfmBandwidth(bw));
                        self.config.ui.nfm_bandwidth_hz = bw;
                        self.config_dirty = true;
                    }
                }
            });

            ui.add_space(4.0);

            // Squelch threshold
            ui.label(RichText::new("SQUELCH").color(theme::TEXT_MUTED).small());
            ui.add_space(2.0);

            let mut sq_threshold = self.shared.read().squelch_threshold;
            let sq_label = format!("{:.0} dBFS", sq_threshold);
            ui.label(RichText::new(&sq_label).color(theme::TEXT_PRIMARY).small());
            let sq_slider = egui::Slider::new(&mut sq_threshold, -120.0_f32..=0.0_f32)
                .show_value(false)
                .trailing_fill(true);
            if ui
                .add(sq_slider)
                .on_hover_text(
                    "Squelch gates audio below this signal level (dBFS).\n\
                     Typical NFM voice: -70 to -40 dBFS.\n\
                     Set lower to hear weaker signals; higher to cut noise.",
                )
                .changed()
            {
                let _ = self
                    .cmd_tx
                    .try_send(SignalPathCommand::SetSquelchThreshold(sq_threshold));
            }

            ui.add_space(4.0);

            // CTCSS tone squelch toggle
            let (ctcss_enabled, ctcss_detected) = {
                let s = self.shared.read();
                (s.ctcss_squelch_enabled, s.ctcss_tone_detected)
            };
            ui.horizontal(|ui| {
                let label_color = if ctcss_enabled { theme::ACCENT } else { theme::TEXT_MUTED };
                let ctcss_label = if ctcss_enabled && ctcss_detected {
                    "CTCSS ✓"
                } else if ctcss_enabled {
                    "CTCSS (no tone)"
                } else {
                    "CTCSS off"
                };
                if ui
                    .small_button(RichText::new(ctcss_label).color(label_color))
                    .on_hover_text(
                        "CTCSS tone squelch: mutes audio when no sub-audible tone (67–254 Hz) is detected.\n\
                         Common on repeaters to prevent opening on distant interference.",
                    )
                    .clicked()
                {
                    let new_enabled = !ctcss_enabled;
                    let _ = self.cmd_tx.try_send(SignalPathCommand::SetCtcssEnabled(new_enabled));
                    self.config.ui.ctcss_enabled = new_enabled;
                    self.config_dirty = true;
                }
            });
        }

        ui.add_space(8.0);
        ui.separator();
        ui.add_space(6.0);

        // ── Bookmarks ─────────────────────────────────────────────────────────
        ui.label(RichText::new("BOOKMARKS").color(theme::TEXT_MUTED).small());
        ui.add_space(4.0);

        // Collect bookmark data and determine actions
        let (bookmarks_snapshot, cursor) = {
            let s = self.shared.read();
            (s.bookmarks.clone(), s.bookmark_cursor)
        };
        let mut remove_idx: Option<usize> = None;
        let mut recall_idx: Option<usize> = None;

        for (i, bm) in bookmarks_snapshot.iter().enumerate() {
            let is_active = i == cursor;
            ui.horizontal(|ui| {
                // Recall button (star for active, circle for inactive)
                let icon = if is_active { "★" } else { "☆" };
                let icon_color = if is_active { theme::ACCENT } else { theme::TEXT_MUTED };
                if ui.small_button(RichText::new(icon).color(icon_color)).clicked() {
                    recall_idx = Some(i);
                }
                // Bookmark name (click also recalls)
                let freq_label = format!("{:.3} MHz", bm.freq_hz as f64 / 1_000_000.0);
                let text = format!("{} — {}", bm.name, freq_label);
                if ui
                    .selectable_label(is_active, RichText::new(&text).color(theme::TEXT_PRIMARY).small())
                    .clicked()
                {
                    recall_idx = Some(i);
                }
                // Delete button
                if ui.small_button(RichText::new("×").color(theme::TEXT_MUTED)).clicked() {
                    remove_idx = Some(i);
                }
            });
        }

        // Apply bookmark actions
        if let Some(i) = recall_idx {
            let (bm_freq, bm_mode) = {
                let mut s = self.shared.write();
                s.bookmark_cursor = i;
                let bm = &s.bookmarks[i];
                (bm.freq_hz, bm.mode)
            };
            let _ = self.cmd_tx.try_send(SignalPathCommand::SetFrequency(bm_freq));
            let _ = self.cmd_tx.try_send(SignalPathCommand::SetDemodMode(bm_mode));
            self.config.ui.frequency_hz = bm_freq;
            self.frequency_widget = FrequencyWidget::new(bm_freq);
            self.config_dirty = true;
        }
        if let Some(i) = remove_idx {
            let _ = self.cmd_tx.try_send(SignalPathCommand::RemoveBookmark(i));
            // Mirror to config
            if i < self.config.bookmarks.len() {
                self.config.bookmarks.remove(i);
                self.config_dirty = true;
            }
        }

        // "Save current" button
        ui.add_space(2.0);
        if ui
            .small_button(RichText::new("+ Save current freq").color(theme::ACCENT_DIM))
            .clicked()
        {
            let (freq, mode) = {
                let s = self.shared.read();
                (s.center_freq_hz, s.demod_mode)
            };
            let name = format!("{:.3} MHz", freq as f64 / 1_000_000.0);
            let _ = self.cmd_tx.try_send(SignalPathCommand::AddBookmark(name.clone()));
            // Mirror to config for persistence
            let mode_str = match mode {
                DemodMode::Nfm => "Nfm",
                DemodMode::Am => "Am",
                _ => "Wbfm",
            };
            self.config.bookmarks.push(BookmarkConfig::new(name, freq, mode_str));
            self.config_dirty = true;
        }

        ui.add_space(8.0);
        ui.separator();
        ui.add_space(6.0);

        // ── Device settings ────────────────────────────────────────────────────
        ui.label(
            RichText::new("DEVICE SETTINGS")
                .color(theme::TEXT_MUTED)
                .small(),
        );
        ui.add_space(4.0);

        // Antenna selector
        ui.horizontal(|ui| {
            ui.label(RichText::new("Antenna").color(theme::TEXT_MUTED).small());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                for port in ["A", "B", "C"] {
                    let selected = self.config.source.antenna == port;
                    let label = RichText::new(port).small();
                    let label = if selected {
                        label.color(theme::ACCENT).strong()
                    } else {
                        label.color(theme::TEXT_MUTED)
                    };
                    if ui.selectable_label(selected, label).clicked() && !selected {
                        self.config.source.antenna = port.into();
                        self.config_dirty = true;
                    }
                }
            });
        });

        // Sample rate dropdown
        ui.horizontal(|ui| {
            ui.label(RichText::new("Rate").color(theme::TEXT_MUTED).small());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let rates: &[(u32, &str)] = &[
                    (200_000, "200k"),
                    (500_000, "500k"),
                    (1_000_000, "1M"),
                    (2_000_000, "2M"),
                    (6_000_000, "6M"),
                    (8_000_000, "8M"),
                    (10_000_000, "10M"),
                ];
                let current = rates
                    .iter()
                    .find(|&&(r, _)| r == self.config.source.sample_rate_sps)
                    .map(|&(_, label)| label)
                    .unwrap_or("?");

                egui::ComboBox::from_id_salt("sample_rate")
                    .selected_text(RichText::new(current).small())
                    .width(60.0)
                    .show_ui(ui, |ui| {
                        for &(rate, label) in rates {
                            let selected = rate == self.config.source.sample_rate_sps;
                            if ui.selectable_label(selected, label).clicked() {
                                self.config.source.sample_rate_sps = rate;
                                self.config_dirty = true;
                            }
                        }
                    });
            });
        });

        // AGC toggle
        ui.horizontal(|ui| {
            ui.label(RichText::new("AGC").color(theme::TEXT_MUTED).small());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let agc = &mut self.config.source.agc_enabled;
                let label = if *agc {
                    RichText::new("ON").color(theme::STATUS_OK).small().strong()
                } else {
                    RichText::new("OFF").color(theme::TEXT_MUTED).small()
                };
                if ui.selectable_label(*agc, label).clicked() {
                    *agc = !*agc;
                    self.config_dirty = true;
                }
            });
        });

        // LNA state (only when AGC is off)
        if !self.config.source.agc_enabled {
            ui.horizontal(|ui| {
                ui.label(RichText::new("LNA").color(theme::TEXT_MUTED).small());
                let mut lna = self.config.source.lna_state as i32;
                if ui
                    .add(egui::Slider::new(&mut lna, 0..=9).show_value(true))
                    .changed()
                {
                    self.config.source.lna_state = lna as u8;
                    self.config_dirty = true;
                }
            });
        }

        // Signal path status
        let (center_freq, is_recording, rds_ps_name, rds_pty, rds_ta, rds_rt) = {
            let s = self.shared.read();
            (
                s.center_freq_hz,
                s.is_recording,
                s.rds_ps_name.clone(),
                s.rds_pty.map(|c| sdrapp_core::dsp::rds::pty_to_str(c).to_string()),
                s.rds_ta,
                s.rds_rt.clone(),
            )
        };

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let status_dot_color = if is_running {
                theme::STATUS_OK
            } else {
                theme::TEXT_DISABLED
            };
            ui.label(RichText::new("●").color(status_dot_color));
            ui.label(
                RichText::new(if is_running { "Running" } else { "Stopped" })
                    .color(theme::TEXT_PRIMARY),
            );
        });

        if center_freq > 0 {
            ui.horizontal(|ui| {
                ui.label(RichText::new("⟳").color(theme::TEXT_MUTED));
                ui.label(
                    RichText::new(format_frequency(center_freq))
                        .color(theme::TEXT_MUTED)
                        .small(),
                );
            });
        }

        if rds_ps_name.is_some() || rds_rt.is_some() {
            ui.add_space(4.0);
            ui.separator();
            ui.add_space(2.0);

            // PS name row: "RDS" badge · station name · PTY · TA badge
            ui.horizontal(|ui| {
                ui.label(RichText::new("RDS").color(theme::ACCENT).small().strong());
                if let Some(ref ps) = rds_ps_name {
                    ui.label(RichText::new(ps).color(theme::TEXT_PRIMARY).strong());
                }
                if let Some(ref pty) = rds_pty {
                    ui.label(RichText::new(pty).color(theme::TEXT_MUTED).small());
                }
                if rds_ta {
                    ui.label(RichText::new("TA").color(theme::AMBER).small().strong());
                }
            });

            // RadioText row
            if let Some(ref rt) = rds_rt {
                ui.horizontal(|ui| {
                    let avail = ui.available_width();
                    let rt_display = if rt.len() > 32 {
                        format!("{}…", &rt[..31])
                    } else {
                        rt.clone()
                    };
                    ui.label(
                        RichText::new(rt_display)
                            .color(theme::TEXT_MUTED)
                            .small()
                    )
                    .on_hover_text(rt.as_str());
                    let _ = avail; // suppress unused warning
                });
            }
        }

        if is_recording {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("●").color(theme::DANGER));
                ui.label(RichText::new("Recording").color(theme::AMBER).strong());
            });
        }
    }

    // ── Center Panel ─────────────────────────────────────────────────────────

    fn center_panel(&mut self, ui: &mut Ui) {
        let fft_data = {
            let s = self.shared.read();
            s.fft_magnitudes.clone()
        };

        let freq = self.config.ui.frequency_hz;
        let (span, zoom_level, waterfall_speed) = {
            let s = self.shared.read();
            let sr_half = s.sample_rate_sps as u64 / 2;
            // Apply zoom: zoom_level 1.0 = full hardware bandwidth, 0.05 = tightest zoom.
            // Prefer SharedState zoom when sample rate is known; fall back to config span_hz.
            let effective_span = if sr_half > 0 {
                let z = s.zoom_level.clamp(0.005, 1.0);
                (sr_half as f64 * z as f64) as u64
            } else {
                self.config.ui.span_hz
            };
            (effective_span, s.zoom_level, s.waterfall_speed)
        };

        // Update peak-hold: expand/shrink buffer with FFT size, then take max
        // per bin with a slow decay (≈ -0.5 dB/frame at 30fps = ~15 dB/s)
        let n = fft_data.len();
        if self.peak_hold.len() != n {
            self.peak_hold = vec![-120.0_f32; n];
        }
        let signal_active = fft_data.iter().any(|&v| v > -119.0);
        if signal_active {
            for (ph, &v) in self.peak_hold.iter_mut().zip(fft_data.iter()) {
                if v > *ph {
                    *ph = v;
                } else {
                    *ph -= 0.5; // decay per frame
                }
            }

            // Auto-ref: anchor ref_level to the noise floor so the full signal
            // range stays visible regardless of signal strength.
            // ref_level = noise_floor + dyn_range * 0.9  means the noise floor
            // sits at ~10% from the bottom of the display, and signals up to
            // 90% of dyn_range above the floor remain on-screen.
            if self.auto_ref && n >= 10 {
                let mut sorted = fft_data.to_vec();
                sorted.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                let floor_sample = sorted[n / 10];
                let ceil_sample = sorted[(n * 99 / 100).min(n - 1)];
                const ALPHA: f32 = 0.95;
                self.noise_floor_ema = ALPHA * self.noise_floor_ema + (1.0 - ALPHA) * floor_sample;
                self.signal_ceil_ema = ALPHA * self.signal_ceil_ema + (1.0 - ALPHA) * ceil_sample;
                // Place ref_level so the noise floor is ~10% up from the bottom.
                self.ref_level = (self.noise_floor_ema + self.dyn_range * 0.9).clamp(-120.0, 20.0);
            }
            let db_floor = self.ref_level - self.dyn_range;
            let db_ceil = self.ref_level;

            // Waterfall uses a shifted range for independent brightness control.
            // Positive wf_gain shifts the mapping down, revealing weaker signals.
            self.waterfall.set_db_range((db_floor - self.wf_gain, db_ceil - self.wf_gain));
            // Fractional accumulator: push_row fires once per integer crossed.
            // Speed 1.0 = 1 row/frame, 2.0 = 2 rows/frame, 0.5 = every other frame.
            self.waterfall_row_frac += waterfall_speed.clamp(0.1, 10.0);
            while self.waterfall_row_frac >= 1.0 {
                self.waterfall.push_row(&fft_data);
                self.waterfall_row_frac -= 1.0;
            }
        }

        let db_floor = self.ref_level - self.dyn_range;
        let db_ceil  = self.ref_level;
        let db_range = (db_floor, db_ceil);

        let available_h = ui.available_height();
        // Spectrum gets a fixed portion; waterfall fills the rest
        let spectrum_height = (available_h * 0.36).clamp(120.0, 380.0);

        // ── Spectrum ──────────────────────────────────────────────────────────
        let (spectrum_rect, spectrum_resp) = ui.allocate_exact_size(
            Vec2::new(ui.available_width(), spectrum_height),
            egui::Sense::click(),
        );

        // Click-to-tune: map click X to frequency
        if let Some(click_pos) = spectrum_resp.interact_pointer_pos() {
            if spectrum_resp.clicked() {
                let t =
                    ((click_pos.x - spectrum_rect.left()) / spectrum_rect.width()).clamp(0.0, 1.0);
                let low = freq.saturating_sub(span) as f64;
                let high = freq as f64 + span as f64;
                let new_freq = (low + t as f64 * (high - low)).round() as u64;
                let _ = self
                    .cmd_tx
                    .try_send(SignalPathCommand::SetFrequency(new_freq));
                self.config.ui.frequency_hz = new_freq;
                self.frequency_widget = FrequencyWidget::new(new_freq);
                self.config_dirty = true;
            }
        }

        // Scroll on spectrum: tune frequency (step); Ctrl+scroll: zoom in/out
        let (scroll_delta, ctrl_held) = ui.input(|i| (i.smooth_scroll_delta.y, i.modifiers.ctrl));
        if spectrum_resp.hovered() && scroll_delta.abs() > 0.5 {
            if ctrl_held {
                // Ctrl+scroll → zoom
                let factor = if scroll_delta > 0.0 { 0.8_f32 } else { 1.25_f32 };
                let new_zoom = (zoom_level * factor).clamp(0.005, 1.0);
                let _ = self.cmd_tx.try_send(SignalPathCommand::SetZoom(new_zoom));
                self.config.ui.zoom_level = new_zoom;
                self.config_dirty = true;
            } else {
                // Plain scroll → step-tune frequency
                let step = self.shared.read().tune_step_hz;
                let new_freq = if scroll_delta > 0.0 {
                    freq.saturating_add(step)
                } else {
                    freq.saturating_sub(step).max(1)
                };
                let _ = self.cmd_tx.try_send(SignalPathCommand::SetFrequency(new_freq));
                self.config.ui.frequency_hz = new_freq;
                self.frequency_widget = FrequencyWidget::new(new_freq);
                self.config_dirty = true;
            }
        }

        let ctx = ui.ctx().clone();
        let mut spectrum_ui = ui.new_child(egui::UiBuilder::new().max_rect(spectrum_rect));
        let peak_ref: Option<&[f32]> = if self.peak_hold.len() == fft_data.len() {
            Some(&self.peak_hold)
        } else {
            None
        };
        SpectrumWidget {
            fft_data: &fft_data,
            db_range,
            freq_range: (freq.saturating_sub(span), freq + span),
            vfo_hz: freq,
            peak_hold: peak_ref,
        }
        .show(&mut spectrum_ui);

        // ── Display controls toolbar ──────────────────────────────────────────
        ui.add_space(3.0);

        // Row 1: Ref Level + Auto toggle
        ui.horizontal(|ui| {
            let auto_color = if self.auto_ref { theme::ACCENT } else { theme::TEXT_MUTED };
            if ui.small_button(RichText::new(if self.auto_ref { "Auto ✓" } else { "Auto" }).color(auto_color))
                .on_hover_text("Auto-track signal ceiling (auto reference level)")
                .clicked()
            {
                self.auto_ref = !self.auto_ref;
            }
            ui.label(RichText::new("Ref").color(theme::TEXT_MUTED).small());
            let mut rl = self.ref_level;
            if ui.add(
                egui::Slider::new(&mut rl, -120.0_f32..=20.0_f32)
                    .show_value(true)
                    .suffix(" dB")
                    .integer(),
            ).on_hover_text("Reference level: top of spectrum display (dBFS). Drag down to see weaker signals.").changed() {
                self.ref_level = rl;
                self.auto_ref = false; // manual override disables auto
            }
            ui.label(RichText::new("Range").color(theme::TEXT_MUTED).small());
            let mut dr = self.dyn_range;
            if ui.add(
                egui::Slider::new(&mut dr, 20.0_f32..=160.0_f32)
                    .show_value(true)
                    .suffix(" dB")
                    .integer(),
            ).on_hover_text("Dynamic range: how many dB the spectrum shows. Narrow = high contrast on weak signals.").changed() {
                self.dyn_range = dr;
            }
        });

        // Row 2: WF Gain + Zoom (with bandwidth label) + WF Speed
        ui.add_space(1.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("WF Gain").color(theme::TEXT_MUTED).small())
                .on_hover_text("Waterfall brightness offset — positive reveals weaker signals in the waterfall");
            let mut wg = self.wf_gain;
            if ui.add(
                egui::Slider::new(&mut wg, -40.0_f32..=40.0_f32)
                    .show_value(true)
                    .suffix(" dB")
                    .integer(),
            ).changed() {
                self.wf_gain = wg;
            }

            ui.add_space(6.0);

            // Bandwidth label derived from current zoom
            let bw_hz = (span * 2) as f64;
            let bw_label = if bw_hz >= 1_000_000.0 {
                format!("{:.2} MHz", bw_hz / 1_000_000.0)
            } else {
                format!("{:.0} kHz", bw_hz / 1_000.0)
            };

            ui.label(RichText::new("Zoom").color(theme::TEXT_MUTED).small());
            // [-] slider [+] pattern for fine control
            let z_step = zoom_level * 0.15;
            if ui.small_button(RichText::new("−").color(theme::TEXT_MUTED))
                .on_hover_text("Zoom in (or Ctrl+scroll up on spectrum)")
                .clicked()
            {
                let new_z = (zoom_level - z_step).clamp(0.005, 1.0);
                let _ = self.cmd_tx.try_send(SignalPathCommand::SetZoom(new_z));
                self.config.ui.zoom_level = new_z;
                self.config_dirty = true;
            }
            let mut z = zoom_level;
            if ui.add(
                egui::Slider::new(&mut z, 0.005_f32..=1.0_f32)
                    .show_value(false)
                    .logarithmic(true),
            ).changed() {
                let _ = self.cmd_tx.try_send(SignalPathCommand::SetZoom(z));
                self.config.ui.zoom_level = z;
                self.config_dirty = true;
            }
            if ui.small_button(RichText::new("+").color(theme::TEXT_MUTED))
                .on_hover_text("Zoom out (or Ctrl+scroll down on spectrum)")
                .clicked()
            {
                let new_z = (zoom_level + z_step).clamp(0.005, 1.0);
                let _ = self.cmd_tx.try_send(SignalPathCommand::SetZoom(new_z));
                self.config.ui.zoom_level = new_z;
                self.config_dirty = true;
            }
            ui.label(RichText::new(&bw_label).color(theme::ACCENT).small())
                .on_hover_text("Displayed bandwidth (zoom × hardware bandwidth)");

            ui.add_space(4.0);
            ui.label(RichText::new("WF").color(theme::TEXT_MUTED).small());
            let mut ws = waterfall_speed;
            if ui.add(
                egui::Slider::new(&mut ws, 0.1_f32..=8.0_f32)
                    .show_value(false),
            ).on_hover_text("Waterfall scroll speed").changed() {
                let _ = self.cmd_tx.try_send(SignalPathCommand::SetWaterfallSpeed(ws));
                self.config.ui.waterfall_speed = ws;
                self.config_dirty = true;
            }
        });

        // ── Waterfall ─────────────────────────────────────────────────────────
        let waterfall_resp = self.waterfall.show(ui, &ctx);

        // Click-to-tune on waterfall (same x→Hz conversion as spectrum)
        if let Some(click_pos) = waterfall_resp.interact_pointer_pos() {
            if waterfall_resp.clicked() {
                let rect = waterfall_resp.rect;
                let t = ((click_pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
                let low = freq.saturating_sub(span) as f64;
                let high = freq as f64 + span as f64;
                let new_freq = (low + t as f64 * (high - low)).round() as u64;
                let _ = self.cmd_tx.try_send(SignalPathCommand::SetFrequency(new_freq));
                self.config.ui.frequency_hz = new_freq;
                self.frequency_widget = FrequencyWidget::new(new_freq);
                self.config_dirty = true;
            }
        }

        // Scroll on waterfall: tune (plain) or zoom (Ctrl)
        let wf_scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if waterfall_resp.hovered() && wf_scroll.abs() > 0.5 {
            let (wf_scroll_delta, wf_ctrl) = ui.input(|i| (i.smooth_scroll_delta.y, i.modifiers.ctrl));
            if wf_ctrl {
                let factor = if wf_scroll_delta > 0.0 { 0.8_f32 } else { 1.25_f32 };
                let new_z = (zoom_level * factor).clamp(0.005, 1.0);
                let _ = self.cmd_tx.try_send(SignalPathCommand::SetZoom(new_z));
                self.config.ui.zoom_level = new_z;
                self.config_dirty = true;
            } else {
                let step = self.shared.read().tune_step_hz;
                let new_freq = if wf_scroll_delta > 0.0 {
                    freq.saturating_add(step)
                } else {
                    freq.saturating_sub(step).max(1)
                };
                let _ = self.cmd_tx.try_send(SignalPathCommand::SetFrequency(new_freq));
                self.config.ui.frequency_hz = new_freq;
                self.frequency_widget = FrequencyWidget::new(new_freq);
                self.config_dirty = true;
            }
        }
    }

    // ── Right Panel ──────────────────────────────────────────────────────────

    fn right_panel(&mut self, ui: &mut Ui) {
        ui.add_space(4.0);

        // ── Volume + VU meter ─────────────────────────────────────────────────
        ui.label(RichText::new("VOLUME").color(theme::TEXT_MUTED).small());
        ui.add_space(4.0);

        let mut vol = self.config.ui.volume;
        let slider = egui::Slider::new(&mut vol, 0.0..=1.0)
            .show_value(false)
            .trailing_fill(true);
        if ui
            .add(slider)
            .on_hover_text("Audio output volume (0–100%). Also controllable with nanoKontrol2 Fader 0.")
            .changed()
        {
            self.config.ui.volume = vol;
            let _ = self.cmd_tx.try_send(SignalPathCommand::SetVolume(vol));
            self.config_dirty = true;
        }

        // VU meter (stereo bars)
        // Peak level decays each frame; in real wiring this reads from AudioSink
        let level = self.vu_peak * vol;
        self.draw_vu_meter(ui, level, level * 0.92); // slight L/R difference for visual interest

        ui.add_space(8.0);
        ui.separator();
        ui.add_space(6.0);

        // ── Band Presets ──────────────────────────────────────────────────────
        ui.label(
            RichText::new("BAND PRESETS")
                .color(theme::TEXT_MUTED)
                .small(),
        );
        ui.add_space(4.0);

        let mut tuned: Option<(u64, u64)> = None;
        for preset in BAND_PRESETS {
            let btn = egui::Button::new(
                RichText::new(preset.name)
                    .color(theme::TEXT_PRIMARY)
                    .small(),
            )
            .fill(theme::WIDGET_BG)
            .stroke(Stroke::new(1.0, theme::BORDER));

            if ui
                .add_sized(Vec2::new(ui.available_width(), 20.0), btn)
                .clicked()
            {
                tuned = Some((preset.center_hz, preset.span_hz));
            }
        }

        if let Some((hz, span)) = tuned {
            let _ = self.cmd_tx.try_send(SignalPathCommand::SetFrequency(hz));
            self.config.ui.frequency_hz = hz;
            self.config.ui.span_hz = span;
            self.frequency_widget = FrequencyWidget::new(hz);
            // Convert preset span to zoom_level relative to hardware bandwidth
            let sr_half = self.shared.read().sample_rate_sps as u64 / 2;
            if sr_half > 0 {
                let z = (span as f32 / sr_half as f32).clamp(0.05, 1.0);
                let _ = self.cmd_tx.try_send(SignalPathCommand::SetZoom(z));
                self.config.ui.zoom_level = z;
            }
            self.config_dirty = true;
        }

        ui.add_space(8.0);
        ui.separator();
        ui.add_space(6.0);

        // ── Recorder ─────────────────────────────────────────────────────────
        ui.label(RichText::new("RECORDER").color(theme::TEXT_MUTED).small());
        ui.add_space(4.0);

        let (is_recording, rec_mode, center_freq, iq_sr) = {
            let s = self.shared.read();
            (s.is_recording, s.recording_mode, s.center_freq_hz, s.sample_rate_sps)
        };

        // Recording mode selector
        if !is_recording {
            ui.horizontal(|ui| {
                for mode in [RecordingMode::AudioOnly, RecordingMode::IqOnly, RecordingMode::Both] {
                    let selected = rec_mode == mode;
                    let label = match mode {
                        RecordingMode::AudioOnly => "Audio",
                        RecordingMode::IqOnly => "IQ",
                        RecordingMode::Both => "Both",
                    };
                    if ui.selectable_label(selected, label).clicked() {
                        self.shared.write().recording_mode = mode;
                    }
                }
            });
            ui.add_space(4.0);
        }

        if is_recording {
            let stop_btn = egui::Button::new(
                RichText::new("■  Stop Recording").color(theme::DANGER).strong(),
            )
            .fill(Color32::from_rgba_premultiplied(80, 10, 10, 200))
            .stroke(Stroke::new(1.5, theme::DANGER));

            if ui.add_sized(Vec2::new(ui.available_width(), 28.0), stop_btn).clicked() {
                let _ = self.recorder_cmd_tx.try_send(RecorderCommand::Stop);
                let _ = self.cmd_tx.try_send(SignalPathCommand::StopRecording);
            }

            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("●").color(theme::DANGER));
                ui.label(RichText::new("REC").color(theme::AMBER).strong());
            });
        } else {
            let rec_btn =
                egui::Button::new(RichText::new("●  Start Recording").color(theme::STATUS_OK))
                    .fill(theme::WIDGET_BG)
                    .stroke(Stroke::new(1.0, theme::STATUS_OK));

            if ui.add_sized(Vec2::new(ui.available_width(), 28.0), rec_btn).clicked() {
                let _ = self.recorder_cmd_tx.try_send(RecorderCommand::Start {
                    freq_hz: center_freq,
                    iq_sample_rate: iq_sr,
                    mode: rec_mode,
                });
                let _ = self.cmd_tx.try_send(SignalPathCommand::StartRecording);
            }
        }

        // ── Scheduled Recording ───────────────────────────────────────────────
        ui.add_space(6.0);
        ui.collapsing("Schedule", |ui| {
            ui.horizontal(|ui| {
                ui.label("Delay (s):");
                let mut delay = self.sched_delay_secs;
                if ui.add(egui::DragValue::new(&mut delay).range(0..=3600)).changed() {
                    self.sched_delay_secs = delay;
                }
            });
            ui.horizontal(|ui| {
                ui.label("Duration (s):");
                let mut dur = self.sched_duration_secs;
                if ui.add(egui::DragValue::new(&mut dur).range(1..=86400)).changed() {
                    self.sched_duration_secs = dur;
                }
            });
            let arm_btn = egui::Button::new("⏱  Arm Schedule")
                .fill(theme::WIDGET_BG)
                .stroke(Stroke::new(1.0, theme::ACCENT));
            if ui.add_sized(Vec2::new(ui.available_width(), 24.0), arm_btn).clicked() {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let _ = self.recorder_cmd_tx.try_send(RecorderCommand::Schedule {
                    start_unix_secs: now + self.sched_delay_secs as u64,
                    duration_secs: self.sched_duration_secs,
                    freq_hz: center_freq,
                    iq_sample_rate: iq_sr,
                    mode: rec_mode,
                });
                tracing::info!(
                    delay = self.sched_delay_secs,
                    duration = self.sched_duration_secs,
                    "scheduled recording armed"
                );
            }
        });

        ui.add_space(8.0);
        ui.separator();
        ui.add_space(6.0);

        // ── MIDI Status ───────────────────────────────────────────────────────
        ui.label(RichText::new("MIDI").color(theme::TEXT_MUTED).small());
        ui.add_space(4.0);

        let (midi_device, midi_page) = {
            let s = self.shared.read();
            (s.midi_device.clone(), s.midi_page)
        };

        if let Some(ref device_name) = midi_device {
            // Connected
            ui.horizontal(|ui| {
                ui.label(RichText::new("●").color(theme::STATUS_OK));
                ui.label(
                    RichText::new(device_name)
                        .color(theme::TEXT_PRIMARY)
                        .small(),
                );
            });

            // Page display with navigation buttons
            let page_names = ["Tune", "Monitor", "Recorder"];
            let page_label = page_names.get(midi_page).copied().unwrap_or("Page ?");
            let page_color = theme::midi_page_color(midi_page);

            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Page:").color(theme::TEXT_MUTED).small());
                ui.label(
                    RichText::new(format!("{midi_page}  {page_label}"))
                        .color(page_color)
                        .small()
                        .strong(),
                );
            });
        } else {
            ui.horizontal(|ui| {
                ui.label(RichText::new("●").color(theme::TEXT_DISABLED));
                ui.label(
                    RichText::new("Not connected")
                        .color(theme::TEXT_MUTED)
                        .small(),
                );
            });
            ui.label(
                RichText::new("Connect nanoKontrol2 via USB")
                    .color(theme::TEXT_DISABLED)
                    .small(),
            );
        }
    }

    // ── VU Meter helper ───────────────────────────────────────────────────────

    fn draw_vu_meter(&mut self, ui: &mut Ui, left: f32, right: f32) {
        let bar_w = ui.available_width() / 2.0 - 4.0;
        let bar_h = 8.0;

        ui.horizontal(|ui| {
            for &level in &[left, right] {
                let (rect, _) =
                    ui.allocate_exact_size(Vec2::new(bar_w, bar_h), egui::Sense::hover());

                let painter = ui.painter();
                // Background track
                painter.rect_filled(rect, 2.0, theme::WIDGET_BG);

                // Fill bar
                let fill_w = rect.width() * level.clamp(0.0, 1.0);
                if fill_w > 0.5 {
                    let fill_rect = egui::Rect::from_min_size(rect.min, Vec2::new(fill_w, bar_h));
                    let color = if level > 0.9 {
                        theme::VU_HIGH
                    } else if level > 0.6 {
                        theme::VU_MID
                    } else {
                        theme::VU_LOW
                    };
                    painter.rect_filled(fill_rect, 2.0, color);
                }
            }
        });

        // Decay the peak
        self.vu_peak = (self.vu_peak - 0.02).max(0.0);
    }

    // ── Status Bar ────────────────────────────────────────────────────────────

    fn status_bar(&self, ui: &mut Ui) {
        let (is_running, is_recording, center_freq, sample_rate, midi_device, midi_page, buf_fill, source_name, is_stereo) = {
            let s = self.shared.read();
            (
                s.is_running,
                s.is_recording,
                s.center_freq_hz,
                s.sample_rate_sps,
                s.midi_device.clone(),
                s.midi_page,
                s.audio_buffer_fill,
                s.source_name.clone(),
                s.is_stereo,
            )
        };

        ui.horizontal(|ui| {
            // Left: device + sample rate + frequency
            let device_label = source_name
                .as_deref()
                .or_else(|| {
                    self.registry
                        .sources
                        .first()
                        .map(|s| s.display_name)
                })
                .unwrap_or("No device");

            let rate_label = if sample_rate >= 1_000_000 {
                format!("{:.1} Msps", sample_rate as f64 / 1_000_000.0)
            } else if sample_rate >= 1_000 {
                format!("{:.0} ksps", sample_rate as f64 / 1_000.0)
            } else {
                format!("{sample_rate} sps")
            };

            let freq_label = format_frequency(center_freq);
            let stereo_badge = if is_stereo { "  ST" } else { "" };
            ui.label(
                RichText::new(format!(
                    "◈  {device_label}  ·  {rate_label}  ·  {freq_label}{stereo_badge}"
                ))
                .color(theme::TEXT_MUTED)
                .small(),
            );

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // Right: recording
                if is_recording {
                    ui.label(RichText::new("● REC").color(theme::DANGER).small().strong());
                    ui.add_space(8.0);
                }

                // Audio buffer health (tiny bar)
                let buf_color = if buf_fill > 0.8 {
                    theme::STATUS_WARN
                } else {
                    theme::STATUS_OK
                };
                ui.label(
                    RichText::new(format!("BUF {:.0}%", buf_fill * 100.0))
                        .color(buf_color)
                        .small(),
                );
                ui.add_space(8.0);

                // MIDI status
                if let Some(ref dev) = midi_device {
                    let page_color = theme::midi_page_color(midi_page);
                    let page_names = ["Tune", "Monitor", "Rec"];
                    let page_name = page_names.get(midi_page).copied().unwrap_or("?");
                    ui.label(
                        RichText::new(format!("MIDI: {dev}  P{midi_page}:{page_name}"))
                            .color(page_color)
                            .small(),
                    );
                } else {
                    ui.label(RichText::new("MIDI: —").color(theme::TEXT_DISABLED).small());
                }
                ui.add_space(8.0);

                // Running indicator
                let dot = if is_running { "●" } else { "○" };
                let color = if is_running {
                    theme::STATUS_OK
                } else {
                    theme::TEXT_DISABLED
                };
                ui.label(RichText::new(dot).color(color).small());
            });
        });
    }
}

// ── eframe::App ───────────────────────────────────────────────────────────────

impl eframe::App for SdrApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.config_dirty {
            self.config.save();
            self.config_dirty = false;
        }

        // ── '?' key toggles help panel ────────────────────────────────────────
        if ctx.input(|i| i.key_pressed(egui::Key::Questionmark)) {
            let mut s = self.shared.write();
            s.help_panel_open = !s.help_panel_open;
        }

        // ── Arrow-key frequency tuning ────────────────────────────────────────
        // Up/Down arrows tune by step_hz. Left/Right arrows step by 10×.
        let (up, down, left, right) = ctx.input(|i| (
            i.key_pressed(egui::Key::ArrowUp),
            i.key_pressed(egui::Key::ArrowDown),
            i.key_pressed(egui::Key::ArrowLeft),
            i.key_pressed(egui::Key::ArrowRight),
        ));
        if up || down || left || right {
            let step = self.shared.read().tune_step_hz;
            let coarse = step * 10;
            let delta: i64 = match (up, down, left, right) {
                (true, _, _, _) => step as i64,
                (_, true, _, _) => -(step as i64),
                (_, _, _, true) => coarse as i64,
                _ => -(coarse as i64),
            };
            let freq = self.config.ui.frequency_hz;
            let new_freq = if delta >= 0 {
                freq.saturating_add(delta as u64)
            } else {
                freq.saturating_sub((-delta) as u64).max(1)
            };
            let _ = self.cmd_tx.try_send(SignalPathCommand::SetFrequency(new_freq));
            self.config.ui.frequency_hz = new_freq;
            self.frequency_widget = FrequencyWidget::new(new_freq);
            self.config_dirty = true;
        }

        // Status bar at the bottom
        egui::TopBottomPanel::bottom("status_bar")
            .exact_height(20.0)
            .frame(
                egui::Frame::none()
                    .fill(theme::BG)
                    .inner_margin(egui::Margin::symmetric(8.0, 0.0)),
            )
            .show(ctx, |ui| self.status_bar(ui));

        // Left panel
        egui::SidePanel::left("left_panel")
            .min_width(190.0)
            .max_width(250.0)
            .frame(
                egui::Frame::none()
                    .fill(theme::PANEL_BG)
                    .inner_margin(egui::Margin::symmetric(8.0, 6.0)),
            )
            .show(ctx, |ui| self.left_panel(ui));

        // Right panel
        egui::SidePanel::right("right_panel")
            .min_width(170.0)
            .max_width(220.0)
            .frame(
                egui::Frame::none()
                    .fill(theme::PANEL_BG)
                    .inner_margin(egui::Margin::symmetric(8.0, 6.0)),
            )
            .show(ctx, |ui| self.right_panel(ui));

        // Center spectrum + waterfall
        egui::CentralPanel::default()
            .frame(
                egui::Frame::none()
                    .fill(theme::BG)
                    .inner_margin(egui::Margin::same(0.0)),
            )
            .show(ctx, |ui| self.center_panel(ui));

        // ── Help panel (floating window) ──────────────────────────────────────
        let mut help_open = self.shared.read().help_panel_open;
        let before = help_open;
        self.help_panel.show(ctx, &mut help_open, &self.midi_bindings);
        if help_open != before {
            self.shared.write().help_panel_open = help_open;
        }

        // Request continuous repaint while running
        if self.shared.read().is_running {
            ctx.request_repaint_after(std::time::Duration::from_millis(33)); // ~30fps
        }
    }

    fn save(&mut self, _storage: &mut dyn eframe::Storage) {
        self.config.save();
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        let _ = self.cmd_tx.try_send(SignalPathCommand::Stop);
        self.config.save();
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn format_frequency(hz: u64) -> String {
    if hz >= 1_000_000_000 {
        format!("{:.3} GHz", hz as f64 / 1_000_000_000.0)
    } else if hz >= 1_000_000 {
        format!("{:.3} MHz", hz as f64 / 1_000_000.0)
    } else if hz >= 1_000 {
        format!("{:.1} kHz", hz as f64 / 1_000.0)
    } else {
        format!("{hz} Hz")
    }
}
