#![forbid(unsafe_code)]

//! Top-level eframe application.
//!
//! Layout:
//!   ┌────────────────┬────────────────────────────────┬──────────────────┐
//!   │  Left panel    │   Spectrum (center)             │  Right panel     │
//!   │  · Source      │   Waterfall                     │  · Volume / VU   │
//!   │  · Frequency   │                                 │  · Band presets  │
//!   │  · Start/Stop  │                                 │  · Recorder      │
//!   │  · Status      │                                 │  · MIDI status   │
//!   ├────────────────┴────────────────────────────────┴──────────────────┤
//!   │  Status bar: device info · MIDI page · buffer health · recording   │
//!   └────────────────────────────────────────────────────────────────────┘

use parking_lot::RwLock;
use std::sync::Arc;

use sdrapp_core::{
    config::AppConfig,
    registry::ModuleRegistry,
    signal_path::{ReceiverCmd, SharedState, SignalPathCommand},
};
use sdrapp_recorder::RecorderCommand;

use crate::{
    frequency::FrequencyWidget,
    handbook::HandbookWindow,
    help::HelpPanel,
    theme,
    waterfall::WaterfallWidget,
};

mod midi_mapper;
mod panels;

use midi_mapper::MidiMapperWindow;

pub struct SdrApp {
    config: AppConfig,
    registry: ModuleRegistry,
    shared: Arc<RwLock<SharedState>>,
    cmd_tx: crossbeam_channel::Sender<SignalPathCommand>,
    recorder_cmd_tx: tokio::sync::mpsc::Sender<RecorderCommand>,
    frequency_widget: FrequencyWidget,
    waterfall: WaterfallWidget,
    /// Dirty flag — save config at next opportunity
    config_dirty: bool,
    /// Simulated audio peak level [0.0, 1.0] for VU meter (updated each frame)
    vu_peak: f32,
    /// Peak-hold buffer: tracks per-bin maximum with slow decay
    peak_hold: Vec<f32>,
    // ── Display range ─────────────────────────────────────────────────────────
    /// Unified spectrum + waterfall floor in dBFS (bottom of Y-axis / darkest colour).
    /// Persisted in AppConfig.ui.fft_floor.
    fft_floor: f32,
    /// Unified spectrum + waterfall ceiling in dBFS (top of Y-axis / brightest colour).
    /// Persisted in AppConfig.ui.fft_ceil.
    fft_ceil: f32,
    /// When true, fft_ceil and fft_floor track the signal level automatically.
    auto_ref: bool,
    /// Slow EMA of 10th-percentile FFT bin — noise floor estimate for auto-ref.
    noise_floor_ema: f32,
    /// Slow EMA of 99th-percentile FFT bin — signal ceiling estimate for auto-ref.
    signal_ceil_ema: f32,
    /// Fractional row accumulator for waterfall speed control.
    /// Incremented by waterfall_speed each frame; push_row fires once per integer crossed.
    waterfall_row_frac: f32,
    /// Help panel widget (tabs state).
    help_panel: HelpPanel,
    /// Live MIDI bindings for the help panel MIDI Map tab.
    /// Populated at construction from the nanoKontrol2 default profile.
    midi_bindings: Vec<(usize, String, String)>,
    /// Scheduled recording: delay before start (seconds).
    sched_delay_secs: u32,
    /// Scheduled recording: duration (seconds).
    sched_duration_secs: u32,
    // ── Bookmark manager state ─────────────────────────────────────────────────
    /// Index of bookmark being edited inline (None = no edit in progress).
    bookmark_edit_idx: Option<usize>,
    /// Temporary edit buffer: (name, freq_str, mode, category, nfm_bw_hz, squelch_dbfs, ctcss)
    /// Temporary edit buffer: (name, freq_str, mode, category, nfm_bw_hz, squelch_dbfs, ctcss, antenna)
    bookmark_edit_buf: (String, String, sdrapp_core::signal_path::DemodMode, String, u32, f32, bool, Option<String>),
    /// Antenna port saved before the last bookmark-with-antenna was recalled.
    /// Restored when the user tunes away (next recall, manual tune, or band preset).
    bookmark_prev_antenna: Option<String>,
    /// Category filter for bookmark list (empty = show all).
    bookmark_cat_filter: String,
    /// Sort bookmarks by frequency (false = insertion order).
    bookmark_sort_by_freq: bool,
    // ── Scanner state ─────────────────────────────────────────────────────────
    /// Dwell time in seconds (local UI state before sending command).
    scan_dwell_ui: f32,
    /// Category filter for scanner (empty = all bookmarks).
    scan_cat_ui: String,
    // ── Range scanner UI state ────────────────────────────────────────────────
    /// FM range scan lower bound (Hz).
    range_scan_lo_hz: u64,
    /// FM range scan upper bound (Hz).
    range_scan_hi_hz: u64,
    /// FM range scan step size (Hz).
    range_scan_step_hz: u64,
    /// FM range scan squelch threshold (dBFS).
    range_scan_squelch: f32,
    /// FM range scan dwell time (seconds).
    range_scan_dwell: f32,
    /// Require stereo pilot before locking.
    range_scan_stereo_only: bool,
    /// egui time (seconds since app start) when the range scanner last locked.
    /// Used to drive the 3-second auto-dismiss banner. None = no lock yet.
    scan_lock_time: Option<f64>,
    /// Last frequency (Hz) where the range scanner locked — persists for the
    /// spectrum indicator even after the banner fades.
    scan_last_locked_freq: Option<u64>,
    // ── App settings ──────────────────────────────────────────────────────────
    /// Whether the settings window (Ctrl+,) is open.
    show_settings: bool,
    /// When true, send Start command on the very first frame (--auto-start flag).
    auto_start_pending: bool,
    /// egui time (seconds since app start) of the last frame where ADC clipping
    /// was detected.  Used to hold the "ADC SAT" badge visible for 2 s.
    last_clipping_time: Option<f64>,
    /// When true, the waterfall level auto-follows the signal ceiling.
    /// Disarmed when the user manually drags the WF Level slider; re-arms after 10 s.
    wf_auto_armed: bool,
    /// egui time of the last manual WF Level slider interaction.
    wf_last_manual_drag: f64,
    /// Show first-run onboarding overlay (true until dismissed once).
    show_onboarding: bool,
    /// Frequency buckets (freq_hz / 500_000) where demod auto-suggest was dismissed.
    demod_suggest_dismissed: std::collections::HashSet<u64>,
    /// Last frequency bucket seen — used to clear dismissed set when user moves >500 kHz.
    last_freq_bucket: u64,
    /// Last center frequency for which the waterfall was valid.
    /// When the center freq changes by more than 10% of the bandwidth,
    /// the waterfall is cleared so stale rows don't mislead the user.
    last_waterfall_freq: u64,
    /// Whether the ? keyboard shortcut overlay is open.
    show_shortcut_overlay: bool,
    /// MIDI mapper floating window (nanoKONTROL2 diagram).
    midi_mapper: std::sync::Arc<parking_lot::Mutex<MidiMapperWindow>>,
    /// Whether the MIDI mapper window is open.
    show_midi_mapper: bool,
    /// Operators Handbook floating window.
    handbook: std::sync::Arc<parking_lot::Mutex<crate::handbook::HandbookWindow>>,
    /// Whether the Operators Handbook window is open.
    show_handbook: bool,
    /// ADS-B aircraft map window.
    adsb_map: std::sync::Arc<parking_lot::Mutex<panels::adsb_map::AdsbMapWindow>>,
    /// Whether the ADS-B map window is open.
    show_adsb_map: bool,
    /// Shared ADS-B aircraft store (populated when decoder is running).
    adsb_store: std::sync::Arc<parking_lot::Mutex<sdrapp_adsb::AircraftStore>>,
    /// IQ broadcast sender — held so the UI can call `.subscribe()` to obtain a
    /// fresh Receiver each time the user starts the ADS-B decoder.  Using the
    /// Sender (not a pre-subscribed Receiver) allows unlimited stop/restart.
    adsb_iq_tx: Option<tokio::sync::broadcast::Sender<std::sync::Arc<[sdrapp_core::sample::IqSample]>>>,
    /// Running ADS-B decoder thread (Some = running, None = stopped).
    adsb_decoder: Option<crate::adsb_decoder::AdsbDecoder>,
    /// Decimation factor saved before entering ADS-B mode so we can restore it on exit.
    /// None = ADS-B mode did not change the hardware decimation.
    adsb_prev_decimation: Option<u32>,
    /// Antenna port ("A"/"B"/"C") saved before entering ADS-B mode so we can restore on exit.
    /// None = ADS-B mode did not change the antenna.
    adsb_prev_antenna: Option<String>,
    /// Whether audio is muted. Independent of the volume knob position so the
    /// knob value is preserved across mute/unmute cycles.
    pub muted: bool,
    /// Whether ADS-B mode triggered the mute (so we can restore it on ADS-B exit).
    adsb_did_mute: bool,
    /// True once we've sent the initial SetVolume to sync signal path with config.
    /// The signal path hardcodes volume=0.8 on startup; we must sync it on the first
    /// active frame to match whatever is in config.
    volume_synced: bool,
    /// When true, the ADS-B decoder should be started as soon as sample_rate_sps >= 2 Msps.
    /// Set after sending SetDecimationFactor(1) while waiting for the device to apply it.
    adsb_start_pending: bool,
}

impl SdrApp {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        config: AppConfig,
        shared: Arc<RwLock<SharedState>>,
        cmd_tx: crossbeam_channel::Sender<SignalPathCommand>,
        recorder_cmd_tx: tokio::sync::mpsc::Sender<RecorderCommand>,
        midi_bindings: Vec<(usize, String, String)>,
        auto_start: bool,
        adsb_iq_tx: Option<tokio::sync::broadcast::Sender<std::sync::Arc<[sdrapp_core::sample::IqSample]>>>,
    ) -> Self {
        // Apply our beautiful dark theme
        theme::apply(&cc.egui_ctx);

        // Apply persisted font scale
        if (config.ui.font_scale - 1.0).abs() > 0.01 {
            cc.egui_ctx.set_pixels_per_point(config.ui.font_scale);
        }

        // Build initial waterfall colormap from config
        let wf_colormap: theme::WaterfallColormap = match config.ui.waterfall_colormap.as_str() {
            "Grayscale" => theme::WaterfallColormap::Grayscale,
            "Inferno" => theme::WaterfallColormap::Inferno,
            "Classic" => theme::WaterfallColormap::Classic,
            _ => theme::WaterfallColormap::Thermal,
        };

        let freq = config.ui.frequency_hz;
        let fft_floor = config.ui.fft_floor;
        let fft_ceil = config.ui.fft_ceil;
        let seen_onboarding = config.ui.seen_onboarding;
        let handbook_section = config.ui.handbook_section;
        let handbook_page = config.ui.handbook_page;
        let show_handbook = config.ui.show_handbook;
        let show_adsb_map = config.ui.show_adsb_map;
        let adsb_map = panels::adsb_map::AdsbMapWindow::with_viewport(
            config.ui.adsb_map_lat,
            config.ui.adsb_map_lon,
            config.ui.adsb_map_zoom,
        );
        // Initial range uses the persisted fft_floor/fft_ceil from config.
        // Both spectrum Y-axis and waterfall colormap use this same range so that
        // a single pair of MIN/MAX controls drives the entire display.
        let mut waterfall_widget = WaterfallWidget::new_with_colormap(1024, (fft_floor, fft_ceil));
        waterfall_widget.set_colormap(wf_colormap.build());

        Self {
            registry: ModuleRegistry::new(),
            frequency_widget: FrequencyWidget::new(freq),
            waterfall: waterfall_widget,
            config,
            shared,
            cmd_tx,
            recorder_cmd_tx,
            config_dirty: false,
            vu_peak: 0.0,
            peak_hold: Vec::new(),
            fft_floor,
            fft_ceil,
            auto_ref: true,
            noise_floor_ema: -85.0,
            signal_ceil_ema: -40.0,
            waterfall_row_frac: 0.0,
            help_panel: HelpPanel::default(),
            midi_bindings,
            sched_delay_secs: 0,
            sched_duration_secs: 60,
            bookmark_edit_idx: None,
            bookmark_edit_buf: (
                String::new(),
                String::new(),
                sdrapp_core::signal_path::DemodMode::Wbfm,
                String::new(),
                12_500_u32,
                -50.0_f32,
                false,
                None,
            ),
            bookmark_prev_antenna: None,
            bookmark_cat_filter: String::new(),
            bookmark_sort_by_freq: false,
            scan_dwell_ui: 2.0,
            scan_cat_ui: String::new(),
            range_scan_lo_hz: 87_500_000,
            range_scan_hi_hz: 108_000_000,
            range_scan_step_hz: 100_000,
            range_scan_squelch: -60.0,
            range_scan_dwell: 0.3,
            range_scan_stereo_only: false,
            scan_lock_time: None,
            scan_last_locked_freq: None,
            show_settings: false,
            auto_start_pending: auto_start,
            last_clipping_time: None,
            wf_auto_armed: true,
            wf_last_manual_drag: 0.0,
            show_onboarding: !seen_onboarding,
            demod_suggest_dismissed: std::collections::HashSet::new(),
            last_freq_bucket: 0,
            last_waterfall_freq: freq,
            show_shortcut_overlay: false,
            midi_mapper: std::sync::Arc::new(parking_lot::Mutex::new(MidiMapperWindow::new_nanokontrol2())),
            show_midi_mapper: false,
            handbook: std::sync::Arc::new(parking_lot::Mutex::new(
                HandbookWindow::with_state(handbook_section, handbook_page),
            )),
            show_handbook,
            adsb_map: std::sync::Arc::new(parking_lot::Mutex::new(adsb_map)),
            show_adsb_map,
            adsb_store: std::sync::Arc::new(parking_lot::Mutex::new(
                sdrapp_adsb::AircraftStore::new(),
            )),
            adsb_iq_tx,
            adsb_decoder: None,
            adsb_prev_decimation: None,
            adsb_prev_antenna: None,
            muted: false,
            adsb_did_mute: false,
            volume_synced: false,
            adsb_start_pending: false,
        }
    }
}

// ── eframe::App ───────────────────────────────────────────────────────────────

/// Parse a demod mode string (as stored in AppConfig) to DemodMode.
pub(crate) fn parse_config_demod_mode(s: &str) -> sdrapp_core::signal_path::DemodMode {
    use sdrapp_core::signal_path::DemodMode;
    match s {
        "Nfm" => DemodMode::Nfm,
        "Am" => DemodMode::Am,
        "Usb" => DemodMode::Usb,
        "Lsb" => DemodMode::Lsb,
        "Dsb" => DemodMode::Dsb,
        "Cw" => DemodMode::Cw,
        _ => DemodMode::Wbfm,
    }
}

// ── SdrApp helpers ────────────────────────────────────────────────────────────

impl SdrApp {
    /// Apply a frequency change: send the hardware command, update config, and
    /// rebuild the frequency widget so all three stay in sync.
    ///
    /// This 4-line pattern is the single correct way to tune from the UI — use
    /// it everywhere instead of duplicating the three state writes inline.
    pub(in crate::app) fn apply_tune(&mut self, freq: u64) {
        // Restore bookmark antenna override if the user is manually tuning away.
        self.restore_bookmark_antenna();
        let _ = self
            .cmd_tx
            .try_send(ReceiverCmd::SetFrequency(freq).into());
        self.config.ui.frequency_hz = freq;
        self.frequency_widget = FrequencyWidget::new(freq);
        self.config_dirty = true;
    }

    /// If a bookmark antenna override is active, restore the previous antenna port.
    /// Called on any manual tune or band preset selection.
    pub(in crate::app) fn restore_bookmark_antenna(&mut self) {
        if let Some(prev) = self.bookmark_prev_antenna.take() {
            let port: u8 = match prev.as_str() { "B" => 1, "C" => 2, _ => 0 };
            self.config.source.antenna = prev;
            self.config_dirty = true;
            let _ = self.cmd_tx.try_send(
                sdrapp_core::signal_path::HardwareCommand::SetAntenna(port).into(),
            );
            tracing::info!(antenna = port, "bookmark antenna override cleared — previous antenna restored");
        }
    }
}

impl eframe::App for SdrApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // ── --auto-start: fire Start on the very first rendered frame ─────────
        if self.auto_start_pending {
            tracing::info!("--auto-start: sending Start command");
            let _ = self.cmd_tx.send(SignalPathCommand::Start);
            // Restore saved demod mode so the signal path doesn't default to WBFM.
            let saved_mode = parse_config_demod_mode(&self.config.ui.demod_mode.clone());
            let _ = self.cmd_tx.try_send(ReceiverCmd::SetDemodMode(saved_mode).into());
            self.auto_start_pending = false;
        }

        // ── Startup volume sync ───────────────────────────────────────────────
        // The signal path hardcodes Volume::new(0.8). Sync config volume on the
        // first frame so a stale config value (e.g. 0.0 from an old session) doesn't
        // leave audio stuck at 0.8 or at the wrong level.
        if !self.volume_synced {
            // Clamp: if config somehow saved 0.0, restore a sensible default.
            if self.config.ui.volume < 0.01 {
                self.config.ui.volume = 0.8;
                self.config_dirty = true;
            }
            // Send the real volume (never 0.0); mute state is a separate gate.
            let _ = self.cmd_tx.try_send(ReceiverCmd::SetVolume(self.config.ui.volume).into());
            self.volume_synced = true;
        }

        // Sync MIDI Learn bindings: if the MIDI dispatcher thread learned a new CC,
        // it wrote to shared.midi_cc_to_knob directly. Detect and persist the change.
        {
            let shared_len = self.shared.read().midi_cc_to_knob.len();
            if shared_len != self.config.midi_learn.len() {
                self.config.midi_learn = self
                    .shared
                    .read()
                    .midi_cc_to_knob
                    .iter()
                    .map(|(&cc, knob_id)| (knob_id.clone(), cc))
                    .collect();
                self.config_dirty = true;
            }
        }

        if self.config_dirty {
            // Snapshot learned MIDI bindings (CC → knob_id becomes knob_id → CC)
            self.config.midi_learn = self
                .shared
                .read()
                .midi_cc_to_knob
                .iter()
                .map(|(&cc, knob_id)| (knob_id.clone(), cc))
                .collect();
            self.config.save();
            self.config_dirty = false;
        }

        // ── F1 toggles the Operators Handbook ────────────────────────────────
        if ctx.input(|i| i.key_pressed(egui::Key::F1)) {
            self.show_handbook = !self.show_handbook;
            self.config.ui.show_handbook = self.show_handbook;
            self.config_dirty = true;
        }

        // ── '?' key toggles help panel ────────────────────────────────────────
        if ctx.input(|i| i.key_pressed(egui::Key::Questionmark)) {
            let mut s = self.shared.write();
            s.help_panel_open = !s.help_panel_open;
        }

        // ── Escape cancels pending mapper bind ────────────────────────────────
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.shared.write().midi_map_pending = None;
        }

        // ── Ctrl+, opens settings ─────────────────────────────────────────────
        if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::Comma)) {
            self.show_settings = !self.show_settings;
        }

        // ── Arrow-key frequency tuning ────────────────────────────────────────
        // Up/Down arrows tune by step_hz. Left/Right arrows step by 10×.
        let (up, down, left, right) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::ArrowUp),
                i.key_pressed(egui::Key::ArrowDown),
                i.key_pressed(egui::Key::ArrowLeft),
                i.key_pressed(egui::Key::ArrowRight),
            )
        });
        if up || down || left || right {
            let step = self.shared.read().demod.tune_step_hz;
            let coarse = step * 10;
            let delta: i64 = match (up, down, left, right) {
                (true, _, _, _) => step as i64,
                (_, true, _, _) => -(step as i64),
                (_, _, _, true) => coarse as i64,
                _ => -(coarse as i64),
            };
            let freq = self.config.ui.frequency_hz;
            let new_freq = if delta >= 0 {
                freq.saturating_add(delta as u64)
            } else {
                freq.saturating_sub((-delta) as u64).max(1)
            };
            let _ = self
                .cmd_tx
                .try_send(ReceiverCmd::SetFrequency(new_freq).into());
            self.config.ui.frequency_hz = new_freq;
            self.frequency_widget = FrequencyWidget::new(new_freq);
            self.config_dirty = true;
        }

        // Status bar at the bottom
        egui::TopBottomPanel::bottom("status_bar")
            .exact_height(20.0)
            .frame(
                egui::Frame::none()
                    .fill(theme::BG)
                    .inner_margin(egui::Margin::symmetric(8.0, 0.0)),
            )
            .show(ctx, |ui| self.status_bar(ui));

        // Left panel (scrollable so controls are always reachable)
        egui::SidePanel::left("left_panel")
            .resizable(true)
            .default_width(210.0)
            .min_width(180.0)
            .max_width(360.0)
            .frame(
                egui::Frame::none()
                    .fill(theme::PANEL_BG)
                    .inner_margin(egui::Margin::symmetric(8.0, 6.0)),
            )
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false; 2])
                    .show(ui, |ui| self.left_panel(ui));
            });

        // Right panel (scrollable so controls are always reachable)
        egui::SidePanel::right("right_panel")
            .min_width(190.0)
            .max_width(260.0)
            .frame(
                egui::Frame::none()
                    .fill(theme::PANEL_BG)
                    // Extra right margin keeps text clear of the scrollbar track
                    .inner_margin(egui::Margin { left: 8.0, right: 18.0, top: 6.0, bottom: 6.0 }),
            )
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false; 2])
                    .show(ui, |ui| self.right_panel(ui));
            });

        // Center spectrum + waterfall
        egui::CentralPanel::default()
            .frame(
                egui::Frame::none()
                    .fill(theme::BG)
                    .inner_margin(egui::Margin::same(0.0)),
            )
            .show(ctx, |ui| self.center_panel(ui));

        // ── Help panel (floating window) ──────────────────────────────────────
        let mut help_open = self.shared.read().help_panel_open;
        let before = help_open;
        self.help_panel
            .show(ctx, &mut help_open, &self.midi_bindings);
        if help_open != before {
            self.shared.write().help_panel_open = help_open;
        }

        // ── Settings window (Ctrl+,) ──────────────────────────────────────────
        if self.show_settings {
            self.settings_window(ctx);
        }

        // ── MIDI Mapper window ────────────────────────────────────────────────
        // Process pending state from last frame (before show_viewport_deferred)
        {
            let mut mapper = self.midi_mapper.lock();
            if !mapper.viewport_open {
                self.show_midi_mapper = false;
                mapper.viewport_open = true; // reset for next open
            }
        }
        if self.show_midi_mapper {
            let mapper_arc = std::sync::Arc::clone(&self.midi_mapper);
            let shared_arc = std::sync::Arc::clone(&self.shared);
            let bindings = self.midi_bindings.clone();
            ctx.show_viewport_deferred(
                egui::ViewportId::from_hash_of("midi_mapper"),
                egui::ViewportBuilder::default()
                    .with_title("MIDI Mapper — Korg nanoKONTROL2")
                    .with_inner_size([800.0, 340.0])
                    .with_min_inner_size([640.0, 240.0]),
                move |ctx, _class| {
                    let mut mapper = mapper_arc.lock();
                    let mut open = true;
                    mapper.show(ctx, &mut open, &shared_arc, &bindings);
                    if !open {
                        mapper.viewport_open = false;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                },
            );
        }

        // ── ADS-B Aircraft Map window ─────────────────────────────────────────
        // Process pending state from last frame (before show_viewport_deferred)
        {
            // Snapshot decoder state to push into the map window
            let decoder_running = self
                .adsb_decoder
                .as_ref()
                .map(|d| d.is_running())
                .unwrap_or(false);
            let frame_count = self
                .adsb_decoder
                .as_ref()
                .map(|d| d.frames_decoded())
                .unwrap_or(0);
            let preamble_count = self
                .adsb_decoder
                .as_ref()
                .map(|d| d.preambles_detected())
                .unwrap_or(0);
            let sr = self.shared.read().sample_rate_sps;

            let (start_req, stop_req) = {
                let mut map = self.adsb_map.lock();

                // Push current state so the map window can display it
                map.decoder_running = decoder_running;
                map.frame_count = frame_count;
                map.preamble_count = preamble_count;
                map.sample_rate_ok = sr >= 2_000_000;
                map.adsb_start_pending = self.adsb_start_pending;

                if !map.viewport_open {
                    self.show_adsb_map = false;
                    map.viewport_open = true; // reset for next open
                }
                if map.set_home_pending {
                    map.set_home_pending = false;
                    self.config.ui.home_lat = map.center_lat();
                    self.config.ui.home_lon = map.center_lon();
                    self.config_dirty = true;
                }
                let (lat, lon, zoom) = (map.center_lat(), map.center_lon(), map.zoom_ppd());
                if self.show_adsb_map
                    && ((self.config.ui.adsb_map_lat - lat).abs() > 0.001
                        || (self.config.ui.adsb_map_lon - lon).abs() > 0.001
                        || (self.config.ui.adsb_map_zoom - zoom).abs() > 0.1)
                {
                    self.config.ui.adsb_map_lat = lat;
                    self.config.ui.adsb_map_lon = lon;
                    self.config.ui.adsb_map_zoom = zoom;
                    self.config_dirty = true;
                }

                // Consume action requests set by the map's toolbar buttons
                (
                    std::mem::take(&mut map.start_requested),
                    std::mem::take(&mut map.stop_requested),
                )
            };

            // Deferred start: fires each frame until the hardware reaches ≥ 2 Msps
            if self.adsb_start_pending && sr >= 2_000_000 {
                self.adsb_start_pending = false;
                self.adsb_start_decoder();
            }

            // Start requested by map toolbar
            if start_req && !decoder_running && !self.adsb_start_pending {
                self.adsb_start_sequence();
            }

            // Stop requested by map toolbar
            if stop_req {
                self.adsb_stop_decoder();
            }
        }
        if self.show_adsb_map {
            self.config.ui.show_adsb_map = true;
            let map_arc = std::sync::Arc::clone(&self.adsb_map);
            let store_arc = std::sync::Arc::clone(&self.adsb_store);
            let home_lat = self.config.ui.home_lat;
            let home_lon = self.config.ui.home_lon;
            ctx.show_viewport_deferred(
                egui::ViewportId::from_hash_of("adsb_map"),
                egui::ViewportBuilder::default()
                    .with_title("✈  ADS-B Aircraft Map")
                    .with_inner_size([900.0, 560.0])
                    .with_min_inner_size([600.0, 400.0]),
                move |ctx, _class| {
                    let aircraft: Vec<_> = store_arc.lock().aircraft().into_iter().cloned().collect();
                    let mut map = map_arc.lock();
                    let mut open = true;
                    map.show(ctx, &mut open, &aircraft, home_lat, home_lon);
                    if !open {
                        map.viewport_open = false;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                },
            );
        } else if self.config.ui.show_adsb_map {
            self.config.ui.show_adsb_map = false;
            self.config_dirty = true;
        }

        // ── Operators Handbook window ─────────────────────────────────────────
        // Process handbook state from last frame
        {
            let hb = self.handbook.lock();
            if !hb.viewport_open {
                drop(hb);
                self.show_handbook = false;
                self.handbook.lock().viewport_open = true; // reset for next open
            } else if self.config.ui.handbook_section != hb.section
                || self.config.ui.handbook_page != hb.page
                || self.config.ui.show_handbook != self.show_handbook
            {
                self.config.ui.handbook_section = hb.section;
                self.config.ui.handbook_page = hb.page;
                self.config.ui.show_handbook = self.show_handbook;
                self.config_dirty = true;
            }
        }
        if self.show_handbook {
            let hb_arc = std::sync::Arc::clone(&self.handbook);
            ctx.show_viewport_deferred(
                egui::ViewportId::from_hash_of("handbook"),
                egui::ViewportBuilder::default()
                    .with_title("📖  Operators Handbook")
                    .with_inner_size([920.0, 660.0])
                    .with_min_inner_size([700.0, 450.0]),
                move |ctx, _class| {
                    let mut hb = hb_arc.lock();
                    let mut open = true;
                    hb.show(ctx, &mut open);
                    if !open {
                        hb.viewport_open = false;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                },
            );
        }

        // Keep the UI live at ~30 fps unconditionally.
        // When idle (not running) this still lets the VU meter, status bar, and
        // waterfall react promptly to state changes (e.g. auto-start, hot-plug).
        ctx.request_repaint_after(std::time::Duration::from_millis(33));
    }

    fn save(&mut self, _storage: &mut dyn eframe::Storage) {
        self.config.save();
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        let _ = self.cmd_tx.try_send(SignalPathCommand::Stop);
        self.config.save();
    }
}
