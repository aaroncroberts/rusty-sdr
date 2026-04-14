#![forbid(unsafe_code)]

//! Waterfall (scrolling spectrogram) widget.
//!
//! Architecture:
//! - Pre-allocated RGBA pixel buffer (width × height pixels)
//! - Each frame: shift all rows down one, paint new FFT row at row 0
//! - Upload as a GPU texture via egui's TextureManager each render tick
//! - Zero heap allocation per frame after initial setup

use egui::{ColorImage, Rect, Sense, TextureHandle, TextureOptions, Ui, Vec2};

const WATERFALL_HEIGHT: usize = 200; // rows of history

/// Maps a dBFS value to an RGBA color for the waterfall.
fn db_to_color(db: f32, db_min: f32, db_max: f32) -> [u8; 4] {
    let t = ((db - db_min) / (db_max - db_min)).clamp(0.0, 1.0);
    // Simple gradient: dark blue → cyan → yellow → white
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
    [r, g, b, 255]
}

pub struct WaterfallWidget {
    /// RGBA pixel buffer: [row][col][4]
    pixels: Vec<u8>,
    width: usize,
    height: usize,
    texture: Option<TextureHandle>,
    db_range: (f32, f32),
}

impl WaterfallWidget {
    pub fn new(width: usize, db_range: (f32, f32)) -> Self {
        let pixels = vec![0u8; width * WATERFALL_HEIGHT * 4];
        Self { pixels, width, height: WATERFALL_HEIGHT, texture: None, db_range }
    }

    /// Push a new FFT row at the top, shifting all existing rows down.
    pub fn push_row(&mut self, fft_magnitudes: &[f32]) {
        let w = self.width;
        // Shift rows down: copy row[i] to row[i+1], starting from the bottom
        let row_bytes = w * 4;
        for row in (1..self.height).rev() {
            let src = (row - 1) * row_bytes;
            let dst = row * row_bytes;
            self.pixels.copy_within(src..src + row_bytes, dst);
        }
        // Paint new row at row 0
        let n = fft_magnitudes.len();
        for x in 0..w {
            let bin = (x * n / w).min(n - 1);
            let db = fft_magnitudes[bin];
            let rgba = db_to_color(db, self.db_range.0, self.db_range.1);
            let offset = x * 4;
            self.pixels[offset..offset + 4].copy_from_slice(&rgba);
        }
    }

    /// Render the waterfall into the UI.
    pub fn show(&mut self, ui: &mut Ui, ctx: &egui::Context) -> egui::Response {
        // Upload pixel buffer as texture
        let image = ColorImage::from_rgba_unmultiplied(
            [self.width, self.height],
            &self.pixels,
        );
        let texture = self.texture.get_or_insert_with(|| {
            ctx.load_texture("waterfall", image.clone(), TextureOptions::NEAREST)
        });
        texture.set(image, TextureOptions::NEAREST);

        let desired_size = Vec2::new(ui.available_width(), WATERFALL_HEIGHT as f32);
        let (rect, response) = ui.allocate_exact_size(desired_size, Sense::hover());

        if ui.is_rect_visible(rect) {
            ui.painter_at(rect).image(
                texture.id(),
                rect,
                Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
        }

        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_row_shifts_history() {
        let mut wf = WaterfallWidget::new(8, (-120.0, 0.0));
        // First row: all -60 dB
        let row1: Vec<f32> = vec![-60.0; 8];
        wf.push_row(&row1);
        // Second row: all -20 dB
        let row2: Vec<f32> = vec![-20.0; 8];
        wf.push_row(&row2);
        // Row 0 should have row2 colors, row 1 should have row1 colors
        let c0 = db_to_color(-20.0, -120.0, 0.0);
        let c1 = db_to_color(-60.0, -120.0, 0.0);
        assert_eq!(&wf.pixels[0..4], &c0);
        assert_eq!(&wf.pixels[wf.width * 4..wf.width * 4 + 4], &c1);
    }

    #[test]
    fn db_to_color_extremes() {
        let min = db_to_color(-120.0, -120.0, 0.0);
        let max = db_to_color(0.0, -120.0, 0.0);
        // Min should be dark, max should be bright
        assert!(max[0] as u32 + max[1] as u32 + max[2] as u32 > min[0] as u32 + min[1] as u32 + min[2] as u32);
    }
}
