#![forbid(unsafe_code)]

use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use sdrapp_core::{block::Block, error::SinkError, sample::StereoFrame, sink::AudioSink};

use crate::config::AudioConfig;

/// cpal-backed audio sink.
///
/// Receives StereoFrame batches via mpsc and plays them via CoreAudio.
pub struct CpalAudioSink {
    config: AudioConfig,
    tx: mpsc::Sender<Arc<[StereoFrame]>>,
    rx: Option<mpsc::Receiver<Arc<[StereoFrame]>>>,
    stop_tx: Option<tokio::sync::oneshot::Sender<()>>,
}

impl CpalAudioSink {
    pub fn new(config: AudioConfig) -> Self {
        let (tx, rx) = mpsc::channel(32);
        Self { config, tx, rx: Some(rx), stop_tx: None }
    }
}

impl Block for CpalAudioSink {
    fn start(&mut self) -> JoinHandle<()> {
        let rx = self.rx.take().expect("CpalAudioSink::start called twice");
        let (_stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        self.stop_tx = Some(_stop_tx);
        let config = self.config.clone();

        tokio::spawn(async move {
            tracing::info!(device = ?config.device_name, "audio sink starting (cpal not yet wired)");
            // TODO: open cpal output stream, bridge mpsc → cpal callback ring buffer.
            let _ = stop_rx.await;
            drop(rx);
            tracing::info!("audio sink stopped");
        })
    }

    fn stop(&self) {
        // Dropping stop_tx signals the task.
    }
}

impl AudioSink for CpalAudioSink {
    fn sender(&self) -> mpsc::Sender<Arc<[StereoFrame]>> {
        self.tx.clone()
    }

    fn set_volume(&self, linear: f32) -> Result<(), SinkError> {
        if !(0.0..=1.0).contains(&linear) {
            return Err(SinkError::Hardware(format!("volume {linear} out of range 0.0–1.0")));
        }
        // TODO: apply to cpal stream gain
        Ok(())
    }
}
