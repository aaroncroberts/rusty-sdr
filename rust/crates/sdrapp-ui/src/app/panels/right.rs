#![allow(clippy::too_many_lines)]

use egui::{Color32, RichText, Stroke, Ui, Vec2};

use sdrapp_core::signal_path::{DemodMode, DisplayCmd, HardwareCommand, ReceiverCmd, RecordingMode, SignalPathCommand};
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

        let (vol_learn, vol_cc, map_pending) = {
            let s = self.shared.read();
            let learn = s.midi_learn_target.as_deref() == Some("volume");
            let cc = s
                .midi_cc_to_knob
                .iter()
                .find(|(_, v)| v.as_str() == "volume")
                .map(|(&c, _)| c);
            (learn, cc, s.midi_map_pending.is_some())
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
                let _ = self.cmd_tx.try_send(ReceiverCmd::SetVolume(vol).into());
                self.config_dirty = true;
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
        // Peak level decays each frame; in real wiring this reads from AudioSink
        let level = self.vu_peak * self.config.ui.volume;
        self.draw_vu_meter(ui, level, level * 0.92); // slight L/R difference for visual interest

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

            // Header row: label + live aircraft count badge + ✈ Map button
            ui.horizontal(|ui| {
                ui.label(RichText::new("ADS-B").color(theme::TEXT_MUTED).small());
                if decoder_running && count > 0 {
                    ui.label(
                        RichText::new(format!("✈ {count}"))
                            .color(theme::STATUS_OK)
                            .small()
                            .strong(),
                    );
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // ✈ Map — the single entry point. One click:
                    //   • tunes to 1090 MHz
                    //   • starts the decoder (if not already running)
                    //   • opens the map window
                    // When already running, just toggles the map window.
                    let map_lbl = if self.show_adsb_map { "▼ Map" } else { "✈ Map" };
                    let map_color = if decoder_running { theme::STATUS_OK } else { theme::ACCENT };
                    let map_btn = egui::Button::new(
                        RichText::new(map_lbl).color(map_color).small(),
                    )
                    .fill(theme::WIDGET_BG)
                    .stroke(Stroke::new(
                        1.0,
                        if self.show_adsb_map { map_color } else { theme::BORDER },
                    ));
                    let hover = if decoder_running {
                        "Toggle aircraft map"
                    } else {
                        "Open map · tune to 1090 MHz · start decoder"
                    };
                    if ui.add(map_btn).on_hover_text(hover).clicked() {
                        if !decoder_running && !self.adsb_start_pending {
                            let sr = self.shared.read().sample_rate_sps;
                            if sr < 2_000_000 {
                                // Auto-reconfigure: save current decimation, set to 1,
                                // then wait for device to apply before starting decoder.
                                let prev = self.config.source.decimation_factor;
                                self.adsb_prev_decimation = Some(prev);
                                self.config.source.decimation_factor = 1;
                                self.config_dirty = true;
                                let _ = self.cmd_tx.try_send(
                                    HardwareCommand::SetDecimationFactor(1).into(),
                                );
                                tracing::info!(
                                    prev_decimation = prev,
                                    "ADS-B entry: reconfiguring hardware to decimation=1 (2 Msps)"
                                );
                                self.adsb_start_pending = true;
                            }
                            // If sr is already ≥ 2 Msps, start immediately (handled below).
                            // If reconfiguring, adsb_start_pending will fire next frame.
                            let current_sr = self.shared.read().sample_rate_sps;
                            if current_sr >= 2_000_000 && !self.adsb_start_pending {
                                self.adsb_start_decoder();
                            }
                        }
                        self.show_adsb_map = !self.show_adsb_map;
                    }

                    // ■ Stop button (shown when running or pending start)
                    if decoder_running || self.adsb_start_pending {
                        let stop_btn = egui::Button::new(
                            RichText::new("■ Stop").color(theme::AMBER).small(),
                        )
                        .fill(theme::WIDGET_BG)
                        .stroke(Stroke::new(1.0, theme::BORDER));
                        if ui.add(stop_btn).on_hover_text("Stop ADS-B decoder").clicked() {
                            if let Some(mut d) = self.adsb_decoder.take() {
                                d.stop();
                            }
                            self.adsb_start_pending = false;
                            // Restore previous decimation if we changed it
                            if let Some(prev) = self.adsb_prev_decimation.take() {
                                if prev > 1 {
                                    self.config.source.decimation_factor = prev;
                                    self.config_dirty = true;
                                    let _ = self.cmd_tx.try_send(
                                        HardwareCommand::SetDecimationFactor(prev).into(),
                                    );
                                    tracing::info!(
                                        decimation = prev,
                                        "ADS-B exit: restoring previous decimation factor"
                                    );
                                }
                            }
                        }
                    }
                });
            });

            // Deferred start: check each frame whether the device has applied our
            // SetDecimationFactor(1) command (sample_rate_sps will flip to ≥ 2 Msps).
            if self.adsb_start_pending {
                let current_sr = self.shared.read().sample_rate_sps;
                if current_sr >= 2_000_000 {
                    self.adsb_start_pending = false;
                    // Tune and start now that the hardware is correctly configured
                    self.adsb_start_decoder();
                }
            }

            // Status line
            let effective_sr = self.shared.read().sample_rate_sps;
            if self.adsb_start_pending {
                ui.label(
                    RichText::new("  ⟳ Reconfiguring hardware for ADS-B…")
                        .color(theme::AMBER)
                        .small(),
                );
            } else if decoder_running {
                let frames = self.adsb_decoder
                    .as_ref()
                    .map(|d| d.frames_decoded())
                    .unwrap_or(0);
                let status = if count > 0 {
                    format!("  ● LIVE  {count} aircraft  {frames} frames")
                } else {
                    format!("  ● LIVE  listening…  {frames} frames")
                };
                ui.label(RichText::new(status).color(theme::STATUS_OK).small());
                // Warn if somehow started at wrong rate (shouldn't happen after the
                // guard above, but keep as a backstop for edge cases).
                let decoder_sr = self.adsb_decoder.as_ref().map(|d| d.sample_rate).unwrap_or(0);
                if decoder_sr < 2_000_000 {
                    ui.label(
                        RichText::new(format!(
                            "  ⚠ {:.0} kHz effective — no frames possible",
                            decoder_sr as f32 / 1_000.0,
                        ))
                        .color(theme::AMBER)
                        .small(),
                    );
                }
            } else if effective_sr < 2_000_000 {
                // Rate too low — ✈ Map will auto-reconfigure hardware to 2 Msps.
                ui.label(
                    RichText::new("  ✈ Map will auto-configure hardware for ADS-B")
                        .color(theme::TEXT_MUTED)
                        .small(),
                );
            } else if count > 0 {
                ui.label(
                    RichText::new(format!("  ○ {count} aircraft cached"))
                        .color(theme::TEXT_MUTED)
                        .small(),
                );
            } else {
                ui.label(
                    RichText::new("  Press ✈ Map to tune and start")
                        .color(theme::TEXT_DISABLED)
                        .small(),
                );
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

        // ── Device Diagnostics (collapsible) ─────────────────────────────────
        ui.separator();
        ui.add_space(2.0);
        ui.collapsing(
            RichText::new("DEVICE DIAGNOSTICS").color(theme::TEXT_MUTED).small(),
            |ui| {
        ui.add_space(4.0);
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

        // Scrollable error log — always visible, shows all entries (newest first)
        ui.add_space(4.0);
        ui.label(RichText::new("Error log:").small().color(theme::TEXT_MUTED));
        let log_height = 80.0_f32;
        egui::ScrollArea::vertical()
            .id_salt("diag_error_log")
            .max_height(log_height)
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
        }); // end Device Diagnostics collapsing
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

    pub(in crate::app) fn draw_vu_meter(&mut self, ui: &mut Ui, left: f32, right: f32) {
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
