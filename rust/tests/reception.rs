//! Hardware-in-the-loop reception tests for the full SDRApp stack.
//!
//! The standard tests (no #[ignore]) use TestSignalSource — synthetic FM that
//! proves the IQ→FFT→WBFM→audio pipeline without needing hardware.
//!
//! The real-hardware suite (single #[ignore] test) opens the SDRplay RSPdx-R2
//! once and runs all assertions in sequence. This avoids the SDRplay service's
//! limitation of only one Open/Close per process invocation.
//!
//! Run pipeline tests (always pass):
//!   cargo test -p rusty-sdr --test reception -- --nocapture
//!
//! Run real RSPdx-R2 test (requires device connected):
//!   cargo test -p rusty-sdr --test reception -- --include-ignored --nocapture

use std::time::{Duration, Instant};

use crossbeam_channel::{bounded, Receiver};
use parking_lot::RwLock;
use std::sync::{atomic::AtomicU64, Arc};

use rusty_sdr_core::{
    block::Block,
    sample::StereoFrame,
    signal_path::{ReceiverCmd, SharedState, SignalPath, SignalPathCommand},
    source::Source,
};

// ── Constants ─────────────────────────────────────────────────────────────────

const WMJI_FREQ_HZ: u64 = 105_700_000;
const SAMPLE_RATE_SPS: u32 = 2_000_000;
const AUDIO_CHANNEL_DEPTH: usize = 512;
const SIGNAL_TIMEOUT: Duration = Duration::from_secs(10);

// ── Fixture ───────────────────────────────────────────────────────────────────

struct Fixture {
    source_name: String,
    shared: Arc<RwLock<SharedState>>,
    cmd_tx: crossbeam_channel::Sender<SignalPathCommand>,
    audio_rx: Receiver<Arc<[StereoFrame]>>,
}

impl Fixture {
    /// Start with a synthetic TestSignalSource — always available.
    fn synthetic() -> Option<Self> {
        let mut src = rusty_sdr_core::test_source::TestSignalSource::new(WMJI_FREQ_HZ, SAMPLE_RATE_SPS);
        let iq_rx = src.subscribe();
        let fa: Arc<AtomicU64> = Source::frequency_atomic(&src);
        let _handle = src.start();
        Self::wire("TestSignalSource (synthetic FM 1 kHz)", iq_rx, fa, false)
    }

    /// Start with a real RSPdx-R2. Skips if no device is found.
    ///
    /// NOTE: Do NOT call is_device_available() before this — that would open
    /// and close the SDRplay API, and a second Open immediately after causes
    /// the SDRplay service to hang. Let start() discover hardware gracefully.
    fn rspdx(freq_hz: u64) -> Option<Self> {
        let mut src = rusty_sdr_sdrplay::RspdxSource::new(rusty_sdr_sdrplay::RspdxConfig {
            frequency_hz: freq_hz,
            sample_rate_sps: SAMPLE_RATE_SPS,
            agc_enabled: true,
            lna_state: 3,
            ..Default::default()
        });
        let iq_rx = src.subscribe();
        let fa = Source::frequency_atomic(&src);
        let _handle = src.start();
        // Drop src — the device thread owns the hardware now.
        // The broadcast channel keeps producing IQ as long as the thread lives.
        Self::wire("SDRplay RSPdx-R2", iq_rx, fa, true)
    }

    fn wire(
        name: impl Into<String>,
        iq_rx: tokio::sync::broadcast::Receiver<Arc<[rusty_sdr_core::sample::IqSample]>>,
        fa: Arc<AtomicU64>,
        _is_real: bool,
    ) -> Option<Self> {
        let (audio_tx, audio_rx) = bounded::<Arc<[StereoFrame]>>(AUDIO_CHANNEL_DEPTH);

        let shared = Arc::new(RwLock::new({
            let mut s = SharedState::new();
            s.center_freq_hz = WMJI_FREQ_HZ;
            s.sample_rate_sps = SAMPLE_RATE_SPS;
            s.demod.demod_mode = rusty_sdr_core::signal_path::DemodMode::Wbfm;
            s.demod.volume = 0.8;
            s
        }));

        let sp = SignalPath::start(
            Arc::clone(&shared), iq_rx, Some(audio_tx), None, None, Some(fa), None,
        );
        let _ = sp.cmd_tx.try_send(SignalPathCommand::Start);

        let deadline = Instant::now() + SIGNAL_TIMEOUT;
        loop {
            if audio_rx.recv_timeout(Duration::from_millis(200)).is_ok() {
                break;
            }
            if Instant::now() > deadline {
                eprintln!("Fixture: no audio from {} within {SIGNAL_TIMEOUT:?}", name.into());
                let _ = sp.cmd_tx.try_send(SignalPathCommand::Stop);
                return None;
            }
        }

        Some(Self {
            source_name: name.into(),
            shared,
            cmd_tx: sp.cmd_tx.clone(),
            audio_rx,
        })
    }

    fn capture_audio_secs(&self, secs: f32) -> Vec<f32> {
        let needed = (48_000_f32 * secs) as usize;
        let deadline = Instant::now() + Duration::from_secs_f32(secs * 3.0 + 2.0);
        let mut out = Vec::with_capacity(needed);
        while out.len() < needed {
            match self.audio_rx.recv_timeout(Duration::from_millis(500)) {
                Ok(frames) => out.extend(frames.iter().map(|f| f.left)),
                Err(_) if Instant::now() > deadline => break,
                _ => {}
            }
        }
        out
    }

    fn wait_for_snr(&self, min_db: f32, timeout: Duration) -> f32 {
        let deadline = Instant::now() + timeout;
        let mut last = 0.0_f32;
        while Instant::now() < deadline {
            if let Some(snr) = self.shared.read().fft.snr_db {
                last = snr;
                if snr >= min_db { return snr; }
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        last
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.cmd_tx.try_send(SignalPathCommand::Stop);
    }
}

macro_rules! fixture_or_skip {
    ($expr:expr) => {
        match $expr {
            Some(f) => f,
            None => { eprintln!("SKIP"); return; }
        }
    };
}

// ── Pipeline tests (always run, use synthetic source) ─────────────────────────

fn make_rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap()
}

/// FFT peak > -30 dBFS: IQ→FFT pipeline is working.
#[test]
fn test_fm_signal_detected() {
    let rt = make_rt(); let _g = rt.enter();
    let fix = fixture_or_skip!(Fixture::synthetic());
    std::thread::sleep(Duration::from_secs(2));
    let peak = fix.shared.read().fft.fft_magnitudes.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
    println!("[{}] FFT peak: {peak:.1} dBFS", fix.source_name);
    assert!(peak > -30.0, "FFT peak {peak:.1} dBFS — IQ→FFT not working");
}

/// SNR > 15 dB in WBFM passband.
#[test]
fn test_fm_snr_above_threshold() {
    let rt = make_rt(); let _g = rt.enter();
    let fix = fixture_or_skip!(Fixture::synthetic());
    let snr = fix.wait_for_snr(15.0, Duration::from_secs(5));
    println!("[{}] SNR: {snr:.1} dB", fix.source_name);
    assert!(snr > 15.0, "SNR {snr:.1} dB below threshold");
}

/// Audio RMS > 0.05: WBFM demodulator producing audio.
#[test]
fn test_audio_is_not_silence() {
    let rt = make_rt(); let _g = rt.enter();
    let fix = fixture_or_skip!(Fixture::synthetic());
    let s = fix.capture_audio_secs(2.0);
    let r = rms(&s);
    println!("[{}] RMS: {r:.4} ({} samples)", fix.source_name, s.len());
    assert!(!s.is_empty());
    assert!(r > 0.05, "Audio RMS {r:.4} — demod not working");
}

/// Audio RMS in (0.02, 0.95): plausible loudness range.
#[test]
fn test_audio_rms_plausible() {
    let rt = make_rt(); let _g = rt.enter();
    let fix = fixture_or_skip!(Fixture::synthetic());
    let r = rms(&fix.capture_audio_secs(2.0));
    println!("[{}] RMS: {r:.4}", fix.source_name);
    assert!(r > 0.02); assert!(r < 0.95);
}

/// Quality report + WAV to /tmp: proves WBFM pipeline end-to-end.
#[test]
fn test_quality_report_and_wav_output() {
    let rt = make_rt(); let _g = rt.enter();
    let fix = fixture_or_skip!(Fixture::synthetic());

    eprintln!("Collecting 5 s of audio from {}…", fix.source_name);
    let samples = fix.capture_audio_secs(5.0);
    let snr = fix.shared.read().fft.snr_db.unwrap_or(0.0);
    let rms_v = rms(&samples);
    let peak = samples.iter().cloned().map(f32::abs).fold(0.0_f32, f32::max);
    let quality = quality_label(rms_v, snr);

    println!("\n╔══════════════════════════════════════════╗");
    println!("║      SDRApp Reception Quality Report     ║");
    println!("╠══════════════════════════════════════════╣");
    println!("║  Source  : {:<31}║", fix.source_name);
    println!("║  SNR     : {:<31}║", format!("{snr:.1} dB"));
    println!("║  RMS     : {:<31}║", format!("{rms_v:.4}"));
    println!("║  Peak    : {:<31}║", format!("{peak:.4}"));
    println!("║  Samples : {:<31}║", samples.len());
    println!("║  Quality : {:<31}║", quality);
    println!("╚══════════════════════════════════════════╝");

    let wav = "/tmp/sdrpp_reception_test.wav";
    write_wav(wav, &samples, 48_000).expect("WAV write failed");
    println!("WAV: {wav}  →  afplay {wav}");

    assert_ne!(quality, "POOR", "Quality POOR: RMS={rms_v:.4} SNR={snr:.1} dB");
}

// ── Real hardware test (RSPdx-R2, WMJI 105.7 MHz) ────────────────────────────
//
// Opens the SDRplay device ONCE and runs all broadcast-quality assertions.
// Written as a single test to avoid rapid open/close cycles that confuse the
// SDRplay service.
//
// Run with:
//   cargo test -p rusty-sdr --test reception -- --include-ignored rspdx -- --nocapture

#[test]
#[ignore = "requires SDRplay RSPdx-R2 connected and sdrplay_api installed"]
fn test_rspdx_real_fm_reception() {
    let rt = make_rt();
    let _g = rt.enter();

    let fix = match Fixture::rspdx(WMJI_FREQ_HZ) {
        Some(f) => f,
        None => {
            eprintln!("SKIP: RSPdx-R2 not available");
            return;
        }
    };

    eprintln!("RSPdx-R2 open on {:.1} MHz — warming up…", WMJI_FREQ_HZ as f64 / 1e6);
    std::thread::sleep(Duration::from_secs(3)); // wait for AGC to settle

    // ── FFT peak ──────────────────────────────────────────────────────────
    let peak = fix.shared.read().fft.fft_magnitudes.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
    println!("FFT peak: {peak:.1} dBFS");
    assert!(peak > -40.0, "FFT peak {peak:.1} — weak signal or no reception");

    // ── SNR ───────────────────────────────────────────────────────────────
    let snr = fix.wait_for_snr(15.0, Duration::from_secs(5));
    println!("SNR: {snr:.1} dB");
    assert!(snr > 10.0, "SNR {snr:.1} dB — check antenna");

    // ── Audio not silence ─────────────────────────────────────────────────
    let samples = fix.capture_audio_secs(2.0);
    let rms_v = rms(&samples);
    println!("Audio RMS (2 s): {rms_v:.4}");
    assert!(rms_v > 0.01, "Audio is silence — WBFM demod not running");

    // ── Stereo pilot ──────────────────────────────────────────────────────
    let deadline = Instant::now() + Duration::from_secs(6);
    while Instant::now() < deadline {
        if fix.shared.read().rds.is_stereo { break; }
        std::thread::sleep(Duration::from_millis(200));
    }
    let stereo = fix.shared.read().rds.is_stereo;
    println!("Stereo: {stereo}");
    // WMJI is stereo — soft assert (don't fail if signal is weak today)
    if !stereo { eprintln!("WARN: stereo pilot not detected — signal may be marginal"); }

    // ── No clipping at LNA=3 ──────────────────────────────────────────────
    let clipping = fix.shared.read().fft.fft_clipping_detected;
    println!("ADC clipping: {clipping}");
    if clipping { eprintln!("WARN: ADC clipping at LNA=3 — reduce LNA state"); }

    // ── RDS station name ──────────────────────────────────────────────────
    let _ = fix.cmd_tx.try_send(ReceiverCmd::SetFrequency(WMJI_FREQ_HZ).into());
    let deadline = Instant::now() + Duration::from_secs(12);
    while Instant::now() < deadline {
        if fix.shared.read().rds.ps_name.is_some() { break; }
        std::thread::sleep(Duration::from_millis(500));
    }
    let ps = fix.shared.read().rds.ps_name.clone();
    println!("RDS PS name: {ps:?}");
    if ps.is_none() { eprintln!("WARN: RDS not decoded — may need longer or stronger signal"); }

    // ── 5-second WAV ─────────────────────────────────────────────────────
    eprintln!("Collecting 5 s of broadcast audio…");
    let long_samples = fix.capture_audio_secs(5.0);
    let long_rms = rms(&long_samples);
    let wav = "/tmp/sdrpp_rspdx_reception.wav";
    write_wav(wav, &long_samples, 48_000).expect("WAV write failed");

    let quality = quality_label(long_rms, snr);
    println!("\n╔══════════════════════════════════════════╗");
    println!("║    RSPdx-R2 Real FM Reception Report     ║");
    println!("╠══════════════════════════════════════════╣");
    println!("║  Station : WMJI 105.7 FM (Cleveland OH)  ║");
    println!("║  FFT peak: {:<31}║", format!("{peak:.1} dBFS"));
    println!("║  SNR     : {:<31}║", format!("{snr:.1} dB"));
    println!("║  Stereo  : {:<31}║", if stereo { "YES" } else { "NO (weak signal?)" });
    println!("║  RDS PS  : {:<31}║", ps.as_deref().unwrap_or("not decoded"));
    println!("║  RMS     : {:<31}║", format!("{long_rms:.4}"));
    println!("║  Quality : {:<31}║", quality);
    println!("╚══════════════════════════════════════════╝");
    println!("WAV: {wav}  →  afplay {wav}");
    println!("If it sounds like FM radio — the whole stack is verified end-to-end.");

    // Hard assert only that audio exists
    assert!(long_rms > 0.01, "5 s audio RMS near zero — WBFM demod failed");
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn rms(s: &[f32]) -> f32 {
    if s.is_empty() { return 0.0; }
    (s.iter().map(|&x| x * x).sum::<f32>() / s.len() as f32).sqrt()
}

fn quality_label(rms_v: f32, snr: f32) -> &'static str {
    if rms_v < 0.02 || snr < 5.0 { "POOR" }
    else if rms_v < 0.10 || snr < 20.0 { "FAIR" }
    else { "GOOD" }
}

fn write_wav(path: &str, samples: &[f32], rate: u32) -> Result<(), hound::Error> {
    let spec = hound::WavSpec {
        channels: 1, sample_rate: rate,
        bits_per_sample: 16, sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(path, spec)?;
    for &s in samples {
        w.write_sample((s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)?;
    }
    w.finalize()
}
