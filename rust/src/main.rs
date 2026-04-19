use parking_lot::RwLock;
use std::sync::Arc;

use rusty_sdr_core::{
    block::Block,
    config::AppConfig,
    signal_path::{SharedState, SignalPath},
    sink::AudioSink,
    source::Source,
};
use rusty_sdr_ui::SdrApp;

fn main() -> anyhow::Result<()> {
    // CLI flags
    let auto_start = std::env::args().any(|a| a == "--auto-start" || a == "-s");

    // Structured logging — RUST_LOG overrides default
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("sdrapp=debug".parse()?),
        )
        .init();

    if auto_start {
        tracing::info!("--auto-start flag set: will begin listening on first frame");
    }
    tracing::info!("sdrapp starting");

    let config = AppConfig::load_or_default();
    tracing::info!(
        freq_hz = config.ui.frequency_hz,
        config_path = %rusty_sdr_core::config::config_path().display(),
        "config loaded"
    );

    // Shared state: signal path writes, UI reads
    let shared = Arc::new(RwLock::new(SharedState::new()));
    {
        let mut s = shared.write();
        s.center_freq_hz = config.ui.frequency_hz;
        s.sample_rate_sps = config.source.sample_rate_sps;
        s.demod.volume = config.ui.volume;
        s.zoom_level = config.ui.zoom_level;
        s.waterfall_speed = config.ui.waterfall_speed;
        s.demod.nfm_bandwidth_hz = config.ui.nfm_bandwidth_hz;
        s.demod.ctcss_squelch_enabled = config.ui.ctcss_enabled;
        s.demod.squelch_threshold = config.ui.squelch_threshold_dbfs;
        s.demod.tune_step_hz = config.ui.tune_step_hz;
        // FFT / spectrum display settings
        s.fft.fft_size = config.ui.fft_size;
        s.fft.fft_averaging = config.ui.fft_averaging;
        s.fft.band_plan_enabled = config.ui.band_plan_enabled;
        s.fft.fft_window = match config.ui.fft_window.as_str() {
            "Rectangular" => rusty_sdr_core::dsp::FftWindow::Rectangular,
            "Hamming" => rusty_sdr_core::dsp::FftWindow::Hamming,
            "BlackmanHarris" => rusty_sdr_core::dsp::FftWindow::BlackmanHarris,
            _ => rusty_sdr_core::dsp::FftWindow::Hann,
        };
        s.fft.fft_magnitudes = vec![-120.0; config.ui.fft_size];
        // Load persisted MIDI Learn bindings (knob_id → CC becomes CC → knob_id)
        s.midi_cc_to_knob = config
            .midi_learn
            .iter()
            .map(|(knob_id, &cc)| (cc, knob_id.clone()))
            .collect();
        // Load persisted bookmarks — from external file if configured, else inline.
        use rusty_sdr_core::signal_path::{Bookmark, DemodMode};
        let bookmark_configs: Vec<rusty_sdr_core::config::BookmarkConfig> =
            if let Some(ref path) = config.bookmarks_file {
                let loaded = rusty_sdr_core::config::BookmarkConfig::load_from_csv(path);
                if loaded.is_empty() {
                    tracing::warn!(path, "bookmarks_file set but no bookmarks loaded — using inline");
                    config.bookmarks.clone()
                } else {
                    tracing::info!(path, count = loaded.len(), "loaded bookmarks from file");
                    loaded
                }
            } else {
                config.bookmarks.clone()
            };
        s.bookmarks = bookmark_configs
            .iter()
            .map(|b| {
                let mode = match b.mode.as_str() {
                    "Nfm" => DemodMode::Nfm,
                    "Am" => DemodMode::Am,
                    "Usb" => DemodMode::Usb,
                    "Lsb" => DemodMode::Lsb,
                    "Dsb" => DemodMode::Dsb,
                    "Cw" => DemodMode::Cw,
                    _ => DemodMode::Wbfm,
                };
                let bm = Bookmark::new(&b.name, b.freq_hz, mode).with_category(&b.category);
                if let (Some(bw), Some(sq), Some(ct)) = (
                    b.nfm_bandwidth_hz,
                    b.squelch_threshold_dbfs,
                    b.ctcss_enabled,
                ) {
                    bm.with_nfm_settings(bw, sq, ct)
                } else {
                    bm
                }
            })
            .collect();
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
    let mut audio_sink = rusty_sdr_audio::CpalAudioSink::new(rusty_sdr_audio::AudioConfig {
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
        "B" => rusty_sdr_sdrplay::Antenna::B,
        "C" => rusty_sdr_sdrplay::Antenna::C,
        _ => rusty_sdr_sdrplay::Antenna::A,
    };

    let mut _sdrplay_source: Option<rusty_sdr_sdrplay::RspdxSource> = None;
    let mut _rtlsdr_source: Option<rusty_sdr_rtlsdr::RtlSdrSource> = None;
    let mut _demo_source: Option<rusty_sdr_core::test_source::TestSignalSource> = None;
    // Holds a hot-plugged hardware source so it stays alive for the app lifetime.
    let hotplug_source: std::sync::Arc<parking_lot::Mutex<Option<rusty_sdr_sdrplay::RspdxSource>>> =
        std::sync::Arc::new(parking_lot::Mutex::new(None));

    // Probe for hardware on a background thread with a timeout so a hung
    // sdrplay_api_GetDevices call never blocks the main thread (and eframe).
    let sdrplay_available = {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(rusty_sdr_sdrplay::RspdxSource::is_device_available());
        });
        rx.recv_timeout(std::time::Duration::from_secs(3)).unwrap_or(false)
    };
    let rtlsdr_available = if !sdrplay_available {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(rusty_sdr_rtlsdr::RtlSdrSource::is_device_available());
        });
        rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap_or(false)
    } else {
        false
    };

    let (iq_rx, iq_recorder_rx, iq_adsb_tx, freq_atomic, hardware_cmd_tx) =
        if sdrplay_available {
            tracing::info!("SDRplay device found — starting in hardware mode");
            shared.write().source_name = Some("SDRplay RSPdx-R2".to_string());

            {
                let mut s = shared.write();
                s.hardware.lna_state = config.source.lna_state;
                s.hardware.if_gain_dbfs = config.source.if_gain_dbfs;
                s.hardware.agc_enabled = config.source.agc_enabled;
                s.hardware.agc_setpoint_dbfs = config.source.agc_setpoint_dbfs;
                s.hardware.bias_t_enabled = config.source.bias_t_enabled;
                s.hardware.hdr_mode = config.source.hdr_mode;
                s.hardware.am_notch_enabled = config.source.am_notch_enabled;
                s.hardware.fm_notch_enabled = config.source.fm_notch_enabled;
                s.hardware.antenna_port = match config.source.antenna.as_str() {
                    "B" => 1,
                    "C" => 2,
                    _ => 0,
                };
            }
            let mut src = rusty_sdr_sdrplay::RspdxSource::new(rusty_sdr_sdrplay::RspdxConfig {
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
                decimation_factor: config.source.decimation_factor,
                if_mode: match config.source.if_mode.as_str() {
                    "LowIF200kHz" => rusty_sdr_sdrplay::IfMode::LowIf200kHz,
                    "LowIF500kHz" => rusty_sdr_sdrplay::IfMode::LowIf500kHz,
                    "LowIF1MHz"   => rusty_sdr_sdrplay::IfMode::LowIf1MHz,
                    "LowIF2MHz"   => rusty_sdr_sdrplay::IfMode::LowIf2MHz,
                    _             => rusty_sdr_sdrplay::IfMode::ZeroIf,
                },
            })
            .with_shared(Arc::clone(&shared));
            let rx = src.subscribe();
            let iq_rec_rx = src.subscribe();
            let iq_adsb = src.iq_sender();
            let fa = Source::frequency_atomic(&src);
            let hw_tx = src.hardware_cmd_tx();
            // Watch device status and update source_name in SharedState.
            let mut status_rx = src.status_rx();
            let shared_for_status = Arc::clone(&shared);
            rt.spawn(async move {
                loop {
                    if status_rx.changed().await.is_err() {
                        break; // sender dropped (source was stopped)
                    }
                    let status = status_rx.borrow().clone();
                    let name = match &status {
                        rusty_sdr_sdrplay::DeviceStatus::Running { serial, .. } =>
                            format!("SDRplay RSPdx-R2 ({})", serial),
                        rusty_sdr_sdrplay::DeviceStatus::Reconnecting { attempt, .. } =>
                            format!("SDRplay RSPdx-R2 (reconnecting…  attempt {attempt})"),
                        rusty_sdr_sdrplay::DeviceStatus::Disconnected =>
                            "SDRplay RSPdx-R2 (disconnected)".into(),
                        rusty_sdr_sdrplay::DeviceStatus::Connecting =>
                            "SDRplay RSPdx-R2 (connecting…)".into(),
                    };
                    shared_for_status.write().source_name = Some(name);
                }
            });
            drop(src.start());
            _sdrplay_source = Some(src);
            (rx, iq_rec_rx, iq_adsb, fa, Some(hw_tx))
        } else if rtlsdr_available {
            tracing::info!("RTL-SDR device found — starting in RTL-SDR mode");
            let rtl_cfg = rusty_sdr_rtlsdr::RtlSdrConfig {
                frequency_hz: config.ui.frequency_hz,
                sample_rate_sps: config.source.sample_rate_sps.min(2_048_000),
                ..Default::default()
            };
            let caps_name = "RTL-SDR (device 0)".to_string();
            let mut src = rusty_sdr_rtlsdr::RtlSdrSource::open(rtl_cfg)
                .expect("device available but open failed");
            shared.write().source_name = Some(caps_name);
            let rx = src.subscribe();
            let iq_rec_rx = src.subscribe();
            let iq_adsb = src.iq_sender();
            let fa = Source::frequency_atomic(&src);
            drop(src.start());
            _rtlsdr_source = Some(src);
            (rx, iq_rec_rx, iq_adsb, fa, None)
        } else {
            tracing::warn!(
                "no hardware device found — starting in demo mode (synthetic test signal)"
            );
            shared.write().source_name = Some("Demo Mode".to_string());

            let mut src = rusty_sdr_core::test_source::TestSignalSource::new(
                config.ui.frequency_hz,
                config.source.sample_rate_sps,
            );
            let rx = src.subscribe();
            let iq_rec_rx = src.subscribe();
            let iq_adsb = src.iq_sender();
            let fa = Source::frequency_atomic(&src);
            drop(src.start());
            _demo_source = Some(src);
            (rx, iq_rec_rx, iq_adsb, fa, None)
        };

    // ── Recorder ─────────────────────────────────────────────────────────────
    let mut recorder = rusty_sdr_recorder::Recorder::new(rusty_sdr_recorder::RecorderConfig {
        output_dir: dirs::audio_dir().unwrap_or_else(|| std::path::PathBuf::from(".")),
        sample_rate: 48_000,
    });
    let recorder_audio_tx = recorder.audio_tx.clone();
    let recorder_cmd_tx = recorder.cmd_tx.clone();
    // Give the recorder a second IQ subscriber for raw .iq file recording.
    recorder.set_iq_source(iq_recorder_rx);
    // Recorder::start() needs to run inside the tokio runtime
    let shared_for_recorder = Arc::clone(&shared);
    rt.spawn(async move { recorder.start(shared_for_recorder).await });

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

    // ── Hardware hot-plug probe ───────────────────────────────────────────────
    // When the app started in demo mode, poll for hardware every 3 seconds.
    // On detection: create a real source, send ReconnectSource so the signal
    // path hot-swaps without a restart.
    if _demo_source.is_some() {
        let shared_probe = Arc::clone(&shared);
        let cmd_tx_probe = cmd_tx.clone();
        let holder = std::sync::Arc::clone(&hotplug_source);
        let sdrplay_cfg = rusty_sdr_sdrplay::RspdxConfig {
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
            decimation_factor: config.source.decimation_factor,
            if_mode: match config.source.if_mode.as_str() {
                "LowIF200kHz" => rusty_sdr_sdrplay::IfMode::LowIf200kHz,
                "LowIF500kHz" => rusty_sdr_sdrplay::IfMode::LowIf500kHz,
                "LowIF1MHz"   => rusty_sdr_sdrplay::IfMode::LowIf1MHz,
                "LowIF2MHz"   => rusty_sdr_sdrplay::IfMode::LowIf2MHz,
                _             => rusty_sdr_sdrplay::IfMode::ZeroIf,
            },
        };
        rt.spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                // Stop probing once hardware is connected.
                if shared_probe.read().source_name.as_deref() != Some("Demo Mode") {
                    break;
                }
                let available =
                    tokio::task::spawn_blocking(rusty_sdr_sdrplay::RspdxSource::is_device_available)
                        .await
                        .unwrap_or(false);

                if available {
                    tracing::info!("hardware device detected while running — hot-swapping source");
                    let mut src = rusty_sdr_sdrplay::RspdxSource::new(sdrplay_cfg.clone())
                        .with_shared(Arc::clone(&shared_probe));
                    let new_rx = src.subscribe();
                    let new_hw_tx = src.hardware_cmd_tx();
                    drop(src.start());
                    *holder.lock() = Some(src);
                    shared_probe.write().source_name = Some("SDRplay RSPdx-R2".to_string());
                    let _ = cmd_tx_probe.try_send(
                        rusty_sdr_core::signal_path::SignalPathCommand::ReconnectSource {
                            iq_rx: new_rx,
                            hardware_cmd_tx: Some(new_hw_tx),
                        },
                    );
                    tracing::info!("hot-plug complete — send Start to begin listening");
                    break;
                }
            }
        });
    }

    // Apply persisted FFT settings (signal path starts with defaults; sync from config).
    {
        use rusty_sdr_core::{dsp::FftWindow, signal_path::DisplayCmd};
        if config.ui.fft_size != 2048 {
            let _ = cmd_tx.try_send(DisplayCmd::SetFftSize(config.ui.fft_size).into());
        }
        if config.ui.fft_averaging != 4 {
            let _ = cmd_tx.try_send(DisplayCmd::SetFftAveraging(config.ui.fft_averaging).into());
        }
        let wf = match config.ui.fft_window.as_str() {
            "Rectangular" => FftWindow::Rectangular,
            "Hamming" => FftWindow::Hamming,
            "BlackmanHarris" => FftWindow::BlackmanHarris,
            _ => FftWindow::Hann,
        };
        if wf != FftWindow::Hann {
            let _ = cmd_tx.try_send(DisplayCmd::SetFftWindow(wf).into());
        }
        if config.ui.band_plan_enabled {
            let _ = cmd_tx.try_send(DisplayCmd::SetBandPlanEnabled(true).into());
        }
    }

    // ── Rigctl server ─────────────────────────────────────────────────────────
    if config.rigctl.enabled {
        let shared_for_rigctl = Arc::clone(&shared);
        let cmd_tx_for_rigctl = cmd_tx.clone();
        let port = config.rigctl.port;
        rt.spawn(async move {
            let handle = rusty_sdr_core::rigctl::start(port, shared_for_rigctl, cmd_tx_for_rigctl);
            let _ = handle.await;
        });
    }

    // ── MIDI controller ───────────────────────────────────────────────────────
    let midi_ctrl = rusty_sdr_midi::MidiController::new(
        rusty_sdr_midi::MidiConfig::with_nanokontrol2_defaults(),
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
            .with_title("Rusty SDR")
            .with_inner_size([config.ui.window_width, config.ui.window_height])
            .with_min_inner_size([900.0, 600.0]),
        ..Default::default()
    };

    // Build MIDI binding descriptions for the help panel.
    // Converts nanoKontrol2 default bindings to (page, key_name, action_name) tuples.
    let midi_bindings: Vec<(usize, String, String)> = {
        use rusty_sdr_midi::{MidiConfig, MidiKeyKind};
        let cfg = MidiConfig::with_nanokontrol2_defaults();
        cfg.bindings
            .iter()
            .map(|b| {
                let key_name = match b.key.kind {
                    MidiKeyKind::ControlChange => format!("CC {}", b.key.number),
                    MidiKeyKind::NoteOn => format!("Note {}", b.key.number),
                };
                let action_name = format!("{:?}", b.action);
                (b.page, key_name, action_name)
            })
            .collect()
    };

    let shared_for_app = Arc::clone(&shared);
    eframe::run_native(
        "Rusty SDR",
        native_options,
        Box::new(move |cc| {
            Ok(Box::new(SdrApp::new(
                cc,
                config,
                shared_for_app,
                cmd_tx,
                recorder_cmd_tx,
                midi_bindings,
                auto_start,
                Some(iq_adsb_tx),
            )))
        }),
    )
    .map_err(|e| anyhow::anyhow!("eframe error: {e}"))?;

    rt.shutdown_timeout(std::time::Duration::from_secs(3));
    tracing::info!("sdrapp exiting cleanly");
    Ok(())
}
