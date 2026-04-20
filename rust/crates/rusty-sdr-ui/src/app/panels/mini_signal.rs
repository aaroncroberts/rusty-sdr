//! Mini signal strip — compact spectrum + waterfall for Aircraft and Satellite views.
//!
//! Renders a ≈120px-tall strip at the bottom of the view containing:
//! - A short spectrum trace (≈50px)
//! - A scrolling waterfall (≈70px)
//!
//! The strip reads FFT data live from [`SharedState`] and owns its own
//! [`WaterfallWidget`] with a 60-row history (vs 200 for the full view).
//! Both the spectrum and waterfall display the same zoomed frequency window
//! as the main display, so they are always in sync.

use egui::Ui;
use parking_lot::RwLock;
use std::sync::Arc;

use rusty_sdr_core::signal_path::SharedState;

use crate::{
    spectrum::SpectrumWidget,
    theme,
    waterfall::{WaterfallWidget, WaterfallOverlay},
};

/// Total height of the mini strip in logical pixels.
pub const MINI_STRIP_HEIGHT: f32 = 120.0;
/// Height of the spectrum trace portion.
const SPECTRUM_HEIGHT: f32 = 50.0;
/// Height of the waterfall portion.
const WATERFALL_HEIGHT: f32 = 70.0;
/// Waterfall history rows for the mini strip.
const MINI_WF_ROWS: usize = 60;

pub struct MiniSignalStrip {
    waterfall: WaterfallWidget,
    /// Fractional row accumulator — same logic as the main waterfall speed control.
    row_frac: f32,
    /// Last center freq seen — used to clear waterfall on retune.
    last_center_freq: u64,
    /// Cached db range from shared state.
    db_range: (f32, f32),
}

impl MiniSignalStrip {
    pub fn new(db_range: (f32, f32)) -> Self {
        let mut wf = WaterfallWidget::new_with_height_colormap(1024, db_range, MINI_WF_ROWS);
        wf.set_colormap(theme::waterfall_colormap());
        Self {
            waterfall: wf,
            row_frac: 0.0,
            last_center_freq: 0,
            db_range,
        }
    }

    /// Update db range (call when fft_floor/fft_ceil changes in main app).
    pub fn set_db_range(&mut self, range: (f32, f32)) {
        self.db_range = range;
        self.waterfall.set_db_range(range);
    }

    /// Push a new FFT row and render the strip into `ui`.
    ///
    /// This should be called once per frame when the view is active.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        shared: &Arc<RwLock<SharedState>>,
    ) {
        let (
            fft_data,
            freq_hz,
            sample_rate,
            zoom_level,
            waterfall_speed,
            demod_mode,
            nfm_bw_hz,
            center_freq,
        ) = {
            let s = shared.read();
            (
                s.fft.fft_magnitudes.clone(),
                s.center_freq_hz,
                s.sample_rate_sps,
                s.zoom_level,
                s.waterfall_speed,
                s.demod.demod_mode,
                s.demod.nfm_bandwidth_hz,
                s.center_freq_hz,
            )
        };

        // Clear waterfall on large retune
        if center_freq != 0 && self.last_center_freq != 0 {
            let bw = sample_rate as u64;
            let threshold = (bw / 10).max(50_000);
            if center_freq.abs_diff(self.last_center_freq) > threshold {
                self.waterfall.clear();
            }
        }
        self.last_center_freq = center_freq;

        // Push a waterfall row at the configured speed
        if !fft_data.is_empty() {
            self.row_frac += waterfall_speed;
            if self.row_frac >= 1.0 {
                self.row_frac -= 1.0;
                self.waterfall.push_row(&fft_data, zoom_level);
            }
        }

        // Compute frequency display range (same logic as center.rs)
        let bw = sample_rate as u64;
        let half_bw = (bw as f64 * zoom_level as f64 / 2.0) as u64;
        let freq_lo = freq_hz.saturating_sub(half_bw);
        let freq_hi = freq_hz.saturating_add(half_bw);

        // Compute filter edges for overlay
        use rusty_sdr_core::signal_path::DemodMode;
        let (filter_lo, filter_hi) = match demod_mode {
            DemodMode::Wbfm => {
                let bw = 75_000u64;
                (freq_hz.saturating_sub(bw), freq_hz.saturating_add(bw))
            }
            DemodMode::Nfm => {
                let bw = (nfm_bw_hz / 2) as u64;
                (freq_hz.saturating_sub(bw), freq_hz.saturating_add(bw))
            }
            DemodMode::Am => {
                let bw = 5_000u64;
                (freq_hz.saturating_sub(bw), freq_hz.saturating_add(bw))
            }
            DemodMode::Usb => (freq_hz, freq_hz.saturating_add(3_000)),
            DemodMode::Lsb => (freq_hz.saturating_sub(3_000), freq_hz),
            DemodMode::Dsb => {
                let bw = 3_000u64;
                (freq_hz.saturating_sub(bw), freq_hz.saturating_add(bw))
            }
            DemodMode::Cw => {
                let bw = 500u64;
                (freq_hz.saturating_sub(bw), freq_hz.saturating_add(bw))
            }
        };

        // Draw a subtle top border line
        let strip_rect = ui.available_rect_before_wrap();
        ui.painter().hline(
            strip_rect.left()..=strip_rect.right(),
            strip_rect.top(),
            egui::Stroke::new(1.0, theme::SEPARATOR),
        );

        // ── Spectrum trace ────────────────────────────────────────────────────
        let (spectrum_rect, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), SPECTRUM_HEIGHT),
            egui::Sense::hover(),
        );
        if ui.is_rect_visible(spectrum_rect) && !fft_data.is_empty() {
            let spectrum = SpectrumWidget {
                fft_data: &fft_data,
                db_range: self.db_range,
                freq_range: (freq_lo, freq_hi),
                vfo_hz: freq_hz,
                filter_lo_hz: filter_lo,
                filter_hi_hz: filter_hi,
                peak_hold: None,
                show_band_plan: false,
                hover_pos: None,
                tune_step_hz: 0,
                peak_marker_hz: &[],
                locked_freq_hz: None,
            };
            // Paint into the allocated rect
            ui.allocate_new_ui(egui::UiBuilder::new().max_rect(spectrum_rect), |inner| {
                spectrum.show(inner);
            });
        }

        // ── Waterfall ─────────────────────────────────────────────────────────
        let wf_overlay = WaterfallOverlay {
            vfo_hz: freq_hz,
            freq_range: (freq_lo, freq_hi),
            filter_lo_hz: filter_lo,
            filter_hi_hz: filter_hi,
        };
        // Resize waterfall width to match current available width
        let avail_w = ui.available_width() as usize;
        self.waterfall.set_width(avail_w.max(64));
        self.waterfall.set_db_range(self.db_range);

        let wf_rect = ui.available_rect_before_wrap();
        let wf_rect = egui::Rect::from_min_size(
            wf_rect.min,
            egui::vec2(wf_rect.width(), WATERFALL_HEIGHT),
        );
        let egui_ctx = ui.ctx().clone();
        let wf = &mut self.waterfall;
        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(wf_rect), |inner| {
            wf.show(inner, &egui_ctx, Some(wf_overlay), 0.0);
        });
        // Consume the allocated height
        ui.allocate_space(egui::vec2(0.0, WATERFALL_HEIGHT));
    }
}
