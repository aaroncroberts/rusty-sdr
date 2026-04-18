//! Command enums for the signal path.
//!
//! Commands are sent from the UI (or MIDI controller) to the signal path task
//! via a `crossbeam_channel::Sender<SignalPathCommand>`.
//!
//! Use `.into()` at call sites — all sub-enums implement `From<_> for SignalPathCommand`:
//!
//! ```rust,ignore
//! cmd_tx.try_send(ReceiverCmd::SetFrequency(101_700_000).into()).ok();
//! cmd_tx.try_send(HardwareCommand::SetLnaState(3).into()).ok();
//! ```

use std::sync::Arc;
use tokio::sync::broadcast;

use super::shared_state::DemodMode;
use crate::sample::IqSample;

/// Commands forwarded from the signal path to the hardware device thread.
///
/// The signal path holds an optional `crossbeam_channel::Sender<HardwareCommand>`.
/// When a hardware-related [`SignalPathCommand`] is received, the signal path
/// updates [`super::SharedState`] and forwards a `HardwareCommand` to the device.
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
    /// Close and reopen the hardware device without restarting the app.
    /// The device thread completes a clean RAII shutdown then starts a new session.
    RestartDevice,
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
    /// Enable or disable the spectrum peak-hold line.
    SetPeakHoldEnabled(bool),
    /// Set peak-hold decay rate in dB per display frame (0.1–2.0).
    SetPeakHoldDecay(f32),
}

/// Bookmark management commands.
#[derive(Debug, Clone)]
pub enum BookmarkCmd {
    /// Add a bookmark at the current frequency and mode.
    Add(String),
    /// Remove bookmark at the given index.
    Remove(usize),
    /// Edit an existing bookmark at index: new (name, freq_hz, mode, category, nfm_bw, squelch, ctcss).
    Edit(usize, String, u64, DemodMode, String, Option<u32>, Option<f32>, Option<bool>),
}

/// Bookmark scanner commands.
#[derive(Debug, Clone)]
pub enum ScanCmd {
    /// Start cycling through bookmarks in the given category (empty = all).
    Start(String),
    /// Start a frequency-range sweep.
    ///
    /// Steps from `freq_lo` to `freq_hi` in `step_hz` increments, dwelling
    /// `dwell_secs` on each frequency before checking signal level.  Stops
    /// when `signal_level_dbfs ≥ squelch_dbfs` (signal found).
    ///
    /// If `stereo_only` is true and `mode` is WBFM, the scanner only stops
    /// when the stereo pilot (19 kHz) is also detected — this proves a real
    /// FM broadcast was found rather than a noise spike.
    StartRange {
        freq_lo: u64,
        freq_hi: u64,
        step_hz: u64,
        dwell_secs: f32,
        squelch_dbfs: f32,
        mode: DemodMode,
        stereo_only: bool,
    },
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
/// types implement `From<_> for SignalPathCommand`).
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
    /// and updates the hardware command channel to the new device.
    ///
    /// After swapping, the signal path re-applies current frequency, demod mode,
    /// and hardware settings so the new device comes up in the user's current state.
    ReconnectSource {
        iq_rx: broadcast::Receiver<Arc<[IqSample]>>,
        /// New hardware command channel, or `None` for sources without one.
        hardware_cmd_tx: Option<crossbeam_channel::Sender<HardwareCommand>>,
    },
}

impl From<ReceiverCmd> for SignalPathCommand {
    fn from(c: ReceiverCmd) -> Self {
        Self::Receiver(c)
    }
}
impl From<HardwareCommand> for SignalPathCommand {
    fn from(c: HardwareCommand) -> Self {
        Self::Hardware(c)
    }
}
impl From<DisplayCmd> for SignalPathCommand {
    fn from(c: DisplayCmd) -> Self {
        Self::Display(c)
    }
}
impl From<BookmarkCmd> for SignalPathCommand {
    fn from(c: BookmarkCmd) -> Self {
        Self::Bookmark(c)
    }
}
impl From<ScanCmd> for SignalPathCommand {
    fn from(c: ScanCmd) -> Self {
        Self::Scan(c)
    }
}
