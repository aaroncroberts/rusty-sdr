//! Device settings UI: antenna, sample rate, AGC, LNA, IF gain, notch filters, RDS.

use egui::{RichText, Ui};

use sdrapp_core::signal_path::HardwareCommand;

use super::super::SdrApp;
use super::format_frequency;
use crate::{knob::KnobWidget, theme};

impl SdrApp {
    /// Render the DEVICE SETTINGS section of the left panel.
    ///
    /// `is_running` and `is_demo` are pre-read by `left_panel()` to avoid
    /// redundant lock acquisitions.
    pub(in crate::app) fn device_settings_section(
        &mut self,
        ui: &mut Ui,
        is_running: bool,
        is_demo: bool,
    ) {
        ui.label(
            RichText::new("DEVICE SETTINGS")
                .color(theme::TEXT_MUTED)
                .small(),
        );
        ui.add_space(4.0);

        // Antenna selector — disabled while running (hardware reconfig during stream can glitch)
        ui.horizontal(|ui| {
            ui.label(RichText::new("Antenna").color(theme::TEXT_MUTED).small());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_enabled_ui(!is_running, |ui| {
                    for port in ["A", "B", "C"] {
                        let selected = self.config.source.antenna == port;
                        let label = RichText::new(port).small();
                        let label = if selected {
                            label.color(theme::ACCENT).strong()
                        } else {
                            label.color(theme::TEXT_MUTED)
                        };
                        if ui
                            .selectable_label(selected, label)
                            .on_disabled_hover_text("Stop playback before switching antenna")
                            .clicked()
                            && !selected
                        {
                            self.config.source.antenna = port.into();
                            self.config_dirty = true;
                            let port_num: u8 = match port {
                                "B" => 1,
                                "C" => 2,
                                _ => 0,
                            };
                            tracing::info!(port, "antenna switched");
                            let _ = self
                                .cmd_tx
                                .try_send(HardwareCommand::SetAntenna(port_num).into());
                        }
                    }
                });
            });
        });

        // Sample rate dropdown — requires restart to take effect.
        // Disabled while running to avoid confusing partial state.
        ui.horizontal(|ui| {
            ui.label(RichText::new("Rate").color(theme::TEXT_MUTED).small());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // RSPdx-R2 in ZeroIF mode requires >= 2 MHz sample rate.
                // Lower rates are valid when LowIF mode is selected.
                let rates: &[(u32, &str)] = if is_demo {
                    &[
                        (200_000, "200k"),
                        (500_000, "500k"),
                        (1_000_000, "1M"),
                        (2_000_000, "2M"),
                    ]
                } else {
                    &[
                        (2_000_000, "2M"),
                        (6_000_000, "6M"),
                        (8_000_000, "8M"),
                        (10_000_000, "10M"),
                    ]
                };
                let current = rates
                    .iter()
                    .find(|&&(r, _)| r == self.config.source.sample_rate_sps)
                    .map(|&(_, label)| label)
                    .unwrap_or("?");

                ui.add_enabled_ui(!is_running, |ui| {
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
                })
                .response
                .on_disabled_hover_text(
                    "Stop playback before changing the sample rate — the device must reinitialize.",
                );
            });
        });

        // IF mode dropdown — requires restart to take effect.
        // Disabled while running; changing IF mode alters the sample-rate valid range.
        ui.horizontal(|ui| {
            ui.label(RichText::new("IF Mode").color(theme::TEXT_MUTED).small());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let modes: &[(&str, &str)] = &[
                    ("ZeroIF",      "Zero-IF"),
                    ("LowIF200kHz", "Low 200k"),
                    ("LowIF500kHz", "Low 500k"),
                    ("LowIF1MHz",   "Low 1M"),
                    ("LowIF2MHz",   "Low 2M"),
                ];
                let current = modes
                    .iter()
                    .find(|&&(k, _)| k == self.config.source.if_mode)
                    .map(|&(_, label)| label)
                    .unwrap_or("?");

                ui.add_enabled_ui(!is_running, |ui| {
                    egui::ComboBox::from_id_salt("if_mode")
                        .selected_text(RichText::new(current).small())
                        .width(80.0)
                        .show_ui(ui, |ui| {
                            for &(key, label) in modes {
                                let selected = key == self.config.source.if_mode;
                                if ui.selectable_label(selected, label).clicked() {
                                    self.config.source.if_mode = key.into();
                                    self.config_dirty = true;
                                }
                            }
                        });
                })
                .response
                .on_disabled_hover_text(
                    "Stop playback before changing IF mode — the device must reinitialize.",
                );
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
                    let _ = self
                        .cmd_tx
                        .try_send(HardwareCommand::SetAgcEnabled(*agc).into());
                }
            });
        });

        // LNA state — always shown; disabled when AGC is active.
        // RSPdx-R2: 0–9 normal; 0–3 in HDR mode.
        {
            let lna_max = if self.config.source.hdr_mode {
                3_i32
            } else {
                9_i32
            };
            // Clamp persisted value in case it exceeds the current mode's limit.
            if self.config.source.lna_state as i32 > lna_max {
                self.config.source.lna_state = lna_max as u8;
                self.config_dirty = true;
            }
            let agc_active = self.config.source.agc_enabled;
            // LNA + IF Gain knobs side by side
            let (lna_learn, lna_cc) = {
                let s = self.shared.read();
                (
                    s.midi_learn_target.as_deref() == Some("lna"),
                    s.midi_cc_to_knob
                        .iter()
                        .find(|(_, v)| v.as_str() == "lna")
                        .map(|(&c, _)| c),
                )
            };
            let (ifg_learn, ifg_cc) = {
                let s = self.shared.read();
                (
                    s.midi_learn_target.as_deref() == Some("if_gain"),
                    s.midi_cc_to_knob
                        .iter()
                        .find(|(_, v)| v.as_str() == "if_gain")
                        .map(|(&c, _)| c),
                )
            };
            let mut lna_learn_req = false;
            let mut lna_learn_cancel = false;
            let mut lna_clear: Option<u8> = None;
            let mut ifg_learn_req = false;
            let mut ifg_learn_cancel = false;
            let mut ifg_clear: Option<u8> = None;
            if agc_active {
                ui.label(
                    RichText::new("LNA / IF — managed by AGC")
                        .color(theme::TEXT_MUTED)
                        .small(),
                )
                .on_hover_text("Disable AGC to adjust LNA and IF gain manually");
            } else {
            ui.horizontal(|ui| {
                let knob_w = (ui.available_width() / 2.0).min(60.0);
                ui.allocate_ui(egui::Vec2::new(knob_w, 72.0), |ui| {
                    ui.vertical_centered(|ui| {
                        let mut lna = self.config.source.lna_state as f32;
                        let resp = KnobWidget {
                            value: &mut lna,
                            range: 0.0..=(lna_max as f32),
                            default_value: 4.0,
                            step: 1.0,
                            diameter: 40.0,
                            label: Some("LNA"),
                            unit: "",
                            midi_cc: lna_cc,
                            learn_active: lna_learn,
                        }
                        .show(ui);
                        resp.context_menu(|ui| {
                            if lna_learn {
                                if ui.button("Cancel MIDI Learn").clicked() {
                                    lna_learn_cancel = true;
                                    ui.close_menu();
                                }
                            } else if ui.button("Assign MIDI CC").clicked() {
                                lna_learn_req = true;
                                ui.close_menu();
                            }
                            if let Some(cc) = lna_cc {
                                if ui.button(format!("Clear CC {cc} binding")).clicked() {
                                    lna_clear = Some(cc);
                                    ui.close_menu();
                                }
                            }
                        });
                        if resp.changed() {
                            let new_lna = lna.round() as u8;
                            self.config.source.lna_state = new_lna;
                            self.config_dirty = true;
                            let _ = self
                                .cmd_tx
                                .try_send(HardwareCommand::SetLnaState(new_lna).into());
                        }
                    });
                });
                ui.allocate_ui(egui::Vec2::new(knob_w, 72.0), |ui| {
                    ui.vertical_centered(|ui| {
                        let mut gain = self.config.source.if_gain_dbfs as f32;
                        let resp = KnobWidget {
                            value: &mut gain,
                            range: -59.0_f32..=0.0_f32,
                            default_value: -34.0,
                            step: 1.0,
                            diameter: 40.0,
                            label: Some("IF"),
                            unit: "dBFS",
                            midi_cc: ifg_cc,
                            learn_active: ifg_learn,
                        }
                        .show(ui);
                        resp.context_menu(|ui| {
                            if ifg_learn {
                                if ui.button("Cancel MIDI Learn").clicked() {
                                    ifg_learn_cancel = true;
                                    ui.close_menu();
                                }
                            } else if ui.button("Assign MIDI CC").clicked() {
                                ifg_learn_req = true;
                                ui.close_menu();
                            }
                            if let Some(cc) = ifg_cc {
                                if ui.button(format!("Clear CC {cc} binding")).clicked() {
                                    ifg_clear = Some(cc);
                                    ui.close_menu();
                                }
                            }
                        });
                        if resp.changed() {
                            let new_gain = gain.round() as i32;
                            self.config.source.if_gain_dbfs = new_gain;
                            self.config_dirty = true;
                            let _ = self
                                .cmd_tx
                                .try_send(HardwareCommand::SetIfGain(new_gain).into());
                        }
                    });
                });
            }); // ui.horizontal
            } // else (AGC off)
            if lna_learn_req {
                self.shared.write().midi_learn_target = Some("lna".into());
            }
            if lna_learn_cancel {
                self.shared.write().midi_learn_target = None;
            }
            if let Some(cc) = lna_clear {
                self.shared.write().midi_cc_to_knob.remove(&cc);
                self.config_dirty = true;
            }
            if ifg_learn_req {
                self.shared.write().midi_learn_target = Some("if_gain".into());
            }
            if ifg_learn_cancel {
                self.shared.write().midi_learn_target = None;
            }
            if let Some(cc) = ifg_clear {
                self.shared.write().midi_cc_to_knob.remove(&cc);
                self.config_dirty = true;
            }
        }

        // AGC setpoint knob (only when AGC is on)
        if self.config.source.agc_enabled {
            let (sp_learn, sp_cc) = {
                let s = self.shared.read();
                (
                    s.midi_learn_target.as_deref() == Some("agc_setpoint"),
                    s.midi_cc_to_knob
                        .iter()
                        .find(|(_, v)| v.as_str() == "agc_setpoint")
                        .map(|(&c, _)| c),
                )
            };
            let mut sp_learn_req = false;
            let mut sp_learn_cancel = false;
            let mut sp_clear: Option<u8> = None;
            ui.vertical_centered(|ui| {
                let mut sp = self.config.source.agc_setpoint_dbfs as f32;
                let resp = KnobWidget {
                    value: &mut sp,
                    range: -60.0_f32..=0.0_f32,
                    default_value: -50.0,
                    step: 1.0,
                    diameter: 40.0,
                    label: Some("AGC Level"),
                    unit: "dBFS",
                    midi_cc: sp_cc,
                    learn_active: sp_learn,
                }
                .show(ui);
                resp.context_menu(|ui| {
                    if sp_learn {
                        if ui.button("Cancel MIDI Learn").clicked() {
                            sp_learn_cancel = true;
                            ui.close_menu();
                        }
                    } else if ui.button("Assign MIDI CC").clicked() {
                        sp_learn_req = true;
                        ui.close_menu();
                    }
                    if let Some(cc) = sp_cc {
                        if ui.button(format!("Clear CC {cc} binding")).clicked() {
                            sp_clear = Some(cc);
                            ui.close_menu();
                        }
                    }
                });
                if resp.changed() {
                    let new_sp = sp.round() as i32;
                    self.config.source.agc_setpoint_dbfs = new_sp;
                    self.config_dirty = true;
                    let _ = self
                        .cmd_tx
                        .try_send(HardwareCommand::SetAgcSetpoint(new_sp).into());
                }

                // Max Attenuation: one-click saturated-input fix — LNA=9, setpoint=−60.
                if ui
                    .add(
                        egui::Button::new(RichText::new("Max Atten").small())
                            .fill(egui::Color32::from_rgb(80, 30, 30)),
                    )
                    .on_hover_text("Set LNA = 9 and AGC Level = −60 dBFS to handle strong inputs")
                    .clicked()
                {
                    self.config.source.agc_setpoint_dbfs = -60;
                    self.config_dirty = true;
                    let _ = self
                        .cmd_tx
                        .try_send(HardwareCommand::SetLnaState(9).into());
                    let _ = self
                        .cmd_tx
                        .try_send(HardwareCommand::SetAgcSetpoint(-60).into());
                }
            });
            if sp_learn_req {
                self.shared.write().midi_learn_target = Some("agc_setpoint".into());
            }
            if sp_learn_cancel {
                self.shared.write().midi_learn_target = None;
            }
            if let Some(cc) = sp_clear {
                self.shared.write().midi_cc_to_knob.remove(&cc);
                self.config_dirty = true;
            }
        }

        // Hardware-only advanced controls (not shown in demo mode)
        if !is_demo {
            ui.add_space(4.0);

            // Bias-T, HDR mode
            ui.horizontal(|ui| {
                // Bias-T
                let bias_t = self.config.source.bias_t_enabled;
                let label = RichText::new("Bias-T").small();
                let label = if bias_t {
                    label.color(theme::ACCENT).strong()
                } else {
                    label.color(theme::TEXT_MUTED)
                };
                if ui
                    .selectable_label(bias_t, label)
                    .on_hover_text(
                        "Enable Bias-T 4.7 V supply on coax connector for active antennas.",
                    )
                    .clicked()
                {
                    self.config.source.bias_t_enabled = !bias_t;
                    self.config_dirty = true;
                    let _ = self
                        .cmd_tx
                        .try_send(HardwareCommand::SetBiasT(!bias_t).into());
                }

                ui.add_space(6.0);

                // HDR mode — RSPdx-R2 only supports HDR below 2 MHz
                let hdr = self.config.source.hdr_mode;
                let freq_too_high_for_hdr = self.config.ui.frequency_hz > 2_000_000;
                let hdr_color = if hdr && !freq_too_high_for_hdr {
                    theme::ACCENT
                } else if freq_too_high_for_hdr {
                    theme::TEXT_DISABLED
                } else {
                    theme::TEXT_MUTED
                };
                let label = RichText::new("HDR").small().color(hdr_color);
                let tip = if freq_too_high_for_hdr {
                    "HDR not available above 2 MHz — tune below 2 MHz first."
                } else {
                    "High Dynamic Range mode — improves ADC performance below 2 MHz."
                };
                if ui
                    .add_enabled(
                        !freq_too_high_for_hdr,
                        egui::SelectableLabel::new(hdr, label),
                    )
                    .on_hover_text(tip)
                    .on_disabled_hover_text(tip)
                    .clicked()
                {
                    self.config.source.hdr_mode = !hdr;
                    self.config_dirty = true;
                    let _ = self
                        .cmd_tx
                        .try_send(HardwareCommand::SetHdrMode(!hdr).into());
                }
            });

            // AM notch, FM notch
            ui.horizontal(|ui| {
                let am = self.config.source.am_notch_enabled;
                let label = RichText::new("AM notch").small();
                let label = if am { label.color(theme::ACCENT).strong() } else { label.color(theme::TEXT_MUTED) };
                if ui.selectable_label(am, label)
                    .on_hover_text("AM broadcast notch filter — reduces LW/MW overload interference.")
                    .clicked()
                {
                    self.config.source.am_notch_enabled = !am;
                    self.config_dirty = true;
                    let _ = self.cmd_tx.try_send(HardwareCommand::SetAmNotch(!am).into());
                }

                ui.add_space(6.0);

                let fm = self.config.source.fm_notch_enabled;
                let label = RichText::new("FM notch").small();
                let label = if fm { label.color(theme::ACCENT).strong() } else { label.color(theme::TEXT_MUTED) };
                if ui.selectable_label(fm, label)
                    .on_hover_text("FM broadcast / DAB notch filter — reduces overload from strong FM stations.")
                    .clicked()
                {
                    self.config.source.fm_notch_enabled = !fm;
                    self.config_dirty = true;
                    let _ = self.cmd_tx.try_send(HardwareCommand::SetFmNotch(!fm).into());
                }
            });

            // Safety warning: FM notch is dangerous when tuned to the FM broadcast band.
            let freq_hz = self.config.ui.frequency_hz;
            let in_fm_band = freq_hz >= 87_000_000 && freq_hz <= 108_000_000;
            let fm = self.config.source.fm_notch_enabled;
            if fm && in_fm_band {
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("⚠ FM notch active — this filter cuts your listening band")
                            .color(egui::Color32::from_rgb(240, 165, 0))
                            .small(),
                    );
                });
            }
        }

        // Signal path status + RDS display
        let (center_freq, is_recording, rds_ps_name, rds_pty, rds_ta, rds_rt) = {
            let s = self.shared.read();
            (
                s.center_freq_hz,
                s.is_recording,
                s.rds.ps_name.clone(),
                s.rds
                    .pty
                    .map(|c| sdrapp_core::dsp::rds::pty_to_str(c).to_string()),
                s.rds.ta,
                s.rds.rt.clone(),
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
                ui.label(RichText::new("RDS").color(theme::TEXT_MUTED));
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
                    ui.label(RichText::new(rt_display).color(theme::TEXT_MUTED).small())
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
}
