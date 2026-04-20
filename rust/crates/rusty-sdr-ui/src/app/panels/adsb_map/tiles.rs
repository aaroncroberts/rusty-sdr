//! OSM tile types, fetch helpers, and the AircraftTrail position buffer.

use std::io::Read;
use std::time::Instant;

use crossbeam_channel::Sender;
use egui::{Color32, ColorImage};

use super::geo::MAX_TRAIL;

// ── OSM tile types ─────────────────────────────────────────────────────────────

/// (zoom_level, tile_x, tile_y)
pub(super) type TileKey = (u8, i32, i32);

pub(super) struct TileFetchResult {
    pub(super) key: TileKey,
    pub(super) image: Option<ColorImage>,
}

// ── OSM tile helpers ───────────────────────────────────────────────────────────

/// Convert zoom_ppd (pixels/degree longitude) to an OSM tile zoom level.
/// Formula: `2^z = zoom_ppd * 360 / 256`, clamped to [0, 12].
pub(super) fn osm_zoom(zoom_ppd: f32) -> u8 {
    let z = (zoom_ppd * 360.0 / 256.0).log2().round() as i32;
    z.clamp(0, 12) as u8
}

/// Return the NW corner (lat, lon) of the OSM tile at (z, x, y).
pub(super) fn tile_nw(z: u8, x: i32, y: i32) -> (f64, f64) {
    let n = 2.0f64.powi(z as i32);
    let lon = x as f64 / n * 360.0 - 180.0;
    let lat = (std::f64::consts::PI * (1.0 - 2.0 * y as f64 / n)).sinh().atan().to_degrees();
    (lat, lon)
}

/// Convert a (lat, lon) to the OSM tile (x, y) at zoom level z.
pub(super) fn lat_lon_to_tile_xy(lat: f64, lon: f64, z: u8) -> (i32, i32) {
    let n = 2.0f64.powi(z as i32);
    let x = ((lon + 180.0) / 360.0 * n).floor() as i32;
    let lat_rad = lat.clamp(-85.05, 85.05).to_radians();
    let y = ((1.0 - (lat_rad.tan() + 1.0 / lat_rad.cos()).ln() / std::f64::consts::PI) / 2.0 * n)
        .floor() as i32;
    (x, y)
}

/// Spawn a background thread to fetch one OSM tile and send the result back.
pub(super) fn fetch_tile_async(tx: Sender<TileFetchResult>, z: u8, x: i32, y: i32) {
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
    pub(super) times: Vec<Instant>,
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
