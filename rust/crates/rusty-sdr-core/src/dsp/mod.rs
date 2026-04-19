#![forbid(unsafe_code)]

//! DSP building blocks.
//!
//! Each block is a pure Rust struct/function with no tokio dependency.
//! This keeps them unit-testable without spinning up an async runtime.
//! The signal_path module wires them into tokio tasks.

pub mod bandpass;
pub mod ctcss;
pub mod demod;
pub mod fft;
pub mod fir_lowpass;
pub mod packer;
pub mod rds;
pub mod resampler;
pub mod squelch;
pub mod stereo_fm;
pub mod volume;

pub use bandpass::AudioBandpass;
pub use ctcss::CtcssDetector;
pub use demod::{AmDemodulator, CwDemodulator, FmDemodulator, SsbDemodulator, SsbMode};
pub use fft::{FftProcessor, FftWindow};
pub use fir_lowpass::FirLowpass;
pub use packer::Packer;
pub use rds::RdsDecoder;
pub use resampler::RationalResampler;
pub use squelch::Squelch;
pub use stereo_fm::StereoFmDecoder;
pub use volume::Volume;
