#![forbid(unsafe_code)]

use std::sync::Arc;
use tokio::sync::broadcast;

use crate::{block::Block, error::SourceError, sample::IqSample};

/// An SDR source: produces a stream of IQ samples and accepts tuning commands.
///
/// Implementors: sdrapp-sdrplay (RSPdx-R2), and test/synthetic sources.
pub trait Source: Block {
    /// Receiver end of the IQ sample stream. Each batch is an Arc slice
    /// to allow zero-copy fan-out to multiple consumers (UI, recorder).
    fn subscribe(&self) -> broadcast::Receiver<Arc<[IqSample]>>;

    fn set_frequency(&self, hz: u64) -> Result<(), SourceError>;
    fn set_sample_rate(&self, sps: u32) -> Result<(), SourceError>;
    fn frequency(&self) -> u64;
    fn sample_rate(&self) -> u32;
}
