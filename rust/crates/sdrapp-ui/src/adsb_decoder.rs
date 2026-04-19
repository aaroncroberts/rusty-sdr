//! ADS-B decoder thread.
//!
//! Subscribes to the IQ broadcast channel and drives `PpmDemodulator` +
//! `parse_df17` in a dedicated OS thread.  The decoded aircraft state is
//! written into a shared `AircraftStore` that the UI reads each frame.
//!
//! # Usage
//! ```ignore
//! let decoder = AdsbDecoder::start(iq_rx, Arc::clone(&adsb_store));
//! // …later…
//! decoder.stop();
//! ```

use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};

use parking_lot::Mutex;
use tokio::sync::broadcast;

use sdrapp_core::sample::IqSample;
use sdrapp_adsb::{
    parser::parse_df17,
    state::AircraftStore,
    PpmDemodulator,
};

/// Running ADS-B decoder.
pub struct AdsbDecoder {
    running: Arc<AtomicBool>,
    /// Total Mode S DF-17 frames that passed CRC (for status display).
    pub frame_count: Arc<AtomicU64>,
    /// Total preamble detections before CRC check.
    /// > 0 means signal is present; if frame_count stays 0 with preambles > 0,
    /// the signal is detected but CRC is failing (wrong rate or frequency offset).
    pub preamble_count: Arc<AtomicU64>,
    /// Sample rate the decoder was started with (Hz).
    pub sample_rate: u32,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl AdsbDecoder {
    /// Spawn the decoder thread.
    ///
    /// `iq_rx` should be subscribed to the same broadcast sender as the main
    /// signal path — both receive every IQ batch independently.
    ///
    /// `sample_rate` is the IQ source rate in sps.  The PPM demodulator
    /// requires exactly 2 Msps; if `sample_rate` is higher, an integer
    /// decimation factor is derived and every Nth sample pair is used.
    /// If `sample_rate` < 2 Msps, a warning is logged and the decoder
    /// will likely produce no valid frames.
    pub fn start(
        iq_rx: broadcast::Receiver<Arc<[IqSample]>>,
        store: Arc<Mutex<AircraftStore>>,
        sample_rate: u32,
    ) -> Self {
        if sample_rate < 2_000_000 {
            tracing::warn!(
                sample_rate,
                "ADS-B decoder started with sample rate < 2 Msps; \
                 PpmDemodulator requires exactly 2 Msps — no frames expected"
            );
        }

        let running = Arc::new(AtomicBool::new(true));
        let frame_count = Arc::new(AtomicU64::new(0));
        let preamble_count = Arc::new(AtomicU64::new(0));

        let running_clone = Arc::clone(&running);
        let frame_count_clone = Arc::clone(&frame_count);
        let preamble_count_clone = Arc::clone(&preamble_count);

        let handle = std::thread::Builder::new()
            .name("sdrapp-adsb-decoder".into())
            .spawn(move || {
                decode_loop(
                    iq_rx,
                    store,
                    running_clone,
                    frame_count_clone,
                    preamble_count_clone,
                    sample_rate,
                );
            })
            .expect("failed to spawn ADS-B decoder thread");

        Self {
            running,
            frame_count,
            preamble_count,
            sample_rate,
            handle: Some(handle),
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

    /// Number of Mode S DF-17 frames that passed CRC.
    pub fn frames_decoded(&self) -> u64 {
        self.frame_count.load(Ordering::Relaxed)
    }

    /// Number of preamble detections (before CRC / DF17 check).
    ///
    /// If this is > 0 but `frames_decoded()` is 0, a signal is present but all
    /// frames are failing CRC — usually a sample-rate or frequency mismatch.
    /// If this stays 0, no signal is reaching the decoder at all.
    pub fn preambles_detected(&self) -> u64 {
        self.preamble_count.load(Ordering::Relaxed)
    }
}

impl Drop for AdsbDecoder {
    fn drop(&mut self) {
        self.stop();
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Integer decimation factor to bring `sample_rate` down to 2 Msps.
///
/// ADS-B PPM at 1090 MHz requires exactly 2 samples per 0.5 µs half-chip,
/// i.e. 2 Msps.  If the IQ source runs faster, take every Nth sample pair.
/// Returns 1 (no decimation) for rates ≤ 2 Msps.
pub(crate) fn decimation_factor(sample_rate: u32) -> usize {
    ((sample_rate / 2_000_000) as usize).max(1)
}

// ── Decoder loop ──────────────────────────────────────────────────────────────

fn decode_loop(
    mut iq_rx: broadcast::Receiver<Arc<[IqSample]>>,
    store: Arc<Mutex<AircraftStore>>,
    running: Arc<AtomicBool>,
    frame_count: Arc<AtomicU64>,
    preamble_count: Arc<AtomicU64>,
    sample_rate: u32,
) {
    let decimate = decimation_factor(sample_rate);
    let mut demod = PpmDemodulator::new();
    let mut prune_counter: u32 = 0;

    while running.load(Ordering::Relaxed) {
        match iq_rx.try_recv() {
            Ok(batch) => {
                // Flatten Complex<f32> to interleaved [re, im, re, im, …].
                // Step by `decimate` to bring higher sample rates down to 2 Msps
                // before feeding the PPM demodulator (which assumes 2 Msps).
                let interleaved: Vec<f32> = batch
                    .iter()
                    .step_by(decimate)
                    .flat_map(|s| [s.re, s.im])
                    .collect();

                let frames = demod.process(&interleaved);
                if !frames.is_empty() {
                    // Count preambles detected (signal present, before CRC check)
                    preamble_count.fetch_add(frames.len() as u64, Ordering::Relaxed);
                    let mut locked = store.lock();
                    for frame in &frames {
                        if let Some(decoded) = parse_df17(frame) {
                            locked.update(&decoded);
                            frame_count.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }

                // Prune stale aircraft every ~4096 batches (roughly every 1–2 s)
                prune_counter = prune_counter.wrapping_add(1);
                if prune_counter & 0x0FFF == 0 {
                    store.lock().prune_expired();
                }
            }
            Err(broadcast::error::TryRecvError::Empty) => {
                // No samples yet — yield briefly to avoid spinning the CPU
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            Err(broadcast::error::TryRecvError::Lagged(n)) => {
                // Fell behind; skip the lost frames and continue
                tracing::debug!("ADS-B decoder lagged by {n} batches");
            }
            Err(broadcast::error::TryRecvError::Closed) => {
                tracing::info!("ADS-B IQ broadcast closed — decoder thread exiting");
                break;
            }
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use sdrapp_adsb::{crc24, state::AircraftStore, PREAMBLE_LEN, SAMPLES_PER_BIT};

    // Preamble high-sample positions (not re-exported from sdrapp_adsb).
    const PREAMBLE_HIGH: [usize; 4] = [0, 2, 7, 9];

    // ── decimation_factor ─────────────────────────────────────────────────────

    #[test]
    fn decimation_factor_exact_2msps() {
        assert_eq!(decimation_factor(2_000_000), 1);
    }

    #[test]
    fn decimation_factor_4msps_halves() {
        assert_eq!(decimation_factor(4_000_000), 2);
    }

    #[test]
    fn decimation_factor_8msps_quarters() {
        assert_eq!(decimation_factor(8_000_000), 4);
    }

    #[test]
    fn decimation_factor_below_2msps_clamped_to_1() {
        // Rates below 2 Msps must not produce factor 0 (would panic in step_by)
        assert_eq!(decimation_factor(1_000_000), 1);
        assert_eq!(decimation_factor(1), 1);
        assert_eq!(decimation_factor(0), 1);
    }

    #[test]
    fn decimation_factor_non_power_of_two() {
        // 6 Msps → factor 3
        assert_eq!(decimation_factor(6_000_000), 3);
    }

    // ── AdsbDecoder lifecycle ─────────────────────────────────────────────────

    #[test]
    fn decoder_starts_and_stops_at_2msps() {
        let (tx, rx) = tokio::sync::broadcast::channel::<Arc<[IqSample]>>(16);
        let store = Arc::new(Mutex::new(AircraftStore::new()));
        let mut decoder = AdsbDecoder::start(rx, store, 2_000_000);
        assert!(decoder.is_running());
        assert_eq!(decoder.sample_rate, 2_000_000);
        drop(tx);
        decoder.stop();
        assert!(!decoder.is_running());
    }

    /// Verify that re-subscribing from a Sender produces a working decoder.
    /// This is the pattern used by the fix: SdrApp holds the Sender, each
    /// Start click calls tx.subscribe() rather than consuming a stored Receiver.
    #[test]
    fn decoder_can_restart_via_sender_resubscribe() {
        let (tx, _) = tokio::sync::broadcast::channel::<Arc<[IqSample]>>(16);
        let store = Arc::new(Mutex::new(AircraftStore::new()));

        // First start
        let rx1 = tx.subscribe();
        let mut decoder1 = AdsbDecoder::start(rx1, Arc::clone(&store), 2_000_000);
        assert!(decoder1.is_running());
        decoder1.stop();
        assert!(!decoder1.is_running());

        // Second start — fresh subscriber from the same sender
        let rx2 = tx.subscribe();
        let mut decoder2 = AdsbDecoder::start(rx2, Arc::clone(&store), 2_000_000);
        assert!(decoder2.is_running(), "decoder should restart after re-subscribing from sender");
        decoder2.stop();
    }

    #[test]
    fn decoder_records_sample_rate() {
        let (_tx, rx) = tokio::sync::broadcast::channel::<Arc<[IqSample]>>(4);
        let store = Arc::new(Mutex::new(AircraftStore::new()));
        let mut decoder = AdsbDecoder::start(rx, store, 8_000_000);
        assert_eq!(decoder.sample_rate, 8_000_000);
        decoder.stop();
    }

    #[test]
    fn frames_decoded_starts_at_zero() {
        let (_tx, rx) = tokio::sync::broadcast::channel::<Arc<[IqSample]>>(4);
        let store = Arc::new(Mutex::new(AircraftStore::new()));
        let mut decoder = AdsbDecoder::start(rx, store, 2_000_000);
        assert_eq!(decoder.frames_decoded(), 0);
        decoder.stop();
    }

    // ── Integration: synthetic IQ → AircraftStore ─────────────────────────────

    /// Build a 16-sample magnitude preamble (ICAO Annex 10 pattern).
    fn make_preamble_mag() -> Vec<f32> {
        let mut v = vec![0.0f32; PREAMBLE_LEN];
        for &p in &PREAMBLE_HIGH {
            v[p] = 1.0;
        }
        v
    }

    /// Encode one byte as 16 magnitude samples (PPM: bit=1 → [1,0], bit=0 → [0,1]).
    fn encode_byte_mag(byte: u8) -> Vec<f32> {
        let mut v = Vec::with_capacity(16);
        for bit in 0..8 {
            let is_one = (byte >> (7 - bit)) & 1 == 1;
            if is_one {
                v.extend_from_slice(&[1.0, 0.0]);
            } else {
                v.extend_from_slice(&[0.0, 1.0]);
            }
        }
        v
    }

    /// Build a magnitude buffer for a complete Mode S frame.
    fn encode_frame_mag(frame_bytes: &[u8]) -> Vec<f32> {
        let mut v = make_preamble_mag();
        for &b in frame_bytes {
            v.extend(encode_byte_mag(b));
        }
        v
    }

    /// Convert magnitude samples to `IqSample` (Complex<f32>) where I=mag, Q=0.
    /// The demodulator reconstructs magnitude as sqrt(I²+Q²), so this is lossless.
    fn mag_to_iq_samples(mags: &[f32]) -> Vec<IqSample> {
        mags.iter().map(|&m| IqSample::new(m, 0.0)).collect()
    }

    /// End-to-end integration test: synthesize a known DF17 frame as IQ at 2 Msps,
    /// send it through the broadcast channel, start `AdsbDecoder`, and assert that
    /// the expected ICAO address appears in `AircraftStore` within the timeout.
    #[test]
    fn decoder_populates_store_from_synthetic_iq() {
        // Known DF17 identification frame: KLM1023 / ICAO 0x4840D6.
        // Source: widely-cited ADS-B test vector (verified CRC in parser tests).
        let frame_bytes: [u8; 14] = [
            0x8D, 0x48, 0x40, 0xD6, // DF=17, ICAO=4840D6
            0x20, 0x2C, 0xC3, 0x71, 0xC3, 0x2C, 0xE0, // ME (TC=4, callsign "KLM1023 ")
            0x57, 0x60, 0x98, // CRC
        ];
        assert_eq!(crc24(&frame_bytes), [0, 0, 0], "test vector CRC must be valid");

        let target_icao: u32 = 0x4840D6;

        // Build IQ batch: preamble + 14 encoded bytes + silence padding so the
        // demodulator has the required lookahead (PREAMBLE_LEN + 112 * 2 = 240 samples).
        let mut mags = encode_frame_mag(&frame_bytes);
        let silence_needed = PREAMBLE_LEN + sdrapp_adsb::LONG_MSG_BITS * SAMPLES_PER_BIT;
        mags.extend(vec![0.0f32; silence_needed]);
        let samples: Arc<[IqSample]> = mag_to_iq_samples(&mags).into();

        let (tx, rx) = tokio::sync::broadcast::channel::<Arc<[IqSample]>>(32);
        let store = Arc::new(Mutex::new(AircraftStore::new()));
        let mut decoder = AdsbDecoder::start(rx, Arc::clone(&store), 2_000_000);

        // Send the synthetic IQ batch.
        tx.send(samples).expect("channel send failed");

        // Spin until the aircraft appears or we time out (~500 ms).
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
        let mut found = false;
        while std::time::Instant::now() < deadline {
            if store.lock().get(target_icao).is_some() {
                found = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }

        decoder.stop();
        assert!(found, "ICAO 0x{target_icao:06X} should appear in AircraftStore after decoding synthetic IQ");
    }
}
