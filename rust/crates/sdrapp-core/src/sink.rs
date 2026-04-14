#![forbid(unsafe_code)]

use std::sync::Arc;
use tokio::sync::mpsc;

use crate::{block::Block, error::SinkError, sample::StereoFrame};

/// An audio sink: consumes stereo frames and sends them to an output device.
///
/// Implementors: sdrapp-audio (cpal/CoreAudio).
pub trait AudioSink: Block {
    /// Sender end — push StereoFrame batches here.
    fn sender(&self) -> mpsc::Sender<Arc<[StereoFrame]>>;

    fn set_volume(&self, linear: f32) -> Result<(), SinkError>;
}
