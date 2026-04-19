//! Orbcomm satellite downlink decoder.
//!
//! Orbcomm uses Staggered Differential Phase Shift Keying (SDPSK) at 4800 baud
//! on channels near 137 MHz.  Each symbol encodes one bit via a ±90° phase
//! shift relative to the previous symbol.
//!
//! ## Pipeline
//!
//! ```text
//! IQ samples (48 kHz)
//!   └─► SDPSK demodulator   (phase difference → ±1.0 soft symbols)
//!         └─► Gardner TED   (clock recovery, ~10 samples/symbol at 48 kHz)
//!               └─► frame synchronizer  (corr against sync word 0x65A8F9)
//!                     └─► packet parser (type + 9 data words + Fletcher FCS)
//! ```
//!
//! ## What we can decode
//!
//! Orbcomm subscriber traffic is encrypted (privacy-protected), but the
//! satellite telemetry frames are unencrypted and contain:
//!
//! - Satellite ID (1 byte)
//! - UTC epoch time (seconds since 1970-01-01)
//! - Channel number
//! - Frame sequence counter
//!
//! These are surfaced in [`OrbcommPacket`].

pub mod demod;
pub mod framer;
pub mod parser;

pub use demod::{GardnerClock, SdpskDemod};
pub use framer::FrameSync;
pub use parser::{OrbcommPacket, PacketType};

/// Sample rate the demodulator is designed for.  Callers must resample to
/// this rate before passing samples in.
pub const SAMPLE_RATE_HZ: u32 = 48_000;

/// Symbol rate in baud.
pub const SYMBOL_RATE: u32 = 4_800;

/// Nominal samples per symbol (must be integer for the Gardner TED).
pub const SPS: usize = (SAMPLE_RATE_HZ / SYMBOL_RATE) as usize; // = 10
