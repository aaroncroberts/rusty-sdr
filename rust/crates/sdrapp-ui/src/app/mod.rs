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
    help::HelpPanel,
    theme,
    waterfall::WaterfallWidget,
};

mod panels;

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
    /// Top of the spectrum display in dBFS (like "max" or "ref level" in SDR# / GQRX).
    /// db_ceil = ref_level;  db_floor = ref_level - dyn_range.
    ref_level: f32,
    /// How many dB of range to show (vertical span of spectrum/waterfall).
    dyn_range: f32,
    /// When true, ref_level tracks the signal ceiling automatically.
    auto_ref: bool,
    /// Waterfall brightness offset (positive = brighter / more sensitive).
    /// Applied only to waterfall colourmap; does not affect spectrum.
    wf_gain: f32,
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
    /// Temporary edit buffer: (name, freq_str, mode, category)
    bookmark_edit_buf: (String, String, sdrapp_core::signal_path::DemodMode, String),
    /// Category filter for bookmark list (empty = show all).
    bookmark_cat_filter: String,
    /// Sort bookmarks by frequency (false = insertion order).
    bookmark_sort_by_freq: bool,
    // ── Scanner state ─────────────────────────────────────────────────────────
    /// Dwell time in seconds (local UI state before sending command).
    scan_dwell_ui: f32,
    /// Category filter for scanner (empty = all bookmarks).
    scan_cat_ui: String,
    // ── App settings ──────────────────────────────────────────────────────────
    /// Whether the settings window (Ctrl+,) is open.
    show_settings: bool,
    /// When true, send Start command on the very first frame (--auto-start flag).
    auto_start_pending: bool,
}

impl SdrApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        config: AppConfig,
        shared: Arc<RwLock<SharedState>>,
        cmd_tx: crossbeam_channel::Sender<SignalPathCommand>,
        recorder_cmd_tx: tokio::sync::mpsc::Sender<RecorderCommand>,
        midi_bindings: Vec<(usize, String, String)>,
        auto_start: bool,
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
        let mut waterfall_widget = WaterfallWidget::new_with_colormap(1024, (-120.0, 0.0));
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
            ref_level: -30.0,
            dyn_range: 60.0,
            auto_ref: true,
            wf_gain: 0.0,
            noise_floor_ema: -85.0,
            signal_ceil_ema: -40.0,
            waterfall_row_frac: 0.0,
            help_panel: HelpPanel::default(),
            midi_bindings,
            sched_delay_secs: 0,
            sched_duration_secs: 60,
            bookmark_edit_idx: None,
            bookmark_edit_buf: (String::new(), String::new(), sdrapp_core::signal_path::DemodMode::Wbfm, String::new()),
            bookmark_cat_filter: String::new(),
            bookmark_sort_by_freq: false,
            scan_dwell_ui: 2.0,
            scan_cat_ui: String::new(),
            show_settings: false,
            auto_start_pending: auto_start,
        }
    }
}

// ── eframe::App ───────────────────────────────────────────────────────────────

impl eframe::App for SdrApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // ── --auto-start: fire Start on the very first rendered frame ─────────
        if self.auto_start_pending {
            tracing::info!("--auto-start: sending Start command");
            let _ = self.cmd_tx.send(SignalPathCommand::Start);
            self.auto_start_pending = false;
        }

        if self.config_dirty {
            self.config.save();
            self.config_dirty = false;
        }

        // ── '?' key toggles help panel ────────────────────────────────────────
        if ctx.input(|i| i.key_pressed(egui::Key::Questionmark)) {
            let mut s = self.shared.write();
            s.help_panel_open = !s.help_panel_open;
        }

        // ── Ctrl+, opens settings ─────────────────────────────────────────────
        if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::Comma)) {
            self.show_settings = !self.show_settings;
        }

        // ── Arrow-key frequency tuning ────────────────────────────────────────
        // Up/Down arrows tune by step_hz. Left/Right arrows step by 10×.
        let (up, down, left, right) = ctx.input(|i| (
            i.key_pressed(egui::Key::ArrowUp),
            i.key_pressed(egui::Key::ArrowDown),
            i.key_pressed(egui::Key::ArrowLeft),
            i.key_pressed(egui::Key::ArrowRight),
        ));
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
            let _ = self.cmd_tx.try_send(ReceiverCmd::SetFrequency(new_freq).into());
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

        // Left panel
        egui::SidePanel::left("left_panel")
            .min_width(190.0)
            .max_width(250.0)
            .frame(
                egui::Frame::none()
                    .fill(theme::PANEL_BG)
                    .inner_margin(egui::Margin::symmetric(8.0, 6.0)),
            )
            .show(ctx, |ui| self.left_panel(ui));

        // Right panel
        egui::SidePanel::right("right_panel")
            .min_width(170.0)
            .max_width(220.0)
            .frame(
                egui::Frame::none()
                    .fill(theme::PANEL_BG)
                    .inner_margin(egui::Margin::symmetric(8.0, 6.0)),
            )
            .show(ctx, |ui| self.right_panel(ui));

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
        self.help_panel.show(ctx, &mut help_open, &self.midi_bindings);
        if help_open != before {
            self.shared.write().help_panel_open = help_open;
        }

        // ── Settings window (Ctrl+,) ──────────────────────────────────────────
        if self.show_settings {
            self.settings_window(ctx);
        }

        // Request continuous repaint while running
        if self.shared.read().is_running {
            ctx.request_repaint_after(std::time::Duration::from_millis(33)); // ~30fps
        }
    }

    fn save(&mut self, _storage: &mut dyn eframe::Storage) {
        self.config.save();
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        let _ = self.cmd_tx.try_send(SignalPathCommand::Stop);
        self.config.save();
    }
}
