//! UI panel modules — each adds methods to [`crate::app::SdrApp`].
//!
//! Splitting the panel implementations across files keeps each concern
//! focused while still sharing `SdrApp`'s private fields (Rust allows
//! `impl` blocks in child modules to access parent-module private fields).

pub(super) mod adsb_map;
pub(super) mod sat_map;
pub(super) mod noaa_apt;
mod orbcomm;
pub(super) mod center;
pub(super) mod left;
mod left_bookmarks;
mod left_device;
pub(super) mod right;
pub(super) mod settings;
pub(super) mod status;

use egui::{RichText, Ui};

use crate::theme;

/// Render a muted small-caps section heading with consistent spacing.
///
/// Used by every panel sub-section to avoid repeating the same
/// `RichText::new(...).color(TEXT_MUTED).small()` pattern.
#[allow(dead_code)]
pub(super) fn section_header(ui: &mut Ui, title: &str) {
    ui.label(RichText::new(title).color(theme::TEXT_MUTED).small());
    ui.add_space(4.0);
}

/// Deterministic color for a bookmark category, based on the string hash.
pub(super) fn category_color(category: &str) -> egui::Color32 {
    let hash: u32 = category
        .bytes()
        .fold(0u32, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u32));
    // Six visually distinct hues: cyan, green, yellow, orange, purple, pink
    const COLORS: [egui::Color32; 6] = [
        egui::Color32::from_rgb(0x4E, 0xC9, 0xE0),
        egui::Color32::from_rgb(0x73, 0xC9, 0x91),
        egui::Color32::from_rgb(0xE8, 0xC5, 0x4B),
        egui::Color32::from_rgb(0xE0, 0x8C, 0x4E),
        egui::Color32::from_rgb(0xB3, 0x7B, 0xD8),
        egui::Color32::from_rgb(0xE0, 0x6C, 0xAA),
    ];
    COLORS[(hash as usize) % COLORS.len()]
}

/// Format a frequency in Hz as a human-readable string.
pub(super) fn format_frequency(hz: u64) -> String {
    if hz >= 1_000_000_000 {
        format!("{:.3} GHz", hz as f64 / 1_000_000_000.0)
    } else if hz >= 1_000_000 {
        format!("{:.3} MHz", hz as f64 / 1_000_000.0)
    } else if hz >= 1_000 {
        format!("{:.1} kHz", hz as f64 / 1_000.0)
    } else {
        format!("{hz} Hz")
    }
}
