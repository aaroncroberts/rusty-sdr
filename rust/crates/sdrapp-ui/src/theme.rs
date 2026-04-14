#![forbid(unsafe_code)]

//! Visual design system for the SDR application.
//!
//! All colors, spacing, and font configuration in one place.
//! Apply with `theme::apply(ctx)` in `SdrApp::new`.

use egui::{Color32, FontDefinitions, FontFamily, Rounding, Stroke, Visuals};

// ── Background / Panel ────────────────────────────────────────────────────────

/// Main window background — deep navy-black
pub const BG: Color32 = Color32::from_rgb(10, 13, 20);

/// Panel backgrounds (side panels, etc.)
pub const PANEL_BG: Color32 = Color32::from_rgb(16, 20, 30);

/// Slightly lighter inset surfaces (collapsible headers, hover)
pub const SURFACE: Color32 = Color32::from_rgb(22, 28, 42);

/// Widget fill (buttons, sliders, inputs at rest)
pub const WIDGET_BG: Color32 = Color32::from_rgb(28, 36, 54);

/// Widget fill on hover
pub const WIDGET_HOVER: Color32 = Color32::from_rgb(38, 50, 72);

/// Widget fill when active / pressed
pub const WIDGET_ACTIVE: Color32 = Color32::from_rgb(48, 65, 95);

// ── Borders / Separators ─────────────────────────────────────────────────────

/// Subtle panel border
pub const BORDER: Color32 = Color32::from_rgb(35, 45, 65);

/// Stronger separator line
pub const SEPARATOR: Color32 = Color32::from_rgb(45, 58, 82);

// ── Text ─────────────────────────────────────────────────────────────────────

/// Primary text — light blue-white
pub const TEXT_PRIMARY: Color32 = Color32::from_rgb(210, 225, 245);

/// Secondary / muted text
pub const TEXT_MUTED: Color32 = Color32::from_rgb(120, 140, 175);

/// Disabled text
pub const TEXT_DISABLED: Color32 = Color32::from_rgb(65, 80, 110);

// ── Accent / Brand ───────────────────────────────────────────────────────────

/// Primary accent — electric cyan (tuning, VFO line, links)
pub const ACCENT: Color32 = Color32::from_rgb(0, 210, 255);

/// Accent dimmed (axis labels, grid minor)
pub const ACCENT_DIM: Color32 = Color32::from_rgb(0, 120, 160);

/// Secondary accent — warm amber (recording, warnings)
pub const AMBER: Color32 = Color32::from_rgb(255, 190, 40);

/// Danger / error — vivid red
pub const DANGER: Color32 = Color32::from_rgb(240, 60, 60);

// ── Signal / Spectrum ────────────────────────────────────────────────────────

/// Spectrum trace fill — cyan-green glow
pub const SPECTRUM_TRACE: Color32 = Color32::from_rgb(40, 220, 180);

/// Spectrum trace fill (semi-transparent base for polygon fill)
pub const SPECTRUM_FILL: Color32 = Color32::from_rgba_premultiplied(20, 140, 110, 80);

/// Spectrum grid lines
pub const SPECTRUM_GRID: Color32 = Color32::from_rgb(28, 40, 55);

/// VFO center line
pub const VFO_LINE: Color32 = ACCENT;

// ── Status Indicators ────────────────────────────────────────────────────────

/// Running / connected / healthy
pub const STATUS_OK: Color32 = Color32::from_rgb(50, 220, 80);

/// Warning / degraded
pub const STATUS_WARN: Color32 = AMBER;

/// Error / stopped / disconnected
pub const STATUS_ERROR: Color32 = DANGER;

// ── VU Meter ─────────────────────────────────────────────────────────────────

/// VU meter safe range (green)
pub const VU_LOW: Color32 = Color32::from_rgb(30, 200, 80);

/// VU meter mid range (yellow)
pub const VU_MID: Color32 = Color32::from_rgb(220, 190, 30);

/// VU meter clip range (red)
pub const VU_HIGH: Color32 = Color32::from_rgb(240, 50, 50);

// ── MIDI Page Colors ──────────────────────────────────────────────────────────

/// Page 0 — Tune
pub const MIDI_PAGE_0: Color32 = Color32::from_rgb(80, 180, 255);

/// Page 1 — Monitor
pub const MIDI_PAGE_1: Color32 = Color32::from_rgb(80, 220, 150);

/// Page 2 — Recorder
pub const MIDI_PAGE_2: Color32 = Color32::from_rgb(255, 140, 60);

// ── Spacing & Sizing ─────────────────────────────────────────────────────────

/// Standard inner panel margin
pub const PANEL_MARGIN: f32 = 10.0;

/// Spacing between logical groups
pub const GROUP_SPACING: f32 = 8.0;

/// Rounding for primary elements (panels, cards)
pub const ROUNDING: Rounding = Rounding::same(4.0);

/// Thin stroke for lines and separators
pub const STROKE_THIN: Stroke = Stroke {
    width: 1.0,
    color: SEPARATOR,
};

/// Accent stroke (VFO line, active selection)
pub const STROKE_ACCENT: Stroke = Stroke {
    width: 1.5,
    color: ACCENT,
};

// ── Waterfall Colormap ───────────────────────────────────────────────────────

/// Generate the 256-entry SDR thermal colormap.
///
/// Maps normalised power [0.0=noise floor, 1.0=full scale] → RGB.
/// Designed for real-world SDR signals where noise floor sits around t≈0.4–0.5
/// on a -120..0 dBFS scale (i.e. -60 to -48 dBFS).  The rapid colour
/// transitions at t=0.55–0.85 highlight signals just above the noise.
///
/// Color sequence: near-black → dark navy → dark blue → bright blue →
///   cyan-teal → green → yellow → orange → white.
pub fn waterfall_colormap() -> [Color32; 256] {
    // Control points: (t, r, g, b) where t ∈ [0.0, 1.0]
    const STOPS: &[(f32, u8, u8, u8)] = &[
        (0.00, 2, 2, 8),       // near-black (way below noise)
        (0.28, 5, 10, 55),     // dark navy (deep noise)
        (0.46, 12, 40, 145),   // dark blue (approaching noise floor)
        (0.56, 5, 120, 195),   // bright blue (noise floor)
        (0.66, 0, 210, 180),   // cyan-teal (signals emerging)
        (0.77, 40, 215, 55),   // green (strong signal)
        (0.86, 250, 215, 0),   // yellow (very strong)
        (0.94, 255, 100, 0),   // orange (near peak)
        (1.00, 255, 255, 255), // white (full scale)
    ];

    let mut lut = [Color32::BLACK; 256];
    for (i, entry) in lut.iter_mut().enumerate() {
        let t = i as f32 / 255.0;
        // Find the two surrounding stops
        let mut lower = STOPS[0];
        let mut upper = STOPS[STOPS.len() - 1];
        for j in 0..STOPS.len() - 1 {
            if t >= STOPS[j].0 && t <= STOPS[j + 1].0 {
                lower = STOPS[j];
                upper = STOPS[j + 1];
                break;
            }
        }
        let span = (upper.0 - lower.0).max(1e-6);
        let alpha = (t - lower.0) / span;
        let r = lerp_u8(lower.1, upper.1, alpha);
        let g = lerp_u8(lower.2, upper.2, alpha);
        let b = lerp_u8(lower.3, upper.3, alpha);
        *entry = Color32::from_rgb(r, g, b);
    }
    lut
}

#[inline]
fn lerp_u8(a: u8, b: u8, t: f32) -> u8 {
    (a as f32 + (b as f32 - a as f32) * t.clamp(0.0, 1.0)) as u8
}

// ── Application ──────────────────────────────────────────────────────────────

/// Apply the SDR app theme to the egui context.
///
/// Call once in `SdrApp::new` via `cc.egui_ctx`.
pub fn apply(ctx: &egui::Context) {
    let mut visuals = Visuals::dark();

    // Window / panel backgrounds
    visuals.window_fill = BG;
    visuals.panel_fill = PANEL_BG;
    visuals.extreme_bg_color = BG;

    // Window borders
    visuals.window_stroke = Stroke::new(1.0, BORDER);

    // Widget fill states
    visuals.widgets.inactive.bg_fill = WIDGET_BG;
    visuals.widgets.hovered.bg_fill = WIDGET_HOVER;
    visuals.widgets.active.bg_fill = WIDGET_ACTIVE;
    visuals.widgets.open.bg_fill = WIDGET_ACTIVE;

    // Widget strokes
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, BORDER);
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT_DIM);
    visuals.widgets.active.bg_stroke = Stroke::new(1.5, ACCENT);
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, SEPARATOR);

    // Text
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);
    visuals.widgets.active.fg_stroke = Stroke::new(1.0, Color32::WHITE);
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT_MUTED);
    visuals.widgets.open.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);

    // Rounding everywhere
    visuals.window_rounding = ROUNDING;
    visuals.widgets.inactive.rounding = ROUNDING;
    visuals.widgets.hovered.rounding = ROUNDING;
    visuals.widgets.active.rounding = ROUNDING;
    visuals.widgets.noninteractive.rounding = ROUNDING;

    // Selection highlight
    visuals.selection.bg_fill = Color32::from_rgba_premultiplied(0, 180, 220, 60);
    visuals.selection.stroke = Stroke::new(1.0, ACCENT);

    // Hyperlinks
    visuals.hyperlink_color = ACCENT;

    // Override slider fill to use accent
    visuals.slider_trailing_fill = true;

    ctx.set_visuals(visuals);

    // Fonts — add a monospace option for frequency display
    let mut fonts = FontDefinitions::default();
    // JetBrains Mono or similar is ideal, but we use egui's built-in monospace
    // to avoid bundling a font binary for now
    fonts.families.entry(FontFamily::Monospace).or_default();
    ctx.set_fonts(fonts);
}

/// Return the page accent color for a given MIDI page index.
pub fn midi_page_color(page: usize) -> Color32 {
    match page % 3 {
        0 => MIDI_PAGE_0,
        1 => MIDI_PAGE_1,
        _ => MIDI_PAGE_2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colormap_has_256_entries() {
        let lut = waterfall_colormap();
        assert_eq!(lut.len(), 256);
    }

    #[test]
    fn colormap_first_entry_is_dark() {
        let lut = waterfall_colormap();
        let r = lut[0].r();
        let g = lut[0].g();
        let b = lut[0].b();
        // First entry (noise floor) should be very dark
        assert!(
            r < 30 && g < 30 && b < 50,
            "noise floor should be dark, got ({r},{g},{b})"
        );
    }

    #[test]
    fn colormap_last_entry_is_bright() {
        let lut = waterfall_colormap();
        let c = lut[255];
        // Last entry (max signal) should be bright
        assert!(
            c.r() > 200 || c.g() > 200 || c.b() > 200,
            "peak should be bright"
        );
    }

    #[test]
    fn midi_page_color_wraps() {
        assert_eq!(midi_page_color(0), MIDI_PAGE_0);
        assert_eq!(midi_page_color(3), MIDI_PAGE_0);
        assert_eq!(midi_page_color(1), MIDI_PAGE_1);
        assert_eq!(midi_page_color(2), MIDI_PAGE_2);
    }
}
