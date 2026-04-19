//! Orbcomm pass log UI section.
//!
//! Rendered in the right panel below the Recorder section.  Shows decoded
//! satellite telemetry frames from the Orbcomm 137 MHz downlink, with live
//! status, a scrollable log, and a Clear button.

use egui::{Color32, RichText, Ui};
use sdrapp_orbcomm::parser::PacketType;

use super::super::SdrApp;
use super::format_frequency;
use crate::theme;

/// Orbcomm satellite ID → name lookup.
///
/// Orbcomm OG2 satellites use IDs 0x01-0x17 (1-23).  OG1 spacecraft are
/// effectively decommissioned so we map only the common OG2 IDs.
fn sat_name(id: u8) -> Option<&'static str> {
    match id {
        1  => Some("OG2-01"),
        2  => Some("OG2-02"),
        3  => Some("OG2-03"),
        4  => Some("OG2-04"),
        5  => Some("OG2-05"),
        6  => Some("OG2-06"),
        7  => Some("OG2-07"),
        8  => Some("OG2-08"),
        9  => Some("OG2-09"),
        10 => Some("OG2-10"),
        11 => Some("OG2-11"),
        12 => Some("OG2-12"),
        13 => Some("OG2-13"),
        14 => Some("OG2-14"),
        15 => Some("OG2-15"),
        16 => Some("OG2-16"),
        17 => Some("OG2-17"),
        18 => Some("OG2-18"),
        _  => None,
    }
}

/// Format a UTC epoch (seconds since 1970-01-01) as `HH:MM:SS UTC`.
fn format_utc(secs: u32) -> String {
    let h = (secs / 3600) % 24;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    format!("{h:02}:{m:02}:{s:02} UTC")
}

impl SdrApp {
    /// Render the ORBCOMM section in the right panel.
    pub(in crate::app) fn orbcomm_section(&mut self, ui: &mut Ui) {
        // ── Section header ────────────────────────────────────────────────────
        ui.label(RichText::new("ORBCOMM").color(theme::TEXT_MUTED).small());
        ui.add_space(4.0);

        // ── Status row ────────────────────────────────────────────────────────
        let is_running = self
            .orbcomm_decoder
            .as_ref()
            .map(|d| d.is_running())
            .unwrap_or(false);

        let frames = self
            .orbcomm_decoder
            .as_ref()
            .map(|d| d.frames_decoded())
            .unwrap_or(0);

        ui.horizontal(|ui| {
            // Live / Off indicator
            let (dot_color, status_text) = if is_running {
                (Color32::from_rgb(0x73, 0xC9, 0x91), "Live")
            } else {
                (Color32::from_rgb(0x6A, 0x7A, 0x8A), "Off")
            };
            ui.colored_label(dot_color, "●");
            ui.label(RichText::new(status_text).color(theme::TEXT_MUTED).small());

            if is_running {
                ui.label(
                    RichText::new(format!("{frames} frames"))
                        .color(theme::TEXT_DISABLED)
                        .small(),
                );
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // Clear button
                if ui
                    .small_button(RichText::new("Clear").small())
                    .on_hover_text("Clear the Orbcomm pass log")
                    .clicked()
                {
                    self.orbcomm_log.clear();
                }
            });
        });

        ui.add_space(4.0);

        // ── Start / Stop hint ─────────────────────────────────────────────────
        if !is_running {
            ui.label(
                RichText::new("Tune to 137.500 MHz to start")
                    .color(theme::TEXT_DISABLED)
                    .small(),
            );
            ui.add_space(4.0);
        }

        // ── Pass log ──────────────────────────────────────────────────────────
        if self.orbcomm_log.is_empty() {
            ui.label(RichText::new("No frames decoded yet").color(theme::TEXT_DISABLED).small());
        } else {
            // Show most recent 20 entries; scroll for older ones.
            let log_height = 120.0_f32;
            egui::ScrollArea::vertical()
                .id_salt("orbcomm_log_scroll")
                .max_height(log_height)
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    // Newest last → iterate in reverse to show newest at bottom.
                    for entry in &self.orbcomm_log {
                        let pkt = &entry.packet;

                        // Timestamp
                        let ts = entry
                            .received_at
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| {
                                let s = d.as_secs();
                                let h = (s / 3600) % 24;
                                let m = (s % 3600) / 60;
                                let sec = s % 60;
                                format!("{h:02}:{m:02}:{sec:02}")
                            })
                            .unwrap_or_else(|_| "??:??:??".into());

                        let (row_color, row_text) = match &pkt.packet_type {
                            PacketType::SatelliteTelemetry { sat_id, utc_seconds, channel, .. } => {
                                let name = sat_name(*sat_id)
                                    .map(|s| s.to_string())
                                    .unwrap_or_else(|| format!("Sat#{sat_id:02X}"));
                                let utc = format_utc(*utc_seconds);
                                let fcs = if pkt.crc_ok { "" } else { " [bad FCS]" };
                                (
                                    theme::STATUS_OK,
                                    format!("{ts}  {name}  ch{channel}  {utc}{fcs}"),
                                )
                            }
                            PacketType::SubscriberMessage => (
                                theme::TEXT_MUTED,
                                format!("{ts}  subscriber msg (encrypted)"),
                            ),
                            PacketType::Ack => (
                                theme::TEXT_MUTED,
                                format!("{ts}  ack"),
                            ),
                            PacketType::Unknown(tw) => (
                                theme::TEXT_DISABLED,
                                format!("{ts}  unknown type 0x{tw:04X}"),
                            ),
                        };

                        let freq_label = format_frequency(entry.freq_hz);
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(&row_text).color(row_color).small().monospace(),
                            );
                            ui.label(
                                RichText::new(format!("@ {freq_label}"))
                                    .color(theme::TEXT_DISABLED)
                                    .small(),
                            );
                        });
                    }
                });
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sat_name_known_ids() {
        assert_eq!(sat_name(1), Some("OG2-01"));
        assert_eq!(sat_name(17), Some("OG2-17"));
    }

    #[test]
    fn sat_name_unknown_id_returns_none() {
        assert_eq!(sat_name(0), None);
        assert_eq!(sat_name(100), None);
    }

    #[test]
    fn format_utc_epoch_zero() {
        // Unix epoch 0 = 00:00:00
        assert_eq!(format_utc(0), "00:00:00 UTC");
    }

    #[test]
    fn format_utc_known_time() {
        // 1 h 23 m 45 s = 3600 + 1380 + 45 = 5025
        assert_eq!(format_utc(5025), "01:23:45 UTC");
    }

    #[test]
    fn format_utc_wraps_at_24h() {
        // 86400 s = exactly 24 h → wraps to 00:00:00
        assert_eq!(format_utc(86400), "00:00:00 UTC");
    }
}
