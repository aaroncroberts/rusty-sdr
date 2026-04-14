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
    config::AppConfig,
    registry::ModuleRegistry,
    signal_path::{DemodMode, SharedState, SignalPathCommand},
};

use crate::{
    frequency::FrequencyWidget, spectrum::SpectrumWidget, theme, waterfall::WaterfallWidget,
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
    frequency_widget: FrequencyWidget,
    waterfall: WaterfallWidget,
    /// Dirty flag — save config at next opportunity
    config_dirty: bool,
    /// Simulated audio peak level [0.0, 1.0] for VU meter (updated each frame)
    vu_peak: f32,
    /// Peak-hold buffer: tracks per-bin maximum with slow decay
    peak_hold: Vec<f32>,
    // ── dBFS range ────────────────────────────────────────────────────────────
    /// Lower bound of display range (dBFS)
    db_floor: f32,
    /// Upper bound of display range (dBFS)
    db_ceil: f32,
    /// Whether auto-range is active
    auto_range: bool,
    /// Slow EMA of 10th-percentile FFT bin — noise floor estimate
    noise_floor_ema: f32,
    /// Slow EMA of 99th-percentile FFT bin — signal ceiling estimate
    signal_ceil_ema: f32,
}

impl SdrApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        config: AppConfig,
        shared: Arc<RwLock<SharedState>>,
        cmd_tx: crossbeam_channel::Sender<SignalPathCommand>,
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
            config_dirty: false,
            vu_peak: 0.0,
            peak_hold: Vec::new(),
            db_floor: -120.0,
            db_ceil: 0.0,
            auto_range: true,
            noise_floor_ema: -90.0,
            signal_ceil_ema: -30.0,
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

        ui.add_space(8.0);
        ui.separator();
        ui.add_space(6.0);

        // ── Demod mode ────────────────────────────────────────────────────────
        ui.label(RichText::new("DEMOD MODE").color(theme::TEXT_MUTED).small());
        ui.add_space(4.0);

        let current_mode = self.shared.read().demod_mode;
        ui.horizontal(|ui| {
            for (mode, label) in [
                (DemodMode::Wbfm, "WBFM"),
                (DemodMode::Nfm, "NFM"),
                (DemodMode::Am, "AM"),
            ] {
                let selected = current_mode == mode;
                let text = RichText::new(label).small();
                let text = if selected {
                    text.color(theme::ACCENT).strong()
                } else {
                    text.color(theme::TEXT_MUTED)
                };
                if ui.selectable_label(selected, text).clicked() && !selected {
                    let _ = self.cmd_tx.try_send(SignalPathCommand::SetDemodMode(mode));
                }
            }
        });

        // ── NFM squelch ───────────────────────────────────────────────────────
        if current_mode == DemodMode::Nfm {
            ui.add_space(8.0);
            ui.separator();
            ui.add_space(6.0);

            ui.label(RichText::new("SQUELCH").color(theme::TEXT_MUTED).small());
            ui.add_space(4.0);

            let mut sq_threshold = self.shared.read().squelch_threshold;
            let sq_label = format!("{:.0} dBFS", sq_threshold);
            ui.label(RichText::new(&sq_label).color(theme::TEXT_PRIMARY).small());
            let sq_slider = egui::Slider::new(&mut sq_threshold, -120.0_f32..=0.0_f32)
                .show_value(false)
                .trailing_fill(true);
            if ui.add(sq_slider).changed() {
                let _ = self
                    .cmd_tx
                    .try_send(SignalPathCommand::SetSquelchThreshold(sq_threshold));
            }
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
        let (center_freq, is_recording, rds_ps_name) = {
            let s = self.shared.read();
            (s.center_freq_hz, s.is_recording, s.rds_ps_name.clone())
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

        if let Some(ref ps) = rds_ps_name {
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("RDS").color(theme::ACCENT).small().strong());
                ui.label(RichText::new(ps).color(theme::TEXT_PRIMARY).strong());
            });
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
        let span = self.config.ui.span_hz;

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

            // Auto-range: slow EMA on 10th/99th percentiles of FFT bins
            if self.auto_range && n >= 10 {
                let mut sorted = fft_data.to_vec();
                sorted.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                let floor_sample = sorted[n / 10];
                let ceil_sample = sorted[(n * 99 / 100).min(n - 1)];
                const ALPHA: f32 = 0.95;
                self.noise_floor_ema = ALPHA * self.noise_floor_ema + (1.0 - ALPHA) * floor_sample;
                self.signal_ceil_ema = ALPHA * self.signal_ceil_ema + (1.0 - ALPHA) * ceil_sample;
                // 20 dB below noise floor … 10 dB above signal ceiling
                self.db_floor = (self.noise_floor_ema - 20.0).max(-140.0);
                self.db_ceil = (self.signal_ceil_ema + 10.0).min(20.0);
                // Ensure minimum 30 dB span to avoid degenerate zoom
                if self.db_ceil - self.db_floor < 30.0 {
                    self.db_ceil = self.db_floor + 30.0;
                }
            }

            let db_range = (self.db_floor, self.db_ceil);
            self.waterfall.set_db_range(db_range);
            self.waterfall.push_row(&fft_data);
        }

        let db_range = (self.db_floor, self.db_ceil);

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

        // Scroll-to-zoom span on spectrum
        let scroll_delta = ui.input(|i| i.smooth_scroll_delta.y);
        if spectrum_resp.hovered() && scroll_delta.abs() > 0.5 {
            let factor = if scroll_delta > 0.0 {
                0.8_f64
            } else {
                1.25_f64
            };
            let new_span = ((span as f64 * factor) as u64).clamp(50_000, 20_000_000);
            self.config.ui.span_hz = new_span;
            self.config_dirty = true;
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

        // ── dBFS range control ────────────────────────────────────────────────
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            // Auto button (toggle)
            let auto_label = if self.auto_range { "↕ Auto ✓" } else { "↕ Auto" };
            let auto_color = if self.auto_range {
                theme::ACCENT
            } else {
                theme::TEXT_MUTED
            };
            if ui
                .small_button(RichText::new(auto_label).color(auto_color))
                .clicked()
            {
                self.auto_range = !self.auto_range;
            }

            if self.auto_range {
                // Display current auto-computed range
                ui.label(
                    RichText::new(format!(
                        "  {:.0} → {:.0} dBFS",
                        self.db_floor, self.db_ceil
                    ))
                    .color(theme::TEXT_MUTED)
                    .small(),
                );
            } else {
                // Manual: compact floor/ceil sliders
                ui.add_space(4.0);
                ui.label(RichText::new("Floor").color(theme::TEXT_MUTED).small());
                let mut floor = self.db_floor;
                if ui
                    .add(
                        egui::Slider::new(&mut floor, -140.0_f32..=-20.0_f32)
                            .show_value(true)
                            .integer(),
                    )
                    .changed()
                {
                    self.db_floor = floor.min(self.db_ceil - 10.0);
                }
                ui.add_space(4.0);
                ui.label(RichText::new("Ceil").color(theme::TEXT_MUTED).small());
                let mut ceil = self.db_ceil;
                if ui
                    .add(
                        egui::Slider::new(&mut ceil, -60.0_f32..=20.0_f32)
                            .show_value(true)
                            .integer(),
                    )
                    .changed()
                {
                    self.db_ceil = ceil.max(self.db_floor + 10.0);
                }
            }
        });

        // ── Waterfall ─────────────────────────────────────────────────────────
        self.waterfall.show(ui, &ctx);
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
        if ui.add(slider).changed() {
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
            self.config_dirty = true;
        }

        ui.add_space(8.0);
        ui.separator();
        ui.add_space(6.0);

        // ── Recorder ─────────────────────────────────────────────────────────
        ui.label(RichText::new("RECORDER").color(theme::TEXT_MUTED).small());
        ui.add_space(4.0);

        let is_recording = self.shared.read().is_recording;

        if is_recording {
            // Stop button
            let stop_btn = egui::Button::new(
                RichText::new("■  Stop Recording")
                    .color(theme::DANGER)
                    .strong(),
            )
            .fill(Color32::from_rgba_premultiplied(80, 10, 10, 200))
            .stroke(Stroke::new(1.5, theme::DANGER));

            if ui
                .add_sized(Vec2::new(ui.available_width(), 28.0), stop_btn)
                .clicked()
            {
                let _ = self.cmd_tx.try_send(SignalPathCommand::StopRecording);
            }

            // Recording indicator
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

            if ui
                .add_sized(Vec2::new(ui.available_width(), 28.0), rec_btn)
                .clicked()
            {
                let _ = self.cmd_tx.try_send(SignalPathCommand::StartRecording);
            }
        }

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
