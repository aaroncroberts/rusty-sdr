//! Hardware integration test: AM broadcast reception (Port C, ML-31 antenna).
//!
//! **Requires SDRplay hardware, ML-31 magnetic loop on Port C with Bias-T power.**
//! Gated behind `SDRPLAY_HW_TEST` so CI never runs it.
//!
//! # How to run
//! ```sh
//! SDRPLAY_HW_TEST=1 cargo test \
//!     --package sdrapp-sdrplay \
//!     --test hw_am_broadcast \
//!     -- --nocapture
//!
//! # Tune to a specific AM station (kHz) to skip the scan:
//! SDRPLAY_HW_TEST=1 SDRPLAY_TEST_FREQ_KHZ=1010 cargo test \
//!     --package sdrapp-sdrplay \
//!     --test hw_am_broadcast \
//!     -- --nocapture
//! ```

use std::time::{Duration, Instant};

use sdrapp_core::{
    block::Block,
    dsp::AmDemodulator,
    source::Source,
};
use sdrapp_sdrplay::{Antenna, IfMode, RspdxConfig, RspdxSource};

/// RMS amplitude of audio samples.
fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() { return 0.0; }
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

/// Scan the AM broadcast band (530–1700 kHz) on Port C with the ML-31 loop antenna,
/// find the strongest station, and assert non-trivial AM audio.
///
/// Port C is the RSPdx-R2's HF/AM port — designed for < 30 MHz with external antennas.
/// Bias-T is enabled to power the ML-31 active preamp.
#[tokio::test]
async fn hw_am_broadcast_audio_present() {
    if std::env::var("SDRPLAY_HW_TEST").is_err() {
        eprintln!("[hw_am] SKIP — set SDRPLAY_HW_TEST=1 to enable");
        return;
    }
    if !RspdxSource::is_device_available() {
        eprintln!("[hw_am] SKIP — no SDRplay device found");
        return;
    }

    eprintln!("[hw_am] SDRplay hardware detected — starting AM broadcast test");

    // ── Open device at 1000 kHz on Port C with Bias-T ────────────────────────
    // LowIF mode works better for HF/AM (ZeroIF can have DC offset issues at HF).
    let init_freq = 1_000_000u64; // 1000 kHz — middle of AM band
    let config = RspdxConfig {
        frequency_hz: init_freq,
        sample_rate_sps: 2_000_000,
        antenna: Antenna::C,
        if_mode: IfMode::ZeroIf,
        lna_state: 1,      // Low attenuation — ML-31 has gain
        agc_enabled: true,
        agc_setpoint_dbfs: -30,
        bias_t_enabled: true,  // Power the ML-31 preamp
        decimation_factor: 4,  // 500 kHz effective
        ..RspdxConfig::default()
    };

    let mut source = RspdxSource::new(config);
    let mut iq_rx = source.subscribe();
    let _handle = source.start();

    // Wait for stream to start (up to 5 s)
    eprintln!("[hw_am] Waiting for IQ stream...");
    let startup_deadline = Instant::now() + Duration::from_secs(5);
    let mut started = false;
    while Instant::now() < startup_deadline {
        match tokio::time::timeout(Duration::from_millis(200), iq_rx.recv()).await {
            Ok(Ok(_)) => { started = true; break; }
            _ => { eprint!("."); }
        }
    }
    if !started {
        panic!("[hw_am] FAIL — no IQ in 5 s. Check ML-31 connection and Bias-T.");
    }
    eprintln!("\n[hw_am] IQ stream started. AGC settling 1 s...");
    tokio::time::sleep(Duration::from_secs(1)).await;

    // ── Determine target frequency ────────────────────────────────────────────
    let target_hz: u64 = if let Ok(val) = std::env::var("SDRPLAY_TEST_FREQ_KHZ") {
        let khz: f64 = val.trim().parse().expect("SDRPLAY_TEST_FREQ_KHZ must be a number");
        let hz = (khz * 1_000.0) as u64;
        eprintln!("[hw_am] Fixed frequency: {:.0} kHz", khz);
        hz
    } else {
        eprintln!("[hw_am] Scanning AM broadcast band (530–1700 kHz, 10 kHz steps, 0.5 s each)...");
        let mut best_freq = init_freq;
        let mut best_rms: f32 = 0.0;

        let mut freq = 530_000u64;
        while freq <= 1_700_000 {
            source.set_frequency(freq).unwrap();

            // Drain stale batches
            let drain = Instant::now() + Duration::from_millis(100);
            while Instant::now() < drain { let _ = iq_rx.try_recv(); }

            // AM demodulator: 250 kHz → 48 kHz
            let mut demod = AmDemodulator::new(250_000, 48_000);
            let mut audio: Vec<f32> = Vec::new();
            let dwell = Instant::now() + Duration::from_millis(500);

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
            if r > 0.01 {
                eprintln!("[hw_am]   {:.0} kHz  rms={:.4}  *** signal ***", freq as f64 / 1e3, r);
            }
            if r > best_rms { best_rms = r; best_freq = freq; }

            freq += 10_000;
        }

        eprintln!("[hw_am] Best: {:.0} kHz  rms={:.4}", best_freq as f64 / 1e3, best_rms);
        best_freq
    };

    // ── 6-second reception at target frequency ────────────────────────────────
    source.set_frequency(target_hz).unwrap();
    let drain = Instant::now() + Duration::from_millis(300);
    while Instant::now() < drain { let _ = iq_rx.try_recv(); }

    eprintln!("[hw_am] Decoding {:.0} kHz for 6 s...", target_hz as f64 / 1e3);

    let mut demod = AmDemodulator::new(250_000, 48_000);
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
                    eprintln!("[hw_am] t={t:.1}s  rms={r:.4}  batches={batches}");
                    second_rms.push(r);
                    window.clear();
                    window_start = Instant::now();
                }
            }
            Ok(Err(_)) => break,
            Err(_) => eprintln!("[hw_am] WARN: no IQ for 200 ms"),
        }
    }

    source.stop();

    let mean_rms = if second_rms.is_empty() { 0.0f32 }
                   else { second_rms.iter().sum::<f32>() / second_rms.len() as f32 };

    eprintln!("[hw_am] Done — batches={batches}  mean_rms={mean_rms:.4}");

    // AM broadcast signals through a loop antenna indoors are much weaker than FM/NOAA.
    // A local AM station should exceed 0.002 RMS; pure noise floor is ~0.0005.
    assert!(
        mean_rms > 0.002,
        "Audio RMS {mean_rms:.5} < 0.002 — AM decoder producing silence.\n\
         Check: ML-31 on Port C, Bias-T enabled, Antenna C selected.\n\
         Hint: set SDRPLAY_TEST_FREQ_KHZ=<local AM station> to target a known station."
    );

    eprintln!(
        "[hw_am] PASS — AM audio confirmed at {:.0} kHz  (mean_rms={mean_rms:.4})",
        target_hz as f64 / 1e3
    );
}
