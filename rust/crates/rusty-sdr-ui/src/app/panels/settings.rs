use super::super::SdrApp;
use crate::theme;

impl SdrApp {
    pub(in crate::app) fn settings_window(&mut self, ctx: &egui::Context) {
        let mut open = self.show_settings;
        egui::Window::new("Settings")
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .min_width(360.0)
            .frame(
                egui::Frame::window(&ctx.style())
                    .fill(theme::PANEL_BG)
                    .stroke(egui::Stroke::new(1.0, theme::BORDER)),
            )
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing.y = 6.0;

                // ── Waterfall colormap ─────────────────────────────────────────
                ui.label(
                    egui::RichText::new("Waterfall")
                        .color(theme::TEXT_MUTED)
                        .small(),
                );
                ui.horizontal(|ui| {
                    ui.label("Colormap");
                    let presets = [
                        theme::WaterfallColormap::Thermal,
                        theme::WaterfallColormap::Grayscale,
                        theme::WaterfallColormap::Inferno,
                        theme::WaterfallColormap::Classic,
                    ];
                    let current: theme::WaterfallColormap =
                        match self.config.ui.waterfall_colormap.as_str() {
                            "Grayscale" => theme::WaterfallColormap::Grayscale,
                            "Inferno" => theme::WaterfallColormap::Inferno,
                            "Classic" => theme::WaterfallColormap::Classic,
                            _ => theme::WaterfallColormap::Thermal,
                        };
                    let mut selected = current;
                    egui::ComboBox::from_id_salt("wf_colormap")
                        .selected_text(selected.label())
                        .show_ui(ui, |ui| {
                            for preset in presets {
                                ui.selectable_value(&mut selected, preset, preset.label());
                            }
                        });
                    if selected != current {
                        self.config.ui.waterfall_colormap = selected.label().to_string();
                        self.waterfall.set_colormap(selected.build());
                        self.config_dirty = true;
                    }
                });

                ui.add_space(4.0);
                ui.separator();
                ui.add_space(4.0);

                // ── UI scale ──────────────────────────────────────────────────
                ui.label(
                    egui::RichText::new("Interface")
                        .color(theme::TEXT_MUTED)
                        .small(),
                );
                ui.horizontal(|ui| {
                    ui.label("UI Scale");
                    let mut scale = self.config.ui.font_scale;
                    let resp = ui.add(
                        egui::Slider::new(&mut scale, 0.75_f32..=2.5)
                            .step_by(0.05)
                            .fixed_decimals(2)
                            .suffix("×"),
                    );
                    if resp.changed() {
                        self.config.ui.font_scale = scale;
                        ctx.set_pixels_per_point(scale);
                        self.config_dirty = true;
                    }
                    if ui.button("Reset").clicked() {
                        self.config.ui.font_scale = 1.0;
                        ctx.set_pixels_per_point(1.0);
                        self.config_dirty = true;
                    }
                });

                ui.add_space(4.0);
                ui.separator();
                ui.add_space(4.0);

                // ── Config file path ──────────────────────────────────────────
                ui.label(
                    egui::RichText::new("Storage")
                        .color(theme::TEXT_MUTED)
                        .small(),
                );
                let config_path = rusty_sdr_core::config::config_path();
                ui.horizontal(|ui| {
                    ui.label("Config file");
                    ui.add(
                        egui::TextEdit::singleline(&mut config_path.display().to_string().as_str())
                            .desired_width(220.0)
                            .interactive(false)
                            .font(egui::TextStyle::Monospace),
                    );
                    if ui.button("Reveal").clicked() {
                        // Open Finder / Explorer to the containing directory
                        if let Some(parent) = config_path.parent() {
                            let _ = std::process::Command::new("open").arg(parent).spawn();
                        }
                    }
                });
            });

        self.show_settings = open;
    }
}
