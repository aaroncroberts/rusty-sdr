//! Orbcomm satellite map panel.
//!
//! Renders a Mercator-projected map showing:
//! - Current ground-track position for each Orbcomm OG2 satellite.
//! - A ±90-minute orbital arc (ground track) for the selected satellite.
//! - A circular coverage footprint (~2000 km radius) at the sub-satellite point.
//! - Home location marker.
//! - Pass schedule sidebar: next 3 passes for the selected satellite.
//!
//! The panel is displayed as a `egui::Window` via `show_viewport_deferred`.
//! TLEs are fetched once in a background thread (Celestrak, 24-hour cache).

use std::collections::HashSet;
use std::io::Read as _;
use std::time::{Duration, Instant, SystemTime};

use crossbeam_channel::{Receiver, Sender};
use egui::{
    Align2, Color32, ColorImage, FontId, Frame, Painter, Pos2, Rect, RichText, Rounding, Sense,
    Stroke, TextureHandle, TextureOptions, Vec2,
};
use sdrapp_tle::{
    parser::TleEntry,
    predictor::{PassEvent, PassPredictor},
    propagator::SatPosition,
};

// ── Constants ──────────────────────────────────────────────────────────────────

const MIN_ZOOM: f32 = 1.2;
const MAX_ZOOM: f32 = 800.0;
const PANEL_WIDTH: f32 = 200.0;

/// Half the orbital arc drawn on the map (each direction from now).
const TRACK_HALF_DURATION: Duration = Duration::from_secs(90 * 60);
/// Ground-track step: a point every 30 s gives smooth curves.
const TRACK_STEP: Duration = Duration::from_secs(30);
/// Coverage footprint radius for Orbcomm OG2 (~750 km altitude, 5° min el).
const FOOTPRINT_KM: f64 = 2_200.0;
/// Earth radius for footprint circle projection.
const EARTH_R_KM: f64 = 6_371.0;
/// Recompute ground tracks / passes no more often than this.
const TRACK_RECOMPUTE_INTERVAL: Duration = Duration::from_secs(60);

// ── OSM tile types (identical to adsb_map) ────────────────────────────────────

type TileKey = (u8, i32, i32);
struct TileFetchResult {
    key: TileKey,
    image: Option<ColorImage>,
}

fn osm_zoom(zoom_ppd: f32) -> u8 {
    let z = (zoom_ppd * 360.0 / 256.0).log2().round() as i32;
    z.clamp(0, 12) as u8
}

fn tile_nw(z: u8, x: i32, y: i32) -> (f64, f64) {
    let n = 2.0f64.powi(z as i32);
    let lon = x as f64 / n * 360.0 - 180.0;
    let lat = (std::f64::consts::PI * (1.0 - 2.0 * y as f64 / n)).sinh().atan().to_degrees();
    (lat, lon)
}

fn lat_lon_to_tile_xy(lat: f64, lon: f64, z: u8) -> (i32, i32) {
    let n = 2.0f64.powi(z as i32);
    let x = ((lon + 180.0) / 360.0 * n).floor() as i32;
    let lat_rad = lat.clamp(-85.05, 85.05).to_radians();
    let y = ((1.0 - (lat_rad.tan() + 1.0 / lat_rad.cos()).ln() / std::f64::consts::PI) / 2.0 * n)
        .floor() as i32;
    (x, y)
}

fn fetch_tile_async(tx: Sender<TileFetchResult>, z: u8, x: i32, y: i32) {
    std::thread::spawn(move || {
        let url = format!("https://tile.openstreetmap.org/{z}/{x}/{y}.png");
        let image = (|| -> Option<ColorImage> {
            let resp = ureq::get(&url)
                .set("User-Agent", "sdrapp satellite map/1.0 (desktop SDR application)")
                .call()
                .ok()?;
            let mut bytes = Vec::with_capacity(32_768);
            resp.into_reader().read_to_end(&mut bytes).ok()?;
            let img = image::load_from_memory(&bytes).ok()?;
            let rgba = img.to_rgba8();
            let (w, h) = rgba.dimensions();
            let pixels = rgba
                .pixels()
                .map(|p| Color32::from_rgba_unmultiplied(p[0], p[1], p[2], p[3]))
                .collect();
            Some(ColorImage { size: [w as usize, h as usize], pixels })
        })();
        let _ = tx.send(TileFetchResult { key: (z, x, y), image });
    });
}

// ── Mercator helpers (identical to adsb_map) ──────────────────────────────────

fn geo_to_screen(
    rect: Rect,
    lat: f64,
    lon: f64,
    center_lat: f64,
    center_lon: f64,
    zoom_ppd: f32,
) -> Pos2 {
    let dx = (lon - center_lon) as f32 * zoom_ppd;
    let merc = |deg: f64| {
        let rad = deg.to_radians();
        (rad.tan() + 1.0 / rad.cos()).ln() as f32
    };
    let dy = -(merc(lat) - merc(center_lat)) * (zoom_ppd * std::f32::consts::FRAC_1_PI * 180.0);
    rect.center() + Vec2::new(dx, dy)
}

// ── TLE fetch ─────────────────────────────────────────────────────────────────

struct TleFetchResult(Vec<TleEntry>);

fn fetch_tles_async(tx: Sender<TleFetchResult>) {
    std::thread::spawn(move || {
        let tles = sdrapp_tle::cache::fetch_orbcomm_tles();
        let _ = tx.send(TleFetchResult(tles));
    });
}

// ── Per-satellite cached data ─────────────────────────────────────────────────

struct SatData {
    /// Current position (updated each frame from SGP4).
    pos: Option<SatPosition>,
    /// Ground track points: (past_track, future_track) in (lat, lon) pairs.
    track: Vec<(f64, f64)>,
    /// Next 3 passes.
    passes: Vec<PassEvent>,
    /// When track/passes were last computed.
    last_track_update: Option<Instant>,
}

impl SatData {
    fn new() -> Self {
        Self { pos: None, track: Vec::new(), passes: Vec::new(), last_track_update: None }
    }
}

// ── Main widget ───────────────────────────────────────────────────────────────

/// Satellite map floating window.
pub struct SatMapWindow {
    // ── Map state ─────────────────────────────────────────────────────────────
    center_lat: f64,
    center_lon: f64,
    zoom_ppd: f32,
    drag_start: Option<(Pos2, f64, f64)>,

    // ── Selection ─────────────────────────────────────────────────────────────
    /// Index into `tles` of the currently selected satellite.
    selected_idx: Option<usize>,

    // ── OSM tiles ─────────────────────────────────────────────────────────────
    tile_cache: std::collections::HashMap<TileKey, TextureHandle>,
    pending_tiles: HashSet<TileKey>,
    tile_tx: Sender<TileFetchResult>,
    tile_rx: Receiver<TileFetchResult>,
    last_tile_z: u8,
    evicted_tiles: Vec<TextureHandle>,

    // ── TLE / propagation data ────────────────────────────────────────────────
    tles: Vec<TleEntry>,
    sat_data: Vec<SatData>,
    tle_tx: Sender<TleFetchResult>,
    tle_rx: Receiver<TleFetchResult>,
    tle_fetch_started: bool,

    // ── Public state written by main app ─────────────────────────────────────
    /// Set to false when OS viewport close button is pressed.
    pub viewport_open: bool,
    /// NORAD IDs of satellites that decoded a frame this frame (flash effect).
    pub flash_norad_ids: Vec<u32>,
}

impl SatMapWindow {
    pub fn new(center_lat: f64, center_lon: f64) -> Self {
        let (tile_tx, tile_rx) = crossbeam_channel::unbounded();
        let (tle_tx, tle_rx) = crossbeam_channel::unbounded();
        Self {
            center_lat,
            center_lon,
            zoom_ppd: 4.0,
            drag_start: None,
            selected_idx: None,
            tile_cache: Default::default(),
            pending_tiles: Default::default(),
            tile_tx,
            tile_rx,
            last_tile_z: 255,
            evicted_tiles: Vec::new(),
            tles: Vec::new(),
            sat_data: Vec::new(),
            tle_tx,
            tle_rx,
            tle_fetch_started: false,
            viewport_open: true,
            flash_norad_ids: Vec::new(),
        }
    }

    /// Show the satellite map window. Call each frame from the deferred viewport.
    pub fn show(&mut self, ctx: &egui::Context, open: &mut bool, home_lat: f64, home_lon: f64) {
        if ctx.input(|i| i.viewport().close_requested()) {
            *open = false;
        }
        ctx.request_repaint_after(Duration::from_millis(500));

        // ── Drain TLE results ─────────────────────────────────────────────────
        if let Ok(result) = self.tle_rx.try_recv() {
            self.tles = result.0;
            self.sat_data = self.tles.iter().map(|_| SatData::new()).collect();
            if self.selected_idx.is_none() && !self.tles.is_empty() {
                self.selected_idx = Some(0);
            }
        }

        // Start TLE fetch on first show.
        if !self.tle_fetch_started {
            self.tle_fetch_started = true;
            fetch_tles_async(self.tle_tx.clone());
        }

        // ── Update satellite positions ────────────────────────────────────────
        let now = SystemTime::now();
        for (i, tle) in self.tles.iter().enumerate() {
            let sd = &mut self.sat_data[i];
            sd.pos = sdrapp_tle::propagator::position_at(tle, now, home_lat, home_lon);
        }

        // ── Recompute tracks + passes for selected satellite ──────────────────
        if let Some(idx) = self.selected_idx {
            if let Some(sd) = self.sat_data.get_mut(idx) {
                let needs_update = sd.last_track_update
                    .map(|t| t.elapsed() > TRACK_RECOMPUTE_INTERVAL)
                    .unwrap_or(true);
                if needs_update {
                    if let Some(tle) = self.tles.get(idx) {
                        let predictor = PassPredictor::new(tle.clone(), home_lat, home_lon);
                        let t_start = now - TRACK_HALF_DURATION;
                        sd.track = predictor.ground_track(t_start, TRACK_HALF_DURATION * 2, TRACK_STEP);
                        sd.passes = predictor.predict(now, Duration::from_secs(24 * 3600), 3);
                        sd.last_track_update = Some(Instant::now());
                    }
                }
            }
        }

        egui::CentralPanel::default()
            .frame(
                Frame::none()
                    .fill(Color32::from_rgb(0x10, 0x14, 0x1A))
                    .inner_margin(egui::Margin::ZERO),
            )
            .show(ctx, |ui| {
                let avail = ui.available_rect_before_wrap();
                let map_rect = Rect::from_min_max(
                    avail.min,
                    Pos2::new(avail.max.x - PANEL_WIDTH, avail.max.y),
                );
                let panel_rect = Rect::from_min_max(
                    Pos2::new(avail.max.x - PANEL_WIDTH, avail.min.y),
                    avail.max,
                );

                self.draw_map(ui, map_rect, home_lat, home_lon);
                self.draw_sidebar(ui, panel_rect);
            });
    }

    // ── Map rendering ─────────────────────────────────────────────────────────

    fn draw_map(
        &mut self,
        ui: &mut egui::Ui,
        map_rect: Rect,
        home_lat: f64,
        home_lon: f64,
    ) {
        let response = ui.allocate_rect(map_rect, Sense::click_and_drag());
        let painter = ui.painter_at(map_rect);

        // ── Pan interaction ───────────────────────────────────────────────────
        if response.drag_started() {
            self.drag_start = Some((response.interact_pointer_pos().unwrap_or_default(),
                                    self.center_lat, self.center_lon));
        }
        if let Some((start_pos, start_lat, start_lon)) = self.drag_start {
            if let Some(cur) = response.interact_pointer_pos() {
                let dlat = ((cur.y - start_pos.y) / self.zoom_ppd) as f64;
                let dlon = ((cur.x - start_pos.x) / self.zoom_ppd) as f64;
                self.center_lat = (start_lat + dlat).clamp(-85.0, 85.0);
                self.center_lon = start_lon - dlon;
            }
        }
        if response.drag_stopped() {
            self.drag_start = None;
        }

        // ── Scroll-wheel zoom ─────────────────────────────────────────────────
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if map_rect.contains(ui.input(|i| i.pointer.hover_pos().unwrap_or_default())) && scroll != 0.0 {
            self.zoom_ppd = (self.zoom_ppd * (1.0 + scroll * 0.002)).clamp(MIN_ZOOM, MAX_ZOOM);
        }

        // ── Draw OSM tiles ────────────────────────────────────────────────────
        self.draw_tiles(&painter, map_rect, ui.ctx());

        // ── Draw ground track ─────────────────────────────────────────────────
        if let Some(idx) = self.selected_idx {
            if let Some(sd) = self.sat_data.get(idx) {
                let pts: Vec<Pos2> = sd.track.iter()
                    .map(|&(lat, lon)| geo_to_screen(map_rect, lat, lon, self.center_lat, self.center_lon, self.zoom_ppd))
                    .filter(|p| map_rect.contains(*p))
                    .collect();

                // Draw track as segmented line — break on large jumps (antimeridian wrap).
                self.draw_track_segments(&painter, &sd.track, map_rect);

                // Coverage footprint circle for selected satellite.
                if let Some(pos) = &sd.pos {
                    self.draw_footprint(&painter, pos, map_rect);
                }
                let _ = pts;
            }
        }

        // ── Draw all satellite dots ───────────────────────────────────────────
        for (i, (tle, sd)) in self.tles.iter().zip(self.sat_data.iter()).enumerate() {
            let Some(ref pos) = sd.pos else { continue };
            let screen = geo_to_screen(
                map_rect, pos.lat_deg, pos.lon_deg,
                self.center_lat, self.center_lon, self.zoom_ppd,
            );
            if !map_rect.contains(screen) { continue }

            let is_selected = self.selected_idx == Some(i);
            let is_flashing = self.flash_norad_ids.contains(&tle.norad_id);

            let color = if is_flashing {
                Color32::from_rgb(0xFF, 0xFF, 0x00) // yellow flash on decode
            } else if is_selected {
                Color32::from_rgb(0x4C, 0xAF, 0xFF)
            } else {
                Color32::from_rgb(0x88, 0xCC, 0x88)
            };

            painter.circle_filled(screen, if is_selected { 7.0 } else { 4.0 }, color);
            painter.circle_stroke(screen, if is_selected { 7.0 } else { 4.0 },
                Stroke::new(1.0, Color32::BLACK.gamma_multiply(0.5)));

            if is_selected {
                let label = format!("{}\n{:.1}° el", tle.name.trim(), pos.el_deg);
                painter.text(
                    screen + Vec2::new(9.0, -8.0),
                    Align2::LEFT_TOP,
                    label,
                    FontId::proportional(11.0),
                    color,
                );
            }

            // Click to select
            if response.clicked() {
                if let Some(click_pos) = response.interact_pointer_pos() {
                    if click_pos.distance(screen) < 12.0 {
                        self.selected_idx = Some(i);
                    }
                }
            }
        }
        self.flash_norad_ids.clear();

        // ── Home marker ───────────────────────────────────────────────────────
        let home_screen = geo_to_screen(
            map_rect, home_lat, home_lon,
            self.center_lat, self.center_lon, self.zoom_ppd,
        );
        if map_rect.contains(home_screen) {
            painter.circle_stroke(home_screen, 5.0, Stroke::new(2.0, Color32::from_rgb(0xFF, 0x99, 0x22)));
            painter.line_segment(
                [home_screen + Vec2::new(-5.0, 0.0), home_screen + Vec2::new(5.0, 0.0)],
                Stroke::new(1.5, Color32::from_rgb(0xFF, 0x99, 0x22)),
            );
            painter.line_segment(
                [home_screen + Vec2::new(0.0, -5.0), home_screen + Vec2::new(0.0, 5.0)],
                Stroke::new(1.5, Color32::from_rgb(0xFF, 0x99, 0x22)),
            );
        }

        // ── Loading overlay ───────────────────────────────────────────────────
        if self.tles.is_empty() && self.tle_fetch_started {
            painter.text(
                map_rect.center(),
                Align2::CENTER_CENTER,
                "Fetching TLEs from Celestrak...",
                FontId::proportional(14.0),
                Color32::from_rgb(0xCC, 0xCC, 0xCC),
            );
        }
    }

    fn draw_track_segments(&self, painter: &Painter, track: &[(f64, f64)], map_rect: Rect) {
        if track.len() < 2 { return }

        let to_screen = |lat: f64, lon: f64| -> Pos2 {
            geo_to_screen(map_rect, lat, lon, self.center_lat, self.center_lon, self.zoom_ppd)
        };

        let mut prev = to_screen(track[0].0, track[0].1);
        for &(lat, lon) in &track[1..] {
            let cur = to_screen(lat, lon);
            // Skip segment if it crosses the antimeridian (large longitude jump in screen space).
            if (cur.x - prev.x).abs() < map_rect.width() * 0.5 {
                painter.line_segment(
                    [prev, cur],
                    Stroke::new(1.5, Color32::from_rgba_unmultiplied(0x4C, 0xAF, 0xFF, 0x90)),
                );
            }
            prev = cur;
        }
    }

    fn draw_footprint(&self, painter: &Painter, pos: &SatPosition, map_rect: Rect) {
        // Angular radius of footprint on Earth's surface (central angle in radians).
        let rho = FOOTPRINT_KM / EARTH_R_KM;

        // Project footprint circle in geographic space: 36 points around the satellite.
        let sat_lat = pos.lat_deg.to_radians();
        let sat_lon = pos.lon_deg.to_radians();
        let steps = 72usize;
        let mut pts: Vec<Pos2> = Vec::with_capacity(steps);

        for i in 0..steps {
            let az = (i as f64 / steps as f64) * std::f64::consts::TAU;
            // Destination lat/lon given bearing and angular distance from sat position.
            let lat2 = (sat_lat.sin() * rho.cos() + sat_lat.cos() * rho.sin() * az.cos()).asin();
            let dlon = az.sin() * rho.sin() * sat_lat.cos();
            let dlon2 = rho.cos() - sat_lat.sin() * lat2.sin();
            let lon2 = sat_lon + dlon.atan2(dlon2);
            pts.push(geo_to_screen(
                map_rect, lat2.to_degrees(), lon2.to_degrees(),
                self.center_lat, self.center_lon, self.zoom_ppd,
            ));
        }

        // Draw polygon edges, skipping antimeridian wraps.
        for i in 0..steps {
            let a = pts[i];
            let b = pts[(i + 1) % steps];
            if (a.x - b.x).abs() < map_rect.width() * 0.5 {
                painter.line_segment(
                    [a, b],
                    Stroke::new(1.0, Color32::from_rgba_unmultiplied(0x4C, 0xAF, 0xFF, 0x50)),
                );
            }
        }
    }

    // ── Sidebar ───────────────────────────────────────────────────────────────

    fn draw_sidebar(&mut self, ui: &mut egui::Ui, panel_rect: Rect) {
        ui.painter().rect_filled(panel_rect, Rounding::ZERO, Color32::from_rgb(0x16, 0x1A, 0x22));

        let mut sidebar_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(panel_rect.shrink(8.0))
                .layout(egui::Layout::top_down(egui::Align::LEFT)),
        );

        sidebar_ui.label(RichText::new("ORBCOMM OG2").strong().color(Color32::from_rgb(0x4C, 0xAF, 0xFF)));
        sidebar_ui.separator();

        if self.tles.is_empty() {
            sidebar_ui.label(
                RichText::new("Loading TLEs…")
                    .color(Color32::from_rgb(0xAA, 0xAA, 0xAA))
                    .italics(),
            );
            return;
        }

        // Satellite list
        sidebar_ui.label(RichText::new("Satellites").small().color(Color32::GRAY));
        egui::ScrollArea::vertical()
            .max_height(panel_rect.height() * 0.35)
            .id_salt("sat_list")
            .show(&mut sidebar_ui, |ui| {
                for (i, (tle, sd)) in self.tles.iter().zip(self.sat_data.iter()).enumerate() {
                    let is_selected = self.selected_idx == Some(i);
                    let el_str = sd.pos.as_ref()
                        .map(|p| format!("{:.1}°", p.el_deg))
                        .unwrap_or_else(|| "—".to_string());
                    let visible = sd.pos.as_ref().map(|p| p.el_deg > 5.0).unwrap_or(false);
                    let dot_color = if visible { Color32::from_rgb(0x44, 0xDD, 0x66) }
                                    else { Color32::from_rgb(0x44, 0x44, 0x44) };
                    let label = format!("● {} el {}", tle.name.trim(), el_str);
                    let text = RichText::new(label).small()
                        .color(if is_selected { Color32::WHITE } else { dot_color });
                    if ui.label(text).clicked() {
                        self.selected_idx = Some(i);
                    }
                }
            });

        sidebar_ui.separator();

        // Pass schedule for selected satellite
        if let Some(idx) = self.selected_idx {
            if let (Some(tle), Some(sd)) = (self.tles.get(idx), self.sat_data.get(idx)) {
                sidebar_ui.label(
                    RichText::new(format!("Next passes — {}", tle.name.trim()))
                        .small()
                        .color(Color32::GRAY),
                );

                if sd.passes.is_empty() {
                    sidebar_ui.label(
                        RichText::new("No passes in 24 h").small()
                            .color(Color32::from_rgb(0x88, 0x88, 0x88)),
                    );
                } else {
                    for pass in &sd.passes {
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
                            .duration_since(SystemTime::now())
                            .map(|d| {
                                let m = d.as_secs() / 60;
                                if m < 60 { format!("in {m}m") } else { format!("in {}h{}m", m/60, m%60) }
                            })
                            .unwrap_or_else(|_| "now".to_string());

                        sidebar_ui.add(egui::Separator::default().spacing(4.0));
                        let el_color = if pass.max_el_deg > 60.0 { Color32::from_rgb(0x44, 0xDD, 0x66) }
                                       else if pass.max_el_deg > 20.0 { Color32::from_rgb(0xE8, 0xC5, 0x4B) }
                                       else { Color32::from_rgb(0x88, 0xCC, 0x88) };
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
                            RichText::new(format!("Az {:.0}°  az at AOS", pass.aos_az_deg))
                                .small()
                                .color(Color32::DARK_GRAY),
                        );
                    }
                }
            }
        }
    }

    // ── OSM tile machinery ────────────────────────────────────────────────────

    fn draw_tiles(&mut self, painter: &Painter, map_rect: Rect, ctx: &egui::Context) {
        let z = osm_zoom(self.zoom_ppd);

        if z != self.last_tile_z {
            // Flush stale textures to the eviction buffer; they'll be dropped next frame.
            self.evicted_tiles.extend(self.tile_cache.drain().map(|(_, v)| v));
            self.pending_tiles.clear();
            self.last_tile_z = z;
        }
        // Drop textures evicted last frame.
        self.evicted_tiles.clear();

        // Drain completed tile fetches.
        while let Ok(result) = self.tile_rx.try_recv() {
            self.pending_tiles.remove(&result.key);
            if let Some(img) = result.image {
                let tex = ctx.load_texture(
                    format!("tile_{}_{}_{}", result.key.0, result.key.1, result.key.2),
                    img,
                    TextureOptions::LINEAR,
                );
                self.tile_cache.insert(result.key, tex);
            }
        }

        // Determine which tiles are visible.
        let tile_deg = 360.0 / 2.0f64.powi(z as i32);
        let tile_px = (tile_deg as f32 * self.zoom_ppd).max(1.0);
        let (cx, cy) = lat_lon_to_tile_xy(self.center_lat, self.center_lon, z);
        let tiles_x = (map_rect.width() / tile_px).ceil() as i32 + 2;
        let tiles_y = (map_rect.height() / tile_px).ceil() as i32 + 2;

        for dy in -tiles_y / 2 - 1..=tiles_y / 2 + 1 {
            for dx in -tiles_x / 2 - 1..=tiles_x / 2 + 1 {
                let tx = cx + dx;
                let ty = (cy + dy).clamp(0, (1 << z) - 1);
                let key: TileKey = (z, tx, ty);

                let (nw_lat, nw_lon) = tile_nw(z, tx, ty);
                let (se_lat, se_lon) = tile_nw(z, tx + 1, ty + 1);
                let nw = geo_to_screen(map_rect, nw_lat, nw_lon, self.center_lat, self.center_lon, self.zoom_ppd);
                let se = geo_to_screen(map_rect, se_lat, se_lon, self.center_lat, self.center_lon, self.zoom_ppd);
                let tile_rect = Rect::from_min_max(nw, se);

                if let Some(tex) = self.tile_cache.get(&key) {
                    painter.image(
                        tex.id(),
                        tile_rect,
                        Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                        Color32::from_rgba_unmultiplied(0xCC, 0xCC, 0xCC, 0xFF),
                    );
                } else {
                    painter.rect_filled(tile_rect, Rounding::ZERO, Color32::from_rgb(0x1E, 0x24, 0x2E));
                    if !self.pending_tiles.contains(&key) && self.pending_tiles.len() < 24 {
                        self.pending_tiles.insert(key);
                        fetch_tile_async(self.tile_tx.clone(), z, tx, ty);
                    }
                }
            }
        }
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
    fn format_utc_hms_epoch() {
        assert_eq!(format_utc_hms(0), "00:00:00");
    }

    #[test]
    fn format_utc_hms_known() {
        // 1704110400 = 2024-01-01 12:00:00 UTC
        assert_eq!(format_utc_hms(1_704_110_400), "12:00:00");
    }

    #[test]
    fn sat_map_window_new_has_no_tles() {
        let w = SatMapWindow::new(41.5, -81.7);
        assert!(w.tles.is_empty());
        assert!(w.selected_idx.is_none());
    }

    #[test]
    fn sat_map_window_viewport_starts_open() {
        let w = SatMapWindow::new(41.5, -81.7);
        assert!(w.viewport_open);
    }

    #[test]
    fn flash_norad_ids_empty_on_start() {
        let w = SatMapWindow::new(41.5, -81.7);
        assert!(w.flash_norad_ids.is_empty());
    }
}
