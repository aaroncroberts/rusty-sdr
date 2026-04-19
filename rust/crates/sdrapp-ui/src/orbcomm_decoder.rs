//! Orbcomm satellite decoder thread.
//!
//! Subscribes to the same IQ broadcast channel used by the ADS-B decoder and
//! runs the `sdrapp-orbcomm` pipeline in a dedicated OS thread:
//!
//! ```text
//! IQ broadcast
//!   └─► decimate to ~48 kHz
//!         └─► SdpskDemod (phase-diff → soft symbols)
//!               └─► GardnerClock (clock recovery)
//!                     └─► FrameSync (sync-word correlation)
//!                           └─► parser (type + FCS → OrbcommPacket)
//!                                 └─► crossbeam channel → UI
//! ```
//!
//! Decoded packets appear as [`OrbcommLogEntry`] values on the receive end of
//! the channel returned by [`OrbcommDecoder::packet_rx`].

use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};

use num_complex::Complex32;
use tokio::sync::broadcast;

use sdrapp_core::sample::IqSample;
use sdrapp_orbcomm::{
    demod::{GardnerClock, SdpskDemod},
    framer::FrameSync,
    parser::{self, OrbcommPacket},
    SYMBOL_RATE,
};

/// A single decoded Orbcomm frame with receive metadata.
#[derive(Debug, Clone)]
pub struct OrbcommLogEntry {
    /// Wall-clock time the frame was decoded.
    pub received_at: std::time::SystemTime,
    /// The decoded packet (type, data words, FCS status).
    pub packet: OrbcommPacket,
    /// Frequency the hardware was tuned to when the frame was received (Hz).
    pub freq_hz: u64,
}

/// Handle to the running Orbcomm decoder thread.
pub struct OrbcommDecoder {
    running: Arc<AtomicBool>,
    /// Total frames whose Fletcher FCS verified correctly.
    pub frame_count: Arc<AtomicU64>,
    /// Total frames received (including FCS failures).
    pub raw_frame_count: Arc<AtomicU64>,
    /// Sample rate the decoder was started with.
    pub sample_rate: u32,
    handle: Option<std::thread::JoinHandle<()>>,
    /// Receiver side of the decoded-packet channel.
    packet_rx: crossbeam_channel::Receiver<OrbcommLogEntry>,
}

impl OrbcommDecoder {
    /// Spawn the decoder thread.
    ///
    /// - `iq_rx` — a fresh broadcast receiver (call `tx.subscribe()` each time).
    /// - `sample_rate` — the IQ source sample rate.  Decimated to ~48 kHz internally.
    /// - `freq_hz` — current hardware centre frequency (for log entries).
    pub fn start(
        iq_rx: broadcast::Receiver<Arc<[IqSample]>>,
        sample_rate: u32,
        freq_hz: u64,
    ) -> Self {
        let running = Arc::new(AtomicBool::new(true));
        let frame_count = Arc::new(AtomicU64::new(0));
        let raw_frame_count = Arc::new(AtomicU64::new(0));

        let (packet_tx, packet_rx) = crossbeam_channel::bounded::<OrbcommLogEntry>(256);

        let running_clone = Arc::clone(&running);
        let frame_count_clone = Arc::clone(&frame_count);
        let raw_frame_count_clone = Arc::clone(&raw_frame_count);

        let handle = std::thread::Builder::new()
            .name("sdrapp-orbcomm-decoder".into())
            .spawn(move || {
                decode_loop(
                    iq_rx,
                    running_clone,
                    frame_count_clone,
                    raw_frame_count_clone,
                    packet_tx,
                    sample_rate,
                    freq_hz,
                );
            })
            .expect("failed to spawn Orbcomm decoder thread");

        Self {
            running,
            frame_count,
            raw_frame_count,
            sample_rate,
            handle: Some(handle),
            packet_rx,
        }
    }

    /// Signal the decoder thread to stop and wait for it to exit.
    pub fn stop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }

    /// `true` while the decoder thread is running.
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }

    /// Number of frames whose Fletcher FCS verified.
    pub fn frames_decoded(&self) -> u64 {
        self.frame_count.load(Ordering::Relaxed)
    }

    /// Number of frames received total (including FCS failures).
    pub fn raw_frames(&self) -> u64 {
        self.raw_frame_count.load(Ordering::Relaxed)
    }

    /// Receive end of the decoded-packet channel.  Drain this each UI frame.
    pub fn packet_rx(&self) -> &crossbeam_channel::Receiver<OrbcommLogEntry> {
        &self.packet_rx
    }
}

impl Drop for OrbcommDecoder {
    fn drop(&mut self) {
        self.stop();
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Compute integer decimation factor to bring `sample_rate` down to ~48 kHz.
///
/// The Orbcomm SDPSK runs at 4800 baud; 48 kHz gives 10 samples/symbol.
/// We keep at least 1× (no decimation) so the function never panics.
pub(crate) fn decimation_factor(sample_rate: u32) -> usize {
    let target = sdrapp_orbcomm::SAMPLE_RATE_HZ;
    ((sample_rate / target).max(1)) as usize
}

/// Effective samples-per-symbol after decimation.
pub(crate) fn effective_sps(sample_rate: u32) -> f32 {
    let decim = decimation_factor(sample_rate) as f32;
    let effective_rate = sample_rate as f32 / decim;
    effective_rate / SYMBOL_RATE as f32
}

// ── Decoder loop ──────────────────────────────────────────────────────────────

fn decode_loop(
    mut iq_rx: broadcast::Receiver<Arc<[IqSample]>>,
    running: Arc<AtomicBool>,
    frame_count: Arc<AtomicU64>,
    raw_frame_count: Arc<AtomicU64>,
    packet_tx: crossbeam_channel::Sender<OrbcommLogEntry>,
    sample_rate: u32,
    freq_hz: u64,
) {
    let decim = decimation_factor(sample_rate);
    let sps = effective_sps(sample_rate);

    let mut demod = SdpskDemod::new();
    let mut clock = GardnerClock::new(sps);
    let mut framer = FrameSync::new();

    tracing::info!(
        sample_rate,
        decim,
        sps,
        freq_hz,
        "Orbcomm decoder thread started"
    );

    while running.load(Ordering::Relaxed) {
        match iq_rx.try_recv() {
            Ok(batch) => {
                // Decimate and convert IqSample (Complex<f32>) to num_complex::Complex32.
                // They are both f32 complex — the cast is a zero-cost transmute.
                for sample in batch.iter().step_by(decim) {
                    let c = Complex32::new(sample.re, sample.im);
                    let soft = demod.push(c);
                    if let Some(sym) = clock.push(soft) {
                        let bit = if sym > 0.0 { 1u8 } else { 0u8 };
                        if framer.push_bit(bit) {
                            if let Some(payload) = framer.take_frame() {
                                raw_frame_count.fetch_add(1, Ordering::Relaxed);
                                match parser::parse(&payload) {
                                    Ok(pkt) => {
                                        if pkt.crc_ok {
                                            frame_count.fetch_add(1, Ordering::Relaxed);
                                        }
                                        let entry = OrbcommLogEntry {
                                            received_at: std::time::SystemTime::now(),
                                            packet: pkt,
                                            freq_hz,
                                        };
                                        // Non-blocking send: if the UI isn't draining fast
                                        // enough, drop the oldest frames rather than blocking.
                                        let _ = packet_tx.try_send(entry);
                                    }
                                    Err(e) => {
                                        tracing::debug!("Orbcomm parse error: {e}");
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Err(broadcast::error::TryRecvError::Empty) => {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            Err(broadcast::error::TryRecvError::Lagged(n)) => {
                tracing::debug!("Orbcomm decoder lagged by {n} batches");
                demod.reset();
                clock.reset();
            }
            Err(broadcast::error::TryRecvError::Closed) => {
                tracing::info!("Orbcomm IQ broadcast closed — decoder thread exiting");
                break;
            }
        }
    }

    tracing::info!("Orbcomm decoder thread stopped");
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── decimation_factor ─────────────────────────────────────────────────────

    #[test]
    fn decimation_at_48khz_is_one() {
        // At exactly 48 kHz, no decimation needed.
        assert_eq!(decimation_factor(48_000), 1);
    }

    #[test]
    fn decimation_at_200khz() {
        // 200 kHz / 48 kHz ≈ 4.2 → integer 4
        assert_eq!(decimation_factor(200_000), 4);
    }

    #[test]
    fn decimation_at_2mhz() {
        // 2 MHz / 48 kHz ≈ 41.7 → integer 41
        assert_eq!(decimation_factor(2_000_000), 41);
    }

    #[test]
    fn decimation_never_zero() {
        // Even absurdly low rates should produce factor ≥ 1.
        assert!(decimation_factor(1) >= 1);
        assert!(decimation_factor(100) >= 1);
    }

    // ── effective_sps ─────────────────────────────────────────────────────────

    #[test]
    fn effective_sps_at_200khz_is_reasonable() {
        // 200 kHz / 4 = 50 kHz effective → 50000 / 4800 ≈ 10.4 sps
        let sps = effective_sps(200_000);
        assert!(sps > 8.0 && sps < 14.0, "Expected sps ≈ 10, got {sps}");
    }

    #[test]
    fn effective_sps_at_48khz_is_exactly_ten() {
        // 48 kHz / 1 = 48 kHz → 48000 / 4800 = 10
        let sps = effective_sps(48_000);
        assert!((sps - 10.0).abs() < 0.01, "Expected sps = 10.0, got {sps}");
    }

    // ── OrbcommDecoder lifecycle ──────────────────────────────────────────────

    #[test]
    fn decoder_starts_and_is_running() {
        let (tx, rx) = tokio::sync::broadcast::channel::<Arc<[IqSample]>>(16);
        let mut decoder = OrbcommDecoder::start(rx, 200_000, 137_500_000);
        assert!(decoder.is_running());
        drop(tx);
        decoder.stop();
        assert!(!decoder.is_running());
    }

    #[test]
    fn decoder_stops_cleanly() {
        let (_tx, rx) = tokio::sync::broadcast::channel::<Arc<[IqSample]>>(16);
        let mut decoder = OrbcommDecoder::start(rx, 200_000, 137_500_000);
        decoder.stop();
        assert!(!decoder.is_running());
    }

    #[test]
    fn decoder_drop_stops_thread() {
        let (tx, rx) = tokio::sync::broadcast::channel::<Arc<[IqSample]>>(16);
        {
            let _decoder = OrbcommDecoder::start(rx, 200_000, 137_500_000);
            // Drop here
        }
        // If the thread didn't stop, the broadcast channel would panic on drop.
        // The test passes if we get here without hanging.
        drop(tx);
    }

    #[test]
    fn decoder_records_sample_rate() {
        let (_tx, rx) = tokio::sync::broadcast::channel::<Arc<[IqSample]>>(4);
        let mut decoder = OrbcommDecoder::start(rx, 250_000, 137_621_000);
        assert_eq!(decoder.sample_rate, 250_000);
        decoder.stop();
    }

    #[test]
    fn decoder_frame_counts_start_at_zero() {
        let (_tx, rx) = tokio::sync::broadcast::channel::<Arc<[IqSample]>>(4);
        let mut decoder = OrbcommDecoder::start(rx, 200_000, 137_500_000);
        assert_eq!(decoder.frames_decoded(), 0);
        assert_eq!(decoder.raw_frames(), 0);
        decoder.stop();
    }

    #[test]
    fn decoder_can_restart_via_resubscribe() {
        let (tx, _) = tokio::sync::broadcast::channel::<Arc<[IqSample]>>(16);

        let rx1 = tx.subscribe();
        let mut dec1 = OrbcommDecoder::start(rx1, 200_000, 137_500_000);
        assert!(dec1.is_running());
        dec1.stop();

        let rx2 = tx.subscribe();
        let mut dec2 = OrbcommDecoder::start(rx2, 200_000, 137_500_000);
        assert!(dec2.is_running(), "should restart via new subscriber");
        dec2.stop();
    }

    #[test]
    fn packet_channel_is_empty_on_start() {
        let (_tx, rx) = tokio::sync::broadcast::channel::<Arc<[IqSample]>>(4);
        let mut decoder = OrbcommDecoder::start(rx, 200_000, 137_500_000);
        assert!(decoder.packet_rx().try_recv().is_err(), "No packets expected at startup");
        decoder.stop();
    }

    /// Send a minimal IQ batch (all zeros) and verify the decoder stays alive
    /// and does not produce phantom packets.
    #[test]
    fn decoder_handles_silent_iq_without_phantom_packets() {
        let (tx, rx) = tokio::sync::broadcast::channel::<Arc<[IqSample]>>(32);
        let mut decoder = OrbcommDecoder::start(rx, 200_000, 137_500_000);

        // Send silence for a while.
        let silence: Arc<[IqSample]> = vec![IqSample::new(0.0, 0.0); 512].into();
        for _ in 0..10 {
            let _ = tx.send(Arc::clone(&silence));
        }
        std::thread::sleep(std::time::Duration::from_millis(50));

        let n_packets: usize = decoder.packet_rx().try_iter().count();
        decoder.stop();
        assert_eq!(n_packets, 0, "Expected no packets from silent IQ");
    }
}
