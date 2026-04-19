#![allow(clippy::too_many_lines)]

use egui::{Color32, RichText, Stroke, Ui, Vec2};

use sdrapp_core::signal_path::{DemodMode, DisplayCmd, HardwareCommand, ReceiverCmd, RecordingMode, ScanCmd, SignalPathCommand};
use sdrapp_recorder::RecorderCommand;

use super::super::SdrApp;
use crate::{frequency::FrequencyWidget, knob::KnobWidget, theme};

/// Band preset: name, center frequency in Hz, span in Hz, and expected demod mode.
struct BandPreset {
    name: &'static str,
    center_hz: u64,
    span_hz: u64,
    /// Demod mode most appropriate for this band.  Applied on preset click so
    /// the user doesn't have to manually switch after tuning.
    demod_mode: DemodMode,
}

const BAND_PRESETS: &[BandPreset] = &[
    BandPreset {
        name: "FM Broadcast",
        center_hz: 97_500_000,
        span_hz: 10_500_000,
        demod_mode: DemodMode::Wbfm,
    },
    BandPreset {
        name: "Aviation VOR",
        center_hz: 113_000_000,
        span_hz: 5_000_000,
        demod_mode: DemodMode::Nfm,
    },
    BandPreset {
        name: "Air Traffic",
        center_hz: 127_500_000,
        span_hz: 9_500_000,
        demod_mode: DemodMode::Nfm,
    },
    BandPreset {
        name: "NOAA Weather",
        center_hz: 162_400_000,
        span_hz: 500_000,
        demod_mode: DemodMode::Nfm,
    },
    BandPreset {
        name: "AIS Marine",
        center_hz: 161_975_000,
        span_hz: 500_000,
        demod_mode: DemodMode::Nfm,
    },
    BandPreset {
        name: "Ham 2m",
        center_hz: 146_000_000,
        span_hz: 4_000_000,
        demod_mode: DemodMode::Nfm,
    },
    BandPreset {
        name: "ISM 433 MHz",
        center_hz: 433_920_000,
        span_hz: 2_000_000,
        demod_mode: DemodMode::Nfm,
    },
];

impl SdrApp {
    pub(in crate::app) fn right_panel(&mut self, ui: &mut Ui) {
        ui.add_space(4.0);

        // ── Volume + VU meter ─────────────────────────────────────────────────
        ui.label(RichText::new("VOLUME").color(theme::TEXT_MUTED).small());
        ui.add_space(4.0);

        let (vol_learn, vol_cc, map_pending, is_running) = {
            let s = self.shared.read();
            let learn = s.midi_learn_target.as_deref() == Some("volume");
            let cc = s
                .midi_cc_to_knob
                .iter()
                .find(|(_, v)| v.as_str() == "volume")
                .map(|(&c, _)| c);
            (learn, cc, s.midi_map_pending.is_some(), s.is_running)
        };
        let mut vol_learn_req = false;
        let mut vol_learn_cancel = false;
        let mut vol_clear: Option<u8> = None;
        let mut vol_bind = false;
        ui.vertical_centered(|ui| {
            let mut vol = self.config.ui.volume;
            let resp = KnobWidget {
                value: &mut vol,
                range: 0.0..=1.0,
                default_value: 0.8,
                step: 0.02,
                diameter: 52.0,
                label: Some("VOL"),
                unit: "%",
                midi_cc: vol_cc,
                learn_active: vol_learn || map_pending,
            }
            .show(ui);
            if map_pending && resp.clicked() {
                vol_bind = true;
            } else {
                resp.context_menu(|ui| {
                    if vol_learn {
                        if ui.button("Cancel MIDI Learn").clicked() {
                            vol_learn_cancel = true;
                            ui.close_menu();
                        }
                    } else if ui.button("Assign MIDI CC").clicked() {
                        vol_learn_req = true;
                        ui.close_menu();
                    }
                    if let Some(cc) = vol_cc {
                        if ui.button(format!("Clear CC {cc} binding")).clicked() {
                            vol_clear = Some(cc);
                            ui.close_menu();
                        }
                    }
                });
            }
            if resp.changed() {
                self.config.ui.volume = vol;
                // Always send the real volume; signal path applies it independently
                // of mute state so the knob position is preserved across mute cycles.
                let _ = self.cmd_tx.try_send(ReceiverCmd::SetVolume(vol).into());
                self.config_dirty = true;
            }

            // Mute toggle button
            let mute_label = if self.muted { "Unmute" } else { "Mute" };
            let mute_color = if self.muted { theme::AMBER } else { theme::TEXT_MUTED };
            if ui
                .add(
                    egui::Button::new(RichText::new(mute_label).small().color(mute_color))
                        .fill(theme::WIDGET_BG)
                        .stroke(egui::Stroke::new(1.0, if self.muted { theme::AMBER } else { theme::BORDER })),
                )
                .on_hover_text(if self.muted { "Unmute audio" } else { "Mute audio" })
                .clicked()
            {
                self.muted = !self.muted;
                self.adsb_did_mute = false; // manual toggle clears ADS-B mute ownership
                let _ = self.cmd_tx.try_send(ReceiverCmd::SetMuted(self.muted).into());
            }
        });
        if vol_bind {
            if let Some(cc) = self.shared.write().midi_map_pending.take() {
                self.shared.write().midi_cc_to_knob.insert(cc, "volume".into());
                self.config_dirty = true;
            }
        }
        if vol_learn_req {
            self.shared.write().midi_learn_target = Some("volume".into());
        }
        if vol_learn_cancel {
            self.shared.write().midi_learn_target = None;
        }
        if let Some(cc) = vol_clear {
            self.shared.write().midi_cc_to_knob.remove(&cc);
            self.config_dirty = true;
        }

        // VU meter (stereo bars)
        let level = self.vu_peak * self.config.ui.volume;
        self.draw_vu_meter(ui, level, level * 0.92, self.muted, is_running);

        // ── ADS-B Flight Tracker ──────────────────────────────────────────────
        ui.add_space(8.0);
        ui.separator();
        ui.add_space(6.0);

        {
            let decoder_running = self.adsb_decoder
                .as_ref()
                .map(|d| d.is_running())
                .unwrap_or(false);
            let count = self.adsb_store.lock().len();

            ui.horizontal(|ui| {
                ui.label(RichText::new("ADS-B").color(theme::TEXT_MUTED).small());
                if decoder_running && count > 0 {
                    ui.label(
                        RichText::new(format!("{count} ac"))
                            .color(theme::STATUS_OK)
                            .small()
                            .strong(),
                    );
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // Map toggle button — Start/Stop controls live inside the map window.
                    let map_lbl = if self.show_adsb_map { "^ Map" } else { "Map" };
                    let map_color = if decoder_running { theme::STATUS_OK } else { theme::ACCENT };
                    let map_btn = egui::Button::new(
                        RichText::new(map_lbl).color(map_color).small(),
                    )
                    .fill(theme::WIDGET_BG)
                    .stroke(Stroke::new(
                        1.0,
                        if self.show_adsb_map { map_color } else { theme::BORDER },
                    ));
                    if ui
                        .add(map_btn)
                        .on_hover_text("Open / close ADS-B aircraft map")
                        .clicked()
                    {
                        self.show_adsb_map = !self.show_adsb_map;
                    }
                });
            });
        }

        // ── FM Band Scan ──────────────────────────────────────────────────────
        ui.add_space(8.0);
        ui.separator();
        ui.add_space(6.0);
        ui.label(RichText::new("FM SCAN").color(theme::TEXT_MUTED).small());
        {
            let is_running = self.shared.read().is_running;
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
                ui.add(egui::Slider::new(&mut self.range_scan_squelch, -120.0_f32..=-10.0_f32).suffix(" dB").show_value(true));
            });
            ui.horizontal(|ui| {
                ui.checkbox(&mut self.range_scan_stereo_only, "")
                    .on_hover_text("Stop only when 19 kHz stereo pilot detected (WBFM)");
                ui.label(RichText::new("Stereo only (pilot lock)").color(theme::TEXT_MUTED).small());
            });
            ui.horizontal(|ui| {
                if range_running {
                    let stop_btn = egui::Button::new(RichText::new("■  Stop").color(theme::DANGER).strong()).fill(theme::WIDGET_BG);
                    if ui.add_sized(Vec2::new(80.0, 22.0), stop_btn).clicked() {
                        let _ = self.cmd_tx.try_send(ScanCmd::Stop.into());
                    }
                    ui.label(RichText::new("SCANNING FM").color(theme::STATUS_OK).small().strong());
                } else {
                    let scan_color = if is_running { theme::STATUS_OK } else { theme::TEXT_MUTED };
                    let start_btn = egui::Button::new(RichText::new("▶  FM Scan").color(scan_color).strong()).fill(theme::WIDGET_BG);
                    let tip = if is_running {
                        format!("Sweep {:.1}–{:.1} MHz in {:.0} kHz steps",
                            self.range_scan_lo_hz as f64 / 1e6,
                            self.range_scan_hi_hz as f64 / 1e6,
                            self.range_scan_step_hz as f64 / 1e3)
                    } else {
                        "Start the radio first".to_string()
                    };
                    if ui.add_sized(Vec2::new(80.0, 22.0), start_btn).on_hover_text(tip).clicked() && is_running {
                        let _ = self.cmd_tx.try_send(ScanCmd::StartRange {
                            freq_lo: self.range_scan_lo_hz,
                            freq_hi: self.range_scan_hi_hz,
                            step_hz: self.range_scan_step_hz,
                            dwell_secs: self.range_scan_dwell,
                            squelch_dbfs: self.range_scan_squelch,
                            mode: DemodMode::Wbfm,
                            stereo_only: self.range_scan_stereo_only,
                        }.into());
                    }
                }
            });
            if range_running {
                let (freq, signal) = {
                    let s = self.shared.read();
                    (s.scanner.range_freq_hz, s.fft.signal_level_dbfs)
                };
                ui.label(RichText::new(format!("{:.3} MHz  {:.1} dBFS", freq as f64 / 1_000_000.0, signal)).color(theme::ACCENT_DIM).small());
            }
        }

        // ── Bookmark Scanner ──────────────────────────────────────────────────
        ui.add_space(8.0);
        ui.separator();
        ui.add_space(6.0);
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
                if ui.add(egui::Slider::new(&mut self.scan_dwell_ui, 0.5_f32..=15.0_f32).suffix(" s").show_value(true)).changed() {
                    let _ = self.cmd_tx.try_send(ScanCmd::SetDwell(self.scan_dwell_ui).into());
                }
            });
            ui.horizontal(|ui| {
                if scan_running {
                    let stop_btn = egui::Button::new(RichText::new("■  Stop").color(theme::DANGER).strong()).fill(theme::WIDGET_BG);
                    if ui.add_sized(Vec2::new(70.0, 22.0), stop_btn).clicked() {
                        let _ = self.cmd_tx.try_send(ScanCmd::Stop.into());
                    }
                    if ui.small_button(RichText::new(">> Next").color(theme::TEXT_MUTED)).clicked() {
                        let _ = self.cmd_tx.try_send(ScanCmd::Next.into());
                    }
                    ui.label(RichText::new("SCAN").color(theme::STATUS_OK).small().strong());
                } else {
                    let start_color = if scan_can_start { theme::STATUS_OK } else { theme::TEXT_MUTED };
                    let start_btn = egui::Button::new(RichText::new("▶  Scan").color(start_color).strong()).fill(theme::WIDGET_BG);
                    let tip = if !is_running {
                        "Start the radio first".to_string()
                    } else if scan_bm_count == 0 {
                        "No bookmarks to scan — add some first".to_string()
                    } else {
                        format!("Scan {scan_bm_count} bookmark(s)")
                    };
                    if ui.add_sized(Vec2::new(70.0, 22.0), start_btn).on_hover_text(tip).clicked() && scan_can_start {
                        let _ = self.cmd_tx.try_send(ScanCmd::SetDwell(self.scan_dwell_ui).into());
                        let _ = self.cmd_tx.try_send(ScanCmd::Start(self.scan_cat_ui.clone()).into());
                    }
                }
            });
            if scan_running {
                if let Some(label) = {
                    let s = self.shared.read();
                    s.bookmarks.get(scan_cursor).map(|b| format!("{} — {:.3} MHz", b.name, b.freq_hz as f64 / 1_000_000.0))
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

        // ── RDS (FM only) ─────────────────────────────────────────────────────
        let (demod_mode, rds_ps, rds_pty, rds_ta, rds_rt) = {
            let s = self.shared.read();
            let pty = s.rds.pty.map(|c| sdrapp_core::dsp::rds::pty_to_str(c).to_string());
            (s.demod.demod_mode, s.rds.ps_name.clone(), pty, s.rds.ta, s.rds.rt.clone())
        };

        if demod_mode == DemodMode::Wbfm {
            ui.add_space(8.0);
            ui.separator();
            ui.add_space(6.0);

            ui.label(RichText::new("RDS").color(theme::TEXT_MUTED).small());
            ui.add_space(4.0);

            if rds_ps.is_some() || rds_rt.is_some() {
                // Station name + PTY + TA badge
                ui.horizontal(|ui| {
                    if let Some(ref ps) = rds_ps {
                        ui.label(RichText::new(ps).color(theme::TEXT_PRIMARY).strong());
                    }
                    if let Some(ref pty) = rds_pty {
                        ui.label(RichText::new(pty).color(theme::TEXT_MUTED).small());
                    }
                    if rds_ta {
                        ui.label(RichText::new("TA").color(theme::AMBER).small().strong());
                    }
                });
                // RadioText (scrolling song/program text)
                if let Some(ref rt) = rds_rt {
                    let rt_display = if rt.len() > 28 {
                        format!("{}…", &rt[..27])
                    } else {
                        rt.clone()
                    };
                    ui.label(
                        RichText::new(rt_display).color(theme::TEXT_MUTED).small(),
                    )
                    .on_hover_text(rt.as_str());
                }
            } else {
                ui.label(
                    RichText::new("Waiting for signal…")
                        .color(theme::TEXT_DISABLED)
                        .small(),
                );
            }
        }

        // ── Band Presets (collapsible 2-column grid) ──────────────────────────
        let mut tuned: Option<(u64, u64, DemodMode)> = None;
        ui.collapsing(
            RichText::new("BAND PRESETS").color(theme::TEXT_MUTED).small(),
            |ui| {
                let btn_w = (ui.available_width() - 4.0) / 2.0;
                let mut col = 0;
                ui.horizontal_wrapped(|ui| {
                    for preset in BAND_PRESETS {
                        let btn = egui::Button::new(
                            RichText::new(preset.name).color(theme::TEXT_PRIMARY).small(),
                        )
                        .fill(theme::WIDGET_BG)
                        .stroke(Stroke::new(1.0, theme::BORDER));
                        if ui.add_sized(Vec2::new(btn_w, 18.0), btn).clicked() {
                            tuned = Some((preset.center_hz, preset.span_hz, preset.demod_mode));
                        }
                        col += 1;
                        if col % 2 == 0 {
                            ui.end_row();
                        }
                    }
                });
            },
        );

        if let Some((hz, span, mode)) = tuned {
            // Band preset = user is tuning away from any bookmark antenna override
            self.restore_bookmark_antenna();
            let _ = self.cmd_tx.try_send(ReceiverCmd::SetFrequency(hz).into());
            let _ = self.cmd_tx.try_send(ReceiverCmd::SetDemodMode(mode).into());
            self.config.ui.frequency_hz = hz;
            self.config.ui.span_hz = span;
            self.config.ui.demod_mode = format!("{mode:?}");
            self.frequency_widget = FrequencyWidget::new(hz);
            let sr_half = self.shared.read().sample_rate_sps as u64 / 2;
            if sr_half > 0 {
                let z = (span as f32 / sr_half as f32).clamp(0.005, 1.0);
                let _ = self.cmd_tx.try_send(DisplayCmd::SetZoom(z).into());
                self.config.ui.zoom_level = z;
            }
            self.config_dirty = true;
        }

        ui.add_space(4.0);
        ui.separator();
        ui.add_space(6.0);

        // ── Recorder ─────────────────────────────────────────────────────────
        ui.label(RichText::new("RECORDER").color(theme::TEXT_MUTED).small());
        ui.add_space(4.0);

        let (is_recording, rec_mode, center_freq, iq_sr, rec_error, rec_peak_db, rec_rms_db) = {
            let s = self.shared.read();
            (
                s.is_recording,
                s.recording_mode,
                s.center_freq_hz,
                s.sample_rate_sps,
                s.recorder_error.clone(),
                s.recording_peak_dbfs,
                s.recording_rms_dbfs,
            )
        };

        if is_recording {
            // ── Active recording: stop button + level meters ──────────────────
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
                if self
                    .recorder_cmd_tx
                    .try_send(RecorderCommand::Stop)
                    .is_err()
                {
                    tracing::error!("failed to send StopRecording to recorder");
                }
                if self
                    .cmd_tx
                    .try_send(SignalPathCommand::StopRecording)
                    .is_err()
                {
                    tracing::error!("failed to send StopRecording to signal path");
                }
            }

            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("●").color(theme::DANGER));
                ui.label(RichText::new("REC").color(theme::AMBER).strong());
                ui.add_space(6.0);
                self.draw_recording_level_bars(ui, rec_peak_db, rec_rms_db);
            });
        } else {
            // ── Idle: mode selector inline above start button ─────────────────
            ui.horizontal(|ui| {
                ui.label(RichText::new("Mode:").color(theme::TEXT_MUTED).small());
                for mode in [
                    RecordingMode::AudioOnly,
                    RecordingMode::IqOnly,
                    RecordingMode::Both,
                ] {
                    let selected = rec_mode == mode;
                    let label = match mode {
                        RecordingMode::AudioOnly => "Audio",
                        RecordingMode::IqOnly => "IQ",
                        RecordingMode::Both => "Both",
                    };
                    if ui.selectable_label(selected, RichText::new(label).small()).clicked() {
                        self.shared.write().recording_mode = mode;
                    }
                }
            });
            ui.add_space(3.0);

            let rec_btn =
                egui::Button::new(RichText::new("●  Start Recording").color(theme::STATUS_OK))
                    .fill(theme::WIDGET_BG)
                    .stroke(Stroke::new(1.0, theme::STATUS_OK));

            if ui
                .add_sized(Vec2::new(ui.available_width(), 28.0), rec_btn)
                .clicked()
            {
                tracing::info!(freq_hz = center_freq, ?rec_mode, "starting recording");
                if self
                    .recorder_cmd_tx
                    .try_send(RecorderCommand::Start {
                        freq_hz: center_freq,
                        iq_sample_rate: iq_sr,
                        mode: rec_mode,
                    })
                    .is_err()
                {
                    tracing::error!("failed to send StartRecording to recorder");
                }
                if self
                    .cmd_tx
                    .try_send(SignalPathCommand::StartRecording)
                    .is_err()
                {
                    tracing::error!("failed to send StartRecording to signal path");
                }
            }
        }

        // Recorder error banner
        if let Some(ref err) = rec_error {
            ui.add_space(4.0);
            let err_frame = egui::Frame::none()
                .fill(Color32::from_rgba_premultiplied(80, 10, 10, 200))
                .stroke(Stroke::new(1.0, theme::DANGER))
                .inner_margin(egui::Margin::same(4.0));
            err_frame.show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("[!]").color(theme::DANGER));
                    ui.label(RichText::new(err).color(theme::DANGER).small());
                    if ui.small_button("x").clicked() {
                        self.shared.write().recorder_error = None;
                    }
                });
            });
        }

        // ── Scheduled Recording ───────────────────────────────────────────────
        ui.add_space(6.0);
        ui.collapsing("Schedule", |ui| {
            ui.horizontal(|ui| {
                ui.label("Delay (s):");
                let mut delay = self.sched_delay_secs;
                if ui
                    .add(egui::DragValue::new(&mut delay).range(0..=3600))
                    .changed()
                {
                    self.sched_delay_secs = delay;
                }
            });
            ui.horizontal(|ui| {
                ui.label("Duration (s):");
                let mut dur = self.sched_duration_secs;
                if ui
                    .add(egui::DragValue::new(&mut dur).range(1..=86400))
                    .changed()
                {
                    self.sched_duration_secs = dur;
                }
            });
            let arm_btn = egui::Button::new("Arm Schedule")
                .fill(theme::WIDGET_BG)
                .stroke(Stroke::new(1.0, theme::ACCENT));
            if ui
                .add_sized(Vec2::new(ui.available_width(), 24.0), arm_btn)
                .clicked()
            {
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

        ui.add_space(4.0);
        ui.separator();
        ui.add_space(6.0);

        // ── Device Settings ───────────────────────────────────────────────────
        {
            let (dev_running, dev_demo) = {
                let s = self.shared.read();
                let running = s.is_running;
                let demo = s.source_name.as_deref().map(|n| n.contains("Demo")).unwrap_or(false);
                (running, demo)
            };
            self.device_settings_section(ui, dev_running, dev_demo);
        }


        // ── Restart Device button ─────────────────────────────────────────────
        {
            let is_demo = self.shared.read().source_name
                .as_deref()
                .map(|n| n.contains("Demo"))
                .unwrap_or(false);
            if !is_demo {
                ui.horizontal(|ui| {
                    if ui
                        .button(RichText::new("↺ Restart Device").small())
                        .on_hover_text(
                            "Close and reopen the hardware without restarting the app.\n\
                             Useful after a cable disconnect or API hang.",
                        )
                        .clicked()
                    {
                        let _ = self.cmd_tx.try_send(HardwareCommand::RestartDevice.into());
                        tracing::info!("Restart Device requested by user");
                    }
                });
                ui.add_space(4.0);
            }
        }

        // ── MIDI Controller ───────────────────────────────────────────────────
        ui.separator();
        ui.add_space(2.0);
        self.midi_section(ui);
    }

    /// MIDI controller status + mapper toggle.
    /// Extracted so it can be placed in whatever panel the layout requires.
    pub(in crate::app) fn midi_section(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("MIDI").color(theme::TEXT_MUTED).small());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let mapper_label = if self.show_midi_mapper { "▼ Mapper" } else { "▶ Mapper" };
                let btn = egui::Button::new(
                    RichText::new(mapper_label).color(theme::ACCENT).small(),
                )
                .fill(theme::WIDGET_BG)
                .stroke(egui::Stroke::new(1.0, if self.show_midi_mapper { theme::ACCENT } else { theme::BORDER }));
                if ui.add(btn).clicked() {
                    self.show_midi_mapper = !self.show_midi_mapper;
                }
            });
        });
        ui.add_space(4.0);

        let (midi_device, midi_page) = {
            let s = self.shared.read();
            (s.midi_device.clone(), s.midi_page)
        };

        if let Some(ref device_name) = midi_device {
            ui.horizontal(|ui| {
                let (dr, _) = ui.allocate_exact_size(egui::Vec2::splat(10.0), egui::Sense::hover());
                ui.painter().circle_filled(dr.center(), 4.0, theme::STATUS_OK);
                let name = if device_name.len() > 22 {
                    format!("{}...", &device_name[..21])
                } else {
                    device_name.clone()
                };
                ui.label(RichText::new(name).color(theme::TEXT_PRIMARY).small());
            });

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
                let (dr, _) = ui.allocate_exact_size(egui::Vec2::splat(10.0), egui::Sense::hover());
                ui.painter().circle_filled(dr.center(), 4.0, theme::TEXT_DISABLED);
                ui.label(RichText::new("Not connected").color(theme::TEXT_MUTED).small());
            });
            ui.label(
                RichText::new("Connect nanoKontrol2 via USB")
                    .color(theme::TEXT_DISABLED)
                    .small(),
            );
        }
    }

    /// Device diagnostics grid + error log (collapsible).
    /// Lives in the left panel (under bookmarks).
    pub(in crate::app) fn device_diagnostics_section(&mut self, ui: &mut Ui) {
        ui.collapsing(
            RichText::new("DEVICE DIAGNOSTICS").color(theme::TEXT_MUTED).small(),
            |ui| {
        ui.add_space(4.0);

        let (serial, hw_ver, api_version, status, error_count, iq_lag_count, sample_rate, all_errors) = {
            let s = self.shared.read();
            let d = &s.device_diagnostics;
            let all_errors: Vec<String> = d
                .error_log
                .iter()
                .rev()
                .map(|e| e.message.clone())
                .collect();
            (
                d.serial.clone(),
                d.hw_ver,
                d.api_version.clone(),
                d.status.clone(),
                d.error_count,
                d.iq_lag_count,
                s.sample_rate_sps,
                all_errors,
            )
        };

        egui::Grid::new("device_diag_grid")
            .num_columns(2)
            .spacing([4.0, 2.0])
            .show(ui, |ui| {
                ui.label(RichText::new("Serial").small().color(theme::TEXT_MUTED));
                ui.label(
                    RichText::new(if serial.is_empty() { "—".to_string() } else { serial })
                        .small()
                        .monospace(),
                );
                ui.end_row();

                ui.label(RichText::new("HW ver").small().color(theme::TEXT_MUTED));
                ui.label(
                    RichText::new(if hw_ver == 0 { "—".to_string() } else { hw_ver.to_string() })
                        .small()
                        .monospace(),
                );
                ui.end_row();

                ui.label(RichText::new("API ver").small().color(theme::TEXT_MUTED));
                ui.label(
                    RichText::new(if api_version.is_empty() { "—".to_string() } else { api_version })
                        .small()
                        .monospace(),
                );
                ui.end_row();

                ui.label(RichText::new("Status").small().color(theme::TEXT_MUTED));
                ui.label(
                    RichText::new(if status.is_empty() { "idle".to_string() } else { status })
                        .small(),
                );
                ui.end_row();

                let rate_str = if sample_rate >= 1_000_000 {
                    format!("{:.1} MHz", sample_rate as f32 / 1_000_000.0)
                } else if sample_rate > 0 {
                    format!("{} kHz", sample_rate / 1_000)
                } else {
                    "—".to_string()
                };
                ui.label(RichText::new("Rate").small().color(theme::TEXT_MUTED));
                ui.label(RichText::new(rate_str).small().monospace());
                ui.end_row();

                ui.label(RichText::new("IQ lags").small().color(theme::TEXT_MUTED));
                ui.label(RichText::new(iq_lag_count.to_string()).small().monospace());
                ui.end_row();

                if error_count > 0 {
                    ui.label(RichText::new("Errors").small().color(theme::TEXT_MUTED));
                    ui.label(
                        RichText::new(error_count.to_string())
                            .small()
                            .monospace()
                            .color(Color32::from_rgb(220, 80, 80)),
                    );
                    ui.end_row();
                }
            });

        ui.add_space(4.0);
        ui.label(RichText::new("Error log:").small().color(theme::TEXT_MUTED));
        egui::ScrollArea::vertical()
            .id_salt("diag_error_log")
            .max_height(80.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                if all_errors.is_empty() {
                    ui.label(RichText::new("No errors").small().color(theme::TEXT_MUTED));
                } else {
                    for err in &all_errors {
                        ui.label(
                            RichText::new(err)
                                .small()
                                .monospace()
                                .color(Color32::from_rgb(220, 120, 80)),
                        );
                    }
                }
            });
        });
    }

    /// Tune to 1090 MHz and start the ADS-B decoder at the current hardware sample rate.
    /// Call only when `shared.sample_rate_sps >= 2_000_000`.
    pub(in crate::app) fn adsb_start_decoder(&mut self) {
        let sr = self.shared.read().sample_rate_sps;
        let _ = self.cmd_tx.try_send(ReceiverCmd::SetFrequency(1_090_000_000).into());
        self.config.ui.frequency_hz = 1_090_000_000;
        self.frequency_widget = FrequencyWidget::new(1_090_000_000);
        self.config_dirty = true;
        if let Some(tx) = self.adsb_iq_tx.as_ref() {
            let iq_rx = tx.subscribe();
            self.adsb_decoder = Some(crate::adsb_decoder::AdsbDecoder::start(
                iq_rx,
                std::sync::Arc::clone(&self.adsb_store),
                sr,
            ));
            tracing::info!(sample_rate_sps = sr, "ADS-B decoder started");
        }
    }

    /// Full ADS-B start sequence: switch to Antenna B, mute audio, configure
    /// sample rate if needed, then start the decoder (or set `adsb_start_pending`).
    pub(in crate::app) fn adsb_start_sequence(&mut self) {
        // Ensure the SDR hardware pipeline is running — without this the IQ
        // broadcast channel never receives data and the decoder sees only Empty.
        if !self.shared.read().is_running {
            tracing::info!("ADS-B start: auto-starting hardware pipeline");
            let _ = self.cmd_tx.try_send(SignalPathCommand::Start);
            let saved_mode = crate::app::parse_config_demod_mode(&self.config.ui.demod_mode.clone());
            let _ = self.cmd_tx.try_send(ReceiverCmd::SetDemodMode(saved_mode).into());
        }
        // Switch to Antenna B where the ADS-B antenna is connected.
        if self.config.source.antenna != "B" {
            let prev_ant = self.config.source.antenna.clone();
            self.adsb_prev_antenna = Some(prev_ant);
            self.config.source.antenna = "B".into();
            self.config_dirty = true;
            let _ = self.cmd_tx.try_send(HardwareCommand::SetAntenna(1).into());
            tracing::info!("ADS-B start: switching to Antenna B");
        }
        // Mute audio while decoder is running.
        if !self.muted {
            self.muted = true;
            self.adsb_did_mute = true;
            let _ = self.cmd_tx.try_send(ReceiverCmd::SetMuted(true).into());
            tracing::info!("ADS-B start: muting audio");
        }
        let sr = self.shared.read().sample_rate_sps;
        if sr < 2_000_000 {
            // Auto-reconfigure: set decimation to 1, wait for hardware to apply.
            let prev = self.config.source.decimation_factor;
            self.adsb_prev_decimation = Some(prev);
            self.config.source.decimation_factor = 1;
            self.config_dirty = true;
            let _ = self.cmd_tx.try_send(HardwareCommand::SetDecimationFactor(1).into());
            tracing::info!(
                prev_decimation = prev,
                "ADS-B start: reconfiguring hardware to decimation=1 (2 Msps)"
            );
            self.adsb_start_pending = true;
        } else {
            self.adsb_start_decoder();
        }
    }

    /// Stop the ADS-B decoder and restore hardware state (antenna, mute, decimation).
    pub(in crate::app) fn adsb_stop_decoder(&mut self) {
        if let Some(mut d) = self.adsb_decoder.take() {
            d.stop();
        }
        self.adsb_start_pending = false;
        // Restore previous antenna if we changed it
        if let Some(prev_ant) = self.adsb_prev_antenna.take() {
            let port: u8 = match prev_ant.as_str() { "B" => 1, "C" => 2, _ => 0 };
            self.config.source.antenna = prev_ant;
            self.config_dirty = true;
            let _ = self.cmd_tx.try_send(HardwareCommand::SetAntenna(port).into());
            tracing::info!(antenna = port, "ADS-B stop: restoring previous antenna");
        }
        // Unmute if ADS-B was the one that muted
        if self.adsb_did_mute {
            self.muted = false;
            self.adsb_did_mute = false;
            let _ = self.cmd_tx.try_send(ReceiverCmd::SetMuted(false).into());
            tracing::info!("ADS-B stop: unmuting audio");
        }
        // Restore previous decimation if we changed it
        if let Some(prev) = self.adsb_prev_decimation.take() {
            if prev > 1 {
                self.config.source.decimation_factor = prev;
                self.config_dirty = true;
                let _ = self.cmd_tx.try_send(HardwareCommand::SetDecimationFactor(prev).into());
                tracing::info!(decimation = prev, "ADS-B stop: restoring decimation factor");
            }
        }
    }

    pub(in crate::app) fn draw_vu_meter(&mut self, ui: &mut Ui, left: f32, right: f32, muted: bool, running: bool) {
        let bar_w = ui.available_width() / 2.0 - 4.0;
        let bar_h = 8.0;

        ui.horizontal(|ui| {
            for &_level in &[left, right] {
                let (rect, _) =
                    ui.allocate_exact_size(Vec2::new(bar_w, bar_h), egui::Sense::hover());

                let painter = ui.painter();

                // Background track
                painter.rect_filled(rect, 2.0, theme::WIDGET_BG);

                if !running {
                    // Stopped: no fill — dark background only.
                } else if muted {
                    // Muted: full-width amber/yellow bar.
                    let mute_color = Color32::from_rgba_premultiplied(0xC0, 0x80, 0x00, 0x88);
                    painter.rect_filled(rect, 2.0, mute_color);
                } else {
                    // Running + unmuted: solid green bar.
                    painter.rect_filled(rect, 2.0, theme::VU_LOW);
                }
            }
        });

        // Decay the peak
        self.vu_peak = (self.vu_peak - 0.02).max(0.0);
    }

    /// Draw two small vertical dBFS bars (peak + RMS) for the recording level meter.
    /// Range: -60..0 dBFS; bars grow upward from a baseline.
    fn draw_recording_level_bars(&self, ui: &mut Ui, peak_db: f32, rms_db: f32) {
        const BAR_W: f32 = 6.0;
        const BAR_H: f32 = 20.0;
        const GAP: f32 = 2.0;
        const DB_FLOOR: f32 = -60.0;

        let total_w = BAR_W * 2.0 + GAP;
        let (rect, _) =
            ui.allocate_exact_size(Vec2::new(total_w, BAR_H), egui::Sense::hover());
        let painter = ui.painter();

        for (i, db) in [peak_db, rms_db].iter().enumerate() {
            let x_off = i as f32 * (BAR_W + GAP);
            let bar_rect = egui::Rect::from_min_size(
                rect.min + Vec2::new(x_off, 0.0),
                Vec2::new(BAR_W, BAR_H),
            );
            // Background
            painter.rect_filled(bar_rect, 1.0, theme::WIDGET_BG);

            // Fill from bottom: fraction of range
            let frac = ((*db - DB_FLOOR) / -DB_FLOOR).clamp(0.0, 1.0);
            let fill_h = BAR_H * frac;
            if fill_h > 0.5 {
                let fill_rect = egui::Rect::from_min_size(
                    rect.min + Vec2::new(x_off, BAR_H - fill_h),
                    Vec2::new(BAR_W, fill_h),
                );
                let color = if *db > -6.0 {
                    theme::VU_HIGH
                } else if *db > -20.0 {
                    theme::VU_MID
                } else {
                    theme::VU_LOW
                };
                painter.rect_filled(fill_rect, 1.0, color);
            }
        }

        // Hover tooltip
        ui.interact(rect, ui.id().with("rec_level_bars"), egui::Sense::hover())
            .on_hover_text(format!(
                "Peak: {peak_db:.1} dBFS\nRMS: {rms_db:.1} dBFS"
            ));
    }
}
