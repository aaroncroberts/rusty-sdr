//! Demodulator type and factory for the signal path.
//!
//! Owns the runtime-switchable `Demod` enum and the `make_demod` constructor
//! that creates the correct demodulator for a given [`DemodMode`].

use crate::dsp::{
    AmDemodulator, CwDemodulator, FmDemodulator, SsbDemodulator, SsbMode, StereoFmDecoder,
};

use super::shared_state::DemodMode;

/// Runtime-switchable demodulator held by the signal-path loop.
///
/// Each variant owns its DSP object.  Variants are swapped at runtime by
/// `SetDemodMode` commands without stopping the signal-path thread.
pub(super) enum Demod {
    Wbfm(StereoFmDecoder),
    Nfm(FmDemodulator),
    Am(AmDemodulator),
    Ssb(SsbDemodulator),
    Cw(CwDemodulator),
}

impl Demod {
    /// Reset all internal DSP state (filters, integrators, resampler).
    ///
    /// Call after a frequency change or demod-mode switch to flush stale
    /// samples and prevent audible glitches.
    pub(super) fn reset(&mut self) {
        match self {
            Self::Wbfm(d) => d.reset(),
            Self::Nfm(d) => d.reset(),
            Self::Am(d) => d.reset(),
            Self::Ssb(d) => d.reset(),
            Self::Cw(d) => d.reset(),
        }
    }

    /// Clear only the FM discriminator's phase reference after an IQ gap.
    ///
    /// Unlike `reset()` this does NOT flush the PLL, LP filters, or
    /// resampler state — it only invalidates the single `prev` sample so
    /// the next discriminator call doesn't produce a garbage phase-spike
    /// from a stale sample reference across a `Lagged` boundary.
    pub(super) fn clear_prev(&mut self) {
        if let Self::Wbfm(d) = self {
            d.clear_prev();
        }
    }
}

/// Create a fresh demodulator for the given mode.
///
/// * `demod_sr` — WBFM decimated sample rate (used for WBFM only).
/// * `narrow_demod_sr` — narrow-mode decimated rate (~200 kHz, used for
///   NFM/AM/SSB/CW).
/// * `nfm_bw_hz` — NFM channel bandwidth in Hz (typically 12 500).
pub(super) fn make_demod(
    mode: DemodMode,
    _sr: u32,
    demod_sr: u32,
    narrow_demod_sr: u32,
    nfm_bw_hz: u32,
) -> Demod {
    match mode {
        DemodMode::Wbfm => Demod::Wbfm(StereoFmDecoder::new(demod_sr)),
        DemodMode::Nfm => Demod::Nfm(FmDemodulator::new(
            narrow_demod_sr,
            48_000,
            nfm_bw_hz as f32,
            0.0,
        )),
        DemodMode::Am => Demod::Am(AmDemodulator::new(narrow_demod_sr, 48_000)),
        DemodMode::Usb => Demod::Ssb(SsbDemodulator::standard(SsbMode::Usb, narrow_demod_sr)),
        DemodMode::Lsb => Demod::Ssb(SsbDemodulator::standard(SsbMode::Lsb, narrow_demod_sr)),
        DemodMode::Dsb => Demod::Ssb(SsbDemodulator::standard(SsbMode::Dsb, narrow_demod_sr)),
        DemodMode::Cw => Demod::Cw(CwDemodulator::new(narrow_demod_sr, 48_000)),
    }
}
