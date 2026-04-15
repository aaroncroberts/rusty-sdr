use egui::{RichText, Ui};

use crate::theme;
use super::super::SdrApp;
use super::format_frequency;

impl SdrApp {
    pub(in crate::app) fn status_bar(&self, ui: &mut Ui) {
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
