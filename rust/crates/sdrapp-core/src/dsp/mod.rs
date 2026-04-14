#![forbid(unsafe_code)]

//! DSP building blocks.
//!
//! Each block is a pure Rust struct/function with no tokio dependency.
//! This keeps them unit-testable without spinning up an async runtime.
//! The signal_path module wires them into tokio tasks.

pub mod demod;
pub mod fft;
pub mod packer;
pub mod squelch;
pub mod volume;

pub use demod::{AmDemodulator, FmDemodulator};
pub use fft::FftProcessor;
pub use packer::Packer;
pub use squelch::Squelch;
pub use volume::Volume;
