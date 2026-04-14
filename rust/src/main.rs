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
    }

    // Signal path command channel: UI → signal path
    let (cmd_tx, _cmd_rx) =
        crossbeam_channel::bounded::<sdrapp_core::signal_path::SignalPathCommand>(64);

    // Tokio runtime — eframe owns the main thread, tokio runs on worker threads
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .thread_name("sdrapp-dsp")
        .enable_all()
        .build()?;
    let _rt_guard = rt.enter();

    // ── Audio sink ────────────────────────────────────────────────────────────
    let mut audio_sink = sdrapp_audio::CpalAudioSink::new(sdrapp_audio::AudioConfig {
        device_name: None,
        sample_rate: 48_000,
        volume: config.ui.volume,
    });
    let audio_tx = audio_sink.sender();
    // start() spawns the cpal std::thread internally
    let _audio_handle = audio_sink.start();

    // ── Recorder ─────────────────────────────────────────────────────────────
    let mut recorder = sdrapp_recorder::Recorder::new(sdrapp_recorder::RecorderConfig {
        output_dir: dirs::audio_dir().unwrap_or_else(|| std::path::PathBuf::from(".")),
        sample_rate: 48_000,
    });
    let recorder_audio_tx = recorder.audio_tx.clone();
    let recorder_cmd_tx = recorder.cmd_tx.clone();
    // Recorder::start() needs to run inside the tokio runtime
    rt.spawn(async move { recorder.start().await });

    // ── SDRplay source ────────────────────────────────────────────────────────
    let antenna = match config.source.antenna.as_str() {
        "B" => sdrapp_sdrplay::Antenna::B,
        "C" => sdrapp_sdrplay::Antenna::C,
        _ => sdrapp_sdrplay::Antenna::A,
    };

    let mut source = sdrapp_sdrplay::RspdxSource::new(sdrapp_sdrplay::RspdxConfig {
        frequency_hz: config.ui.frequency_hz,
        sample_rate_sps: config.source.sample_rate_sps,
        antenna,
        agc_enabled: config.source.agc_enabled,
        lna_state: config.source.lna_state,
        ..Default::default()
    });

    let iq_rx = source.subscribe();
    let _source_handle = source.start();

    // ── Signal path ───────────────────────────────────────────────────────────
    let _signal_path = SignalPath::start(
        Arc::clone(&shared),
        iq_rx,
        Some(audio_tx),
        Some(recorder_audio_tx),
        None, // RepaintHandle: egui context not available yet; UI polls SharedState
    );

    // ── MIDI controller ───────────────────────────────────────────────────────
    let midi_ctrl = sdrapp_midi::MidiController::new(
        sdrapp_midi::MidiConfig::with_nanokontrol2_defaults(),
        recorder_cmd_tx,
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

    let shared_for_app = Arc::clone(&shared);
    eframe::run_native(
        "SDR App",
        native_options,
        Box::new(move |cc| Ok(Box::new(SdrApp::new(cc, config, shared_for_app, cmd_tx)))),
    )
    .map_err(|e| anyhow::anyhow!("eframe error: {e}"))?;

    rt.shutdown_timeout(std::time::Duration::from_secs(3));
    tracing::info!("sdrapp exiting cleanly");
    Ok(())
}
