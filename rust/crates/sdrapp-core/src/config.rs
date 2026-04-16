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
    /// Saved frequency bookmarks.
    #[serde(default = "default_bookmarks")]
    pub bookmarks: Vec<BookmarkConfig>,
    /// Hamlib rigctl TCP server configuration.
    #[serde(default)]
    pub rigctl: RigctlConfig,
    /// Persisted MIDI Learn bindings: knob_id → CC number.
    #[serde(default)]
    pub midi_learn: std::collections::HashMap<String, u8>,
}

fn default_bookmarks() -> Vec<BookmarkConfig> {
    vec![BookmarkConfig::new("WMJI 105.7 (Cleveland OH)", 105_700_000, "Wbfm")]
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
}

fn default_if_gain_dbfs() -> i32 {
    0
}
fn default_agc_setpoint_dbfs() -> i32 {
    -60
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
fn default_wf_level() -> f32 {
    -80.0
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
            seen_onboarding: false,
            handbook_section: 0,
            handbook_page: 0,
            show_handbook: false,
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
}

impl BookmarkConfig {
    pub fn new(name: impl Into<String>, freq_hz: u64, mode: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            freq_hz,
            mode: mode.into(),
            category: String::new(),
        }
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
            bookmarks: vec![BookmarkConfig::new("WMJI 105.7 (Cleveland OH)", 105_700_000, "Wbfm")],
            rigctl: RigctlConfig::default(),
            midi_learn: std::collections::HashMap::new(),
        }
    }
}

impl AppConfig {
    /// Load config from disk, or return defaults if the file doesn't exist or fails to parse.
    pub fn load_or_default() -> Self {
        let path = config_path();
        match std::fs::read_to_string(&path) {
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
}
