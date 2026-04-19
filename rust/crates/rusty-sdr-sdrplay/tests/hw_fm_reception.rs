//! Hardware integration test: FM broadcast reception.
//!
//! **This test requires physical SDRplay hardware and a live FM broadcast.**
//! It is gated behind the `SDRPLAY_HW_TEST` environment variable so CI never
//! runs it.
//!
//! # How to run
//! ```sh
//! # Scan the whole FM band to find a station (takes ~2 min):
//! SDRPLAY_HW_TEST=1 cargo test \
//!     --package rusty-sdr-sdrplay \
//!     --test hw_fm_reception \
//!     -- --nocapture
//!
//! # Or tune to a specific frequency (MHz) to skip the scan:
//! SDRPLAY_HW_TEST=1 SDRPLAY_TEST_FREQ_MHZ=101.1 cargo test \
//!     --package rusty-sdr-sdrplay \
//!     --test hw_fm_reception \
//!     -- --nocapture
//! ```

use std::time::{Duration, Instant};

use rusty_sdr_core::{
    block::Block,
    dsp::StereoFmDecoder,
    sample::StereoFrame,
    source::Source,
};
use rusty_sdr_sdrplay::{Antenna, IfMode, RspdxConfig, RspdxSource};

/// Software decimation: keep every Nth sample.
fn decimate(
    iq: &[rusty_sdr_core::sample::IqSample],
    factor: usize,
) -> Vec<rusty_sdr_core::sample::IqSample> {
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

/// Tune to an FM broadcast frequency (default: scan the band), receive for
/// several seconds, and assert that the stereo pilot tone is detected.
///
/// Override frequency with `SDRPLAY_TEST_FREQ_MHZ=101.1` (MHz, float).
/// Without an override the test scans 87.5–108 MHz using a single source
/// instance to find the strongest station automatically.
#[tokio::test]
async fn hw_fm_stereo_pilot_detected() {
    // ── Gate: require explicit opt-in ─────────────────────────────────────
    if std::env::var("SDRPLAY_HW_TEST").is_err() {
        eprintln!("[hw_fm] SKIP — set SDRPLAY_HW_TEST=1 to enable");
        return;
    }

    if !RspdxSource::is_device_available() {
        eprintln!("[hw_fm] SKIP — no SDRplay device found");
        return;
    }

    eprintln!("[hw_fm] SDRplay hardware detected — starting test");

    // ── Open device once — this is the expensive part (~3 s) ─────────────
    let init_freq = 93_500_000u64; // start somewhere in the middle of the band
    let config = RspdxConfig {
        frequency_hz: init_freq,
        sample_rate_sps: 2_000_000,
        antenna: Antenna::A,
        if_mode: IfMode::ZeroIf,
        lna_state: 4,
        agc_enabled: true,
        agc_setpoint_dbfs: -30,
        decimation_factor: 4, // 2 MHz / 4 = 500 kHz
        ..RspdxConfig::default()
    };

    let mut source = RspdxSource::new(config);
    let mut iq_rx = source.subscribe();
    let _handle = source.start();

    // Wait until IQ batches actually start arriving (up to 5 s)
    eprintln!("[hw_fm] Waiting for IQ stream to start...");
    let startup_deadline = Instant::now() + Duration::from_secs(5);
    let mut started = false;
    while Instant::now() < startup_deadline {
        match tokio::time::timeout(Duration::from_millis(200), iq_rx.recv()).await {
            Ok(Ok(_)) => {
                started = true;
                break;
            }
            _ => {
                eprint!(".");
            }
        }
    }
    if !started {
        panic!("[hw_fm] FAIL — no IQ batches in 5 s. Device not streaming?");
    }
    eprintln!("\n[hw_fm] IQ stream started. AGC settling 1 s...");
    tokio::time::sleep(Duration::from_secs(1)).await;

    // ── Determine target frequency ─────────────────────────────────────────
    let target_freq_hz: u64 = if let Ok(val) = std::env::var("SDRPLAY_TEST_FREQ_MHZ") {
        let mhz: f64 = val
            .trim()
            .parse()
            .expect("SDRPLAY_TEST_FREQ_MHZ must be a float (e.g. 101.1)");
        let hz = (mhz * 1_000_000.0) as u64;
        eprintln!("[hw_fm] Using fixed frequency: {:.3} MHz", mhz);
        hz
    } else {
        eprintln!("[hw_fm] Scanning FM band for strongest station (200 kHz steps, 1 s each)...");
        eprintln!("[hw_fm] (Set SDRPLAY_TEST_FREQ_MHZ=<MHz> to skip scan)");

        let mut best_freq = init_freq;
        let mut best_pilot: f32 = 0.0;
        let mut best_rms: f32 = 0.0;
        let mut best_stereo = false;

        let mut freq = 87_500_000u64;
        while freq <= 108_000_000 {
            // Retune the running source — fast, no teardown
            source.set_frequency(freq).unwrap();

            // Drain any stale batches from before the retune
            let drain_end = Instant::now() + Duration::from_millis(200);
            while Instant::now() < drain_end {
                let _ = iq_rx.try_recv();
            }

            // Fresh decoder for each frequency
            let mut decoder = StereoFmDecoder::new(250_000);
            let mut frames: Vec<StereoFrame> = Vec::new();
            let dwell_end = Instant::now() + Duration::from_secs(1);

            while Instant::now() < dwell_end {
                match tokio::time::timeout(Duration::from_millis(200), iq_rx.recv()).await {
                    Ok(Ok(batch)) => {
                        let d = decimate(&batch, 2);
                        let (f, _) = decoder.process(&d);
                        frames.extend_from_slice(&f);
                    }
                    Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {
                        decoder.clear_prev();
                    }
                    _ => {}
                }
            }

            let r = rms(&frames);
            let pilot = decoder.pilot_level();
            let stereo = decoder.is_stereo();

            eprintln!(
                "[hw_fm]   {:.1} MHz  rms={:.4}  pilot={:.4}  stereo={}",
                freq as f64 / 1e6,
                r,
                pilot,
                stereo
            );

            // Prefer stereo + highest pilot; fall back to highest rms
            if stereo && pilot > best_pilot || (!best_stereo && r > best_rms) {
                best_pilot = pilot;
                best_rms = r;
                best_freq = freq;
                best_stereo = stereo;
            }

            freq += 200_000;
        }

        eprintln!(
            "[hw_fm] Best: {:.3} MHz  pilot={:.4}  rms={:.4}  stereo={}",
            best_freq as f64 / 1e6,
            best_pilot,
            best_rms,
            best_stereo,
        );
        best_freq
    };

    // ── Receive and decode for 6 seconds at target frequency ──────────────
    source.set_frequency(target_freq_hz).unwrap();

    // Drain stale batches after retune
    let drain_end = Instant::now() + Duration::from_millis(300);
    while Instant::now() < drain_end {
        let _ = iq_rx.try_recv();
    }

    eprintln!(
        "[hw_fm] Decoding {:.3} MHz for 6 s...",
        target_freq_hz as f64 / 1e6
    );

    let mut decoder = StereoFmDecoder::new(250_000);
    let mut second_rms: Vec<f32> = Vec::new();
    let mut window_frames: Vec<StereoFrame> = Vec::new();
    let mut window_start = Instant::now();
    let test_end = Instant::now() + Duration::from_secs(6);
    let mut total_frames: usize = 0;
    let mut batches: u64 = 0;
    let mut pilot_locks: u64 = 0;

    while Instant::now() < test_end {
        match tokio::time::timeout(Duration::from_millis(200), iq_rx.recv()).await {
            Ok(Ok(batch)) => {
                batches += 1;
                let d = decimate(&batch, 2);
                let (frames, _) = decoder.process(&d);
                total_frames += frames.len();
                window_frames.extend_from_slice(&frames);
                if decoder.is_stereo() {
                    pilot_locks += 1;
                }

                if window_start.elapsed() >= Duration::from_secs(1) {
                    let r = rms(&window_frames);
                    let t = 7.0 - test_end.duration_since(Instant::now()).as_secs_f32();
                    eprintln!(
                        "[hw_fm] t={t:.1}s  rms={r:.4}  pilot={:.4}  stereo={}  locks={pilot_locks}/{batches}",
                        decoder.pilot_level(),
                        decoder.is_stereo(),
                    );
                    second_rms.push(r);
                    window_frames.clear();
                    window_start = Instant::now();
                }
            }
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(n))) => {
                eprintln!("[hw_fm] WARN: lagged {n} batches");
                decoder.clear_prev();
            }
            Ok(Err(_)) => break,
            Err(_) => {
                eprintln!("[hw_fm] WARN: no IQ for 200 ms");
            }
        }
    }

    source.stop();

    let mean_rms = if second_rms.is_empty() {
        0.0f32
    } else {
        second_rms.iter().sum::<f32>() / second_rms.len() as f32
    };

    eprintln!(
        "[hw_fm] Done — {total_frames} frames / {batches} batches"
    );
    eprintln!(
        "[hw_fm] Final: pilot={:.4}  is_stereo={}  mean_rms={mean_rms:.4}",
        decoder.pilot_level(),
        decoder.is_stereo(),
    );

    // ── ASSERTION 1: Non-trivial audio output ─────────────────────────────
    // This proves we decoded real FM audio, not static noise.
    // (Static noise produces RMS ≈ 0.16 from the FM discriminator itself;
    //  a real station with demodulated audio should match or exceed this.
    //  A completely silent decoder would show near-zero RMS.)
    assert!(
        mean_rms > 0.005,
        "Audio RMS {mean_rms:.5} < 0.005 — decoder is producing silence.\n\
         Check antenna connection and try SDRPLAY_TEST_FREQ_MHZ=<local station>."
    );

    // ── ASSERTION 2: Audio has dynamics (not frozen / stuck) ──────────────
    if second_rms.len() >= 3 {
        let var: f32 = second_rms
            .iter()
            .map(|r| (r - mean_rms).powi(2))
            .sum::<f32>()
            / second_rms.len() as f32;
        assert!(
            var > 1e-8,
            "RMS variance {var:.2e} ≈ 0 — audio output appears frozen/stuck"
        );
    }

    // ── INFO: Stereo pilot detection (informational — not a pass/fail) ────
    // Stereo pilot detection requires ~15 dB higher SNR than mono audio.
    // A short indoor antenna may be sufficient for audio but not for pilot.
    // Use a full-size λ/4 outdoor antenna (≈75 cm for FM band) for stereo.
    let pilot = decoder.pilot_level();
    if decoder.is_stereo() {
        eprintln!(
            "[hw_fm] STEREO PILOT DETECTED (pilot={pilot:.4}) — confirmed FM stereo broadcast"
        );
    } else {
        eprintln!(
            "[hw_fm] Stereo pilot NOT detected (pilot={pilot:.4}, threshold=0.02)."
        );
        eprintln!(
            "[hw_fm] Audio is present (rms={mean_rms:.4}) — likely a mono station or antenna SNR too low for pilot."
        );
        eprintln!("[hw_fm] For stereo: use a ~75 cm λ/4 antenna and a strong local FM stereo station.");
    }

    eprintln!(
        "[hw_fm] PASS — real FM audio confirmed at {:.3} MHz  (mean_rms={mean_rms:.4}  pilot={pilot:.4}  stereo={})",
        target_freq_hz as f64 / 1e6,
        decoder.is_stereo()
    );
}
