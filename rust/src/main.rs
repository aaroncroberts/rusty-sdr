use parking_lot::RwLock;
use std::sync::Arc;

use sdrapp_core::{
    block::Block,
    config::AppConfig,
    signal_path::{SharedState, SignalPath},
    sink::AudioSink,
    source::Source,
};
use sdrapp_ui::SdrApp;

fn main() -> anyhow::Result<()> {
    // Structured logging — RUST_LOG overrides default
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("sdrapp=debug".parse()?),
        )
        .init();

    tracing::info!("sdrapp starting");

    let config = AppConfig::load_or_default();
    tracing::info!(
        freq_hz = config.ui.frequency_hz,
        config_path = %sdrapp_core::config::config_path().display(),
        "config loaded"
    );

    // Shared state: signal path writes, UI reads
    let shared = Arc::new(RwLock::new(SharedState::new()));
    {
        let mut s = shared.write();
        s.center_freq_hz = config.ui.frequency_hz;
        s.sample_rate_sps = config.source.sample_rate_sps;
        s.volume = config.ui.volume;
        s.zoom_level = config.ui.zoom_level;
        s.waterfall_speed = config.ui.waterfall_speed;
        s.nfm_bandwidth_hz = config.ui.nfm_bandwidth_hz;
        s.ctcss_squelch_enabled = config.ui.ctcss_enabled;
        // FFT / spectrum display settings
        s.fft_size = config.ui.fft_size;
        s.fft_averaging = config.ui.fft_averaging;
        s.band_plan_enabled = config.ui.band_plan_enabled;
        s.fft_window = match config.ui.fft_window.as_str() {
            "Rectangular" => sdrapp_core::dsp::FftWindow::Rectangular,
            "Hamming" => sdrapp_core::dsp::FftWindow::Hamming,
            "BlackmanHarris" => sdrapp_core::dsp::FftWindow::BlackmanHarris,
            _ => sdrapp_core::dsp::FftWindow::Hann,
        };
        s.fft_magnitudes = vec![-120.0; config.ui.fft_size];
        // Load persisted bookmarks
        s.bookmarks = config.bookmarks.iter().map(|b| {
            use sdrapp_core::signal_path::{Bookmark, DemodMode};
            let mode = match b.mode.as_str() {
                "Nfm" => DemodMode::Nfm,
                "Am" => DemodMode::Am,
                "Usb" => DemodMode::Usb,
                "Lsb" => DemodMode::Lsb,
                "Dsb" => DemodMode::Dsb,
                "Cw" => DemodMode::Cw,
                _ => DemodMode::Wbfm,
            };
            Bookmark::new(&b.name, b.freq_hz, mode)
        }).collect();
    }

    // Signal path command channel is created inside SignalPath::start() and
    // exposed via signal_path.cmd_tx.  This placeholder is replaced below.

    // Tokio runtime — eframe owns the main thread, tokio runs on worker threads
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .thread_name("sdrapp-dsp")
        .enable_all()
        .build()?;
    let _rt_guard = rt.enter();

    // ── Audio sink ────────────────────────────────────────────────────────────
    // Volume is applied by the signal path's Volume DSP block; sink runs at unity.
    let mut audio_sink = sdrapp_audio::CpalAudioSink::new(sdrapp_audio::AudioConfig {
        device_name: None,
        sample_rate: 48_000,
        volume: 1.0,
    });
    let audio_tx = audio_sink.sender();
    // start() spawns the cpal std::thread internally
    let _audio_handle = audio_sink.start();

    // ── IQ source: real hardware or demo mode ─────────────────────────────────
    //
    // Try to open the SDRplay API and enumerate devices.  If that fails (device
    // not connected, service not running), fall back to a synthetic test signal
    // source so the UI is always usable.
    //
    // Both source types implement Source + Block and expose `frequency_atomic()`
    // so the signal path and UI work identically in both modes.
    //
    // The source variables must stay alive until `eframe::run_native` returns
    // (i.e. the whole app lifetime) so their internal Arcs remain valid.
    //
    // Two subscribers are created: one for the signal path, one for IQ recording.
    let antenna = match config.source.antenna.as_str() {
        "B" => sdrapp_sdrplay::Antenna::B,
        "C" => sdrapp_sdrplay::Antenna::C,
        _ => sdrapp_sdrplay::Antenna::A,
    };

    let mut _sdrplay_source: Option<sdrapp_sdrplay::RspdxSource> = None;
    let mut _demo_source: Option<sdrapp_core::test_source::TestSignalSource> = None;

    let (iq_rx, iq_recorder_rx, freq_atomic, hardware_cmd_tx) = if sdrapp_sdrplay::RspdxSource::is_device_available() {
        tracing::info!("SDRplay device found — starting in hardware mode");
        shared.write().source_name = Some("SDRplay RSPdx-R2".to_string());

        {
            let mut s = shared.write();
            s.lna_state = config.source.lna_state;
            s.if_gain_dbfs = config.source.if_gain_dbfs;
            s.agc_enabled = config.source.agc_enabled;
            s.agc_setpoint_dbfs = config.source.agc_setpoint_dbfs;
            s.bias_t_enabled = config.source.bias_t_enabled;
            s.hdr_mode = config.source.hdr_mode;
            s.am_notch_enabled = config.source.am_notch_enabled;
            s.fm_notch_enabled = config.source.fm_notch_enabled;
            s.antenna_port = match config.source.antenna.as_str() { "B" => 1, "C" => 2, _ => 0 };
        }
        let mut src = sdrapp_sdrplay::RspdxSource::new(sdrapp_sdrplay::RspdxConfig {
            frequency_hz: config.ui.frequency_hz,
            sample_rate_sps: config.source.sample_rate_sps,
            antenna,
            agc_enabled: config.source.agc_enabled,
            lna_state: config.source.lna_state,
            if_gain_dbfs: config.source.if_gain_dbfs,
            agc_setpoint_dbfs: config.source.agc_setpoint_dbfs,
            bias_t_enabled: config.source.bias_t_enabled,
            hdr_mode: config.source.hdr_mode,
            am_notch_enabled: config.source.am_notch_enabled,
            fm_notch_enabled: config.source.fm_notch_enabled,
            ..Default::default()
        });
        let rx = src.subscribe();
        let iq_rec_rx = src.subscribe();
        let fa = src.frequency_atomic();
        let hw_tx = src.hardware_cmd_tx();
        drop(src.start());
        _sdrplay_source = Some(src);
        (rx, iq_rec_rx, fa, Some(hw_tx))
    } else {
        tracing::warn!("no SDRplay device — starting in demo mode (synthetic test signal)");
        shared.write().source_name = Some("Demo Mode".to_string());

        let mut src = sdrapp_core::test_source::TestSignalSource::new(
            config.ui.frequency_hz,
            config.source.sample_rate_sps,
        );
        let rx = src.subscribe();
        let iq_rec_rx = src.subscribe();
        let fa = src.frequency_atomic();
        drop(src.start());
        _demo_source = Some(src);
        (rx, iq_rec_rx, fa, None)
    };

    // ── Recorder ─────────────────────────────────────────────────────────────
    let mut recorder = sdrapp_recorder::Recorder::new(sdrapp_recorder::RecorderConfig {
        output_dir: dirs::audio_dir().unwrap_or_else(|| std::path::PathBuf::from(".")),
        sample_rate: 48_000,
    });
    let recorder_audio_tx = recorder.audio_tx.clone();
    let recorder_cmd_tx = recorder.cmd_tx.clone();
    // Give the recorder a second IQ subscriber for raw .iq file recording.
    recorder.set_iq_source(iq_recorder_rx);
    // Recorder::start() needs to run inside the tokio runtime
    rt.spawn(async move { recorder.start().await });

    // ── Signal path ───────────────────────────────────────────────────────────
    let signal_path = SignalPath::start(
        Arc::clone(&shared),
        iq_rx,
        Some(audio_tx),
        Some(recorder_audio_tx),
        None, // RepaintHandle: egui context not available yet; UI polls SharedState
        Some(freq_atomic),
        hardware_cmd_tx,
    );
    // Use the command sender that the signal path actually reads from.
    let cmd_tx = signal_path.cmd_tx.clone();

    // Apply persisted FFT settings (signal path starts with defaults; sync from config).
    {
        use sdrapp_core::{dsp::FftWindow, signal_path::SignalPathCommand};
        if config.ui.fft_size != 2048 {
            let _ = cmd_tx.try_send(SignalPathCommand::SetFftSize(config.ui.fft_size));
        }
        if config.ui.fft_averaging != 4 {
            let _ = cmd_tx.try_send(SignalPathCommand::SetFftAveraging(config.ui.fft_averaging));
        }
        let wf = match config.ui.fft_window.as_str() {
            "Rectangular" => FftWindow::Rectangular,
            "Hamming" => FftWindow::Hamming,
            "BlackmanHarris" => FftWindow::BlackmanHarris,
            _ => FftWindow::Hann,
        };
        if wf != FftWindow::Hann {
            let _ = cmd_tx.try_send(SignalPathCommand::SetFftWindow(wf));
        }
        if config.ui.band_plan_enabled {
            let _ = cmd_tx.try_send(SignalPathCommand::SetBandPlanEnabled(true));
        }
    }

    // ── MIDI controller ───────────────────────────────────────────────────────
    let midi_ctrl = sdrapp_midi::MidiController::new(
        sdrapp_midi::MidiConfig::with_nanokontrol2_defaults(),
        recorder_cmd_tx.clone(),
    );

    let shared_for_midi = Arc::clone(&shared);
    let cmd_tx_for_midi = cmd_tx.clone();
    rt.spawn(async move {
        let (handle, _inject_tx) = midi_ctrl.start(shared_for_midi, cmd_tx_for_midi);
        tracing::info!("MIDI controller started");
        let _ = handle.await;
    });

    // ── eframe (main thread UI loop) ─────────────────────────────────────────
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("SDR App")
            .with_inner_size([config.ui.window_width, config.ui.window_height])
            .with_min_inner_size([900.0, 600.0]),
        ..Default::default()
    };

    // Build MIDI binding descriptions for the help panel.
    // Converts nanoKontrol2 default bindings to (page, key_name, action_name) tuples.
    let midi_bindings: Vec<(usize, String, String)> = {
        use sdrapp_midi::{MidiConfig, MidiKeyKind};
        let cfg = MidiConfig::with_nanokontrol2_defaults();
        cfg.bindings.iter().map(|b| {
            let key_name = match b.key.kind {
                MidiKeyKind::ControlChange => format!("CC {}", b.key.number),
                MidiKeyKind::NoteOn => format!("Note {}", b.key.number),
            };
            let action_name = format!("{:?}", b.action);
            (b.page, key_name, action_name)
        }).collect()
    };

    let shared_for_app = Arc::clone(&shared);
    eframe::run_native(
        "SDR App",
        native_options,
        Box::new(move |cc| Ok(Box::new(SdrApp::new(cc, config, shared_for_app, cmd_tx, recorder_cmd_tx, midi_bindings)))),
    )
    .map_err(|e| anyhow::anyhow!("eframe error: {e}"))?;

    rt.shutdown_timeout(std::time::Duration::from_secs(3));
    tracing::info!("sdrapp exiting cleanly");
    Ok(())
}
