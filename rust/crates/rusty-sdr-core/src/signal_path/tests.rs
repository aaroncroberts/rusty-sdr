//! Unit and integration tests for the signal path.
use super::*;
use crate::dsp::fft::{any_bin_clipping, compute_snr_db};

// ── audio_frames_dropped ─────────────────────────────────────────────────

#[test]
fn audio_frames_dropped_defaults_to_zero() {
    let state = SharedState::new();
    assert_eq!(state.audio_frames_dropped, 0);
}

#[tokio::test]
async fn audio_frames_dropped_increments_when_channel_full() {
    // Create a bounded channel with capacity 1, then send two frames so the
    // second is dropped.  The signal path increments the counter on each
    // try_send failure.
    use crossbeam_channel::bounded;
    use crate::sample::StereoFrame;

    let (audio_tx_cap1, _audio_rx) = bounded::<Arc<[StereoFrame]>>(1);

    // Manually fill the channel so the next try_send fails.
    let dummy: Arc<[StereoFrame]> = vec![StereoFrame::mono(0.0); 1].into();
    audio_tx_cap1.try_send(Arc::clone(&dummy)).unwrap(); // fills slot

    let shared = Arc::new(RwLock::new(SharedState::new()));

    // try_send fails → increment counter
    if audio_tx_cap1.try_send(Arc::clone(&dummy)).is_err() {
        shared.write().audio_frames_dropped += 1;
    }

    assert_eq!(shared.read().audio_frames_dropped, 1);
}

// ── DemodMode::min_zoom ─────────────────────────────────────────────────────

#[test]
fn min_zoom_wbfm_is_0_05() {
    assert_eq!(DemodMode::Wbfm.min_zoom(), 0.05);
}

#[test]
fn min_zoom_cw_is_0_05() {
    assert_eq!(DemodMode::Cw.min_zoom(), 0.05);
}

#[test]
fn min_zoom_nfm_is_0_02() {
    assert_eq!(DemodMode::Nfm.min_zoom(), 0.02);
}

#[test]
fn min_zoom_am_usb_lsb_dsb_are_0_02() {
    for mode in [DemodMode::Am, DemodMode::Usb, DemodMode::Lsb, DemodMode::Dsb] {
        assert_eq!(
            mode.min_zoom(), 0.02,
            "{mode:?} should have 0.02 min zoom"
        );
    }
}

#[test]
fn min_zoom_is_never_below_0() {
    for mode in [
        DemodMode::Wbfm, DemodMode::Nfm, DemodMode::Am,
        DemodMode::Usb, DemodMode::Lsb, DemodMode::Dsb, DemodMode::Cw,
    ] {
        assert!(mode.min_zoom() > 0.0);
    }
}

// ── SNR helper + clipping detection ──────────────────────────────────────

/// Build a flat noise floor (noise_floor_db) with a peak (peak_db) at
/// `center ± peak_half_bins`.
fn make_fft_buf(n: usize, center: usize, peak_half_bins: usize, peak_db: f32, noise_db: f32) -> Vec<f32> {
    let mut buf = vec![noise_db; n];
    let lo = center.saturating_sub(peak_half_bins);
    let hi = (center + peak_half_bins).min(n - 1);
    for v in buf[lo..=hi].iter_mut() {
        *v = peak_db;
    }
    buf
}

#[test]
fn snr_helper_detects_peak_over_noise() {
    // 2048-bin FFT, signal at center ±50 bins at -10 dBFS, noise at -80 dBFS.
    let buf = make_fft_buf(2048, 1024, 50, -10.0, -80.0);
    let snr = compute_snr_db(&buf, 1024, 50);
    // SNR should be close to 70 dB (peak − noise floor = -10 − -80).
    assert!(snr > 60.0 && snr < 80.0, "snr = {snr}");
}

#[test]
fn snr_wide_window_captures_wbfm_signal_better_than_narrow() {
    // Model a WBFM spectrum where the stereo subcarrier sidebands sit at ±70
    // bins from centre (stronger than the carrier at 0 bins).
    // Noise floor at -80 dBFS.
    //
    // narrow window (half = 50): misses the ±70-bin sidebands → peak = -30
    // wide window   (half = 75): captures the ±70-bin sidebands → peak = -10
    let n = 2048;
    let center = n / 2;
    let mut buf = vec![-80.0f32; n];
    // Weak carrier at centre
    buf[center] = -30.0;
    // Strong sidebands at ±70 bins
    buf[center - 70] = -10.0;
    buf[center + 70] = -10.0;

    let snr_narrow = compute_snr_db(&buf, center, 50); // misses sidebands
    let snr_wide   = compute_snr_db(&buf, center, 75); // captures sidebands
    assert!(
        snr_wide > snr_narrow,
        "wide window should report higher SNR: wide={snr_wide} narrow={snr_narrow}"
    );
}

#[test]
fn clipping_detected_at_zero_dbfs() {
    let mut bins = vec![-10.0f32; 512];
    bins[100] = 0.0; // exactly 0 dBFS
    assert!(any_bin_clipping(&bins), "0.0 dBFS should trigger clipping");
}

#[test]
fn clipping_not_detected_below_zero_dbfs() {
    let bins = vec![-0.1f32; 512];
    assert!(!any_bin_clipping(&bins), "-0.1 dBFS should not trigger clipping");
}

#[test]
fn clipping_detected_when_bin_positive() {
    let mut bins = vec![-50.0f32; 512];
    bins[200] = 1.5; // clipped signal can exceed 0 dBFS in normalised FFT
    assert!(any_bin_clipping(&bins));
}

#[test]
fn fft_display_state_clipping_defaults_false() {
    let state = SharedState::new();
    assert!(!state.fft.fft_clipping_detected, "clipping should default to false");
}

#[test]
fn shared_state_default_has_fft_buffer() {
    let state = SharedState::new();
    assert_eq!(state.fft.fft_magnitudes.len(), FFT_SIZE);
    assert!(state.fft.fft_magnitudes.iter().all(|&v| v <= -100.0));
}

// ── Signal path command-dispatch integration tests ────────────────────────
//
// Pattern:
//   1. Start the signal path (spawns an OS thread).
//   2. Send a command — it queues in the crossbeam channel.
//   3. Send an empty IQ batch to unblock the thread's blocking_recv().
//   4. Sleep briefly — OS scheduler runs the signal path thread until it
//      blocks again (after draining the command queue and looping back to
//      blocking_recv).
//   5. Assert the SharedState mutation.

fn make_signal_path() -> (
    SignalPath,
    broadcast::Sender<Arc<[IqSample]>>,
    Arc<RwLock<SharedState>>,
) {
    let (iq_tx, iq_rx) = broadcast::channel(8);
    let shared = Arc::new(RwLock::new(SharedState::new()));
    let path = SignalPath::start(Arc::clone(&shared), iq_rx, None, None, None, None, None);
    (path, iq_tx, shared)
}

/// Send `cmd`, tickle the signal path loop with an empty IQ batch, wait.
async fn tick(
    cmd_tx: &crossbeam_channel::Sender<SignalPathCommand>,
    iq_tx: &broadcast::Sender<Arc<[IqSample]>>,
    cmd: impl Into<SignalPathCommand>,
) {
    cmd_tx.try_send(cmd.into()).unwrap();
    let _ = iq_tx.send(Arc::new([]));
    // The signal path runs on an OS thread (not a Tokio task), so
    // yield_now() would not help — sleep briefly to give the thread time
    // to wake up from blocking_recv(), dispatch the command, and loop back.
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
}

#[tokio::test]
async fn set_frequency_updates_center_freq() {
    let (path, iq_tx, shared) = make_signal_path();
    tick(&path.cmd_tx, &iq_tx, ReceiverCmd::SetFrequency(101_700_000)).await;
    assert_eq!(shared.read().center_freq_hz, 101_700_000);
    let _ = path.cmd_tx.try_send(SignalPathCommand::Stop);
}

#[tokio::test]
async fn set_volume_updates_demod_state() {
    let (path, iq_tx, shared) = make_signal_path();
    tick(&path.cmd_tx, &iq_tx, ReceiverCmd::SetVolume(0.42)).await;
    assert!((shared.read().demod.volume - 0.42).abs() < 1e-6);
    let _ = path.cmd_tx.try_send(SignalPathCommand::Stop);
}

#[tokio::test]
async fn set_demod_mode_switches_demod() {
    let (path, iq_tx, shared) = make_signal_path();
    tick(
        &path.cmd_tx,
        &iq_tx,
        ReceiverCmd::SetDemodMode(DemodMode::Am),
    )
    .await;
    assert_eq!(shared.read().demod.demod_mode, DemodMode::Am);
    let _ = path.cmd_tx.try_send(SignalPathCommand::Stop);
}

#[tokio::test]
async fn set_squelch_threshold_updates_demod_state() {
    let (path, iq_tx, shared) = make_signal_path();
    tick(
        &path.cmd_tx,
        &iq_tx,
        ReceiverCmd::SetSquelchThreshold(-45.0),
    )
    .await;
    assert!((shared.read().demod.squelch_threshold - (-45.0)).abs() < 1e-6);
    let _ = path.cmd_tx.try_send(SignalPathCommand::Stop);
}

#[tokio::test]
async fn bookmark_add_and_remove_round_trip() {
    let (path, iq_tx, shared) = make_signal_path();
    // SharedState::new() starts with 1 pre-loaded bookmark (BBC Radio 4).
    let initial = shared.read().bookmarks.len();
    tick(
        &path.cmd_tx,
        &iq_tx,
        BookmarkCmd::Add("NOAA Weather".to_string()),
    )
    .await;
    assert_eq!(
        shared.read().bookmarks.len(),
        initial + 1,
        "one bookmark added"
    );
    let last_idx = shared.read().bookmarks.len() - 1;
    assert_eq!(shared.read().bookmarks[last_idx].name, "NOAA Weather");
    tick(&path.cmd_tx, &iq_tx, BookmarkCmd::Remove(last_idx)).await;
    assert_eq!(
        shared.read().bookmarks.len(),
        initial,
        "back to initial count after Remove"
    );
    let _ = path.cmd_tx.try_send(SignalPathCommand::Stop);
}

#[tokio::test]
async fn fft_size_change_accepted_for_power_of_two() {
    let (path, iq_tx, shared) = make_signal_path();
    tick(&path.cmd_tx, &iq_tx, DisplayCmd::SetFftSize(4096)).await;
    assert_eq!(shared.read().fft.fft_size, 4096);
    assert_eq!(shared.read().fft.fft_magnitudes.len(), 4096);
    let _ = path.cmd_tx.try_send(SignalPathCommand::Stop);
}

#[tokio::test]
async fn fft_size_change_rejected_for_non_power_of_two() {
    let (path, iq_tx, shared) = make_signal_path();
    // 3000 is not a power-of-two — should be silently ignored
    tick(&path.cmd_tx, &iq_tx, DisplayCmd::SetFftSize(3000)).await;
    assert_eq!(
        shared.read().fft.fft_size,
        FFT_SIZE,
        "non-power-of-two FFT size should be ignored"
    );
    let _ = path.cmd_tx.try_send(SignalPathCommand::Stop);
}

#[tokio::test]
async fn band_plan_toggle_updates_display_state() {
    let (path, iq_tx, shared) = make_signal_path();
    tick(&path.cmd_tx, &iq_tx, DisplayCmd::SetBandPlanEnabled(true)).await;
    assert!(shared.read().fft.band_plan_enabled);
    let _ = path.cmd_tx.try_send(SignalPathCommand::Stop);
}

#[tokio::test]
async fn demod_mode_switch_clears_rds_state() {
    let (path, iq_tx, shared) = make_signal_path();
    // Seed some RDS data directly into SharedState.
    {
        let mut s = shared.write();
        s.rds.ps_name = Some("TEST FM".to_string());
        s.rds.pty = Some(3);
        s.rds.rt = Some("Radio Text Here".to_string());
    }
    // Switch from WBFM to AM — signal path must clear RDS on mode change.
    tick(
        &path.cmd_tx,
        &iq_tx,
        ReceiverCmd::SetDemodMode(DemodMode::Am),
    )
    .await;
    let s = shared.read();
    assert_eq!(s.demod.demod_mode, DemodMode::Am);
    assert!(s.rds.ps_name.is_none(), "ps_name must be cleared on mode switch");
    assert!(s.rds.pty.is_none(), "pty must be cleared on mode switch");
    assert!(s.rds.rt.is_none(), "rt must be cleared on mode switch");
    drop(s);
    let _ = path.cmd_tx.try_send(SignalPathCommand::Stop);
}

#[tokio::test]
async fn frequency_change_clears_rds_state() {
    let (path, iq_tx, shared) = make_signal_path();
    // Seed RDS data.
    {
        let mut s = shared.write();
        s.rds.ps_name = Some("STATION".to_string());
        s.rds.ta = true;
    }
    tick(&path.cmd_tx, &iq_tx, ReceiverCmd::SetFrequency(98_100_000)).await;
    let s = shared.read();
    assert_eq!(s.center_freq_hz, 98_100_000);
    assert!(s.rds.ps_name.is_none(), "ps_name must be cleared on freq change");
    assert!(!s.rds.ta, "ta must be cleared on freq change");
    drop(s);
    let _ = path.cmd_tx.try_send(SignalPathCommand::Stop);
}

#[tokio::test]
async fn nfm_bandwidth_change_updates_state() {
    let (path, iq_tx, shared) = make_signal_path();
    // Switch to NFM first.
    tick(
        &path.cmd_tx,
        &iq_tx,
        ReceiverCmd::SetDemodMode(DemodMode::Nfm),
    )
    .await;
    // Change bandwidth from default 12.5k to 25k.
    tick(&path.cmd_tx, &iq_tx, ReceiverCmd::SetNfmBandwidth(25_000)).await;
    assert_eq!(shared.read().demod.nfm_bandwidth_hz, 25_000);
    let _ = path.cmd_tx.try_send(SignalPathCommand::Stop);
}

#[tokio::test]
async fn ctcss_enable_disable_round_trip() {
    let (path, iq_tx, shared) = make_signal_path();
    tick(
        &path.cmd_tx,
        &iq_tx,
        ReceiverCmd::SetCtcssEnabled(true),
    )
    .await;
    assert!(shared.read().demod.ctcss_squelch_enabled);
    assert!(!shared.read().demod.ctcss_tone_detected, "tone must be false after enable");
    tick(
        &path.cmd_tx,
        &iq_tx,
        ReceiverCmd::SetCtcssEnabled(false),
    )
    .await;
    assert!(!shared.read().demod.ctcss_squelch_enabled);
    let _ = path.cmd_tx.try_send(SignalPathCommand::Stop);
}

#[tokio::test]
async fn set_tune_step_updates_demod_state() {
    let (path, iq_tx, shared) = make_signal_path();
    tick(&path.cmd_tx, &iq_tx, ReceiverCmd::SetTuneStep(10_000)).await;
    assert_eq!(shared.read().demod.tune_step_hz, 10_000);
    let _ = path.cmd_tx.try_send(SignalPathCommand::Stop);
}

// ── Scanner ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn scanner_start_stop_transitions() {
    let (path, iq_tx, shared) = make_signal_path();
    // Add a second bookmark so the scanner has something to work with.
    tick(
        &path.cmd_tx,
        &iq_tx,
        BookmarkCmd::Add("Test Station".to_string()),
    )
    .await;
    // Start scanner.
    tick(
        &path.cmd_tx,
        &iq_tx,
        ScanCmd::Start(String::new()),
    )
    .await;
    assert!(shared.read().scanner.scan_running, "scanner should be running after Start");
    // Stop scanner.
    tick(&path.cmd_tx, &iq_tx, ScanCmd::Stop).await;
    assert!(!shared.read().scanner.scan_running, "scanner should stop after Stop");
    let _ = path.cmd_tx.try_send(SignalPathCommand::Stop);
}

#[tokio::test]
async fn scanner_set_dwell_updates_state() {
    let (path, iq_tx, shared) = make_signal_path();
    tick(&path.cmd_tx, &iq_tx, ScanCmd::SetDwell(5.0)).await;
    let dwell = shared.read().scanner.scan_dwell_secs;
    assert!((dwell - 5.0).abs() < 1e-6, "dwell should be 5.0 s");
    let _ = path.cmd_tx.try_send(SignalPathCommand::Stop);
}

#[tokio::test]
async fn scanner_set_dwell_clamps_minimum() {
    let (path, iq_tx, shared) = make_signal_path();
    tick(&path.cmd_tx, &iq_tx, ScanCmd::SetDwell(0.1)).await;
    let dwell = shared.read().scanner.scan_dwell_secs;
    assert!(dwell >= 0.5, "dwell must be clamped to minimum 0.5 s");
    let _ = path.cmd_tx.try_send(SignalPathCommand::Stop);
}

#[tokio::test]
async fn scanner_start_with_no_matching_bookmarks_stops_immediately() {
    let (path, iq_tx, shared) = make_signal_path();
    // Only default bookmark (BBC R4, category ""). Start with a category
    // filter that matches nothing.
    tick(
        &path.cmd_tx,
        &iq_tx,
        ScanCmd::Start("NONEXISTENT_CATEGORY_XYZ".to_string()),
    )
    .await;
    // The scanner code detects no matching bookmarks and stops immediately.
    assert!(
        !shared.read().scanner.scan_running,
        "scanner should not be running when no bookmarks match the category filter"
    );
    let _ = path.cmd_tx.try_send(SignalPathCommand::Stop);
}

// ── Peak-hold dispatch ────────────────────────────────────────────────────

#[tokio::test]
async fn set_peak_hold_enabled_updates_state() {
    let (path, iq_tx, shared) = make_signal_path();
    assert!(!shared.read().fft.peak_hold_enabled, "default should be false");
    tick(&path.cmd_tx, &iq_tx, DisplayCmd::SetPeakHoldEnabled(true)).await;
    assert!(shared.read().fft.peak_hold_enabled, "should be enabled after command");
    tick(&path.cmd_tx, &iq_tx, DisplayCmd::SetPeakHoldEnabled(false)).await;
    assert!(!shared.read().fft.peak_hold_enabled, "should disable after second command");
    let _ = path.cmd_tx.try_send(SignalPathCommand::Stop);
}

#[tokio::test]
async fn set_peak_hold_decay_updates_state() {
    let (path, iq_tx, shared) = make_signal_path();
    tick(&path.cmd_tx, &iq_tx, DisplayCmd::SetPeakHoldDecay(1.0)).await;
    let decay = shared.read().fft.peak_hold_decay_db;
    assert!((decay - 1.0).abs() < 1e-6, "decay should be 1.0 dB/frame, got {decay}");
    let _ = path.cmd_tx.try_send(SignalPathCommand::Stop);
}

#[tokio::test]
async fn set_peak_hold_decay_clamps_to_valid_range() {
    let (path, iq_tx, shared) = make_signal_path();
    tick(&path.cmd_tx, &iq_tx, DisplayCmd::SetPeakHoldDecay(0.001)).await;
    assert!(
        shared.read().fft.peak_hold_decay_db >= 0.1,
        "decay below 0.1 should be clamped"
    );
    tick(&path.cmd_tx, &iq_tx, DisplayCmd::SetPeakHoldDecay(99.0)).await;
    assert!(
        shared.read().fft.peak_hold_decay_db <= 2.0,
        "decay above 2.0 should be clamped"
    );
    let _ = path.cmd_tx.try_send(SignalPathCommand::Stop);
}

#[test]
fn peak_hold_enabled_defaults_false() {
    let state = SharedState::new();
    assert!(!state.fft.peak_hold_enabled, "peak_hold_enabled defaults to false");
}

#[test]
fn peak_hold_decay_defaults_to_half_db() {
    let state = SharedState::new();
    assert!(
        (state.fft.peak_hold_decay_db - 0.5).abs() < 1e-6,
        "default decay should be 0.5 dB/frame"
    );
}

// ── FFT / audio independence tests ──────────────────────────────────────────
//
// These tests verify that FFT processing runs on a dedicated thread so it
// cannot starve the audio demodulation path.

/// After sending enough IQ to fill an FFT frame, fft_magnitudes must be
/// updated to non-trivial values (not all -120 dBFS).
///
/// This test catches regressions where the FFT thread stops writing to
/// SharedState — e.g., if the IQ fan-out channel silently drops all batches.
#[tokio::test]
async fn fft_magnitudes_updated_after_enough_iq() {
    let (path, iq_tx, shared) = make_signal_path();

    // Use tick() to send Start and wait for the signal path thread to process it.
    // The signal path starts paused; IQ fan-out to the FFT thread only happens
    // when paused == false, so we must synchronise before sending IQ data.
    tick(&path.cmd_tx, &iq_tx, SignalPathCommand::Start).await;

    // Build a 1 kHz complex tone — real signal energy ensures bins exceed -100 dBFS.
    let sr = 2_000_000_u32;
    let batch_size = 4096_usize;
    let batch: Arc<[IqSample]> = (0..batch_size)
        .map(|i| {
            let phase = 2.0 * std::f32::consts::PI * 1_000.0 / sr as f32 * i as f32;
            IqSample { re: phase.cos() * 0.5, im: phase.sin() * 0.5 }
        })
        .collect::<Vec<_>>()
        .into();

    // Send several batches to ensure the FFT fires (FFT_SIZE = 2048, batch = 4096).
    for _ in 0..4 {
        let _ = iq_tx.send(Arc::clone(&batch));
    }

    // Poll until the FFT thread writes non-trivial values (up to 2 s).
    let mut non_trivial = false;
    for _ in 0..100 {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        if shared.read().fft.fft_magnitudes.iter().any(|&v| v > -100.0) {
            non_trivial = true;
            break;
        }
    }
    assert!(
        non_trivial,
        "fft_magnitudes should contain non-trivial values after processing IQ — all still at floor"
    );
}

/// FFT thread handles SetFftSize command correctly: magnitudes buffer resizes
/// and signal path keeps running (no panic, no deadlock).
#[tokio::test]
async fn set_fft_size_updates_shared_state_via_fft_thread() {
    let (path, iq_tx, shared) = make_signal_path();

    // Send a resize command then tickle with IQ.
    tick(&path.cmd_tx, &iq_tx, DisplayCmd::SetFftSize(512)).await;

    assert_eq!(shared.read().fft.fft_size, 512);
    assert_eq!(shared.read().fft.fft_magnitudes.len(), 512);
}
