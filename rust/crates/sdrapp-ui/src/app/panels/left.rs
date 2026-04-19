use egui::{RichText, Stroke, Ui, Vec2};

use sdrapp_core::signal_path::{DemodMode, ReceiverCmd, ScanCmd, SignalPathCommand};

use super::super::SdrApp;
use crate::theme;

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
                // Restore saved demod mode so the signal path doesn't default to WBFM.
                let saved_mode = crate::app::parse_config_demod_mode(&self.config.ui.demod_mode.clone());
                let _ = self.cmd_tx.try_send(ReceiverCmd::SetDemodMode(saved_mode).into());
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
                    self.config.ui.tune_step_hz = hz;
                    self.config_dirty = true;
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
                    self.apply_tune(new_freq);
                }
            }
        });

        ui.add_space(6.0);
        ui.separator();
        ui.add_space(4.0);

        // ── FM Band Scan ──────────────────────────────────────────────────────
        ui.label(RichText::new("FM SCAN").color(theme::TEXT_MUTED).small());
        let range_running = {
            let s = self.shared.read();
            s.scanner.scan_running && s.scanner.range_mode
        };
        ui.horizontal(|ui| {
            ui.label(RichText::new("Lo").color(theme::TEXT_MUTED).small());
            let mut lo_mhz = self.range_scan_lo_hz as f64 / 1_000_000.0;
            if ui.add(egui::DragValue::new(&mut lo_mhz).range(70.0..=200.0).speed(0.1).suffix(" MHz")).changed() {
                self.range_scan_lo_hz = (lo_mhz * 1_000_000.0) as u64;
            }
            ui.label(RichText::new("Hi").color(theme::TEXT_MUTED).small());
            let mut hi_mhz = self.range_scan_hi_hz as f64 / 1_000_000.0;
            if ui.add(egui::DragValue::new(&mut hi_mhz).range(70.0..=200.0).speed(0.1).suffix(" MHz")).changed() {
                self.range_scan_hi_hz = (hi_mhz * 1_000_000.0) as u64;
            }
        });
        ui.horizontal(|ui| {
            ui.label(RichText::new("Step").color(theme::TEXT_MUTED).small());
            let mut step_khz = self.range_scan_step_hz as f64 / 1_000.0;
            if ui.add(egui::DragValue::new(&mut step_khz).range(10.0..=500.0).speed(10.0).suffix(" kHz")).changed() {
                self.range_scan_step_hz = (step_khz * 1_000.0) as u64;
            }
            ui.label(RichText::new("Dwell").color(theme::TEXT_MUTED).small());
            ui.add(egui::DragValue::new(&mut self.range_scan_dwell).range(0.1_f32..=5.0_f32).speed(0.05).suffix(" s"));
        });
        ui.horizontal(|ui| {
            ui.label(RichText::new("SQ").color(theme::TEXT_MUTED).small());
            ui.add(
                egui::Slider::new(&mut self.range_scan_squelch, -120.0_f32..=-10.0_f32)
                    .suffix(" dB")
                    .show_value(true),
            );
        });
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.range_scan_stereo_only, "")
                .on_hover_text("Stop only when 19 kHz stereo pilot detected (WBFM)");
            ui.label(RichText::new("Stereo only (pilot lock)").color(theme::TEXT_MUTED).small());
        });
        {
            let is_running = self.shared.read().is_running;
            ui.horizontal(|ui| {
                if range_running {
                    let stop_btn = egui::Button::new(RichText::new("■  Stop").color(theme::DANGER).strong())
                        .fill(theme::WIDGET_BG);
                    if ui.add_sized(Vec2::new(80.0, 22.0), stop_btn).clicked() {
                        let _ = self.cmd_tx.try_send(ScanCmd::Stop.into());
                    }
                    ui.label(RichText::new("SCANNING FM").color(theme::STATUS_OK).small().strong());
                } else {
                    let scan_color = if is_running { theme::STATUS_OK } else { theme::TEXT_MUTED };
                    let start_btn = egui::Button::new(RichText::new("▶  FM Scan").color(scan_color).strong())
                        .fill(theme::WIDGET_BG);
                    let tip = if is_running {
                        format!(
                            "Sweep {:.1}–{:.1} MHz in {:.0} kHz steps",
                            self.range_scan_lo_hz as f64 / 1e6,
                            self.range_scan_hi_hz as f64 / 1e6,
                            self.range_scan_step_hz as f64 / 1e3,
                        )
                    } else {
                        "Start the radio first".to_string()
                    };
                    let resp = ui.add_sized(Vec2::new(80.0, 22.0), start_btn).on_hover_text(tip);
                    if resp.clicked() && is_running {
                        let _ = self.cmd_tx.try_send(
                            ScanCmd::StartRange {
                                freq_lo: self.range_scan_lo_hz,
                                freq_hi: self.range_scan_hi_hz,
                                step_hz: self.range_scan_step_hz,
                                dwell_secs: self.range_scan_dwell,
                                squelch_dbfs: self.range_scan_squelch,
                                mode: DemodMode::Wbfm,
                                stereo_only: self.range_scan_stereo_only,
                            }
                            .into(),
                        );
                    }
                }
            });
            if range_running {
                let (freq, signal) = {
                    let s = self.shared.read();
                    (s.scanner.range_freq_hz, s.fft.signal_level_dbfs)
                };
                ui.label(
                    RichText::new(format!(
                        "{:.3} MHz  {:.1} dBFS",
                        freq as f64 / 1_000_000.0,
                        signal,
                    ))
                    .color(theme::ACCENT_DIM)
                    .small(),
                );
            }
        }

        ui.add_space(4.0);
        ui.separator();
        ui.add_space(4.0);

        // ── Bookmark Scanner ──────────────────────────────────────────────────
        ui.label(RichText::new("SCANNER").color(theme::TEXT_MUTED).small());
        {
            let is_running = self.shared.read().is_running;
            let (scan_running, scan_cursor, scan_bm_count) = {
                let s = self.shared.read();
                let cat = self.scan_cat_ui.clone();
                let count = s.bookmarks.iter().filter(|b| cat.is_empty() || b.category == cat).count();
                (s.scanner.scan_running, s.scanner.scan_cursor, count)
            };
            let scan_can_start = is_running && scan_bm_count > 0;
            ui.horizontal(|ui| {
                ui.label(RichText::new("Cat").color(theme::TEXT_MUTED).small());
                ui.text_edit_singleline(&mut self.scan_cat_ui)
                    .on_hover_text("Scan only this category (empty = all bookmarks)");
            });
            ui.horizontal(|ui| {
                ui.label(RichText::new("Dwell").color(theme::TEXT_MUTED).small());
                if ui.add(
                    egui::Slider::new(&mut self.scan_dwell_ui, 0.5_f32..=15.0_f32)
                        .suffix(" s")
                        .show_value(true),
                ).changed() {
                    let _ = self.cmd_tx.try_send(ScanCmd::SetDwell(self.scan_dwell_ui).into());
                }
            });
            ui.horizontal(|ui| {
                if scan_running {
                    let stop_btn = egui::Button::new(RichText::new("■  Stop").color(theme::DANGER).strong())
                        .fill(theme::WIDGET_BG);
                    if ui.add_sized(Vec2::new(70.0, 22.0), stop_btn).clicked() {
                        let _ = self.cmd_tx.try_send(ScanCmd::Stop.into());
                    }
                    if ui.small_button(RichText::new(">> Next").color(theme::TEXT_MUTED)).clicked() {
                        let _ = self.cmd_tx.try_send(ScanCmd::Next.into());
                    }
                    ui.label(RichText::new("SCAN").color(theme::STATUS_OK).small().strong());
                } else {
                    let start_color = if scan_can_start { theme::STATUS_OK } else { theme::TEXT_MUTED };
                    let start_btn = egui::Button::new(RichText::new("▶  Scan").color(start_color).strong())
                        .fill(theme::WIDGET_BG);
                    let tip = if !is_running {
                        "Start the radio first".to_string()
                    } else if scan_bm_count == 0 {
                        "No bookmarks to scan — add some first".to_string()
                    } else {
                        format!("Scan {scan_bm_count} bookmark(s)")
                    };
                    let start_resp = ui.add_sized(Vec2::new(70.0, 22.0), start_btn).on_hover_text(tip);
                    if start_resp.clicked() && scan_can_start {
                        let _ = self.cmd_tx.try_send(ScanCmd::SetDwell(self.scan_dwell_ui).into());
                        let _ = self.cmd_tx.try_send(ScanCmd::Start(self.scan_cat_ui.clone()).into());
                    }
                }
            });
            if scan_running {
                if let Some(label) = {
                    let s = self.shared.read();
                    s.bookmarks.get(scan_cursor).map(|b| {
                        format!("{} — {:.3} MHz", b.name, b.freq_hz as f64 / 1_000_000.0)
                    })
                } {
                    ui.add_space(2.0);
                    ui.label(RichText::new(label).color(theme::ACCENT_DIM).small());
                }
            } else if !is_running {
                ui.add_space(2.0);
                ui.label(RichText::new("Start the radio to enable scanning").color(theme::TEXT_MUTED).small());
            } else if scan_bm_count == 0 {
                ui.add_space(2.0);
                ui.label(RichText::new("Add bookmarks to enable scanning").color(theme::DANGER).small());
            }
        }

        ui.add_space(8.0);
        ui.separator();
        ui.add_space(6.0);

        // ── Rigctl (Hamlib) server ────────────────────────────────────────────
        ui.label(RichText::new("RIGCTL").color(theme::TEXT_MUTED).small());
        ui.add_space(4.0);

        let rigctl_enabled = self.config.rigctl.enabled;
        ui.horizontal(|ui| {
            let en_color = if rigctl_enabled { theme::STATUS_OK } else { theme::TEXT_MUTED };
            let en_label = RichText::new(if rigctl_enabled { "ON" } else { "OFF" })
                .color(en_color).small().strong();
            if ui.selectable_label(rigctl_enabled, en_label)
                .on_hover_text("Enable Hamlib-compatible CAT server (requires app restart to take effect)")
                .clicked()
            {
                self.config.rigctl.enabled = !rigctl_enabled;
                self.config_dirty = true;
            }
            if rigctl_enabled {
                ui.label(
                    RichText::new(format!("port {}", self.config.rigctl.port))
                        .color(theme::TEXT_MUTED).small(),
                );
            }
        });
        if rigctl_enabled {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Port").color(theme::TEXT_MUTED).small());
                let mut port = self.config.rigctl.port as i32;
                if ui.add(egui::DragValue::new(&mut port).range(1024..=65535)).changed() {
                    self.config.rigctl.port = port as u16;
                    self.config_dirty = true;
                }
            });
            ui.label(
                RichText::new("Connect: nc 127.0.0.1 <port>")
                    .color(theme::TEXT_DISABLED).small(),
            );
        }

        ui.add_space(8.0);
        ui.separator();
        ui.add_space(6.0);

        // ── Bookmarks ─────────────────────────────────────────────────────────
        self.bookmarks_section(ui);

        // ── Device Diagnostics ────────────────────────────────────────────────
        ui.add_space(4.0);
        ui.separator();
        ui.add_space(2.0);
        self.device_diagnostics_section(ui);

    }
}

