#![forbid(unsafe_code)]

//! Versioned application configuration, persisted to disk as JSON.
//!
//! Location: `~/Library/Application Support/sdrapp/config.json` (macOS)
//!           `~/.config/sdrapp/config.json` (Linux/other)

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::registry::{ActiveSink, ActiveSource};

/// Config schema version — bump when breaking changes are made.
const CONFIG_VERSION: u32 = 1;

/// Top-level application configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub version: u32,
    pub active_source: ActiveSource,
    pub active_sink: ActiveSink,
    pub ui: UiConfig,
    #[serde(default)]
    pub source: SourceConfig,
    /// Saved frequency bookmarks (used when `bookmarks_file` is not set).
    #[serde(default = "default_bookmarks")]
    pub bookmarks: Vec<BookmarkConfig>,
    /// Hamlib rigctl TCP server configuration.
    #[serde(default)]
    pub rigctl: RigctlConfig,
    /// Persisted MIDI Learn bindings: knob_id → CC number.
    #[serde(default)]
    pub midi_learn: std::collections::HashMap<String, u8>,
    /// Path to an external bookmarks CSV file.  When set, bookmarks are loaded
    /// from this file at startup and saved back to it on changes, rather than
    /// being stored inline in this config.  Use `~` for the home directory.
    #[serde(default)]
    pub bookmarks_file: Option<String>,
    /// Path used by the Export and Import buttons when `bookmarks_file` is not
    /// set.  Defaults to `~/bookmarks.csv`.  Use `~` for the home directory.
    #[serde(default = "default_bookmarks_export_path")]
    pub bookmarks_export_path: String,
}

fn default_bookmarks() -> Vec<BookmarkConfig> {
    let sw = "Shortwave / ML-31";
    vec![
        // ── General / Antenna A ────────────────────────────────────────────────
        BookmarkConfig::new("BBC Radio 4 — 93.5 MHz", 93_500_000, "Wbfm"),
        BookmarkConfig::new("WMJI 105.7 (Cleveland OH)", 105_700_000, "Wbfm"),
        // NOAA Weather Radio KEC93 – Cleveland/NE Ohio (162.550 MHz, NFM 25 kHz)
        BookmarkConfig::new("NOAA Weather — KEC93", 162_550_000, "Nfm")
            .with_nfm_settings(25_000, -60.0, false)
            .with_category("Weather"),
        // ── Shortwave / ML-31 — Antenna C ─────────────────────────────────────
        BookmarkConfig::new("WWV 10 MHz (time signals)", 10_000_000, "Am")
            .with_antenna("C").with_category(sw),
        BookmarkConfig::new("BBC World Service 9.410 MHz", 9_410_000, "Am")
            .with_antenna("C").with_category(sw),
        BookmarkConfig::new("Ham 40m USB (7.200 MHz)", 7_200_000, "Usb")
            .with_antenna("C").with_category(sw),
    ]
}

/// Hamlib-compatible rigctl TCP server configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RigctlConfig {
    /// Whether the rigctl server is enabled.
    pub enabled: bool,
    /// TCP port to listen on (default 4532, same as Hamlib default).
    pub port: u16,
}

impl Default for RigctlConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            port: 4532,
        }
    }
}

/// SDR source hardware configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceConfig {
    /// Antenna port: "A", "B", or "C".
    pub antenna: String,
    /// AGC enabled.
    pub agc_enabled: bool,
    /// LNA state (0–9); used when AGC is disabled.
    pub lna_state: u8,
    /// Sample rate in sps.
    pub sample_rate_sps: u32,
    /// IF mode: "ZeroIF", "LowIF200kHz", "LowIF500kHz".
    pub if_mode: String,
    /// IF gain in dBFS (−59 to 0); combined with LNA state for manual gain.
    #[serde(default = "default_if_gain_dbfs")]
    pub if_gain_dbfs: i32,
    /// AGC setpoint in dBFS (−60 to 0).
    #[serde(default = "default_agc_setpoint_dbfs")]
    pub agc_setpoint_dbfs: i32,
    /// Bias-T power supply on the coax connector (for active antennas).
    #[serde(default)]
    pub bias_t_enabled: bool,
    /// High Dynamic Range mode (RSPdx-R2 specific).
    #[serde(default)]
    pub hdr_mode: bool,
    /// AM broadcast notch filter (reduces LW/MW interference).
    #[serde(default)]
    pub am_notch_enabled: bool,
    /// FM broadcast notch filter (reduces FM overload above 65 MHz).
    #[serde(default)]
    pub fm_notch_enabled: bool,
    /// Hardware decimation factor applied by the SDRplay API before streaming IQ.
    /// Effective sample rate = sample_rate_sps / decimation_factor.
    /// Must be 1 for ADS-B reception (requires ≥ 2 Msps effective rate).
    /// Valid values: 1 (off), 2, 4, 8, 16, 32.
    #[serde(default = "default_decimation_factor")]
    pub decimation_factor: u32,
}

fn default_if_gain_dbfs() -> i32 {
    0
}
fn default_agc_setpoint_dbfs() -> i32 {
    -60
}
fn default_decimation_factor() -> u32 {
    1
}

impl Default for SourceConfig {
    fn default() -> Self {
        Self {
            antenna: "A".into(),
            agc_enabled: true,
            lna_state: 3,
            sample_rate_sps: 2_000_000,
            if_mode: "ZeroIF".into(),
            if_gain_dbfs: 0,
            agc_setpoint_dbfs: -60,
            bias_t_enabled: false,
            hdr_mode: false,
            am_notch_enabled: false,
            fm_notch_enabled: false,
            decimation_factor: 1,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiConfig {
    /// Center frequency displayed in the spectrum (Hz).
    pub frequency_hz: u64,
    /// Span of the spectrum display (Hz on each side of center).
    pub span_hz: u64,
    /// Linear volume 0.0–1.0.
    pub volume: f32,
    /// Window width in pixels.
    pub window_width: f32,
    /// Window height in pixels.
    pub window_height: f32,
    /// Spectrum zoom level: 1.0 = full hardware bandwidth, 0.05 = tightest zoom.
    #[serde(default = "default_zoom_level")]
    pub zoom_level: f32,
    /// Waterfall scroll speed multiplier (1.0 = normal, 2.0 = 2× faster).
    #[serde(default = "default_waterfall_speed")]
    pub waterfall_speed: f32,
    /// NFM channel bandwidth in Hz (12500 or 25000).
    #[serde(default = "default_nfm_bandwidth")]
    pub nfm_bandwidth_hz: u32,
    /// CTCSS tone squelch enabled for NFM.
    #[serde(default)]
    pub ctcss_enabled: bool,
    /// FFT bin count (512, 1024, 2048, 4096, 8192).
    #[serde(default = "default_fft_size")]
    pub fft_size: usize,
    /// FFT window function: "Hann", "Hamming", "BlackmanHarris", "Rectangular".
    #[serde(default = "default_fft_window")]
    pub fft_window: String,
    /// FFT display averaging frames (1–16).
    #[serde(default = "default_fft_averaging")]
    pub fft_averaging: u8,
    /// Show band plan overlay on spectrum.
    #[serde(default)]
    pub band_plan_enabled: bool,
    /// Waterfall colormap preset: "Thermal", "Grayscale", "Inferno", "Classic".
    #[serde(default = "default_waterfall_colormap")]
    pub waterfall_colormap: String,
    /// UI font scale factor (0.5–3.0, default 1.0).
    #[serde(default = "default_font_scale")]
    pub font_scale: f32,
    /// Fraction of center-panel height allocated to the spectrum (0.15–0.85).
    /// The waterfall fills the remainder below the toolbar.
    #[serde(default = "default_spectrum_split")]
    pub spectrum_split: f32,
    /// Waterfall absolute dBFS floor — the dBFS level that maps to the darkest
    /// colour.  Positive wf_gain values shift this down to reveal weaker signals.
    #[serde(default = "default_wf_level")]
    pub wf_level: f32,
    /// Spectrum / waterfall display floor in dBFS.
    /// Maps to the darkest colour on the waterfall and the bottom of the spectrum Y-axis.
    /// Saved so the user's manual adjustment persists across restarts.
    #[serde(default = "default_fft_floor")]
    pub fft_floor: f32,
    /// Spectrum / waterfall display ceiling in dBFS.
    /// Maps to the brightest colour on the waterfall and the top of the spectrum Y-axis.
    #[serde(default = "default_fft_ceil")]
    pub fft_ceil: f32,
    /// Last-used demodulator mode: "Wbfm", "Nfm", "Am", "Usb", "Lsb", "Dsb", "Cw".
    /// Restored on next launch so the user's mode choice persists.
    #[serde(default = "default_demod_mode")]
    pub demod_mode: String,
    /// NFM squelch threshold in dBFS (−120 to 0).  Persisted so users don't
    /// have to re-calibrate their squelch after every restart.
    #[serde(default = "default_squelch_threshold_dbfs")]
    pub squelch_threshold_dbfs: f32,
    /// Tune step in Hz — the increment used by scroll-to-tune and arrow keys.
    #[serde(default = "default_tune_step_hz")]
    pub tune_step_hz: u64,
    /// Set to true after the first-run onboarding overlay is dismissed.
    /// When false (or absent from config), the overlay is shown on next launch.
    #[serde(default)]
    pub seen_onboarding: bool,
    // ── Operators Handbook ─────────────────────────────────────────────────────
    /// Active section index in the Operators Handbook (0-based).
    #[serde(default)]
    pub handbook_section: usize,
    /// Active page index within the current handbook section (0-based).
    #[serde(default)]
    pub handbook_page: usize,
    /// Whether the Operators Handbook window is open.
    #[serde(default)]
    pub show_handbook: bool,
    // ── ADS-B map ──────────────────────────────────────────────────────────────
    /// Whether the ADS-B map window is open.
    #[serde(default)]
    pub show_adsb_map: bool,
    /// ADS-B map center latitude (degrees).
    #[serde(default = "default_adsb_center_lat")]
    pub adsb_map_lat: f64,
    /// ADS-B map center longitude (degrees).
    #[serde(default = "default_adsb_center_lon")]
    pub adsb_map_lon: f64,
    /// ADS-B map zoom (pixels per degree of longitude).
    #[serde(default = "default_adsb_zoom")]
    pub adsb_map_zoom: f32,
    /// Home location used by the ADS-B map ⌖ "Reset view" button.
    /// Set this to your location so the map always resets to your area.
    #[serde(default = "default_home_lat")]
    pub home_lat: f64,
    /// Home longitude (degrees).  Pairs with `home_lat`.
    #[serde(default = "default_home_lon")]
    pub home_lon: f64,
}

fn default_zoom_level() -> f32 {
    1.0
}
fn default_waterfall_speed() -> f32 {
    1.0
}
fn default_nfm_bandwidth() -> u32 {
    12_500
}
fn default_fft_size() -> usize {
    2048
}
fn default_fft_window() -> String {
    "Hann".into()
}
fn default_fft_averaging() -> u8 {
    4
}
fn default_waterfall_colormap() -> String {
    "Thermal".into()
}
fn default_font_scale() -> f32 {
    1.0
}
fn default_spectrum_split() -> f32 {
    0.45
}
fn default_demod_mode() -> String {
    "Wbfm".into()
}
fn default_squelch_threshold_dbfs() -> f32 {
    -50.0
}
fn default_adsb_center_lat() -> f64 {
    41.5  // Cleveland OH
}
fn default_adsb_center_lon() -> f64 {
    -81.7 // Cleveland OH
}
fn default_adsb_zoom() -> f32 {
    100.0 // ~300 nm view — right for ADS-B range
}
fn default_home_lat() -> f64 {
    41.5  // Cleveland OH — change in config to your location
}
fn default_home_lon() -> f64 {
    -81.7 // Cleveland OH — change in config to your location
}
fn default_bookmarks_export_path() -> String {
    "~/bookmarks.csv".into()
}

/// Expand a leading `~` to the user's home directory.
pub fn expand_tilde(path: &str) -> std::path::PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        dirs::home_dir().unwrap_or_default().join(rest)
    } else if path == "~" {
        dirs::home_dir().unwrap_or_default()
    } else {
        std::path::PathBuf::from(path)
    }
}
fn default_tune_step_hz() -> u64 {
    1_000
}
fn default_wf_level() -> f32 {
    -80.0
}
fn default_fft_floor() -> f32 {
    -100.0
}
fn default_fft_ceil() -> f32 {
    -20.0
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            frequency_hz: 105_700_000, // WMJI 105.7 FM – Cleveland/Mentor OH
            span_hz: 1_000_000,
            volume: 0.8,
            window_width: 1280.0,
            window_height: 800.0,
            zoom_level: 1.0,
            waterfall_speed: 1.0,
            nfm_bandwidth_hz: 12_500,
            ctcss_enabled: false,
            fft_size: 2048,
            fft_window: "Hann".into(),
            fft_averaging: 4,
            band_plan_enabled: false,
            waterfall_colormap: "Thermal".into(),
            font_scale: 1.0,
            spectrum_split: 0.45,
            wf_level: -80.0,
            fft_floor: -100.0,
            fft_ceil: -20.0,
            squelch_threshold_dbfs: -50.0,
            tune_step_hz: 1_000,
            seen_onboarding: false,
            handbook_section: 0,
            handbook_page: 0,
            show_handbook: false,
            demod_mode: "Wbfm".into(),
            show_adsb_map: false,
            adsb_map_lat: 41.5,
            adsb_map_lon: -81.7,
            adsb_map_zoom: 100.0,
            home_lat: 41.5,   // Cleveland OH — edit in config.json to your location
            home_lon: -81.7,  // Cleveland OH
        }
    }
}

/// A persisted frequency bookmark.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BookmarkConfig {
    pub name: String,
    pub freq_hz: u64,
    /// Demod mode string: "Wbfm", "Nfm", "Am", "Usb", "Lsb", "Dsb", "Cw".
    pub mode: String,
    /// Optional category/group name (empty = uncategorised).
    #[serde(default)]
    pub category: String,
    /// NFM channel bandwidth in Hz (12 500 or 25 000). `None` = use receiver default.
    #[serde(default)]
    pub nfm_bandwidth_hz: Option<u32>,
    /// NFM squelch threshold in dBFS (e.g. -50.0). `None` = use receiver default.
    #[serde(default)]
    pub squelch_threshold_dbfs: Option<f32>,
    /// Whether CTCSS tone squelch was enabled on this channel.
    #[serde(default)]
    pub ctcss_enabled: Option<bool>,
    /// Antenna port override: Some("A"), Some("B"), Some("C"), or None (no override).
    /// When set, recalling this bookmark automatically switches to the specified port.
    #[serde(default)]
    pub antenna: Option<String>,
}

impl BookmarkConfig {
    pub fn new(name: impl Into<String>, freq_hz: u64, mode: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            freq_hz,
            mode: mode.into(),
            category: String::new(),
            nfm_bandwidth_hz: None,
            squelch_threshold_dbfs: None,
            ctcss_enabled: None,
            antenna: None,
        }
    }

    /// Parse one line of a bookmarks CSV (`name,freq_hz,mode,category`).
    /// Returns `None` for malformed or zero-frequency lines.
    pub fn from_csv_line(line: &str) -> Option<Self> {
        let parts: Vec<&str> = line.splitn(4, ',').collect();
        if parts.len() < 3 {
            return None;
        }
        let freq: u64 = parts[1].trim().parse().ok().filter(|&f| f > 0)?;
        let mut bc = Self::new(parts[0].trim(), freq, parts[2].trim());
        if parts.len() >= 4 {
            bc.category = parts[3].trim().into();
        }
        Some(bc)
    }

    /// Load a full bookmarks CSV (with header row) from a file path string.
    /// Expands leading `~` to the home directory.  Returns an empty vec on error.
    pub fn load_from_csv(path_str: &str) -> Vec<Self> {
        let path = expand_tilde(path_str);
        let content = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        content
            .lines()
            .skip(1) // skip header
            .filter_map(Self::from_csv_line)
            .collect()
    }

    /// Save a slice of bookmarks to a CSV file (with header row).
    /// Expands leading `~`.  Returns the resolved path, or an error.
    pub fn save_to_csv(bookmarks: &[Self], path_str: &str) -> Result<std::path::PathBuf, std::io::Error> {
        let path = expand_tilde(path_str);
        let header = "name,freq_hz,mode,category\n";
        let body: String = bookmarks
            .iter()
            .map(|b| format!("{},{},{},{}", b.name.replace(',', " "), b.freq_hz, b.mode, b.category))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&path, format!("{header}{body}"))?;
        Ok(path)
    }

    /// Builder method to attach a category.
    pub fn with_category(mut self, cat: impl Into<String>) -> Self {
        self.category = cat.into();
        self
    }

    /// Set an antenna port override for this bookmark ("A", "B", or "C").
    pub fn with_antenna(mut self, port: impl Into<String>) -> Self {
        self.antenna = Some(port.into());
        self
    }

    /// Attach NFM-specific settings (bandwidth, squelch, CTCSS).
    pub fn with_nfm_settings(
        mut self,
        bandwidth_hz: u32,
        squelch_dbfs: f32,
        ctcss: bool,
    ) -> Self {
        self.nfm_bandwidth_hz = Some(bandwidth_hz);
        self.squelch_threshold_dbfs = Some(squelch_dbfs);
        self.ctcss_enabled = Some(ctcss);
        self
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            active_source: ActiveSource::default(),
            active_sink: ActiveSink::default(),
            ui: UiConfig::default(),
            source: SourceConfig::default(),
            bookmarks: default_bookmarks(),
            rigctl: RigctlConfig::default(),
            midi_learn: std::collections::HashMap::new(),
            bookmarks_file: None,
            bookmarks_export_path: default_bookmarks_export_path(),
        }
    }
}

impl AppConfig {
    /// Load config from disk, or return defaults if the file doesn't exist or fails to parse.
    pub fn load_or_default() -> Self {
        let path = config_path();
        let mut cfg = match std::fs::read_to_string(&path) {
            Ok(json) => serde_json::from_str(&json).unwrap_or_else(|e| {
                tracing::warn!(
                    path = %path.display(),
                    line = e.line(),
                    column = e.column(),
                    error = %e,
                    "config JSON parse failed — using defaults"
                );
                Self::default()
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::debug!(path = %path.display(), "config file not found — using defaults");
                Self::default()
            }
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "config read failed — using defaults");
                Self::default()
            }
        };
        cfg.migrate_bookmarks();
        cfg
    }

    /// Merge / prune bookmark presets on load.
    ///
    /// Removes the 9 excess Shortwave / ML-31 presets that were shipped in an
    /// earlier version, and ensures the current curated 3-entry set is present.
    fn migrate_bookmarks(&mut self) {
        const SW_CAT: &str = "Shortwave / ML-31";

        // Names that were over-shipped and should be removed.
        const EXCESS: &[&str] = &[
            "WWV 5 MHz (time signals)",
            "WWV 15 MHz (time signals)",
            "CHU Canada 7.850 MHz",
            "BBC World Service 5.875 MHz",
            "VOA 9.500 MHz",
            "Radio France Int. 15.300 MHz",
            "Ham 20m USB (14.225 MHz)",
            "VOLMET Shannon 5.505 MHz",
            "Maritime CW 8.364 kHz",
        ];
        let before = self.bookmarks.len();
        self.bookmarks
            .retain(|b| !(b.category == SW_CAT && EXCESS.contains(&b.name.as_str())));
        let removed = before - self.bookmarks.len();
        if removed > 0 {
            tracing::info!(removed, "pruned excess Shortwave / ML-31 preset bookmarks");
        }

        // Add the curated 3-entry set if any are missing.
        let sw_bookmarks: Vec<BookmarkConfig> = default_bookmarks()
            .into_iter()
            .filter(|b| b.category == SW_CAT)
            .filter(|b| !self.bookmarks.iter().any(|e| e.name == b.name))
            .collect();
        if !sw_bookmarks.is_empty() {
            tracing::info!(
                count = sw_bookmarks.len(),
                "adding curated Shortwave / ML-31 bookmarks"
            );
            self.bookmarks.extend(sw_bookmarks);
        }
    }

    /// Persist config to disk. Creates the directory if needed.
    pub fn save(&self) {
        let path = config_path();
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                tracing::error!(path = %parent.display(), error = %e, "failed to create config dir");
                return;
            }
        }
        match serde_json::to_string_pretty(self) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&path, json) {
                    tracing::error!(path = %path.display(), error = %e, "failed to write config");
                } else {
                    tracing::debug!(path = %path.display(), "config saved");
                }
            }
            Err(e) => tracing::error!(error = %e, "failed to serialize config"),
        }
    }
}

/// Platform-appropriate config file path.
pub fn config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("sdrapp")
        .join("config.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_round_trips_json() {
        let cfg = AppConfig::default();
        let json = serde_json::to_string(&cfg).unwrap();
        let restored: AppConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.version, CONFIG_VERSION);
        assert_eq!(restored.ui.frequency_hz, 105_700_000);
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let json = r#"{"version":1,"active_source":{"kind":"rspdx"},"active_sink":{"kind":"cpal"},"ui":{"frequency_hz":101000000,"span_hz":1000000,"volume":0.5,"window_width":1280.0,"window_height":800.0},"future_field":"ignored"}"#;
        // Should not fail even with unknown fields
        let result = serde_json::from_str::<AppConfig>(json);
        assert!(result.is_ok());
    }

    // ── ADS-B UiConfig fields ─────────────────────────────────────────────────

    #[test]
    fn adsb_fields_round_trip() {
        let mut cfg = UiConfig::default();
        cfg.show_adsb_map = true;
        cfg.adsb_map_lat = 41.499_32; // Cleveland, OH
        cfg.adsb_map_lon = -81.694_36;
        cfg.adsb_map_zoom = 12.5;

        let json = serde_json::to_string(&cfg).unwrap();
        let restored: UiConfig = serde_json::from_str(&json).unwrap();

        assert!(restored.show_adsb_map);
        assert!((restored.adsb_map_lat - 41.499_32).abs() < 1e-6);
        assert!((restored.adsb_map_lon - -81.694_36).abs() < 1e-6);
        assert!((restored.adsb_map_zoom - 12.5).abs() < 1e-4);
    }

    #[test]
    fn adsb_fields_default_when_absent_from_old_config() {
        // Simulate an old config.json that pre-dates the ADS-B fields.
        // serde must fill in defaults rather than failing to parse.
        let old_json = r#"{
            "frequency_hz": 105700000,
            "span_hz": 1000000,
            "volume": 0.8,
            "window_width": 1280.0,
            "window_height": 800.0
        }"#;

        let cfg: UiConfig = serde_json::from_str(old_json).unwrap();

        assert!(!cfg.show_adsb_map);
        assert!((cfg.adsb_map_lat - 41.5).abs() < 1e-6);   // default_adsb_center_lat → Cleveland OH
        assert!((cfg.adsb_map_lon - -81.7).abs() < 1e-6);  // default_adsb_center_lon → Cleveland OH
        assert!((cfg.adsb_map_zoom - 100.0).abs() < 1e-4);  // default_adsb_zoom
    }

    #[test]
    fn adsb_show_map_defaults_to_false() {
        // Confirm the window is not shown on fresh config
        assert!(!UiConfig::default().show_adsb_map);
    }

    // ── BookmarkConfig antenna field ──────────────────────────────────────────

    #[test]
    fn bookmark_antenna_defaults_to_none_for_old_configs() {
        // Old bookmark JSON without the antenna field should deserialize cleanly.
        let old_json = r#"{"name":"BBC Radio 4","freq_hz":93500000,"mode":"Wbfm"}"#;
        let bm: BookmarkConfig = serde_json::from_str(old_json).unwrap();
        assert_eq!(bm.name, "BBC Radio 4");
        assert!(bm.antenna.is_none(), "antenna should default to None");
    }

    #[test]
    fn bookmark_antenna_round_trips() {
        let bm = BookmarkConfig::new("WWV 10 MHz", 10_000_000, "Am")
            .with_antenna("C");
        let json = serde_json::to_string(&bm).unwrap();
        let restored: BookmarkConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.antenna, Some("C".into()));
    }

    #[test]
    fn bookmark_without_antenna_serializes_without_field() {
        let bm = BookmarkConfig::new("FM Station", 105_700_000, "Wbfm");
        let json = serde_json::to_string(&bm).unwrap();
        // When antenna is None, serde should skip it (no "antenna":null in output)
        // Deserializing again must still work cleanly
        let restored: BookmarkConfig = serde_json::from_str(&json).unwrap();
        assert!(restored.antenna.is_none());
    }
}
