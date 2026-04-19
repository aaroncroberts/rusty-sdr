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
    Color32, FontId, Frame, Grid, Key, Margin, Painter, Pos2, Rect, Response,
    RichText, Rounding, Sense, Stroke, Vec2,
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

/// Width of the aircraft detail side panel.
const DETAIL_WIDTH: f32 = 210.0;

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

// ── Position trail storage ────────────────────────────────────────────────────

#[derive(Default)]
pub(crate) struct AircraftTrail {
    /// Circular buffer of (lat, lon) positions.
    pub(crate) points: Vec<(f64, f64)>,
    /// Age of each trail point (for fading).
    times: Vec<Instant>,
}

impl AircraftTrail {
    pub(crate) fn push(&mut self, lat: f64, lon: f64) {
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
    /// Set to true when the user clicks "Set Home" (📍); caller clears it and persists.
    pub set_home_pending: bool,
    /// Tracks whether the OS viewport window is open. Set to false when the OS window
    /// close button is pressed; caller resets to true when it re-opens the window.
    pub viewport_open: bool,
}

impl AdsbMapWindow {
    /// Create a new map centered over Cleveland OH.
    pub fn new() -> Self {
        Self::with_viewport(41.5, -81.7, 12.0)
    }

    /// Create a map with a specific initial viewport (restored from config).
    pub fn with_viewport(center_lat: f64, center_lon: f64, zoom_ppd: f32) -> Self {
        Self {
            center_lat,
            center_lon,
            zoom_ppd,
            selected_icao: None,
            trails: HashMap::new(),
            drag_start: None,
            set_home_pending: false,
            viewport_open: true,
        }
    }

    /// Current map center latitude (for config persistence).
    pub fn center_lat(&self) -> f64 { self.center_lat }
    /// Current map center longitude (for config persistence).
    pub fn center_lon(&self) -> f64 { self.center_lon }
    /// Current zoom in pixels-per-degree (for config persistence).
    pub fn zoom_ppd(&self) -> f32 { self.zoom_ppd }

    /// Show the ADS-B map window.  Returns the ICAO of any aircraft clicked.
    ///
    /// Renders directly as viewport content (no egui::Window wrapper).
    /// `open` is set to false when the OS viewport close button is pressed.
    ///
    /// `home_lat` / `home_lon` are the saved home coordinates (from config) used
    /// by the ⌖ reset button.  If the user clicks 📍 Set Home, `set_home_pending`
    /// is set to `true`; the caller should persist `center_lat()`/`center_lon()`
    /// back to config and clear the flag.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        open: &mut bool,
        aircraft: &[AircraftState],
        home_lat: f64,
        home_lon: f64,
    ) -> Option<u32> {
        // Handle OS window close button
        if ctx.input(|i| i.viewport().close_requested()) {
            *open = false;
        }

        // Aircraft positions update continuously — request a repaint every frame.
        // Without this, deferred viewports only repaint on OS events (mouse move, etc.)
        // which means the map would appear static between interactions.
        ctx.request_repaint();

        let mut clicked = None;

        egui::CentralPanel::default()
            .frame(
                Frame::none()
                    .fill(Color32::from_rgb(0x10, 0x14, 0x1A)),
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

                // Escape deselects
                if ui.input(|i| i.key_pressed(Key::Escape)) {
                    self.selected_icao = None;
                }

                // ── Toolbar ───────────────────────────────────────────────────
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!("  {} aircraft", aircraft.len()))
                            .color(Color32::from_rgb(0x8A, 0x9A, 0xB0))
                            .small(),
                    );
                    ui.separator();
                    if ui.small_button("+").on_hover_text("Zoom in").clicked() {
                        self.zoom_ppd = (self.zoom_ppd * 1.5).min(MAX_ZOOM);
                    }
                    if ui.small_button("-").on_hover_text("Zoom out").clicked() {
                        self.zoom_ppd = (self.zoom_ppd / 1.5).max(MIN_ZOOM);
                    }
                    if ui.small_button("Home").on_hover_text("Reset to home location").clicked() {
                        self.center_lat = home_lat;
                        self.center_lon = home_lon;
                        self.zoom_ppd = 12.0;
                    }
                    if ui.small_button("Pin").on_hover_text("Set current view as home").clicked() {
                        self.set_home_pending = true;
                    }
                    ui.separator();
                    // Altitude legend
                    ui.label(RichText::new("●").color(Color32::from_rgb(0x73, 0xC9, 0x91)).small());
                    ui.label(RichText::new("Low").color(Color32::from_rgb(0x8A, 0x9A, 0xB0)).small());
                    ui.label(RichText::new("●").color(Color32::from_rgb(0xE8, 0xC5, 0x4B)).small());
                    ui.label(RichText::new("Mid").color(Color32::from_rgb(0x8A, 0x9A, 0xB0)).small());
                    ui.label(RichText::new("●").color(Color32::from_rgb(0xFF, 0x55, 0x55)).small());
                    ui.label(RichText::new("High").color(Color32::from_rgb(0x8A, 0x9A, 0xB0)).small());
                });
                ui.separator();

                // ── Detail side panel (pre-clone to avoid borrow conflict) ───
                let selected_ac = self.selected_icao
                    .and_then(|icao| aircraft.iter().find(|a| a.icao == icao).cloned());

                let mut close_detail = false;
                if let Some(ref ac) = selected_ac {
                    egui::SidePanel::right("adsb_detail_panel")
                        .exact_width(DETAIL_WIDTH)
                        .resizable(false)
                        .frame(
                            Frame::default()
                                .fill(Color32::from_rgb(0x0D, 0x11, 0x1C))
                                .stroke(Stroke::new(1.0, Color32::from_rgb(0x22, 0x2A, 0x38)))
                                .inner_margin(Margin::same(10.0)),
                        )
                        .show_inside(ui, |ui| {
                            close_detail = show_aircraft_detail(ui, ac);
                        });
                }
                if close_detail {
                    self.selected_icao = None;
                }

                // ── Map canvas (takes all remaining space) ────────────────────
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

            // Click hit-test (12 px radius as per spec; ICON_R * 1.4 ≈ 12.6)
            if let Some(cp) = click_pos {
                if (cp - screen).length() < ICON_R * 1.4 {
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

// ── Aircraft detail panel ─────────────────────────────────────────────────────

/// Render the aircraft detail side panel.
/// Returns `true` if the user clicked the deselect button.
fn show_aircraft_detail(ui: &mut egui::Ui, ac: &AircraftState) -> bool {
    let muted = Color32::from_rgb(0x5A, 0x6A, 0x7A);
    let value_color = Color32::from_rgb(0xD8, 0xE8, 0xF0);
    let accent = Color32::from_rgb(0x4E, 0xC9, 0xE0);

    // ── ICAO address (click to copy) ─────────────────────────────────────────
    let icao_str = format!("{:06X}", ac.icao);
    let icao_resp = ui.add(
        egui::Label::new(
            RichText::new(&icao_str)
                .monospace()
                .size(18.0)
                .color(accent),
        )
        .sense(Sense::click()),
    );
    if icao_resp.clicked() {
        ui.ctx().copy_text(icao_str);
        // Visual feedback via tooltip — egui doesn't have a toast API here
    }
    icao_resp.on_hover_text("Click to copy ICAO address");

    // ── Callsign ─────────────────────────────────────────────────────────────
    let callsign = ac.callsign.as_deref().map(str::trim).unwrap_or("—");
    ui.label(RichText::new(callsign).size(15.0).color(Color32::WHITE));

    ui.add_space(6.0);
    ui.separator();
    ui.add_space(4.0);

    // ── Telemetry grid ───────────────────────────────────────────────────────
    Grid::new("adsb_detail_grid")
        .num_columns(2)
        .spacing([8.0, 4.0])
        .show(ui, |ui| {
            // Altitude
            ui.label(RichText::new("ALT").small().color(muted));
            let alt_text = ac.altitude_ft.map(format_altitude).unwrap_or_else(|| "—".into());
            let alt_color = if ac.altitude_ft.is_some() { value_color } else { muted };
            ui.label(RichText::new(alt_text).small().color(alt_color));
            ui.end_row();

            // Speed
            ui.label(RichText::new("SPD").small().color(muted));
            let spd_text = ac.speed_kt.map(format_speed).unwrap_or_else(|| "—".into());
            let spd_color = if ac.speed_kt.is_some() { value_color } else { muted };
            ui.label(RichText::new(spd_text).small().color(spd_color));
            ui.end_row();

            // Heading
            ui.label(RichText::new("HDG").small().color(muted));
            let hdg_text = ac.heading_deg
                .map(|h| format!("{h:.0}°  {}", heading_compass(h)))
                .unwrap_or_else(|| "—".into());
            let hdg_color = if ac.heading_deg.is_some() { value_color } else { muted };
            ui.label(RichText::new(hdg_text).small().color(hdg_color));
            ui.end_row();

            // Vertical rate
            ui.label(RichText::new("V/S").small().color(muted));
            if let Some(vr) = ac.vert_rate_fpm {
                let (arrow, vr_color) = vert_rate_display(vr, muted);
                ui.label(RichText::new(format!("{arrow} {vr:+} fpm")).small().color(vr_color));
            } else {
                ui.label(RichText::new("—").small().color(muted));
            }
            ui.end_row();

            // Position
            ui.label(RichText::new("POS").small().color(muted));
            let pos_text = ac.lat.zip(ac.lon)
                .map(|(lat, lon)| format_position(lat, lon))
                .unwrap_or_else(|| "—".into());
            let pos_color = if ac.lat.is_some() { value_color } else { muted };
            ui.label(RichText::new(pos_text).small().color(pos_color));
            ui.end_row();

            // Last seen
            ui.label(RichText::new("AGE").small().color(muted));
            let age = ac.last_seen.elapsed().as_secs_f32();
            let age_color = if age < 5.0 {
                Color32::from_rgb(0x73, 0xC9, 0x91)
            } else if age < 15.0 {
                value_color
            } else {
                Color32::from_rgb(0xE8, 0xC5, 0x4B)
            };
            ui.label(RichText::new(format_age(age)).small().color(age_color));
            ui.end_row();
        });

    ui.add_space(8.0);
    ui.separator();
    ui.add_space(4.0);

    // ── Deselect button ───────────────────────────────────────────────────────
    let close_clicked = ui
        .add(egui::Button::new(RichText::new("✕  Deselect").small().color(muted)).frame(false))
        .on_hover_text("Deselect aircraft (or press Escape)")
        .clicked();

    ui.add_space(2.0);
    ui.label(RichText::new("Click map to change selection").size(9.0).color(muted));

    close_clicked
}

// ── Detail panel pure formatters ──────────────────────────────────────────────

/// Format altitude as "38000 ft / 11582 m".
pub(crate) fn format_altitude(ft: i32) -> String {
    let m = (ft as f64 * 0.3048) as i32;
    format!("{ft} ft / {m} m")
}

/// Format speed as "450 kts / 834 km/h".
pub(crate) fn format_speed(kt: f32) -> String {
    let kmh = (kt * 1.852) as u32;
    format!("{kt:.0} kts / {kmh} km/h")
}

/// Format position as "41.499°N 81.694°W".
pub(crate) fn format_position(lat: f64, lon: f64) -> String {
    let lat_hem = if lat >= 0.0 { "N" } else { "S" };
    let lon_hem = if lon >= 0.0 { "E" } else { "W" };
    format!("{:.3}°{lat_hem} {:.3}°{lon_hem}", lat.abs(), lon.abs())
}

/// Arrow glyph and Color32 for a vertical rate in fpm.
/// Returns (arrow, color): ▲ green for climb, ▼ orange for descent, ━ muted for level.
pub(crate) fn vert_rate_display(fpm: i32, muted: Color32) -> (&'static str, Color32) {
    if fpm > 64 {
        ("▲", Color32::from_rgb(0x73, 0xC9, 0x91))
    } else if fpm < -64 {
        ("▼", Color32::from_rgb(0xFF, 0x88, 0x55))
    } else {
        ("━", muted)
    }
}

/// Format age as "12.3 s ago" (< 60 s) or "2 min ago" (≥ 60 s).
pub(crate) fn format_age(secs: f32) -> String {
    if secs < 60.0 {
        format!("{secs:.1} s ago")
    } else {
        format!("{:.0} min ago", secs / 60.0)
    }
}

/// Cardinal compass label for a heading in degrees.
pub(crate) fn heading_compass(deg: f32) -> &'static str {
    let idx = ((deg + 22.5) / 45.0) as usize % 8;
    ["N", "NE", "E", "SE", "S", "SW", "W", "NW"][idx]
}

// ── Aircraft icon ─────────────────────────────────────────────────────────────

/// Generate three vertices of an aircraft-shaped triangle centered at `pos`,
/// pointing in direction `heading_deg` (0 = North, clockwise).
pub(crate) fn aircraft_triangle(pos: Pos2, heading_deg: f32, r: f32) -> [Pos2; 3] {
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

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── heading_compass ───────────────────────────────────────────────────────

    #[test]
    fn heading_compass_cardinal_directions() {
        assert_eq!(heading_compass(0.0), "N");
        assert_eq!(heading_compass(90.0), "E");
        assert_eq!(heading_compass(180.0), "S");
        assert_eq!(heading_compass(270.0), "W");
    }

    #[test]
    fn heading_compass_intercardinal_directions() {
        assert_eq!(heading_compass(45.0), "NE");
        assert_eq!(heading_compass(135.0), "SE");
        assert_eq!(heading_compass(225.0), "SW");
        assert_eq!(heading_compass(315.0), "NW");
    }

    #[test]
    fn heading_compass_sector_boundaries() {
        // 22.5° is the midpoint of the N sector — still N
        assert_eq!(heading_compass(22.4), "N");
        // 22.5° crosses into NE
        assert_eq!(heading_compass(22.5), "NE");
        // 359° wraps back to N
        assert_eq!(heading_compass(359.0), "N");
    }

    // ── altitude_color ────────────────────────────────────────────────────────

    #[test]
    fn altitude_color_none_returns_ground_color() {
        let ground = altitude_color(Some(0));
        let none_color = altitude_color(None);
        assert_eq!(ground, none_color);
    }

    #[test]
    fn altitude_color_ground_is_green() {
        let c = altitude_color(Some(0));
        // Ground = Color32::from_rgb(0x73, 0xC9, 0x91)
        assert!(c.g() > c.r() && c.g() > c.b(), "ground should be greenish");
    }

    #[test]
    fn altitude_color_high_is_red() {
        let c = altitude_color(Some(45_000));
        // High alt = Color32::from_rgb(0xFF, 0x55, 0x55)
        assert!(c.r() > c.g() && c.r() > c.b(), "high altitude should be reddish");
    }

    #[test]
    fn altitude_color_negative_clamped_to_ground() {
        let ground = altitude_color(Some(0));
        let below = altitude_color(Some(-500));
        assert_eq!(ground, below, "negative altitude should clamp to ground color");
    }

    #[test]
    fn altitude_color_mid_is_yellowish() {
        // ~15k ft is the first breakpoint (green → yellow)
        let c = altitude_color(Some(15_000));
        // yellow = high R and G, low B
        assert!(c.r() > 150 && c.g() > 150 && c.b() < 100, "mid altitude should be yellowish");
    }

    // ── lerp_color ────────────────────────────────────────────────────────────

    #[test]
    fn lerp_color_t0_returns_a() {
        let a = Color32::from_rgb(255, 0, 0);
        let b = Color32::from_rgb(0, 255, 0);
        assert_eq!(lerp_color(a, b, 0.0), a);
    }

    #[test]
    fn lerp_color_t1_returns_b() {
        let a = Color32::from_rgb(255, 0, 0);
        let b = Color32::from_rgb(0, 255, 0);
        let result = lerp_color(a, b, 1.0);
        assert_eq!(result.r(), 0);
        assert_eq!(result.g(), 255);
    }

    #[test]
    fn lerp_color_midpoint() {
        let a = Color32::from_rgb(0, 0, 0);
        let b = Color32::from_rgb(100, 200, 50);
        let mid = lerp_color(a, b, 0.5);
        assert_eq!(mid.r(), 50);
        assert_eq!(mid.g(), 100);
        assert_eq!(mid.b(), 25);
    }

    // ── geo_to_screen ─────────────────────────────────────────────────────────

    fn test_rect() -> Rect {
        Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0))
    }

    #[test]
    fn geo_to_screen_center_maps_to_rect_center() {
        let rect = test_rect();
        let center = rect.center();
        let lat = 51.5_f64;
        let lon = 0.0_f64;
        let screen = geo_to_screen(rect, lat, lon, lat, lon, 8.0);
        assert!(
            (screen.x - center.x).abs() < 0.01,
            "center lat/lon should map to rect center x"
        );
        assert!(
            (screen.y - center.y).abs() < 0.01,
            "center lat/lon should map to rect center y"
        );
    }

    #[test]
    fn geo_to_screen_east_is_right_of_center() {
        let rect = test_rect();
        let lat = 0.0_f64;
        let center = geo_to_screen(rect, lat, 0.0, lat, 0.0, 8.0);
        let east = geo_to_screen(rect, lat, 1.0, lat, 0.0, 8.0);
        assert!(east.x > center.x, "east longitude should be right of center");
    }

    #[test]
    fn geo_to_screen_north_is_above_center() {
        let rect = test_rect();
        let lon = 0.0_f64;
        let center = geo_to_screen(rect, 0.0, lon, 0.0, lon, 8.0);
        let north = geo_to_screen(rect, 1.0, lon, 0.0, lon, 8.0);
        assert!(north.y < center.y, "north latitude should be above (smaller y) center");
    }

    #[test]
    fn geo_to_screen_longitude_offset_proportional_to_zoom() {
        let rect = test_rect();
        let lat = 0.0;
        let p1 = geo_to_screen(rect, lat, 1.0, lat, 0.0, 8.0);
        let p2 = geo_to_screen(rect, lat, 1.0, lat, 0.0, 16.0);
        let dx1 = p1.x - rect.center().x;
        let dx2 = p2.x - rect.center().x;
        assert!(
            (dx2 / dx1 - 2.0).abs() < 0.01,
            "doubling zoom_ppd should double x offset; got {dx2}/{dx1} = {}",
            dx2 / dx1
        );
    }

    // ── merc_scale ────────────────────────────────────────────────────────────

    #[test]
    fn merc_scale_equator_equals_zoom_ppd() {
        let zoom = 8.0_f32;
        let scale = merc_scale(0.0, zoom);
        assert!((scale - zoom).abs() < 0.01, "equator merc_scale should equal zoom_ppd");
    }

    #[test]
    fn merc_scale_increases_toward_poles() {
        // Scale = zoom / cos(lat); cos decreases as |lat| increases
        let zoom = 8.0;
        let s0 = merc_scale(0.0, zoom);
        let s30 = merc_scale(30.0, zoom);
        let s60 = merc_scale(60.0, zoom);
        assert!(s30 > s0, "scale should increase from equator to 30°");
        assert!(s60 > s30, "scale should increase from 30° to 60°");
    }

    // ── AdsbMapWindow viewport ────────────────────────────────────────────────

    #[test]
    fn with_viewport_stores_and_returns_correct_values() {
        let map = AdsbMapWindow::with_viewport(41.5, -81.7, 12.0);
        assert!((map.center_lat() - 41.5).abs() < 1e-9);
        assert!((map.center_lon() - -81.7).abs() < 1e-9);
        assert!((map.zoom_ppd() - 12.0).abs() < 1e-4);
    }

    #[test]
    fn new_defaults_to_cleveland_viewport() {
        let map = AdsbMapWindow::new();
        assert!((map.center_lat() - 41.5).abs() < 1e-9);
        assert!((map.center_lon() - -81.7).abs() < 1e-9);
        assert!((map.zoom_ppd() - 12.0).abs() < 1e-4);
    }

    // ── AircraftTrail::push ───────────────────────────────────────────────────

    #[test]
    fn trail_push_adds_distinct_points() {
        let mut trail = AircraftTrail::default();
        trail.push(51.0, 0.0);
        trail.push(52.0, 0.0); // 1° lat apart — distinct
        assert_eq!(trail.points.len(), 2);
    }

    #[test]
    fn trail_push_suppresses_duplicate_within_threshold() {
        let mut trail = AircraftTrail::default();
        trail.push(51.0, 0.0);
        trail.push(51.0005, 0.0005); // < 0.001° — suppressed
        assert_eq!(trail.points.len(), 1, "near-duplicate should be suppressed");
    }

    #[test]
    fn trail_push_allows_point_just_beyond_threshold() {
        let mut trail = AircraftTrail::default();
        trail.push(51.0, 0.0);
        trail.push(51.0011, 0.0); // > 0.001° lat — accepted
        assert_eq!(trail.points.len(), 2);
    }

    #[test]
    fn trail_push_caps_at_max_trail() {
        let mut trail = AircraftTrail::default();
        for i in 0..MAX_TRAIL + 10 {
            trail.push(i as f64, 0.0); // all distinct (1° apart)
        }
        assert_eq!(
            trail.points.len(),
            MAX_TRAIL,
            "trail should cap at MAX_TRAIL={MAX_TRAIL}"
        );
    }

    #[test]
    fn aircraft_triangle_nose_in_heading_direction() {
        let center = Pos2::new(400.0, 300.0);
        // North: nose should be above center (smaller y)
        let pts_n = aircraft_triangle(center, 0.0, 9.0);
        let nose_n = pts_n[0];
        assert!(nose_n.y < center.y, "nose should be above center for heading=0 (North)");

        // East: nose should be right of center (larger x)
        let pts_e = aircraft_triangle(center, 90.0, 9.0);
        let nose_e = pts_e[0];
        assert!(nose_e.x > center.x, "nose should be right of center for heading=90 (East)");
    }

    // ── Detail panel formatters ───────────────────────────────────────────────

    #[test]
    fn format_altitude_converts_to_meters() {
        assert_eq!(format_altitude(0), "0 ft / 0 m");
        assert_eq!(format_altitude(38000), "38000 ft / 11582 m");
        assert_eq!(format_altitude(-500), "-500 ft / -152 m");
    }

    #[test]
    fn format_speed_converts_kts_to_kmh() {
        assert_eq!(format_speed(0.0), "0 kts / 0 km/h");
        assert_eq!(format_speed(450.0), "450 kts / 833 km/h"); // 450 * 1.852 = 833.4 → 833
        // 1 knot = 1.852 km/h exactly
        assert_eq!(format_speed(1.0), "1 kts / 1 km/h");
    }

    #[test]
    fn format_position_hemispheres() {
        assert!(format_position(41.5, -81.7).contains("N"));
        assert!(format_position(41.5, -81.7).contains("W"));
        assert!(format_position(-33.9, 151.2).contains("S"));
        assert!(format_position(-33.9, 151.2).contains("E"));
    }

    #[test]
    fn format_position_three_decimal_places() {
        let s = format_position(51.499, 0.001);
        assert!(s.contains("51.499"), "should have 3dp lat: {s}");
        assert!(s.contains("0.001"), "should have 3dp lon: {s}");
    }

    #[test]
    fn vert_rate_display_climb_is_green_up_arrow() {
        let muted = Color32::from_rgb(0x5A, 0x6A, 0x7A);
        let (arrow, color) = vert_rate_display(500, muted);
        assert_eq!(arrow, "▲");
        assert!(color.g() > color.r(), "climb should be greenish");
    }

    #[test]
    fn vert_rate_display_descent_is_orange_down_arrow() {
        let muted = Color32::from_rgb(0x5A, 0x6A, 0x7A);
        let (arrow, color) = vert_rate_display(-500, muted);
        assert_eq!(arrow, "▼");
        assert!(color.r() > color.g(), "descent should be orange-ish");
    }

    #[test]
    fn vert_rate_display_level_is_muted() {
        let muted = Color32::from_rgb(0x5A, 0x6A, 0x7A);
        let (arrow, color) = vert_rate_display(0, muted);
        assert_eq!(arrow, "━");
        assert_eq!(color, muted);
    }

    #[test]
    fn vert_rate_display_threshold_boundary() {
        let muted = Color32::TRANSPARENT;
        // Exactly 64 fpm = level (not climb)
        assert_eq!(vert_rate_display(64, muted).0, "━");
        assert_eq!(vert_rate_display(65, muted).0, "▲");
        assert_eq!(vert_rate_display(-64, muted).0, "━");
        assert_eq!(vert_rate_display(-65, muted).0, "▼");
    }

    #[test]
    fn format_age_seconds() {
        assert_eq!(format_age(0.0), "0.0 s ago");
        assert_eq!(format_age(12.3), "12.3 s ago");
        assert_eq!(format_age(59.9), "59.9 s ago");
    }

    #[test]
    fn format_age_minutes() {
        assert_eq!(format_age(60.0), "1 min ago");
        assert_eq!(format_age(120.0), "2 min ago");
        assert_eq!(format_age(90.0), "2 min ago"); // rounds to nearest
    }

    #[test]
    fn aircraft_triangle_vertices_at_expected_distance() {
        let center = Pos2::new(0.0, 0.0);
        let r = 9.0_f32;
        let pts = aircraft_triangle(center, 0.0, r);
        // Nose at (0, -r*1.4)
        assert!((pts[0].y - (-r * 1.4)).abs() < 0.01);
        assert!(pts[0].x.abs() < 0.01);
    }
}
