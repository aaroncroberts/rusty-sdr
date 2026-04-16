#![forbid(unsafe_code)]

//! Compile-time embedded screenshot assets for the Operators Handbook.
//!
//! Each PNG is embedded via `include_bytes!` at compile time.  The first call
//! to [`AssetLoader::get`] for a given key registers the texture with the egui
//! context and caches the [`egui::TextureHandle`] — subsequent calls return the
//! cached handle with no allocation.
//!
//! # Replacing placeholder screenshots
//!
//! The PNGs in `assets/` ship as minimal placeholders.  To replace with real
//! annotated screenshots:
//! 1. Capture the app UI (e.g. with macOS Screenshot or any image tool).
//! 2. Annotate with numbered callout circles.
//! 3. Save as PNG (< 512 KB each) to `src/handbook/assets/<name>.png`.
//! 4. `cargo build` — `include_bytes!` re-embeds them automatically.

use egui::{ColorImage, Context, TextureHandle, TextureOptions, Vec2};
use std::collections::HashMap;

// ── Embedded PNG bytes ────────────────────────────────────────────────────────

const FULL_LAYOUT_PNG: &[u8] =
    include_bytes!("assets/full_layout.png");
const LEFT_PANEL_PNG: &[u8] =
    include_bytes!("assets/left_panel.png");
const RIGHT_PANEL_PNG: &[u8] =
    include_bytes!("assets/right_panel.png");
const SPECTRUM_WF_PNG: &[u8] =
    include_bytes!("assets/spectrum_waterfall.png");
const STATUS_BAR_PNG: &[u8] =
    include_bytes!("assets/status_bar.png");
const FREQ_WIDGET_PNG: &[u8] =
    include_bytes!("assets/frequency_widget.png");

/// Map of asset key → raw PNG bytes.
fn asset_bytes(key: &str) -> Option<&'static [u8]> {
    match key {
        "full_layout" => Some(FULL_LAYOUT_PNG),
        "left_panel" => Some(LEFT_PANEL_PNG),
        "right_panel" => Some(RIGHT_PANEL_PNG),
        "spectrum_waterfall" => Some(SPECTRUM_WF_PNG),
        "status_bar" => Some(STATUS_BAR_PNG),
        "frequency_widget" => Some(FREQ_WIDGET_PNG),
        _ => None,
    }
}

// ── AssetLoader ───────────────────────────────────────────────────────────────

/// Caches egui texture handles for embedded PNG assets.
///
/// Call [`AssetLoader::get`] each frame — it registers the texture on first
/// access and returns a reference to the cached handle thereafter.
pub struct AssetLoader {
    cache: HashMap<&'static str, TextureHandle>,
}

impl AssetLoader {
    pub fn new() -> Self {
        Self {
            cache: HashMap::new(),
        }
    }

    /// Return a reference to the texture for `key`, registering it with the
    /// egui context on first call.  Returns `None` for unknown keys.
    pub fn get(&mut self, ctx: &Context, key: &'static str) -> Option<&TextureHandle> {
        if !self.cache.contains_key(key) {
            let bytes = asset_bytes(key)?;
            let image = load_png_bytes(bytes)?;
            let handle = ctx.load_texture(key, image, TextureOptions::LINEAR);
            self.cache.insert(key, handle);
        }
        self.cache.get(key)
    }

    /// Render an embedded image into `ui`, scaling it to fit `max_width` while
    /// maintaining aspect ratio.  Draws a thin border and an optional caption.
    pub fn render_image(
        &mut self,
        ui: &mut egui::Ui,
        key: &'static str,
        max_width: f32,
        caption: Option<&str>,
    ) {
        let ctx = ui.ctx().clone();
        if let Some(handle) = self.get(&ctx, key) {
            let orig = handle.size_vec2();
            let scale = (max_width / orig.x).min(1.0);
            let size = Vec2::new(orig.x * scale, orig.y * scale);

            // Centered
            ui.vertical_centered(|ui| {
                // Border frame around image
                egui::Frame::none()
                    .stroke(egui::Stroke::new(1.0, egui::Color32::from_gray(180)))
                    .inner_margin(egui::Margin::same(2.0))
                    .show(ui, |ui| {
                        ui.image((handle.id(), size));
                    });

                if let Some(cap) = caption {
                    ui.add_space(2.0);
                    ui.label(
                        egui::RichText::new(cap)
                            .small()
                            .color(egui::Color32::from_gray(110))
                            .italics(),
                    );
                }
            });
        } else {
            // Graceful fallback if key is unknown
            ui.label(
                egui::RichText::new(format!("[image: {key}]"))
                    .small()
                    .color(egui::Color32::from_gray(140)),
            );
        }
    }
}

impl Default for AssetLoader {
    fn default() -> Self {
        Self::new()
    }
}

// ── PNG decoding ──────────────────────────────────────────────────────────────

/// Decode raw PNG bytes into an egui `ColorImage`.
/// Uses the `image` crate which is already a transitive dependency of eframe.
fn load_png_bytes(bytes: &[u8]) -> Option<ColorImage> {
    let img = image::load_from_memory(bytes).ok()?.to_rgba8();
    let (w, h) = img.dimensions();
    let pixels: Vec<egui::Color32> = img
        .pixels()
        .map(|p| egui::Color32::from_rgba_unmultiplied(p[0], p[1], p[2], p[3]))
        .collect();
    Some(ColorImage {
        size: [w as usize, h as usize],
        pixels,
    })
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_asset_keys_have_bytes() {
        let keys = [
            "full_layout",
            "left_panel",
            "right_panel",
            "spectrum_waterfall",
            "status_bar",
            "frequency_widget",
        ];
        for key in &keys {
            let b = asset_bytes(key);
            assert!(b.is_some(), "missing bytes for key '{key}'");
            assert!(!b.unwrap().is_empty(), "empty PNG for key '{key}'");
        }
    }

    #[test]
    fn unknown_key_returns_none() {
        assert!(asset_bytes("does_not_exist").is_none());
    }

    #[test]
    fn placeholder_pngs_are_valid_png_signature() {
        let keys = ["full_layout", "left_panel", "spectrum_waterfall"];
        let png_sig = b"\x89PNG\r\n\x1a\n";
        for key in &keys {
            let bytes = asset_bytes(key).unwrap();
            assert!(
                bytes.starts_with(png_sig),
                "'{key}' does not start with PNG signature"
            );
        }
    }

    #[test]
    fn placeholder_pngs_decode_to_color_image() {
        let keys = [
            "full_layout",
            "left_panel",
            "right_panel",
            "spectrum_waterfall",
            "status_bar",
            "frequency_widget",
        ];
        for key in &keys {
            let bytes = asset_bytes(key).unwrap();
            let img = load_png_bytes(bytes);
            assert!(img.is_some(), "could not decode PNG for key '{key}'");
            let img = img.unwrap();
            assert!(img.size[0] > 0 && img.size[1] > 0, "zero-size image for '{key}'");
            assert_eq!(
                img.pixels.len(),
                img.size[0] * img.size[1],
                "pixel count mismatch for '{key}'"
            );
        }
    }

    #[test]
    fn asset_loader_default_is_empty() {
        let loader = AssetLoader::default();
        assert!(loader.cache.is_empty());
    }
}
