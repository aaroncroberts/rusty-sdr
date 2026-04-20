//! View-level layout methods for the Aircraft and Satellite views.
//!
//! Each view owns the `CentralPanel` area and is responsible for arranging
//! its primary content (map) and the mini signal strip at the bottom.
//!
//! These are stub implementations wired to the existing floating-window panels.
//! The full embedded layout (sdrpp-ir8z, sdrpp-g2pp) will replace these stubs.

use egui::Ui;
use std::sync::Arc;

use super::super::SdrApp;
use super::mini_signal::MINI_STRIP_HEIGHT;
use crate::theme;

impl SdrApp {
    /// Aircraft view center: ADS-B map filling the full area.
    ///
    /// Stub: renders a placeholder until the embedded ADS-B panel (sdrpp-ir8z)
    /// is built. A live mini signal strip is shown at the bottom so the operator
    /// always has spectrum/waterfall context. Clicking "Open Map" opens the
    /// existing floating window.
    pub(in crate::app) fn aircraft_view_center(&mut self, ui: &mut Ui) {
        // Mini signal strip anchored to the bottom
        egui::TopBottomPanel::bottom("aircraft_mini_signal")
            .exact_height(MINI_STRIP_HEIGHT)
            .frame(egui::Frame::none().fill(theme::BG))
            .show_inside(ui, |strip_ui| {
                let shared = Arc::clone(&self.shared);
                self.mini_signal.set_db_range((self.fft_floor, self.fft_ceil));
                self.mini_signal.show(strip_ui, &shared);
            });

        // Map placeholder in remaining space
        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(theme::BG))
            .show_inside(ui, |inner| {
                inner.vertical_centered(|inner| {
                    inner.add_space(60.0);
                    inner.label(
                        egui::RichText::new("✈  Aircraft View")
                            .size(28.0)
                            .color(theme::ACCENT),
                    );
                    inner.add_space(12.0);
                    inner.label(
                        egui::RichText::new("ADS-B map embeds here")
                            .color(theme::TEXT_MUTED),
                    );
                    inner.add_space(24.0);
                    if inner.button("Open Aircraft Map Window").clicked() {
                        self.show_adsb_map = true;
                    }
                });
            });
    }

    /// Satellite view center: satellite map filling the full area.
    ///
    /// Stub: renders a placeholder until the embedded satellite panel (sdrpp-g2pp)
    /// is built. A live mini signal strip is shown at the bottom.
    pub(in crate::app) fn satellite_view_center(&mut self, ui: &mut Ui) {
        // Mini signal strip anchored to the bottom
        egui::TopBottomPanel::bottom("satellite_mini_signal")
            .exact_height(MINI_STRIP_HEIGHT)
            .frame(egui::Frame::none().fill(theme::BG))
            .show_inside(ui, |strip_ui| {
                let shared = Arc::clone(&self.shared);
                self.mini_signal.set_db_range((self.fft_floor, self.fft_ceil));
                self.mini_signal.show(strip_ui, &shared);
            });

        // Map placeholder in remaining space
        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(theme::BG))
            .show_inside(ui, |inner| {
                inner.vertical_centered(|inner| {
                    inner.add_space(60.0);
                    inner.label(
                        egui::RichText::new("🛰  Satellite View")
                            .size(28.0)
                            .color(theme::ACCENT),
                    );
                    inner.add_space(12.0);
                    inner.label(
                        egui::RichText::new("Satellite map embeds here")
                            .color(theme::TEXT_MUTED),
                    );
                    inner.add_space(24.0);
                    if inner.button("Open Satellite Map Window").clicked() {
                        self.show_sat_map = true;
                    }
                });
            });
    }
}
