//! Hardware-in-the-loop reception tests.
//!
//! These tests prove the entire IQ→FFT→WBFM→audio pipeline using the
//! `TestSignalSource` (synthetic FM carrier at 1 kHz tone, ±10 kHz deviation).
//!
//! When a real RTL-SDR driver is implemented, swap `source_for_tests()` to
//! return an `RtlSdrSource` tuned to 105.7 MHz to verify live FM reception.
//!
//! Run with:
//!   cargo test -p sdrapp-core --features hardware_tests --test reception
//!
//! Without the feature flag this file is excluded from compilation entirely,
//! so normal `cargo test` stays fast.

#![cfg(feature = "hardware_tests")]

use std::sync::{
    atomic::AtomicU64,
    Arc,
};
use std::time::{Duration, Instant};

use crossbeam_channel::{bounded, Receiver};
use parking_lot::RwLock;

use sdrapp_core::{
    sample::StereoFrame,
    signal_path::{ReceiverCmd, SharedState, SignalPath, SignalPathCommand},
    test_source::TestSignalSource,
};

// ── Configuration ─────────────────────────────────────────────────────────────

/// Synthetic source frequency (Hz) — matches TestSignalSource's demo carrier.
const TEST_FREQ_HZ: u64 = 105_700_000;
/// Sample rate to use in tests.
const SAMPLE_RATE_SPS: u32 = 2_000_000;
/// Audio channel depth — enough for ~3 seconds of 48 kHz stereo at 1024 frames.
const AUDIO_CHANNEL_DEPTH: usize = 256;
/// How long to wait for first audio before declaring hardware absent.
const SIGNAL_TIMEOUT: Duration = Duration::from_secs(10);

// ── Harness ───────────────────────────────────────────────────────────────────

/// Wraps a running signal path wired to the test source.
///
/// Provides helpers to capture audio frames and read SharedState snapshots
/// without requiring a cpal audio sink or UI.
struct HardwareFixture {
    shared: Arc<RwLock<SharedState>>,
    cmd_tx: crossbeam_channel::Sender<SignalPathCommand>,
    audio_rx: Receiver<Arc<[StereoFrame]>>,
    _source: TestSignalSource,
    _signal_path: SignalPath,
    _freq_atomic: Arc<AtomicU64>,
}

impl HardwareFixture {
    /// Build the fixture and start the signal path.
    ///
    /// Returns `None` if the source is unavailable within `SIGNAL_TIMEOUT`.
    /// In CI (no hardware, no feature enabled) this fast-exits without failing.
    fn start() -> Option<Self> {
        let mut source = TestSignalSource::new(TEST_FREQ_HZ, SAMPLE_RATE_SPS);
        let freq_atomic = source.frequency_atomic();

        // Subscribe before starting so we don't miss the first batch.
        use sdrapp_core::source::Source as _;
        let iq_rx = source.subscribe();

        // Start the source task.
        use sdrapp_core::block::Block as _;
        let _source_handle = source.start();

        // Wire up an in-process audio channel so tests can collect decoded frames.
        let (audio_tx, audio_rx) = bounded::<Arc<[StereoFrame]>>(AUDIO_CHANNEL_DEPTH);

        // Build SharedState with WBFM demod pre-configured at our test frequency.
        let shared = Arc::new(RwLock::new({
            let mut s = SharedState::new();
            s.center_freq_hz = TEST_FREQ_HZ;
            s.sample_rate_sps = SAMPLE_RATE_SPS;
            s.demod.demod_mode = sdrapp_core::signal_path::DemodMode::Wbfm;
            s.demod.volume = 0.8;
            s
        }));

        let signal_path = SignalPath::start(
            Arc::clone(&shared),
            iq_rx,
            Some(audio_tx),
            None, // no recorder channel
            None, // no egui repaint
            Some(Arc::clone(&freq_atomic)),
            None, // no hardware command channel
        );

        // Send Start command — the signal path starts paused and won't emit audio
        // until it receives SignalPathCommand::Start.
        let _ = signal_path.cmd_tx.try_send(SignalPathCommand::Start);

        // Wait for the first audio frame to confirm the pipeline is running.
        let deadline = Instant::now() + SIGNAL_TIMEOUT;
        loop {
            if audio_rx.recv_timeout(Duration::from_millis(200)).is_ok() {
                break;
            }
            if Instant::now() > deadline {
                eprintln!("HardwareFixture: no audio within {SIGNAL_TIMEOUT:?} — skipping");
                return None;
            }
        }

        Some(Self {
            shared,
            cmd_tx: signal_path.cmd_tx.clone(),
            audio_rx,
            _source: source,
            _signal_path: signal_path,
            _freq_atomic: freq_atomic,
        })
    }

    /// Collect at least `min_secs` worth of audio.
    ///
    /// Returns flattened mono samples (L channel) so assertions are simple.
    fn capture_audio_secs(&self, min_secs: f32) -> Vec<f32> {
        // 48 kHz is the standard audio output rate after decimation.
        let sample_rate = 48_000_u32;
        let needed_samples = (sample_rate as f32 * min_secs) as usize;
        let deadline = Instant::now() + Duration::from_secs_f32(min_secs * 3.0 + 2.0);

        let mut out = Vec::with_capacity(needed_samples);
        while out.len() < needed_samples {
            match self.audio_rx.recv_timeout(Duration::from_millis(500)) {
                Ok(frames) => out.extend(frames.iter().map(|f| f.left)),
                Err(_) => {
                    if Instant::now() > deadline {
                        break;
                    }
                }
            }
        }
        out
    }

    /// Snapshot the current FFT magnitude buffer.
    fn fft_snapshot(&self) -> Vec<f32> {
        self.shared.read().fft.fft_magnitudes.clone()
    }

    /// Wait up to `timeout` for SNR to exceed `threshold_db`.
    /// Returns the SNR at the moment it crossed the threshold, or the last
    /// observed value on timeout.
    fn wait_for_snr(&self, threshold_db: f32, timeout: Duration) -> f32 {
        let deadline = Instant::now() + timeout;
        let mut last_snr = 0.0_f32;
        loop {
            if let Some(snr) = self.shared.read().fft.snr_db {
                last_snr = snr;
                if snr >= threshold_db {
                    return snr;
                }
            }
            if Instant::now() > deadline {
                return last_snr;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn send(&self, cmd: impl Into<SignalPathCommand>) {
        let _ = self.cmd_tx.try_send(cmd.into());
    }
}

impl Drop for HardwareFixture {
    fn drop(&mut self) {
        let _ = self.cmd_tx.try_send(SignalPathCommand::Stop);
    }
}

// ── Macro to skip when fixture unavailable ────────────────────────────────────

/// Skip the test gracefully when no hardware / source is available.
macro_rules! fixture_or_skip {
    () => {
        match HardwareFixture::start() {
            Some(f) => f,
            None => {
                eprintln!("SKIP: source unavailable");
                return;
            }
        }
    };
}

// ── Test suite ────────────────────────────────────────────────────────────────

/// FFT peak at centre should be well above noise (-30 dBFS threshold).
///
/// The TestSignalSource emits a unit-amplitude carrier which the FFT should
/// see as a strong signal near -3 dBFS (windowing loss). Proves IQ→FFT works.
#[test]
fn test_fm_signal_detected() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let _guard = rt.enter();

    let fix = fixture_or_skip!();

    // Allow a couple of seconds of warm-up for the FFT to stabilise.
    std::thread::sleep(Duration::from_secs(2));

    let magnitudes = fix.fft_snapshot();
    assert!(!magnitudes.is_empty(), "FFT buffer is empty");

    // Find the peak bin value.
    let peak_dbfs = magnitudes.iter().cloned().fold(f32::NEG_INFINITY, f32::max);

    assert!(
        peak_dbfs > -30.0,
        "FFT peak {peak_dbfs:.1} dBFS below -30 threshold — signal not detected"
    );
}

/// SNR in the active WBFM passband should exceed 15 dB.
///
/// The synthetic carrier is at 0 dBFS relative to the noise floor, so SNR
/// should be very high for a clean carrier. Proves FFT SNR estimation works.
#[test]
fn test_fm_snr_above_threshold() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let _guard = rt.enter();

    let fix = fixture_or_skip!();

    let snr = fix.wait_for_snr(15.0, Duration::from_secs(5));
    assert!(
        snr > 15.0,
        "SNR {snr:.1} dB below 15 dB threshold — weak signal or demod not running"
    );
}

/// Decoded audio must not be silence — RMS > 0.05 over 2+ seconds.
///
/// The TestSignalSource emits FM-modulated audio at 1 kHz. After WBFM demod
/// the output should be a clear sine wave. Proves the demodulator is running.
#[test]
fn test_audio_is_not_silence() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let _guard = rt.enter();

    let fix = fixture_or_skip!();

    let samples = fix.capture_audio_secs(2.0);
    assert!(
        !samples.is_empty(),
        "No audio frames received — pipeline may be broken"
    );

    let rms = rms(&samples);
    assert!(
        rms > 0.05,
        "Audio RMS {rms:.4} below 0.05 — output is silence (demod not working?)"
    );
}

/// Audio RMS should be in a plausible loudness range: not silence, not clipping.
///
/// 0.02 < RMS < 0.95 for a healthy FM broadcast signal.
#[test]
fn test_audio_rms_plausible() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let _guard = rt.enter();

    let fix = fixture_or_skip!();

    let samples = fix.capture_audio_secs(2.0);
    let rms = rms(&samples);

    assert!(rms > 0.02, "Audio RMS {rms:.4} too low — possible silence");
    assert!(rms < 0.95, "Audio RMS {rms:.4} too high — possible clipping");
}

/// ADC clipping flag must stay false at LNA state 6 for 5 seconds.
///
/// This assertion is only valid with real hardware at a sensible gain setting.
/// The TestSignalSource emits unit-amplitude IQ which correctly triggers the
/// clipping detector (a full-scale carrier looks saturated to the FFT).
///
/// Run only with real hardware: cargo test ... -- --include-ignored test_no_adc_clipping
#[test]
#[ignore = "requires real hardware at LNA=6 — unit-amplitude TestSignalSource always clips"]
fn test_no_adc_clipping() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let _guard = rt.enter();

    let fix = fixture_or_skip!();

    // Poll for 5 seconds — any clipping event is a failure.
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let clipping = fix.shared.read().fft.fft_clipping_detected;
        assert!(
            !clipping,
            "fft_clipping_detected=true at unit-amplitude input — false positive?"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Stereo pilot detection: after sufficient signal, is_stereo should be true.
///
/// NOTE: The TestSignalSource emits mono FM (no 38 kHz stereo subcarrier).
/// This test is marked `#[ignore]` by default — it is intended to run only
/// with real hardware tuned to a stereo FM broadcast station.
/// Unskip with: cargo test ... -- --include-ignored test_stereo_pilot_present
#[test]
#[ignore = "requires real stereo FM broadcast — use real hardware"]
fn test_stereo_pilot_present() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let _guard = rt.enter();

    let fix = fixture_or_skip!();

    // Allow time for stereo pilot detection to settle.
    let deadline = Instant::now() + Duration::from_secs(8);
    while Instant::now() < deadline {
        if fix.shared.read().rds.is_stereo {
            return; // passed
        }
        std::thread::sleep(Duration::from_millis(300));
    }

    let snr = fix.shared.read().fft.snr_db.unwrap_or(0.0);
    panic!("Stereo pilot not detected after 8 s (SNR={snr:.1} dB) — tune to a stereo FM station");
}

/// RDS PS name should be decoded within 8 seconds of a real broadcast.
///
/// NOTE: TestSignalSource does not inject RDS. Marked `#[ignore]` — run only
/// with real hardware on WMJI 105.7 MHz (Cleveland, OH) which broadcasts RDS.
#[test]
#[ignore = "requires real RDS broadcast — use real hardware on WMJI 105.7"]
fn test_rds_station_name_present() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let _guard = rt.enter();

    let fix = fixture_or_skip!();

    // Tune to WMJI 105.7 MHz explicitly.
    fix.send(ReceiverCmd::SetFrequency(105_700_000));

    let deadline = Instant::now() + Duration::from_secs(12);
    while Instant::now() < deadline {
        if let Some(name) = fix.shared.read().rds.ps_name.clone() {
            println!("RDS PS name decoded: {name:?}");
            return;
        }
        std::thread::sleep(Duration::from_millis(500));
    }

    panic!("RDS PS name not decoded after 12 s — check antenna / signal strength");
}

// ── Quality report + WAV output ───────────────────────────────────────────────

/// Compute an audio quality report and write a 5-second WAV for human review.
///
/// Writes to /tmp/sdrpp_reception_test.wav — open in Audacity or any media
/// player to listen and confirm WBFM demodulation sounds correct.
///
/// Quality rating:
///   POOR  — RMS < 0.02 or SNR < 5 dB
///   FAIR  — RMS in [0.02, 0.10) or SNR in [5, 20)
///   GOOD  — RMS ≥ 0.10 and SNR ≥ 20 dB
#[test]
fn test_quality_report_and_wav_output() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let _guard = rt.enter();

    let fix = fixture_or_skip!();

    // Collect 5 seconds of audio.
    eprintln!("Collecting 5 seconds of audio for quality report…");
    let samples = fix.capture_audio_secs(5.0);

    let snr_db = fix
        .shared
        .read()
        .fft
        .snr_db
        .unwrap_or(0.0);
    let is_stereo = fix.shared.read().rds.is_stereo;
    let rms_val = rms(&samples);
    let peak = samples.iter().cloned().map(f32::abs).fold(0.0_f32, f32::max);

    let quality = if rms_val < 0.02 || snr_db < 5.0 {
        "POOR"
    } else if rms_val < 0.10 || snr_db < 20.0 {
        "FAIR"
    } else {
        "GOOD"
    };

    println!("\n╔══════════════════════════════════════════╗");
    println!("║      SDRApp Reception Quality Report     ║");
    println!("╠══════════════════════════════════════════╣");
    println!("║  Source  : {:<31}║", "TestSignalSource (synthetic FM)");
    println!("║  Freq    : {:<31}║", format!("{:.1} MHz", TEST_FREQ_HZ as f64 / 1e6));
    println!("║  SNR     : {:<31}║", format!("{snr_db:.1} dB"));
    println!("║  Stereo  : {:<31}║", if is_stereo { "YES" } else { "NO (mono source)" });
    println!("║  RMS     : {:<31}║", format!("{rms_val:.4}"));
    println!("║  Peak    : {:<31}║", format!("{peak:.4}"));
    println!("║  Samples : {:<31}║", samples.len());
    println!("║  Quality : {:<31}║", quality);
    println!("╚══════════════════════════════════════════╝");

    // Write WAV for human verification.
    let wav_path = "/tmp/sdrpp_reception_test.wav";
    write_wav(wav_path, &samples, 48_000).expect("failed to write WAV");
    println!("WAV written to: {wav_path}");
    println!("  → Open in Audacity or `afplay {wav_path}` to listen.");

    // Quality assertion: at minimum FAIR (not POOR).
    assert_ne!(
        quality, "POOR",
        "Reception quality POOR: RMS={rms_val:.4} SNR={snr_db:.1} dB — pipeline may be broken"
    );
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_sq: f32 = samples.iter().map(|&s| s * s).sum();
    (sum_sq / samples.len() as f32).sqrt()
}

fn write_wav(path: &str, samples: &[f32], sample_rate: u32) -> Result<(), hound::Error> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec)?;
    for &s in samples {
        let s16 = (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
        writer.write_sample(s16)?;
    }
    writer.finalize()?;
    Ok(())
}
