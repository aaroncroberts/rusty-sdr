#![forbid(unsafe_code)]

//! Signal path: wires source → IQ frontend (FFT) → audio sink.
//!
//! # Topology
//! ```text
//! Source ──broadcast──► IQ Frontend ──┬──► FFT (spectrum display)
//!                                     └──► Audio Sink (cpal)
//!                                     └──► Recorder
//! ```
//!
//! # Communication
//! - Source → IQ frontend: broadcast channel (`Arc<[IqSample]>` batches)
//! - IQ frontend → spectrum: writes into `SharedState.fft_magnitudes` (RwLock)
//! - IQ frontend → audio/record: mpsc channels (`Arc<[StereoFrame]>` batches)
//!
//! # Submodules
//! - [`shared_state`]: All shared data structures read by the UI
//! - [`commands`]: Command enums sent from UI/MIDI to the signal path

mod commands;
mod demod_dispatch;
mod fft_pipeline;
mod scanner;
mod shared_state;

pub use commands::*;
pub use shared_state::*;

use demod_dispatch::{make_demod, Demod};
use fft_pipeline::FftPipeline;
use scanner::{scan_next_bookmark, ScanThreadState};

use parking_lot::RwLock;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use tokio::sync::{broadcast, mpsc};

use rustfft::num_complex::Complex;

use crate::dsp::{
    volume::soft_limit,
    AudioBandpass, CtcssDetector, FirLowpass, RdsDecoder, Squelch, Volume,
};
use crate::sample::{IqSample, StereoFrame};

pub(super) const FFT_SIZE: usize = 2048;
// Smaller frame size = more frequent ring-buffer refills = fewer underruns.
// 256 samples @ 48 kHz = 5.3 ms per chunk (was 1024 = 21.3 ms).
const AUDIO_FRAME_SIZE: usize = 256;

/// Manages the running signal path tasks.
pub struct SignalPath {
    shared: Arc<RwLock<SharedState>>,
    pub cmd_tx: crossbeam_channel::Sender<SignalPathCommand>,
    /// Handles to all running threads/tasks, kept alive until SignalPath drops.
    _handles: Vec<std::thread::JoinHandle<()>>,
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
        let mut hw_cmd_tx = hardware_cmd_tx;
        // iq_rx must be mut so ReconnectSource can swap it at runtime.
        let mut iq_rx = iq_rx;

        // Read initial sample rate before moving shared into the task
        let sample_rate = shared.read().sample_rate_sps;

        // Run the signal path on a dedicated OS thread rather than a Tokio task.
        // The upstream C++ app uses std::thread for DSP for the same reason:
        // real-time audio processing must not share a cooperative scheduler with
        // I/O tasks.  With Tokio, the scheduler can starve the signal path for
        // tens of milliseconds (one UI frame + any blocking I/O), which causes
        // broadcast channel overflows, demodulator resets, and audio dropouts.
        // A preemptive OS thread is scheduled independently and never starved.
        let handle = std::thread::Builder::new()
            .name("sdrapp-signal-path".into())
            .spawn(move || {
            // These are declared `mut` so the Start handler can refresh them
            // after the hardware device sets the effective sample rate (which
            // may differ from the config rate due to hardware decimation).
            let mut sr = sample_rate.max(200_000);
            // WBFM demodulation decimation: run FM demod at ≤250 kHz to cut
            // transcendental-math cost (atan2/sin_cos) by the decimation factor.
            // The FFT still sees the full-rate IQ for wide spectrum display.
            // Factor is chosen so demod_sr is in [200_000, 500_000].
            // FM composite content tops out at 57 kHz (RDS subcarrier);
            // 125 kHz Nyquist (at 250 kHz demod rate) is sufficient with margin.
            let mut wbfm_decim: u32 = (sr / 250_000).max(1);
            let mut demod_sr = sr / wbfm_decim;
            // Narrow-mode decimation: NFM/AM/SSB/CW only need ~200 kHz of IQ
            // bandwidth (max signal is 25 kHz for wide NFM).
            let mut narrow_decim: u32 = (sr / 200_000).max(1);
            let mut narrow_demod_sr: u32 = sr / narrow_decim;
            let mut fftp = FftPipeline::new(FFT_SIZE, crate::dsp::FftWindow::Hann, 4);
            let mut vol = Volume::new(0.8);
            let mut demod: Demod = make_demod(DemodMode::Wbfm, sr, demod_sr, narrow_demod_sr, 12_500);
            let mut squelch = Squelch::new(48_000, -50.0);
            let mut rds = RdsDecoder::new(demod_sr);
            let mut audio_bp = AudioBandpass::voice(48_000.0);
            let mut am_audio_bp = AudioBandpass::am(48_000.0);
            let mut ctcss = CtcssDetector::with_default_threshold(48_000.0);
            let mut nfm_bw_hz: u32 = 12_500;
            let mut ctcss_enabled: bool = false;
            let mut ctcss_was_detected: bool = false;
            let mut audio_accumulator: Vec<StereoFrame> = Vec::with_capacity(AUDIO_FRAME_SIZE * 2);
            // Reuse buffer for mono→stereo conversion in non-WBFM arms (NFM/AM/SSB/CW).
            // clear() + extend() reuses the allocation; first call allocates, rest are free.
            let mut stereo: Vec<StereoFrame> = Vec::with_capacity(512);
            // Track previous values to skip write-lock acquisitions when nothing changed.
            let mut last_is_stereo: bool = false;
            // Rate-limit audio RMS log to once per 5 seconds in WBFM mode.
            let mut last_rms_log = std::time::Instant::now();
            const RMS_LOG_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);
            let mut rms_accum_sq: f64 = 0.0;
            let mut rms_accum_n: u64 = 0;
            // ── Pre-allocated hot-path working buffers ──────────────────────────
            // Reusing these Vec<_>s across iterations eliminates thousands of
            // allocator round-trips per second in the IQ processing hot loop.
            // Capacity is sized for the largest expected batch (sr / callback_rate).
            let max_batch = (sr as usize / 50).max(8192); // ~20 ms @ any supported rate
            let mut iq_complex_buf: Vec<Complex<f32>> = Vec::with_capacity(max_batch);
            // Demodulator audio output buffer (NFM / AM / SSB / CW mono path).
            // clear() + process_into() reuses this allocation; first call may grow, rest are free.
            let mut demod_audio_buf: Vec<f32> = Vec::with_capacity(max_batch);
            // FM composite buffer (WBFM path only — fed to the RDS decoder).
            let mut composite_buf: Vec<f32> = Vec::with_capacity(max_batch);
            // Decimated IQ buffer for WBFM (wbfm_decim×) and a separate one
            // for narrow modes (narrow_decim×).  Keeping them separate avoids
            // borrow-checker conflicts when both slices would otherwise point
            // into the same Vec while the next path also needs to write it.
            let mut iq_decimated_buf: Vec<Complex<f32>> = Vec::with_capacity(max_batch);
            let mut narrow_decimated_buf: Vec<Complex<f32>> = Vec::with_capacity(max_batch);
            // Anti-aliasing FIR lowpass applied before WBFM decimation.
            // Cutoff = 88% of the post-decimation Nyquist (0.88 × demod_sr/2).
            // Example at 2 MSps, 4× decimation: demod_sr=500 kHz, cutoff=220 kHz.
            //   - Passes WBFM signal (±100 kHz bandwidth, including 57 kHz RDS)
            //   - Rejects content above 250 kHz (post-decim Nyquist) that would
            //     alias into the passband; e.g. a station 400 kHz away aliases to
            //     100 kHz without the filter.
            // 127 taps with Kaiser β=6 give ≥60 dB stopband rejection.
            let mut aa_filter: Option<FirLowpass> = if wbfm_decim > 1 {
                let cutoff = 0.88 * (demod_sr as f32 / 2.0);
                Some(FirLowpass::new(cutoff, sr as f32, 127, 6.0))
            } else {
                None
            };
            let mut aa_filter_buf: Vec<Complex<f32>> = Vec::with_capacity(max_batch);
            // Anti-aliasing filter for narrow modes (NFM/AM/SSB/CW).
            // 63 taps (lighter than WBFM's 127) with Kaiser β=6 → ~60 dB rejection.
            // Cutoff = 88% of post-decimation Nyquist, same design rule as WBFM.
            // Only allocated when narrow_decim > 1 (i.e. sr > 200 kHz, always true).
            let mut narrow_aa_filter: Option<FirLowpass> = if narrow_decim > 1 {
                let cutoff = 0.88 * (narrow_demod_sr as f32 / 2.0);
                Some(FirLowpass::new(cutoff, sr as f32, 63, 6.0))
            } else {
                None
            };
            let mut narrow_aa_buf: Vec<Complex<f32>> = Vec::with_capacity(max_batch);
            // All scanner state in one place — bookmark and range-sweep modes.
            let mut sc = ScanThreadState::default();

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
                                demod = make_demod(mode, sr, demod_sr, narrow_demod_sr, nfm_bw_hz);
                                squelch.reset();
                                audio_bp.reset();
                                am_audio_bp.reset();
                                ctcss.reset();
                                rds.reset();
                                // If a range scan is in progress, update the restore target so
                                // the user's explicit choice is honoured when the scan stops/locks.
                                if sc.pre_mode.is_some() {
                                    sc.pre_mode = Some(mode);
                                }
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
                                    demod = make_demod(DemodMode::Nfm, sr, demod_sr, narrow_demod_sr, bw);
                                    audio_bp.reset();
                                    am_audio_bp.reset();
                                    ctcss.reset();
                                    audio_accumulator.clear(); // discard cross-rate samples
                                }
                                shared_clone.write().demod.nfm_bandwidth_hz = bw;
                            }
                            ReceiverCmd::SetCtcssEnabled(enabled) => {
                                ctcss_enabled = enabled;
                                ctcss_was_detected = false;
                                ctcss.reset();
                                shared_clone.write().demod.ctcss_squelch_enabled = enabled;
                                shared_clone.write().demod.ctcss_tone_detected = false;
                            }
                        },
                        SignalPathCommand::Hardware(hw) => {
                            // Update SharedState to mirror the hardware change
                            {
                                let mut s = shared_clone.write();
                                match &hw {
                                    HardwareCommand::SetLnaState(n) => s.hardware.lna_state = *n,
                                    HardwareCommand::SetIfGain(g) => s.hardware.if_gain_dbfs = *g,
                                    HardwareCommand::SetAgcEnabled(en) => {
                                        s.hardware.agc_enabled = *en
                                    }
                                    HardwareCommand::SetAgcSetpoint(sp) => {
                                        s.hardware.agc_setpoint_dbfs = *sp
                                    }
                                    HardwareCommand::SetBiasT(en) => {
                                        s.hardware.bias_t_enabled = *en
                                    }
                                    HardwareCommand::SetHdrMode(en) => s.hardware.hdr_mode = *en,
                                    HardwareCommand::SetAmNotch(en) => {
                                        s.hardware.am_notch_enabled = *en
                                    }
                                    HardwareCommand::SetFmNotch(en) => {
                                        s.hardware.fm_notch_enabled = *en
                                    }
                                    HardwareCommand::SetAntenna(port) => {
                                        s.hardware.antenna_port = *port
                                    }
                                    // RestartDevice has no SharedState mirror — forwarded
                                    // directly to the device thread without a state update.
                                    HardwareCommand::RestartDevice => {}
                                }
                            }
                            // Forward verbatim to the device thread
                            if let Some(ref tx) = hw_cmd_tx {
                                if let Err(crossbeam_channel::TrySendError::Full(dropped)) =
                                    tx.try_send(hw)
                                {
                                    tracing::warn!(
                                        ?dropped,
                                        "hardware command channel full — command dropped"
                                    );
                                }
                            } else {
                                tracing::trace!(
                                    ?hw,
                                    "hardware command dropped — no hardware device connected"
                                );
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
                                if sz.is_power_of_two() && (512..=8192).contains(&sz) {
                                    fftp.resize(sz, fftp.window);
                                    shared_clone.write().fft.fft_size = sz;
                                    shared_clone.write().fft.fft_magnitudes = vec![-120.0; sz];
                                }
                            }
                            DisplayCmd::SetFftWindow(wf) => {
                                fftp.resize(fftp.fft_size, wf);
                                shared_clone.write().fft.fft_window = wf;
                            }
                            DisplayCmd::SetFftAveraging(n) => {
                                fftp.fft_averaging = n.clamp(1, 16);
                                fftp.fft_avg_buf = vec![-120.0; fftp.fft_size];
                                shared_clone.write().fft.fft_averaging = fftp.fft_averaging;
                            }
                            DisplayCmd::SetBandPlanEnabled(en) => {
                                shared_clone.write().fft.band_plan_enabled = en;
                            }
                            DisplayCmd::SetPeakHoldEnabled(en) => {
                                shared_clone.write().fft.peak_hold_enabled = en;
                            }
                            DisplayCmd::SetPeakHoldDecay(db) => {
                                shared_clone.write().fft.peak_hold_decay_db =
                                    db.clamp(0.1, 2.0);
                            }
                        },
                        SignalPathCommand::Bookmark(c) => match c {
                            BookmarkCmd::Add(name) => {
                                let (freq, mode) = {
                                    let s = shared_clone.read();
                                    (s.center_freq_hz, s.demod.demod_mode)
                                };
                                shared_clone
                                    .write()
                                    .bookmarks
                                    .push(Bookmark::new(name, freq, mode));
                            }
                            BookmarkCmd::Remove(idx) => {
                                let mut s = shared_clone.write();
                                if idx < s.bookmarks.len() {
                                    s.bookmarks.remove(idx);
                                    if s.bookmark_cursor >= s.bookmarks.len()
                                        && !s.bookmarks.is_empty()
                                    {
                                        s.bookmark_cursor = s.bookmarks.len() - 1;
                                    }
                                }
                            }
                            BookmarkCmd::Edit(idx, name, freq, mode, cat) => {
                                let mut s = shared_clone.write();
                                if idx < s.bookmarks.len() {
                                    s.bookmarks[idx] = Bookmark {
                                        name,
                                        freq_hz: freq,
                                        mode,
                                        category: cat,
                                    };
                                }
                            }
                        },
                        SignalPathCommand::Scan(c) => match c {
                            ScanCmd::StartRange {
                                freq_lo, freq_hi, step_hz, dwell_secs,
                                squelch_dbfs, mode, stereo_only,
                            } => {
                                sc.range_lo = freq_lo;
                                sc.range_hi = freq_hi;
                                // Guard against zero step which would stall the
                                // scanner thread in an infinite loop.
                                sc.range_step = step_hz.max(1);
                                sc.range_squelch = squelch_dbfs;
                                sc.range_stereo_only = stereo_only;
                                sc.dwell_secs = dwell_secs.clamp(0.1, 10.0);
                                sc.range_freq = freq_lo;
                                sc.range_mode = true;
                                sc.running = true;
                                sc.dwell_samples = 0;
                                {
                                    let mut s = shared_clone.write();
                                    // Save current demod mode so we can restore it when scan stops.
                                    sc.pre_mode = Some(s.demod.demod_mode);
                                    s.scanner.scan_running = true;
                                    s.scanner.range_mode = true;
                                    s.scanner.range_freq_hz = freq_lo;
                                    s.scanner.range_freq_lo = freq_lo;
                                    s.scanner.range_freq_hi = freq_hi;
                                    s.scanner.range_step_hz = step_hz;
                                    s.scanner.range_squelch_dbfs = squelch_dbfs;
                                    s.scanner.range_stereo_only = stereo_only;
                                    s.scanner.scan_dwell_secs = dwell_secs;
                                    s.scanner.last_locked_freq_hz = None; // clear previous lock
                                    s.center_freq_hz = freq_lo;
                                    // Clear stale signal level so first dwell doesn't false-lock.
                                    s.fft.signal_level_dbfs = -120.0;
                                }
                                if let Some(ref atomic) = freq_atomic_clone {
                                    atomic.store(freq_lo, Ordering::Relaxed);
                                }
                                demod = make_demod(mode, sr, demod_sr, narrow_demod_sr, nfm_bw_hz);
                                shared_clone.write().demod.demod_mode = mode;
                                demod.reset();
                                rds.reset();
                                audio_accumulator.clear();
                                fftp.clear_accumulator();
                                tracing::info!(
                                    freq_lo, freq_hi, step_hz, squelch_dbfs, stereo_only,
                                    "FM range scanner started"
                                );
                            }
                            ScanCmd::Start(cat) => {
                                sc.category = cat.clone();
                                sc.running = true;
                                sc.cursor = 0;
                                sc.dwell_samples = 0;
                                {
                                    let mut s = shared_clone.write();
                                    s.scanner.scan_running = true;
                                    s.scanner.scan_category = cat;
                                    s.scanner.scan_cursor = 0;
                                }
                                let first = {
                                    let s = shared_clone.read();
                                    scan_next_bookmark(&s.bookmarks, &sc.category, sc.cursor)
                                };
                                if let Some((idx, bm_freq, bm_mode)) = first {
                                    sc.cursor = idx;
                                    shared_clone.write().scanner.scan_cursor = idx;
                                    shared_clone.write().center_freq_hz = bm_freq;
                                    if let Some(ref atomic) = freq_atomic_clone {
                                        atomic.store(bm_freq, Ordering::Relaxed);
                                    }
                                    demod = make_demod(bm_mode, sr, demod_sr, narrow_demod_sr, nfm_bw_hz);
                                    shared_clone.write().demod.demod_mode = bm_mode;
                                    demod.reset();
                                    rds.reset();
                                    audio_accumulator.clear();
                                    fftp.clear_accumulator();
                                    tracing::debug!(
                                        category = %sc.category,
                                        first_freq_hz = bm_freq,
                                        dwell_secs = sc.dwell_secs,
                                        "scanner started"
                                    );
                                } else {
                                    tracing::warn!(
                                        category = %sc.category,
                                        "scanner started but no matching bookmarks found — stopping"
                                    );
                                    sc.running = false;
                                    shared_clone.write().scanner.scan_running = false;
                                }
                            }
                            ScanCmd::Stop => {
                                tracing::debug!("scanner stopped");
                                sc.running = false;
                                sc.range_mode = false;
                                let mut s = shared_clone.write();
                                s.scanner.scan_running = false;
                                s.scanner.range_mode = false;
                                // Restore demod mode that was active before the range scan.
                                if let Some(prev_mode) = sc.pre_mode.take() {
                                    s.demod.demod_mode = prev_mode;
                                    demod = make_demod(prev_mode, sr, demod_sr, narrow_demod_sr, nfm_bw_hz);
                                }
                            }
                            ScanCmd::Next => {
                                if sc.running {
                                    tracing::debug!("scanner: manual next requested");
                                    sc.dwell_samples = u64::MAX;
                                }
                            }
                            ScanCmd::SetDwell(secs) => {
                                sc.dwell_secs = secs.clamp(0.5, 30.0);
                                shared_clone.write().scanner.scan_dwell_secs = sc.dwell_secs;
                            }
                        },
                        SignalPathCommand::StartRecording => {
                            shared_clone.write().is_recording = true;
                        }
                        SignalPathCommand::StopRecording => {
                            shared_clone.write().is_recording = false;
                        }
                        SignalPathCommand::Start => {
                            if paused {
                                paused = false;
                                fftp.clear_accumulator();
                                audio_accumulator.clear();
                                // Drain IQ that accumulated while the hardware was
                                // initialising (typically ~3 s worth of batches).
                                // Without this drain, blocking_recv() returns Lagged(N)
                                // immediately on every call for several seconds, causing
                                // repeated demod.reset() and preventing the FM PLL from
                                // ever locking → silent audio.
                                let mut drained = 0u64;
                                while iq_rx.try_recv().is_ok() {
                                    drained += 1;
                                }
                                if drained > 0 {
                                    tracing::info!(drained, "drained stale IQ batches on start");
                                }

                                // Re-read the sample rate — the SDRplay device writes
                                // the effective (post-hardware-decimation) rate to
                                // SharedState after connecting, which happens AFTER
                                // this thread starts.  Without this refresh, the
                                // signal path uses the config rate (2 MHz) while the
                                // hardware delivers 500 kHz IQ, producing a 8× ratio
                                // mismatch that corrupts all demodulator coefficients.
                                let live_sr = shared_clone.read().sample_rate_sps.max(200_000);
                                if live_sr != sr {
                                    tracing::info!(
                                        old_sr = sr,
                                        new_sr = live_sr,
                                        "sample rate changed — rebuilding signal path filters and demodulators"
                                    );
                                    sr = live_sr;
                                    wbfm_decim = (sr / 250_000).max(1);
                                    demod_sr = sr / wbfm_decim;
                                    narrow_decim = (sr / 200_000).max(1);
                                    narrow_demod_sr = sr / narrow_decim;
                                    aa_filter = if wbfm_decim > 1 {
                                        let cutoff = 0.88 * (demod_sr as f32 / 2.0);
                                        Some(FirLowpass::new(cutoff, sr as f32, 127, 6.0))
                                    } else {
                                        None
                                    };
                                    narrow_aa_filter = if narrow_decim > 1 {
                                        let cutoff = 0.88 * (narrow_demod_sr as f32 / 2.0);
                                        Some(FirLowpass::new(cutoff, sr as f32, 63, 6.0))
                                    } else {
                                        None
                                    };
                                    let current_mode = shared_clone.read().demod.demod_mode;
                                    demod = make_demod(current_mode, sr, demod_sr, narrow_demod_sr, nfm_bw_hz);
                                    rds = RdsDecoder::new(demod_sr);
                                }

                                shared_clone.write().is_running = true;
                                tracing::info!("signal path started");
                            } else {
                                tracing::debug!("Start received while already running — ignored");
                            }
                        }
                        SignalPathCommand::Stop => {
                            if !paused {
                                paused = true;
                                let mut s = shared_clone.write();
                                s.is_running = false;
                                // Halt the scanner so the SCAN badge clears and the
                                // dwell timer doesn't silently stall while paused.
                                if s.scanner.scan_running {
                                    s.scanner.scan_running = false;
                                    sc.running = false;
                                }
                                tracing::info!("signal path stopped");
                            } else {
                                tracing::debug!("Stop received while already stopped — ignored");
                            }
                        }
                        SignalPathCommand::ReconnectSource {
                            iq_rx: new_rx,
                            hardware_cmd_tx: new_hw_tx,
                        } => {
                            iq_rx = new_rx;
                            hw_cmd_tx = new_hw_tx;
                            source_dead = false;
                            fftp.clear_accumulator();
                            audio_accumulator.clear();
                            tracing::info!("IQ source hot-swapped — signal path live");

                            // Re-apply current state to the new hardware device so it
                            // comes up at the user's current frequency and gain settings.
                            let (freq, hw_snap, demod_mode) = {
                                let s = shared_clone.read();
                                (s.center_freq_hz, s.hardware.clone(), s.demod.demod_mode)
                            };
                            if let Some(ref atomic) = freq_atomic_clone {
                                atomic.store(freq, Ordering::Relaxed);
                            }
                            if let Some(ref tx) = hw_cmd_tx {
                                let cmds = [
                                    HardwareCommand::SetLnaState(hw_snap.lna_state),
                                    HardwareCommand::SetIfGain(hw_snap.if_gain_dbfs),
                                    HardwareCommand::SetAgcEnabled(hw_snap.agc_enabled),
                                    HardwareCommand::SetAgcSetpoint(hw_snap.agc_setpoint_dbfs),
                                    HardwareCommand::SetBiasT(hw_snap.bias_t_enabled),
                                    HardwareCommand::SetHdrMode(hw_snap.hdr_mode),
                                    HardwareCommand::SetAmNotch(hw_snap.am_notch_enabled),
                                    HardwareCommand::SetFmNotch(hw_snap.fm_notch_enabled),
                                    HardwareCommand::SetAntenna(hw_snap.antenna_port),
                                ];
                                for cmd in cmds {
                                    let _ = tx.try_send(cmd);
                                }
                                tracing::debug!(
                                    freq_hz = freq,
                                    ?demod_mode,
                                    lna = hw_snap.lna_state,
                                    agc = hw_snap.agc_enabled,
                                    "hardware state re-applied to new device"
                                );
                            }
                            // Rebuild demodulator for current mode (new device, fresh state)
                            demod = make_demod(demod_mode, sr, demod_sr, narrow_demod_sr, nfm_bw_hz);
                            demod.reset();
                            rds.reset();
                        }
                    }
                }

                // When the source is dead, sleep briefly and loop to keep
                // processing commands (so Start/Stop/freq changes still work).
                if source_dead {
                    std::thread::sleep(std::time::Duration::from_millis(16));
                    continue;
                }

                // Receive a batch of IQ samples — blocking call on this OS thread.
                // Unlike Tokio's .await, blocking_recv() truly sleeps when no
                // data is available, yielding the CPU to other threads.
                let batch = match iq_rx.blocking_recv() {
                    Ok(b) => b,
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        shared_clone.write().device_diagnostics.iq_lag_count += 1;
                        // Calculate how much audio was dropped.
                        // Only reset demodulator state for large gaps (>100 ms) where
                        // the FM discriminator phase continuity is definitely broken.
                        // For small gaps (<= 10 batches ≈ 20 ms at 500 kHz / 1024 samp),
                        // the demodulator can recover on its own — keeping state
                        // avoids audio glitches from repeated PLL resync.
                        let dropped_ms =
                            n as f64 * 1024.0 * 1000.0 / sr.max(1) as f64;
                        if dropped_ms > 100.0 {
                            tracing::warn!(
                                dropped = n,
                                dropped_ms = dropped_ms as u32,
                                "signal path lagged — large gap, resetting demod"
                            );
                            demod.reset();
                            rds.reset();
                            audio_accumulator.clear();
                            last_is_stereo = false;
                            if let Some(ref mut f) = aa_filter {
                                f.reset();
                            }
                            if let Some(ref mut f) = narrow_aa_filter {
                                f.reset();
                            }
                            narrow_aa_buf.clear();
                        } else {
                            tracing::debug!(
                                dropped = n,
                                dropped_ms = dropped_ms as u32,
                                "signal path minor lag — clearing prev only"
                            );
                            // IQ continuity is broken across the gap.  The FM
                            // discriminator computes arg(prev* × s) — if prev is
                            // from before the gap, the first sample after the gap
                            // produces a random phase spike (up to ±π).  Clear only
                            // prev so the spike is suppressed without flushing the
                            // PLL lock or filter state.
                            demod.clear_prev();
                        }
                        // Sleep briefly so the SDRplay callback thread can
                        // refill the broadcast channel and we don't spin at
                        // 100% CPU.  blocking_recv() on a Lagged channel
                        // returns the oldest buffered item immediately (no
                        // blocking), which would otherwise create a hot loop.
                        std::thread::sleep(std::time::Duration::from_millis(2));
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

                // FFT: accumulate → EMA average → rate-limited shared-state write (~30 Hz).
                fftp.tick(&batch, &shared_clone, &egui_ctx);

                // ── Scanner tick ─────────────────────────────────────────────
                if sc.running {
                    sc.dwell_samples += batch.len() as u64;
                    let dwell_target = (sc.dwell_secs * sr as f32) as u64;
                    if sc.dwell_samples >= dwell_target {
                        sc.dwell_samples = 0;

                        // ── Range sweep mode ─────────────────────────────────
                        if sc.range_mode {
                            let signal_level = shared_clone.read().fft.signal_level_dbfs;
                            let is_stereo = shared_clone.read().rds.is_stereo;
                            let locked = signal_level >= sc.range_squelch
                                && (!sc.range_stereo_only || is_stereo);

                            if locked {
                                tracing::info!(
                                    freq_hz = sc.range_freq,
                                    signal_level_dbfs = signal_level,
                                    is_stereo,
                                    "FM range scanner: station locked"
                                );
                                sc.running = false;
                                sc.range_mode = false;
                                {
                                    let mut s = shared_clone.write();
                                    s.scanner.scan_running = false;
                                    s.scanner.range_mode = false;
                                    s.scanner.last_locked_freq_hz = Some(sc.range_freq);
                                    // Restore the demod mode that was active before the scan.
                                    // This ensures e.g. NFM users aren't left in WBFM after an
                                    // FM band scan; the locked frequency is held but mode reverts.
                                    if let Some(prev_mode) = sc.pre_mode.take() {
                                        s.demod.demod_mode = prev_mode;
                                        demod = make_demod(prev_mode, sr, demod_sr, narrow_demod_sr, nfm_bw_hz);
                                    }
                                }
                            } else {
                                // Advance to next frequency, wrap around.
                                let next = sc.range_freq + sc.range_step;
                                sc.range_freq = if next > sc.range_hi {
                                    sc.range_lo
                                } else {
                                    next
                                };
                                tracing::debug!(
                                    freq_hz = sc.range_freq,
                                    signal_level_dbfs = signal_level,
                                    "FM range scanner: advancing"
                                );
                                {
                                    let mut s = shared_clone.write();
                                    s.scanner.range_freq_hz = sc.range_freq;
                                    s.center_freq_hz = sc.range_freq;
                                    // Clear stale signal level so the dwell at the new
                                    // frequency doesn't read a value from the old frequency.
                                    s.fft.signal_level_dbfs = -120.0;
                                }
                                if let Some(ref atomic) = freq_atomic_clone {
                                    atomic.store(sc.range_freq, Ordering::Relaxed);
                                }
                                // Reset demod so no stale audio bleeds into the new frequency.
                                demod.reset();
                                rds.reset();
                                audio_accumulator.clear();
                                fftp.clear_accumulator();
                                last_is_stereo = false;
                            }
                            continue;
                        }

                        // ── Bookmark mode ────────────────────────────────────
                        let next_idx = sc.cursor + 1;
                        let (bm_freq, bm_mode, new_idx) = {
                            let s = shared_clone.read();
                            if let Some((idx, freq, mode)) =
                                scan_next_bookmark(&s.bookmarks, &sc.category, next_idx)
                            {
                                (freq, mode, idx)
                            } else if let Some((idx, freq, mode)) =
                                scan_next_bookmark(&s.bookmarks, &sc.category, 0)
                            {
                                (freq, mode, idx) // wrap around
                            } else {
                                // No bookmarks → stop scan
                                (0, DemodMode::Wbfm, usize::MAX)
                            }
                        };
                        if new_idx == usize::MAX {
                            tracing::warn!(category = %sc.category, "scanner: no bookmarks to advance to — stopping");
                            sc.running = false;
                            shared_clone.write().scanner.scan_running = false;
                        } else {
                            tracing::debug!(
                                new_freq_hz = bm_freq,
                                cursor = new_idx,
                                dwell_secs = sc.dwell_secs,
                                "scanner: dwell expired, advancing to next bookmark"
                            );
                            sc.cursor = new_idx;
                            shared_clone.write().scanner.scan_cursor = new_idx;
                            shared_clone.write().center_freq_hz = bm_freq;
                            if let Some(ref atomic) = freq_atomic_clone {
                                atomic.store(bm_freq, Ordering::Relaxed);
                            }
                            demod = make_demod(bm_mode, sr, demod_sr, narrow_demod_sr, nfm_bw_hz);
                            shared_clone.write().demod.demod_mode = bm_mode;
                            demod.reset();
                            rds.reset();
                            // Clear accumulators so no stale audio/IQ bleeds into the new channel.
                            audio_accumulator.clear();
                            fftp.clear_accumulator();
                        }
                    }
                }

                // Demodulate IQ → StereoFrame batches at 48 kHz.
                // For WBFM, anti-alias filter then decimate to demod_sr before the
                // FM discriminator to reduce atan2/sin_cos calls by wbfm_decim×.
                // For NFM/AM/SSB/CW, anti-alias filter then decimate to ~200 kHz
                // (narrow_demod_sr) for the same CPU savings on narrow-band modes.
                //
                // All buffers are pre-allocated and reused — no per-batch heap allocs.
                iq_complex_buf.clear();
                iq_complex_buf.extend(batch.iter().map(|s| Complex::new(s.re, s.im)));

                // Build the decimated IQ view for WBFM demodulation.
                let iq_for_demod: &[Complex<f32>] = if wbfm_decim > 1 {
                    // 1. Anti-aliasing lowpass (prevents adjacent-station aliasing)
                    if let Some(ref mut fir) = aa_filter {
                        fir.process(&iq_complex_buf, &mut aa_filter_buf);
                    } else {
                        aa_filter_buf.clear();
                        aa_filter_buf.extend_from_slice(&iq_complex_buf);
                    }
                    // 2. Integer decimation (boxcar average, now alias-free)
                    let d = wbfm_decim as usize;
                    let scale = 1.0 / d as f32;
                    iq_decimated_buf.clear();
                    iq_decimated_buf.extend(
                        aa_filter_buf.chunks(d).map(|c| {
                            c.iter().fold(Complex::new(0.0_f32, 0.0), |a, &b| a + b) * scale
                        }),
                    );
                    &iq_decimated_buf
                } else {
                    // No decimation — pass full-rate IQ directly (no clone)
                    &iq_complex_buf
                };

                // Build the decimated IQ view for narrow-mode demodulation
                // (NFM/AM/SSB/CW).  Same AA+decimate pattern as WBFM above.
                // Only computed when the active demod is not WBFM — skip entirely
                // in WBFM mode to avoid wasted AA filter work each batch.
                let iq_for_narrow: &[Complex<f32>] =
                    if narrow_decim > 1 && !matches!(demod, Demod::Wbfm(_)) {
                        // 1. Anti-aliasing lowpass
                        if let Some(ref mut fir) = narrow_aa_filter {
                            fir.process(&iq_complex_buf, &mut narrow_aa_buf);
                        } else {
                            narrow_aa_buf.clear();
                            narrow_aa_buf.extend_from_slice(&iq_complex_buf);
                        }
                        // 2. Integer decimation: average narrow_decim samples.
                        // (The AA filter has already removed content that would alias.)
                        let d = narrow_decim as usize;
                        let scale = 1.0 / d as f32;
                        narrow_decimated_buf.clear();
                        narrow_decimated_buf.extend(
                            narrow_aa_buf.chunks(d).map(|c| {
                                c.iter().fold(Complex::new(0.0_f32, 0.0), |a, &b| a + b) * scale
                            }),
                        );
                        &narrow_decimated_buf
                    } else {
                        &iq_complex_buf
                    };

                match &mut demod {
                    Demod::Wbfm(d) => {
                        // Zero-alloc: write stereo frames and FM composite directly into
                        // pre-allocated buffers — no per-batch Vec construction.
                        let is_stereo = d.process_with_composite_into(
                            iq_for_demod,
                            &mut stereo,
                            &mut composite_buf,
                        );
                        // Only acquire write lock when stereo status actually changes.
                        if is_stereo != last_is_stereo {
                            shared_clone.write().rds.is_stereo = is_stereo;
                            last_is_stereo = is_stereo;
                            if is_stereo {
                                tracing::info!("WBFM: stereo pilot acquired — locked to FM station");
                            } else {
                                tracing::info!("WBFM: stereo pilot lost — signal weak or noise");
                            }
                        }
                        // Accumulate audio RMS; log every 5 seconds so we can confirm
                        // real audio vs static from the terminal output.
                        for f in &stereo {
                            rms_accum_sq += (f.left * f.left + f.right * f.right) as f64;
                            rms_accum_n += 2;
                        }
                        if last_rms_log.elapsed() >= RMS_LOG_INTERVAL && rms_accum_n > 0 {
                            let rms = (rms_accum_sq / rms_accum_n as f64).sqrt() as f32;
                            tracing::info!(
                                rms = format_args!("{rms:.4}"),
                                stereo = is_stereo,
                                "WBFM audio level"
                            );
                            rms_accum_sq = 0.0;
                            rms_accum_n = 0;
                            last_rms_log = std::time::Instant::now();
                        }
                        if rds.process(&composite_buf) {
                            let mut s = shared_clone.write();
                            s.rds.ps_name = rds.data.ps_name.clone();
                            s.rds.pty = rds.data.pty;
                            s.rds.tp = rds.data.tp;
                            s.rds.ta = rds.data.ta;
                            s.rds.rt = rds.data.rt.clone();
                        }
                    }
                    Demod::Nfm(d) => {
                        // Zero-alloc: process_into reuses demod_audio_buf.
                        d.process_into(iq_for_narrow, &mut demod_audio_buf);
                        // Run CTCSS detector on raw demodulated audio (before squelch/filter)
                        if ctcss_enabled {
                            ctcss.process_batch(&demod_audio_buf);
                            let detected = ctcss.is_tone_present();
                            if detected != ctcss_was_detected {
                                if detected {
                                    tracing::debug!("CTCSS: tone detected — squelch open");
                                } else {
                                    tracing::debug!("CTCSS: tone lost — squelch closed");
                                }
                                ctcss_was_detected = detected;
                            }
                            shared_clone.write().demod.ctcss_tone_detected = detected;
                        }
                        // Squelch in-place — no allocation, updates EMA and gates demod_audio_buf.
                        squelch.process_inplace(&mut demod_audio_buf);
                        shared_clone.write().demod.nfm_signal_level_dbfs = squelch.level_dbfs();
                        // CTCSS gate: mute in-place if enabled and no tone detected.
                        if ctcss_enabled && !ctcss.is_tone_present() {
                            demod_audio_buf.iter_mut().for_each(|s| *s = 0.0);
                        }
                        // Voice bandpass: 300 Hz – 3 kHz
                        audio_bp.process_inplace(&mut demod_audio_buf);
                        // Reuse stereo buffer — clear+extend avoids a per-batch allocation.
                        stereo.clear();
                        stereo.extend(demod_audio_buf.iter().copied().map(StereoFrame::mono));
                    }
                    Demod::Am(d) => {
                        d.process_into(iq_for_narrow, &mut demod_audio_buf);
                        am_audio_bp.process_inplace(&mut demod_audio_buf);
                        stereo.clear();
                        stereo.extend(demod_audio_buf.iter().copied().map(StereoFrame::mono));
                    }
                    Demod::Ssb(d) => {
                        d.process_into(iq_for_narrow, &mut demod_audio_buf);
                        stereo.clear();
                        stereo.extend(demod_audio_buf.iter().copied().map(StereoFrame::mono));
                    }
                    Demod::Cw(d) => {
                        d.process_into(iq_for_narrow, &mut demod_audio_buf);
                        stereo.clear();
                        stereo.extend(demod_audio_buf.iter().copied().map(StereoFrame::mono));
                    }
                }

                // Apply volume and soft-limit in-place, then extend accumulator.
                // vol.apply() avoids the intermediate Vec that vol.process() created;
                // extend_from_slice is cheaper than into_iter().map().
                vol.apply(&mut stereo);
                for f in &mut stereo {
                    f.left = soft_limit(f.left);
                    f.right = soft_limit(f.right);
                }
                audio_accumulator.extend_from_slice(&stereo);

                // Emit audio frames — drain directly into Arc<[StereoFrame]> to
                // avoid the intermediate Vec allocation that .to_vec().into() caused.
                while audio_accumulator.len() >= AUDIO_FRAME_SIZE {
                    let frame: Arc<[StereoFrame]> =
                        audio_accumulator.drain(..AUDIO_FRAME_SIZE).collect();

                    if let Some(ref tx) = audio_tx {
                        // try_send: drop frame on backpressure rather than blocking.
                        if tx.try_send(Arc::clone(&frame)).is_err() {
                            shared_clone.write().audio_frames_dropped += 1;
                        }
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
        })
        .expect("failed to spawn signal path thread");

        Self {
            shared,
            cmd_tx,
            _handles: vec![handle],
        }
    }

    /// Send a command to the signal path (non-blocking).
    pub fn send_command(&self, cmd: SignalPathCommand) {
        let _ = self.cmd_tx.try_send(cmd);
    }

    /// Access the shared state arc.
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

// ── Zoom helpers ─────────────────────────────────────────────────────────────

/// Returns the minimum allowed zoom level for the given demodulation mode.
///


#[cfg(test)]
mod tests;
