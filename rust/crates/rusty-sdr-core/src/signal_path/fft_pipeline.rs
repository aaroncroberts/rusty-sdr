//! FFT accumulation, EMA averaging, and passband metric computation.
//!
//! Wraps the per-batch FFT pipeline so `mod.rs` only calls
//! `fft_pipeline.tick(batch, shared, egui_ctx)` instead of managing seven
//! local variables inline.

use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::RwLock;

use crate::dsp::{
    fft::{any_bin_clipping, compute_snr_db},
    FftProcessor, FftWindow,
};
use crate::sample::IqSample;

use super::egui_repaint::RepaintHandle;
use super::shared_state::{DemodMode, SharedState};

/// Rate limit for writing FFT results to shared state / requesting UI repaints.
const WRITE_INTERVAL: Duration = Duration::from_millis(33); // ~30 Hz

/// FFT accumulator + EMA averager + passband metric publisher.
///
/// Call [`FftPipeline::tick`] once per IQ batch. The pipeline:
/// 1. Appends the batch to an internal accumulator.
/// 2. When the accumulator reaches `fft_size`, runs the FFT.
/// 3. Updates the EMA-averaged magnitude buffer.
/// 4. At most 30 times per second, writes metrics to `SharedState` and
///    requests a UI repaint.
pub(super) struct FftPipeline {
    pub fft_size: usize,
    pub window: FftWindow,
    fft: FftProcessor,
    pub fft_averaging: u8,
    /// EMA-averaged magnitude buffer (dBFS per bin, center-DC ordered).
    pub fft_avg_buf: Vec<f32>,
    /// IQ sample accumulator — filled until `fft_size` samples are available.
    iq_acc: Vec<IqSample>,
    last_write: Instant,
}

impl FftPipeline {
    pub fn new(fft_size: usize, window: FftWindow, averaging: u8) -> Self {
        Self {
            fft_size,
            window,
            fft: FftProcessor::new(fft_size, window),
            fft_averaging: averaging,
            fft_avg_buf: vec![-120.0; fft_size],
            iq_acc: Vec::with_capacity(fft_size * 2),
            last_write: Instant::now() - WRITE_INTERVAL, // fire on first ready frame
        }
    }

    /// Change FFT size and/or window at runtime (e.g. from a UI command).
    ///
    /// Resets the accumulator and average buffer so stale data is discarded.
    pub fn resize(&mut self, new_size: usize, window: FftWindow) {
        self.fft_size = new_size;
        self.window = window;
        self.fft = FftProcessor::new(new_size, window);
        self.fft_avg_buf = vec![-120.0; new_size];
        self.iq_acc.clear();
    }

    /// Discard accumulated IQ samples (e.g. after frequency change or demod switch).
    pub fn clear_accumulator(&mut self) {
        self.iq_acc.clear();
    }

    /// Process one IQ batch.
    ///
    /// When the internal accumulator has enough samples, runs the FFT, updates
    /// the EMA, and — at most 30 Hz — writes the averaged magnitudes plus
    /// passband metrics to `SharedState` and requests a UI repaint.
    ///
    /// Returns `Some(signal_level_dbfs)` when a shared-state write actually
    /// occurred (i.e., the 30 Hz gate fired), so the scanner can read the
    /// freshest level without an extra lock acquisition.
    pub fn tick(
        &mut self,
        batch: &[IqSample],
        shared: &Arc<RwLock<SharedState>>,
        egui_ctx: &Option<RepaintHandle>,
    ) -> Option<f32> {
        self.iq_acc.extend_from_slice(batch);
        if self.iq_acc.len() < self.fft_size {
            return None;
        }

        // Run FFT on the accumulated samples.
        if let Some(mags) = self.fft.process(&self.iq_acc) {
            let alpha = if self.fft_averaging <= 1 {
                1.0_f32
            } else {
                2.0 / (self.fft_averaging as f32 + 1.0)
            };
            for (avg, &new) in self.fft_avg_buf.iter_mut().zip(mags.iter()) {
                *avg = alpha * new + (1.0 - alpha) * *avg;
            }
        }
        self.iq_acc.drain(..self.fft_size);

        // Rate-limited metrics write.
        let now = Instant::now();
        if now.duration_since(self.last_write) < WRITE_INTERVAL {
            return None;
        }
        self.last_write = now;

        let n = self.fft_avg_buf.len();
        let center = n / 2;

        // Read demod context under a short read-lock.
        let (bw_hz, is_wbfm, sr_hz) = {
            let s = shared.read();
            let bw = match s.demod.demod_mode {
                DemodMode::Wbfm => 200_000_u32,
                DemodMode::Nfm => s.demod.nfm_bandwidth_hz,
                DemodMode::Am | DemodMode::Usb | DemodMode::Lsb | DemodMode::Dsb => 10_000,
                DemodMode::Cw => 1_000,
            };
            let wbfm = s.demod.demod_mode == DemodMode::Wbfm;
            let rate = s.sample_rate_sps.max(1) as f32;
            (bw, wbfm, rate)
        };

        let mut half_bw_bins = ((bw_hz as f32 / sr_hz * n as f32) as usize)
            .max(2)
            .min(n / 4);
        if is_wbfm {
            half_bw_bins = (half_bw_bins * 3 / 2).min(n / 4);
        }

        let snr = compute_snr_db(&self.fft_avg_buf, center, half_bw_bins);
        let lo = center.saturating_sub(half_bw_bins);
        let hi = (center + half_bw_bins + 1).min(n);
        let signal_level_dbfs = self.fft_avg_buf[lo..hi]
            .iter()
            .cloned()
            .fold(f32::NEG_INFINITY, f32::max);

        let clipping = any_bin_clipping(&self.fft_avg_buf);

        {
            let mut s = shared.write();
            s.fft.fft_magnitudes = self.fft_avg_buf.clone();
            s.fft.snr_db = Some(snr);
            s.fft.fft_clipping_detected = clipping;
            s.fft.signal_level_dbfs = signal_level_dbfs;
        }

        if let Some(ref ctx) = egui_ctx {
            ctx.request_repaint();
        }

        Some(signal_level_dbfs)
    }
}
