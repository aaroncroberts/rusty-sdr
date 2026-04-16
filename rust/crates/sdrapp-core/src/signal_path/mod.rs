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
mod shared_state;

pub use commands::*;
pub use shared_state::*;

use parking_lot::RwLock;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;

use rustfft::num_complex::Complex;

use crate::dsp::{
    volume::soft_limit, AmDemodulator, AudioBandpass, CtcssDetector, CwDemodulator, FftProcessor,
    FmDemodulator, RdsDecoder, Squelch, SsbDemodulator, SsbMode, StereoFmDecoder, Volume,
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
        let mut hw_cmd_tx = hardware_cmd_tx;
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
                    Self::Wbfm(d) => d.reset(),
                    Self::Nfm(d) => d.reset(),
                    Self::Am(d) => d.reset(),
                    Self::Ssb(d) => d.reset(),
                    Self::Cw(d) => d.reset(),
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
            let mut ctcss_was_detected: bool = false;
            let mut iq_accumulator: Vec<IqSample> = Vec::with_capacity(FFT_SIZE * 2);
            let mut audio_accumulator: Vec<StereoFrame> = Vec::with_capacity(AUDIO_FRAME_SIZE * 2);
            // Scanner state
            let mut scan_running = false;
            let mut scan_cursor: usize = 0;
            let mut scan_category = String::new();
            let mut scan_dwell_secs: f32 = 2.0;
            // Accumulates IQ sample count for dwell timer; compare to sample_rate * dwell_secs
            let mut scan_dwell_samples: u64 = 0;

            /// Create a fresh demodulator for the given mode.
            fn make_demod(mode: DemodMode, sr: u32, nfm_bw_hz: u32) -> Demod {
                match mode {
                    DemodMode::Wbfm => Demod::Wbfm(StereoFmDecoder::new(sr)),
                    DemodMode::Nfm => {
                        Demod::Nfm(FmDemodulator::new(sr, 48_000, nfm_bw_hz as f32, 0.0))
                    }
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
                                    demod =
                                        Demod::Nfm(FmDemodulator::new(sr, 48_000, bw as f32, 0.0));
                                    audio_bp.reset();
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
                                fft_averaging = n.clamp(1, 16);
                                fft_avg_buf = vec![-120.0; fft_size];
                                shared_clone.write().fft.fft_averaging = fft_averaging;
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
                                    audio_accumulator.clear();
                                    iq_accumulator.clear();
                                    tracing::debug!(
                                        category = %scan_category,
                                        first_freq_hz = bm_freq,
                                        dwell_secs = scan_dwell_secs,
                                        "scanner started"
                                    );
                                } else {
                                    tracing::warn!(
                                        category = %scan_category,
                                        "scanner started but no matching bookmarks found — stopping"
                                    );
                                    scan_running = false;
                                    shared_clone.write().scanner.scan_running = false;
                                }
                            }
                            ScanCmd::Stop => {
                                tracing::debug!("scanner stopped");
                                scan_running = false;
                                shared_clone.write().scanner.scan_running = false;
                            }
                            ScanCmd::Next => {
                                if scan_running {
                                    tracing::debug!("scanner: manual next requested");
                                    scan_dwell_samples = u64::MAX;
                                }
                            }
                            ScanCmd::SetDwell(secs) => {
                                scan_dwell_secs = secs.clamp(0.5, 30.0);
                                shared_clone.write().scanner.scan_dwell_secs = scan_dwell_secs;
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
                                shared_clone.write().is_running = true;
                                iq_accumulator.clear();
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
                                    scan_running = false;
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
                            iq_accumulator.clear();
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
                            demod = make_demod(demod_mode, sr, nfm_bw_hz);
                            demod.reset();
                            rds.reset();
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
                        // Reset demodulator state so stale `prev` doesn't produce
                        // a huge phase-jump click on the next received batch.
                        demod.reset();
                        rds.reset();
                        audio_accumulator.clear();
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
                        // Passband metrics: SNR and signal level for S-meter.
                        // half_bw_bins is the half-bandwidth of the active demod
                        // channel expressed in FFT bins. For WBFM it's widened by
                        // 1.5× to capture stereo-subcarrier sidebands.
                        let (snr, signal_level_dbfs) = {
                            let n = fft_avg_buf.len();
                            let center = n / 2;
                            let (bw_hz, is_wbfm) = {
                                let s = shared_clone.read();
                                let bw = match s.demod.demod_mode {
                                    DemodMode::Wbfm => 200_000_u32,
                                    DemodMode::Nfm => s.demod.nfm_bandwidth_hz,
                                    DemodMode::Am
                                    | DemodMode::Usb
                                    | DemodMode::Lsb
                                    | DemodMode::Dsb => 10_000,
                                    DemodMode::Cw => 1_000,
                                };
                                let wbfm = s.demod.demod_mode == DemodMode::Wbfm;
                                (bw, wbfm)
                            };
                            let sr_hz = shared_clone.read().sample_rate_sps.max(1) as f32;
                            let mut half_bw_bins =
                                ((bw_hz as f32 / sr_hz * n as f32) as usize)
                                    .max(2)
                                    .min(n / 4);
                            if is_wbfm {
                                half_bw_bins = (half_bw_bins * 3 / 2).min(n / 4);
                            }
                            let snr = compute_snr_db(&fft_avg_buf, center, half_bw_bins);
                            // Peak value within the passband → drives the S-meter.
                            let lo = center.saturating_sub(half_bw_bins);
                            let hi = (center + half_bw_bins + 1).min(n);
                            let sig = fft_avg_buf[lo..hi]
                                .iter()
                                .cloned()
                                .fold(f32::NEG_INFINITY, f32::max);
                            (snr, sig)
                        };
                        let clipping = any_bin_clipping(&fft_avg_buf);
                        {
                            let mut s = shared_clone.write();
                            s.fft.fft_magnitudes = fft_avg_buf.clone();
                            s.fft.snr_db = Some(snr);
                            s.fft.fft_clipping_detected = clipping;
                            s.fft.signal_level_dbfs = signal_level_dbfs;
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
                    let dwell_target = (scan_dwell_secs * sr as f32) as u64;
                    if scan_dwell_samples >= dwell_target {
                        scan_dwell_samples = 0;
                        // Advance to next matching bookmark.
                        let next_idx = scan_cursor + 1;
                        let (bm_freq, bm_mode, new_idx) = {
                            let s = shared_clone.read();
                            if let Some((idx, freq, mode)) =
                                scan_next_bookmark(&s.bookmarks, &scan_category, next_idx)
                            {
                                (freq, mode, idx)
                            } else if let Some((idx, freq, mode)) =
                                scan_next_bookmark(&s.bookmarks, &scan_category, 0)
                            {
                                (freq, mode, idx) // wrap around
                            } else {
                                // No bookmarks → stop scan
                                (0, DemodMode::Wbfm, usize::MAX)
                            }
                        };
                        if new_idx == usize::MAX {
                            tracing::warn!(category = %scan_category, "scanner: no bookmarks to advance to — stopping");
                            scan_running = false;
                            shared_clone.write().scanner.scan_running = false;
                        } else {
                            tracing::debug!(
                                new_freq_hz = bm_freq,
                                cursor = new_idx,
                                dwell_secs = scan_dwell_secs,
                                "scanner: dwell expired, advancing to next bookmark"
                            );
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
                            // Clear accumulators so no stale audio/IQ bleeds into the new channel.
                            audio_accumulator.clear();
                            iq_accumulator.clear();
                        }
                    }
                }

                // Demodulate IQ → StereoFrame batches at 48 kHz.
                let iq_complex: Vec<Complex<f32>> =
                    batch.iter().map(|s| Complex::new(s.re, s.im)).collect();

                let stereo: Vec<StereoFrame> = match &mut demod {
                    Demod::Wbfm(d) => {
                        let (frames, is_stereo, composite) = d.process_with_composite(&iq_complex);
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
                        // Apply squelch (dBFS threshold gate)
                        let gated = squelch.process(&mono);
                        shared_clone.write().demod.nfm_signal_level_dbfs = squelch.level_dbfs();
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

                let mut stereo_processed: Vec<StereoFrame> = vol
                    .process(&stereo)
                    .into_iter()
                    .map(|f| StereoFrame {
                        left: soft_limit(f.left),
                        right: soft_limit(f.right),
                    })
                    .collect();
                audio_accumulator.append(&mut stereo_processed);

                // Emit audio frames
                while audio_accumulator.len() >= AUDIO_FRAME_SIZE {
                    let frame: Arc<[StereoFrame]> =
                        audio_accumulator[..AUDIO_FRAME_SIZE].to_vec().into();
                    audio_accumulator.drain(..AUDIO_FRAME_SIZE);

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
        });

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
/// Prevents the user from zooming so tight that the active signal becomes
/// invisible in the spectrum panel:
/// * WBFM needs ≥ 5 % of hardware bandwidth (≥ 100 kHz on a 2 MHz SDR)
/// * CW is a very narrow mode but still needs context — keep at 5 %
/// * Narrowband modes (NFM, AM, SSB) can zoom tighter but stop at 2 %
pub fn min_zoom_for_mode(mode: DemodMode) -> f32 {
    match mode {
        DemodMode::Wbfm | DemodMode::Cw => 0.05,
        DemodMode::Nfm
        | DemodMode::Am
        | DemodMode::Usb
        | DemodMode::Lsb
        | DemodMode::Dsb => 0.02,
    }
}

// ── FFT analysis helpers ──────────────────────────────────────────────────────

/// Compute SNR (dB) for a signal centred at `center` bin with half-width
/// `half_bw_bins`.
///
/// * Signal power  = max bin in `[center−half_bw, center+half_bw]`
/// * Noise floor   = median of all bins **outside** that window
///
/// Extracted from the signal-path loop so it is unit-testable without spinning
/// up the full async machinery.
pub(crate) fn compute_snr_db(bins: &[f32], center: usize, half_bw_bins: usize) -> f32 {
    let n = bins.len();
    let sig_lo = center.saturating_sub(half_bw_bins);
    let sig_hi = (center + half_bw_bins).min(n - 1);
    let peak = bins[sig_lo..=sig_hi]
        .iter()
        .cloned()
        .fold(f32::NEG_INFINITY, f32::max);
    let mut noise: Vec<f32> = bins[..sig_lo]
        .iter()
        .chain(bins[sig_hi + 1..].iter())
        .cloned()
        .collect();
    let noise_floor = if noise.is_empty() {
        -120.0_f32
    } else {
        noise.sort_by(|a, b| a.partial_cmp(b).unwrap());
        noise[noise.len() / 2]
    };
    peak - noise_floor
}

/// Returns `true` when any bin in `bins` has reached or exceeded 0 dBFS —
/// a reliable indicator of ADC saturation.
#[inline]
pub(crate) fn any_bin_clipping(bins: &[f32]) -> bool {
    bins.iter().any(|&v| v >= 0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── audio_frames_dropped ─────────────────────────────────────────────────

    #[test]
    fn audio_frames_dropped_defaults_to_zero() {
        let state = SharedState::new();
        assert_eq!(state.audio_frames_dropped, 0);
    }

    #[tokio::test]
    async fn audio_frames_dropped_increments_when_channel_full() {
        // Create a bounded channel with capacity 1, then send two frames so the
        // second is dropped.  The signal path increments the counter on each
        // try_send failure.
        use crossbeam_channel::bounded;
        use crate::sample::StereoFrame;

        let (audio_tx_cap1, _audio_rx) = bounded::<Arc<[StereoFrame]>>(1);

        // Manually fill the channel so the next try_send fails.
        let dummy: Arc<[StereoFrame]> = vec![StereoFrame::mono(0.0); 1].into();
        audio_tx_cap1.try_send(Arc::clone(&dummy)).unwrap(); // fills slot

        let shared = Arc::new(RwLock::new(SharedState::new()));

        // try_send fails → increment counter
        if audio_tx_cap1.try_send(Arc::clone(&dummy)).is_err() {
            shared.write().audio_frames_dropped += 1;
        }

        assert_eq!(shared.read().audio_frames_dropped, 1);
    }

    // ── min_zoom_for_mode ─────────────────────────────────────────────────────

    #[test]
    fn min_zoom_wbfm_is_0_05() {
        assert_eq!(min_zoom_for_mode(DemodMode::Wbfm), 0.05);
    }

    #[test]
    fn min_zoom_cw_is_0_05() {
        assert_eq!(min_zoom_for_mode(DemodMode::Cw), 0.05);
    }

    #[test]
    fn min_zoom_nfm_is_0_02() {
        assert_eq!(min_zoom_for_mode(DemodMode::Nfm), 0.02);
    }

    #[test]
    fn min_zoom_am_usb_lsb_dsb_are_0_02() {
        for mode in [DemodMode::Am, DemodMode::Usb, DemodMode::Lsb, DemodMode::Dsb] {
            assert_eq!(
                min_zoom_for_mode(mode), 0.02,
                "{mode:?} should have 0.02 min zoom"
            );
        }
    }

    #[test]
    fn min_zoom_is_never_below_0() {
        for mode in [
            DemodMode::Wbfm, DemodMode::Nfm, DemodMode::Am,
            DemodMode::Usb, DemodMode::Lsb, DemodMode::Dsb, DemodMode::Cw,
        ] {
            assert!(min_zoom_for_mode(mode) > 0.0);
        }
    }

    // ── SNR helper + clipping detection ──────────────────────────────────────

    /// Build a flat noise floor (noise_floor_db) with a peak (peak_db) at
    /// `center ± peak_half_bins`.
    fn make_fft_buf(n: usize, center: usize, peak_half_bins: usize, peak_db: f32, noise_db: f32) -> Vec<f32> {
        let mut buf = vec![noise_db; n];
        let lo = center.saturating_sub(peak_half_bins);
        let hi = (center + peak_half_bins).min(n - 1);
        for v in buf[lo..=hi].iter_mut() {
            *v = peak_db;
        }
        buf
    }

    #[test]
    fn snr_helper_detects_peak_over_noise() {
        // 2048-bin FFT, signal at center ±50 bins at -10 dBFS, noise at -80 dBFS.
        let buf = make_fft_buf(2048, 1024, 50, -10.0, -80.0);
        let snr = compute_snr_db(&buf, 1024, 50);
        // SNR should be close to 70 dB (peak − noise floor = -10 − -80).
        assert!(snr > 60.0 && snr < 80.0, "snr = {snr}");
    }

    #[test]
    fn snr_wide_window_captures_wbfm_signal_better_than_narrow() {
        // Model a WBFM spectrum where the stereo subcarrier sidebands sit at ±70
        // bins from centre (stronger than the carrier at 0 bins).
        // Noise floor at -80 dBFS.
        //
        // narrow window (half = 50): misses the ±70-bin sidebands → peak = -30
        // wide window   (half = 75): captures the ±70-bin sidebands → peak = -10
        let n = 2048;
        let center = n / 2;
        let mut buf = vec![-80.0f32; n];
        // Weak carrier at centre
        buf[center] = -30.0;
        // Strong sidebands at ±70 bins
        buf[center - 70] = -10.0;
        buf[center + 70] = -10.0;

        let snr_narrow = compute_snr_db(&buf, center, 50); // misses sidebands
        let snr_wide   = compute_snr_db(&buf, center, 75); // captures sidebands
        assert!(
            snr_wide > snr_narrow,
            "wide window should report higher SNR: wide={snr_wide} narrow={snr_narrow}"
        );
    }

    #[test]
    fn clipping_detected_at_zero_dbfs() {
        let mut bins = vec![-10.0f32; 512];
        bins[100] = 0.0; // exactly 0 dBFS
        assert!(any_bin_clipping(&bins), "0.0 dBFS should trigger clipping");
    }

    #[test]
    fn clipping_not_detected_below_zero_dbfs() {
        let bins = vec![-0.1f32; 512];
        assert!(!any_bin_clipping(&bins), "-0.1 dBFS should not trigger clipping");
    }

    #[test]
    fn clipping_detected_when_bin_positive() {
        let mut bins = vec![-50.0f32; 512];
        bins[200] = 1.5; // clipped signal can exceed 0 dBFS in normalised FFT
        assert!(any_bin_clipping(&bins));
    }

    #[test]
    fn fft_display_state_clipping_defaults_false() {
        let state = SharedState::new();
        assert!(!state.fft.fft_clipping_detected, "clipping should default to false");
    }

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
        let path = SignalPath::start(Arc::clone(&shared), iq_rx, None, None, None, None, None);
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
        tick(
            &path.cmd_tx,
            &iq_tx,
            ReceiverCmd::SetDemodMode(DemodMode::Am),
        )
        .await;
        assert_eq!(shared.read().demod.demod_mode, DemodMode::Am);
        let _ = path.cmd_tx.try_send(SignalPathCommand::Stop);
    }

    #[tokio::test]
    async fn set_squelch_threshold_updates_demod_state() {
        let (path, iq_tx, shared) = make_signal_path();
        tick(
            &path.cmd_tx,
            &iq_tx,
            ReceiverCmd::SetSquelchThreshold(-45.0),
        )
        .await;
        assert!((shared.read().demod.squelch_threshold - (-45.0)).abs() < 1e-6);
        let _ = path.cmd_tx.try_send(SignalPathCommand::Stop);
    }

    #[tokio::test]
    async fn bookmark_add_and_remove_round_trip() {
        let (path, iq_tx, shared) = make_signal_path();
        // SharedState::new() starts with 1 pre-loaded bookmark (BBC Radio 4).
        let initial = shared.read().bookmarks.len();
        tick(
            &path.cmd_tx,
            &iq_tx,
            BookmarkCmd::Add("NOAA Weather".to_string()),
        )
        .await;
        assert_eq!(
            shared.read().bookmarks.len(),
            initial + 1,
            "one bookmark added"
        );
        let last_idx = shared.read().bookmarks.len() - 1;
        assert_eq!(shared.read().bookmarks[last_idx].name, "NOAA Weather");
        tick(&path.cmd_tx, &iq_tx, BookmarkCmd::Remove(last_idx)).await;
        assert_eq!(
            shared.read().bookmarks.len(),
            initial,
            "back to initial count after Remove"
        );
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
        assert_eq!(
            shared.read().fft.fft_size,
            FFT_SIZE,
            "non-power-of-two FFT size should be ignored"
        );
        let _ = path.cmd_tx.try_send(SignalPathCommand::Stop);
    }

    #[tokio::test]
    async fn band_plan_toggle_updates_display_state() {
        let (path, iq_tx, shared) = make_signal_path();
        tick(&path.cmd_tx, &iq_tx, DisplayCmd::SetBandPlanEnabled(true)).await;
        assert!(shared.read().fft.band_plan_enabled);
        let _ = path.cmd_tx.try_send(SignalPathCommand::Stop);
    }

    #[tokio::test]
    async fn demod_mode_switch_clears_rds_state() {
        let (path, iq_tx, shared) = make_signal_path();
        // Seed some RDS data directly into SharedState.
        {
            let mut s = shared.write();
            s.rds.ps_name = Some("TEST FM".to_string());
            s.rds.pty = Some(3);
            s.rds.rt = Some("Radio Text Here".to_string());
        }
        // Switch from WBFM to AM — signal path must clear RDS on mode change.
        tick(
            &path.cmd_tx,
            &iq_tx,
            ReceiverCmd::SetDemodMode(DemodMode::Am),
        )
        .await;
        let s = shared.read();
        assert_eq!(s.demod.demod_mode, DemodMode::Am);
        assert!(s.rds.ps_name.is_none(), "ps_name must be cleared on mode switch");
        assert!(s.rds.pty.is_none(), "pty must be cleared on mode switch");
        assert!(s.rds.rt.is_none(), "rt must be cleared on mode switch");
        drop(s);
        let _ = path.cmd_tx.try_send(SignalPathCommand::Stop);
    }

    #[tokio::test]
    async fn frequency_change_clears_rds_state() {
        let (path, iq_tx, shared) = make_signal_path();
        // Seed RDS data.
        {
            let mut s = shared.write();
            s.rds.ps_name = Some("STATION".to_string());
            s.rds.ta = true;
        }
        tick(&path.cmd_tx, &iq_tx, ReceiverCmd::SetFrequency(98_100_000)).await;
        let s = shared.read();
        assert_eq!(s.center_freq_hz, 98_100_000);
        assert!(s.rds.ps_name.is_none(), "ps_name must be cleared on freq change");
        assert!(!s.rds.ta, "ta must be cleared on freq change");
        drop(s);
        let _ = path.cmd_tx.try_send(SignalPathCommand::Stop);
    }

    #[tokio::test]
    async fn nfm_bandwidth_change_updates_state() {
        let (path, iq_tx, shared) = make_signal_path();
        // Switch to NFM first.
        tick(
            &path.cmd_tx,
            &iq_tx,
            ReceiverCmd::SetDemodMode(DemodMode::Nfm),
        )
        .await;
        // Change bandwidth from default 12.5k to 25k.
        tick(&path.cmd_tx, &iq_tx, ReceiverCmd::SetNfmBandwidth(25_000)).await;
        assert_eq!(shared.read().demod.nfm_bandwidth_hz, 25_000);
        let _ = path.cmd_tx.try_send(SignalPathCommand::Stop);
    }

    #[tokio::test]
    async fn ctcss_enable_disable_round_trip() {
        let (path, iq_tx, shared) = make_signal_path();
        tick(
            &path.cmd_tx,
            &iq_tx,
            ReceiverCmd::SetCtcssEnabled(true),
        )
        .await;
        assert!(shared.read().demod.ctcss_squelch_enabled);
        assert!(!shared.read().demod.ctcss_tone_detected, "tone must be false after enable");
        tick(
            &path.cmd_tx,
            &iq_tx,
            ReceiverCmd::SetCtcssEnabled(false),
        )
        .await;
        assert!(!shared.read().demod.ctcss_squelch_enabled);
        let _ = path.cmd_tx.try_send(SignalPathCommand::Stop);
    }

    #[tokio::test]
    async fn set_tune_step_updates_demod_state() {
        let (path, iq_tx, shared) = make_signal_path();
        tick(&path.cmd_tx, &iq_tx, ReceiverCmd::SetTuneStep(10_000)).await;
        assert_eq!(shared.read().demod.tune_step_hz, 10_000);
        let _ = path.cmd_tx.try_send(SignalPathCommand::Stop);
    }

    // ── Scanner ───────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn scanner_start_stop_transitions() {
        let (path, iq_tx, shared) = make_signal_path();
        // Add a second bookmark so the scanner has something to work with.
        tick(
            &path.cmd_tx,
            &iq_tx,
            BookmarkCmd::Add("Test Station".to_string()),
        )
        .await;
        // Start scanner.
        tick(
            &path.cmd_tx,
            &iq_tx,
            ScanCmd::Start(String::new()),
        )
        .await;
        assert!(shared.read().scanner.scan_running, "scanner should be running after Start");
        // Stop scanner.
        tick(&path.cmd_tx, &iq_tx, ScanCmd::Stop).await;
        assert!(!shared.read().scanner.scan_running, "scanner should stop after Stop");
        let _ = path.cmd_tx.try_send(SignalPathCommand::Stop);
    }

    #[tokio::test]
    async fn scanner_set_dwell_updates_state() {
        let (path, iq_tx, shared) = make_signal_path();
        tick(&path.cmd_tx, &iq_tx, ScanCmd::SetDwell(5.0)).await;
        let dwell = shared.read().scanner.scan_dwell_secs;
        assert!((dwell - 5.0).abs() < 1e-6, "dwell should be 5.0 s");
        let _ = path.cmd_tx.try_send(SignalPathCommand::Stop);
    }

    #[tokio::test]
    async fn scanner_set_dwell_clamps_minimum() {
        let (path, iq_tx, shared) = make_signal_path();
        tick(&path.cmd_tx, &iq_tx, ScanCmd::SetDwell(0.1)).await;
        let dwell = shared.read().scanner.scan_dwell_secs;
        assert!(dwell >= 0.5, "dwell must be clamped to minimum 0.5 s");
        let _ = path.cmd_tx.try_send(SignalPathCommand::Stop);
    }

    #[tokio::test]
    async fn scanner_start_with_no_matching_bookmarks_stops_immediately() {
        let (path, iq_tx, shared) = make_signal_path();
        // Only default bookmark (BBC R4, category ""). Start with a category
        // filter that matches nothing.
        tick(
            &path.cmd_tx,
            &iq_tx,
            ScanCmd::Start("NONEXISTENT_CATEGORY_XYZ".to_string()),
        )
        .await;
        // The scanner code detects no matching bookmarks and stops immediately.
        assert!(
            !shared.read().scanner.scan_running,
            "scanner should not be running when no bookmarks match the category filter"
        );
        let _ = path.cmd_tx.try_send(SignalPathCommand::Stop);
    }

    // ── Peak-hold dispatch ────────────────────────────────────────────────────

    #[tokio::test]
    async fn set_peak_hold_enabled_updates_state() {
        let (path, iq_tx, shared) = make_signal_path();
        assert!(!shared.read().fft.peak_hold_enabled, "default should be false");
        tick(&path.cmd_tx, &iq_tx, DisplayCmd::SetPeakHoldEnabled(true)).await;
        assert!(shared.read().fft.peak_hold_enabled, "should be enabled after command");
        tick(&path.cmd_tx, &iq_tx, DisplayCmd::SetPeakHoldEnabled(false)).await;
        assert!(!shared.read().fft.peak_hold_enabled, "should disable after second command");
        let _ = path.cmd_tx.try_send(SignalPathCommand::Stop);
    }

    #[tokio::test]
    async fn set_peak_hold_decay_updates_state() {
        let (path, iq_tx, shared) = make_signal_path();
        tick(&path.cmd_tx, &iq_tx, DisplayCmd::SetPeakHoldDecay(1.0)).await;
        let decay = shared.read().fft.peak_hold_decay_db;
        assert!((decay - 1.0).abs() < 1e-6, "decay should be 1.0 dB/frame, got {decay}");
        let _ = path.cmd_tx.try_send(SignalPathCommand::Stop);
    }

    #[tokio::test]
    async fn set_peak_hold_decay_clamps_to_valid_range() {
        let (path, iq_tx, shared) = make_signal_path();
        tick(&path.cmd_tx, &iq_tx, DisplayCmd::SetPeakHoldDecay(0.001)).await;
        assert!(
            shared.read().fft.peak_hold_decay_db >= 0.1,
            "decay below 0.1 should be clamped"
        );
        tick(&path.cmd_tx, &iq_tx, DisplayCmd::SetPeakHoldDecay(99.0)).await;
        assert!(
            shared.read().fft.peak_hold_decay_db <= 2.0,
            "decay above 2.0 should be clamped"
        );
        let _ = path.cmd_tx.try_send(SignalPathCommand::Stop);
    }

    #[test]
    fn peak_hold_enabled_defaults_false() {
        let state = SharedState::new();
        assert!(!state.fft.peak_hold_enabled, "peak_hold_enabled defaults to false");
    }

    #[test]
    fn peak_hold_decay_defaults_to_half_db() {
        let state = SharedState::new();
        assert!(
            (state.fft.peak_hold_decay_db - 0.5).abs() < 1e-6,
            "default decay should be 0.5 dB/frame"
        );
    }
}
