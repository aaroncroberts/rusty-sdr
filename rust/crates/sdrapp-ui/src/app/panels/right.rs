#![allow(clippy::too_many_lines)]

use egui::{Color32, RichText, Stroke, Ui, Vec2};

use sdrapp_core::signal_path::{DisplayCmd, ReceiverCmd, RecordingMode, SignalPathCommand};
use sdrapp_recorder::RecorderCommand;

use crate::{
    frequency::FrequencyWidget,
    knob::KnobWidget,
    theme,
};
use super::super::SdrApp;

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

impl SdrApp {
    pub(in crate::app) fn right_panel(&mut self, ui: &mut Ui) {
        ui.add_space(4.0);

        // ── Volume + VU meter ─────────────────────────────────────────────────
        ui.label(RichText::new("VOLUME").color(theme::TEXT_MUTED).small());
        ui.add_space(4.0);

        let (vol_learn, vol_cc) = {
            let s = self.shared.read();
            let learn = s.midi_learn_target.as_deref() == Some("volume");
            let cc = s.midi_cc_to_knob.iter().find(|(_, v)| v.as_str() == "volume").map(|(&c, _)| c);
            (learn, cc)
        };
        let mut vol_learn_req = false;
        let mut vol_clear: Option<u8> = None;
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
                learn_active: vol_learn,
            }.show(ui);
            resp.context_menu(|ui| {
                if ui.button("Assign MIDI CC").clicked() { vol_learn_req = true; ui.close_menu(); }
                if let Some(cc) = vol_cc {
                    if ui.button(format!("Clear CC {cc} binding")).clicked() { vol_clear = Some(cc); ui.close_menu(); }
                }
            });
            if resp.changed() {
                self.config.ui.volume = vol;
                let _ = self.cmd_tx.try_send(ReceiverCmd::SetVolume(vol).into());
                self.config_dirty = true;
            }
        });
        if vol_learn_req { self.shared.write().midi_learn_target = Some("volume".into()); }
        if let Some(cc) = vol_clear { self.shared.write().midi_cc_to_knob.remove(&cc); self.config_dirty = true; }

        // VU meter (stereo bars)
        // Peak level decays each frame; in real wiring this reads from AudioSink
        let level = self.vu_peak * self.config.ui.volume;
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
            let _ = self.cmd_tx.try_send(ReceiverCmd::SetFrequency(hz).into());
            self.config.ui.frequency_hz = hz;
            self.config.ui.span_hz = span;
            self.frequency_widget = FrequencyWidget::new(hz);
            // Convert preset span to zoom_level relative to hardware bandwidth
            let sr_half = self.shared.read().sample_rate_sps as u64 / 2;
            if sr_half > 0 {
                let z = (span as f32 / sr_half as f32).clamp(0.005, 1.0);
                let _ = self.cmd_tx.try_send(DisplayCmd::SetZoom(z).into());
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
                if self.recorder_cmd_tx.try_send(RecorderCommand::Stop).is_err() {
                    tracing::error!("failed to send StopRecording to recorder");
                }
                if self.cmd_tx.try_send(SignalPathCommand::StopRecording).is_err() {
                    tracing::error!("failed to send StopRecording to signal path");
                }
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
                tracing::info!(freq_hz = center_freq, ?rec_mode, "starting recording");
                if self.recorder_cmd_tx.try_send(RecorderCommand::Start {
                    freq_hz: center_freq,
                    iq_sample_rate: iq_sr,
                    mode: rec_mode,
                }).is_err() {
                    tracing::error!("failed to send StartRecording to recorder");
                }
                if self.cmd_tx.try_send(SignalPathCommand::StartRecording).is_err() {
                    tracing::error!("failed to send StartRecording to signal path");
                }
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

        ui.add_space(8.0);
        ui.separator();
        ui.add_space(6.0);

        // ── Rigctl (Hamlib) server ────────────────────────────────────────────
        ui.label(RichText::new("RIGCTL").color(theme::TEXT_MUTED).small());
        ui.add_space(4.0);

        let enabled = self.config.rigctl.enabled;
        ui.horizontal(|ui| {
            let en_color = if enabled { theme::STATUS_OK } else { theme::TEXT_MUTED };
            let en_label = RichText::new(if enabled { "ON" } else { "OFF" }).color(en_color).small().strong();
            if ui.selectable_label(enabled, en_label)
                .on_hover_text("Enable Hamlib-compatible CAT server (requires app restart to take effect)")
                .clicked()
            {
                self.config.rigctl.enabled = !enabled;
                self.config_dirty = true;
            }

            if enabled {
                ui.label(RichText::new(format!("port {}", self.config.rigctl.port)).color(theme::TEXT_MUTED).small());
            }
        });

        if enabled {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Port").color(theme::TEXT_MUTED).small());
                let mut port = self.config.rigctl.port as i32;
                if ui.add(egui::DragValue::new(&mut port).range(1024..=65535)).changed() {
                    self.config.rigctl.port = port as u16;
                    self.config_dirty = true;
                }
            });
            ui.label(RichText::new("Connect: nc 127.0.0.1 <port>").color(theme::TEXT_DISABLED).small());
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
}
