#![forbid(unsafe_code)]

//! WAV recorder using `hound`.
//!
//! Subscribes to the audio stream and writes stereo f32 WAV files on demand.
//! Start/stop recording is controlled via the `RecorderCommand` channel,
//! which the MIDI controller and UI both send to.

mod config;
mod recorder;

pub use config::RecorderConfig;
pub use recorder::{Recorder, RecorderCommand, RecordingMode};
