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

use crate::dsp::{AmDemodulator, FmDemodulator, FftProcessor, Volume};
use crate::sample::{IqSample, StereoFrame};

const FFT_SIZE: usize = 2048;
const AUDIO_FRAME_SIZE: usize = 1024;

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
}

impl SharedState {
    pub fn new() -> Self {
        Self {
            fft_magnitudes: vec![-120.0; FFT_SIZE],
            volume: 0.8,
            ..Default::default()
        }
    }
}

/// Commands from the UI to the signal path.
#[derive(Debug)]
pub enum SignalPathCommand {
    SetFrequency(u64),
    SetVolume(f32),
    SetDemodMode(DemodMode),
    StartRecording,
    StopRecording,
    Stop,
}

/// Manages the running signal path tasks.
pub struct SignalPath {
    shared: Arc<RwLock<SharedState>>,
    cmd_tx: crossbeam_channel::Sender<SignalPathCommand>,
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
    ) -> Self {
        let (cmd_tx, cmd_rx) = crossbeam_channel::bounded::<SignalPathCommand>(64);
        let shared_clone = Arc::clone(&shared);
        let freq_atomic_clone = freq_atomic;

        // Read initial sample rate before moving shared into the task
        let sample_rate = shared.read().sample_rate_sps;

        // Demodulator state — switched at runtime by SetDemodMode
        enum Demod {
            Wbfm(FmDemodulator),
            Nfm(FmDemodulator),
            Am(AmDemodulator),
        }

        impl Demod {
            fn process(&mut self, samples: &[Complex<f32>]) -> Vec<f32> {
                match self {
                    Demod::Wbfm(d) | Demod::Nfm(d) => d.process(samples),
                    Demod::Am(d) => d.process(samples),
                }
            }

            fn reset(&mut self) {
                match self {
                    Demod::Wbfm(d) | Demod::Nfm(d) => d.reset(),
                    Demod::Am(d) => d.reset(),
                }
            }
        }

        let handle = tokio::spawn(async move {
            let sr = sample_rate.max(200_000);
            let mut fft = FftProcessor::new(FFT_SIZE);
            let mut vol = Volume::new(0.8);
            let mut demod: Demod = Demod::Wbfm(FmDemodulator::wbfm(sr));
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
                        }
                        SignalPathCommand::SetVolume(v) => {
                            vol.set(v);
                            shared_clone.write().volume = v;
                        }
                        SignalPathCommand::SetDemodMode(mode) => {
                            demod = match mode {
                                DemodMode::Wbfm => Demod::Wbfm(FmDemodulator::wbfm(sr)),
                                DemodMode::Nfm => {
                                    Demod::Nfm(FmDemodulator::new(sr, 48_000, 12_500.0, 0.0))
                                }
                                DemodMode::Am => Demod::Am(AmDemodulator::standard(sr)),
                            };
                            shared_clone.write().demod_mode = mode;
                            tracing::info!(?mode, "demod mode changed");
                        }
                        SignalPathCommand::StartRecording => {
                            shared_clone.write().is_recording = true;
                        }
                        SignalPathCommand::StopRecording => {
                            shared_clone.write().is_recording = false;
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

                // Demodulate IQ → mono audio at 48 kHz
                let iq_complex: Vec<Complex<f32>> = batch
                    .iter()
                    .map(|s| Complex::new(s.re, s.im))
                    .collect();
                let demod_audio = demod.process(&iq_complex);

                let stereo: Vec<StereoFrame> = demod_audio
                    .into_iter()
                    .map(StereoFrame::mono)
                    .collect();

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
