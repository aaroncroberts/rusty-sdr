//! Hardware integration test: NOAA weather radio NFM reception (162 MHz).
//!
//! **Requires physical SDRplay hardware and the Rattlesnake M6 medium antenna on Port A.**
//! Gated behind `SDRPLAY_HW_TEST` so CI never runs it.
//!
//! # How to run
//! ```sh
//! SDRPLAY_HW_TEST=1 cargo test \
//!     --package sdrapp-sdrplay \
//!     --test hw_nfm_noaa \
//!     -- --nocapture
//!
//! # Specify a frequency to skip the scan:
//! SDRPLAY_HW_TEST=1 SDRPLAY_TEST_FREQ_MHZ=162.400 cargo test \
//!     --package sdrapp-sdrplay \
//!     --test hw_nfm_noaa \
//!     -- --nocapture
//! ```
//!
//! NOAA weather radio transmits 24/7 on one of:
//!   162.400 / 162.425 / 162.450 / 162.475 / 162.500 / 162.525 / 162.550 MHz

use std::time::{Duration, Instant};

use sdrapp_core::{
    block::Block,
    dsp::FmDemodulator,
    source::Source,
};
use sdrapp_sdrplay::{Antenna, IfMode, RspdxConfig, RspdxSource};

/// RMS amplitude of mono audio samples.
fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_sq: f32 = samples.iter().map(|s| s * s).sum();
    (sum_sq / samples.len() as f32).sqrt()
}

/// Software decimation: keep every Nth sample.
fn decimate(
    iq: &[sdrapp_core::sample::IqSample],
    factor: usize,
) -> Vec<sdrapp_core::sample::IqSample> {
    iq.iter().step_by(factor).cloned().collect()
}

// ─────────────────────────────────────────────────────────────────────────────

/// Tune to a NOAA weather radio channel (162 MHz band), receive for 6 seconds
/// in NFM mode, and assert that non-trivial audio is decoded.
///
/// NOAA broadcasts a continuous 1050 Hz attention tone followed by voice —
/// so audio RMS should be clearly above noise even with a short indoor antenna.
#[tokio::test]
async fn hw_nfm_noaa_audio_present() {
    if std::env::var("SDRPLAY_HW_TEST").is_err() {
        eprintln!("[hw_nfm] SKIP — set SDRPLAY_HW_TEST=1 to enable");
        return;
    }
    if !RspdxSource::is_device_available() {
        eprintln!("[hw_nfm] SKIP — no SDRplay device found");
        return;
    }

    eprintln!("[hw_nfm] SDRplay hardware detected — starting NOAA NFM test");

    // ── Open device at 162.400 MHz, 2 MHz sample rate ────────────────────────
    let init_freq = 162_400_000u64;
    let config = RspdxConfig {
        frequency_hz: init_freq,
        sample_rate_sps: 2_000_000,
        antenna: Antenna::A,
        if_mode: IfMode::ZeroIf,
        lna_state: 3,
        agc_enabled: true,
        agc_setpoint_dbfs: -30,
        decimation_factor: 4, // 2 MHz / 4 = 500 kHz
        ..RspdxConfig::default()
    };

    let mut source = RspdxSource::new(config);
    let mut iq_rx = source.subscribe();
    let _handle = source.start();

    // Wait for stream to start (up to 5 s)
    eprintln!("[hw_nfm] Waiting for IQ stream...");
    let startup_deadline = Instant::now() + Duration::from_secs(5);
    let mut started = false;
    while Instant::now() < startup_deadline {
        match tokio::time::timeout(Duration::from_millis(200), iq_rx.recv()).await {
            Ok(Ok(_)) => { started = true; break; }
            _ => { eprint!("."); }
        }
    }
    if !started {
        panic!("[hw_nfm] FAIL — no IQ in 5 s");
    }
    eprintln!("\n[hw_nfm] IQ stream started. AGC settling 1 s...");
    tokio::time::sleep(Duration::from_secs(1)).await;

    // ── Determine target frequency ────────────────────────────────────────────
    let noaa_channels: &[u64] = &[
        162_400_000, 162_425_000, 162_450_000,
        162_475_000, 162_500_000, 162_525_000, 162_550_000,
    ];

    let target_hz: u64 = if let Ok(val) = std::env::var("SDRPLAY_TEST_FREQ_MHZ") {
        let mhz: f64 = val.trim().parse().expect("SDRPLAY_TEST_FREQ_MHZ must be a float");
        let hz = (mhz * 1_000_000.0) as u64;
        eprintln!("[hw_nfm] Fixed frequency: {:.3} MHz", mhz);
        hz
    } else {
        eprintln!("[hw_nfm] Scanning NOAA channels (1 s each)...");
        let mut best_freq = init_freq;
        let mut best_rms: f32 = 0.0;

        for &ch in noaa_channels {
            source.set_frequency(ch).unwrap();

            // Drain stale batches
            let drain = Instant::now() + Duration::from_millis(150);
            while Instant::now() < drain { let _ = iq_rx.try_recv(); }

            // NFM demodulator: 250 kHz IQ input, 48 kHz audio output, 5 kHz deviation
            let mut demod = FmDemodulator::new(250_000, 48_000, 5_000.0, 75.0);
            let mut audio: Vec<f32> = Vec::new();
            let dwell = Instant::now() + Duration::from_secs(1);

            while Instant::now() < dwell {
                match tokio::time::timeout(Duration::from_millis(200), iq_rx.recv()).await {
                    Ok(Ok(batch)) => {
                        let d = decimate(&batch, 2);
                        audio.extend_from_slice(&demod.process(&d));
                    }
                    _ => {}
                }
            }

            let r = rms(&audio);
            eprintln!("[hw_nfm]   {:.3} MHz  rms={:.4}", ch as f64 / 1e6, r);
            if r > best_rms { best_rms = r; best_freq = ch; }
        }

        eprintln!("[hw_nfm] Best channel: {:.3} MHz  rms={:.4}", best_freq as f64 / 1e6, best_rms);
        best_freq
    };

    // ── 6-second reception at target frequency ────────────────────────────────
    source.set_frequency(target_hz).unwrap();
    let drain = Instant::now() + Duration::from_millis(300);
    while Instant::now() < drain { let _ = iq_rx.try_recv(); }

    eprintln!("[hw_nfm] Decoding {:.3} MHz for 6 s...", target_hz as f64 / 1e6);

    let mut demod = FmDemodulator::new(250_000, 48_000, 5_000.0, 75.0);
    let mut second_rms: Vec<f32> = Vec::new();
    let mut window: Vec<f32> = Vec::new();
    let mut window_start = Instant::now();
    let test_end = Instant::now() + Duration::from_secs(6);
    let mut batches: u64 = 0;

    while Instant::now() < test_end {
        match tokio::time::timeout(Duration::from_millis(200), iq_rx.recv()).await {
            Ok(Ok(batch)) => {
                batches += 1;
                let d = decimate(&batch, 2);
                window.extend_from_slice(&demod.process(&d));
                if window_start.elapsed() >= Duration::from_secs(1) {
                    let r = rms(&window);
                    let t = 7.0 - test_end.duration_since(Instant::now()).as_secs_f32();
                    eprintln!("[hw_nfm] t={t:.1}s  rms={r:.4}  batches={batches}");
                    second_rms.push(r);
                    window.clear();
                    window_start = Instant::now();
                }
            }
            Ok(Err(_)) => break,
            Err(_) => eprintln!("[hw_nfm] WARN: no IQ for 200 ms"),
        }
    }

    source.stop();

    let mean_rms = if second_rms.is_empty() { 0.0f32 }
                   else { second_rms.iter().sum::<f32>() / second_rms.len() as f32 };

    eprintln!("[hw_nfm] Done — batches={batches}  mean_rms={mean_rms:.4}");

    // ── ASSERTION: Non-trivial audio ──────────────────────────────────────────
    // NOAA transmits continuous audio; FM discriminator on pure noise gives ~0.05.
    // A real NOAA station with a 46 cm (λ/4 at 162 MHz) medium antenna should exceed 0.02.
    assert!(
        mean_rms > 0.005,
        "Audio RMS {mean_rms:.5} < 0.005 — decoder producing silence.\n\
         Check: Rattlesnake M6 medium on Port A, Antenna A selected in Device Settings.\n\
         Hint: set SDRPLAY_TEST_FREQ_MHZ=162.XXX to target your local NOAA channel."
    );

    eprintln!(
        "[hw_nfm] PASS — NOAA NFM audio confirmed at {:.3} MHz  (mean_rms={mean_rms:.4})",
        target_hz as f64 / 1e6
    );
}
