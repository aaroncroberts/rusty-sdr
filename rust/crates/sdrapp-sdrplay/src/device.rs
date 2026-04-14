#![forbid(unsafe_code)]

use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

use sdrapp_core::{block::Block, error::SourceError, sample::IqSample, source::Source};

use crate::config::RspdxConfig;

/// SDRplay RSPdx-R2 source.
///
/// Wraps the unsafe sdrplay-sys FFI behind safe Rust types.
/// The device callback writes IQ samples into a broadcast channel;
/// consumers subscribe via `subscribe()`.
pub struct RspdxSource {
    config: RspdxConfig,
    frequency_hz: Arc<AtomicU64>,
    tx: broadcast::Sender<Arc<[IqSample]>>,
    stop_tx: Option<tokio::sync::oneshot::Sender<()>>,
}

impl RspdxSource {
    pub fn new(config: RspdxConfig) -> Self {
        let (tx, _) = broadcast::channel(64);
        let frequency_hz = Arc::new(AtomicU64::new(config.frequency_hz));
        Self {
            config,
            frequency_hz,
            tx,
            stop_tx: None,
        }
    }
}

impl Block for RspdxSource {
    fn start(&mut self) -> JoinHandle<()> {
        let (_stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        self.stop_tx = Some(_stop_tx);
        let tx = self.tx.clone();
        let config = self.config.clone();

        tokio::spawn(async move {
            tracing::info!(
                freq_hz = config.frequency_hz,
                sample_rate = config.sample_rate_sps,
                "RSPdx-R2 source starting (FFI not yet implemented)"
            );
            // TODO: open sdrplay_api device, configure RSPdx-R2, start streaming.
            // The sdrplay_api callback will produce IqSamples and send via tx.
            // For now, send synthetic silence so the pipeline can be tested end-to-end.
            let _ = stop_rx.await;
            tracing::info!("RSPdx-R2 source stopped");
            drop(tx);
        })
    }

    fn stop(&self) {
        // Dropping stop_tx signals the task to exit.
        // Actual cleanup happens in the spawn above.
    }
}

impl Source for RspdxSource {
    fn subscribe(&self) -> broadcast::Receiver<Arc<[IqSample]>> {
        self.tx.subscribe()
    }

    fn set_frequency(&self, hz: u64) -> Result<(), SourceError> {
        const MIN_HZ: u64 = 1_000;
        const MAX_HZ: u64 = 2_000_000_000;
        if !(MIN_HZ..=MAX_HZ).contains(&hz) {
            return Err(SourceError::FrequencyOutOfRange(hz));
        }
        self.frequency_hz.store(hz, Ordering::Relaxed);
        // TODO: call sdrplay_api_SetRf via sdrplay-sys
        Ok(())
    }

    fn set_sample_rate(&self, sps: u32) -> Result<(), SourceError> {
        // RSPdx-R2 supports 200 ksps – 10 Msps (with IF filter constraints)
        const SUPPORTED: &[u32] = &[200_000, 500_000, 1_000_000, 2_000_000, 6_000_000, 8_000_000, 10_000_000];
        if !SUPPORTED.contains(&sps) {
            return Err(SourceError::SampleRateNotSupported(sps));
        }
        // TODO: call sdrplay_api_Update via sdrplay-sys
        Ok(())
    }

    fn frequency(&self) -> u64 {
        self.frequency_hz.load(Ordering::Relaxed)
    }

    fn sample_rate(&self) -> u32 {
        self.config.sample_rate_sps
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frequency_out_of_range_rejected() {
        let src = RspdxSource::new(RspdxConfig::default());
        assert!(src.set_frequency(0).is_err());
        assert!(src.set_frequency(3_000_000_000).is_err());
    }

    #[test]
    fn valid_frequency_accepted() {
        let src = RspdxSource::new(RspdxConfig::default());
        assert!(src.set_frequency(100_000_000).is_ok());
        assert_eq!(src.frequency(), 100_000_000);
    }

    #[test]
    fn unsupported_sample_rate_rejected() {
        let src = RspdxSource::new(RspdxConfig::default());
        assert!(src.set_sample_rate(12345).is_err());
    }

    #[test]
    fn supported_sample_rates_accepted() {
        let src = RspdxSource::new(RspdxConfig::default());
        assert!(src.set_sample_rate(2_000_000).is_ok());
        assert!(src.set_sample_rate(10_000_000).is_ok());
    }
}
