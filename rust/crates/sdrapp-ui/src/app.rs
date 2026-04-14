#![forbid(unsafe_code)]

use eframe::egui;

/// Top-level eframe application.
pub struct SdrApp {
    // TODO: wire signal path state here
}

impl SdrApp {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        Self {}
    }
}

impl eframe::App for SdrApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("SDR App");
            ui.label("Signal path not yet connected.");
        });
    }
}
