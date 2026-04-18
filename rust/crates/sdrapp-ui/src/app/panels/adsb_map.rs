//! ADS-B aircraft map panel.
//!
//! Renders a Mercator-projected interactive map with live aircraft positions,
//! heading vectors, altitude-coded colors, and fade-out position trails.
//!
//! The panel is displayed as a `egui::Window`.  Call
//! [`AdsbMapWindow::show`] each frame when `open == true`.

use std::collections::HashMap;
use std::time::Instant;

use egui::{
    Color32, FontId, Painter, Pos2, Rect, Response, Rounding, Sense, Stroke, Vec2,
};
use sdrapp_adsb::state::AircraftState;

// ── Constants ─────────────────────────────────────────────────────────────────

/// Maximum trail points kept per aircraft.
const MAX_TRAIL: usize = 120;

/// Minimum pixels-per-degree to clamp zoom (shows the whole world).
const MIN_ZOOM: f32 = 1.2;
/// Maximum pixels per degree (roughly street-level).
const MAX_ZOOM: f32 = 800.0;

/// Size of the aircraft icon (radius in pixels).
const ICON_R: f32 = 9.0;

// ── Altitude → color gradient ──────────────────────────────────────────────

fn altitude_color(alt_ft: Option<i32>) -> Color32 {
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

fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
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
fn geo_to_screen(
    rect: Rect,
    lat: f64,
    lon: f64,
    center_lat: f64,
    center_lon: f64,
    zoom_ppd: f32,
) -> Pos2 {
    let dx = (lon - center_lon) as f32 * zoom_ppd;

    // Mercator Y: use lat-to-radians for proper scaling
    let merc = |deg: f64| {
        let rad = deg.to_radians();
        (rad.sin().asin().tan() + 1.0 / rad.cos()).ln() as f32
    };
    // dy: positive screen Y = south, so negate
    let dy = -(merc(lat) - merc(center_lat)) * (zoom_ppd * std::f32::consts::FRAC_1_PI * 180.0);

    rect.center() + Vec2::new(dx, dy)
}

/// Mercator Y scale factor at a given latitude (pixels per degree).
fn merc_scale(lat_deg: f64, zoom_ppd: f32) -> f32 {
    let rad = lat_deg.to_radians().cos().max(0.001);
    zoom_ppd / rad as f32
}

// ── Position trail storage ────────────────────────────────────────────────────

#[derive(Default)]
struct AircraftTrail {
    /// Circular buffer of (lat, lon) positions.
    points: Vec<(f64, f64)>,
    /// Age of each trail point (for fading).
    times: Vec<Instant>,
}

impl AircraftTrail {
    fn push(&mut self, lat: f64, lon: f64) {
        // Avoid duplicates (only store if moved ≥ ~100 m)
        if let Some(&(last_lat, last_lon)) = self.points.last() {
            let dlat = (lat - last_lat).abs();
            let dlon = (lon - last_lon).abs();
            if dlat < 0.001 && dlon < 0.001 {
                return;
            }
        }
        if self.points.len() >= MAX_TRAIL {
            self.points.remove(0);
            self.times.remove(0);
        }
        self.points.push((lat, lon));
        self.times.push(Instant::now());
    }
}

// ── Main widget ───────────────────────────────────────────────────────────────

/// ADS-B map floating window.
pub struct AdsbMapWindow {
    /// Map center (degrees).
    center_lat: f64,
    center_lon: f64,
    /// Pixels per degree of longitude.
    zoom_ppd: f32,
    /// Currently selected aircraft ICAO (highlighted).
    pub selected_icao: Option<u32>,
    /// Position trails per ICAO.
    trails: HashMap<u32, AircraftTrail>,
    /// Drag start state: (screen position at drag start, center_lat, center_lon).
    drag_start: Option<(Pos2, f64, f64)>,
}

impl AdsbMapWindow {
    /// Create a new map centered over Europe (suitable for most SDR setups).
    pub fn new() -> Self {
        Self {
            center_lat: 51.5,
            center_lon: 0.0,
            zoom_ppd: 8.0,
            selected_icao: None,
            trails: HashMap::new(),
            drag_start: None,
        }
    }

    /// Show the ADS-B map window.  Returns the ICAO of any aircraft clicked.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        open: &mut bool,
        aircraft: &[AircraftState],
    ) -> Option<u32> {
        let mut clicked = None;

        egui::Window::new("ADS-B Aircraft Map")
            .open(open)
            .default_size([800.0, 560.0])
            .resizable(true)
            .collapsible(false)
            .frame(
                egui::Frame::default()
                    .fill(Color32::from_rgb(0x10, 0x14, 0x1A))
                    .stroke(Stroke::new(1.0, Color32::from_rgb(0x2A, 0x30, 0x3A))),
            )
            .show(ctx, |ui| {
                // Update trails for aircraft that have positions.
                for ac in aircraft {
                    if let (Some(lat), Some(lon)) = (ac.lat, ac.lon) {
                        self.trails.entry(ac.icao).or_default().push(lat, lon);
                    }
                }
                // Evict trails for aircraft no longer in the list.
                let active: std::collections::HashSet<u32> =
                    aircraft.iter().map(|a| a.icao).collect();
                self.trails.retain(|icao, _| active.contains(icao));

                // ── Toolbar ───────────────────────────────────────────────────
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(format!("  {} aircraft", aircraft.len()))
                            .color(Color32::from_rgb(0x8A, 0x9A, 0xB0))
                            .small(),
                    );
                    ui.separator();
                    if ui.small_button("⊕").on_hover_text("Zoom in").clicked() {
                        self.zoom_ppd = (self.zoom_ppd * 1.5).min(MAX_ZOOM);
                    }
                    if ui.small_button("⊖").on_hover_text("Zoom out").clicked() {
                        self.zoom_ppd = (self.zoom_ppd / 1.5).max(MIN_ZOOM);
                    }
                    if ui.small_button("⌖").on_hover_text("Reset view").clicked() {
                        self.center_lat = 51.5;
                        self.center_lon = 0.0;
                        self.zoom_ppd = 8.0;
                    }
                    ui.separator();
                    // Altitude legend
                    ui.label(
                        egui::RichText::new("●")
                            .color(Color32::from_rgb(0x73, 0xC9, 0x91))
                            .small(),
                    );
                    ui.label(egui::RichText::new("Low").color(Color32::from_rgb(0x8A, 0x9A, 0xB0)).small());
                    ui.label(
                        egui::RichText::new("●")
                            .color(Color32::from_rgb(0xE8, 0xC5, 0x4B))
                            .small(),
                    );
                    ui.label(egui::RichText::new("Mid").color(Color32::from_rgb(0x8A, 0x9A, 0xB0)).small());
                    ui.label(
                        egui::RichText::new("●")
                            .color(Color32::from_rgb(0xFF, 0x55, 0x55))
                            .small(),
                    );
                    ui.label(egui::RichText::new("High").color(Color32::from_rgb(0x8A, 0x9A, 0xB0)).small());
                });
                ui.separator();

                // ── Map canvas ────────────────────────────────────────────────
                let available = ui.available_size();
                let (response, painter) = ui.allocate_painter(available, Sense::click_and_drag());
                let rect = response.rect;

                clicked = self.render_map(
                    &painter,
                    rect,
                    &response,
                    aircraft,
                );
            });

        clicked
    }

    /// Render the map background, graticule, aircraft, trails.
    /// Returns selected ICAO if clicked.
    fn render_map(
        &mut self,
        painter: &Painter,
        rect: Rect,
        response: &Response,
        aircraft: &[AircraftState],
    ) -> Option<u32> {
        let mut clicked_icao = None;

        // ── Background ────────────────────────────────────────────────────────
        painter.rect_filled(rect, Rounding::ZERO, Color32::from_rgb(0x0D, 0x11, 0x17));

        // ── Drag to pan ───────────────────────────────────────────────────────
        if response.drag_started() {
            if let Some(pos) = response.interact_pointer_pos() {
                self.drag_start = Some((pos, self.center_lat, self.center_lon));
            }
        }
        if response.dragged() {
            if let (Some((start, lat0, lon0)), Some(pos)) =
                (self.drag_start, response.interact_pointer_pos())
            {
                let dx = pos.x - start.x;
                let dy = pos.y - start.y;
                self.center_lon = lon0 - dx as f64 / self.zoom_ppd as f64;
                let scale = merc_scale(self.center_lat, self.zoom_ppd);
                self.center_lat = (lat0 + dy as f64 / scale as f64).clamp(-85.0, 85.0);
            }
        }
        if response.drag_stopped() {
            self.drag_start = None;
        }

        // ── Scroll to zoom ────────────────────────────────────────────────────
        let scroll = response.ctx.input(|i| i.smooth_scroll_delta.y);
        if rect.contains(response.ctx.input(|i| i.pointer.hover_pos().unwrap_or_default()))
            && scroll != 0.0
        {
            let factor = 1.0 + scroll * 0.003;
            self.zoom_ppd = (self.zoom_ppd * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        }

        // ── Graticule ─────────────────────────────────────────────────────────
        self.draw_graticule(painter, rect);

        // ── Trails ───────────────────────────────────────────────────────────
        for ac in aircraft {
            if let Some(trail) = self.trails.get(&ac.icao) {
                self.draw_trail(painter, rect, trail, ac);
            }
        }

        // ── Aircraft icons ────────────────────────────────────────────────────
        let click_pos = if response.clicked() {
            response.interact_pointer_pos()
        } else {
            None
        };

        for ac in aircraft {
            let (Some(lat), Some(lon)) = (ac.lat, ac.lon) else { continue };
            let screen =
                geo_to_screen(rect, lat, lon, self.center_lat, self.center_lon, self.zoom_ppd);

            if !rect.expand(ICON_R * 2.0).contains(screen) {
                continue; // off-screen
            }

            let color = altitude_color(ac.altitude_ft);
            let is_selected = self.selected_icao == Some(ac.icao);

            // Selection ring
            if is_selected {
                painter.circle_stroke(
                    screen,
                    ICON_R + 4.0,
                    Stroke::new(1.5, Color32::from_rgb(0x4E, 0xC9, 0xE0)),
                );
            }

            // Aircraft triangle
            let heading = ac.heading_deg.unwrap_or(0.0);
            let pts = aircraft_triangle(screen, heading, ICON_R);
            painter.add(egui::Shape::convex_polygon(
                pts.to_vec(),
                color,
                Stroke::new(0.8, color.linear_multiply(1.4)),
            ));

            // Heading vector
            if let Some(hdg) = ac.heading_deg {
                let hdg_rad = (hdg as f64).to_radians();
                let tip = screen
                    + Vec2::new(
                        (hdg_rad.sin() * 20.0) as f32,
                        (-hdg_rad.cos() * 20.0) as f32,
                    );
                painter.line_segment(
                    [screen, tip],
                    Stroke::new(1.0, color.linear_multiply(0.7)),
                );
            }

            // Label (callsign / ICAO hex)
            let label = ac
                .callsign
                .as_deref()
                .unwrap_or("")
                .trim()
                .to_string();
            let label = if label.is_empty() {
                format!("{:06X}", ac.icao)
            } else {
                label
            };
            painter.text(
                screen + Vec2::new(ICON_R + 3.0, -6.0),
                egui::Align2::LEFT_CENTER,
                &label,
                FontId::proportional(10.0),
                Color32::from_rgb(0xC8, 0xD8, 0xE8),
            );

            // Altitude label
            if let Some(alt) = ac.altitude_ft {
                painter.text(
                    screen + Vec2::new(ICON_R + 3.0, 5.0),
                    egui::Align2::LEFT_CENTER,
                    format!("{}ft", alt / 100 * 100),
                    FontId::proportional(9.0),
                    Color32::from_rgb(0x6A, 0x8A, 0xA0),
                );
            }

            // Click hit-test
            if let Some(cp) = click_pos {
                if (cp - screen).length() < ICON_R * 2.0 {
                    self.selected_icao = Some(ac.icao);
                    clicked_icao = Some(ac.icao);
                }
            }
        }

        // Deselect on click outside all aircraft
        if let Some(cp) = click_pos {
            if clicked_icao.is_none() && rect.contains(cp) {
                self.selected_icao = None;
            }
        }

        // ── Border ────────────────────────────────────────────────────────────
        painter.rect_stroke(
            rect,
            Rounding::ZERO,
            Stroke::new(1.0, Color32::from_rgb(0x2A, 0x30, 0x3A)),
        );

        clicked_icao
    }

    // ── Graticule ─────────────────────────────────────────────────────────────

    fn draw_graticule(&self, painter: &Painter, rect: Rect) {
        let grid_color = Color32::from_rgba_premultiplied(0x1E, 0x26, 0x32, 0xC0);
        let label_color = Color32::from_rgb(0x3A, 0x4A, 0x5A);

        // Determine appropriate grid spacing based on zoom
        let grid_deg: f64 = if self.zoom_ppd > 100.0 {
            1.0
        } else if self.zoom_ppd > 20.0 {
            5.0
        } else {
            10.0
        };

        // Latitude lines
        let lat_start = ((-90.0_f64).max(
            self.center_lat - rect.height() as f64 / merc_scale(self.center_lat, self.zoom_ppd) as f64 * 1.5,
        ) / grid_deg)
            .floor() as i32;
        let lat_end = (90.0_f64.min(
            self.center_lat + rect.height() as f64 / merc_scale(self.center_lat, self.zoom_ppd) as f64 * 1.5,
        ) / grid_deg)
            .ceil() as i32;

        for lat_i in lat_start..=lat_end {
            let lat = lat_i as f64 * grid_deg;
            if lat.abs() > 85.0 { continue; }
            let y = geo_to_screen(rect, lat, self.center_lon, self.center_lat, self.center_lon, self.zoom_ppd).y;
            if y < rect.top() || y > rect.bottom() { continue; }
            painter.line_segment(
                [Pos2::new(rect.left(), y), Pos2::new(rect.right(), y)],
                Stroke::new(0.5, grid_color),
            );
            if lat != 0.0 {
                painter.text(
                    Pos2::new(rect.left() + 4.0, y - 2.0),
                    egui::Align2::LEFT_BOTTOM,
                    format!("{lat}°"),
                    FontId::proportional(9.0),
                    label_color,
                );
            }
        }

        // Equator highlight
        let eq_y = geo_to_screen(rect, 0.0, self.center_lon, self.center_lat, self.center_lon, self.zoom_ppd).y;
        if eq_y >= rect.top() && eq_y <= rect.bottom() {
            painter.line_segment(
                [Pos2::new(rect.left(), eq_y), Pos2::new(rect.right(), eq_y)],
                Stroke::new(1.0, Color32::from_rgba_premultiplied(0x2A, 0x3A, 0x4A, 0xFF)),
            );
        }

        // Longitude lines
        let lon_start = ((self.center_lon - rect.width() as f64 / self.zoom_ppd as f64 * 0.6)
            / grid_deg)
            .floor() as i32;
        let lon_end = ((self.center_lon + rect.width() as f64 / self.zoom_ppd as f64 * 0.6)
            / grid_deg)
            .ceil() as i32;

        for lon_i in lon_start..=lon_end {
            let lon = (lon_i as f64 * grid_deg).clamp(-180.0, 180.0);
            let x = geo_to_screen(rect, self.center_lat, lon, self.center_lat, self.center_lon, self.zoom_ppd).x;
            if x < rect.left() || x > rect.right() { continue; }
            painter.line_segment(
                [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
                Stroke::new(0.5, grid_color),
            );
            painter.text(
                Pos2::new(x + 2.0, rect.bottom() - 4.0),
                egui::Align2::LEFT_BOTTOM,
                format!("{lon}°"),
                FontId::proportional(9.0),
                label_color,
            );
        }

        // Prime meridian highlight
        let pm_x = geo_to_screen(rect, self.center_lat, 0.0, self.center_lat, self.center_lon, self.zoom_ppd).x;
        if pm_x >= rect.left() && pm_x <= rect.right() {
            painter.line_segment(
                [Pos2::new(pm_x, rect.top()), Pos2::new(pm_x, rect.bottom())],
                Stroke::new(1.0, Color32::from_rgba_premultiplied(0x2A, 0x3A, 0x4A, 0xFF)),
            );
        }
    }

    // ── Trails ────────────────────────────────────────────────────────────────

    fn draw_trail(&self, painter: &Painter, rect: Rect, trail: &AircraftTrail, ac: &AircraftState) {
        if trail.points.len() < 2 { return; }

        let base_color = altitude_color(ac.altitude_ft);
        let now = Instant::now();

        for i in 0..trail.points.len().saturating_sub(1) {
            let p0 = trail.points[i];
            let p1 = trail.points[i + 1];
            let t0 = trail.times[i];

            let age_secs = now.duration_since(t0).as_secs_f32();
            let alpha = (1.0 - age_secs / 120.0).clamp(0.0, 0.6); // fade over 2 min
            if alpha < 0.02 { continue; }

            let c = base_color.linear_multiply(alpha);
            let s0 = geo_to_screen(rect, p0.0, p0.1, self.center_lat, self.center_lon, self.zoom_ppd);
            let s1 = geo_to_screen(rect, p1.0, p1.1, self.center_lat, self.center_lon, self.zoom_ppd);

            if rect.expand(4.0).contains(s0) || rect.expand(4.0).contains(s1) {
                painter.line_segment([s0, s1], Stroke::new(1.2, c));
            }
        }
    }
}

impl Default for AdsbMapWindow {
    fn default() -> Self {
        Self::new()
    }
}

// ── Aircraft icon ─────────────────────────────────────────────────────────────

/// Generate three vertices of an aircraft-shaped triangle centered at `pos`,
/// pointing in direction `heading_deg` (0 = North, clockwise).
fn aircraft_triangle(pos: Pos2, heading_deg: f32, r: f32) -> [Pos2; 3] {
    let a = heading_deg.to_radians();
    let (sin_a, cos_a) = (a.sin(), a.cos());

    // Local-space points: nose up, tail split
    let local = [
        (0.0f32, -r * 1.4),             // nose
        (-r * 0.7, r * 0.8),            // left wing-tip
        (r * 0.7, r * 0.8),             // right wing-tip
    ];

    local.map(|(x, y)| {
        let rx = x * cos_a - y * sin_a;
        let ry = x * sin_a + y * cos_a;
        pos + Vec2::new(rx, ry)
    })
}

