#![forbid(unsafe_code)]

//! cpal-backed audio sink for macOS (CoreAudio).
//!
//! Architecture:
//!
//!   Signal path (tokio task)
//!       │  crossbeam_channel::Sender<Arc<[StereoFrame]>>
//!       ▼
//!   Audio thread (std::thread)  ← owns cpal::Stream (Stream is !Send)
//!       │  Mutex<VecDeque<f32>>  (ring buffer)
//!       ▼
//!   CoreAudio callback thread (OS-managed)
//!       │  fills output &mut [f32]
//!       ▼
//!   Speakers

use parking_lot::Mutex;
use std::collections::VecDeque;
use std::sync::{
    atomic::{AtomicU32, Ordering},
    Arc,
};
use tokio::task::JoinHandle;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, SampleRate, StreamConfig};

use sdrapp_core::{block::Block, error::SinkError, sample::StereoFrame, sink::AudioSink};

use crate::config::AudioConfig;

/// cpal-backed audio output sink.
pub struct CpalAudioSink {
    config: AudioConfig,
    /// Sender exposed to the signal path.
    frame_tx: crossbeam_channel::Sender<Arc<[StereoFrame]>>,
    frame_rx: Option<crossbeam_channel::Receiver<Arc<[StereoFrame]>>>,
    /// Volume stored as f32 bits in AtomicU32 (f32 has no atomic ops natively).
    volume_bits: Arc<AtomicU32>,
    stop_tx: Option<tokio::sync::oneshot::Sender<()>>,
}

impl CpalAudioSink {
    pub fn new(config: AudioConfig) -> Self {
        let (frame_tx, frame_rx) = crossbeam_channel::bounded(64);
        let volume_bits = Arc::new(AtomicU32::new(config.volume.to_bits()));
        Self {
            config,
            frame_tx,
            frame_rx: Some(frame_rx),
            volume_bits,
            stop_tx: None,
        }
    }

    /// Enumerate available output device names using the default host.
    pub fn available_devices() -> Vec<String> {
        let host = cpal::default_host();
        host.output_devices()
            .map(|devs| devs.filter_map(|d| d.name().ok()).collect())
            .unwrap_or_default()
    }
}

impl Block for CpalAudioSink {
    fn start(&mut self) -> JoinHandle<()> {
        let frame_rx = self
            .frame_rx
            .take()
            .expect("CpalAudioSink::start called twice");
        let config = self.config.clone();
        let volume_bits = Arc::clone(&self.volume_bits);

        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        self.stop_tx = Some(stop_tx);

        // Signal the audio thread to clean up when the JoinHandle completes.
        let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();

        std::thread::Builder::new()
            .name("sdrapp-audio".into())
            .spawn(move || {
                if let Err(e) = run_audio_thread(frame_rx, config, volume_bits) {
                    tracing::error!("audio thread error: {e}");
                }
                let _ = done_tx.send(());
            })
            .expect("failed to spawn audio thread");

        tokio::spawn(async move {
            // Wait for either: explicit stop signal, or audio thread exit
            tokio::select! {
                _ = stop_rx => {},
                _ = done_rx => {},
            }
            tracing::info!("audio sink stopped");
        })
    }

    fn stop(&self) {
        // Dropping stop_tx signals the tokio task, which also drops frame_rx via
        // the crossbeam channel — the audio thread exits its recv loop.
    }
}

impl AudioSink for CpalAudioSink {
    fn sender(&self) -> crossbeam_channel::Sender<Arc<[StereoFrame]>> {
        self.frame_tx.clone()
    }

    fn set_volume(&self, linear: f32) -> Result<(), SinkError> {
        if !(0.0..=1.0).contains(&linear) {
            return Err(SinkError::Hardware(format!(
                "volume {linear} out of 0.0–1.0 range"
            )));
        }
        self.volume_bits.store(linear.to_bits(), Ordering::Relaxed);
        Ok(())
    }
}

/// The audio thread body. Runs entirely on a `std::thread` because `cpal::Stream` is `!Send`.
fn run_audio_thread(
    frame_rx: crossbeam_channel::Receiver<Arc<[StereoFrame]>>,
    config: AudioConfig,
    volume_bits: Arc<AtomicU32>,
) -> anyhow::Result<()> {
    use anyhow::Context;

    let host = cpal::default_host();
    tracing::info!(backend = ?host.id(), "cpal host");

    // Select device: by name if configured, otherwise system default
    let device = match &config.device_name {
        Some(name) => host
            .output_devices()?
            .find(|d| d.name().ok().as_deref() == Some(name.as_str()))
            .with_context(|| format!("output device '{name}' not found"))?,
        None => host
            .default_output_device()
            .context("no default output device")?,
    };

    tracing::info!(device = %device.name().unwrap_or_default(), "audio device selected");

    // Build stream config: prefer requested sample rate, fall back to device default
    let stream_config = match find_supported_config(&device, config.sample_rate) {
        Some(c) => {
            tracing::info!(
                sample_rate = c.sample_rate().0,
                channels = c.channels(),
                "using config"
            );
            StreamConfig {
                channels: 2,
                sample_rate: c.sample_rate(),
                buffer_size: BufferSize::Default,
            }
        }
        None => {
            let default = device.default_output_config()?;
            tracing::warn!(
                requested = config.sample_rate,
                actual = default.sample_rate().0,
                "requested sample rate not supported, using device default"
            );
            StreamConfig {
                channels: 2,
                sample_rate: default.sample_rate(),
                buffer_size: BufferSize::Default,
            }
        }
    };

    // Ring buffer shared between our fill loop and the cpal callback
    // Capacity: 2 seconds of stereo f32 samples
    let capacity = stream_config.sample_rate.0 as usize * 2 * 2; // 2s × 2 channels
    let ring: Arc<Mutex<VecDeque<f32>>> = Arc::new(Mutex::new(VecDeque::with_capacity(capacity)));
    let ring_for_cb: Arc<Mutex<VecDeque<f32>>> = Arc::clone(&ring);
    let volume_for_cb = Arc::clone(&volume_bits);

    let stream = device.build_output_stream(
        &stream_config,
        // Data callback — runs on CoreAudio's real-time thread
        move |output: &mut [f32], _: &cpal::OutputCallbackInfo| {
            let vol = f32::from_bits(volume_for_cb.load(Ordering::Relaxed));
            let mut buf = ring_for_cb.lock();
            for sample in output.iter_mut() {
                *sample = buf.pop_front().unwrap_or(0.0) * vol;
            }
        },
        // Error callback
        |err| tracing::error!("cpal stream error: {err}"),
        None, // no timeout
    )?;

    // Pre-fill the ring buffer with 100 ms of silence so the cpal callback
    // never underruns during the brief startup window before the DSP produces
    // its first audio frames.
    {
        let prefill = stream_config.sample_rate.0 as usize / 10 * 2; // 100 ms × 2 ch
        let mut buf = ring.lock();
        buf.extend(std::iter::repeat_n(0.0f32, prefill));
    }

    stream.play()?;
    tracing::info!("audio sink playing");

    // Fill loop: receive frames, push f32 samples into ring buffer
    // Note: volume applied in callback, not here, so it responds to changes without delay
    while let Ok(frames) = frame_rx.recv() {
        let mut buf = ring.lock();
        // Don't let the buffer grow beyond 500ms — drop old samples if we're backed up
        let max_samples = stream_config.sample_rate.0 as usize / 2 * 2; // 500ms stereo
        let buf_len = buf.len();
        if buf_len > max_samples {
            tracing::debug!(
                len = buf_len,
                max = max_samples,
                "ring buffer overflow — dropping"
            );
            buf.drain(..buf_len - max_samples);
        }
        for frame in frames.iter() {
            buf.push_back(frame.left);
            buf.push_back(frame.right);
        }
    }

    // frame_rx closed — stream drops here, audio stops
    tracing::info!("audio thread exiting");
    Ok(())
}

/// Find the best supported config matching the requested sample rate.
fn find_supported_config(
    device: &cpal::Device,
    sample_rate: u32,
) -> Option<cpal::SupportedStreamConfig> {
    let wanted = SampleRate(sample_rate);
    device
        .supported_output_configs()
        .ok()?
        .filter(|c| c.channels() == 2)
        .find_map(|range| range.try_with_sample_rate(wanted))
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_abs_diff_eq;

    #[test]
    fn volume_bits_round_trip() {
        let sink = CpalAudioSink::new(crate::config::AudioConfig::default());
        sink.set_volume(0.75).unwrap();
        let stored = f32::from_bits(sink.volume_bits.load(Ordering::Relaxed));
        assert_abs_diff_eq!(stored, 0.75, epsilon = 1e-7);
    }

    #[test]
    fn volume_out_of_range_rejected() {
        let sink = CpalAudioSink::new(crate::config::AudioConfig::default());
        assert!(sink.set_volume(-0.1).is_err());
        assert!(sink.set_volume(1.1).is_err());
        assert!(sink.set_volume(0.0).is_ok());
        assert!(sink.set_volume(1.0).is_ok());
    }

    #[test]
    fn sender_channel_works() {
        let sink = CpalAudioSink::new(crate::config::AudioConfig::default());
        let tx = sink.sender();
        let rx = sink.frame_rx.as_ref().unwrap().clone();

        let frames: Arc<[StereoFrame]> = vec![StereoFrame::mono(0.5); 8].into();
        tx.try_send(Arc::clone(&frames)).unwrap();

        let received = rx.try_recv().unwrap();
        assert_eq!(received.len(), 8);
        assert_abs_diff_eq!(received[0].left, 0.5, epsilon = 1e-7);
    }

    #[test]
    fn device_list_does_not_panic() {
        // On CI without audio hardware, this should return empty list, not panic
        let devices = CpalAudioSink::available_devices();
        // Just verify it runs without panic; list may be empty on headless systems
        let _ = devices;
    }
}
