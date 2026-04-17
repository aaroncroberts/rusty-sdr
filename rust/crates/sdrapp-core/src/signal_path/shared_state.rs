//! Shared mutable state between the signal path task and the UI thread.
//!
//! All fields are written by the signal path (or MIDI controller) and read by
//! the egui render loop.  Access is guarded by `parking_lot::RwLock<SharedState>`.

use crate::dsp::FftWindow;

/// Which streams to capture when recording.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RecordingMode {
    /// Demodulated stereo audio → .wav
    #[default]
    AudioOnly,
    /// Raw I/Q complex samples → .iq
    IqOnly,
    /// Both simultaneously.
    Both,
}

impl std::fmt::Display for RecordingMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AudioOnly => write!(f, "Audio only"),
            Self::IqOnly => write!(f, "IQ only"),
            Self::Both => write!(f, "Audio + IQ"),
        }
    }
}

/// Demodulation mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DemodMode {
    /// Wideband FM broadcast (75 kHz deviation, 75 µs de-emphasis)
    #[default]
    Wbfm,
    /// Narrow FM (12.5 kHz deviation, no de-emphasis)
    Nfm,
    /// AM envelope detection
    Am,
    /// Upper sideband SSB
    Usb,
    /// Lower sideband SSB
    Lsb,
    /// Double sideband (both sidebands, suppressed carrier)
    Dsb,
    /// CW (Morse code) — narrow 400–900 Hz bandpass
    Cw,
}

/// A saved frequency bookmark.
#[derive(Debug, Clone)]
pub struct Bookmark {
    pub name: String,
    pub freq_hz: u64,
    pub mode: DemodMode,
    /// Optional group/category name (empty = uncategorised).
    pub category: String,
}

impl Bookmark {
    /// Create a new bookmark with no category.
    pub fn new(name: impl Into<String>, freq_hz: u64, mode: DemodMode) -> Self {
        Self {
            name: name.into(),
            freq_hz,
            mode,
            category: String::new(),
        }
    }

    /// Builder method to attach a category.
    pub fn with_category(mut self, cat: impl Into<String>) -> Self {
        self.category = cat.into();
        self
    }
}

/// A single timestamped entry in the device error log.
#[derive(Clone)]
pub struct ErrorEntry {
    /// Unix timestamp (seconds) for display in the diagnostics panel.
    pub timestamp_secs: u64,
    pub message: String,
}

/// Real-time diagnostics for the active hardware device.
///
/// Written by the device thread; read by the UI diagnostics panel.
/// Stored inside `SharedState` under the same `RwLock` as all other shared state.
#[derive(Default, Clone)]
pub struct DeviceDiagnostics {
    /// Device serial number string (e.g. "RSPdxR2SN12345").
    pub serial: String,
    /// Hardware version byte reported by the API (7 = RSPdx-R2).
    pub hw_ver: u8,
    /// SDRplay API version string (e.g. "3.15").
    pub api_version: String,
    /// Human-readable status string: "Running", "Reconnecting (attempt 2)", etc.
    pub status: String,
    /// Total error count since the source was opened.
    pub error_count: u32,
    /// Ring buffer of the last 20 errors with timestamps.
    pub error_log: std::collections::VecDeque<ErrorEntry>,
    /// Cumulative count of IQ broadcast-channel lag events since the source was opened.
    pub iq_lag_count: u64,
}

impl DeviceDiagnostics {
    /// Append an error message to the ring buffer and increment the counter.
    pub fn push_error(&mut self, msg: impl Into<String>) {
        self.error_count += 1;
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        if self.error_log.len() >= 20 {
            self.error_log.pop_front();
        }
        self.error_log.push_back(ErrorEntry {
            timestamp_secs: ts,
            message: msg.into(),
        });
    }

    /// Reset all diagnostics (called when the device source is torn down).
    pub fn clear(&mut self) {
        *self = DeviceDiagnostics::default();
    }
}

/// Hardware control state (RSPdx-R2).
#[derive(Default, Clone)]
pub struct HardwareState {
    /// LNA gain reduction state (0–9). 0 = max gain, 9 = max attenuation.
    pub lna_state: u8,
    /// IF gain in dBFS (−59 to 0). Used when AGC is disabled.
    pub if_gain_dbfs: i32,
    /// AGC enabled flag.
    pub agc_enabled: bool,
    /// AGC setpoint in dBFS (−60 to 0). Ignored when AGC is disabled.
    pub agc_setpoint_dbfs: i32,
    /// Bias-T power on coax (powers active antennas).
    pub bias_t_enabled: bool,
    /// High Dynamic Range mode (RSPdx-R2 specific).
    pub hdr_mode: bool,
    /// AM broadcast notch filter enabled.
    pub am_notch_enabled: bool,
    /// FM broadcast notch filter enabled.
    pub fm_notch_enabled: bool,
    /// Active antenna port: 0=A, 1=B, 2=C.
    pub antenna_port: u8,
}

/// Demodulation configuration and audio state.
#[derive(Default)]
pub struct DemodState {
    /// Current demodulation mode.
    pub demod_mode: DemodMode,
    /// Current volume (linear).
    pub volume: f32,
    /// NFM squelch threshold in dBFS (e.g. -50.0). Applied only in NFM mode.
    pub squelch_threshold: f32,
    /// NFM channel bandwidth in Hz (12500 or 25000).
    pub nfm_bandwidth_hz: u32,
    /// Whether CTCSS tone squelch is enabled in NFM mode.
    pub ctcss_squelch_enabled: bool,
    /// Whether a CTCSS tone is currently detected (NFM + CTCSS enabled).
    pub ctcss_tone_detected: bool,
    /// Frequency step size for keyboard/scroll tuning (Hz).
    pub tune_step_hz: u64,
    /// Live NFM signal level in dBFS (updated every audio block when in NFM mode).
    /// Used to render the squelch meter in the UI. Range ≈ -120 to 0.
    pub nfm_signal_level_dbfs: f32,
}

/// Frequency scanner state.
#[derive(Default)]
pub struct ScannerState {
    /// Whether the frequency scanner is currently running.
    pub scan_running: bool,
    /// Index of the bookmark the scanner is currently dwelling on.
    pub scan_cursor: usize,
    /// Dwell time in seconds before advancing to the next bookmark.
    pub scan_dwell_secs: f32,
    /// Category filter for scanner (empty = scan all bookmarks).
    pub scan_category: String,
}

/// FFT / spectrum display settings and data.
#[derive(Default)]
pub struct FftDisplayState {
    /// Latest FFT magnitudes (dBFS), length = fft_size.
    pub fft_magnitudes: Vec<f32>,
    /// FFT bin count for spectrum display (512, 1024, 2048, 4096, 8192).
    pub fft_size: usize,
    /// FFT window function applied before transform.
    pub fft_window: FftWindow,
    /// Number of FFT frames to average (exponential moving average). 1 = no averaging.
    pub fft_averaging: u8,
    /// Whether to show the band plan overlay on the spectrum.
    pub band_plan_enabled: bool,
    /// Estimated SNR in the active demod channel (dB). None until computed.
    pub snr_db: Option<f32>,
    /// True when any FFT bin reached ≥ 0 dBFS in the most recent frame —
    /// indicates ADC saturation. Cleared by the UI after a 2-second hold.
    pub fft_clipping_detected: bool,
    /// Whether the spectrum peak-hold line is enabled.
    pub peak_hold_enabled: bool,
    /// Peak-hold decay rate in dB per display frame (default 0.5).
    pub peak_hold_decay_db: f32,
    /// Peak signal level (dBFS) within the current filter passband, updated
    /// each FFT frame by the signal path. Used to drive the S-meter bar.
    /// -120.0 when not running or no signal in passband.
    pub signal_level_dbfs: f32,
}

/// RDS (Radio Data System) decoded state (WBFM only).
#[derive(Default)]
pub struct RdsState {
    /// Whether a stereo pilot tone is currently detected (WBFM only).
    pub is_stereo: bool,
    /// RDS Programme Service name, if decoded (WBFM only).
    pub ps_name: Option<String>,
    /// RDS Programme Type code (0-31).
    pub pty: Option<u8>,
    /// RDS Traffic Programme flag.
    pub tp: bool,
    /// RDS Traffic Announcement flag.
    pub ta: bool,
    /// RDS RadioText (up to 64 chars).
    pub rt: Option<String>,
}

/// Shared display state written by the signal path, read by the UI.
#[derive(Default)]
pub struct SharedState {
    // ── Display / core ────────────────────────────────────────────────
    /// Center frequency (Hz) as reported by the source.
    pub center_freq_hz: u64,
    /// Sample rate (sps) as reported by the source.
    pub sample_rate_sps: u32,
    /// Whether the signal path is currently running.
    pub is_running: bool,
    /// Whether recording is active.
    pub is_recording: bool,
    /// Active source name — "Demo Mode" when running on the test signal source.
    pub source_name: Option<String>,
    /// MIDI device name when connected, None otherwise.
    pub midi_device: Option<String>,
    /// Active MIDI page index.
    pub midi_page: usize,
    /// Audio buffer fill fraction [0.0, 1.0] — written by audio sink.
    pub audio_buffer_fill: f32,
    /// Cumulative count of audio frames dropped due to try_send backpressure.
    /// Reset by the UI via a direct write.
    pub audio_frames_dropped: u64,
    /// Spectrum zoom level: 1.0 = full bandwidth, 0.1 = 10× zoom.
    pub zoom_level: f32,
    /// Waterfall scroll speed multiplier (1.0 = normal).
    pub waterfall_speed: f32,
    /// Saved frequency bookmarks.
    pub bookmarks: Vec<Bookmark>,
    /// Index of the currently selected bookmark (for MIDI navigation).
    pub bookmark_cursor: usize,
    /// Whether the help panel is open.
    pub help_panel_open: bool,
    /// Active recording mode (what to capture when recording starts).
    pub recording_mode: RecordingMode,
    /// Last recorder error message (disk full, permission denied, etc.).
    /// Cleared when a new recording starts successfully.
    pub recorder_error: Option<String>,
    /// Peak audio level in dBFS (≤ 0) from the most recent WAV frame batch.
    /// Updated ≈10×/s while recording; reset to -60.0 when recording stops.
    pub recording_peak_dbfs: f32,
    /// RMS audio level in dBFS (≤ 0) from the most recent WAV frame batch.
    /// Updated ≈10×/s while recording; reset to -60.0 when recording stops.
    pub recording_rms_dbfs: f32,
    // ── MIDI Learn ────────────────────────────────────────────────────
    /// If Some(knob_id), the next incoming MIDI CC will be bound to that knob.
    pub midi_learn_target: Option<String>,
    /// Learned CC bindings: MIDI CC number → knob ID string.
    /// Set by the MIDI controller; read by the MIDI dispatcher and KnobWidget.
    pub midi_cc_to_knob: std::collections::HashMap<u8, String>,
    /// Set by the MIDI mapper window when the user clicks a CC control, waiting for
    /// the user to click a UI knob to complete the binding.  Cleared on bind or Escape.
    pub midi_map_pending: Option<u8>,
    /// Scheduled recording: seconds until start (0 = start now, None = not scheduled).
    pub scheduled_record_delay_secs: Option<u64>,
    /// Scheduled recording duration in seconds.
    pub scheduled_record_duration_secs: u32,
    // ── Sub-structs ───────────────────────────────────────────────────
    pub hardware: HardwareState,
    pub demod: DemodState,
    pub scanner: ScannerState,
    pub fft: FftDisplayState,
    pub rds: RdsState,
    /// Real-time diagnostics for the hardware device (empty in demo mode).
    pub device_diagnostics: DeviceDiagnostics,
}

impl SharedState {
    /// Create a new SharedState with sensible defaults.
    pub fn new() -> Self {
        use super::FFT_SIZE;
        Self {
            zoom_level: 1.0,
            waterfall_speed: 1.0,
            bookmarks: vec![Bookmark::new("BBC Radio 4", 93_500_000, DemodMode::Wbfm)],
            demod: DemodState {
                volume: 0.8,
                squelch_threshold: -50.0,
                nfm_bandwidth_hz: 12_500,
                tune_step_hz: 100_000,
                ..Default::default()
            },
            scanner: ScannerState {
                scan_dwell_secs: 2.0,
                ..Default::default()
            },
            fft: FftDisplayState {
                fft_magnitudes: vec![-120.0; FFT_SIZE],
                fft_size: FFT_SIZE,
                fft_window: FftWindow::Hann,
                fft_averaging: 4,
                peak_hold_decay_db: 0.5,
                ..Default::default()
            },
            recording_peak_dbfs: -60.0,
            recording_rms_dbfs: -60.0,
            ..Default::default()
        }
    }
}
