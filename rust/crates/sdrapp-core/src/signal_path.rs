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
}

impl Bookmark {
    pub fn new(name: impl Into<String>, freq_hz: u64, mode: DemodMode) -> Self {
        Self { name: name.into(), freq_hz, mode }
    }
}

/// Shared display state written by the signal path, read by the UI.
#[derive(Default)]
pub struct SharedState {
    /// Latest FFT magnitudes (dBFS), length = FFT_SIZE.
    pub fft_magnitudes: Vec<f32>,
    /// Center frequency (Hz) as reported by the source.
    pub center_freq_hz: u64,
    /// Sample rate (sps) as reported by the source.
    pub sample_rate_sps: u32,
    /// Whether the signal path is currently running.
    pub is_running: bool,
    /// Whether recording is active.
    pub is_recording: bool,
    /// Current volume (linear).
    pub volume: f32,
    /// Active source name — "Demo Mode" when running on the test signal source.
    pub source_name: Option<String>,
    /// MIDI device name when connected, None otherwise.
    pub midi_device: Option<String>,
    /// Active MIDI page index.
    pub midi_page: usize,
    /// Audio buffer fill fraction [0.0, 1.0] — written by audio sink.
    pub audio_buffer_fill: f32,
    /// Current demodulation mode.
    pub demod_mode: DemodMode,
    /// NFM squelch threshold in dBFS (e.g. -50.0). Applied only in NFM mode.
    pub squelch_threshold: f32,
    /// Whether a stereo pilot tone is currently detected (WBFM only).
    pub is_stereo: bool,
    /// RDS Programme Service name, if decoded (WBFM only).
    pub rds_ps_name: Option<String>,
    /// RDS Programme Type code (0-31).
    pub rds_pty: Option<u8>,
    /// RDS Traffic Programme flag.
    pub rds_tp: bool,
    /// RDS Traffic Announcement flag.
    pub rds_ta: bool,
    /// RDS RadioText (up to 64 chars).
    pub rds_rt: Option<String>,
    /// Spectrum zoom level: 1.0 = full bandwidth, 0.1 = 10× zoom.
    pub zoom_level: f32,
    /// Waterfall scroll speed multiplier (1.0 = normal).
    pub waterfall_speed: f32,
    /// Frequency step size for keyboard/scroll tuning (Hz).
    pub tune_step_hz: u64,
    /// Saved frequency bookmarks.
    pub bookmarks: Vec<Bookmark>,
    /// Index of the currently selected bookmark (for MIDI navigation).
    pub bookmark_cursor: usize,
    /// Whether the help panel is open.
    pub help_panel_open: bool,
    /// NFM channel bandwidth in Hz (12500 or 25000).
    pub nfm_bandwidth_hz: u32,
    /// Whether CTCSS tone squelch is enabled in NFM mode.
    pub ctcss_squelch_enabled: bool,
    /// Whether a CTCSS tone is currently detected (NFM + CTCSS enabled).
    pub ctcss_tone_detected: bool,
    /// Active recording mode (what to capture when recording starts).
    pub recording_mode: RecordingMode,
    /// Scheduled recording: seconds until start (0 = start now, None = not scheduled).
    pub scheduled_record_delay_secs: Option<u64>,
    /// Scheduled recording duration in seconds.
    pub scheduled_record_duration_secs: u32,

    // ── Hardware control state (RSPdx-R2) ─────────────────────────────────────
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

impl SharedState {
    pub fn new() -> Self {
        Self {
            fft_magnitudes: vec![-120.0; FFT_SIZE],
            volume: 0.8,
            squelch_threshold: -50.0,
            zoom_level: 1.0,
            waterfall_speed: 1.0,
            tune_step_hz: 100_000,
            bookmarks: vec![
                Bookmark::new("BBC Radio 4", 93_500_000, DemodMode::Wbfm),
            ],
            nfm_bandwidth_hz: 12_500,
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

/// Commands from the UI to the signal path.
#[derive(Debug)]
pub enum SignalPathCommand {
    SetFrequency(u64),
    SetVolume(f32),
    SetDemodMode(DemodMode),
    /// Set NFM squelch threshold in dBFS (ignored outside NFM mode).
    SetSquelchThreshold(f32),
    StartRecording,
    StopRecording,
    /// Adjust zoom level (1.0 = full BW, lower = zoomed in).
    SetZoom(f32),
    /// Adjust waterfall scroll speed multiplier.
    SetWaterfallSpeed(f32),
    /// Set keyboard/scroll tuning step in Hz.
    SetTuneStep(u64),
    /// Add a bookmark at the current frequency and mode.
    AddBookmark(String),
    /// Remove bookmark at the given index.
    RemoveBookmark(usize),
    /// Set NFM channel bandwidth in Hz (12500 or 25000).
    SetNfmBandwidth(u32),
    /// Enable or disable CTCSS tone squelch in NFM mode.
    SetCtcssEnabled(bool),
    // ── Hardware controls (forwarded to device thread via HardwareCommand) ───
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
    Stop,
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
        mut iq_rx: broadcast::Receiver<Arc<[IqSample]>>,
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
            let mut fft = FftProcessor::new(FFT_SIZE);
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

            shared_clone.write().is_running = true;

            loop {
                // Drain any pending commands (non-blocking)
                while let Ok(cmd) = cmd_rx.try_recv() {
                    match cmd {
                        SignalPathCommand::SetFrequency(hz) => {
                            shared_clone.write().center_freq_hz = hz;
                            // Push new frequency to hardware (device thread polls every 50ms)
                            if let Some(ref atomic) = freq_atomic_clone {
                                atomic.store(hz, Ordering::Relaxed);
                            }
                            demod.reset();
                            rds.reset();
                            {
                                let mut s = shared_clone.write();
                                s.rds_ps_name = None;
                                s.rds_pty = None;
                                s.rds_ta = false;
                                s.rds_rt = None;
                            }
                        }
                        SignalPathCommand::SetVolume(v) => {
                            vol.set(v);
                            shared_clone.write().volume = v;
                        }
                        SignalPathCommand::SetDemodMode(mode) => {
                            demod = match mode {
                                DemodMode::Wbfm => Demod::Wbfm(StereoFmDecoder::new(sr)),
                                DemodMode::Nfm => {
                                    Demod::Nfm(FmDemodulator::new(sr, 48_000, nfm_bw_hz as f32, 0.0))
                                }
                                DemodMode::Am => Demod::Am(AmDemodulator::standard(sr)),
                                DemodMode::Usb => {
                                    Demod::Ssb(SsbDemodulator::standard(SsbMode::Usb, sr))
                                }
                                DemodMode::Lsb => {
                                    Demod::Ssb(SsbDemodulator::standard(SsbMode::Lsb, sr))
                                }
                                DemodMode::Dsb => {
                                    Demod::Ssb(SsbDemodulator::standard(SsbMode::Dsb, sr))
                                }
                                DemodMode::Cw => Demod::Cw(CwDemodulator::standard(sr)),
                            };
                            squelch.reset();
                            audio_bp.reset();
                            ctcss.reset();
                            rds.reset();
                            let mut s = shared_clone.write();
                            s.demod_mode = mode;
                            s.is_stereo = false;
                            s.rds_ps_name = None;
                            s.rds_pty = None;
                            s.rds_ta = false;
                            s.rds_rt = None;
                            tracing::info!(?mode, "demod mode changed");
                        }
                        SignalPathCommand::SetSquelchThreshold(t) => {
                            squelch.set_threshold_dbfs(t);
                            shared_clone.write().squelch_threshold = t;
                        }
                        SignalPathCommand::StartRecording => {
                            shared_clone.write().is_recording = true;
                        }
                        SignalPathCommand::StopRecording => {
                            shared_clone.write().is_recording = false;
                        }
                        SignalPathCommand::SetZoom(z) => {
                            shared_clone.write().zoom_level = z.clamp(0.01, 1.0);
                        }
                        SignalPathCommand::SetWaterfallSpeed(s) => {
                            shared_clone.write().waterfall_speed = s.clamp(0.1, 10.0);
                        }
                        SignalPathCommand::SetTuneStep(step) => {
                            shared_clone.write().tune_step_hz = step;
                        }
                        SignalPathCommand::AddBookmark(name) => {
                            let (freq, mode) = {
                                let s = shared_clone.read();
                                (s.center_freq_hz, s.demod_mode)
                            };
                            shared_clone.write().bookmarks.push(Bookmark::new(name, freq, mode));
                        }
                        SignalPathCommand::RemoveBookmark(idx) => {
                            let mut s = shared_clone.write();
                            if idx < s.bookmarks.len() {
                                s.bookmarks.remove(idx);
                                if s.bookmark_cursor >= s.bookmarks.len() && !s.bookmarks.is_empty() {
                                    s.bookmark_cursor = s.bookmarks.len() - 1;
                                }
                            }
                        }
                        SignalPathCommand::SetNfmBandwidth(bw) => {
                            nfm_bw_hz = bw;
                            // Rebuild NFM demod with new bandwidth if currently in NFM
                            if matches!(demod, Demod::Nfm(_)) {
                                demod = Demod::Nfm(FmDemodulator::new(sr, 48_000, bw as f32, 0.0));
                                audio_bp.reset();
                                ctcss.reset();
                            }
                            shared_clone.write().nfm_bandwidth_hz = bw;
                        }
                        SignalPathCommand::SetCtcssEnabled(enabled) => {
                            ctcss_enabled = enabled;
                            ctcss.reset();
                            shared_clone.write().ctcss_squelch_enabled = enabled;
                            shared_clone.write().ctcss_tone_detected = false;
                        }
                        // ── Hardware control commands ─────────────────────────
                        SignalPathCommand::SetLnaState(n) => {
                            shared_clone.write().lna_state = n;
                            if let Some(ref tx) = hw_cmd_tx {
                                let _ = tx.try_send(HardwareCommand::SetLnaState(n));
                            }
                        }
                        SignalPathCommand::SetIfGain(g) => {
                            shared_clone.write().if_gain_dbfs = g;
                            if let Some(ref tx) = hw_cmd_tx {
                                let _ = tx.try_send(HardwareCommand::SetIfGain(g));
                            }
                        }
                        SignalPathCommand::SetAgcEnabled(en) => {
                            shared_clone.write().agc_enabled = en;
                            if let Some(ref tx) = hw_cmd_tx {
                                let _ = tx.try_send(HardwareCommand::SetAgcEnabled(en));
                            }
                        }
                        SignalPathCommand::SetAgcSetpoint(sp) => {
                            shared_clone.write().agc_setpoint_dbfs = sp;
                            if let Some(ref tx) = hw_cmd_tx {
                                let _ = tx.try_send(HardwareCommand::SetAgcSetpoint(sp));
                            }
                        }
                        SignalPathCommand::SetBiasT(en) => {
                            shared_clone.write().bias_t_enabled = en;
                            if let Some(ref tx) = hw_cmd_tx {
                                let _ = tx.try_send(HardwareCommand::SetBiasT(en));
                            }
                        }
                        SignalPathCommand::SetHdrMode(en) => {
                            shared_clone.write().hdr_mode = en;
                            if let Some(ref tx) = hw_cmd_tx {
                                let _ = tx.try_send(HardwareCommand::SetHdrMode(en));
                            }
                        }
                        SignalPathCommand::SetAmNotch(en) => {
                            shared_clone.write().am_notch_enabled = en;
                            if let Some(ref tx) = hw_cmd_tx {
                                let _ = tx.try_send(HardwareCommand::SetAmNotch(en));
                            }
                        }
                        SignalPathCommand::SetFmNotch(en) => {
                            shared_clone.write().fm_notch_enabled = en;
                            if let Some(ref tx) = hw_cmd_tx {
                                let _ = tx.try_send(HardwareCommand::SetFmNotch(en));
                            }
                        }
                        SignalPathCommand::SetAntenna(port) => {
                            shared_clone.write().antenna_port = port;
                            if let Some(ref tx) = hw_cmd_tx {
                                let _ = tx.try_send(HardwareCommand::SetAntenna(port));
                            }
                        }
                        SignalPathCommand::Stop => {
                            shared_clone.write().is_running = false;
                            return;
                        }
                    }
                }

                // Receive a batch of IQ samples
                let batch = match iq_rx.recv().await {
                    Ok(b) => b,
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(dropped = n, "signal path lagged — dropped batches");
                        continue;
                    }
                    Err(_) => break, // Source closed
                };

                // Accumulate for FFT
                iq_accumulator.extend_from_slice(&batch);
                if iq_accumulator.len() >= FFT_SIZE {
                    if let Some(mags) = fft.process(&iq_accumulator) {
                        shared_clone.write().fft_magnitudes = mags;
                        if let Some(ref ctx) = egui_ctx {
                            ctx.request_repaint();
                        }
                    }
                    iq_accumulator.drain(..FFT_SIZE);
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
                        shared_clone.write().is_stereo = is_stereo;
                        if rds.process(&composite) {
                            let mut s = shared_clone.write();
                            s.rds_ps_name = rds.data.ps_name.clone();
                            s.rds_pty = rds.data.pty;
                            s.rds_tp = rds.data.tp;
                            s.rds_ta = rds.data.ta;
                            s.rds_rt = rds.data.rt.clone();
                        }
                        frames
                    }
                    Demod::Nfm(d) => {
                        let mono = d.process(&iq_complex);
                        // Run CTCSS detector on raw demodulated audio (before squelch/filter)
                        if ctcss_enabled {
                            ctcss.process_batch(&mono);
                            let detected = ctcss.is_tone_present();
                            shared_clone.write().ctcss_tone_detected = detected;
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

            shared_clone.write().is_running = false;
            tracing::info!("signal path stopped");
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
        assert_eq!(state.fft_magnitudes.len(), FFT_SIZE);
        assert!(state.fft_magnitudes.iter().all(|&v| v <= -100.0));
    }
}
