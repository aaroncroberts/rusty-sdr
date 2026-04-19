//! ADS-B aircraft map panel.
//!
//! Renders a Mercator-projected interactive map with live aircraft positions,
//! heading vectors, altitude-coded colors, and fade-out position trails.
//!
//! The panel is displayed as a `egui::Window`.  Call
//! [`AdsbMapWindow::show`] each frame when `open == true`.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::time::Instant;

use crossbeam_channel::{Receiver, Sender};
use egui::{
    Color32, ColorImage, FontId, Frame, Grid, Key, Margin, Painter, Pos2, Rect, Response,
    RichText, Rounding, Sense, Stroke, TextureHandle, TextureOptions, Vec2,
};
use sdrapp_adsb::state::AircraftState;

// ── Flight info lookup ────────────────────────────────────────────────────────

/// Enriched aircraft data fetched from external APIs.
#[derive(Debug, Clone, Default)]
pub struct FlightInfo {
    /// Tail / registration number (e.g. "N12345", "G-EUOE").
    pub registration: Option<String>,
    /// ICAO aircraft type code (e.g. "B738", "A320").
    pub aircraft_type: Option<String>,
    /// Human-readable aircraft description (e.g. "BOEING 737-800").
    pub aircraft_desc: Option<String>,
    /// Operator / airline (e.g. "United Airlines").
    pub operator: Option<String>,
    /// Flight / callsign from API lookup (e.g. "DAL1234").
    pub callsign_api: Option<String>,
    /// ICAO departure airport (e.g. "KORD").
    pub origin: Option<String>,
    /// ICAO arrival airport (e.g. "KJFK").
    pub destination: Option<String>,
}

#[derive(Debug, Clone)]
enum FlightLookupState {
    Fetching,
    Ready(FlightInfo),
    Failed,
}

struct FlightInfoResult {
    icao: u32,
    info: Option<FlightInfo>,
}

/// Spawn two background threads to fetch aircraft data:
/// 1. adsb.lol  — registration, type, operator (fast, no auth)
/// 2. OpenSky   — estimated departure / arrival airports
fn fetch_flight_info_async(
    tx: crossbeam_channel::Sender<FlightInfoResult>,
    icao: u32,
    callsign: Option<String>,
) {
    std::thread::spawn(move || {
        let hex = format!("{:06x}", icao);
        let mut info = FlightInfo::default();

        // ── adsb.lol: registration + type + operator ──────────────────────────
        let lol_url = format!("https://api.adsb.lol/v2/icao/{hex}");
        if let Ok(resp) = ureq::get(&lol_url)
            .set("User-Agent", "sdrapp ADS-B map/1.0 (desktop SDR application)")
            .call()
        {
            if let Ok(body) = resp.into_string() {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&body) {
                    if let Some(ac) = json["ac"].as_array().and_then(|a| a.first()) {
                        info.registration = ac["r"].as_str()
                            .filter(|s| !s.is_empty()).map(str::to_string);
                        info.aircraft_type = ac["t"].as_str()
                            .filter(|s| !s.is_empty()).map(str::to_string);
                        // Human-readable description (e.g. "AIRBUS A-321") —
                        // prefer this over the raw ICAO type code for display.
                        info.aircraft_desc = ac["desc"].as_str()
                            .filter(|s| !s.is_empty())
                            .map(|s| titlecase(s));
                        info.operator = ac["ownOp"].as_str()
                            .filter(|s| !s.is_empty()).map(str::to_string);
                        // Callsign / flight number from live feed (may be more
                        // up-to-date than what the aircraft has broadcast so far).
                        info.callsign_api = ac["flight"].as_str()
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(str::to_string);
                    }
                }
            }
        }

        // ── OpenSky: departure / arrival airports (last 24 h) ─────────────────
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let begin = now.saturating_sub(86_400);
        let sky_url = format!(
            "https://opensky-network.org/api/flights/aircraft?icao24={hex}&begin={begin}&end={now}"
        );
        if let Ok(resp) = ureq::get(&sky_url)
            .set("User-Agent", "sdrapp ADS-B map/1.0 (desktop SDR application)")
            .call()
        {
            if let Ok(body) = resp.into_string() {
                if let Ok(serde_json::Value::Array(flights)) =
                    serde_json::from_str::<serde_json::Value>(&body)
                {
                    // Use the most recent entry (last in the array).
                    if let Some(last) = flights.last() {
                        let valid_airport = |v: &serde_json::Value| -> Option<String> {
                            v.as_str()
                                .filter(|s| !s.is_empty() && *s != "null")
                                .map(str::to_string)
                        };
                        info.origin = valid_airport(&last["estDepartureAirport"]);
                        info.destination = valid_airport(&last["estArrivalAirport"]);
                        // Fall back to callsign from OpenSky if we didn't have one.
                        if callsign.as_deref().map(str::trim).unwrap_or("").is_empty() {
                            if let Some(cs) = last["callsign"].as_str()
                                .map(str::trim).filter(|s| !s.is_empty())
                            {
                                let _ = cs; // callsign already in AircraftState
                            }
                        }
                    }
                }
            }
        }

        let _ = tx.send(FlightInfoResult { icao, info: Some(info) });
    });
}

// ── OSM tile types ─────────────────────────────────────────────────────────────

/// (zoom_level, tile_x, tile_y)
type TileKey = (u8, i32, i32);

struct TileFetchResult {
    key: TileKey,
    image: Option<ColorImage>,
}

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

// ── OSM tile helpers ───────────────────────────────────────────────────────────

/// Convert zoom_ppd (pixels/degree longitude) to an OSM tile zoom level.
/// Formula: `2^z = zoom_ppd * 360 / 256`, clamped to [0, 12].
fn osm_zoom(zoom_ppd: f32) -> u8 {
    let z = (zoom_ppd * 360.0 / 256.0).log2().round() as i32;
    z.clamp(0, 12) as u8
}

/// Return the NW corner (lat, lon) of the OSM tile at (z, x, y).
fn tile_nw(z: u8, x: i32, y: i32) -> (f64, f64) {
    let n = 2.0f64.powi(z as i32);
    let lon = x as f64 / n * 360.0 - 180.0;
    let lat = (std::f64::consts::PI * (1.0 - 2.0 * y as f64 / n)).sinh().atan().to_degrees();
    (lat, lon)
}

/// Convert a (lat, lon) to the OSM tile (x, y) at zoom level z.
fn lat_lon_to_tile_xy(lat: f64, lon: f64, z: u8) -> (i32, i32) {
    let n = 2.0f64.powi(z as i32);
    let x = ((lon + 180.0) / 360.0 * n).floor() as i32;
    let lat_rad = lat.clamp(-85.05, 85.05).to_radians();
    let y = ((1.0 - (lat_rad.tan() + 1.0 / lat_rad.cos()).ln() / std::f64::consts::PI) / 2.0 * n)
        .floor() as i32;
    (x, y)
}

/// Spawn a background thread to fetch one OSM tile and send the result back.
fn fetch_tile_async(tx: Sender<TileFetchResult>, z: u8, x: i32, y: i32) {
    std::thread::spawn(move || {
        let url = format!("https://tile.openstreetmap.org/{z}/{x}/{y}.png");
        let color_image = (|| -> Option<ColorImage> {
            let resp = ureq::get(&url)
                .set("User-Agent", "sdrapp ADS-B map/1.0 (desktop SDR application)")
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
        let _ = tx.send(TileFetchResult { key: (z, x, y), image: color_image });
    });
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
    /// When true, OSM tiles are tinted dark so aircraft stand out better.
    map_dim: bool,
    /// Set to true when the user clicks "Set Home" (📍); caller clears it and persists.
    pub set_home_pending: bool,
    /// Tracks whether the OS viewport window is open. Set to false when the OS window
    /// close button is pressed; caller resets to true when it re-opens the window.
    pub viewport_open: bool,

    // ── Decoder state (written by main app each frame) ────────────────────────
    /// True while the ADS-B decoder thread is running.
    pub decoder_running: bool,
    /// True while waiting for hardware to reconfigure to 2 Msps before starting.
    pub adsb_start_pending: bool,
    /// DF-17 frames that passed CRC.
    pub frame_count: u64,
    /// Frames where CRC-24 passed (any DF type).
    /// > 0 means decoding is working.  If frame_count = 0 but crc_ok_count > 0,
    /// we're decoding valid Mode S but no DF17 ADS-B squitter is present.
    pub crc_ok_count: u64,
    /// Preamble detections before CRC check.  > 0 means signal is present.
    /// If crc_ok_count = 0 but preamble_count > 0, signal is detected but all
    /// frames are failing CRC (sample-rate or frequency issue).
    pub preamble_count: u64,
    /// True when the hardware sample rate is ≥ 2 Msps (required for ADS-B).
    pub sample_rate_ok: bool,

    // ── Action requests (set by map UI, consumed by main app each frame) ──────
    /// Set when the user clicks Start in the map toolbar; main app consumes & clears.
    pub start_requested: bool,
    /// Set when the user clicks Stop in the map toolbar; main app consumes & clears.
    pub stop_requested: bool,

    /// Whether the FLIGHT DATA section in the detail panel is expanded.
    flight_info_expanded: bool,

    // ── Flight info lookup cache ──────────────────────────────────────────────
    /// Per-ICAO enriched data fetched from adsb.lol + OpenSky.
    flight_info_cache: HashMap<u32, FlightLookupState>,
    /// Sender cloned into worker threads.
    flight_info_tx: crossbeam_channel::Sender<FlightInfoResult>,
    /// Receiver drained each frame.
    flight_info_rx: crossbeam_channel::Receiver<FlightInfoResult>,
    /// ICAO that was selected last frame — used to detect selection changes.
    prev_selected_icao: Option<u32>,

    // ── OSM tile cache ────────────────────────────────────────────────────────
    /// Loaded tile textures keyed by (z, x, y).
    tile_cache: HashMap<TileKey, TextureHandle>,
    /// Tiles that have been requested but not yet received.
    pending_tiles: HashSet<TileKey>,
    /// Sender end of the tile-fetch result channel (cloned into worker threads).
    tile_tx: Sender<TileFetchResult>,
    /// Receiver end; drained each frame in the render loop.
    tile_rx: Receiver<TileFetchResult>,
    /// OSM zoom level used for the most recent tile set. When this changes, the
    /// cache and pending set are flushed so stale tiles don't accumulate.
    last_tile_z: u8,
    /// TextureHandles evicted during the previous frame, held for one extra frame
    /// so in-flight GPU commands can finish before wgpu destroys the textures.
    evicted_tiles: Vec<TextureHandle>,
}

impl AdsbMapWindow {
    /// Create a new map centered over Cleveland OH.
    pub fn new() -> Self {
        Self::with_viewport(41.5, -81.7, 100.0)
    }

    /// Create a map with a specific initial viewport (restored from config).
    pub fn with_viewport(center_lat: f64, center_lon: f64, zoom_ppd: f32) -> Self {
        let (tile_tx, tile_rx) = crossbeam_channel::unbounded();
        let (fi_tx, fi_rx) = crossbeam_channel::unbounded::<FlightInfoResult>();
        Self {
            center_lat,
            center_lon,
            zoom_ppd,
            selected_icao: None,
            trails: HashMap::new(),
            drag_start: None,
            map_dim: true,
            set_home_pending: false,
            viewport_open: true,
            decoder_running: false,
            adsb_start_pending: false,
            frame_count: 0,
            crc_ok_count: 0,
            preamble_count: 0,
            sample_rate_ok: true,
            start_requested: false,
            stop_requested: false,
            tile_cache: HashMap::new(),
            pending_tiles: HashSet::new(),
            tile_tx,
            tile_rx,
            last_tile_z: 255, // force first-frame flush
            evicted_tiles: Vec::new(),
            flight_info_expanded: true,
            flight_info_cache: HashMap::new(),
            flight_info_tx: fi_tx,
            flight_info_rx: fi_rx,
            prev_selected_icao: None,
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

        // ── Drain flight info results ─────────────────────────────────────────
        while let Ok(result) = self.flight_info_rx.try_recv() {
            let state = match result.info {
                Some(info) => FlightLookupState::Ready(info),
                None => FlightLookupState::Failed,
            };
            self.flight_info_cache.insert(result.icao, state);
        }

        // ── Trigger lookup when selection changes ─────────────────────────────
        if self.selected_icao != self.prev_selected_icao {
            self.prev_selected_icao = self.selected_icao;
            if let Some(icao) = self.selected_icao {
                if !self.flight_info_cache.contains_key(&icao) {
                    let callsign = aircraft.iter()
                        .find(|a| a.icao == icao)
                        .and_then(|a| a.callsign.clone());
                    self.flight_info_cache.insert(icao, FlightLookupState::Fetching);
                    fetch_flight_info_async(self.flight_info_tx.clone(), icao, callsign);
                }
            }
        }

        let mut clicked = None;

        egui::CentralPanel::default()
            .frame(
                Frame::none()
                    .fill(Color32::from_rgb(0x10, 0x14, 0x1A))
                    .inner_margin(egui::Margin { left: 6.0, right: 0.0, top: 4.0, bottom: 0.0 }),
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

                // ── Toolbar (single row) ──────────────────────────────────────
                ui.horizontal(|ui| {
                    ui.spacing_mut().button_padding = Vec2::new(8.0, 4.0);
                    ui.spacing_mut().item_spacing.x = 4.0;
                    let btn_fill = Color32::from_rgb(0x1E, 0x28, 0x38);
                    let muted    = Color32::from_rgb(0x8A, 0x9A, 0xB0);
                    let accent   = Color32::from_rgb(0x4E, 0xC9, 0xE0);

                    // ── Start / Stop ──────────────────────────────────────────
                    if self.decoder_running || self.adsb_start_pending {
                        if ui
                            .add(egui::Button::new(RichText::new("Stop").color(Color32::from_rgb(0xFF, 0x88, 0x88)))
                                .fill(Color32::from_rgb(0x40, 0x14, 0x14)))
                            .on_hover_text("Stop ADS-B decoder · restore audio")
                            .clicked()
                        {
                            self.stop_requested = true;
                        }
                    } else {
                        let start_lbl = if self.sample_rate_ok { "Start" } else { "Start*" };
                        let tip = if self.sample_rate_ok {
                            "Tune to 1090 MHz · switch to Antenna B · start decoder"
                        } else {
                            "Will auto-reconfigure hardware to 2 Msps, then start"
                        };
                        if ui
                            .add(egui::Button::new(RichText::new(start_lbl).color(Color32::from_rgb(0x73, 0xC9, 0x91)))
                                .fill(Color32::from_rgb(0x10, 0x32, 0x1A)))
                            .on_hover_text(tip)
                            .clicked()
                        {
                            self.start_requested = true;
                        }
                    }

                    ui.separator();

                    // ── Zoom ─────────────────────────────────────────────────
                    if ui.add(egui::Button::new(RichText::new("+").color(muted)).fill(btn_fill))
                        .on_hover_text("Zoom in").clicked()
                    {
                        self.zoom_ppd = (self.zoom_ppd * 1.5).min(MAX_ZOOM);
                    }
                    if ui.add(egui::Button::new(RichText::new("−").color(muted)).fill(btn_fill))
                        .on_hover_text("Zoom out").clicked()
                    {
                        self.zoom_ppd = (self.zoom_ppd / 1.5).max(MIN_ZOOM);
                    }

                    // ── Map controls ─────────────────────────────────────────
                    if ui.add(egui::Button::new(RichText::new("Home").color(muted)).fill(btn_fill))
                        .on_hover_text("Reset to saved home location").clicked()
                    {
                        self.center_lat = home_lat;
                        self.center_lon = home_lon;
                        self.zoom_ppd = 100.0;
                    }
                    if ui.add(egui::Button::new(RichText::new("Pin").color(accent)).fill(btn_fill))
                        .on_hover_text("Save current view as home").clicked()
                    {
                        self.set_home_pending = true;
                    }
                    let has_positions = aircraft.iter().any(|a| a.lat.is_some() && a.lon.is_some());
                    let center_fill = if has_positions { btn_fill } else { Color32::from_rgb(0x14, 0x18, 0x22) };
                    if ui
                        .add(egui::Button::new(
                            RichText::new("Center").color(if has_positions { accent } else { Color32::from_rgb(0x4A, 0x5A, 0x6A) })
                        ).fill(center_fill))
                        .on_hover_text("Center map on received aircraft")
                        .clicked()
                        && has_positions
                    {
                        let (sum_lat, sum_lon, count) = aircraft.iter()
                            .filter_map(|a| a.lat.zip(a.lon))
                            .fold((0.0f64, 0.0f64, 0u32), |(slat, slon, n), (lat, lon)| {
                                (slat + lat, slon + lon, n + 1)
                            });
                        if count > 0 {
                            self.center_lat = sum_lat / count as f64;
                            self.center_lon = sum_lon / count as f64;
                        }
                    }
                    let dim_active = self.map_dim;
                    let dim_fill = if dim_active { Color32::from_rgb(0x10, 0x28, 0x3A) } else { btn_fill };
                    if ui.add(egui::Button::new(
                            RichText::new("Dim").color(if dim_active { accent } else { muted })
                        ).fill(dim_fill))
                        .on_hover_text(if dim_active { "Map dim: ON — click to turn off" } else { "Map dim: OFF — click to dim map" })
                        .clicked()
                    {
                        self.map_dim = !self.map_dim;
                    }

                    ui.separator();

                    // ── Altitude legend ───────────────────────────────────────
                    for (color, label) in [
                        (Color32::from_rgb(0x73, 0xC9, 0x91), "Low"),
                        (Color32::from_rgb(0xE8, 0xC5, 0x4B), "Mid"),
                        (Color32::from_rgb(0xFF, 0x55, 0x55), "High"),
                    ] {
                        let (dot_rect, _) = ui.allocate_exact_size(Vec2::splat(10.0), egui::Sense::hover());
                        ui.painter().circle_filled(dot_rect.center(), 4.0, color);
                        ui.label(RichText::new(label).color(muted).small());
                    }
                });
                ui.separator();

                // ── Aircraft list side panel (left) ──────────────────────────
                // Shows all decoded aircraft even if they have no position yet.
                // Click to select + pan to aircraft.
                let mut pan_to: Option<(f64, f64)> = None;
                egui::SidePanel::left("adsb_list_panel")
                    .exact_width(170.0)
                    .resizable(false)
                    .frame(
                        Frame::default()
                            .fill(Color32::from_rgb(0x0A, 0x0E, 0x16))
                            .stroke(Stroke::new(1.0, Color32::from_rgb(0x1A, 0x22, 0x30)))
                            .inner_margin(Margin::same(6.0)),
                    )
                    .show_inside(ui, |ui| {
                        let muted = Color32::from_rgb(0x4A, 0x5A, 0x6A);
                        ui.label(RichText::new("AIRCRAFT").color(muted).small());
                        ui.add_space(2.0);

                        // Sort: live + positioned first, then stale, then no-position
                        let mut sorted: Vec<&AircraftState> = aircraft.iter().collect();
                        sorted.sort_by(|a, b| {
                            let stale_a = a.is_stale() as u8;
                            let stale_b = b.is_stale() as u8;
                            let pos_a = a.lat.is_some() as u8;
                            let pos_b = b.lat.is_some() as u8;
                            stale_a.cmp(&stale_b)
                                .then(pos_b.cmp(&pos_a))
                                .then(b.altitude_ft.unwrap_or(0).cmp(&a.altitude_ft.unwrap_or(0)))
                        });

                        egui::ScrollArea::vertical().show(ui, |ui| {
                            let mut prev_stale = false;
                            for ac in &sorted {
                                let is_sel = self.selected_icao == Some(ac.icao);
                                let has_pos = ac.lat.is_some();
                                let stale = ac.is_stale();

                                // Divider between live and stale groups
                                if stale && !prev_stale && !sorted.iter().all(|a| a.is_stale()) {
                                    ui.add_space(2.0);
                                    ui.label(RichText::new("── lost signal ──").size(8.0).color(Color32::from_rgb(0x30, 0x3A, 0x48)));
                                }
                                prev_stale = stale;

                                let row_bg = if is_sel {
                                    Color32::from_rgb(0x1A, 0x2A, 0x3A)
                                } else {
                                    Color32::TRANSPARENT
                                };

                                let callsign = ac.callsign.as_deref()
                                    .map(str::trim)
                                    .filter(|s| !s.is_empty())
                                    .unwrap_or("");
                                let icao_str = format!("{:06X}", ac.icao);
                                let primary = if callsign.is_empty() { &icao_str } else { callsign };

                                // ✈ for positioned live, ○ for no-pos, ◌ for stale
                                let icon = if stale { "◌" } else if has_pos { "✈" } else { "○" };
                                let alt_str = ac.altitude_ft
                                    .map(|a| format!(" {}ft", a / 100 * 100))
                                    .unwrap_or_default();
                                let row_text = format!("{icon} {primary}{alt_str}");

                                let label_color = if stale {
                                    Color32::from_rgb(0x38, 0x48, 0x58)
                                } else if has_pos {
                                    altitude_color(ac.altitude_ft)
                                } else {
                                    muted
                                };

                                let resp = ui.add(
                                    egui::Label::new(
                                        RichText::new(&row_text)
                                            .small()
                                            .color(label_color)
                                            .background_color(row_bg),
                                    )
                                    .sense(egui::Sense::click()),
                                );
                                if resp.clicked() {
                                    self.selected_icao = Some(ac.icao);
                                    if let (Some(lat), Some(lon)) = (ac.lat, ac.lon) {
                                        pan_to = Some((lat, lon));
                                    }
                                }
                                let age = ac.last_seen.elapsed().as_secs();
                                let hover = format!(
                                    "{icao_str}{}{}",
                                    if callsign.is_empty() { String::new() } else { format!(" · {callsign}") },
                                    if stale { format!("  (lost {}s ago)", age) } else { String::new() }
                                );
                                resp.on_hover_text(hover);
                            }
                        });
                    });
                if let Some((lat, lon)) = pan_to {
                    self.center_lat = lat;
                    self.center_lon = lon;
                }

                // ── Detail side panel (pre-clone to avoid borrow conflict) ───
                let selected_ac = self.selected_icao
                    .and_then(|icao| aircraft.iter().find(|a| a.icao == icao).cloned());
                let selected_flight_info = self.selected_icao
                    .and_then(|icao| self.flight_info_cache.get(&icao));

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
                            close_detail = show_aircraft_detail(
                                ui, ac, selected_flight_info,
                                &mut self.flight_info_expanded,
                            );
                        });
                }
                if close_detail {
                    self.selected_icao = None;
                }

                // ── Bottom status bar ─────────────────────────────────────────
                // Diagnostic counters live here so the toolbar stays uncluttered.
                egui::TopBottomPanel::bottom("adsb_status_bar")
                    .exact_height(20.0)
                    .frame(
                        Frame::none()
                            .fill(Color32::from_rgb(0x08, 0x0C, 0x12))
                            .stroke(Stroke::new(1.0, Color32::from_rgb(0x1A, 0x22, 0x30)))
                            .inner_margin(Margin { left: 8.0, right: 8.0, top: 2.0, bottom: 2.0 }),
                    )
                    .show_inside(ui, |ui| {
                        ui.horizontal(|ui| {
                            let muted = Color32::from_rgb(0x5A, 0x6A, 0x7A);

                            // Status dot + label always visible in the bar
                            let (dot_color, status_text) = if self.adsb_start_pending {
                                (Color32::from_rgb(0xC0, 0x80, 0x00), "Configuring")
                            } else if self.decoder_running {
                                (Color32::from_rgb(0x73, 0xC9, 0x91), "Live")
                            } else {
                                (Color32::from_rgb(0x6A, 0x7A, 0x8A), "Stopped")
                            };
                            let (dot_r, _) = ui.allocate_exact_size(Vec2::splat(10.0), egui::Sense::hover());
                            ui.painter().circle_filled(dot_r.center(), 4.0, dot_color);
                            ui.label(RichText::new(status_text).small().color(dot_color));
                            ui.label(RichText::new("·").small().color(muted));

                            if self.decoder_running {
                                let ac_count = aircraft.len();
                                let fr = self.frame_count;
                                let ok = self.crc_ok_count;
                                let pr = self.preamble_count;

                                ui.label(
                                    RichText::new(format!("{ac_count} aircraft"))
                                        .small()
                                        .color(Color32::from_rgb(0x8A, 0x9A, 0xB0)),
                                );
                                ui.label(RichText::new("·").small().color(muted));

                                let (signal_text, signal_color) = if fr > 0 {
                                    (format!("{fr} DF17 frames"), Color32::from_rgb(0x73, 0xC9, 0x91))
                                } else if ok > 0 {
                                    (format!("{ok} Mode S  (no ADS-B)"), Color32::from_rgb(0xE8, 0xC5, 0x4B))
                                } else if pr > 0 {
                                    (format!("{pr} preambles  (bad CRC)"), Color32::from_rgb(0xC0, 0x80, 0x00))
                                } else {
                                    ("No signal".to_string(), Color32::from_rgb(0x8A, 0x4A, 0x4A))
                                };
                                ui.label(RichText::new(signal_text).small().color(signal_color));

                                if !self.sample_rate_ok {
                                    ui.add_space(4.0);
                                    ui.label(
                                        RichText::new("2 Msps required")
                                            .small()
                                            .color(Color32::from_rgb(0xFF, 0xC0, 0x40))
                                            .background_color(Color32::from_rgba_premultiplied(60, 40, 0, 120)),
                                    );
                                }
                            } else if self.adsb_start_pending {
                                ui.label(
                                    RichText::new("Configuring hardware (2 Msps, 1090 MHz)…")
                                        .small()
                                        .color(Color32::from_rgb(0xC0, 0x80, 0x00)),
                                );
                            } else {
                                ui.label(
                                    RichText::new("Press Start to begin receiving")
                                        .small()
                                        .color(muted),
                                );
                            }
                        });
                    });

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

    /// Drain the tile fetch channel and upload newly arrived textures to GPU.
    fn drain_tile_results(&mut self, ctx: &egui::Context) {
        while let Ok(result) = self.tile_rx.try_recv() {
            self.pending_tiles.remove(&result.key);
            if let Some(color_image) = result.image {
                let (z, x, y) = result.key;
                let tex = ctx.load_texture(
                    format!("osm_tile_{z}_{x}_{y}"),
                    color_image,
                    TextureOptions::LINEAR,
                );
                self.tile_cache.insert(result.key, tex);
            }
        }
    }

    /// Draw OSM tiles for the current viewport, requesting any that are missing.
    fn draw_tiles(&mut self, painter: &Painter, rect: Rect) {
        let z = osm_zoom(self.zoom_ppd);

        // When zoom level changes, move stale tiles into evicted_tiles rather than
        // dropping immediately — in-flight GPU commands from the previous frame may
        // still reference those textures.  We drop them next frame instead.
        if z != self.last_tile_z {
            self.evicted_tiles.extend(self.tile_cache.drain().map(|(_, v)| v));
            self.pending_tiles.clear();
            self.last_tile_z = z;
        }

        let n = 2i32.pow(z as u32);

        // Visible lon range in degrees
        let half_w_deg = rect.width() as f64 / self.zoom_ppd as f64 * 0.6;
        let half_h_deg = rect.height() as f64
            / merc_scale(self.center_lat, self.zoom_ppd) as f64
            * 0.6;

        let (x_min, y_min) = lat_lon_to_tile_xy(
            (self.center_lat + half_h_deg).min(85.0),
            self.center_lon - half_w_deg,
            z,
        );
        let (x_max, y_max) = lat_lon_to_tile_xy(
            (self.center_lat - half_h_deg).max(-85.0),
            self.center_lon + half_w_deg,
            z,
        );

        // Safety: never try to render more than 9×9 tiles per frame.
        let tile_w = (x_max - x_min + 1).min(9).max(0);
        let tile_h = (y_max - y_min + 1).min(9).max(0);
        if tile_w * tile_h > 81 {
            return;
        }

        for ty in y_min..=y_min + tile_h - 1 {
            if ty < 0 || ty >= n {
                continue;
            }
            for tx in x_min..=x_min + tile_w - 1 {
                // Wrap longitude tiles
                let tx_w = ((tx % n) + n) % n;
                let key = (z, tx_w, ty);

                // Compute screen rect: project NW and SE corners of this tile.
                let (nw_lat, nw_lon) = tile_nw(z, tx_w, ty);
                let (se_lat, se_lon) = tile_nw(z, tx_w + 1, ty + 1);
                let nw = geo_to_screen(
                    rect, nw_lat, nw_lon, self.center_lat, self.center_lon, self.zoom_ppd,
                );
                let se = geo_to_screen(
                    rect, se_lat, se_lon, self.center_lat, self.center_lon, self.zoom_ppd,
                );
                // Expand by 0.5 px on each side to close sub-pixel seams between tiles.
                let tile_rect = Rect::from_min_max(nw, se).expand(0.5);
                if !rect.intersects(tile_rect) {
                    continue;
                }

                if let Some(tex) = self.tile_cache.get(&key) {
                    let uv = egui::Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
                    let tint = if self.map_dim {
                        Color32::from_rgb(110, 120, 130) // dim + slight blue-grey tint
                    } else {
                        Color32::WHITE
                    };
                    painter.image(tex.id(), tile_rect, uv, tint);
                } else if !self.pending_tiles.contains(&key) {
                    self.pending_tiles.insert(key);
                    fetch_tile_async(self.tile_tx.clone(), z, tx_w, ty);
                }
            }
        }
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

        // ── Drain tile fetch results + draw OSM tiles ─────────────────────────
        // Drop handles evicted last frame — GPU submit from that frame is now done.
        self.evicted_tiles.clear();
        self.drain_tile_results(&response.ctx);
        self.draw_tiles(painter, rect);

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

            let stale = ac.is_stale();
            let dim = if stale { 0.25 } else { 1.0 };
            let base_color = altitude_color(ac.altitude_ft);
            let color = base_color.linear_multiply(dim);
            let is_selected = self.selected_icao == Some(ac.icao);

            // Selection ring
            if is_selected {
                let ring_color = if stale {
                    Color32::from_rgb(0x40, 0x50, 0x60)
                } else {
                    Color32::from_rgb(0x4E, 0xC9, 0xE0)
                };
                painter.circle_stroke(screen, ICON_R + 4.0, Stroke::new(1.5, ring_color));
            }

            // Aircraft triangle — dark halo first so it's visible over map tiles
            let heading = ac.heading_deg.unwrap_or(0.0);
            let icon_r = if stale { ICON_R * 0.75 } else { ICON_R };
            let halo_pts = aircraft_triangle(screen, heading, icon_r + 2.0);
            painter.add(egui::Shape::convex_polygon(
                halo_pts.to_vec(),
                Color32::from_rgba_premultiplied(0, 0, 0, if stale { 80 } else { 160 }),
                Stroke::NONE,
            ));
            let pts = aircraft_triangle(screen, heading, icon_r);
            let outline = if stale {
                Stroke::new(1.0, Color32::from_rgba_premultiplied(80, 90, 100, 120))
            } else {
                Stroke::new(1.2, Color32::WHITE.linear_multiply(0.9))
            };
            painter.add(egui::Shape::convex_polygon(pts.to_vec(), color, outline));

            // Heading vector (live only)
            if !stale {
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
            // Label background pill so text is readable over any map tile color
            let label_pos = screen + Vec2::new(ICON_R + 4.0, -5.0);
            let alt_pos = screen + Vec2::new(ICON_R + 4.0, 6.0);
            let bg = Color32::from_rgba_premultiplied(0, 0, 0, 140);
            painter.rect_filled(
                egui::Rect::from_min_max(
                    label_pos + Vec2::new(-2.0, -7.0),
                    label_pos + Vec2::new((label.len() as f32 * 6.5).max(30.0), 7.0),
                ),
                egui::Rounding::same(2.0),
                bg,
            );
            painter.text(
                label_pos,
                egui::Align2::LEFT_CENTER,
                &label,
                FontId::proportional(10.0),
                Color32::WHITE,
            );

            // Altitude label
            if let Some(alt) = ac.altitude_ft {
                let alt_str = format!("{}ft", alt / 100 * 100);
                painter.rect_filled(
                    egui::Rect::from_min_max(
                        alt_pos + Vec2::new(-2.0, -6.0),
                        alt_pos + Vec2::new((alt_str.len() as f32 * 6.0).max(28.0), 6.0),
                    ),
                    egui::Rounding::same(2.0),
                    bg,
                );
                painter.text(
                    alt_pos,
                    egui::Align2::LEFT_CENTER,
                    alt_str,
                    FontId::proportional(9.0),
                    Color32::from_rgb(0xA0, 0xD0, 0xF0),
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
        let clip = rect.expand(2.0);

        for i in 0..trail.points.len().saturating_sub(1) {
            let p0 = trail.points[i];
            let p1 = trail.points[i + 1];
            let t0 = trail.times[i];

            let age_secs = now.duration_since(t0).as_secs_f32();
            let alpha = (1.0 - age_secs / 120.0).clamp(0.0, 0.6);
            if alpha < 0.02 { continue; }

            let s0 = geo_to_screen(rect, p0.0, p0.1, self.center_lat, self.center_lon, self.zoom_ppd);
            let s1 = geo_to_screen(rect, p1.0, p1.1, self.center_lat, self.center_lon, self.zoom_ppd);

            // Cohen-Sutherland clip so long off-screen segments don't shoot across the map.
            if let Some((c0, c1)) = clip_segment(s0, s1, clip) {
                let c = base_color.linear_multiply(alpha);
                painter.line_segment([c0, c1], Stroke::new(1.5, c));
            }
        }
    }
}

impl Default for AdsbMapWindow {
    fn default() -> Self {
        Self::new()
    }
}

// ── Cohen-Sutherland line clipping ────────────────────────────────────────────

/// Clip line segment (p0, p1) to the rectangle `clip`.
/// Returns the clipped endpoints, or `None` if the segment is entirely outside.
fn clip_segment(mut p0: Pos2, mut p1: Pos2, clip: Rect) -> Option<(Pos2, Pos2)> {
    const LEFT: u8 = 1; const RIGHT: u8 = 2; const BOTTOM: u8 = 4; const TOP: u8 = 8;
    let code = |p: Pos2| -> u8 {
        let mut c = 0u8;
        if p.x < clip.left()   { c |= LEFT; }
        if p.x > clip.right()  { c |= RIGHT; }
        if p.y < clip.top()    { c |= TOP; }
        if p.y > clip.bottom() { c |= BOTTOM; }
        c
    };
    let mut c0 = code(p0);
    let mut c1 = code(p1);
    loop {
        if c0 | c1 == 0 { return Some((p0, p1)); }  // both inside
        if c0 & c1 != 0 { return None; }             // trivially outside
        let c = if c0 != 0 { c0 } else { c1 };
        let pt = if c & TOP != 0 {
            Pos2::new(p0.x + (p1.x - p0.x) * (clip.top()    - p0.y) / (p1.y - p0.y), clip.top())
        } else if c & BOTTOM != 0 {
            Pos2::new(p0.x + (p1.x - p0.x) * (clip.bottom() - p0.y) / (p1.y - p0.y), clip.bottom())
        } else if c & RIGHT != 0 {
            Pos2::new(clip.right(),  p0.y + (p1.y - p0.y) * (clip.right()  - p0.x) / (p1.x - p0.x))
        } else {
            Pos2::new(clip.left(),   p0.y + (p1.y - p0.y) * (clip.left()   - p0.x) / (p1.x - p0.x))
        };
        if c == c0 { p0 = pt; c0 = code(p0); } else { p1 = pt; c1 = code(p1); }
    }
}

// ── Aircraft detail panel ─────────────────────────────────────────────────────

/// Render the aircraft detail side panel.
/// Returns `true` if the user clicked the deselect button.
fn show_aircraft_detail(
    ui: &mut egui::Ui,
    ac: &AircraftState,
    flight: Option<&FlightLookupState>,
    flight_expanded: &mut bool,
) -> bool {
    let muted = Color32::from_rgb(0x5A, 0x6A, 0x7A);
    let value_color = Color32::from_rgb(0xD8, 0xE8, 0xF0);
    let accent = Color32::from_rgb(0x4E, 0xC9, 0xE0);
    let stale = ac.is_stale();

    // ── ICAO + stale badge ────────────────────────────────────────────────────
    let icao_str = format!("{:06X}", ac.icao);
    ui.horizontal(|ui| {
        let icao_resp = ui.add(
            egui::Label::new(
                RichText::new(&icao_str).monospace().size(18.0).color(accent),
            )
            .sense(Sense::click()),
        );
        if icao_resp.clicked() {
            ui.ctx().copy_text(icao_str.clone());
        }
        icao_resp.on_hover_text("Click to copy ICAO address");

        if stale {
            ui.label(
                RichText::new("LOST").small()
                    .color(Color32::from_rgb(0xE8, 0xA0, 0x40))
                    .background_color(Color32::from_rgba_premultiplied(60, 30, 0, 140)),
            );
        }
    });

    // ── Callsign (ADS-B broadcast or API fallback) ────────────────────────────
    let adsb_cs = ac.callsign.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let api_cs = if let Some(FlightLookupState::Ready(ref info)) = flight {
        info.callsign_api.as_deref()
    } else {
        None
    };
    let callsign = adsb_cs.or(api_cs).unwrap_or("—");
    let cs_color = if stale { muted } else { Color32::WHITE };
    ui.label(RichText::new(callsign).size(15.0).color(cs_color));

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

    ui.add_space(6.0);
    ui.separator();
    ui.add_space(4.0);

    // ── FLIGHT DATA section (collapsible) ─────────────────────────────────────
    ui.horizontal(|ui| {
        // Painted triangle toggle
        let expand_icon_rect = ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover()).0;
        let c = expand_icon_rect.center();
        let tri_pts: Vec<egui::Pos2> = if *flight_expanded {
            vec![egui::pos2(c.x - 4.0, c.y - 2.5), egui::pos2(c.x + 4.0, c.y - 2.5), egui::pos2(c.x, c.y + 3.0)]
        } else {
            vec![egui::pos2(c.x - 2.5, c.y - 4.0), egui::pos2(c.x + 3.0, c.y), egui::pos2(c.x - 2.5, c.y + 4.0)]
        };
        ui.painter().add(egui::Shape::convex_polygon(tri_pts, muted, egui::Stroke::NONE));

        let hdr = ui.add(
            egui::Label::new(RichText::new("FLIGHT DATA").small().color(muted))
                .sense(Sense::click()),
        );
        if hdr.clicked() { *flight_expanded = !*flight_expanded; }

        // Status chip
        match flight {
            None | Some(FlightLookupState::Fetching) => { ui.spinner(); }
            Some(FlightLookupState::Failed) => {
                ui.label(RichText::new("—").small().color(muted));
            }
            Some(FlightLookupState::Ready(_)) => {
                let (dot_r, _) = ui.allocate_exact_size(Vec2::splat(8.0), Sense::hover());
                ui.painter().circle_filled(dot_r.center(), 3.0, Color32::from_rgb(0x73, 0xC9, 0x91));
            }
        }
    });

    if *flight_expanded {
        ui.add_space(4.0);
        match flight {
            None | Some(FlightLookupState::Fetching) => {
                ui.label(RichText::new("  Looking up…").small().color(muted));
            }
            Some(FlightLookupState::Failed) => {
                ui.label(RichText::new("  No data available").small().color(muted));
            }
            Some(FlightLookupState::Ready(info)) => {
                // ── Operator (most important — show prominently) ──────────────
                if let Some(ref op) = info.operator {
                    ui.add_space(2.0);
                    ui.label(RichText::new(op).color(Color32::WHITE).strong());
                }

                // ── Route banner: KORD → KJFK ─────────────────────────────────
                let has_route = info.origin.is_some() || info.destination.is_some();
                if has_route {
                    let origin = info.origin.as_deref().unwrap_or("???");
                    let dest   = info.destination.as_deref().unwrap_or("???");
                    ui.label(
                        RichText::new(format!("{origin}  →  {dest}"))
                            .strong()
                            .color(accent),
                    );
                }

                // ── Registration + readable aircraft type ──────────────────────
                // Prefer human-readable desc ("Airbus A-321") over raw code ("A21N").
                let reg = info.registration.as_deref();
                let type_display = info.aircraft_desc.as_deref()
                    .or(info.aircraft_type.as_deref());
                if reg.is_some() || type_display.is_some() {
                    ui.add_space(2.0);
                    ui.horizontal(|ui| {
                        if let Some(r) = reg {
                            ui.label(RichText::new(r).small().strong().color(accent));
                        }
                        if let (Some(_), Some(t)) = (reg, type_display) {
                            ui.label(RichText::new("·").small().color(muted));
                            ui.label(RichText::new(t).small().color(value_color));
                        } else if let Some(t) = type_display {
                            ui.label(RichText::new(t).small().color(value_color));
                        }
                    });
                }

                if info.operator.is_none() && !has_route && reg.is_none() {
                    ui.label(RichText::new("No data available").small().color(muted));
                }
            }
        }
        ui.add_space(4.0);
    }

    ui.separator();
    ui.add_space(4.0);

    // ── External lookup ───────────────────────────────────────────────────────
    let icao_hex = format!("{:06X}", ac.icao);
    let fa_url    = format!("https://flightaware.com/live/modes/{}/redirect", icao_hex.to_lowercase());
    let adsbx_url = format!("https://globe.adsbexchange.com/?icao={}", icao_hex.to_lowercase());
    let ps_url    = format!("https://www.planespotters.net/hex/{}", icao_hex.to_uppercase());

    let btn_fill = Color32::from_rgb(0x16, 0x20, 0x2E);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().button_padding = egui::Vec2::new(6.0, 3.0);
        if ui.add(egui::Button::new(RichText::new("↗ FlightAware").small().color(accent)).fill(btn_fill))
            .on_hover_text(&fa_url).clicked() { let _ = open::that(&fa_url); }
        if ui.add(egui::Button::new(RichText::new("↗ ADS-B Exch.").small().color(accent)).fill(btn_fill))
            .on_hover_text(&adsbx_url).clicked() { let _ = open::that(&adsbx_url); }
        if ui.add(egui::Button::new(RichText::new("↗ Planespotters").small().color(accent)).fill(btn_fill))
            .on_hover_text(&ps_url).clicked() { let _ = open::that(&ps_url); }
    });

    ui.add_space(6.0);
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

/// Convert an ALL-CAPS string to Title Case (e.g. "AIRBUS A-321" → "Airbus A-321").
fn titlecase(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut cap_next = true;
    for c in s.chars() {
        if c == ' ' || c == '-' {
            result.push(c);
            cap_next = true;
        } else if cap_next {
            result.extend(c.to_uppercase());
            cap_next = false;
        } else {
            result.extend(c.to_lowercase());
        }
    }
    result
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
        let map = AdsbMapWindow::with_viewport(41.5, -81.7, 75.0);
        assert!((map.center_lat() - 41.5).abs() < 1e-9);
        assert!((map.center_lon() - -81.7).abs() < 1e-9);
        assert!((map.zoom_ppd() - 75.0).abs() < 1e-4);
    }

    #[test]
    fn new_defaults_to_cleveland_viewport() {
        let map = AdsbMapWindow::new();
        assert!((map.center_lat() - 41.5).abs() < 1e-9);
        assert!((map.center_lon() - -81.7).abs() < 1e-9);
        assert!((map.zoom_ppd() - 100.0).abs() < 1e-4);
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
