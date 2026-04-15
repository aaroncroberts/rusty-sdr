#![forbid(unsafe_code)]

//! Signal path: wires source → IQ frontend (FFT) → audio sink.
//!
//! Topology:
//!   Source ──broadcast──► IQ Frontend ──┬──► FFT (spectrum display)
//!                                       └──► Audio Sink (cpal)
//!                                       └──► Recorder
//!
//! Communication:
//!   - Source → IQ frontend: broadcast channel (Arc<[IqSample]> batches)
//!   - IQ frontend → spectrum: writes into SharedState.fft_magnitudes (RwLock)
//!   - IQ frontend → audio/record: mpsc channels (Arc<[StereoFrame]> batches)
//!
//! The IQ frontend is intentionally simple right now (no decimation, no VFO shift).
//! It will grow as we add demodulation.

use parking_lot::RwLock;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;

use rustfft::num_complex::Complex;

use crate::dsp::{AmDemodulator, AudioBandpass, CtcssDetector, CwDemodulator, FmDemodulator, FftProcessor, RdsDecoder, Squelch, SsbDemodulator, SsbMode, StereoFmDecoder, Volume};
use crate::sample::{IqSample, StereoFrame};

const FFT_SIZE: usize = 2048;
const AUDIO_FRAME_SIZE: usize = 1024;

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
    pub fn new(name: impl Into<String>, freq_hz: u64, mode: DemodMode) -> Self {
        Self { name: name.into(), freq_hz, mode, category: String::new() }
    }

    pub fn with_category(mut self, cat: impl Into<String>) -> Self {
        self.category = cat.into();
        self
    }
}

/// Hardware control state (RSPdx-R2).
#[derive(Default)]
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
    pub fft_window: crate::dsp::FftWindow,
    /// Number of FFT frames to average (exponential moving average). 1 = no averaging.
    pub fft_averaging: u8,
    /// Whether to show the band plan overlay on the spectrum.
    pub band_plan_enabled: bool,
    /// Estimated SNR in the active demod channel (dB). None until computed.
    pub snr_db: Option<f32>,
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
}

impl SharedState {
    pub fn new() -> Self {
        Self {
            zoom_level: 1.0,
            waterfall_speed: 1.0,
            bookmarks: vec![
                Bookmark::new("BBC Radio 4", 93_500_000, DemodMode::Wbfm),
            ],
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
                fft_window: crate::dsp::FftWindow::Hann,
                fft_averaging: 4,
                ..Default::default()
            },
            ..Default::default()
        }
    }
}

/// Commands forwarded from the signal path to the hardware device thread.
///
/// The signal path holds an optional `crossbeam_channel::Sender<HardwareCommand>`.
/// When a hardware-related [`SignalPathCommand`] is received, the signal path
/// updates [`SharedState`] and forwards a `HardwareCommand` to the device.
#[derive(Debug, Clone)]
pub enum HardwareCommand {
    SetLnaState(u8),
    SetIfGain(i32),
    SetAgcEnabled(bool),
    SetAgcSetpoint(i32),
    SetBiasT(bool),
    SetHdrMode(bool),
    SetAmNotch(bool),
    SetFmNotch(bool),
    /// Antenna port: 0 = A, 1 = B, 2 = C.
    SetAntenna(u8),
}

/// Core receiver tuning and demodulation commands.
#[derive(Debug, Clone)]
pub enum ReceiverCmd {
    SetFrequency(u64),
    SetVolume(f32),
    SetDemodMode(DemodMode),
    /// Set NFM squelch threshold in dBFS (ignored outside NFM mode).
    SetSquelchThreshold(f32),
    /// Set keyboard/scroll tuning step in Hz.
    SetTuneStep(u64),
    /// Set NFM channel bandwidth in Hz (12500 or 25000).
    SetNfmBandwidth(u32),
    /// Enable or disable CTCSS tone squelch in NFM mode.
    SetCtcssEnabled(bool),
}

/// Spectrum/waterfall display commands.
#[derive(Debug, Clone)]
pub enum DisplayCmd {
    /// Adjust zoom level (1.0 = full BW, lower = zoomed in).
    SetZoom(f32),
    /// Adjust waterfall scroll speed multiplier.
    SetWaterfallSpeed(f32),
    /// Change the FFT bin count (must be a power of two: 512–8192).
    SetFftSize(usize),
    /// Change the FFT window function.
    SetFftWindow(crate::dsp::FftWindow),
    /// Set exponential averaging: 1 = off, 2–16 = frames to average.
    SetFftAveraging(u8),
    /// Toggle band plan overlay.
    SetBandPlanEnabled(bool),
}

/// Bookmark management commands.
#[derive(Debug, Clone)]
pub enum BookmarkCmd {
    /// Add a bookmark at the current frequency and mode.
    Add(String),
    /// Remove bookmark at the given index.
    Remove(usize),
    /// Edit an existing bookmark at index: new (name, freq_hz, mode, category).
    Edit(usize, String, u64, DemodMode, String),
}

/// Bookmark scanner commands.
#[derive(Debug, Clone)]
pub enum ScanCmd {
    /// Start cycling through bookmarks in the given category (empty = all).
    Start(String),
    /// Stop the scanner.
    Stop,
    /// Skip to the next bookmark immediately (also works during scan).
    Next,
    /// Set scanner dwell time in seconds (0.5–30 s).
    SetDwell(f32),
}

/// Commands from the UI to the signal path.
///
/// Each variant wraps a domain-specific sub-enum so the match handler can
/// delegate to focused sub-handlers.  Use `.into()` at call sites (all sub-enum
/// types implement `From<_> for SignalPathCommand`):
///
/// ```rust,ignore
/// cmd_tx.try_send(ReceiverCmd::SetFrequency(101_700_000).into()).ok();
/// cmd_tx.try_send(HardwareCommand::SetLnaState(3).into()).ok();
/// ```
#[derive(Debug)]
pub enum SignalPathCommand {
    Receiver(ReceiverCmd),
    /// Hardware device settings — forwarded verbatim to the device thread.
    Hardware(HardwareCommand),
    Display(DisplayCmd),
    Bookmark(BookmarkCmd),
    Scan(ScanCmd),
    StartRecording,
    StopRecording,
    /// Begin (or resume) signal processing.  The signal path starts in a
    /// paused state; send this command to begin demodulating and producing audio.
    Start,
    /// Pause signal processing.  The task stays alive; send `Start` to resume.
    Stop,
    /// Hot-swap the IQ source.  Sent by the hardware probe task when a device
    /// appears (or reappears) while the app is running.  Clears `source_dead`
    /// so the signal path resumes reading from the new receiver.
    ReconnectSource(broadcast::Receiver<Arc<[IqSample]>>),
}

impl From<ReceiverCmd> for SignalPathCommand {
    fn from(c: ReceiverCmd) -> Self { Self::Receiver(c) }
}
impl From<HardwareCommand> for SignalPathCommand {
    fn from(c: HardwareCommand) -> Self { Self::Hardware(c) }
}
impl From<DisplayCmd> for SignalPathCommand {
    fn from(c: DisplayCmd) -> Self { Self::Display(c) }
}
impl From<BookmarkCmd> for SignalPathCommand {
    fn from(c: BookmarkCmd) -> Self { Self::Bookmark(c) }
}
impl From<ScanCmd> for SignalPathCommand {
    fn from(c: ScanCmd) -> Self { Self::Scan(c) }
}

/// Manages the running signal path tasks.
pub struct SignalPath {
    shared: Arc<RwLock<SharedState>>,
    pub cmd_tx: crossbeam_channel::Sender<SignalPathCommand>,
    /// Handles to all running tasks, for awaiting shutdown.
    _handles: Vec<JoinHandle<()>>,
}

impl SignalPath {
    /// Start the signal path with the given IQ source broadcast receiver.
    ///
    /// `iq_rx` is the broadcast receiver from the source (e.g. RspdxSource).
    pub fn start(
        shared: Arc<RwLock<SharedState>>,
        iq_rx: broadcast::Receiver<Arc<[IqSample]>>,
        // crossbeam channels used for audio/recorder (sync threads, not async tasks)
        audio_tx: Option<crossbeam_channel::Sender<Arc<[StereoFrame]>>>,
        recorder_tx: Option<mpsc::Sender<Arc<[StereoFrame]>>>,
        egui_ctx: Option<egui_repaint::RepaintHandle>,
        // Optional hardware frequency atomic — if Some, SetFrequency writes here
        // so the device thread (polling every 50ms) picks up the new value.
        freq_atomic: Option<Arc<AtomicU64>>,
        // Optional hardware command channel — if Some, hardware control commands
        // (LNA, AGC, bias-T, etc.) are forwarded to the device thread.
        hardware_cmd_tx: Option<crossbeam_channel::Sender<HardwareCommand>>,
    ) -> Self {
        let (cmd_tx, cmd_rx) = crossbeam_channel::bounded::<SignalPathCommand>(64);
        let shared_clone = Arc::clone(&shared);
        let freq_atomic_clone = freq_atomic;
        let hw_cmd_tx = hardware_cmd_tx;
        // iq_rx must be mut so ReconnectSource can swap it at runtime.
        let mut iq_rx = iq_rx;

        // Read initial sample rate before moving shared into the task
        let sample_rate = shared.read().sample_rate_sps;

        // Demodulator state — switched at runtime by SetDemodMode.
        // WBFM uses StereoFmDecoder (outputs Vec<StereoFrame> + is_stereo flag).
        // NFM, AM, SSB, and CW use mono demodulators converted to StereoFrame.
        enum Demod {
            Wbfm(StereoFmDecoder),
            Nfm(FmDemodulator),
            Am(AmDemodulator),
            Ssb(SsbDemodulator),
            Cw(CwDemodulator),
        }

        impl Demod {
            fn reset(&mut self) {
                match self {
                    Demod::Wbfm(d) => d.reset(),
                    Demod::Nfm(d) => d.reset(),
                    Demod::Am(d) => d.reset(),
                    Demod::Ssb(d) => d.reset(),
                    Demod::Cw(d) => d.reset(),
                }
            }
        }

        let handle = tokio::spawn(async move {
            let sr = sample_rate.max(200_000);
            let mut fft_size = FFT_SIZE;
            let mut fft_window = crate::dsp::FftWindow::Hann;
            let mut fft = FftProcessor::new(fft_size, fft_window);
            let mut fft_averaging: u8 = 4;
            let mut fft_avg_buf: Vec<f32> = vec![-120.0; fft_size];
            let mut vol = Volume::new(0.8);
            let mut demod: Demod = Demod::Wbfm(StereoFmDecoder::new(sr));
            let mut squelch = Squelch::new(48_000, -50.0);
            let mut rds = RdsDecoder::new(sr);
            let mut audio_bp = AudioBandpass::voice(48_000.0);
            let mut ctcss = CtcssDetector::with_default_threshold(48_000.0);
            let mut nfm_bw_hz: u32 = 12_500;
            let mut ctcss_enabled: bool = false;
            let mut iq_accumulator: Vec<IqSample> = Vec::with_capacity(FFT_SIZE * 2);
            let mut audio_accumulator: Vec<StereoFrame> = Vec::with_capacity(AUDIO_FRAME_SIZE * 2);
            // Scanner state
            let mut scan_running = false;
            let mut scan_cursor: usize = 0;
            let mut scan_category = String::new();
            let mut scan_dwell_secs: f32 = 2.0;
            // Accumulates IQ sample count for dwell timer; compare to sample_rate * dwell_secs
            let mut scan_dwell_samples: u64 = 0;
            // True while squelch is open (signal active) — scanner pauses.
            let mut _scan_squelch_open = false;

            /// Create a fresh demodulator for the given mode.
            fn make_demod(mode: DemodMode, sr: u32, nfm_bw_hz: u32) -> Demod {
                match mode {
                    DemodMode::Wbfm => Demod::Wbfm(StereoFmDecoder::new(sr)),
                    DemodMode::Nfm => Demod::Nfm(FmDemodulator::new(sr, 48_000, nfm_bw_hz as f32, 0.0)),
                    DemodMode::Am => Demod::Am(AmDemodulator::new(sr, 48_000)),
                    DemodMode::Usb => Demod::Ssb(SsbDemodulator::standard(SsbMode::Usb, sr)),
                    DemodMode::Lsb => Demod::Ssb(SsbDemodulator::standard(SsbMode::Lsb, sr)),
                    DemodMode::Dsb => Demod::Ssb(SsbDemodulator::standard(SsbMode::Dsb, sr)),
                    DemodMode::Cw => Demod::Cw(CwDemodulator::new(sr, 48_000)),
                }
            }

            /// Find the next bookmark at or after `start_idx` matching `category`.
            /// Returns (index, freq_hz, mode) or None.
            fn scan_next_bookmark(
                bookmarks: &[Bookmark],
                category: &str,
                start_idx: usize,
            ) -> Option<(usize, u64, DemodMode)> {
                bookmarks
                    .iter()
                    .enumerate()
                    .skip(start_idx)
                    .find(|(_, b)| category.is_empty() || b.category == category)
                    .map(|(i, b)| (i, b.freq_hz, b.mode))
            }

            // Start paused — the UI must send `Start` to begin processing.
            shared_clone.write().is_running = false;
            let mut paused = true;
            let mut source_dead = false;

            loop {
                // Drain any pending commands (non-blocking)
                while let Ok(cmd) = cmd_rx.try_recv() {
                    match cmd {
                        SignalPathCommand::Receiver(c) => match c {
                            ReceiverCmd::SetFrequency(hz) => {
                                shared_clone.write().center_freq_hz = hz;
                                if let Some(ref atomic) = freq_atomic_clone {
                                    atomic.store(hz, Ordering::Relaxed);
                                }
                                demod.reset();
                                rds.reset();
                                {
                                    let mut s = shared_clone.write();
                                    s.rds.ps_name = None;
                                    s.rds.pty = None;
                                    s.rds.ta = false;
                                    s.rds.rt = None;
                                }
                            }
                            ReceiverCmd::SetVolume(v) => {
                                vol.set(v);
                                shared_clone.write().demod.volume = v;
                            }
                            ReceiverCmd::SetDemodMode(mode) => {
                                demod = make_demod(mode, sr, nfm_bw_hz);
                                squelch.reset();
                                audio_bp.reset();
                                ctcss.reset();
                                rds.reset();
                                let mut s = shared_clone.write();
                                s.demod.demod_mode = mode;
                                s.rds.is_stereo = false;
                                s.rds.ps_name = None;
                                s.rds.pty = None;
                                s.rds.ta = false;
                                s.rds.rt = None;
                                tracing::info!(?mode, "demod mode changed");
                            }
                            ReceiverCmd::SetSquelchThreshold(t) => {
                                squelch.set_threshold_dbfs(t);
                                shared_clone.write().demod.squelch_threshold = t;
                            }
                            ReceiverCmd::SetTuneStep(step) => {
                                shared_clone.write().demod.tune_step_hz = step;
                            }
                            ReceiverCmd::SetNfmBandwidth(bw) => {
                                nfm_bw_hz = bw;
                                if matches!(demod, Demod::Nfm(_)) {
                                    demod = Demod::Nfm(FmDemodulator::new(sr, 48_000, bw as f32, 0.0));
                                    audio_bp.reset();
                                    ctcss.reset();
                                }
                                shared_clone.write().demod.nfm_bandwidth_hz = bw;
                            }
                            ReceiverCmd::SetCtcssEnabled(enabled) => {
                                ctcss_enabled = enabled;
                                ctcss.reset();
                                shared_clone.write().demod.ctcss_squelch_enabled = enabled;
                                shared_clone.write().demod.ctcss_tone_detected = false;
                            }
                        }
                        SignalPathCommand::Hardware(hw) => {
                            // Update SharedState to mirror the hardware change
                            {
                                let mut s = shared_clone.write();
                                match &hw {
                                    HardwareCommand::SetLnaState(n)    => s.hardware.lna_state = *n,
                                    HardwareCommand::SetIfGain(g)      => s.hardware.if_gain_dbfs = *g,
                                    HardwareCommand::SetAgcEnabled(en) => s.hardware.agc_enabled = *en,
                                    HardwareCommand::SetAgcSetpoint(sp)=> s.hardware.agc_setpoint_dbfs = *sp,
                                    HardwareCommand::SetBiasT(en)      => s.hardware.bias_t_enabled = *en,
                                    HardwareCommand::SetHdrMode(en)    => s.hardware.hdr_mode = *en,
                                    HardwareCommand::SetAmNotch(en)    => s.hardware.am_notch_enabled = *en,
                                    HardwareCommand::SetFmNotch(en)    => s.hardware.fm_notch_enabled = *en,
                                    HardwareCommand::SetAntenna(port)  => s.hardware.antenna_port = *port,
                                }
                            }
                            // Forward verbatim to the device thread
                            if let Some(ref tx) = hw_cmd_tx {
                                let _ = tx.try_send(hw);
                            }
                        }
                        SignalPathCommand::Display(c) => match c {
                            DisplayCmd::SetZoom(z) => {
                                shared_clone.write().zoom_level = z.clamp(0.01, 1.0);
                            }
                            DisplayCmd::SetWaterfallSpeed(spd) => {
                                shared_clone.write().waterfall_speed = spd.clamp(0.1, 10.0);
                            }
                            DisplayCmd::SetFftSize(sz) => {
                                if sz.is_power_of_two() && sz >= 512 && sz <= 8192 {
                                    fft_size = sz;
                                    fft = FftProcessor::new(fft_size, fft_window);
                                    fft_avg_buf = vec![-120.0; fft_size];
                                    iq_accumulator.clear();
                                    shared_clone.write().fft.fft_size = sz;
                                    shared_clone.write().fft.fft_magnitudes = vec![-120.0; sz];
                                }
                            }
                            DisplayCmd::SetFftWindow(wf) => {
                                fft_window = wf;
                                fft = FftProcessor::new(fft_size, fft_window);
                                shared_clone.write().fft.fft_window = wf;
                            }
                            DisplayCmd::SetFftAveraging(n) => {
                                fft_averaging = n.max(1).min(16);
                                fft_avg_buf = vec![-120.0; fft_size];
                                shared_clone.write().fft.fft_averaging = fft_averaging;
                            }
                            DisplayCmd::SetBandPlanEnabled(en) => {
                                shared_clone.write().fft.band_plan_enabled = en;
                            }
                        }
                        SignalPathCommand::Bookmark(c) => match c {
                            BookmarkCmd::Add(name) => {
                                let (freq, mode) = {
                                    let s = shared_clone.read();
                                    (s.center_freq_hz, s.demod.demod_mode)
                                };
                                shared_clone.write().bookmarks.push(Bookmark::new(name, freq, mode));
                            }
                            BookmarkCmd::Remove(idx) => {
                                let mut s = shared_clone.write();
                                if idx < s.bookmarks.len() {
                                    s.bookmarks.remove(idx);
                                    if s.bookmark_cursor >= s.bookmarks.len() && !s.bookmarks.is_empty() {
                                        s.bookmark_cursor = s.bookmarks.len() - 1;
                                    }
                                }
                            }
                            BookmarkCmd::Edit(idx, name, freq, mode, cat) => {
                                let mut s = shared_clone.write();
                                if idx < s.bookmarks.len() {
                                    s.bookmarks[idx] = Bookmark { name, freq_hz: freq, mode, category: cat };
                                }
                            }
                        }
                        SignalPathCommand::Scan(c) => match c {
                            ScanCmd::Start(cat) => {
                                scan_category = cat.clone();
                                scan_running = true;
                                scan_cursor = 0;
                                scan_dwell_samples = 0;
                                {
                                    let mut s = shared_clone.write();
                                    s.scanner.scan_running = true;
                                    s.scanner.scan_category = cat;
                                    s.scanner.scan_cursor = 0;
                                }
                                let first = {
                                    let s = shared_clone.read();
                                    scan_next_bookmark(&s.bookmarks, &scan_category, scan_cursor)
                                };
                                if let Some((idx, bm_freq, bm_mode)) = first {
                                    scan_cursor = idx;
                                    shared_clone.write().scanner.scan_cursor = idx;
                                    shared_clone.write().center_freq_hz = bm_freq;
                                    if let Some(ref atomic) = freq_atomic_clone {
                                        atomic.store(bm_freq, Ordering::Relaxed);
                                    }
                                    demod = make_demod(bm_mode, sr, nfm_bw_hz);
                                    shared_clone.write().demod.demod_mode = bm_mode;
                                    demod.reset();
                                    rds.reset();
                                }
                            }
                            ScanCmd::Stop => {
                                scan_running = false;
                                shared_clone.write().scanner.scan_running = false;
                            }
                            ScanCmd::Next => {
                                if scan_running {
                                    scan_dwell_samples = u64::MAX;
                                }
                            }
                            ScanCmd::SetDwell(secs) => {
                                scan_dwell_secs = secs.clamp(0.5, 30.0);
                                shared_clone.write().scanner.scan_dwell_secs = scan_dwell_secs;
                            }
                        }
                        SignalPathCommand::StartRecording => {
                            shared_clone.write().is_recording = true;
                        }
                        SignalPathCommand::StopRecording => {
                            shared_clone.write().is_recording = false;
                        }
                        SignalPathCommand::Start => {
                            paused = false;
                            shared_clone.write().is_running = true;
                            iq_accumulator.clear();
                        }
                        SignalPathCommand::Stop => {
                            paused = true;
                            shared_clone.write().is_running = false;
                            iq_accumulator.clear();
                        }
                        SignalPathCommand::ReconnectSource(new_rx) => {
                            iq_rx = new_rx;
                            source_dead = false;
                            tracing::info!("IQ source hot-swapped — signal path live");
                        }
                    }
                }

                // When the source is dead, sleep briefly and loop to keep
                // processing commands (so Start/Stop/freq changes still work).
                if source_dead {
                    tokio::time::sleep(std::time::Duration::from_millis(16)).await;
                    continue;
                }

                // Receive a batch of IQ samples
                let batch = match iq_rx.recv().await {
                    Ok(b) => b,
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(dropped = n, "signal path lagged — dropped batches");
                        continue;
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        // Source disconnected (device thread exited). Keep the
                        // signal path alive so the UI can still send commands.
                        tracing::warn!("IQ source disconnected — signal path idle");
                        source_dead = true;
                        paused = true;
                        shared_clone.write().is_running = false;
                        continue;
                    }
                };

                // When paused, drain IQ without processing to avoid broadcast lag.
                if paused {
                    continue;
                }

                // Accumulate for FFT
                iq_accumulator.extend_from_slice(&batch);
                if iq_accumulator.len() >= fft_size {
                    if let Some(mags) = fft.process(&iq_accumulator) {
                        // Exponential moving average: alpha ≈ 2/(N+1) for N-frame avg.
                        let alpha = if fft_averaging <= 1 {
                            1.0_f32
                        } else {
                            2.0 / (fft_averaging as f32 + 1.0)
                        };
                        for (avg, &new) in fft_avg_buf.iter_mut().zip(mags.iter()) {
                            *avg = alpha * new + (1.0 - alpha) * *avg;
                        }
                        // SNR: peak in center ±bw_bins vs median of remaining bins.
                        let snr = {
                            let n = fft_avg_buf.len();
                            let center = n / 2;
                            let bw_hz = {
                                let s = shared_clone.read();
                                match s.demod.demod_mode {
                                    DemodMode::Wbfm => 200_000_u32,
                                    DemodMode::Nfm => s.demod.nfm_bandwidth_hz,
                                    DemodMode::Am | DemodMode::Usb | DemodMode::Lsb | DemodMode::Dsb => 10_000,
                                    DemodMode::Cw => 1_000,
                                }
                            };
                            let sr_hz = shared_clone.read().sample_rate_sps.max(1) as f32;
                            let half_bw_bins = ((bw_hz as f32 / sr_hz * n as f32) as usize).max(2).min(n / 4);
                            let sig_lo = center.saturating_sub(half_bw_bins);
                            let sig_hi = (center + half_bw_bins).min(n - 1);
                            let peak = fft_avg_buf[sig_lo..=sig_hi]
                                .iter()
                                .cloned()
                                .fold(f32::NEG_INFINITY, f32::max);
                            // Noise: collect all bins outside signal window, take median.
                            let mut noise_bins: Vec<f32> = fft_avg_buf[..sig_lo].iter()
                                .chain(fft_avg_buf[sig_hi + 1..].iter())
                                .cloned()
                                .collect();
                            let noise_floor = if noise_bins.is_empty() {
                                -120.0_f32
                            } else {
                                noise_bins.sort_by(|a, b| a.partial_cmp(b).unwrap());
                                noise_bins[noise_bins.len() / 2]
                            };
                            peak - noise_floor
                        };
                        {
                            let mut s = shared_clone.write();
                            s.fft.fft_magnitudes = fft_avg_buf.clone();
                            s.fft.snr_db = Some(snr);
                        }
                        if let Some(ref ctx) = egui_ctx {
                            ctx.request_repaint();
                        }
                    }
                    iq_accumulator.drain(..fft_size);
                }

                // ── Scanner tick ─────────────────────────────────────────────
                if scan_running {
                    scan_dwell_samples += batch.len() as u64;
                    // Read squelch state (above threshold = signal present = pause).
                    let sq_threshold = shared_clone.read().demod.squelch_threshold;
                    let snr_now = shared_clone.read().fft.snr_db.unwrap_or(-120.0);
                    // "squelch open" = signal detected above threshold
                    let signal_present = snr_now > (sq_threshold + 120.0).max(0.0);
                    if signal_present {
                        // Signal active → stay on this channel; reset dwell timer.
                        scan_dwell_samples = 0;
                        _scan_squelch_open = true;
                    } else {
                        _scan_squelch_open = false;
                    }
                    let dwell_target = (scan_dwell_secs * sr as f32) as u64;
                    if scan_dwell_samples >= dwell_target {
                        scan_dwell_samples = 0;
                        // Advance to next matching bookmark.
                        let next_idx = scan_cursor + 1;
                        let (bm_freq, bm_mode, new_idx) = {
                            let s = shared_clone.read();
                            if let Some((idx, freq, mode)) = scan_next_bookmark(&s.bookmarks, &scan_category, next_idx) {
                                (freq, mode, idx)
                            } else if let Some((idx, freq, mode)) = scan_next_bookmark(&s.bookmarks, &scan_category, 0) {
                                (freq, mode, idx) // wrap around
                            } else {
                                // No bookmarks → stop scan
                                (0, DemodMode::Wbfm, usize::MAX)
                            }
                        };
                        if new_idx == usize::MAX {
                            scan_running = false;
                            shared_clone.write().scanner.scan_running = false;
                        } else {
                            scan_cursor = new_idx;
                            shared_clone.write().scanner.scan_cursor = new_idx;
                            shared_clone.write().center_freq_hz = bm_freq;
                            if let Some(ref atomic) = freq_atomic_clone {
                                atomic.store(bm_freq, Ordering::Relaxed);
                            }
                            demod = make_demod(bm_mode, sr, nfm_bw_hz);
                            shared_clone.write().demod.demod_mode = bm_mode;
                            demod.reset();
                            rds.reset();
                        }
                    }
                }

                // Demodulate IQ → StereoFrame batches at 48 kHz.
                let iq_complex: Vec<Complex<f32>> = batch
                    .iter()
                    .map(|s| Complex::new(s.re, s.im))
                    .collect();

                let stereo: Vec<StereoFrame> = match &mut demod {
                    Demod::Wbfm(d) => {
                        let (frames, is_stereo, composite) =
                            d.process_with_composite(&iq_complex);
                        shared_clone.write().rds.is_stereo = is_stereo;
                        if rds.process(&composite) {
                            let mut s = shared_clone.write();
                            s.rds.ps_name = rds.data.ps_name.clone();
                            s.rds.pty = rds.data.pty;
                            s.rds.tp = rds.data.tp;
                            s.rds.ta = rds.data.ta;
                            s.rds.rt = rds.data.rt.clone();
                        }
                        frames
                    }
                    Demod::Nfm(d) => {
                        let mono = d.process(&iq_complex);
                        // Run CTCSS detector on raw demodulated audio (before squelch/filter)
                        if ctcss_enabled {
                            ctcss.process_batch(&mono);
                            let detected = ctcss.is_tone_present();
                            shared_clone.write().demod.ctcss_tone_detected = detected;
                        }
                        // Apply squelch (dBFS threshold gate)
                        let gated = squelch.process(&mono);
                        // CTCSS gate: mute if enabled and no tone detected
                        let ctcss_gated: Vec<f32> = if ctcss_enabled && !ctcss.is_tone_present() {
                            vec![0.0; gated.len()]
                        } else {
                            gated
                        };
                        // Voice bandpass: 300 Hz – 3 kHz
                        let mut filtered = ctcss_gated;
                        audio_bp.process_inplace(&mut filtered);
                        filtered.into_iter().map(StereoFrame::mono).collect()
                    }
                    Demod::Am(d) => {
                        let mono = d.process(&iq_complex);
                        mono.into_iter().map(StereoFrame::mono).collect()
                    }
                    Demod::Ssb(d) => {
                        let mono = d.process(&iq_complex);
                        mono.into_iter().map(StereoFrame::mono).collect()
                    }
                    Demod::Cw(d) => {
                        let mono = d.process(&iq_complex);
                        mono.into_iter().map(StereoFrame::mono).collect()
                    }
                };

                let mut stereo_processed = vol.process(&stereo);
                audio_accumulator.append(&mut stereo_processed);

                // Emit audio frames
                while audio_accumulator.len() >= AUDIO_FRAME_SIZE {
                    let frame: Arc<[StereoFrame]> =
                        audio_accumulator[..AUDIO_FRAME_SIZE].to_vec().into();
                    audio_accumulator.drain(..AUDIO_FRAME_SIZE);

                    if let Some(ref tx) = audio_tx {
                        // try_send: drop frame on backpressure rather than blocking
                        let _ = tx.try_send(Arc::clone(&frame));
                    }
                    if shared_clone.read().is_recording {
                        if let Some(ref tx) = recorder_tx {
                            let _ = tx.try_send(Arc::clone(&frame));
                        }
                    }
                }
            }

            // Note: the loop above never breaks (signal path runs for the app lifetime).
            // Kept here as a logical boundary; dead code warning is expected.
        });

        Self {
            shared,
            cmd_tx,
            _handles: vec![handle],
        }
    }

    pub fn send_command(&self, cmd: SignalPathCommand) {
        let _ = self.cmd_tx.try_send(cmd);
    }

    pub fn shared(&self) -> &Arc<RwLock<SharedState>> {
        &self.shared
    }
}

/// Minimal handle for requesting egui repaints from a background task.
/// Wraps egui::Context in a Send+Sync way.
pub mod egui_repaint {
    use std::sync::Arc;

    pub struct RepaintHandle(Arc<dyn Fn() + Send + Sync>);

    impl RepaintHandle {
        pub fn new(f: impl Fn() + Send + Sync + 'static) -> Self {
            Self(Arc::new(f))
        }

        pub fn request_repaint(&self) {
            (self.0)();
        }
    }

    impl Clone for RepaintHandle {
        fn clone(&self) -> Self {
            Self(Arc::clone(&self.0))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_state_default_has_fft_buffer() {
        let state = SharedState::new();
        assert_eq!(state.fft.fft_magnitudes.len(), FFT_SIZE);
        assert!(state.fft.fft_magnitudes.iter().all(|&v| v <= -100.0));
    }

    // ── Signal path command-dispatch integration tests ────────────────────────
    //
    // Pattern:
    //   1. Start the signal path (spawns a tokio task).
    //   2. Send a command — it queues in the crossbeam channel.
    //   3. Send an empty IQ batch to unblock the task's `iq_rx.recv().await`.
    //   4. `yield_now()` — scheduler runs the signal path task until it blocks
    //      again (after draining the command queue and looping back to recv).
    //   5. Assert the SharedState mutation.

    fn make_signal_path() -> (
        SignalPath,
        broadcast::Sender<Arc<[IqSample]>>,
        Arc<RwLock<SharedState>>,
    ) {
        let (iq_tx, iq_rx) = broadcast::channel(8);
        let shared = Arc::new(RwLock::new(SharedState::new()));
        let path = SignalPath::start(
            Arc::clone(&shared),
            iq_rx,
            None, None, None, None, None,
        );
        (path, iq_tx, shared)
    }

    /// Send `cmd`, tickle the signal path loop with an empty IQ batch, yield.
    async fn tick(
        cmd_tx: &crossbeam_channel::Sender<SignalPathCommand>,
        iq_tx: &broadcast::Sender<Arc<[IqSample]>>,
        cmd: impl Into<SignalPathCommand>,
    ) {
        cmd_tx.try_send(cmd.into()).unwrap();
        let _ = iq_tx.send(Arc::new([]));
        tokio::task::yield_now().await;
    }

    #[tokio::test]
    async fn set_frequency_updates_center_freq() {
        let (path, iq_tx, shared) = make_signal_path();
        tick(&path.cmd_tx, &iq_tx, ReceiverCmd::SetFrequency(101_700_000)).await;
        assert_eq!(shared.read().center_freq_hz, 101_700_000);
        let _ = path.cmd_tx.try_send(SignalPathCommand::Stop);
    }

    #[tokio::test]
    async fn set_volume_updates_demod_state() {
        let (path, iq_tx, shared) = make_signal_path();
        tick(&path.cmd_tx, &iq_tx, ReceiverCmd::SetVolume(0.42)).await;
        assert!((shared.read().demod.volume - 0.42).abs() < 1e-6);
        let _ = path.cmd_tx.try_send(SignalPathCommand::Stop);
    }

    #[tokio::test]
    async fn set_demod_mode_switches_demod() {
        let (path, iq_tx, shared) = make_signal_path();
        tick(&path.cmd_tx, &iq_tx, ReceiverCmd::SetDemodMode(DemodMode::Am)).await;
        assert_eq!(shared.read().demod.demod_mode, DemodMode::Am);
        let _ = path.cmd_tx.try_send(SignalPathCommand::Stop);
    }

    #[tokio::test]
    async fn set_squelch_threshold_updates_demod_state() {
        let (path, iq_tx, shared) = make_signal_path();
        tick(&path.cmd_tx, &iq_tx, ReceiverCmd::SetSquelchThreshold(-45.0)).await;
        assert!((shared.read().demod.squelch_threshold - (-45.0)).abs() < 1e-6);
        let _ = path.cmd_tx.try_send(SignalPathCommand::Stop);
    }

    #[tokio::test]
    async fn bookmark_add_and_remove_round_trip() {
        let (path, iq_tx, shared) = make_signal_path();
        // SharedState::new() starts with 1 pre-loaded bookmark (BBC Radio 4).
        let initial = shared.read().bookmarks.len();
        tick(&path.cmd_tx, &iq_tx, BookmarkCmd::Add("NOAA Weather".to_string())).await;
        assert_eq!(shared.read().bookmarks.len(), initial + 1, "one bookmark added");
        let last_idx = shared.read().bookmarks.len() - 1;
        assert_eq!(shared.read().bookmarks[last_idx].name, "NOAA Weather");
        tick(&path.cmd_tx, &iq_tx, BookmarkCmd::Remove(last_idx)).await;
        assert_eq!(shared.read().bookmarks.len(), initial, "back to initial count after Remove");
        let _ = path.cmd_tx.try_send(SignalPathCommand::Stop);
    }

    #[tokio::test]
    async fn fft_size_change_accepted_for_power_of_two() {
        let (path, iq_tx, shared) = make_signal_path();
        tick(&path.cmd_tx, &iq_tx, DisplayCmd::SetFftSize(4096)).await;
        assert_eq!(shared.read().fft.fft_size, 4096);
        assert_eq!(shared.read().fft.fft_magnitudes.len(), 4096);
        let _ = path.cmd_tx.try_send(SignalPathCommand::Stop);
    }

    #[tokio::test]
    async fn fft_size_change_rejected_for_non_power_of_two() {
        let (path, iq_tx, shared) = make_signal_path();
        // 3000 is not a power-of-two — should be silently ignored
        tick(&path.cmd_tx, &iq_tx, DisplayCmd::SetFftSize(3000)).await;
        assert_eq!(shared.read().fft.fft_size, FFT_SIZE, "non-power-of-two FFT size should be ignored");
        let _ = path.cmd_tx.try_send(SignalPathCommand::Stop);
    }

    #[tokio::test]
    async fn band_plan_toggle_updates_display_state() {
        let (path, iq_tx, shared) = make_signal_path();
        tick(&path.cmd_tx, &iq_tx, DisplayCmd::SetBandPlanEnabled(true)).await;
        assert!(shared.read().fft.band_plan_enabled);
        let _ = path.cmd_tx.try_send(SignalPathCommand::Stop);
    }
}
