use egui::{RichText, Stroke, Ui, Vec2};

use sdrapp_core::signal_path::{DemodMode, ReceiverCmd, ScanCmd, SignalPathCommand};

use crate::{
    frequency::FrequencyWidget,
    knob::KnobWidget,
    theme,
};
use super::super::SdrApp;

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
            .or_else(|| {
                self.registry
                    .sources
                    .first()
                    .map(|s| s.display_name)
            })
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
                ("<",  -(step_hz as i64),       "-1x step  (or Down arrow key)"),
                (">",   step_hz as i64,          "+1x step  (or Up arrow key)"),
                (">>",  step_hz as i64 * 10,    "+10x step"),
            ] {
                if ui.add_sized(
                    Vec2::new(btn_w, 22.0),
                    egui::Button::new(RichText::new(label).color(theme::TEXT_PRIMARY))
                        .fill(theme::WIDGET_BG),
                ).on_hover_text(tip).clicked() {
                    let new_freq = (freq as i64 + delta).max(1) as u64;
                    let _ = self.cmd_tx.try_send(ReceiverCmd::SetFrequency(new_freq).into());
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

            ui.label(RichText::new("NFM SETTINGS").color(theme::TEXT_MUTED).small());
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
            let (sq_learn, sq_cc) = {
                let s = self.shared.read();
                let learn = s.midi_learn_target.as_deref() == Some("squelch");
                let cc = s.midi_cc_to_knob.iter().find(|(_, v)| v.as_str() == "squelch").map(|(&c, _)| c);
                (learn, cc)
            };
            let mut sq_learn_req = false;
            let mut sq_clear: Option<u8> = None;
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
                    learn_active: sq_learn,
                }.show(ui);
                resp.context_menu(|ui| {
                    if ui.button("Assign MIDI CC").clicked() { sq_learn_req = true; ui.close_menu(); }
                    if let Some(cc) = sq_cc {
                        if ui.button(format!("Clear CC {cc} binding")).clicked() { sq_clear = Some(cc); ui.close_menu(); }
                    }
                });
                if resp.changed() {
                    let _ = self.cmd_tx.try_send(ReceiverCmd::SetSquelchThreshold(sq_threshold).into());
                }
            });
            if sq_learn_req { self.shared.write().midi_learn_target = Some("squelch".into()); }
            if let Some(cc) = sq_clear { self.shared.write().midi_cc_to_knob.remove(&cc); self.config_dirty = true; }

            ui.add_space(4.0);

            // CTCSS tone squelch toggle
            let (ctcss_enabled, ctcss_detected) = {
                let s = self.shared.read();
                (s.demod.ctcss_squelch_enabled, s.demod.ctcss_tone_detected)
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

        let scan_running = self.shared.read().scanner.scan_running;

        // Category filter
        ui.horizontal(|ui| {
            ui.label(RichText::new("Cat").color(theme::TEXT_MUTED).small());
            ui.text_edit_singleline(&mut self.scan_cat_ui).on_hover_text("Scan only this category (empty = all bookmarks)");
        });

        // Dwell time
        ui.horizontal(|ui| {
            ui.label(RichText::new("Dwell").color(theme::TEXT_MUTED).small());
            if ui.add(egui::Slider::new(&mut self.scan_dwell_ui, 0.5_f32..=15.0_f32)
                .suffix(" s").show_value(true))
                .changed()
            {
                let _ = self.cmd_tx.try_send(ScanCmd::SetDwell(self.scan_dwell_ui).into());
            }
        });

        // Start / Stop / Next
        ui.horizontal(|ui| {
            if scan_running {
                let stop_btn = egui::Button::new(RichText::new("■  Stop").color(theme::DANGER).strong())
                    .fill(theme::WIDGET_BG);
                if ui.add_sized(Vec2::new(70.0, 22.0), stop_btn).clicked() {
                    let _ = self.cmd_tx.try_send(ScanCmd::Stop.into());
                }
                if ui.small_button(RichText::new("▶▶ Next").color(theme::TEXT_MUTED)).clicked() {
                    let _ = self.cmd_tx.try_send(ScanCmd::Next.into());
                }
                ui.label(RichText::new("SCAN").color(theme::STATUS_OK).small().strong());
            } else {
                let start_btn = egui::Button::new(RichText::new("▶  Scan").color(theme::STATUS_OK).strong())
                    .fill(theme::WIDGET_BG);
                if ui.add_sized(Vec2::new(70.0, 22.0), start_btn).clicked() {
                    let _ = self.cmd_tx.try_send(ScanCmd::SetDwell(self.scan_dwell_ui).into());
                    let _ = self.cmd_tx.try_send(ScanCmd::Start(self.scan_cat_ui.clone()).into());
                }
            }
        });

        ui.add_space(8.0);
        ui.separator();
        ui.add_space(6.0);

        // ── Device settings ────────────────────────────────────────────────────
        self.device_settings_section(ui, is_running, is_demo);
    }
}
