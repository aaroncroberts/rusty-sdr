#![forbid(unsafe_code)]

//! RTL-SDR IQ source adapter.
//!
//! # Feature flag
//!
//! This crate compiles in two modes:
//!
//! - **Default (no features)**: `RtlSdrSource::is_device_available()` always
//!   returns `false`. No hardware driver is linked. CI uses this mode.
//! - **`rtlsdr` feature**: Links the `rtlsdr-rs` hardware driver. Enables real
//!   device access.
//!
//! # Usage in `main.rs`
//!
//! ```rust,ignore
//! if sdrapp_rtlsdr::RtlSdrSource::is_device_available() {
//!     let mut src = sdrapp_rtlsdr::RtlSdrSource::open(0, cfg).unwrap();
//!     let rx = src.subscribe();
//!     drop(src.start());
//! }
//! ```

use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};

use sdrapp_core::{
    block::Block,
    error::SourceError,
    sample::IqSample,
    source::{Source, SourceCapabilities},
};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

/// Configuration for the RTL-SDR source.
#[derive(Debug, Clone)]
pub struct RtlSdrConfig {
    /// Device index (0 = first device).
    pub device_index: u32,
    /// Initial centre frequency in Hz.
    pub frequency_hz: u64,
    /// Sample rate in samples/sec (typ. 2_048_000).
    pub sample_rate_sps: u32,
    /// RF gain in dB (0 = auto-gain).
    pub gain_db: f32,
    /// Enable bias-T (phantom power).
    pub bias_t: bool,
    /// Enable direct sampling mode (for MW/SW: 0=off, 1=I-branch, 2=Q-branch).
    pub direct_sampling: u8,
}

impl Default for RtlSdrConfig {
    fn default() -> Self {
        Self {
            device_index: 0,
            frequency_hz: 100_000_000,
            sample_rate_sps: 2_048_000,
            gain_db: 0.0,
            bias_t: false,
            direct_sampling: 0,
        }
    }
}

/// RTL-SDR IQ source.
///
/// Wraps an RTL-SDR dongle as an [`sdrapp_core::source::Source`].
/// When the `rtlsdr` feature is disabled, the source is always unavailable
/// (stub mode for CI).
pub struct RtlSdrSource {
    frequency_hz: Arc<AtomicU64>,
    sample_rate_sps: u32,
    running: Arc<AtomicBool>,
    tx: broadcast::Sender<Arc<[IqSample]>>,
    config: RtlSdrConfig,
}

impl RtlSdrSource {
    /// Returns `true` if at least one RTL-SDR device is detected.
    pub fn is_device_available() -> bool {
        #[cfg(feature = "rtlsdr")]
        {
            rtlsdr_device_count() > 0
        }
        #[cfg(not(feature = "rtlsdr"))]
        {
            false
        }
    }

    /// Open the RTL-SDR device at the given index with the supplied config.
    ///
    /// Returns `None` when the `rtlsdr` feature is disabled (stub mode).
    pub fn open(config: RtlSdrConfig) -> Option<Self> {
        #[cfg(feature = "rtlsdr")]
        {
            let (tx, _) = broadcast::channel(64);
            Some(Self {
                frequency_hz: Arc::new(AtomicU64::new(config.frequency_hz)),
                sample_rate_sps: config.sample_rate_sps,
                running: Arc::new(AtomicBool::new(false)),
                tx,
                config,
            })
        }
        #[cfg(not(feature = "rtlsdr"))]
        {
            let _ = config;
            None
        }
    }
}

// ── Hardware driver glue (rtlsdr feature only) ────────────────────────────────
//
// When the "rtlsdr" feature is enabled, supply a `rtlsdr_device_count()`
// function and the `run_rtlsdr_thread` body by linking against a compatible
// RTL-SDR Rust binding (e.g. a future `rtlsdr-rs` version that does not
// conflict with the sdrplay bindgen version, or a hand-written sys crate).
//
// Stub below makes the crate compile cleanly without a driver.

#[cfg(feature = "rtlsdr")]
fn rtlsdr_device_count() -> u32 {
    // TODO: call into the real librtlsdr binding once one is linked.
    0
}

// ── Block impl ────────────────────────────────────────────────────────────────

impl Block for RtlSdrSource {
    fn start(&mut self) -> JoinHandle<()> {
        self.running.store(true, Ordering::Relaxed);

        let tx = self.tx.clone();
        let running = Arc::clone(&self.running);
        let freq_atomic = Arc::clone(&self.frequency_hz);
        let cfg = self.config.clone();

        tokio::task::spawn_blocking(move || {
            #[cfg(feature = "rtlsdr")]
            run_rtlsdr_thread(tx, running, freq_atomic, cfg);

            #[cfg(not(feature = "rtlsdr"))]
            {
                let _ = (tx, running, freq_atomic, cfg);
            }
        })
    }

    fn stop(&self) {
        self.running.store(false, Ordering::Relaxed);
    }
}

// ── Source impl ───────────────────────────────────────────────────────────────

impl Source for RtlSdrSource {
    fn subscribe(&self) -> broadcast::Receiver<Arc<[IqSample]>> {
        self.tx.subscribe()
    }

    fn set_frequency(&self, hz: u64) -> Result<(), SourceError> {
        self.frequency_hz.store(hz, Ordering::Relaxed);
        Ok(())
    }

    fn set_sample_rate(&self, sps: u32) -> Result<(), SourceError> {
        // Sample rate changes require re-opening the device; not supported at runtime.
        Err(SourceError::SampleRateNotSupported(sps))
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
            name: format!(
                "RTL-SDR (device {})",
                self.config.device_index
            ),
            bias_t: true,
            direct_sampling: true,
            gain_range_db: (0.0, 49.6),
            has_hardware_cmd_tx: false,
        }
    }
}

// ── RTL-SDR device thread (rtlsdr feature only) ───────────────────────────────
//
// TODO: implement once a compatible RTL-SDR Rust binding is available.
// The thread should:
//   1. Open device at cfg.device_index
//   2. Set sample rate, centre frequency, gain mode, bias-T, direct sampling
//   3. Loop: poll freq_atomic for retuning, call read_sync, convert u8→f32,
//      publish Arc<[IqSample]> batches via tx
//   4. Exit when running goes false

#[cfg(feature = "rtlsdr")]
fn run_rtlsdr_thread(
    _tx: broadcast::Sender<Arc<[IqSample]>>,
    _running: Arc<AtomicBool>,
    _freq_atomic: Arc<AtomicU64>,
    _cfg: RtlSdrConfig,
) {
    // Placeholder: replace with real driver call once binding is linked.
    tracing::warn!("RTL-SDR feature enabled but driver not yet implemented");
}
