use egui::{RichText, Stroke, Ui, Vec2};

use sdrapp_core::signal_path::{DemodMode, ReceiverCmd, ScanCmd, SignalPathCommand};

use super::super::SdrApp;
use crate::{frequency::FrequencyWidget, knob::KnobWidget, theme};

impl SdrApp {
    pub(in crate::app) fn left_panel(&mut self, ui: &mut Ui) {
        ui.add_space(4.0);

        // App title with version
        ui.horizontal(|ui| {
            ui.add_space(2.0);
            ui.label(
                RichText::new("SDR App")
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
            .or_else(|| self.registry.sources.first().map(|s| s.display_name))
            .unwrap_or("No device");

        ui.horizontal(|ui| {
            let (icon, color) = if is_demo {
                ("[!]", theme::DANGER)
            } else {
                ("[*]", theme::ACCENT_DIM)
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
        {
            if is_running {
                tracing::info!("UI: Stop clicked");
                if self.cmd_tx.try_send(SignalPathCommand::Stop).is_err() {
                    tracing::error!("failed to send Stop command");
                }
            } else {
                tracing::info!("UI: Start clicked");
                if self.cmd_tx.try_send(SignalPathCommand::Start).is_err() {
                    tracing::error!("failed to send Start command");
                }
            }
        }

        ui.add_space(8.0);
        ui.separator();
        ui.add_space(6.0);

        // ── Frequency section ─────────────────────────────────────────────────
        ui.label(RichText::new("FREQUENCY").color(theme::TEXT_MUTED).small());
        ui.add_space(4.0);

        let (_, new_freq) = self.frequency_widget.show(ui);
        if let Some(freq) = new_freq {
            let _ = self.cmd_tx.try_send(ReceiverCmd::SetFrequency(freq).into());
            self.config.ui.frequency_hz = freq;
            self.config_dirty = true;
        }

        // ── Tuning step controls ──────────────────────────────────────────────
        let step_hz = self.shared.read().demod.tune_step_hz;
        ui.add_space(4.0);

        // Step size selector row
        ui.label(RichText::new("STEP").color(theme::TEXT_MUTED).small());
        ui.add_space(2.0);
        ui.horizontal_wrapped(|ui| {
            for (hz, label) in [
                (100_u64, "100 Hz"),
                (1_000, "1 kHz"),
                (10_000, "10 kHz"),
                (100_000, "100 kHz"),
                (1_000_000, "1 MHz"),
                (10_000_000, "10 MHz"),
            ] {
                let selected = step_hz == hz;
                let text = RichText::new(label).small();
                let text = if selected {
                    text.color(theme::ACCENT).strong()
                } else {
                    text.color(theme::TEXT_MUTED)
                };
                if ui.selectable_label(selected, text).clicked() {
                    let _ = self.cmd_tx.try_send(ReceiverCmd::SetTuneStep(hz).into());
                }
            }
        });

        // Nudge buttons: << < > >> (×10 / ×1 step)
        ui.add_space(4.0);
        let freq = self.config.ui.frequency_hz;
        ui.horizontal(|ui| {
            let btn_w = (ui.available_width() - 16.0) / 4.0;
            for (label, delta, tip) in [
                ("<<", -(step_hz as i64 * 10), "-10x step"),
                ("<", -(step_hz as i64), "-1x step  (or Down arrow key)"),
                (">", step_hz as i64, "+1x step  (or Up arrow key)"),
                (">>", step_hz as i64 * 10, "+10x step"),
            ] {
                if ui
                    .add_sized(
                        Vec2::new(btn_w, 22.0),
                        egui::Button::new(RichText::new(label).color(theme::TEXT_PRIMARY))
                            .fill(theme::WIDGET_BG),
                    )
                    .on_hover_text(tip)
                    .clicked()
                {
                    let new_freq = (freq as i64 + delta).max(1) as u64;
                    let _ = self
                        .cmd_tx
                        .try_send(ReceiverCmd::SetFrequency(new_freq).into());
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

        let current_mode = self.shared.read().demod.demod_mode;
        // Row 1: FM and AM modes
        ui.horizontal(|ui| {
            for (mode, label, tooltip) in [
                (DemodMode::Wbfm, "WBFM", "Wideband FM — FM broadcast stations (88–108 MHz). 75 kHz deviation, stereo, RDS."),
                (DemodMode::Nfm, "NFM", "Narrow FM — voice comms (aviation, marine, amateur, PMR). 12.5–25 kHz channels. Enable squelch."),
                (DemodMode::Am, "AM", "Amplitude Modulation — AM broadcast (530 kHz–1.7 MHz), shortwave, aviation voice."),
            ] {
                let selected = current_mode == mode;
                let text = RichText::new(label).small();
                let text = if selected { text.color(theme::ACCENT).strong() } else { text.color(theme::TEXT_MUTED) };
                if ui.selectable_label(selected, text).on_hover_text(tooltip).clicked() && !selected {
                    let _ = self.cmd_tx.try_send(ReceiverCmd::SetDemodMode(mode).into());
                }
            }
        });
        // Row 2: SSB and CW modes
        ui.horizontal(|ui| {
            for (mode, label, tooltip) in [
                (DemodMode::Usb, "USB", "Upper Sideband SSB — HF amateur and maritime voice. Tune above the suppressed carrier."),
                (DemodMode::Lsb, "LSB", "Lower Sideband SSB — HF amateur voice below 10 MHz (160m–40m). Tune below the carrier."),
                (DemodMode::Dsb, "DSB", "Double Sideband — both sidebands, suppressed carrier. Rare; used in some utility stations."),
                (DemodMode::Cw, "CW", "CW / Morse code — narrow 400–900 Hz bandpass centred on the 700 Hz sidetone."),
            ] {
                let selected = current_mode == mode;
                let text = RichText::new(label).small();
                let text = if selected { text.color(theme::ACCENT).strong() } else { text.color(theme::TEXT_MUTED) };
                if ui.selectable_label(selected, text).on_hover_text(tooltip).clicked() && !selected {
                    let _ = self.cmd_tx.try_send(ReceiverCmd::SetDemodMode(mode).into());
                }
            }
        });

        // ── NFM squelch & settings ────────────────────────────────────────────
        if current_mode == DemodMode::Nfm {
            ui.add_space(8.0);
            ui.separator();
            ui.add_space(6.0);

            ui.label(
                RichText::new("NFM SETTINGS")
                    .color(theme::TEXT_MUTED)
                    .small(),
            );
            ui.add_space(4.0);

            // Channel bandwidth selector (12.5 / 25 kHz)
            let nfm_bw = self.shared.read().demod.nfm_bandwidth_hz;
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
                        let _ = self.cmd_tx.try_send(ReceiverCmd::SetNfmBandwidth(bw).into());
                        self.config.ui.nfm_bandwidth_hz = bw;
                        self.config_dirty = true;
                    }
                }
            });

            ui.add_space(4.0);

            // Squelch threshold knob
            let (sq_learn, sq_cc, sq_map_pending) = {
                let s = self.shared.read();
                let learn = s.midi_learn_target.as_deref() == Some("squelch");
                let cc = s
                    .midi_cc_to_knob
                    .iter()
                    .find(|(_, v)| v.as_str() == "squelch")
                    .map(|(&c, _)| c);
                (learn, cc, s.midi_map_pending.is_some())
            };
            let mut sq_learn_req = false;
            let mut sq_learn_cancel = false;
            let mut sq_clear: Option<u8> = None;
            let mut sq_bind = false;
            ui.vertical_centered(|ui| {
                let mut sq_threshold = self.shared.read().demod.squelch_threshold;
                let resp = KnobWidget {
                    value: &mut sq_threshold,
                    range: -120.0_f32..=0.0_f32,
                    default_value: -50.0,
                    step: 2.0,
                    diameter: 44.0,
                    label: Some("SQUELCH"),
                    unit: "dBFS",
                    midi_cc: sq_cc,
                    learn_active: sq_learn || sq_map_pending,
                }
                .show(ui);
                if sq_map_pending && resp.clicked() {
                    sq_bind = true;
                } else {
                    resp.context_menu(|ui| {
                        if sq_learn {
                            if ui.button("Cancel MIDI Learn").clicked() {
                                sq_learn_cancel = true;
                                ui.close_menu();
                            }
                        } else if ui.button("Assign MIDI CC").clicked() {
                            sq_learn_req = true;
                            ui.close_menu();
                        }
                        if let Some(cc) = sq_cc {
                            if ui.button(format!("Clear CC {cc} binding")).clicked() {
                                sq_clear = Some(cc);
                                ui.close_menu();
                            }
                        }
                    });
                }
                if resp.changed() {
                    let _ = self
                        .cmd_tx
                        .try_send(ReceiverCmd::SetSquelchThreshold(sq_threshold).into());
                }
            });
            if sq_bind {
                if let Some(cc) = self.shared.write().midi_map_pending.take() {
                    self.shared.write().midi_cc_to_knob.insert(cc, "squelch".into());
                    self.config_dirty = true;
                }
            }
            if sq_learn_req {
                self.shared.write().midi_learn_target = Some("squelch".into());
            }
            if sq_learn_cancel {
                self.shared.write().midi_learn_target = None;
            }
            if let Some(cc) = sq_clear {
                self.shared.write().midi_cc_to_knob.remove(&cc);
                self.config_dirty = true;
            }

            // Inline signal level meter: shows live dBFS vs threshold
            {
                let (sig_level, sq_threshold) = {
                    let s = self.shared.read();
                    (s.demod.nfm_signal_level_dbfs, s.demod.squelch_threshold)
                };
                let range = -120.0_f32..=0.0_f32;
                let fill = ((sig_level - *range.start()) / (*range.end() - *range.start()))
                    .clamp(0.0, 1.0);
                let thresh_frac = ((sq_threshold - *range.start())
                    / (*range.end() - *range.start()))
                .clamp(0.0, 1.0);
                let bar_color = if sig_level >= sq_threshold {
                    theme::STATUS_OK
                } else {
                    theme::TEXT_MUTED
                };
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("SIG").color(theme::TEXT_MUTED).small());
                    let (rect, _) = ui.allocate_exact_size(
                        egui::Vec2::new(ui.available_width(), 8.0),
                        egui::Sense::hover(),
                    );
                    if ui.is_rect_visible(rect) {
                        let painter = ui.painter_at(rect);
                        // Background
                        painter.rect_filled(rect, 2.0, theme::WIDGET_BG);
                        // Signal fill
                        let filled = egui::Rect::from_min_max(
                            rect.left_top(),
                            egui::pos2(rect.left() + rect.width() * fill, rect.bottom()),
                        );
                        painter.rect_filled(filled, 2.0, bar_color);
                        // Threshold marker line
                        let tx = rect.left() + rect.width() * thresh_frac;
                        painter.line_segment(
                            [egui::pos2(tx, rect.top()), egui::pos2(tx, rect.bottom())],
                            egui::Stroke::new(1.5, theme::DANGER),
                        );
                    }
                });
                ui.label(
                    egui::RichText::new(format!("{sig_level:.0} dBFS"))
                        .color(bar_color)
                        .small(),
                );
            }

            ui.add_space(4.0);

            // CTCSS tone squelch toggle
            let (ctcss_enabled, ctcss_detected) = {
                let s = self.shared.read();
                (s.demod.ctcss_squelch_enabled, s.demod.ctcss_tone_detected)
            };
            ui.horizontal(|ui| {
                let label_color = if ctcss_enabled { theme::ACCENT } else { theme::TEXT_MUTED };
                let ctcss_label = if ctcss_enabled && ctcss_detected {
                    "CTCSS on"
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
                    let _ = self.cmd_tx.try_send(ReceiverCmd::SetCtcssEnabled(new_enabled).into());
                    self.config.ui.ctcss_enabled = new_enabled;
                    self.config_dirty = true;
                }
            });
        }

        ui.add_space(8.0);
        ui.separator();
        ui.add_space(6.0);

        // ── Bookmarks ─────────────────────────────────────────────────────────
        self.bookmarks_section(ui);

        ui.add_space(8.0);
        ui.separator();
        ui.add_space(6.0);

        // ── Scanner ───────────────────────────────────────────────────────────
        ui.label(RichText::new("SCANNER").color(theme::TEXT_MUTED).small());
        ui.add_space(4.0);

        let (scan_running, scan_cursor, scan_bm_count) = {
            let s = self.shared.read();
            let cat = &self.scan_cat_ui;
            let count = s
                .bookmarks
                .iter()
                .filter(|b| cat.is_empty() || b.category == *cat)
                .count();
            (s.scanner.scan_running, s.scanner.scan_cursor, count)
        };
        let scan_can_start = is_running && scan_bm_count > 0;

        // Category filter
        ui.horizontal(|ui| {
            ui.label(RichText::new("Cat").color(theme::TEXT_MUTED).small());
            ui.text_edit_singleline(&mut self.scan_cat_ui)
                .on_hover_text("Scan only this category (empty = all bookmarks)");
        });

        // Dwell time
        ui.horizontal(|ui| {
            ui.label(RichText::new("Dwell").color(theme::TEXT_MUTED).small());
            if ui
                .add(
                    egui::Slider::new(&mut self.scan_dwell_ui, 0.5_f32..=15.0_f32)
                        .suffix(" s")
                        .show_value(true),
                )
                .changed()
            {
                let _ = self
                    .cmd_tx
                    .try_send(ScanCmd::SetDwell(self.scan_dwell_ui).into());
            }
        });

        // Start / Stop / Next
        ui.horizontal(|ui| {
            if scan_running {
                let stop_btn =
                    egui::Button::new(RichText::new("■  Stop").color(theme::DANGER).strong())
                        .fill(theme::WIDGET_BG);
                if ui.add_sized(Vec2::new(70.0, 22.0), stop_btn).clicked() {
                    let _ = self.cmd_tx.try_send(ScanCmd::Stop.into());
                }
                if ui
                    .small_button(RichText::new(">> Next").color(theme::TEXT_MUTED))
                    .clicked()
                {
                    let _ = self.cmd_tx.try_send(ScanCmd::Next.into());
                }
                ui.label(
                    RichText::new("SCAN")
                        .color(theme::STATUS_OK)
                        .small()
                        .strong(),
                );
            } else {
                let start_color = if scan_can_start {
                    theme::STATUS_OK
                } else {
                    theme::TEXT_MUTED
                };
                let start_btn =
                    egui::Button::new(RichText::new("▶  Scan").color(start_color).strong())
                        .fill(theme::WIDGET_BG);
                let tip = if !is_running {
                    "Start the radio first".to_string()
                } else if scan_bm_count == 0 {
                    "No bookmarks to scan — add some first".to_string()
                } else {
                    format!("Scan {scan_bm_count} bookmark(s)")
                };
                let start_resp = ui
                    .add_sized(Vec2::new(70.0, 22.0), start_btn)
                    .on_hover_text(tip);
                if start_resp.clicked() && scan_can_start {
                    let _ = self
                        .cmd_tx
                        .try_send(ScanCmd::SetDwell(self.scan_dwell_ui).into());
                    let _ = self
                        .cmd_tx
                        .try_send(ScanCmd::Start(self.scan_cat_ui.clone()).into());
                }
            }
        });

        // When scanning: show current target bookmark name + freq
        if scan_running {
            let bm_label = {
                let s = self.shared.read();
                s.bookmarks.get(scan_cursor).map(|b| {
                    format!("{} — {:.3} MHz", b.name, b.freq_hz as f64 / 1_000_000.0)
                })
            };
            if let Some(label) = bm_label {
                ui.add_space(2.0);
                ui.label(RichText::new(label).color(theme::ACCENT_DIM).small());
            }
        } else if !is_running {
            ui.add_space(2.0);
            ui.label(
                RichText::new("Start the radio to enable scanning")
                    .color(theme::TEXT_MUTED)
                    .small(),
            );
        } else if scan_bm_count == 0 {
            ui.add_space(2.0);
            ui.label(
                RichText::new("Add bookmarks to enable scanning")
                    .color(theme::DANGER)
                    .small(),
            );
        }

        ui.add_space(8.0);
        ui.separator();
        ui.add_space(6.0);

        // ── FM Band Scanner ───────────────────────────────────────────────────
        ui.label(RichText::new("FM BAND SCAN").color(theme::TEXT_MUTED).small());
        ui.add_space(4.0);

        let range_running = {
            let s = self.shared.read();
            s.scanner.scan_running && s.scanner.range_mode
        };

        // Freq range row
        ui.horizontal(|ui| {
            ui.label(RichText::new("Lo").color(theme::TEXT_MUTED).small());
            let mut lo_mhz = self.range_scan_lo_hz as f64 / 1_000_000.0;
            if ui
                .add(
                    egui::DragValue::new(&mut lo_mhz)
                        .range(70.0..=200.0)
                        .speed(0.1)
                        .suffix(" MHz"),
                )
                .changed()
            {
                self.range_scan_lo_hz = (lo_mhz * 1_000_000.0) as u64;
            }
            ui.label(RichText::new("Hi").color(theme::TEXT_MUTED).small());
            let mut hi_mhz = self.range_scan_hi_hz as f64 / 1_000_000.0;
            if ui
                .add(
                    egui::DragValue::new(&mut hi_mhz)
                        .range(70.0..=200.0)
                        .speed(0.1)
                        .suffix(" MHz"),
                )
                .changed()
            {
                self.range_scan_hi_hz = (hi_mhz * 1_000_000.0) as u64;
            }
        });

        // Step + dwell row
        ui.horizontal(|ui| {
            ui.label(RichText::new("Step").color(theme::TEXT_MUTED).small());
            let mut step_khz = self.range_scan_step_hz as f64 / 1_000.0;
            if ui
                .add(
                    egui::DragValue::new(&mut step_khz)
                        .range(10.0..=500.0)
                        .speed(10.0)
                        .suffix(" kHz"),
                )
                .changed()
            {
                self.range_scan_step_hz = (step_khz * 1_000.0) as u64;
            }
            ui.label(RichText::new("Dwell").color(theme::TEXT_MUTED).small());
            ui.add(
                egui::DragValue::new(&mut self.range_scan_dwell)
                    .range(0.1_f32..=5.0_f32)
                    .speed(0.05)
                    .suffix(" s"),
            );
        });

        // Squelch threshold
        ui.horizontal(|ui| {
            ui.label(RichText::new("Squelch").color(theme::TEXT_MUTED).small());
            ui.add(
                egui::Slider::new(&mut self.range_scan_squelch, -120.0_f32..=-10.0_f32)
                    .suffix(" dB")
                    .show_value(true),
            );
        });

        // Stereo-only checkbox
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.range_scan_stereo_only, "")
                .on_hover_text("Stop only when 19 kHz stereo pilot detected (WBFM)");
            ui.label(
                RichText::new("Stereo only (pilot lock)")
                    .color(theme::TEXT_MUTED)
                    .small(),
            );
        });

        // Start / Stop button
        ui.horizontal(|ui| {
            if range_running {
                let stop_btn =
                    egui::Button::new(RichText::new("■  Stop").color(theme::DANGER).strong())
                        .fill(theme::WIDGET_BG);
                if ui.add_sized(Vec2::new(80.0, 22.0), stop_btn).clicked() {
                    let _ = self.cmd_tx.try_send(ScanCmd::Stop.into());
                }
                ui.label(
                    RichText::new("SCANNING FM")
                        .color(theme::STATUS_OK)
                        .small()
                        .strong(),
                );
            } else {
                let scan_color = if is_running {
                    theme::STATUS_OK
                } else {
                    theme::TEXT_MUTED
                };
                let start_btn =
                    egui::Button::new(RichText::new("▶  FM Scan").color(scan_color).strong())
                        .fill(theme::WIDGET_BG);
                let tip = if is_running {
                    format!(
                        "Sweep {:.1}–{:.1} MHz in {:.0} kHz steps",
                        self.range_scan_lo_hz as f64 / 1e6,
                        self.range_scan_hi_hz as f64 / 1e6,
                        self.range_scan_step_hz as f64 / 1e3
                    )
                } else {
                    "Start the radio first".to_string()
                };
                let resp = ui
                    .add_sized(Vec2::new(80.0, 22.0), start_btn)
                    .on_hover_text(tip);
                if resp.clicked() && is_running {
                    let _ = self.cmd_tx.try_send(
                        ScanCmd::StartRange {
                            freq_lo: self.range_scan_lo_hz,
                            freq_hi: self.range_scan_hi_hz,
                            step_hz: self.range_scan_step_hz,
                            dwell_secs: self.range_scan_dwell,
                            squelch_dbfs: self.range_scan_squelch,
                            mode: sdrapp_core::signal_path::DemodMode::Wbfm,
                            stereo_only: self.range_scan_stereo_only,
                        }
                        .into(),
                    );
                }
            }
        });

        // Status line when range scan is running
        if range_running {
            let (freq, signal) = {
                let s = self.shared.read();
                (s.scanner.range_freq_hz, s.fft.signal_level_dbfs)
            };
            ui.add_space(2.0);
            ui.label(
                RichText::new(format!(
                    "{:.3} MHz  {:.1} dBFS",
                    freq as f64 / 1_000_000.0,
                    signal
                ))
                .color(theme::ACCENT_DIM)
                .small(),
            );
        }

        ui.add_space(8.0);
        ui.separator();
        ui.add_space(6.0);

        // ── Device settings ────────────────────────────────────────────────────
        self.device_settings_section(ui, is_running, is_demo);
    }
}
