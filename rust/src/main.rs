use std::sync::Arc;
use parking_lot::RwLock;

use sdrapp_core::{
    config::AppConfig,
    signal_path::SharedState,
};
use sdrapp_ui::SdrApp;

fn main() -> anyhow::Result<()> {
    // Structured logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("sdrapp=debug".parse()?)
        )
        .init();

    tracing::info!("sdrapp starting");

    // Load config from disk (creates defaults on first run)
    let config = AppConfig::load_or_default();
    tracing::info!(
        freq_hz = config.ui.frequency_hz,
        config_path = %sdrapp_core::config::config_path().display(),
        "config loaded"
    );

    // Shared state: signal path writes, UI reads
    let shared = Arc::new(RwLock::new(SharedState::new()));

    // Command channel: UI writes, signal path reads
    // crossbeam_channel is Send + Sync and works across thread boundaries
    let (cmd_tx, _cmd_rx) = crossbeam_channel::bounded::<sdrapp_core::signal_path::SignalPathCommand>(64);

    // Start tokio runtime on a background thread pool
    // eframe will own the main thread event loop, so tokio must not.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("sdrapp-dsp")
        .enable_all()
        .build()?;

    // TODO: start signal path tasks in rt when source is connected
    // For now, runtime is available for future async module init
    let _rt_guard = rt.enter();

    // Launch eframe — this blocks the main thread until the window closes
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("SDR App")
            .with_inner_size([config.ui.window_width, config.ui.window_height])
            .with_min_inner_size([800.0, 500.0]),
        ..Default::default()
    };

    let shared_for_app = Arc::clone(&shared);
    let config_for_app = config;
    let cmd_tx_for_app = cmd_tx;

    eframe::run_native(
        "SDR App",
        native_options,
        Box::new(move |cc| {
            Ok(Box::new(SdrApp::new(
                cc,
                config_for_app,
                shared_for_app,
                cmd_tx_for_app,
            )))
        }),
    ).map_err(|e| anyhow::anyhow!("eframe error: {e}"))?;

    // Signal path tasks get a clean shutdown when rt is dropped
    rt.shutdown_timeout(std::time::Duration::from_secs(3));
    tracing::info!("sdrapp exiting cleanly");

    Ok(())
}
