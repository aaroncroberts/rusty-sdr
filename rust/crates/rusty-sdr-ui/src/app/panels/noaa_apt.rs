//! NOAA APT (Automatic Picture Transmission) weather satellite panel.
//!
//! Shows pass predictions for NOAA-15, -18, and -19, one-click tune buttons,
//! decoder status, a live image preview, and a Save PNG button.
//!
//! The panel is displayed as a `egui::Window` via `show_viewport_deferred`.
//! TLEs are fetched once in a background thread (Celestrak, 24-hour cache).

use std::time::{Duration, Instant, SystemTime};

use crossbeam_channel::{Receiver, Sender};
use egui::{
    Align2, Color32, ColorImage, FontId, Frame, RichText, Rounding, TextureHandle, TextureOptions,
    Vec2,
};

use rusty_sdr_apt::AptLine;
use rusty_sdr_tle::{
    parser::TleEntry,
    predictor::{PassEvent, PassPredictor},
    NoaaSat, NOAA_APT_SATS,
};

// ── TLE fetch ─────────────────────────────────────────────────────────────────

struct TleFetchResult(Vec<TleEntry>);

fn fetch_tles_async(tx: Sender<TleFetchResult>) {
    std::thread::spawn(move || {
        let tles = rusty_sdr_tle::fetch_noaa_tles();
        let _ = tx.send(TleFetchResult(tles));
    });
}

// ── Per-satellite pass cache ──────────────────────────────────────────────────

struct SatPassData {
    passes: Vec<PassEvent>,
    last_update: Option<Instant>,
}

impl SatPassData {
    fn new() -> Self {
        Self { passes: Vec::new(), last_update: None }
    }
}

// ── Main panel ────────────────────────────────────────────────────────────────

/// NOAA APT floating window.
pub struct NoaaAptWindow {
    // ── TLE / pass data ───────────────────────────────────────────────────────
    tles: Vec<TleEntry>,
    /// Per-satellite pass data, indexed same as NOAA_APT_SATS.
    sat_passes: Vec<SatPassData>,
    tle_tx: Sender<TleFetchResult>,
    tle_rx: Receiver<TleFetchResult>,
    tle_fetch_started: bool,

    // ── APT decoder ───────────────────────────────────────────────────────────
    decoder: rusty_sdr_apt::AptDecoder,
    /// Accumulated image lines for the current pass.
    image_lines: Vec<AptLine>,
    /// egui texture for the live image preview (None until first lines arrive).
    image_texture: Option<TextureHandle>,
    /// True when new lines have arrived and the texture needs rebuilding.
    texture_dirty: bool,
    /// Line count at which the texture was last rebuilt.
    texture_rebuilt_at: usize,

    // ── UI state ──────────────────────────────────────────────────────────────
    /// When set, the main app should tune to this frequency.
    pub tune_frequency_hz: Option<u64>,
    /// True when the NOAA APT panel is actively tuned and ready to decode.
    pub is_active: bool,
    /// Selected satellite index (into NOAA_APT_SATS).
    selected_sat: usize,
    /// Save-PNG status message shown briefly after saving.
    save_status: Option<(String, Instant)>,
    /// Set to false when the OS viewport close button is pressed.
    pub viewport_open: bool,
}

impl NoaaAptWindow {
    pub fn new() -> Self {
        let (tle_tx, tle_rx) = crossbeam_channel::unbounded();
        Self {
            tles: Vec::new(),
            sat_passes: NOAA_APT_SATS.iter().map(|_| SatPassData::new()).collect(),
            tle_tx,
            tle_rx,
            tle_fetch_started: false,
            decoder: rusty_sdr_apt::AptDecoder::new(),
            image_lines: Vec::new(),
            image_texture: None,
            texture_dirty: false,
            texture_rebuilt_at: 0,
            tune_frequency_hz: None,
            is_active: false,
            selected_sat: 0,
            save_status: None,
            viewport_open: true,
        }
    }

    /// Push audio samples into the decoder (called by the main app when active).
    #[allow(dead_code)]
    pub fn push_audio(&mut self, audio: &[f32]) {
        let new_lines = self.decoder.push_audio(audio);
        if !new_lines.is_empty() {
            self.image_lines.extend(new_lines);
            self.texture_dirty = true;
        }
    }

    /// Reset the decoder and clear the current image (call when starting a new pass).
    pub fn reset_decoder(&mut self) {
        self.decoder.reset();
        self.image_lines.clear();
        self.image_texture = None;
        self.texture_dirty = false;
        self.texture_rebuilt_at = 0;
    }

    /// Show the NOAA APT window. Call each frame from the deferred viewport.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        open: &mut bool,
        home_lat: f64,
        home_lon: f64,
    ) {
        if ctx.input(|i| i.viewport().close_requested()) {
            *open = false;
        }
        ctx.request_repaint_after(Duration::from_millis(500));

        // ── Drain TLE results ─────────────────────────────────────────────────
        if let Ok(result) = self.tle_rx.try_recv() {
            self.tles = result.0;
            // Trigger pass recompute on next frame
            for sd in &mut self.sat_passes {
                sd.last_update = None;
            }
        }

        // Start TLE fetch on first show.
        if !self.tle_fetch_started {
            self.tle_fetch_started = true;
            fetch_tles_async(self.tle_tx.clone());
        }

        // ── Update pass predictions ───────────────────────────────────────────
        let now = SystemTime::now();
        for (sat_idx, sat_info) in NOAA_APT_SATS.iter().enumerate() {
            let sd = &mut self.sat_passes[sat_idx];
            let needs_update = sd.last_update
                .map(|t| t.elapsed() > Duration::from_secs(60))
                .unwrap_or(true);
            if needs_update {
                // Find the TLE for this NORAD ID
                if let Some(tle) = self.tles.iter().find(|t| t.norad_id == sat_info.norad_id) {
                    let predictor = PassPredictor::new(tle.clone(), home_lat, home_lon);
                    sd.passes = predictor.predict(now, Duration::from_secs(24 * 3600), 3);
                    sd.last_update = Some(Instant::now());
                }
            }
        }

        // ── Rebuild texture if needed ─────────────────────────────────────────
        let line_count = self.image_lines.len();
        if self.texture_dirty && line_count > 0
            && (line_count - self.texture_rebuilt_at >= 10 || self.texture_rebuilt_at == 0)
        {
            self.rebuild_texture(ctx);
            self.texture_rebuilt_at = line_count;
            self.texture_dirty = false;
        }

        // ── Render UI ─────────────────────────────────────────────────────────
        egui::CentralPanel::default()
            .frame(
                Frame::none()
                    .fill(Color32::from_rgb(0x10, 0x14, 0x1A))
                    .inner_margin(egui::Margin::ZERO),
            )
            .show(ctx, |ui| {
                // Split: left sidebar + right image area
                let avail = ui.available_rect_before_wrap();
                let sidebar_width = 220.0_f32;
                let sidebar_rect = egui::Rect::from_min_size(
                    avail.min,
                    egui::Vec2::new(sidebar_width, avail.height()),
                );
                let image_rect = egui::Rect::from_min_max(
                    egui::Pos2::new(avail.min.x + sidebar_width, avail.min.y),
                    avail.max,
                );

                self.draw_sidebar(ui, sidebar_rect, home_lat, home_lon);
                self.draw_image_area(ui, image_rect, ctx);
            });
    }

    // ── Sidebar ───────────────────────────────────────────────────────────────

    fn draw_sidebar(
        &mut self,
        ui: &mut egui::Ui,
        panel_rect: egui::Rect,
        _home_lat: f64,
        _home_lon: f64,
    ) {
        ui.painter()
            .rect_filled(panel_rect, Rounding::ZERO, Color32::from_rgb(0x16, 0x1A, 0x22));

        let mut sidebar_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(panel_rect.shrink(8.0))
                .layout(egui::Layout::top_down(egui::Align::LEFT)),
        );

        sidebar_ui.label(
            RichText::new("NOAA APT")
                .strong()
                .color(Color32::from_rgb(0x73, 0xC9, 0x91)),
        );
        sidebar_ui.label(
            RichText::new("Weather Satellite Imagery")
                .small()
                .color(Color32::GRAY),
        );
        sidebar_ui.separator();

        // ── Satellite list ────────────────────────────────────────────────────
        for (idx, sat) in NOAA_APT_SATS.iter().enumerate() {
            let is_selected = self.selected_sat == idx;
            let passes = &self.sat_passes[idx].passes;
            let now = SystemTime::now();

            // Next pass summary
            let next_pass_str = passes.first()
                .map(|p| {
                    p.aos.duration_since(now)
                        .map(|d| {
                            let m = d.as_secs() / 60;
                            if m == 0 {
                                "overhead now".to_string()
                            } else if m < 60 {
                                format!("in {m}m")
                            } else {
                                format!("in {}h{}m", m / 60, m % 60)
                            }
                        })
                        .unwrap_or_else(|_| "overhead".to_string())
                })
                .unwrap_or_else(|| {
                    if self.tles.is_empty() { "Loading…".to_string() } else { "No pass in 24h".to_string() }
                });

            let max_el_str = passes.first()
                .map(|p| format!(" {:.0}°", p.max_el_deg))
                .unwrap_or_default();

            let freq_mhz = sat.freq_hz as f64 / 1_000_000.0;
            let row_color = if is_selected {
                Color32::WHITE
            } else {
                Color32::from_rgb(0xAA, 0xAA, 0xAA)
            };

            sidebar_ui.add_space(4.0);
            let bg_color = if is_selected {
                Color32::from_rgb(0x1E, 0x2A, 0x1E)
            } else {
                Color32::TRANSPARENT
            };

            let row_resp = egui::Frame::none()
                .fill(bg_color)
                .inner_margin(egui::Margin::symmetric(4.0, 3.0))
                .show(&mut sidebar_ui, |ui| {
                    ui.set_min_width(panel_rect.width() - 24.0);

                    ui.horizontal(|ui| {
                        // Dot indicator
                        let dot_color = Color32::from_rgb(0x73, 0xC9, 0x91);
                        let (dot_rect, _) = ui.allocate_exact_size(Vec2::new(10.0, 12.0), egui::Sense::hover());
                        ui.painter().circle_filled(dot_rect.center(), if is_selected { 4.0 } else { 2.5 }, dot_color);

                        ui.vertical(|ui| {
                            ui.label(
                                RichText::new(sat.name)
                                    .strong()
                                    .small()
                                    .color(row_color),
                            );
                            ui.label(
                                RichText::new(format!("{freq_mhz:.4} MHz  {next_pass_str}{max_el_str}"))
                                    .size(9.5)
                                    .color(Color32::GRAY),
                            );
                        });
                    });
                });

            if row_resp.response.interact(egui::Sense::click()).clicked() {
                self.selected_sat = idx;
            }

            // Tune button for this satellite
            let tune_lbl = format!("Tune {:.3} MHz", freq_mhz);
            let btn_color = Color32::from_rgb(0x73, 0xC9, 0x91);
            if sidebar_ui
                .add(
                    egui::Button::new(
                        RichText::new(&tune_lbl).small().color(btn_color),
                    )
                    .fill(Color32::from_rgb(0x10, 0x1E, 0x14))
                    .min_size(Vec2::new(panel_rect.width() - 24.0, 22.0)),
                )
                .on_hover_text(format!("Tune to {} APT downlink on antenna C (ML-31)", sat.name))
                .clicked()
            {
                self.tune_frequency_hz = Some(sat.freq_hz);
                self.selected_sat = idx;
            }
        }

        sidebar_ui.add_space(6.0);
        sidebar_ui.separator();

        // ── Pass schedule for selected satellite ──────────────────────────────
        let sat_info: &NoaaSat = &NOAA_APT_SATS[self.selected_sat];
        let passes = &self.sat_passes[self.selected_sat].passes;

        sidebar_ui.label(
            RichText::new(format!("Next passes — {}", sat_info.name))
                .small()
                .color(Color32::GRAY),
        );

        if passes.is_empty() {
            sidebar_ui.label(
                RichText::new(if self.tles.is_empty() {
                    "Fetching TLEs from Celestrak…"
                } else {
                    "No passes in next 24 h"
                })
                .small()
                .color(Color32::from_rgb(0x88, 0x88, 0x88)),
            );
        } else {
            let now = SystemTime::now();
            for pass in passes {
                let aos_secs = pass.aos
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let los_secs = pass.los
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let duration_min = (los_secs.saturating_sub(aos_secs)) / 60;
                let aos_hms = format_utc_hms(aos_secs);
                let los_hms = format_utc_hms(los_secs);
                let from_now = pass.aos
                    .duration_since(now)
                    .map(|d| {
                        let m = d.as_secs() / 60;
                        if m < 60 {
                            format!("in {m}m")
                        } else {
                            format!("in {}h{}m", m / 60, m % 60)
                        }
                    })
                    .unwrap_or_else(|_| "now".to_string());

                sidebar_ui.add(egui::Separator::default().spacing(4.0));
                let el_color = if pass.max_el_deg > 60.0 {
                    Color32::from_rgb(0x44, 0xDD, 0x66)
                } else if pass.max_el_deg > 20.0 {
                    Color32::from_rgb(0xE8, 0xC5, 0x4B)
                } else {
                    Color32::from_rgb(0x88, 0xCC, 0x88)
                };
                sidebar_ui.label(
                    RichText::new(format!("{from_now}  max {:.0}°", pass.max_el_deg))
                        .small()
                        .color(el_color),
                );
                sidebar_ui.label(
                    RichText::new(format!("AOS {aos_hms} → LOS {los_hms}  ({duration_min}m)"))
                        .small()
                        .color(Color32::from_rgb(0xAA, 0xAA, 0xAA)),
                );
                sidebar_ui.label(
                    RichText::new(format!("Az {:.0}°  at AOS", pass.aos_az_deg))
                        .small()
                        .color(Color32::DARK_GRAY),
                );
            }
        }

        // ── Decoder status ────────────────────────────────────────────────────
        sidebar_ui.add_space(6.0);
        sidebar_ui.separator();

        let line_count = self.decoder.line_count();
        let (dot_color, status_text) = if self.is_active {
            (Color32::from_rgb(0x73, 0xC9, 0x91), "Active")
        } else {
            (Color32::from_rgb(0x6A, 0x7A, 0x8A), "Idle")
        };

        sidebar_ui.horizontal(|ui| {
            ui.colored_label(dot_color, "●");
            ui.label(RichText::new(status_text).color(Color32::GRAY).small());
            if line_count > 0 {
                ui.label(
                    RichText::new(format!("{line_count} lines"))
                        .color(Color32::from_rgb(0x88, 0x88, 0x88))
                        .small(),
                );
            }
        });

        if self.is_active && line_count == 0 {
            sidebar_ui.label(
                RichText::new("Waiting for sync…")
                    .color(Color32::from_rgb(0x88, 0x88, 0x88))
                    .small(),
            );
        }

        // ── Save PNG button ───────────────────────────────────────────────────
        sidebar_ui.add_space(6.0);

        let save_enabled = !self.image_lines.is_empty();
        let save_btn = egui::Button::new(
            RichText::new("💾  Save PNG")
                .small()
                .color(if save_enabled {
                    Color32::from_rgb(0xE8, 0xC5, 0x4B)
                } else {
                    Color32::from_rgb(0x55, 0x55, 0x55)
                }),
        )
        .fill(Color32::from_rgb(0x1A, 0x18, 0x0E));

        if sidebar_ui
            .add_enabled(save_enabled, save_btn)
            .on_hover_text("Save current image as PNG in ~/Pictures")
            .clicked()
        {
            self.save_png_to_pictures();
        }

        // Show save status if recent
        if let Some((ref msg, ts)) = self.save_status {
            if ts.elapsed() < Duration::from_secs(4) {
                sidebar_ui.label(
                    RichText::new(msg.as_str())
                        .small()
                        .color(Color32::from_rgb(0x73, 0xC9, 0x91)),
                );
            } else {
                self.save_status = None;
            }
        }

        // ── Clear image button ────────────────────────────────────────────────
        if save_enabled {
            sidebar_ui.add_space(4.0);
            if sidebar_ui
                .add(
                    egui::Button::new(
                        RichText::new("Clear image").small().color(Color32::GRAY),
                    )
                    .fill(Color32::TRANSPARENT),
                )
                .on_hover_text("Discard current image and start fresh")
                .clicked()
            {
                self.reset_decoder();
            }
        }
    }

    // ── Image area ────────────────────────────────────────────────────────────

    fn draw_image_area(
        &mut self,
        ui: &mut egui::Ui,
        image_rect: egui::Rect,
        _ctx: &egui::Context,
    ) {
        ui.painter().rect_filled(
            image_rect,
            Rounding::ZERO,
            Color32::from_rgb(0x08, 0x0C, 0x10),
        );

        if self.image_lines.is_empty() {
            // Placeholder when no image data yet
            ui.painter().text(
                image_rect.center(),
                Align2::CENTER_CENTER,
                if self.is_active {
                    "Receiving…\nWaiting for APT sync pulse"
                } else {
                    "No image data\nTune to a NOAA satellite to start"
                },
                FontId::proportional(13.0),
                Color32::from_rgb(0x55, 0x66, 0x55),
            );
            return;
        }

        // Channel labels
        let label_y = image_rect.min.y + 6.0;
        let label_color = Color32::from_rgb(0xAA, 0xAA, 0xAA);
        let label_font = FontId::proportional(10.0);

        if let Some(ref tex) = self.image_texture {
            let available = image_rect.shrink(4.0);
            // Maintain aspect ratio: tex_w x tex_h scaled to fit
            let tex_size = tex.size();
            let tex_w = tex_size[0] as f32;
            let tex_h = tex_size[1] as f32;
            let aspect = tex_w / tex_h.max(1.0);
            let draw_w = available.width().min(available.height() * aspect);
            let draw_h = draw_w / aspect.max(1e-6);
            let draw_rect = egui::Rect::from_center_size(
                available.center(),
                egui::Vec2::new(draw_w, draw_h),
            );

            ui.painter().image(
                tex.id(),
                draw_rect,
                egui::Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );

            // Channel A / B labels at top of image
            let mid_x = draw_rect.min.x + draw_rect.width() * 0.25;
            let mid_b_x = draw_rect.min.x + draw_rect.width() * 0.75;
            ui.painter().text(
                egui::Pos2::new(mid_x, label_y),
                Align2::CENTER_TOP,
                "Ch A (Visible / Near-IR)",
                label_font.clone(),
                label_color,
            );
            ui.painter().text(
                egui::Pos2::new(mid_b_x, label_y),
                Align2::CENTER_TOP,
                "Ch B (Thermal IR)",
                label_font,
                label_color,
            );
        }
    }

    // ── Texture rebuild ───────────────────────────────────────────────────────

    fn rebuild_texture(&mut self, ctx: &egui::Context) {
        let line_count = self.image_lines.len();
        if line_count == 0 {
            return;
        }

        let width = rusty_sdr_apt::CHAN_A_WIDTH + rusty_sdr_apt::CHAN_B_WIDTH; // 1818
        let mut pixels: Vec<u8> = vec![0u8; width * line_count];

        for (row, line) in self.image_lines.iter().enumerate() {
            // Channel A
            for (col, &px) in line.pixels[rusty_sdr_apt::CHAN_A_RANGE].iter().enumerate() {
                pixels[row * width + col] = px;
            }
            // Channel B
            for (col, &px) in line.pixels[rusty_sdr_apt::CHAN_B_RANGE].iter().enumerate() {
                pixels[row * width + rusty_sdr_apt::CHAN_A_WIDTH + col] = px;
            }
        }

        let color_image = ColorImage::from_gray([width, line_count], &pixels);
        self.image_texture = Some(ctx.load_texture(
            "noaa_apt_image",
            color_image,
            TextureOptions::LINEAR,
        ));
    }

    // ── PNG save ──────────────────────────────────────────────────────────────

    fn save_png_to_pictures(&mut self) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let filename = format!("noaa_apt_{now}.png");

        let save_dir = dirs::picture_dir()
            .or_else(|| dirs::home_dir())
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        let path = save_dir.join(&filename);

        match rusty_sdr_apt::save_png(&self.image_lines, &path) {
            Ok(()) => {
                tracing::info!(path = %path.display(), "NOAA APT PNG saved");
                self.save_status = Some((
                    format!("Saved: {}", path.display()),
                    Instant::now(),
                ));
            }
            Err(e) => {
                tracing::warn!(error = %e, "NOAA APT PNG save failed");
                self.save_status = Some((format!("Save failed: {e}"), Instant::now()));
            }
        }
    }
}

impl Default for NoaaAptWindow {
    fn default() -> Self {
        Self::new()
    }
}

// ── Utility ───────────────────────────────────────────────────────────────────

fn format_utc_hms(unix_secs: u64) -> String {
    let h = (unix_secs / 3600) % 24;
    let m = (unix_secs / 60) % 60;
    let s = unix_secs % 60;
    format!("{h:02}:{m:02}:{s:02}")
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_starts_with_viewport_open() {
        let w = NoaaAptWindow::new();
        assert!(w.viewport_open);
    }

    #[test]
    fn window_starts_idle() {
        let w = NoaaAptWindow::new();
        assert!(!w.is_active);
        assert!(w.image_lines.is_empty());
    }

    #[test]
    fn format_utc_hms_epoch() {
        assert_eq!(format_utc_hms(0), "00:00:00");
    }

    #[test]
    fn format_utc_hms_known() {
        // 1704110400 = 2024-01-01 12:00:00 UTC
        assert_eq!(format_utc_hms(1_704_110_400), "12:00:00");
    }

    #[test]
    fn noaa_sat_count() {
        assert_eq!(NOAA_APT_SATS.len(), 3);
    }

    #[test]
    fn reset_decoder_clears_image() {
        let mut w = NoaaAptWindow::new();
        w.image_lines.push(rusty_sdr_apt::AptLine {
            pixels: vec![128u8; rusty_sdr_apt::PIXELS_PER_LINE],
            line_num: 0,
        });
        w.reset_decoder();
        assert!(w.image_lines.is_empty());
    }
}
