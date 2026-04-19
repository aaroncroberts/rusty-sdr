use egui::{RichText, Stroke, Ui, Vec2};

use rusty_sdr_core::signal_path::{ReceiverCmd, SignalPathCommand};

use super::super::SdrApp;
use crate::theme;

impl SdrApp {
    pub(in crate::app) fn left_panel(&mut self, ui: &mut Ui) {
        ui.add_space(4.0);

        // App title with version
        ui.horizontal(|ui| {
            ui.add_space(2.0);
            ui.label(
                RichText::new("Rusty SDR")
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

        // ── MIDI Controller ───────────────────────────────────────────────────
        self.midi_section(ui);
        ui.add_space(2.0);
        ui.separator();
        ui.add_space(4.0);

        // ── Bookmarks ─────────────────────────────────────────────────────────
        self.bookmarks_section(ui);

        // ── Device Diagnostics ────────────────────────────────────────────────
        ui.add_space(4.0);
        ui.separator();
        ui.add_space(2.0);
        self.device_diagnostics_section(ui);

    }
}

