#![forbid(unsafe_code)]

//! Synthetic IQ source for demo / offline mode.
//!
//! Emits a continuous FM-modulated carrier at unit amplitude. The modulation is
//! a 1 kHz sine with ±10 kHz deviation so that the FmDemodulator produces
//! clearly audible 1 kHz audio even at low volume.
//!
//! The source accepts `set_frequency` calls without error (demo mode ignores the
//! tuned centre frequency — the signal is always at baseband). This satisfies
//! the acceptance criterion that frequency changes must not panic.

use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};

use num_complex::Complex;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

use crate::{block::Block, error::SourceError, sample::IqSample, source::{Source, SourceCapabilities}};

/// IQ samples per broadcast batch — matches the SDRplay source's batch size.
const BATCH_SIZE: usize = 1024;

/// Peak frequency deviation of the test FM signal (Hz).
const FM_DEVIATION_HZ: f32 = 10_000.0;

/// Audio modulation frequency (Hz). Produces a 1 kHz tone after FM demodulation.
const FM_AUDIO_HZ: f32 = 1_000.0;

/// Synthetic FM IQ source for demo and test use.
///
/// Implements both [`Block`] and [`Source`], so it can be used anywhere a real
/// hardware source would be used.
pub struct TestSignalSource {
    frequency_hz: Arc<AtomicU64>,
    sample_rate_sps: u32,
    running: Arc<AtomicBool>,
    tx: broadcast::Sender<Arc<[IqSample]>>,
}

impl TestSignalSource {
    /// Create a new test source with the given initial frequency and sample rate.
    pub fn new(frequency_hz: u64, sample_rate_sps: u32) -> Self {
        let (tx, _) = broadcast::channel(64);
        Self {
            frequency_hz: Arc::new(AtomicU64::new(frequency_hz)),
            sample_rate_sps: sample_rate_sps.max(48_000),
            running: Arc::new(AtomicBool::new(false)),
            tx,
        }
    }

    /// Returns the shared frequency atomic.
    ///
    /// The signal path writes new frequencies here when the user tunes; the
    /// test source accepts but ignores the value (demo mode always emits the
    /// same signal). Exposing the atomic keeps the signal-path interface
    /// consistent with the real hardware source.
    pub fn frequency_atomic(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.frequency_hz)
    }
}

impl Block for TestSignalSource {
    fn start(&mut self) -> JoinHandle<()> {
        self.running.store(true, Ordering::Relaxed);

        let running = Arc::clone(&self.running);
        let tx = self.tx.clone();
        let sr = self.sample_rate_sps as f32;

        // Duration of one batch in real time — used to pace output to real-time rate.
        let batch_duration =
            std::time::Duration::from_secs_f64(BATCH_SIZE as f64 / sr as f64);

        tokio::spawn(async move {
            let tau = 2.0 * std::f32::consts::PI;
            let phase_step_mod = tau * FM_AUDIO_HZ / sr;
            let phase_step_dev = tau * FM_DEVIATION_HZ / sr;

            let mut carrier_phase: f32 = 0.0;
            let mut mod_phase: f32 = 0.0;

            while running.load(Ordering::Relaxed) {
                let mut batch = Vec::with_capacity(BATCH_SIZE);

                for _ in 0..BATCH_SIZE {
                    // IQ sample: unit-amplitude phasor at current carrier phase.
                    batch.push(Complex::new(carrier_phase.cos(), carrier_phase.sin()));

                    // Advance audio modulation oscillator.
                    mod_phase += phase_step_mod;
                    if mod_phase >= tau {
                        mod_phase -= tau;
                    }

                    // FM: phase incremented by deviation * sin(mod_phase).
                    // This integrates to a sinusoidal instantaneous frequency.
                    carrier_phase += phase_step_dev * mod_phase.sin();
                    // Keep phase in [0, 2π] to avoid f32 precision drift.
                    carrier_phase = carrier_phase.rem_euclid(tau);
                }

                let arc_batch: Arc<[IqSample]> = batch.into();
                let _ = tx.send(arc_batch);

                // Throttle to real-time so downstream consumers aren't overwhelmed.
                tokio::time::sleep(batch_duration).await;
            }

            tracing::info!("test signal source stopped");
        })
    }

    fn stop(&self) {
        self.running.store(false, Ordering::Relaxed);
    }
}

impl Source for TestSignalSource {
    fn subscribe(&self) -> broadcast::Receiver<Arc<[IqSample]>> {
        self.tx.subscribe()
    }

    fn set_frequency(&self, hz: u64) -> Result<(), SourceError> {
        // Demo source: accept any frequency; actual signal is unaffected.
        self.frequency_hz.store(hz, Ordering::Relaxed);
        Ok(())
    }

    fn set_sample_rate(&self, _sps: u32) -> Result<(), SourceError> {
        // Sample rate is fixed at construction time in demo mode.
        Ok(())
    }

    fn frequency(&self) -> u64 {
        self.frequency_hz.load(Ordering::Relaxed)
    }

    fn sample_rate(&self) -> u32 {
        self.sample_rate_sps
    }

    fn frequency_atomic(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.frequency_hz)
    }

    fn capabilities(&self) -> SourceCapabilities {
        SourceCapabilities {
            name: "Demo Mode".into(),
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn emits_unit_amplitude_batches() {
        let mut src = TestSignalSource::new(100_000_000, 2_000_000);
        let mut rx = src.subscribe();
        let _handle = src.start();

        let batch = tokio::time::timeout(
            std::time::Duration::from_millis(200),
            rx.recv(),
        )
        .await
        .expect("timeout waiting for first batch")
        .expect("recv error");

        assert_eq!(batch.len(), BATCH_SIZE, "batch size mismatch");

        for s in batch.iter() {
            let mag = (s.re * s.re + s.im * s.im).sqrt();
            assert!((mag - 1.0).abs() < 0.01, "magnitude {mag} not ~1.0");
        }

        src.stop();
    }

    #[test]
    fn set_frequency_never_errors() {
        let src = TestSignalSource::new(100_000_000, 2_000_000);
        assert!(src.set_frequency(0).is_ok());
        assert!(src.set_frequency(u64::MAX).is_ok());
        assert!(src.set_frequency(98_000_000).is_ok());
    }

    #[test]
    fn set_sample_rate_never_errors() {
        let src = TestSignalSource::new(100_000_000, 2_000_000);
        assert!(src.set_sample_rate(12345).is_ok());
    }
}
