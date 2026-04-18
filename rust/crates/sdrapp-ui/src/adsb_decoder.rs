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
    /// Total Mode S frames decoded (for status display).
    pub frame_count: Arc<AtomicU64>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl AdsbDecoder {
    /// Spawn the decoder thread.
    ///
    /// `iq_rx` should be subscribed to the same broadcast sender as the main
    /// signal path — both receive every IQ batch independently.
    pub fn start(
        iq_rx: broadcast::Receiver<Arc<[IqSample]>>,
        store: Arc<Mutex<AircraftStore>>,
    ) -> Self {
        let running = Arc::new(AtomicBool::new(true));
        let frame_count = Arc::new(AtomicU64::new(0));

        let running_clone = Arc::clone(&running);
        let frame_count_clone = Arc::clone(&frame_count);

        let handle = std::thread::Builder::new()
            .name("sdrapp-adsb-decoder".into())
            .spawn(move || {
                decode_loop(iq_rx, store, running_clone, frame_count_clone);
            })
            .expect("failed to spawn ADS-B decoder thread");

        Self {
            running,
            frame_count,
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

    /// Number of Mode S frames successfully decoded so far.
    pub fn frames_decoded(&self) -> u64 {
        self.frame_count.load(Ordering::Relaxed)
    }
}

impl Drop for AdsbDecoder {
    fn drop(&mut self) {
        self.stop();
    }
}

// ── Decoder loop ──────────────────────────────────────────────────────────────

fn decode_loop(
    mut iq_rx: broadcast::Receiver<Arc<[IqSample]>>,
    store: Arc<Mutex<AircraftStore>>,
    running: Arc<AtomicBool>,
    frame_count: Arc<AtomicU64>,
) {
    let mut demod = PpmDemodulator::new();
    let mut prune_counter: u32 = 0;

    while running.load(Ordering::Relaxed) {
        match iq_rx.try_recv() {
            Ok(batch) => {
                // Flatten Complex<f32> to interleaved [re, im, re, im, …]
                let interleaved: Vec<f32> = batch
                    .iter()
                    .flat_map(|s| [s.re, s.im])
                    .collect();

                let frames = demod.process(&interleaved);
                if !frames.is_empty() {
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
