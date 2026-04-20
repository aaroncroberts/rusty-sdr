//! Geo/Mercator projection helpers and altitude color gradient.

use egui::{Color32, Pos2, Rect, Vec2};

// ── Constants ─────────────────────────────────────────────────────────────────

/// Maximum trail points kept per aircraft.
pub const MAX_TRAIL: usize = 120;

/// Minimum pixels-per-degree to clamp zoom (shows the whole world).
pub(super) const MIN_ZOOM: f32 = 1.2;
/// Maximum pixels per degree (roughly street-level).
pub(super) const MAX_ZOOM: f32 = 800.0;

/// Size of the aircraft icon (radius in pixels).
pub(super) const ICON_R: f32 = 9.0;

/// Width of the aircraft detail side panel.
pub(super) const DETAIL_WIDTH: f32 = 210.0;

// ── Altitude → color gradient ──────────────────────────────────────────────

pub(crate) fn altitude_color(alt_ft: Option<i32>) -> Color32 {
    let alt = alt_ft.unwrap_or(0).max(0) as f32;
    // Gradient: ground (green) → 15k (yellow) → 30k (orange) → 45k (red)
    let t = (alt / 45_000.0).clamp(0.0, 1.0);
    if t < 0.33 {
        let u = t / 0.33;
        lerp_color(Color32::from_rgb(0x73, 0xC9, 0x91), Color32::from_rgb(0xE8, 0xC5, 0x4B), u)
    } else if t < 0.66 {
        let u = (t - 0.33) / 0.33;
        lerp_color(Color32::from_rgb(0xE8, 0xC5, 0x4B), Color32::from_rgb(0xE0, 0x8C, 0x4E), u)
    } else {
        let u = (t - 0.66) / 0.34;
        lerp_color(Color32::from_rgb(0xE0, 0x8C, 0x4E), Color32::from_rgb(0xFF, 0x55, 0x55), u)
    }
}

pub(crate) fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    Color32::from_rgb(
        (a.r() as f32 + (b.r() as f32 - a.r() as f32) * t) as u8,
        (a.g() as f32 + (b.g() as f32 - a.g() as f32) * t) as u8,
        (a.b() as f32 + (b.b() as f32 - a.b() as f32) * t) as u8,
    )
}

// ── Mercator projection ────────────────────────────────────────────────────────

/// Convert geographic coordinates to Mercator screen position within `rect`.
///
/// - `zoom_ppd`: pixels per degree of longitude.
/// - `center_lat / center_lon`: map center in geographic degrees.
pub(crate) fn geo_to_screen(
    rect: Rect,
    lat: f64,
    lon: f64,
    center_lat: f64,
    center_lon: f64,
    zoom_ppd: f32,
) -> Pos2 {
    let dx = (lon - center_lon) as f32 * zoom_ppd;

    // Mercator Y: use lat-to-radians for proper scaling
    // Mercator Y coordinate: ln(tan(lat) + sec(lat)) = ln((1+sin(lat))/cos(lat))
    let merc = |deg: f64| {
        let rad = deg.to_radians();
        (rad.tan() + 1.0 / rad.cos()).ln() as f32
    };
    // dy: positive screen Y = south, so negate
    let dy = -(merc(lat) - merc(center_lat)) * (zoom_ppd * std::f32::consts::FRAC_1_PI * 180.0);

    rect.center() + Vec2::new(dx, dy)
}

/// Mercator Y scale factor at a given latitude (pixels per degree).
pub(crate) fn merc_scale(lat_deg: f64, zoom_ppd: f32) -> f32 {
    let rad = lat_deg.to_radians().cos().max(0.001);
    zoom_ppd / rad as f32
}
