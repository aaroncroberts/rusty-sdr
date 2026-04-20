//! View-level layout methods for the Aircraft and Satellite views.
//!
//! Each view owns the `CentralPanel` area and arranges its primary content
//! (map) and the mini signal strip at the bottom.

use egui::Ui;
use std::sync::Arc;

use super::super::SdrApp;
use super::mini_signal::MINI_STRIP_HEIGHT;
use crate::theme;

impl SdrApp {
    /// Aircraft view: embedded ADS-B map + mini signal strip at bottom.
    pub(in crate::app) fn aircraft_view_center(&mut self, ui: &mut Ui) {
        let aircraft: Vec<_> = self.adsb_store.lock().aircraft().into_iter()
            .filter(|a| !a.mode_s_only)
            .cloned()
            .collect();

        // Push decoder state into map so its toolbar shows correct status
        let decoder_running = self.adsb_decoder.as_ref().map(|d| d.is_running()).unwrap_or(false);
        {
            let mut map = self.adsb_map.lock();
            map.decoder_running = decoder_running;
            map.frame_count = self.adsb_decoder.as_ref().map(|d| d.frames_decoded()).unwrap_or(0);
            map.crc_ok_count = self.adsb_decoder.as_ref().map(|d| d.crc_ok_frames()).unwrap_or(0);
            map.preamble_count = self.adsb_decoder.as_ref().map(|d| d.preambles_detected()).unwrap_or(0);
            map.sample_rate_ok = self.shared.read().sample_rate_sps >= 2_000_000;
            map.adsb_start_pending = self.adsb_start_pending;
        }

        let home_lat = self.config.ui.home_lat;
        let home_lon = self.config.ui.home_lon;

        // Mini signal strip at bottom
        let shared = Arc::clone(&self.shared);
        egui::TopBottomPanel::bottom("aircraft_mini_signal")
            .exact_height(MINI_STRIP_HEIGHT)
            .frame(egui::Frame::none().fill(theme::BG))
            .show_inside(ui, |strip_ui| {
                self.mini_signal.set_db_range((self.fft_floor, self.fft_ceil));
                self.mini_signal.show(strip_ui, &shared);
            });

        // ADS-B map fills remaining space
        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(egui::Color32::from_rgb(0x10, 0x14, 0x1A)))
            .show_inside(ui, |map_ui| {
                self.adsb_map.lock().show_embedded(map_ui, &aircraft, home_lat, home_lon, self.atc_mode_active);
            });

        // Consume action requests set by the embedded map
        let sr = self.shared.read().sample_rate_sps;
        let (start_req, stop_req, tune_freq) = {
            let mut map = self.adsb_map.lock();
            if map.set_home_pending {
                map.set_home_pending = false;
                self.config.ui.home_lat = map.center_lat();
                self.config.ui.home_lon = map.center_lon();
                self.config_dirty = true;
            }
            // Save map viewport to config
            let (lat, lon, zoom) = (map.center_lat(), map.center_lon(), map.zoom_ppd());
            if (self.config.ui.adsb_map_lat - lat).abs() > 0.001
                || (self.config.ui.adsb_map_lon - lon).abs() > 0.001
                || (self.config.ui.adsb_map_zoom - zoom).abs() > 0.1
            {
                self.config.ui.adsb_map_lat = lat;
                self.config.ui.adsb_map_lon = lon;
                self.config.ui.adsb_map_zoom = zoom;
                self.config_dirty = true;
            }
            (
                std::mem::take(&mut map.start_requested),
                std::mem::take(&mut map.stop_requested),
                map.tune_frequency_hz.take(),
            )
        };

        // Deferred start: fires each frame until the hardware reaches ≥ 2 Msps
        if self.adsb_start_pending && sr >= 2_000_000 {
            self.adsb_start_pending = false;
            self.adsb_start_decoder();
        }
        if start_req && !decoder_running && !self.adsb_start_pending {
            self.adsb_start_sequence();
        }
        if stop_req {
            self.adsb_stop_decoder();
            self.atc_mode_active = false;
        }
        if let Some(freq_hz) = tune_freq {
            self.handle_aircraft_tune(freq_hz);
        }
    }

    /// Satellite view: sat map (center) + NOAA APT sidebar (right) + mini signal strip (bottom).
    pub(in crate::app) fn satellite_view_center(&mut self, ui: &mut Ui) {
        let home_lat = self.config.ui.home_lat;
        let home_lon = self.config.ui.home_lon;

        // Forward decoded Orbcomm NORAD IDs for flash effect
        {
            let mut map = self.sat_map.lock();
            for entry in &self.orbcomm_log {
                if let rusty_sdr_orbcomm::parser::PacketType::SatelliteTelemetry { sat_id, .. } =
                    entry.packet.packet_type
                {
                    if let Some(norad) = rusty_sdr_tle::orbcomm_norad_id(sat_id) {
                        map.flash_norad_ids.push(norad);
                    }
                }
            }
        }

        // Mini signal strip at bottom
        let shared = Arc::clone(&self.shared);
        egui::TopBottomPanel::bottom("satellite_mini_signal")
            .exact_height(MINI_STRIP_HEIGHT)
            .frame(egui::Frame::none().fill(theme::BG))
            .show_inside(ui, |strip_ui| {
                self.mini_signal.set_db_range((self.fft_floor, self.fft_ceil));
                self.mini_signal.show(strip_ui, &shared);
            });

        // NOAA APT sidebar on the right
        egui::SidePanel::right("noaa_sidebar")
            .exact_width(320.0)
            .frame(egui::Frame::none().fill(theme::PANEL_BG))
            .show_inside(ui, |sidebar_ui| {
                self.noaa_apt.lock().show_embedded(sidebar_ui, home_lat, home_lon);
            });

        // Satellite map fills remaining center space
        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(egui::Color32::from_rgb(0x10, 0x14, 0x1A)))
            .show_inside(ui, |map_ui| {
                self.sat_map.lock().show_embedded(map_ui, home_lat, home_lon);
            });

        // Drain NOAA audio each frame when active
        if self.noaa_apt.lock().is_active {
            self.noaa_apt.lock().drain_audio();
        }

        // Consume tune request from sat map
        let sat_tune_freq = self.sat_map.lock().tune_frequency_hz.take();
        if let Some(freq_hz) = sat_tune_freq {
            self.handle_sat_tune(freq_hz);
        }

        // Consume tune request from NOAA panel
        let noaa_tune_freq = self.noaa_apt.lock().tune_frequency_hz.take();
        if let Some(freq_hz) = noaa_tune_freq {
            self.handle_noaa_tune(freq_hz);
        }
    }
}
