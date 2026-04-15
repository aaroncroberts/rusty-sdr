#![forbid(unsafe_code)]

use std::sync::{
    atomic::AtomicU64,
    Arc,
};
use tokio::sync::broadcast;

use crate::{block::Block, error::SourceError, sample::IqSample};

/// Capability flags advertised by an IQ source.
///
/// Used by the UI to show/hide hardware-specific controls.
#[derive(Debug, Clone, Default)]
pub struct SourceCapabilities {
    /// Device display name, e.g. "SDRplay RSPdx-R2" or "RTL-SDR Blog v4".
    pub name: String,
    /// Supports bias-T (phantom power on antenna port).
    pub bias_t: bool,
    /// Supports direct sampling mode (AM/SW reception).
    pub direct_sampling: bool,
    /// Minimum and maximum RF gain in dB (inclusive).
    pub gain_range_db: (f32, f32),
    /// Whether the device exposes a separate hardware command channel for
    /// real-time parameter updates without signal-path command latency.
    pub has_hardware_cmd_tx: bool,
}

/// An SDR source: produces a stream of IQ samples and accepts tuning commands.
///
/// Implementors: `RspdxSource` (SDRplay), `TestSignalSource` (synthetic demo),
/// and any future adapter crates (RTL-SDR, HackRF, AirSpy, …).
pub trait Source: Block {
    /// Receiver end of the IQ sample stream. Each batch is an Arc slice
    /// to allow zero-copy fan-out to multiple consumers (UI, recorder).
    fn subscribe(&self) -> broadcast::Receiver<Arc<[IqSample]>>;

    fn set_frequency(&self, hz: u64) -> Result<(), SourceError>;
    fn set_sample_rate(&self, sps: u32) -> Result<(), SourceError>;
    fn frequency(&self) -> u64;
    fn sample_rate(&self) -> u32;

    /// Shared atomic tracking the current centre frequency.
    ///
    /// The signal path writes to this on every `SetFrequency` command so that
    /// the hardware device loop can pick it up without an extra channel hop.
    /// All implementors must expose this; synthetic sources may expose a no-op
    /// atomic that they quietly ignore.
    fn frequency_atomic(&self) -> Arc<AtomicU64>;

    /// Hardware capabilities for this source.
    /// Returns a default struct if the source does not override this.
    fn capabilities(&self) -> SourceCapabilities {
        SourceCapabilities::default()
    }
}
