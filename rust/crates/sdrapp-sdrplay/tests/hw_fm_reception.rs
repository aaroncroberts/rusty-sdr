//! Hardware integration test: FM broadcast reception at 93.5 MHz.
//!
//! **This test requires physical SDRplay hardware and a live FM broadcast.**
//! It is gated behind the `SDRPLAY_HW_TEST` environment variable so CI never
//! runs it.
//!
//! # How to run
//! ```sh
//! SDRPLAY_HW_TEST=1 cargo test \
//!     --package sdrapp-sdrplay \
//!     --test hw_fm_reception \
//!     -- --nocapture
//! ```
//!
//! # What is being proved
//!
//! Static noise has no coherent 19 kHz component — it is band-limited white
//! noise.  A real FM stereo broadcast always contains a pilot tone at exactly
//! 19 kHz (100 Hz tolerance).  Our `StereoFmDecoder` drives a PLL that locks
//! to this pilot; `is_stereo()` returns `true` only when the normalised pilot
//! amplitude exceeds `PILOT_THRESHOLD = 0.02`.
//!
//! If this assertion passes: **we decoded a real FM station, not static.**

use std::time::{Duration, Instant};

use sdrapp_core::{
    block::Block,
    dsp::StereoFmDecoder,
    sample::StereoFrame,
    source::Source,
};
use sdrapp_sdrplay::{Antenna, IfMode, RspdxConfig, RspdxSource};

/// Software decimation: keep every Nth sample.
fn decimate(iq: &[sdrapp_core::sample::IqSample], factor: usize) -> Vec<sdrapp_core::sample::IqSample> {
    iq.iter().step_by(factor).cloned().collect()
}

/// RMS amplitude of stereo audio frames (average of L and R).
fn rms(frames: &[StereoFrame]) -> f32 {
    if frames.is_empty() {
        return 0.0;
    }
    let sum_sq: f32 = frames
        .iter()
        .map(|f| f.left * f.left + f.right * f.right)
        .sum();
    (sum_sq / (frames.len() as f32 * 2.0)).sqrt()
}

// ─────────────────────────────────────────────────────────────────────────────

/// Tune to 93.5 MHz (BBC Radio 4 FM, UK), receive for 4 seconds, and assert
/// that the stereo pilot tone is detected — proof of real FM broadcast reception.
///
/// Signal path mirrors the production pipeline exactly:
///   SDRplay 2 MHz → 4× hw decimation → 500 kHz IQ
///   → 2× SW decimation → 250 kHz
///   → StereoFmDecoder(250 kHz) → 48 kHz audio
#[tokio::test]
async fn hw_fm_93_5mhz_stereo_pilot_detected() {
    // ── Gate: require explicit opt-in ──────────────────────────────────────
    if std::env::var("SDRPLAY_HW_TEST").is_err() {
        eprintln!("[hw_fm_reception] SKIP — set SDRPLAY_HW_TEST=1 to enable");
        return;
    }

    // ── Gate: require hardware to be present ───────────────────────────────
    if !RspdxSource::is_device_available() {
        eprintln!("[hw_fm_reception] SKIP — no SDRplay device found");
        return;
    }

    eprintln!("[hw_fm_reception] SDRplay hardware detected — starting test");

    // ── Configure: 93.5 MHz, LNA=4 (avoids ADC overload on strong UK FM) ──
    // LNA=3 causes ADC overload on strong FM stations (confirmed empirically).
    // LNA=4 keeps signal within range; AGC handles IF gain from there.
    let config = RspdxConfig {
        frequency_hz: 93_500_000, // 93.5 MHz — BBC Radio 4 FM
        sample_rate_sps: 2_000_000,
        antenna: Antenna::A,
        if_mode: IfMode::ZeroIf,
        lna_state: 4,
        agc_enabled: true,
        agc_setpoint_dbfs: -30, // Target -30 dBFS — good FM reception level
        decimation_factor: 4,   // 2 MHz / 4 = 500 kHz post-hardware
        ..RspdxConfig::default()
    };

    // ── Subscribe before start() so no batches are missed ──────────────────
    let mut source = RspdxSource::new(config);
    let mut iq_rx = source.subscribe();
    let _handle = source.start();

    eprintln!("[hw_fm_reception] Waiting 1 s for AGC to settle...");
    tokio::time::sleep(Duration::from_secs(1)).await;

    // ── Receive and decode for 4 seconds ───────────────────────────────────
    // demod_sr = 500kHz / 2 = 250 kHz (matches production signal_path)
    let mut decoder = StereoFmDecoder::new(250_000);

    // Accumulate per-second RMS windows to verify audio dynamics
    let mut second_rms: Vec<f32> = Vec::new();
    let mut window_frames: Vec<StereoFrame> = Vec::new();
    let mut window_start = Instant::now();

    let test_end = Instant::now() + Duration::from_secs(4);
    let mut total_frames: usize = 0;
    let mut batches_received: u64 = 0;
    let mut pilot_locks: u64 = 0;

    while Instant::now() < test_end {
        match tokio::time::timeout(Duration::from_millis(200), iq_rx.recv()).await {
            Ok(Ok(batch)) => {
                batches_received += 1;

                // 2× software decimation: 500 kHz → 250 kHz
                let decimated = decimate(&batch, 2);

                let (frames, _) = decoder.process(&decimated);
                total_frames += frames.len();
                window_frames.extend_from_slice(&frames);

                if decoder.is_stereo() {
                    pilot_locks += 1;
                }

                // Snapshot per-second RMS window
                if window_start.elapsed() >= Duration::from_secs(1) {
                    let r = rms(&window_frames);
                    eprintln!(
                        "[hw_fm_reception] t={:.1}s  RMS={:.4}  stereo={}  pilot_locks={}/{}",
                        (4.0 - test_end.duration_since(Instant::now()).as_secs_f32()),
                        r,
                        decoder.is_stereo(),
                        pilot_locks,
                        batches_received,
                    );
                    second_rms.push(r);
                    window_frames.clear();
                    window_start = Instant::now();
                }
            }
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(n))) => {
                eprintln!("[hw_fm_reception] WARN: receiver lagged {n} batches — IQ processing too slow?");
            }
            Ok(Err(_)) => {
                eprintln!("[hw_fm_reception] IQ channel closed");
                break;
            }
            Err(_) => {
                // timeout — no batch arrived in 200 ms
                eprintln!("[hw_fm_reception] WARN: no IQ batch for 200 ms");
            }
        }
    }

    source.stop();

    eprintln!(
        "[hw_fm_reception] Done — {} frames decoded across {} batches",
        total_frames, batches_received
    );
    eprintln!(
        "[hw_fm_reception] Pilot locks: {}/{} batches ({:.1}%)",
        pilot_locks,
        batches_received,
        if batches_received > 0 {
            100.0 * pilot_locks as f32 / batches_received as f32
        } else {
            0.0
        }
    );
    if !second_rms.is_empty() {
        let mean_rms: f32 = second_rms.iter().sum::<f32>() / second_rms.len() as f32;
        let rms_variance: f32 = second_rms
            .iter()
            .map(|r| (r - mean_rms).powi(2))
            .sum::<f32>()
            / second_rms.len() as f32;
        eprintln!(
            "[hw_fm_reception] Per-second RMS: {:?}",
            second_rms
                .iter()
                .map(|r| format!("{r:.4}"))
                .collect::<Vec<_>>()
        );
        eprintln!(
            "[hw_fm_reception] Mean RMS={mean_rms:.4}  Variance={rms_variance:.6}"
        );
    }

    // ── ASSERTION 1: Stereo pilot detected ────────────────────────────────
    // This is the primary proof: a coherent 19 kHz pilot exists in the signal.
    // Static / noise has no coherent 19 kHz component.
    assert!(
        decoder.is_stereo(),
        "Stereo pilot NOT detected at 93.5 MHz — receiving noise/static, not a real FM station.\n\
         Check: antenna connected? Correct frequency for your location? LNA setting appropriate?\n\
         Try tuning to a strong local FM station and re-running."
    );

    // ── ASSERTION 2: Audible output ─────────────────────────────────────────
    let final_rms = rms(&window_frames);
    let overall_rms = if !second_rms.is_empty() {
        second_rms.iter().sum::<f32>() / second_rms.len() as f32
    } else {
        final_rms
    };

    assert!(
        overall_rms > 0.005,
        "Audio RMS {overall_rms:.5} is below 0.005 — decoder is producing silence"
    );

    // ── ASSERTION 3: Audio has dynamics (not frozen / stuck) ────────────────
    // Static has near-constant RMS. Real audio (speech, music) varies.
    // We only check this if we have at least 3 complete 1-second windows.
    if second_rms.len() >= 3 {
        let mean_rms: f32 = second_rms.iter().sum::<f32>() / second_rms.len() as f32;
        let rms_variance: f32 = second_rms
            .iter()
            .map(|r| (r - mean_rms).powi(2))
            .sum::<f32>()
            / second_rms.len() as f32;

        // A stuck decoder outputs identical frames every window → variance ≈ 0.
        // Even a sine wave carrier has zero variance. Real audio must vary.
        // We use a very loose threshold (>1e-8) to avoid flakiness.
        assert!(
            rms_variance > 1e-8,
            "RMS variance {rms_variance:.2e} is zero — audio output appears frozen/stuck"
        );
    }

    eprintln!("[hw_fm_reception] PASS — real FM broadcast decoded at 93.5 MHz");
}
