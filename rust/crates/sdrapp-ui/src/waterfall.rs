#![forbid(unsafe_code)]

//! Waterfall (scrolling spectrogram) widget.
//!
//! Architecture:
//! - Pre-allocated RGBA pixel buffer (width × height pixels)
//! - Each frame: shift all rows down one, paint new FFT row at row 0
//! - Upload as a GPU texture via egui's TextureManager each render tick
//! - Zero heap allocation per frame after initial setup
//! - Colormap is a 256-entry viridis-inspired LUT from theme::waterfall_colormap()

use egui::{Color32, ColorImage, Rect, Sense, TextureHandle, TextureOptions, Ui, Vec2};

use crate::theme;

const WATERFALL_HEIGHT: usize = 200; // rows of history

/// Parameters for the VFO overlay drawn on top of the waterfall texture.
///
/// Mirrors the passband shading already shown on the spectrum above, so the
/// user can see exactly where they are tuned in the scrolling history.
pub struct WaterfallOverlay {
    /// Center frequency of the active VFO in Hz.
    pub vfo_hz: u64,
    /// Visible frequency range (lo, hi) in Hz — maps to the left/right edges.
    pub freq_range: (u64, u64),
    /// Left edge of the demodulator passband in Hz.
    pub filter_lo_hz: u64,
    /// Right edge of the demodulator passband in Hz.
    pub filter_hi_hz: u64,
}

pub struct WaterfallWidget {
    /// RGBA pixel buffer: row-major, width × height pixels × 4 bytes
    pixels: Vec<u8>,
    width: usize,
    height: usize,
    texture: Option<TextureHandle>,
    db_range: (f32, f32),
    /// Pre-built colormap LUT
    colormap: [Color32; 256],
}

impl WaterfallWidget {
    /// Create with the default simple gradient (backwards-compatible).
    pub fn new(width: usize, db_range: (f32, f32)) -> Self {
        let pixels = vec![0u8; width * WATERFALL_HEIGHT * 4];
        let colormap = build_simple_colormap();
        Self {
            pixels,
            width,
            height: WATERFALL_HEIGHT,
            texture: None,
            db_range,
            colormap,
        }
    }

    /// Create with the theme's viridis-inspired colormap.
    pub fn new_with_colormap(width: usize, db_range: (f32, f32)) -> Self {
        let pixels = vec![0u8; width * WATERFALL_HEIGHT * 4];
        let colormap = theme::waterfall_colormap();
        Self {
            pixels,
            width,
            height: WATERFALL_HEIGHT,
            texture: None,
            db_range,
            colormap,
        }
    }

    /// Push a new FFT row at the top, shifting all existing rows down.
    pub fn push_row(&mut self, fft_magnitudes: &[f32]) {
        let w = self.width;
        let row_bytes = w * 4;

        // Shift rows down: row[i] ← row[i-1], starting from the bottom
        for row in (1..self.height).rev() {
            let src = (row - 1) * row_bytes;
            let dst = row * row_bytes;
            self.pixels.copy_within(src..src + row_bytes, dst);
        }

        // Paint new row at row 0 using the colormap LUT
        let n = fft_magnitudes.len();
        let (db_min, db_max) = self.db_range;
        for x in 0..w {
            let bin = (x * n / w).min(n - 1);
            let db = fft_magnitudes[bin];
            let t = ((db - db_min) / (db_max - db_min)).clamp(0.0, 1.0);
            let idx = (t * 255.0) as usize;
            let c = self.colormap[idx];
            let offset = x * 4;
            self.pixels[offset] = c.r();
            self.pixels[offset + 1] = c.g();
            self.pixels[offset + 2] = c.b();
            self.pixels[offset + 3] = 255;
        }
    }

    /// Update the dBFS mapping range (used by push_row for future rows).
    pub fn set_db_range(&mut self, range: (f32, f32)) {
        self.db_range = range;
    }

    /// Replace the colormap LUT (takes effect on the next push_row call).
    pub fn set_colormap(&mut self, colormap: [egui::Color32; 256]) {
        self.colormap = colormap;
    }

    /// Clear all waterfall history (fill with black).
    ///
    /// Call when the center frequency changes substantially so stale rows
    /// from the old frequency are not mixed with new data at the new frequency.
    pub fn clear(&mut self) {
        self.pixels.iter_mut().for_each(|b| *b = 0);
        // Force texture re-upload on the next show() call.
        self.texture = None;
    }

    /// Render the waterfall into the UI.
    ///
    /// The texture contains `WATERFALL_HEIGHT` rows of history; the display rect
    /// is stretched to fill all remaining vertical space so there is no dead zone.
    ///
    /// Pass `overlay` to draw a VFO center-line and passband shading on top of
    /// the spectrogram texture (same visual feedback as the spectrum above).
    pub fn show(
        &mut self,
        ui: &mut Ui,
        ctx: &egui::Context,
        overlay: Option<WaterfallOverlay>,
    ) -> egui::Response {
        let image = ColorImage::from_rgba_unmultiplied([self.width, self.height], &self.pixels);

        let texture = self.texture.get_or_insert_with(|| {
            ctx.load_texture("waterfall", image.clone(), TextureOptions::LINEAR)
        });
        texture.set(image, TextureOptions::LINEAR);

        // Fill ALL remaining height — stretch the texture, which gives the
        // appearance of slower scrolling and eliminates the dead zone below.
        let display_h = ui.available_height().max(60.0);
        let desired_size = Vec2::new(ui.available_width(), display_h);
        let (rect, response) = ui.allocate_exact_size(desired_size, Sense::click_and_drag());

        if ui.is_rect_visible(rect) {
            let painter = ui.painter_at(rect);
            painter.image(
                texture.id(),
                rect,
                Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                Color32::WHITE,
            );

            // ── VFO overlay ───────────────────────────────────────────────────
            if let Some(ov) = overlay {
                let (freq_lo, freq_hi) = ov.freq_range;
                let span = (freq_hi as f64 - freq_lo as f64).max(1.0);

                // Helper: frequency → x pixel within rect
                let freq_to_x = |hz: u64| -> f32 {
                    let t = ((hz as f64 - freq_lo as f64) / span).clamp(0.0, 1.0) as f32;
                    rect.left() + t * rect.width()
                };

                // Passband shading (semi-transparent tint matching the spectrum)
                if ov.filter_lo_hz < ov.filter_hi_hz {
                    let x0 = freq_to_x(ov.filter_lo_hz);
                    let x1 = freq_to_x(ov.filter_hi_hz);
                    if x1 > x0 {
                        let shade_rect = Rect::from_x_y_ranges(x0..=x1, rect.top()..=rect.bottom());
                        painter.rect_filled(
                            shade_rect,
                            0.0,
                            Color32::from_rgba_unmultiplied(100, 160, 255, 30),
                        );
                    }
                }

                // VFO center-line (1 px wide, white with moderate alpha)
                let cx = freq_to_x(ov.vfo_hz);
                painter.line_segment(
                    [
                        egui::pos2(cx, rect.top()),
                        egui::pos2(cx, rect.bottom()),
                    ],
                    egui::Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 180)),
                );
            }
        }

        response
    }
}

/// Simple gradient for backwards compatibility in tests.
fn build_simple_colormap() -> [Color32; 256] {
    let mut lut = [Color32::BLACK; 256];
    for (i, entry) in lut.iter_mut().enumerate() {
        let t = i as f32 / 255.0;
        let (r, g, b) = if t < 0.33 {
            let s = t / 0.33;
            (0_u8, (s * 180.0) as u8, (50.0 + s * 205.0) as u8)
        } else if t < 0.66 {
            let s = (t - 0.33) / 0.33;
            ((s * 255.0) as u8, 180_u8, (255.0 - s * 255.0) as u8)
        } else {
            let s = (t - 0.66) / 0.34;
            (255_u8, (180.0 + s * 75.0) as u8, (s * 255.0) as u8)
        };
        *entry = Color32::from_rgb(r, g, b);
    }
    lut
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_row_shifts_history() {
        let mut wf = WaterfallWidget::new(8, (-120.0, 0.0));

        let row1: Vec<f32> = vec![-60.0; 8];
        wf.push_row(&row1);

        let row2: Vec<f32> = vec![-20.0; 8];
        wf.push_row(&row2);

        // Row 0 (top) should reflect row2; row 1 should reflect row1
        // We can't compare exact colors without knowing the LUT, so verify they differ
        let r0 = &wf.pixels[0..4];
        let r1 = &wf.pixels[wf.width * 4..wf.width * 4 + 4];
        assert_ne!(
            r0, r1,
            "rows should differ after pushing different dBFS levels"
        );
    }

    #[test]
    fn push_row_bright_above_noise() {
        let mut wf = WaterfallWidget::new(8, (-120.0, 0.0));

        // Strong signal at 0 dBFS → should produce a bright (high value) color
        wf.push_row(&vec![0.0; 8]);
        let bright: u32 = wf.pixels[0..3].iter().map(|&v| v as u32).sum();

        let mut wf2 = WaterfallWidget::new(8, (-120.0, 0.0));
        // Noise floor at -120 dBFS → should be dark
        wf2.push_row(&vec![-120.0; 8]);
        let dark: u32 = wf2.pixels[0..3].iter().map(|&v| v as u32).sum();

        assert!(
            bright > dark,
            "0 dBFS should be brighter than -120 dBFS: {bright} vs {dark}"
        );
    }

    #[test]
    fn viridis_colormap_extremes() {
        let lut = theme::waterfall_colormap();
        let dark_sum: u32 = [lut[0].r(), lut[0].g(), lut[0].b()]
            .iter()
            .map(|&v| v as u32)
            .sum();
        let bright_sum: u32 = [lut[255].r(), lut[255].g(), lut[255].b()]
            .iter()
            .map(|&v| v as u32)
            .sum();
        assert!(
            bright_sum > dark_sum,
            "viridis max should be brighter than min"
        );
    }
}
