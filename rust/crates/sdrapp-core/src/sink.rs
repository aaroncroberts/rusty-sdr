#![forbid(unsafe_code)]

use std::sync::Arc;

use crate::{block::Block, error::SinkError, sample::StereoFrame};

/// An audio sink: consumes stereo frames and sends them to an output device.
///
/// Uses `crossbeam_channel::Sender` rather than `tokio::sync::mpsc` because:
/// - cpal's audio thread is a plain `std::thread` (not tokio)
/// - `try_send` is non-blocking and appropriate for real-time audio
/// - avoids an unnecessary async-to-sync bridge
pub trait AudioSink: Block {
    /// Sender end — push `Arc<[StereoFrame]>` batches here.
    /// Non-blocking: if the channel is full, batches are dropped (backpressure by dropping).
    fn sender(&self) -> crossbeam_channel::Sender<Arc<[StereoFrame]>>;

    fn set_volume(&self, linear: f32) -> Result<(), SinkError>;
}
