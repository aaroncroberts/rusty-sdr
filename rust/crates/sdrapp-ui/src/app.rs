#![forbid(unsafe_code)]

//! Top-level eframe application.
//!
//! Layout:
//!   ┌─────────────┬──────────────────────────┬──────────────┐
//!   │  Left panel │   Spectrum (center)       │  Right panel │
//!   │  · Source   │   Waterfall               │  · Volume    │
//!   │  · Freq     │                           │  · Recorder  │
//!   │  · Modules  │                           │  · Status    │
//!   └─────────────┴──────────────────────────┴──────────────┘
//!
//! The App owns:
//! - AppConfig (saved to disk on change)
//! - Arc<RwLock<SharedState>> (read from signal path tasks)
//! - crossbeam_channel::Sender<SignalPathCommand> (send to signal path)

use std::sync::Arc;
use parking_lot::RwLock;

use egui::{Color32, RichText, Ui};

use sdrapp_core::{
    config::AppConfig,
    registry::ModuleRegistry,
    signal_path::{SharedState, SignalPathCommand},
};

use crate::{
    frequency::FrequencyWidget,
    spectrum::SpectrumWidget,
    waterfall::WaterfallWidget,
};

pub struct SdrApp {
    config: AppConfig,
    registry: ModuleRegistry,
    shared: Arc<RwLock<SharedState>>,
    cmd_tx: crossbeam_channel::Sender<SignalPathCommand>,
    frequency_widget: FrequencyWidget,
    waterfall: WaterfallWidget,
    /// Dirty flag — save config at next opportunity
    config_dirty: bool,
}

impl SdrApp {
    pub fn new(
        _cc: &eframe::CreationContext<'_>,
        config: AppConfig,
        shared: Arc<RwLock<SharedState>>,
        cmd_tx: crossbeam_channel::Sender<SignalPathCommand>,
    ) -> Self {
        let freq = config.ui.frequency_hz;
        Self {
            registry: ModuleRegistry::new(),
            frequency_widget: FrequencyWidget::new(freq),
            waterfall: WaterfallWidget::new(1024, (-120.0, 0.0)),
            config,
            shared,
            cmd_tx,
            config_dirty: false,
        }
    }

    fn left_panel(&mut self, ui: &mut Ui) {
        ui.heading(RichText::new("SDR App").color(Color32::from_rgb(100, 200, 255)));
        ui.separator();

        // Source selector
        ui.label("Source");
        for src in &self.registry.sources {
            ui.label(format!("  {}", src.display_name));
        }

        ui.add_space(8.0);
        ui.separator();

        // Frequency display + scroll-to-tune
        ui.label("Frequency");
        let (_, new_freq) = self.frequency_widget.show(ui);
        if let Some(freq) = new_freq {
            let _ = self.cmd_tx.try_send(SignalPathCommand::SetFrequency(freq));
            self.config.ui.frequency_hz = freq;
            self.config_dirty = true;
        }

        ui.add_space(8.0);
        ui.separator();

        // Signal path status
        let (is_running, is_recording) = {
            let s = self.shared.read();
            (s.is_running, s.is_recording)
        };
        let status_color = if is_running { Color32::GREEN } else { Color32::DARK_GRAY };
        ui.colored_label(status_color, if is_running { "● Running" } else { "○ Stopped" });

        if is_recording {
            ui.colored_label(Color32::RED, "● Recording");
        }
    }

    fn center_panel(&mut self, ui: &mut Ui) {
        // Pull FFT data from shared state
        let fft_data = {
            let s = self.shared.read();
            s.fft_magnitudes.clone()
        };

        let freq = self.config.ui.frequency_hz;
        let span = self.config.ui.span_hz;

        // Push new row to waterfall if we have data
        if !fft_data.iter().all(|&v| v <= -119.0) {
            self.waterfall.push_row(&fft_data);
        }

        // Spectrum
        let spectrum_height = ui.available_height() * 0.4;
        let (spectrum_rect, _) = ui.allocate_exact_size(
            egui::Vec2::new(ui.available_width(), spectrum_height),
            egui::Sense::hover(),
        );
        let mut spectrum_ui = ui.child_ui(spectrum_rect, egui::Layout::default(), None);
        SpectrumWidget {
            fft_data: &fft_data,
            db_range: (-120.0, 0.0),
            freq_range: (freq.saturating_sub(span), freq + span),
            vfo_hz: freq,
        }.show(&mut spectrum_ui);

        // Waterfall below spectrum
        let ctx = ui.ctx().clone();
        self.waterfall.show(ui, &ctx);
    }

    fn right_panel(&mut self, ui: &mut Ui) {
        ui.heading("Controls");
        ui.separator();

        // Volume slider
        ui.label("Volume");
        let mut vol = self.config.ui.volume;
        if ui.add(egui::Slider::new(&mut vol, 0.0..=1.0).text("")).changed() {
            self.config.ui.volume = vol;
            let _ = self.cmd_tx.try_send(SignalPathCommand::SetVolume(vol));
            self.config_dirty = true;
        }

        ui.add_space(8.0);
        ui.separator();

        // Recording controls
        ui.label("Recorder");
        let is_recording = self.shared.read().is_recording;
        if is_recording {
            if ui.button(RichText::new("■ Stop").color(Color32::RED)).clicked() {
                let _ = self.cmd_tx.try_send(SignalPathCommand::StopRecording);
            }
        } else {
            if ui.button(RichText::new("● Record").color(Color32::from_rgb(255, 100, 100))).clicked() {
                let _ = self.cmd_tx.try_send(SignalPathCommand::StartRecording);
            }
        }

        ui.add_space(8.0);
        ui.separator();

        // Sink info
        ui.label("Output");
        for sink in &self.registry.sinks {
            ui.label(format!("  {}", sink.display_name));
        }
    }
}

impl eframe::App for SdrApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Persist config if dirty
        if self.config_dirty {
            self.config.save();
            self.config_dirty = false;
        }

        // Left panel
        egui::SidePanel::left("left_panel")
            .min_width(180.0)
            .max_width(240.0)
            .show(ctx, |ui| self.left_panel(ui));

        // Right panel
        egui::SidePanel::right("right_panel")
            .min_width(160.0)
            .max_width(200.0)
            .show(ctx, |ui| self.right_panel(ui));

        // Center (spectrum + waterfall)
        egui::CentralPanel::default().show(ctx, |ui| self.center_panel(ui));

        // Request continuous repaint while signal path is running
        if self.shared.read().is_running {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
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
