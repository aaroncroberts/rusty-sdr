#![forbid(unsafe_code)]

//! cpal-based audio sink for macOS (CoreAudio).
//!
//! Receives StereoFrame batches via mpsc channel and plays them through
//! the selected output device using cpal's callback model.

mod config;
mod sink;

pub use config::AudioConfig;
pub use sink::CpalAudioSink;
